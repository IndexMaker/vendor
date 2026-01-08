use super::types::{GasConfig, SubmissionResult, SubmissionSummary, TxReceipt};
use crate::processor::SubmissionPayload;
use alloy::{
    network::EthereumWallet,
    primitives::Address,
    providers::Provider,
    rpc::types::TransactionRequest,
    signers::local::PrivateKeySigner,
    sol_types::SolCall,
};
use common::interfaces::factor::IFactor;
use common::{labels::Labels, vector::Vector};
use std::time::Duration;

/// Configuration for on-chain submitter
#[derive(Debug, Clone)]
pub struct SubmitterConfig {
    pub rpc_url: String,
    pub castle_address: Address,
    pub vendor_id: u128,
    pub private_key: Option<String>,
    pub dry_run: bool,
    pub gas_config: GasConfig,
    pub retry_attempts: u32,
    pub retry_delay_ms: u64,
}

/// On-chain transaction submitter - generic over provider type
pub struct OnchainSubmitter<P>
where
    P: Provider + Clone,
{
    config: SubmitterConfig,
    provider: Option<P>,
    wallet: Option<EthereumWallet>,
}

impl<P> OnchainSubmitter<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    /// Create a new submitter with an existing provider
    pub fn new_with_provider(config: SubmitterConfig, provider: P) -> eyre::Result<Self> {
        // Create wallet if private key provided
        let wallet = if let Some(ref pk) = config.private_key {
            let signer: PrivateKeySigner = pk.parse()?;
            Some(EthereumWallet::from(signer))
        } else {
            None
        };

        Ok(Self {
            config,
            provider: Some(provider),
            wallet,
        })
    }

    /// Submit a complete payload to the blockchain
    pub async fn submit_payload(&self, payload: SubmissionPayload) -> eyre::Result<SubmissionResult> {
        if self.config.dry_run {
            return Ok(self.dry_run_summary(&payload));
        }

        let _provider = self.provider.as_ref().ok_or_else(|| eyre::eyre!("Provider not initialized"))?;

        tracing::info!("🔗 Submitting to blockchain...");
        tracing::info!("  Castle address: {:?}", self.config.castle_address);
        tracing::info!("  Vendor ID: {}", self.config.vendor_id);

        let mut market_data_tx = None;
        let mut buy_order_txs = Vec::new();

        // Step 1: Submit market data
        if !payload.market_data.assets.is_empty() {
            tracing::info!("📡 Submitting market data for {} assets...", payload.market_data.assets.len());
            
            match self.submit_market_data(&payload).await {
                Ok(receipt) => {
                    tracing::info!("  ✓ Market data submitted: {}", receipt.tx_hash);
                    tracing::info!("    Block: {}, Gas: {}", receipt.block_number, receipt.gas_used);
                    market_data_tx = Some(receipt);
                }
                Err(e) => {
                    tracing::error!("  ✗ Market data submission failed: {:?}", e);
                    return Ok(SubmissionResult::Failed {
                        error: format!("Market data submission failed: {}", e),
                    });
                }
            }
        }

        // Step 2: Submit buy orders
        for buy_order in &payload.buy_orders {
            tracing::info!("📝 Submitting buy order for index {}...", buy_order.index_id);
            
            match self.submit_buy_order(&payload, buy_order.index_id).await {
                Ok(receipt) => {
                    tracing::info!("  ✓ Buy order submitted: {}", receipt.tx_hash);
                    tracing::info!("    Block: {}, Gas: {}", receipt.block_number, receipt.gas_used);
                    buy_order_txs.push((buy_order.index_id, receipt));
                }
                Err(e) => {
                    tracing::error!("  ✗ Buy order failed for index {}: {:?}", buy_order.index_id, e);
                    // Continue with other orders
                }
            }
        }

        Ok(SubmissionResult::Success {
            market_data_tx,
            buy_order_txs,
        })
    }

    /// Submit market data to IFactor::submitMarketData()
    async fn submit_market_data(&self, payload: &SubmissionPayload) -> eyre::Result<TxReceipt> {
        let _provider = self.provider.as_ref().unwrap();

        // Prepare data arrays
        let mut asset_ids = Vec::new();
        let mut liquidity = Vec::new();
        let mut prices = Vec::new();
        let mut slopes = Vec::new();

        // Sort by asset_id for consistency
        let mut assets: Vec<_> = payload.market_data.assets.values().collect();
        assets.sort_by_key(|a| a.asset_id);

        for asset in assets {
            asset_ids.push(asset.asset_id);
            liquidity.push(asset.liquidity.to_u128_raw());
            prices.push(asset.price.to_u128_raw());
            slopes.push(asset.slope.to_u128_raw());
        }

        // Convert to Labels and Vector
        let asset_names = Labels::from_vec_u128(asset_ids);
        let asset_liquidity = Vector::from_vec_u128(liquidity);
        let asset_prices = Vector::from_vec_u128(prices);
        let asset_slopes = Vector::from_vec_u128(slopes);

        // Build contract call
        let call = IFactor::submitMarketDataCall {
            vendor_id: self.config.vendor_id,
            asset_names: asset_names.to_vec().into(),
            asset_liquidity: asset_liquidity.to_vec().into(),
            asset_prices: asset_prices.to_vec().into(),
            asset_slopes: asset_slopes.to_vec().into(),
        };

        // Build transaction
        let tx = TransactionRequest::default()
            .to(self.config.castle_address)
            .input(call.abi_encode().into());

        // Send with retry
        let receipt = self.send_transaction_with_retry(tx).await?;

        Ok(receipt)
    }

    /// Submit buy order to IFactor::submitBuyOrder()
    async fn submit_buy_order(
        &self,
        payload: &SubmissionPayload,
        index_id: u128,
    ) -> eyre::Result<TxReceipt> {
        // Find the buy order
        let buy_order = payload
            .buy_orders
            .iter()
            .find(|o| o.index_id == index_id)
            .ok_or_else(|| eyre::eyre!("Buy order not found for index {}", index_id))?;

        // Prepare asset contribution fractions (ACF)
        // For now, we use equal contribution (1.0 for each asset)
        let acf_values: Vec<u128> = buy_order
            .asset_allocations
            .iter()
            .map(|_| common::amount::Amount::ONE.to_u128_raw())
            .collect();

        let asset_contribution_fractions = Vector::from_vec_u128(acf_values);

        // Build contract call
        // TODO see scenario5 in valutWorks and fill trader_address correctly
        let call = IFactor::submitBuyOrderCall {
            vendor_id: self.config.vendor_id,
            index_id,
            collateral_added: buy_order.collateral_added.to_u128_raw(),
            collateral_removed: buy_order.collateral_removed.to_u128_raw(),
            max_order_size: buy_order.max_order_size.to_u128_raw(),
            asset_contribution_fractions: asset_contribution_fractions.to_vec().into(),
            trader_address: Address::ZERO,
        };

        // Build transaction
        let tx = TransactionRequest::default()
            .to(self.config.castle_address)
            .input(call.abi_encode().into());

        // Send with retry
        let receipt = self.send_transaction_with_retry(tx).await?;

        Ok(receipt)
    }

    /// Send transaction with retry logic
    async fn send_transaction_with_retry(
        &self,
        tx: TransactionRequest,
    ) -> eyre::Result<TxReceipt> {
        let mut last_error = None;

        for attempt in 1..=self.config.retry_attempts {
            match self.try_send_transaction(tx.clone()).await {
                Ok(receipt) => {
                    return Ok(receipt);
                }
                Err(e) => {
                    tracing::warn!("Transaction attempt {}/{} failed: {:?}", attempt, self.config.retry_attempts, e);
                    last_error = Some(e);

                    if attempt < self.config.retry_attempts {
                        tokio::time::sleep(Duration::from_millis(self.config.retry_delay_ms)).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| eyre::eyre!("All retry attempts failed")))
    }

    /// Try to send a single transaction
    async fn try_send_transaction(
        &self,
        tx: TransactionRequest,
    ) -> eyre::Result<TxReceipt> {
        let provider = self.provider.as_ref().unwrap();

        // Send transaction and wait for receipt
        let pending_tx = provider.send_transaction(tx).await?;
        let tx_hash = *pending_tx.tx_hash();
        
        tracing::debug!("Transaction sent: {:?}", tx_hash);

        // Wait for confirmation
        let receipt = pending_tx.get_receipt().await?;

        Ok(TxReceipt {
            tx_hash: format!("{:?}", tx_hash),
            block_number: receipt.block_number.unwrap_or(0),
            gas_used: receipt.gas_used as u128,
            status: receipt.status(),
        })
    }

    /// Generate dry-run summary without submitting
    fn dry_run_summary(&self, payload: &SubmissionPayload) -> SubmissionResult {
        tracing::info!("🔍 DRY RUN MODE - No transactions will be submitted");
        
        let mut total_collateral = common::amount::Amount::ZERO;
        for buy_order in &payload.buy_orders {
            total_collateral = total_collateral
                .checked_add(buy_order.net_collateral_change())
                .unwrap();
        }

        let summary = SubmissionSummary {
            market_data_assets: payload.market_data.assets.len(),
            buy_orders_count: payload.buy_orders.len(),
            total_collateral,
        };

        tracing::info!("  Would submit market data for {} assets", summary.market_data_assets);
        tracing::info!("  Would submit {} buy orders", summary.buy_orders_count);
        tracing::info!(
            "  Total collateral: ${:.2}",
            summary.total_collateral.to_u128_raw() as f64 / 1e18
        );

        for buy_order in &payload.buy_orders {
            tracing::info!("\n  Index {} buy order:", buy_order.index_id);
            tracing::info!(
                "    Collateral change: ${:.2}",
                buy_order.net_collateral_change().to_u128_raw() as f64 / 1e18
            );
            tracing::info!("    Assets: {}", buy_order.asset_allocations.len());
        }

        SubmissionResult::DryRun {
            would_submit: summary,
        }
    }
}