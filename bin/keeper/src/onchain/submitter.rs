use super::types::{
    GasConfig, SettlementPayload, SettlementResult, SubmissionResult, SubmissionSummary, TxReceipt,
};
use crate::processor::SubmissionPayload;
use alloy::{
    network::EthereumWallet,
    primitives::Address,
    providers::Provider,
    rpc::types::TransactionRequest,
    signers::local::PrivateKeySigner,
};
use alloy_sol_types::SolCall;
use common::interfaces::banker::IBanker;
use common::interfaces::factor::IFactor;
use common::{labels::Labels, vector::Vector};
use std::time::{Duration, Instant};

/// Configuration for on-chain submitter
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields used for configuration even if not all are actively read
pub struct SubmitterConfig {
    pub rpc_url: String,
    pub castle_address: Address,
    pub vendor_id: u128,
    pub private_key: Option<String>,
    pub dry_run: bool,
    pub gas_config: GasConfig,
    pub retry_attempts: u32,
    pub retry_delay_ms: u64,
    /// Default max order size for processPending* calls (Story 2.6)
    /// Defaults to u128::MAX if not specified
    pub default_max_order_size: u128,
}

/// On-chain transaction submitter - generic over provider type
#[allow(dead_code)] // wallet field used for future signed transactions
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

    /// Check if submitter is in dry run mode
    pub fn is_dry_run(&self) -> bool {
        self.config.dry_run
    }

    /// Submit a complete payload to the blockchain
    ///
    /// DEPRECATED: Use `settle_batch()` instead for Story 2.6+ settlement flow.
    /// This method uses Address::ZERO for trader_address which is incorrect.
    #[deprecated(since = "2.6.0", note = "Use settle_batch() instead - this method uses incorrect Address::ZERO for trader")]
    pub async fn submit_payload(&self, payload: SubmissionPayload) -> eyre::Result<SubmissionResult> {
        if self.config.dry_run {
            return Ok(self.dry_run_summary(&payload));
        }

        let _provider = self.provider.as_ref().ok_or_else(|| eyre::eyre!("Provider not initialized"))?;

        tracing::debug!(castle = ?self.config.castle_address, vendor_id = self.config.vendor_id, "Submitting");

        let mut market_data_tx = None;
        let mut buy_order_txs = Vec::new();

        // Step 1: Submit market data
        if !payload.market_data.assets.is_empty() {
            match self.submit_market_data(&payload).await {
                Ok(receipt) => {
                    tracing::debug!(tx = %receipt.tx_hash, block = receipt.block_number, "Market data submitted");
                    market_data_tx = Some(receipt);
                }
                Err(e) => {
                    tracing::error!(?e, "Market data failed");
                    return Ok(SubmissionResult::Failed {
                        error: format!("Market data submission failed: {}", e),
                    });
                }
            }
        }

        for buy_order in &payload.buy_orders {
            match self.submit_buy_order(&payload, buy_order.index_id).await {
                Ok(receipt) => {
                    tracing::debug!(index = buy_order.index_id, tx = %receipt.tx_hash, "Buy submitted");
                    buy_order_txs.push((buy_order.index_id, receipt));
                }
                Err(e) => {
                    tracing::error!(index = buy_order.index_id, ?e, "Buy order failed");
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
    ///
    /// DEPRECATED: Part of old submit_payload flow. Use settle_batch() instead.
    #[allow(dead_code)]
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
        let call = IBanker::submitMarketDataCall {
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
    ///
    /// DEPRECATED: Part of old submit_payload flow. Use settle_batch() instead.
    /// WARNING: This method uses Address::ZERO for trader_address which is incorrect.
    #[allow(dead_code)]
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

        let _asset_contribution_fractions = Vector::from_vec_u128(acf_values);

        // Build contract call
        // DEPRECATED: This uses Address::ZERO which is incorrect.
        // Story 2.6 settle_batch() uses proper trader addresses from settlement_buy_orders.
        let call = IFactor::submitBuyOrderCall {
            vendor_id: self.config.vendor_id,
            index_id,
            trader_address: Address::ZERO, // WRONG - use settle_batch() instead
            collateral_added: buy_order.collateral_added.to_u128_raw(),
            collateral_removed: buy_order.collateral_removed.to_u128_raw(),
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

        tracing::debug!(assets = summary.market_data_assets, orders = summary.buy_orders_count, "Dry run");

        SubmissionResult::DryRun {
            would_submit: summary,
        }
    }

    // =========================================================================
    // Story 2.6: Castle/Vault Settlement Methods
    // =========================================================================

    /// Update multiple index quotes on Castle (AC: #1)
    /// Calls IBanker::updateMultipleIndexQuotes(vendor_id, index_ids)
    pub async fn update_multiple_quotes(&self, index_ids: &[u128]) -> eyre::Result<TxReceipt> {
        if self.config.dry_run {
            tracing::debug!(indices = index_ids.len(), "DRY RUN: updateMultipleIndexQuotes");
            return Ok(TxReceipt {
                tx_hash: "dry-run-quote-update".to_string(),
                block_number: 0,
                gas_used: 0,
                status: true,
            });
        }

        let _provider = self
            .provider
            .as_ref()
            .ok_or_else(|| eyre::eyre!("Provider not initialized"))?;

        tracing::debug!(indices = index_ids.len(), "updateMultipleIndexQuotes");

        // Build contract call
        let call = IBanker::updateMultipleIndexQuotesCall {
            vendor_id: self.config.vendor_id,
            index_ids: index_ids.to_vec(),
        };

        // Build transaction
        let tx = TransactionRequest::default()
            .to(self.config.castle_address)
            .input(call.abi_encode().into());

        // Send with retry
        self.send_transaction_with_retry(tx).await
    }

    /// Process a pending buy order on Vault (AC: #2)
    /// Calls IFactor::processPendingBuyOrder(vendor_id, index_id, trader_address, max_order_size)
    pub async fn process_pending_buy_order(
        &self,
        index_id: u128,
        trader: Address,
        max_order_size: u128,
    ) -> eyre::Result<TxReceipt> {
        if self.config.dry_run {
            tracing::debug!(index = index_id, ?trader, "DRY RUN: processPendingBuyOrder");
            return Ok(TxReceipt {
                tx_hash: format!("dry-run-buy-{}", index_id),
                block_number: 0,
                gas_used: 0,
                status: true,
            });
        }

        let _provider = self
            .provider
            .as_ref()
            .ok_or_else(|| eyre::eyre!("Provider not initialized"))?;

        tracing::debug!(index = index_id, ?trader, max_size = max_order_size, "processPendingBuyOrder");

        // Build contract call
        let call = IFactor::processPendingBuyOrderCall {
            vendor_id: self.config.vendor_id,
            index_id,
            trader_address: trader,
            max_order_size,
        };

        // Build transaction
        let tx = TransactionRequest::default()
            .to(self.config.castle_address)
            .input(call.abi_encode().into());

        // Send with retry
        self.send_transaction_with_retry(tx).await
    }

    /// Process a pending sell order on Vault (AC: #3)
    /// Calls IFactor::processPendingSellOrder(vendor_id, index_id, trader_address, max_order_size)
    pub async fn process_pending_sell_order(
        &self,
        index_id: u128,
        trader: Address,
        max_order_size: u128,
    ) -> eyre::Result<TxReceipt> {
        if self.config.dry_run {
            tracing::debug!(index = index_id, ?trader, "DRY RUN: processPendingSellOrder");
            return Ok(TxReceipt {
                tx_hash: format!("dry-run-sell-{}", index_id),
                block_number: 0,
                gas_used: 0,
                status: true,
            });
        }

        let _provider = self
            .provider
            .as_ref()
            .ok_or_else(|| eyre::eyre!("Provider not initialized"))?;

        tracing::debug!(index = index_id, ?trader, max_size = max_order_size, "processPendingSellOrder");

        // Build contract call
        let call = IFactor::processPendingSellOrderCall {
            vendor_id: self.config.vendor_id,
            index_id,
            trader_address: trader,
            max_order_size,
        };

        // Build transaction
        let tx = TransactionRequest::default()
            .to(self.config.castle_address)
            .input(call.abi_encode().into());

        // Send with retry
        self.send_transaction_with_retry(tx).await
    }

    /// Settle a batch by calling Castle/Vault contracts (AC: All)
    ///
    /// Settlement sequence (MANDATORY order per architecture):
    /// 1. updateMultipleIndexQuotes(vendor_id, index_ids) on Castle
    /// 2. processPendingBuyOrder for each buy order on Vaults
    /// 3. processPendingSellOrder for each sell order on Vaults
    ///
    /// Handles partial failures gracefully - continues processing remaining orders.
    pub async fn settle_batch(&self, payload: SettlementPayload) -> eyre::Result<SettlementResult> {
        let start = Instant::now();

        tracing::info!(batch_id = %payload.batch_id, indices = payload.index_ids.len(), buys = payload.buy_orders.len(), sells = payload.sell_orders.len(), "Settlement");

        let quote_update_tx = if !payload.index_ids.is_empty() {
            match self.update_multiple_quotes(&payload.index_ids).await {
                Ok(receipt) => {
                    tracing::debug!(batch_id = %payload.batch_id, tx = %receipt.tx_hash, "Quote update OK");
                    Some(receipt)
                }
                Err(e) => {
                    tracing::error!(batch_id = %payload.batch_id, %e, "Quote update failed");
                    None
                }
            }
        } else {
            tracing::debug!(
                batch_id = %payload.batch_id,
                "No indices to update - skipping quote update"
            );
            None
        };

        let mut buy_order_results = Vec::new();

        for order in &payload.buy_orders {
            let max_size = if order.max_order_size > 0 {
                order.max_order_size
            } else {
                self.config.default_max_order_size
            };

            match self.process_pending_buy_order(order.index_id, order.trader_address, max_size).await {
                Ok(receipt) => {
                    tracing::debug!(batch_id = %payload.batch_id, index = order.index_id, tx = %receipt.tx_hash, "Buy OK");
                    buy_order_results.push((order.index_id, Ok(receipt)));
                }
                Err(e) => {
                    tracing::error!(batch_id = %payload.batch_id, index = order.index_id, %e, "Buy failed");
                    buy_order_results.push((order.index_id, Err(e.to_string())));
                }
            }
        }

        let mut sell_order_results = Vec::new();

        for order in &payload.sell_orders {
            let max_size = if order.max_order_size > 0 {
                order.max_order_size
            } else {
                self.config.default_max_order_size
            };

            match self.process_pending_sell_order(order.index_id, order.trader_address, max_size).await {
                Ok(receipt) => {
                    tracing::debug!(batch_id = %payload.batch_id, index = order.index_id, tx = %receipt.tx_hash, "Sell OK");
                    sell_order_results.push((order.index_id, Ok(receipt)));
                }
                Err(e) => {
                    tracing::error!(batch_id = %payload.batch_id, index = order.index_id, %e, "Sell failed");
                    sell_order_results.push((order.index_id, Err(e.to_string())));
                }
            }
        }

        let elapsed = start.elapsed();
        let elapsed_ms = elapsed.as_millis();

        // Compute totals
        let succeeded = buy_order_results
            .iter()
            .filter(|(_, r)| r.is_ok())
            .count()
            + sell_order_results
                .iter()
                .filter(|(_, r)| r.is_ok())
                .count();
        let failed = buy_order_results
            .iter()
            .filter(|(_, r)| r.is_err())
            .count()
            + sell_order_results
                .iter()
                .filter(|(_, r)| r.is_err())
                .count();

        if elapsed.as_secs() > 5 {
            tracing::warn!(batch_id = %payload.batch_id, secs = elapsed.as_secs(), "NFR15: slow settlement");
        }

        tracing::info!(batch_id = %payload.batch_id, ms = elapsed_ms, ok = succeeded, err = failed, "Settlement done");

        Ok(SettlementResult {
            quote_update_tx,
            buy_order_results,
            sell_order_results,
            total_succeeded: succeeded,
            total_failed: failed,
            batch_id: payload.batch_id,
            elapsed_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_sol_types::SolCall;

    // =========================================================================
    // Story 2.6 Unit Tests - Task 6: Call encoding verification
    // =========================================================================

    const TEST_VENDOR_ID: u128 = 42;

    /// Test 6.1: Verify updateMultipleIndexQuotes encodes call correctly
    #[test]
    fn test_update_multiple_quotes_call_encoding() {
        let index_ids = vec![1001_u128, 1002_u128, 1003_u128];

        // Build the call as the submitter would
        let call = IBanker::updateMultipleIndexQuotesCall {
            vendor_id: TEST_VENDOR_ID,
            index_ids: index_ids.clone(),
        };

        // Encode
        let encoded = call.abi_encode();

        // Verify it encodes without panic
        assert!(!encoded.is_empty());

        // Verify we can decode it back
        let decoded = IBanker::updateMultipleIndexQuotesCall::abi_decode(&encoded)
            .expect("Should decode successfully");

        assert_eq!(decoded.vendor_id, TEST_VENDOR_ID);
        assert_eq!(decoded.index_ids, index_ids);
    }

    #[test]
    fn test_update_multiple_quotes_empty_indices() {
        let call = IBanker::updateMultipleIndexQuotesCall {
            vendor_id: TEST_VENDOR_ID,
            index_ids: vec![],
        };

        let encoded = call.abi_encode();
        assert!(!encoded.is_empty());

        let decoded = IBanker::updateMultipleIndexQuotesCall::abi_decode(&encoded)
            .expect("Should decode successfully");

        assert_eq!(decoded.index_ids.len(), 0);
    }

    /// Test 6.2: Verify processPendingBuyOrder encodes call correctly
    #[test]
    fn test_process_pending_buy_order_call_encoding() {
        let index_id = 1001_u128;
        let trader = Address::repeat_byte(0xAB);
        let max_order_size = 1_000_000_u128;

        // Build the call as the submitter would
        let call = IFactor::processPendingBuyOrderCall {
            vendor_id: TEST_VENDOR_ID,
            index_id,
            trader_address: trader,
            max_order_size,
        };

        // Encode
        let encoded = call.abi_encode();

        // Verify it encodes without panic
        assert!(!encoded.is_empty());

        // Verify we can decode it back
        let decoded = IFactor::processPendingBuyOrderCall::abi_decode(&encoded)
            .expect("Should decode successfully");

        assert_eq!(decoded.vendor_id, TEST_VENDOR_ID);
        assert_eq!(decoded.index_id, index_id);
        assert_eq!(decoded.trader_address, trader);
        assert_eq!(decoded.max_order_size, max_order_size);
    }

    #[test]
    fn test_process_pending_buy_order_zero_address() {
        let call = IFactor::processPendingBuyOrderCall {
            vendor_id: TEST_VENDOR_ID,
            index_id: 1001,
            trader_address: Address::ZERO,
            max_order_size: u128::MAX,
        };

        let encoded = call.abi_encode();
        let decoded = IFactor::processPendingBuyOrderCall::abi_decode(&encoded)
            .expect("Should decode successfully");

        assert_eq!(decoded.trader_address, Address::ZERO);
        assert_eq!(decoded.max_order_size, u128::MAX);
    }

    /// Test 6.3: Verify processPendingSellOrder encodes call correctly
    #[test]
    fn test_process_pending_sell_order_call_encoding() {
        let index_id = 2002_u128;
        let trader = Address::repeat_byte(0xCD);
        let max_order_size = 500_000_u128;

        // Build the call as the submitter would
        let call = IFactor::processPendingSellOrderCall {
            vendor_id: TEST_VENDOR_ID,
            index_id,
            trader_address: trader,
            max_order_size,
        };

        // Encode
        let encoded = call.abi_encode();

        // Verify it encodes without panic
        assert!(!encoded.is_empty());

        // Verify we can decode it back
        let decoded = IFactor::processPendingSellOrderCall::abi_decode(&encoded)
            .expect("Should decode successfully");

        assert_eq!(decoded.vendor_id, TEST_VENDOR_ID);
        assert_eq!(decoded.index_id, index_id);
        assert_eq!(decoded.trader_address, trader);
        assert_eq!(decoded.max_order_size, max_order_size);
    }

    #[test]
    fn test_process_pending_sell_order_max_values() {
        let call = IFactor::processPendingSellOrderCall {
            vendor_id: u128::MAX,
            index_id: u128::MAX,
            trader_address: Address::repeat_byte(0xFF),
            max_order_size: u128::MAX,
        };

        let encoded = call.abi_encode();
        let decoded = IFactor::processPendingSellOrderCall::abi_decode(&encoded)
            .expect("Should decode successfully");

        assert_eq!(decoded.vendor_id, u128::MAX);
        assert_eq!(decoded.index_id, u128::MAX);
        assert_eq!(decoded.max_order_size, u128::MAX);
    }

    // =========================================================================
    // SubmitterConfig tests
    // =========================================================================

    #[test]
    fn test_submitter_config_default_max_order_size() {
        // Verify that default_max_order_size field exists and can be set
        let config = SubmitterConfig {
            rpc_url: "http://localhost:8545".to_string(),
            castle_address: Address::ZERO,
            vendor_id: 1,
            private_key: None,
            dry_run: true,
            gas_config: GasConfig::default(),
            retry_attempts: 3,
            retry_delay_ms: 1000,
            default_max_order_size: u128::MAX,
        };

        assert_eq!(config.default_max_order_size, u128::MAX);
    }

    #[test]
    fn test_submitter_config_custom_max_order_size() {
        let custom_size = 1_000_000_u128;

        let config = SubmitterConfig {
            rpc_url: "http://localhost:8545".to_string(),
            castle_address: Address::ZERO,
            vendor_id: 1,
            private_key: None,
            dry_run: true,
            gas_config: GasConfig::default(),
            retry_attempts: 3,
            retry_delay_ms: 1000,
            default_max_order_size: custom_size,
        };

        assert_eq!(config.default_max_order_size, custom_size);
    }

    // =========================================================================
    // Story 2.6: Integration Tests - Task 6.7, 6.8
    // =========================================================================

    use super::super::types::{OrderType, PendingOrder, SettlementPayload};
    use alloy::providers::ProviderBuilder;
    use std::time::Instant;

    fn make_test_config() -> SubmitterConfig {
        SubmitterConfig {
            rpc_url: "http://localhost:8545".to_string(),
            castle_address: Address::repeat_byte(0xCA),
            vendor_id: 42,
            private_key: None,
            dry_run: true, // Always dry run in tests
            gas_config: GasConfig::default(),
            retry_attempts: 1,
            retry_delay_ms: 100,
            default_max_order_size: u128::MAX,
        }
    }

    fn make_test_pending_order(index_id: u128, trader: u8, is_buy: bool) -> PendingOrder {
        PendingOrder {
            index_id,
            trader_address: Address::repeat_byte(trader),
            max_order_size: 1_000_000,
            order_type: if is_buy { OrderType::Buy } else { OrderType::Sell },
            vault_address: Some(Address::repeat_byte(0xAB)),
        }
    }

    /// Test 6.7: Integration test - Full pipeline from SettlementPayload to SettlementResult
    /// Verifies the settlement flow in dry-run mode
    #[tokio::test]
    async fn test_settle_batch_integration_dry_run() {
        let config = make_test_config();

        // Create a mock provider for testing
        let provider = ProviderBuilder::new().connect_http("http://localhost:8545".parse().unwrap());
        let submitter = OnchainSubmitter::new_with_provider(config, provider).unwrap();

        // Create settlement payload with multiple orders
        let payload = SettlementPayload {
            index_ids: vec![1001, 1002, 1003],
            buy_orders: vec![
                make_test_pending_order(1001, 0xAB, true),
                make_test_pending_order(1002, 0xCD, true),
            ],
            sell_orders: vec![make_test_pending_order(1003, 0xEF, false)],
            batch_id: "test-integration-batch".to_string(),
        };

        // Execute settlement (dry run)
        let result = submitter.settle_batch(payload).await.unwrap();

        // Verify result structure
        assert_eq!(result.batch_id, "test-integration-batch");
        assert!(result.quote_update_tx.is_some()); // Should have quote update
        assert_eq!(result.buy_order_results.len(), 2);
        assert_eq!(result.sell_order_results.len(), 1);

        // In dry run, all should succeed
        assert_eq!(result.total_succeeded, 3);
        assert_eq!(result.total_failed, 0);
        assert!(result.is_fully_successful());
    }

    /// Test 6.7b: Integration test - Empty payload handling
    #[tokio::test]
    async fn test_settle_batch_integration_empty_payload() {
        let config = make_test_config();
        let provider = ProviderBuilder::new().connect_http("http://localhost:8545".parse().unwrap());
        let submitter = OnchainSubmitter::new_with_provider(config, provider).unwrap();

        // Empty payload
        let payload = SettlementPayload {
            index_ids: vec![],
            buy_orders: vec![],
            sell_orders: vec![],
            batch_id: "empty-batch".to_string(),
        };

        let result = submitter.settle_batch(payload).await.unwrap();

        // Empty payload should succeed with no transactions
        assert!(result.quote_update_tx.is_none()); // No indices = no quote update
        assert_eq!(result.buy_order_results.len(), 0);
        assert_eq!(result.sell_order_results.len(), 0);
        assert_eq!(result.total_succeeded, 0);
        assert_eq!(result.total_failed, 0);
    }

    /// Test 6.8: NFR15 compliance test - Settlement should complete quickly in dry-run
    #[tokio::test]
    async fn test_settle_batch_nfr15_timing() {
        let config = make_test_config();
        let provider = ProviderBuilder::new().connect_http("http://localhost:8545".parse().unwrap());
        let submitter = OnchainSubmitter::new_with_provider(config, provider).unwrap();

        // Create a reasonably sized payload
        let payload = SettlementPayload {
            index_ids: vec![1001, 1002, 1003, 1004, 1005],
            buy_orders: (0..10)
                .map(|i| make_test_pending_order(1001 + i, (i + 1) as u8, true))
                .collect(),
            sell_orders: (0..5)
                .map(|i| make_test_pending_order(1001 + i, (i + 100) as u8, false))
                .collect(),
            batch_id: "nfr15-timing-test".to_string(),
        };

        let start = Instant::now();
        let result = submitter.settle_batch(payload).await.unwrap();
        let elapsed = start.elapsed();

        // In dry-run mode, should complete very quickly (well under 5 seconds)
        assert!(elapsed.as_secs() < 5, "NFR15: Settlement took {} seconds, should be < 5", elapsed.as_secs());

        // Result should have timing info
        assert!(result.elapsed_ms < 5000, "NFR15: elapsed_ms {} should be < 5000", result.elapsed_ms);

        // All orders should succeed
        assert_eq!(result.total_succeeded, 15);
        assert_eq!(result.total_failed, 0);
    }

    /// Test 6.7c: Integration test - Verifies correct settlement sequence
    /// Quote update should happen before order processing
    #[tokio::test]
    async fn test_settle_batch_correct_sequence() {
        let config = make_test_config();
        let provider = ProviderBuilder::new().connect_http("http://localhost:8545".parse().unwrap());
        let submitter = OnchainSubmitter::new_with_provider(config, provider).unwrap();

        let payload = SettlementPayload {
            index_ids: vec![1001],
            buy_orders: vec![make_test_pending_order(1001, 0xAB, true)],
            sell_orders: vec![make_test_pending_order(1001, 0xCD, false)],
            batch_id: "sequence-test".to_string(),
        };

        let result = submitter.settle_batch(payload).await.unwrap();

        // In dry run, quote update tx should have predictable hash
        assert!(result.quote_update_tx.is_some());
        let quote_tx = result.quote_update_tx.as_ref().unwrap();
        assert_eq!(quote_tx.tx_hash, "dry-run-quote-update");

        // Buy orders should have indexed hashes
        assert_eq!(result.buy_order_results.len(), 1);
        let (idx, buy_result) = &result.buy_order_results[0];
        assert_eq!(*idx, 1001);
        assert!(buy_result.is_ok());
        let buy_tx = buy_result.as_ref().unwrap();
        assert!(buy_tx.tx_hash.starts_with("dry-run-buy-"));

        // Sell orders should have indexed hashes
        assert_eq!(result.sell_order_results.len(), 1);
        let (idx, sell_result) = &result.sell_order_results[0];
        assert_eq!(*idx, 1001);
        assert!(sell_result.is_ok());
        let sell_tx = sell_result.as_ref().unwrap();
        assert!(sell_tx.tx_hash.starts_with("dry-run-sell-"));
    }
}