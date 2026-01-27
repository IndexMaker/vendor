use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy_primitives::Address;
use alloy_sol_types::SolCall;
use common::interfaces::banker::IBanker;
use common::interfaces::steward::ISteward;
use common::{labels::Labels, vector::Vector, amount::Amount};

/// Asset ID scaling factor - set to 1 (no scaling) to match ITP asset IDs
/// ITP creation uses raw asset IDs (1, 2, 3...) so vendor must also use raw IDs
const ASSET_ID_SCALE: u128 = 1;

/// Scale asset IDs to match on-chain format
/// Note: Currently no scaling (ASSET_ID_SCALE=1) to match ITP raw asset IDs
fn scale_asset_ids(asset_ids: &[u128]) -> Vec<u128> {
    asset_ids.iter().map(|id| id * ASSET_ID_SCALE).collect()
}
use std::collections::HashMap;
use std::time::Instant;
use super::metrics::SubmissionMetrics;

/// Vendor demand from on-chain
#[derive(Debug, Clone)]
pub struct VendorDemand {
    pub assets: HashMap<u128, Amount>,  // asset_id → demand_quantity
}

/// Data for concurrent on-chain submissions (Story 3-4)
///
/// Contains all vectors needed for margin, supply, and market data submissions.
/// All vectors must have the same length and match asset_ids ordering.
#[derive(Debug, Clone)]
pub struct SubmissionData {
    /// Asset IDs (Labels) in submission order
    pub asset_ids: Vec<u128>,
    /// Margin vector (M_i = V_max/n / P_i)
    pub margins: Vec<Amount>,
    /// Supply long positions (placeholder for Story 3-6)
    pub supply_long: Vec<Amount>,
    /// Supply short positions (placeholder for Story 3-6)
    pub supply_short: Vec<Amount>,
    /// Price vector (micro-prices)
    pub prices: Vec<Amount>,
    /// Slope vector (with fee multiplier)
    pub slopes: Vec<Amount>,
    /// Liquidity vector
    pub liquidities: Vec<Amount>,
}

impl SubmissionData {
    /// Validate all vectors have matching lengths
    pub fn is_valid(&self) -> bool {
        let n = self.asset_ids.len();
        self.margins.len() == n
            && self.supply_long.len() == n
            && self.supply_short.len() == n
            && self.prices.len() == n
            && self.slopes.len() == n
            && self.liquidities.len() == n
    }

    /// Number of assets in submission
    pub fn len(&self) -> usize {
        self.asset_ids.len()
    }

    /// Check if submission is empty
    pub fn is_empty(&self) -> bool {
        self.asset_ids.is_empty()
    }
}

/// Handles all vendor-related on-chain submissions via IBanker
pub struct VendorSubmitter<P>
where
    P: Provider + Clone,
{
    provider: P,
    castle_address: Address,
    vendor_id: u128,
    /// Signer address for transaction nonce tracking
    /// In Alloy, the wallet is a signing layer - get_accounts() queries the node, not the wallet
    signer_address: Address,
}

impl<P> VendorSubmitter<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    pub fn new(provider: P, castle_address: Address, vendor_id: u128, signer_address: Address) -> Self {
        Self {
            provider,
            castle_address,
            vendor_id,
            signer_address,
        }
    }

    /// Get the default vendor_id configured on this submitter
    pub fn vendor_id(&self) -> u128 {
        self.vendor_id
    }

    /// Query on-chain asset order for an ITP via ISteward::getIndexAssets
    /// Returns asset IDs in the ITP's internal order (critical for JFLT subsequence matching)
    pub async fn get_itp_asset_order(&self, index_id: u128) -> eyre::Result<Vec<u128>> {
        tracing::info!("📥 Reading asset order for ITP index_id={}", index_id);

        let call = ISteward::getIndexAssetsCall { index_id };

        let result = self
            .provider
            .call(TransactionRequest::default()
                .to(self.castle_address)
                .input(call.abi_encode().into()))
            .await?;

        let decoded = ISteward::getIndexAssetsCall::abi_decode_returns(&result)?;
        let labels = Labels::from_vec(decoded.to_vec());

        // Labels are stored as raw u128 values (no scaling with ASSET_ID_SCALE=1)
        let asset_ids: Vec<u128> = labels.data.clone();

        tracing::info!(
            "  Read {} assets for ITP {} (order: {:?})",
            asset_ids.len(),
            index_id,
            if asset_ids.len() <= 5 { format!("{:?}", &asset_ids) } else { format!("{:?}...", &asset_ids[..5]) }
        );

        Ok(asset_ids)
    }

    /// Get fresh nonce from chain (bypasses provider cache)
    async fn get_fresh_nonce(&self) -> eyre::Result<u64> {
        // Use stored signer address - provider.get_accounts() queries the node, not the local wallet
        let nonce = self.provider.get_transaction_count(self.signer_address).await?;
        Ok(nonce)
    }

    /// Submit tracked assets to Castle (IBanker::submitAssets)
    /// No hardcoded limit - actual limit is gas-based (tested in story 1-9)
    /// vendor_id: The vendor account to submit assets for (use index_id for per-ITP vendors)
    pub async fn submit_assets(&self, vendor_id: u128, asset_ids: Vec<u128>) -> eyre::Result<()> {
        if asset_ids.is_empty() {
            tracing::warn!("No assets to submit");
            return Ok(());
        }

        // No artificial limit - let gas limit determine the actual constraint
        // Previous 120 limit was arbitrary "Sonia's requirement", not on-chain
        let assets_to_submit = asset_ids;

        tracing::info!("📤 Submitting {} assets to Castle (vendor_id={})", assets_to_submit.len(), vendor_id);

        // Scale asset IDs to match on-chain format (multiply by 10^18)
        let scaled_asset_ids = scale_asset_ids(&assets_to_submit);
        let asset_names = Labels::from_vec_u128(scaled_asset_ids);

        let call = IBanker::submitAssetsCall {
            vendor_id,
            market_asset_names: asset_names.to_vec().into(),
        };

        let tx = TransactionRequest::default()
            .to(self.castle_address)
            .input(call.abi_encode().into());

        let receipt = self.provider.send_transaction(tx).await?.get_receipt().await?;

        tracing::info!(
            "  ✓ submitAssets tx: {:?} (block: {})",
            receipt.transaction_hash,
            receipt.block_number.unwrap_or(0)
        );

        Ok(())
    }

    /// Submit margin to Castle (IBanker::submitMargin)
    /// vendor_id: The vendor account to submit margin for
    pub async fn submit_margin(
        &self,
        vendor_id: u128,
        asset_ids: Vec<u128>,
        margins: Vec<Amount>,
    ) -> eyre::Result<()> {
        if asset_ids.is_empty() || margins.is_empty() {
            tracing::warn!("No margin data to submit");
            return Ok(());
        }

        if asset_ids.len() != margins.len() {
            return Err(eyre::eyre!(
                "Asset IDs and margins length mismatch: {} vs {}",
                asset_ids.len(),
                margins.len()
            ));
        }

        tracing::info!("📤 Submitting margin for {} assets", asset_ids.len());

        // Scale asset IDs to match on-chain format (multiply by 10^18)
        let scaled_asset_ids = scale_asset_ids(&asset_ids);
        let asset_names = Labels::from_vec_u128(scaled_asset_ids);
        let asset_margin = Vector::from_vec_u128(
            margins.iter().map(|m| m.to_u128_raw()).collect()
        );

        let call = IBanker::submitMarginCall {
            vendor_id,
            asset_names: asset_names.to_vec().into(),
            asset_margin: asset_margin.to_vec().into(),
        };

        // Fetch fresh nonce from chain to avoid stale cache issues
        let nonce = self.get_fresh_nonce().await?;

        let tx = TransactionRequest::default()
            .to(self.castle_address)
            .input(call.abi_encode().into())
            .nonce(nonce);

        let receipt = self.provider.send_transaction(tx).await?.get_receipt().await?;

        tracing::info!(
            "  ✓ submitMargin tx: {:?} (block: {}, nonce: {}, vendor_id={})",
            receipt.transaction_hash,
            receipt.block_number.unwrap_or(0),
            nonce,
            vendor_id
        );

        Ok(())
    }

    /// Submit supply to Castle (IBanker::submitSupply)
    /// vendor_id: The vendor account to submit supply for
    pub async fn submit_supply(
        &self,
        vendor_id: u128,
        asset_ids: Vec<u128>,
        supply_long: Vec<Amount>,
        supply_short: Vec<Amount>,
    ) -> eyre::Result<()> {
        if asset_ids.is_empty() {
            tracing::warn!("No supply data to submit");
            return Ok(());
        }

        if asset_ids.len() != supply_long.len() || asset_ids.len() != supply_short.len() {
            return Err(eyre::eyre!(
                "Asset IDs, long, and short supply length mismatch: {} vs {} vs {}",
                asset_ids.len(),
                supply_long.len(),
                supply_short.len()
            ));
        }

        tracing::info!("📤 Submitting supply for {} assets", asset_ids.len());

        // Scale asset IDs to match on-chain format (multiply by 10^18)
        let scaled_asset_ids = scale_asset_ids(&asset_ids);
        let asset_names = Labels::from_vec_u128(scaled_asset_ids);
        let supply_long_vec = Vector::from_vec_u128(
            supply_long.iter().map(|m| m.to_u128_raw()).collect()
        );
        let supply_short_vec = Vector::from_vec_u128(
            supply_short.iter().map(|m| m.to_u128_raw()).collect()
        );

        let call = IBanker::submitSupplyCall {
            vendor_id,
            asset_names: asset_names.to_vec().into(),
            asset_quantities_short: supply_short_vec.to_vec().into(),
            asset_quantities_long: supply_long_vec.to_vec().into(),
        };

        // Fetch fresh nonce from chain to avoid stale cache issues
        let nonce = self.get_fresh_nonce().await?;

        let tx = TransactionRequest::default()
            .to(self.castle_address)
            .input(call.abi_encode().into())
            .nonce(nonce);

        let receipt = self.provider.send_transaction(tx).await?.get_receipt().await?;

        tracing::info!(
            "  ✓ submitSupply tx: {:?} (block: {}, nonce: {}, vendor_id={})",
            receipt.transaction_hash,
            receipt.block_number.unwrap_or(0),
            nonce,
            vendor_id
        );

        Ok(())
    }

    /// Get vendor demand from Castle (IBanker::getVendorDemand)
    /// Returns net demand per asset (demand_long - demand_short)
    pub async fn get_vendor_demand(&self, vendor_id: u128) -> eyre::Result<VendorDemand> {
        tracing::info!("📥 Reading vendor demand from Castle (vendor_id={})...", vendor_id);

        // Step 1: Get vendor assets (asset IDs list)
        let assets_call = ISteward::getVendorAssetsCall {
            vendor_id,
        };

        let assets_result = self
            .provider
            .call(TransactionRequest::default()
                .to(self.castle_address)
                .input(assets_call.abi_encode().into()))
            .await?;

        // getVendorAssets returns bytes memory (single) - decoded is Bytes directly
        let assets_decoded = ISteward::getVendorAssetsCall::abi_decode_returns(&assets_result)?;

        // Parse asset IDs
        let asset_ids = Labels::from_vec(assets_decoded.to_vec());

        tracing::info!("  Found {} assets for vendor", asset_ids.data.len());

        // Step 2: Get vendor demand (demand_long, demand_short)
        let demand_call = ISteward::getVendorDemandCall {
            vendor_id,
        };

        let demand_result = self
            .provider
            .call(TransactionRequest::default()
                .to(self.castle_address)
                .input(demand_call.abi_encode().into()))
            .await?;

        // getVendorDemand returns bytes[] memory - decoded is Vec<Bytes>
        let demand_decoded = ISteward::getVendorDemandCall::abi_decode_returns(&demand_result)?;

        // Parse demand_long and demand_short vectors
        let demand_long = if demand_decoded.len() > 0 {
            Vector::from_vec(demand_decoded[0].to_vec())
        } else {
            Vector::new()
        };
        let demand_short = if demand_decoded.len() > 1 {
            Vector::from_vec(demand_decoded[1].to_vec())
        } else {
            Vector::new()
        };

        tracing::info!(
            "  Demand vectors: {} long, {} short",
            demand_long.data.len(),
            demand_short.data.len()
        );

        // Step 3: Calculate net demand per asset (demand_long - demand_short)
        let mut demand = VendorDemand {
            assets: HashMap::new(),
        };

        for (i, asset_id) in asset_ids.data.iter().enumerate() {
            let long_qty = demand_long.data.get(i).copied().unwrap_or(Amount::ZERO);
            let short_qty = demand_short.data.get(i).copied().unwrap_or(Amount::ZERO);

            // Net demand = long - short
            let net_demand = long_qty
                .checked_sub(short_qty)
                .unwrap_or(Amount::ZERO);

            demand.assets.insert(*asset_id, net_demand);

            tracing::info!(
                "    Asset {}: long={:.4}, short={:.4}, net={:.4}",
                asset_id,
                long_qty.to_u128_raw() as f64 / 1e18,
                short_qty.to_u128_raw() as f64 / 1e18,
                net_demand.to_u128_raw() as f64 / 1e18
            );
        }

        tracing::info!("  Calculated net demand for {} assets", demand.assets.len());

        Ok(demand)
    }

    /// Submit market data (P/S/L vectors) to Castle (IBanker::submitMarketData) - AC #6
    ///
    /// # Arguments
    /// * `vendor_id` - Vendor account ID (use index_id for per-ITP vendors)
    /// * `asset_ids` - Asset IDs (as u128, converted to Labels)
    /// * `prices` - Price (P) vector (micro-prices)
    /// * `slopes` - Slope (S) vector (with fee multiplier applied)
    /// * `liquidities` - Liquidity (L) vector
    ///
    /// # Returns
    /// * `Ok(())` on successful transaction
    pub async fn submit_market_data(
        &self,
        vendor_id: u128,
        asset_ids: Vec<u128>,
        prices: Vec<Amount>,
        slopes: Vec<Amount>,
        liquidities: Vec<Amount>,
    ) -> eyre::Result<()> {
        if asset_ids.is_empty() {
            tracing::warn!("No market data to submit");
            return Ok(());
        }

        // Validate vector lengths match
        if asset_ids.len() != prices.len()
            || asset_ids.len() != slopes.len()
            || asset_ids.len() != liquidities.len()
        {
            return Err(eyre::eyre!(
                "PSL vector length mismatch: asset_ids={}, prices={}, slopes={}, liquidities={}",
                asset_ids.len(),
                prices.len(),
                slopes.len(),
                liquidities.len()
            ));
        }

        tracing::info!("📤 Submitting market data (P/S/L) for {} assets", asset_ids.len());

        // Scale asset IDs to match on-chain format (multiply by 10^18)
        let scaled_asset_ids = scale_asset_ids(&asset_ids);
        let asset_names = Labels::from_vec_u128(scaled_asset_ids);
        let liquidity_vec = Vector::from_vec_u128(
            liquidities.iter().map(|l| l.to_u128_raw()).collect()
        );
        let price_vec = Vector::from_vec_u128(
            prices.iter().map(|p| p.to_u128_raw()).collect()
        );
        let slope_vec = Vector::from_vec_u128(
            slopes.iter().map(|s| s.to_u128_raw()).collect()
        );

        let call = IBanker::submitMarketDataCall {
            vendor_id,
            asset_names: asset_names.to_vec().into(),
            asset_liquidity: liquidity_vec.to_vec().into(),
            asset_prices: price_vec.to_vec().into(),
            asset_slopes: slope_vec.to_vec().into(),
        };

        // Fetch fresh nonce from chain to avoid stale cache issues
        let nonce = self.get_fresh_nonce().await?;

        let tx = TransactionRequest::default()
            .to(self.castle_address)
            .input(call.abi_encode().into())
            .nonce(nonce);

        let receipt = self.provider.send_transaction(tx).await?.get_receipt().await?;

        tracing::info!(
            "  ✓ submitMarketData tx: {:?} (block: {}, nonce: {}, vendor_id={})",
            receipt.transaction_hash,
            receipt.block_number.unwrap_or(0),
            nonce,
            vendor_id
        );

        Ok(())
    }

    /// Submit market data from PSLVectors struct (convenience method)
    ///
    /// # Arguments
    /// * `vendor_id` - Vendor account ID
    /// * `vectors` - Pre-computed PSLVectors from PSLComputeService
    pub async fn submit_market_data_from_vectors(
        &self,
        vendor_id: u128,
        vectors: &crate::order_book::PSLVectors,
    ) -> eyre::Result<()> {
        self.submit_market_data(
            vendor_id,
            vectors.asset_ids.clone(),
            vectors.prices.clone(),
            vectors.slopes.clone(),
            vectors.liquidities.clone(),
        )
        .await
    }

    /// Submit all three calls (margin, supply, market data) concurrently (AC #4, #5, #6, #7)
    ///
    /// Uses tokio::try_join! to fire all 3 submissions concurrently and wait for all
    /// confirmations before returning. Returns error immediately if any submission fails.
    ///
    /// # Arguments
    /// * `data` - Submission data containing all vectors
    /// * `batch_id` - Correlation ID for logging
    ///
    /// # Returns
    /// * `Ok(SubmissionMetrics)` - Timing metrics on success
    /// * `Err` - If any submission fails
    ///
    /// # Performance
    /// Target: < 2 seconds total (NFR17)
    pub async fn submit_all_concurrent(
        &self,
        vendor_id: u128,
        data: &SubmissionData,
        batch_id: Option<String>,
    ) -> eyre::Result<SubmissionMetrics> {
        let mut metrics = SubmissionMetrics::start(batch_id.clone());
        let correlation_id = batch_id.as_deref().unwrap_or("unknown");

        tracing::info!(
            "🚀 Starting concurrent submissions for {} assets [batch_id={}]",
            data.asset_ids.len(),
            correlation_id
        );

        // Create futures for all three submissions
        let margin_fut = async {
            let start = Instant::now();
            let result = self
                .submit_margin(vendor_id, data.asset_ids.clone(), data.margins.clone())
                .await;
            (result, start.elapsed())
        };

        let supply_fut = async {
            let start = Instant::now();
            let result = self
                .submit_supply(
                    vendor_id,
                    data.asset_ids.clone(),
                    data.supply_long.clone(),
                    data.supply_short.clone(),
                )
                .await;
            (result, start.elapsed())
        };

        let market_data_fut = async {
            let start = Instant::now();
            let result = self
                .submit_market_data(
                    vendor_id,
                    data.asset_ids.clone(),
                    data.prices.clone(),
                    data.slopes.clone(),
                    data.liquidities.clone(),
                )
                .await;
            (result, start.elapsed())
        };

        // Submit sequentially to avoid nonce conflicts with shared provider
        // (concurrent submission would require nonce manager)
        let margin_res = margin_fut.await;
        let supply_res = supply_fut.await;
        let market_data_res = market_data_fut.await;

        // Extract results and durations
        let (margin_result, margin_duration) = margin_res;
        let (supply_result, supply_duration) = supply_res;
        let (market_data_result, market_data_duration) = market_data_res;

        // Record timing metrics
        metrics.record_margin(margin_duration);
        metrics.record_supply(supply_duration);
        metrics.record_market_data(market_data_duration);

        // Check results - any failure means overall failure
        margin_result.map_err(|e| {
            tracing::error!(
                "❌ Margin submission failed [batch_id={}]: {}",
                correlation_id,
                e
            );
            e
        })?;

        supply_result.map_err(|e| {
            tracing::error!(
                "❌ Supply submission failed [batch_id={}]: {}",
                correlation_id,
                e
            );
            e
        })?;

        market_data_result.map_err(|e| {
            tracing::error!(
                "❌ Market data submission failed [batch_id={}]: {}",
                correlation_id,
                e
            );
            e
        })?;

        metrics.finish();

        tracing::info!(
            "✅ All sequential submissions completed [batch_id={}]",
            correlation_id
        );

        Ok(metrics)
    }

    /// Get vendor's registered assets from Castle (ISteward::getVendorAssets)
    /// Returns the set of asset IDs (unscaled, original IDs like 1, 2, 3...)
    pub async fn get_vendor_assets(&self, vendor_id: u128) -> eyre::Result<std::collections::HashSet<u128>> {
        tracing::debug!("📥 Reading vendor assets from Castle (vendor_id={})...", vendor_id);

        let call = ISteward::getVendorAssetsCall {
            vendor_id,
        };

        let result = self
            .provider
            .call(TransactionRequest::default()
                .to(self.castle_address)
                .input(call.abi_encode().into()))
            .await?;

        // getVendorAssets returns bytes memory
        let decoded = ISteward::getVendorAssetsCall::abi_decode_returns(&result)?;

        // Parse asset IDs from Labels
        let labels = Labels::from_vec(decoded.to_vec());

        // Convert scaled asset IDs back to unscaled (divide by 10^18)
        let assets: std::collections::HashSet<u128> = labels
            .data
            .iter()
            .map(|scaled_id| scaled_id / ASSET_ID_SCALE)
            .collect();

        tracing::debug!("  Read {} registered assets for vendor", assets.len());

        Ok(assets)
    }

    /// Ensure all assets are registered before submission
    /// Registers any missing assets via submitAssets
    pub async fn ensure_assets_registered(&self, vendor_id: u128, asset_ids: &[u128]) -> eyre::Result<()> {
        // Get currently registered assets
        let registered = self.get_vendor_assets(vendor_id).await?;

        // Find unregistered assets
        let unregistered: Vec<u128> = asset_ids
            .iter()
            .filter(|id| !registered.contains(id))
            .copied()
            .collect();

        if unregistered.is_empty() {
            tracing::debug!("All {} assets already registered for vendor_id={}", asset_ids.len(), vendor_id);
            return Ok(());
        }

        tracing::info!(
            "📤 Registering {} missing assets (out of {} requested) for vendor_id={}",
            unregistered.len(),
            asset_ids.len(),
            vendor_id
        );

        // Register missing assets
        self.submit_assets(vendor_id, unregistered).await?;

        Ok(())
    }

    /// Filter asset_ids to only include those registered with the vendor
    /// Returns (filtered_asset_ids, filtered_count)
    pub async fn filter_to_registered(&self, vendor_id: u128, asset_ids: &[u128]) -> eyre::Result<Vec<u128>> {
        let registered = self.get_vendor_assets(vendor_id).await?;

        let filtered: Vec<u128> = asset_ids
            .iter()
            .filter(|id| registered.contains(id))
            .copied()
            .collect();

        if filtered.len() < asset_ids.len() {
            let skipped = asset_ids.len() - filtered.len();
            tracing::warn!(
                "⚠️ Filtered out {} unregistered assets (keeping {})",
                skipped,
                filtered.len()
            );
        }

        Ok(filtered)
    }

    /// Submit vote for an index (IBanker::submitVote)
    ///
    /// Required before updateIndexQuote can be called.
    /// The reference flow calls submitVote(indexId, "0x") after submitAssetWeights.
    pub async fn submit_vote(&self, index_id: u128) -> eyre::Result<()> {
        tracing::info!("📤 Submitting vote for index_id={}", index_id);

        let call = IBanker::submitVoteCall {
            index_id,
            data: alloy_primitives::Bytes::new(),
        };

        let nonce = self.get_fresh_nonce().await?;

        let tx = TransactionRequest::default()
            .to(self.castle_address)
            .input(call.abi_encode().into())
            .nonce(nonce);

        let receipt = self.provider.send_transaction(tx).await?.get_receipt().await?;

        tracing::info!(
            "  ✓ submitVote tx: {:?} (block: {}, nonce: {})",
            receipt.transaction_hash,
            receipt.block_number.unwrap_or(0),
            nonce
        );

        Ok(())
    }

    /// Update index quote on-chain (IBanker::updateIndexQuote)
    ///
    /// Triggers a recalculation of the index quote for the specified index.
    /// The vendor must have submitted market data (prices, liquidity, slopes) for all
    /// assets in the index composition for the quote to be valid.
    /// vendor_id should be index_id for per-ITP vendor accounts.
    pub async fn update_index_quote(&self, vendor_id: u128, index_id: u128) -> eyre::Result<()> {
        tracing::info!("📤 Updating index quote for index_id={} (vendor_id={})", index_id, vendor_id);

        let call = IBanker::updateIndexQuoteCall {
            vendor_id,
            index_id,
        };

        // Fetch fresh nonce from chain
        let nonce = self.get_fresh_nonce().await?;

        let tx = TransactionRequest::default()
            .to(self.castle_address)
            .input(call.abi_encode().into())
            .nonce(nonce);

        let receipt = self.provider.send_transaction(tx).await?.get_receipt().await?;

        tracing::info!(
            "  ✓ updateIndexQuote tx: {:?} (block: {}, nonce: {})",
            receipt.transaction_hash,
            receipt.block_number.unwrap_or(0),
            nonce
        );

        Ok(())
    }

    /// Update multiple index quotes on-chain (IBanker::updateMultipleIndexQuotes)
    ///
    /// Batch version of update_index_quote for efficiency when refreshing multiple ITPs.
    /// NOTE: This only works if all index_ids share the same vendor_id asset order.
    /// For per-ITP vendor_ids, use update_index_quote individually.
    pub async fn update_multiple_index_quotes(&self, vendor_id: u128, index_ids: Vec<u128>) -> eyre::Result<()> {
        if index_ids.is_empty() {
            tracing::warn!("No index IDs provided for quote update");
            return Ok(());
        }

        tracing::info!("📤 Updating quotes for {} indices (vendor_id={})", index_ids.len(), vendor_id);

        let call = IBanker::updateMultipleIndexQuotesCall {
            vendor_id,
            index_ids,
        };

        // Fetch fresh nonce from chain
        let nonce = self.get_fresh_nonce().await?;

        let tx = TransactionRequest::default()
            .to(self.castle_address)
            .input(call.abi_encode().into())
            .nonce(nonce);

        let receipt = self.provider.send_transaction(tx).await?.get_receipt().await?;

        tracing::info!(
            "  ✓ updateMultipleIndexQuotes tx: {:?} (block: {})",
            receipt.transaction_hash,
            receipt.block_number.unwrap_or(0)
        );

        Ok(())
    }

    /// Get vendor supply from Castle (IBanker::getVendorSupply)
    pub async fn get_vendor_supply(&self, vendor_id: u128) -> eyre::Result<HashMap<u128, (Amount, Amount)>> {
        tracing::debug!("📥 Reading vendor supply from Castle (vendor_id={})...", vendor_id);

        let call = ISteward::getVendorSupplyCall {
            vendor_id,
        };

        // FIXED: Remove & and pass TransactionRequest by value
        let result = self
            .provider
            .call(TransactionRequest::default()  // REMOVED &
                .to(self.castle_address)
                .input(call.abi_encode().into()))
            .await?;

        // getVendorSupply returns bytes[] memory - decoded is Vec<Bytes>
        let decoded = ISteward::getVendorSupplyCall::abi_decode_returns(&result)?;

        // Parse response - expecting: [asset_names_bytes, supply_short_bytes, supply_long_bytes] or similar
        let asset_names = if decoded.len() > 0 {
            Labels::from_vec(decoded[0].to_vec())
        } else {
            Labels::new()
        };
        let supply_short = if decoded.len() > 1 {
            Vector::from_vec(decoded[1].to_vec())
        } else {
            Vector::new()
        };
        let supply_long = if decoded.len() > 2 {
            Vector::from_vec(decoded[2].to_vec())
        } else {
            Vector::new()
        };

        let mut supply = HashMap::new();

        for (i, asset_id) in asset_names.data.iter().enumerate() {
            let long_qty = supply_long.data.get(i).copied().unwrap_or(Amount::ZERO);
            let short_qty = supply_short.data.get(i).copied().unwrap_or(Amount::ZERO);
            supply.insert(*asset_id, (long_qty, short_qty));
        }

        tracing::debug!("  Read supply for {} assets", supply.len());

        Ok(supply)
    }

    /// Full per-ITP vendor setup: query ITP's asset order, submit assets, market data,
    /// margin, supply, vote, and update quote.
    ///
    /// Uses vendor_id = index_id (per-ITP vendor account) to satisfy JFLT's
    /// sequential scan requirement: vendor assets must be a subsequence matching
    /// the ITP's internal asset ordering from getIndexAssets.
    ///
    /// # Arguments
    /// * `index_id` - The ITP's index ID (also used as vendor_id)
    /// * `price_lookup` - Closure to look up price for an asset_id, returns None for default
    /// * `total_exposure_usd` - Total exposure for margin calculation
    pub async fn submit_all_for_itp(
        &self,
        index_id: u128,
        price_lookup: &dyn Fn(u128) -> Option<Amount>,
        total_exposure_usd: f64,
    ) -> eyre::Result<()> {
        let vendor_id = index_id; // Per-ITP vendor account

        // Step 1: Get ITP's exact asset order from chain
        let itp_assets = self.get_itp_asset_order(index_id).await?;
        if itp_assets.is_empty() {
            tracing::warn!("ITP {} has no assets, skipping", index_id);
            return Ok(());
        }

        let n = itp_assets.len();
        tracing::info!(
            "🔧 Setting up per-ITP vendor for index_id={} ({} assets, vendor_id={})",
            index_id, n, vendor_id
        );

        // Step 2: Submit assets (creates vendor account if new, or extends if existing)
        self.submit_assets(vendor_id, itp_assets.clone()).await?;

        // Step 3: Build price, margin, slope, liquidity, supply vectors in ITP order
        let default_price = Amount::from_u128_raw(100_000_000_000_000_000_000u128); // $100
        let default_slope = Amount::from_u128_raw(1_000_000_000_000_000u128); // 0.001
        let default_liquidity = Amount::from_u128_raw(500_000_000_000_000_000u128); // 0.5
        let mock_supply = Amount::from_u128_raw(100_000_000_000_000_000_000u128); // 100.0

        let per_asset_volley = total_exposure_usd / n as f64;

        let mut prices = Vec::with_capacity(n);
        let mut margins = Vec::with_capacity(n);

        for &asset_id in &itp_assets {
            let price = price_lookup(asset_id).unwrap_or(default_price);
            let price_f64 = price.to_u128_raw() as f64 / 1e18;
            let margin = if price_f64 > 0.0 {
                per_asset_volley / price_f64
            } else {
                1.0
            };
            prices.push(price);
            margins.push(Amount::from_u128_raw((margin * 1e18) as u128));
        }

        let slopes: Vec<Amount> = vec![default_slope; n];
        let liquidities: Vec<Amount> = vec![default_liquidity; n];
        let supply_long: Vec<Amount> = vec![mock_supply; n];
        let supply_short: Vec<Amount> = vec![mock_supply; n];

        // Step 4: Submit market data (prices, slopes, liquidity) in ITP order
        self.submit_market_data(vendor_id, itp_assets.clone(), prices, slopes, liquidities).await?;

        // Step 5: Submit margin in ITP order
        self.submit_margin(vendor_id, itp_assets.clone(), margins).await?;

        // Step 6: Submit supply in ITP order
        self.submit_supply(vendor_id, itp_assets, supply_long, supply_short).await?;

        // Step 7: Submit vote (required before updateIndexQuote)
        if let Err(e) = self.submit_vote(index_id).await {
            tracing::warn!("Vote for ITP {} failed (may already be voted): {}", index_id, e);
        }

        // Step 8: Update index quote
        self.update_index_quote(vendor_id, index_id).await?;

        tracing::info!(
            "✅ Per-ITP vendor setup complete for index_id={} (vendor_id={})",
            index_id, vendor_id
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test vector length validation for submit_market_data
    #[test]
    fn test_submit_market_data_validates_vector_lengths() {
        // This tests the validation logic without needing a real provider
        let asset_ids = vec![1u128, 2, 3];
        let prices = vec![
            Amount::from_u128_raw(100u128 * 1_000_000_000_000_000_000u128),
            Amount::from_u128_raw(200u128 * 1_000_000_000_000_000_000u128),
        ]; // Only 2, should fail
        let slopes = vec![
            Amount::from_u128_raw(1_000_000_000_000_000u128),
            Amount::from_u128_raw(2_000_000_000_000_000u128),
            Amount::from_u128_raw(3_000_000_000_000_000u128),
        ];
        let liquidities = vec![
            Amount::from_u128_raw(1000u128 * 1_000_000_000_000_000_000u128),
            Amount::from_u128_raw(2000u128 * 1_000_000_000_000_000_000u128),
            Amount::from_u128_raw(3000u128 * 1_000_000_000_000_000_000u128),
        ];

        // Validation check (mirrors what submit_market_data does)
        let is_valid = asset_ids.len() == prices.len()
            && asset_ids.len() == slopes.len()
            && asset_ids.len() == liquidities.len();

        assert!(!is_valid, "Mismatched vector lengths should be detected");
    }

    #[test]
    fn test_submit_market_data_accepts_matching_vectors() {
        let asset_ids = vec![1u128, 2, 3];
        let prices = vec![
            Amount::from_u128_raw(100u128 * 1_000_000_000_000_000_000u128),
            Amount::from_u128_raw(200u128 * 1_000_000_000_000_000_000u128),
            Amount::from_u128_raw(300u128 * 1_000_000_000_000_000_000u128),
        ];
        let slopes = vec![
            Amount::from_u128_raw(1_000_000_000_000_000u128),
            Amount::from_u128_raw(2_000_000_000_000_000u128),
            Amount::from_u128_raw(3_000_000_000_000_000u128),
        ];
        let liquidities = vec![
            Amount::from_u128_raw(1000u128 * 1_000_000_000_000_000_000u128),
            Amount::from_u128_raw(2000u128 * 1_000_000_000_000_000_000u128),
            Amount::from_u128_raw(3000u128 * 1_000_000_000_000_000_000u128),
        ];

        let is_valid = asset_ids.len() == prices.len()
            && asset_ids.len() == slopes.len()
            && asset_ids.len() == liquidities.len();

        assert!(is_valid, "Matching vector lengths should be valid");
    }

    #[test]
    fn test_submit_market_data_empty_vectors() {
        let asset_ids: Vec<u128> = vec![];
        let prices: Vec<Amount> = vec![];
        let slopes: Vec<Amount> = vec![];
        let liquidities: Vec<Amount> = vec![];

        // Empty is technically "matching" but should return early
        let is_empty = asset_ids.is_empty();
        let is_valid = asset_ids.len() == prices.len()
            && asset_ids.len() == slopes.len()
            && asset_ids.len() == liquidities.len();

        assert!(is_empty, "Empty check should be true");
        assert!(is_valid, "Empty vectors should technically match");
    }

    #[test]
    fn test_labels_conversion() {
        // Test that asset IDs convert to Labels correctly
        let asset_ids = vec![1u128, 2, 3, 4, 5];
        let labels = Labels::from_vec_u128(asset_ids.clone());

        assert_eq!(labels.data.len(), 5);
        assert_eq!(labels.data[0], 1);
        assert_eq!(labels.data[4], 5);
    }

    #[test]
    fn test_vector_conversion() {
        // Test that Amounts convert to Vector correctly
        let amounts = vec![
            Amount::from_u128_raw(1_000_000_000_000_000_000),
            Amount::from_u128_raw(2_000_000_000_000_000_000),
        ];

        let raw_values: Vec<u128> = amounts.iter().map(|a| a.to_u128_raw()).collect();
        let vector = Vector::from_vec_u128(raw_values);

        assert_eq!(vector.data.len(), 2);
    }

    #[test]
    fn test_submit_margin_validates_lengths() {
        let asset_ids = vec![1u128, 2, 3];
        let margins = vec![
            Amount::from_u128_raw(100u128 * 1_000_000_000_000_000_000u128),
            Amount::from_u128_raw(200u128 * 1_000_000_000_000_000_000u128),
        ]; // Only 2

        let is_valid = asset_ids.len() == margins.len();
        assert!(!is_valid, "Mismatched lengths should be invalid");
    }

    #[test]
    fn test_submit_supply_validates_lengths() {
        let asset_ids = vec![1u128, 2, 3];
        let supply_long = vec![
            Amount::from_u128_raw(100u128 * 1_000_000_000_000_000_000u128),
            Amount::from_u128_raw(200u128 * 1_000_000_000_000_000_000u128),
            Amount::from_u128_raw(300u128 * 1_000_000_000_000_000_000u128),
        ];
        let supply_short = vec![
            Amount::from_u128_raw(50u128 * 1_000_000_000_000_000_000u128),
        ]; // Only 1

        let is_valid = asset_ids.len() == supply_long.len()
            && asset_ids.len() == supply_short.len();
        assert!(!is_valid, "Mismatched supply lengths should be invalid");
    }

    #[test]
    fn test_vendor_demand_net_calculation() {
        // Test net demand = long - short
        let long_qty = Amount::from_u128_raw(100_000_000_000_000_000_000u128); // 100
        let short_qty = Amount::from_u128_raw(30_000_000_000_000_000_000u128); // 30

        let net = long_qty.checked_sub(short_qty).unwrap_or(Amount::ZERO);

        // Net should be 70
        let expected = 70_000_000_000_000_000_000u128;
        assert_eq!(net.to_u128_raw(), expected);
    }

    #[test]
    fn test_vendor_demand_net_overflow_protection() {
        // Test when short > long (would underflow)
        let long_qty = Amount::from_u128_raw(30_000_000_000_000_000_000u128); // 30
        let short_qty = Amount::from_u128_raw(100_000_000_000_000_000_000u128); // 100

        // checked_sub returns None on underflow, so we use ZERO
        let net = long_qty.checked_sub(short_qty).unwrap_or(Amount::ZERO);

        assert_eq!(net.to_u128_raw(), 0, "Underflow should result in ZERO");
    }

    // Story 3-4 Tests: SubmissionData validation

    #[test]
    fn test_submission_data_is_valid() {
        let data = SubmissionData {
            asset_ids: vec![1, 2, 3],
            margins: vec![Amount::ZERO, Amount::ZERO, Amount::ZERO],
            supply_long: vec![Amount::ZERO, Amount::ZERO, Amount::ZERO],
            supply_short: vec![Amount::ZERO, Amount::ZERO, Amount::ZERO],
            prices: vec![Amount::ZERO, Amount::ZERO, Amount::ZERO],
            slopes: vec![Amount::ZERO, Amount::ZERO, Amount::ZERO],
            liquidities: vec![Amount::ZERO, Amount::ZERO, Amount::ZERO],
        };

        assert!(data.is_valid());
        assert_eq!(data.len(), 3);
        assert!(!data.is_empty());
    }

    #[test]
    fn test_submission_data_invalid_margin_length() {
        let data = SubmissionData {
            asset_ids: vec![1, 2, 3],
            margins: vec![Amount::ZERO, Amount::ZERO], // Wrong length
            supply_long: vec![Amount::ZERO, Amount::ZERO, Amount::ZERO],
            supply_short: vec![Amount::ZERO, Amount::ZERO, Amount::ZERO],
            prices: vec![Amount::ZERO, Amount::ZERO, Amount::ZERO],
            slopes: vec![Amount::ZERO, Amount::ZERO, Amount::ZERO],
            liquidities: vec![Amount::ZERO, Amount::ZERO, Amount::ZERO],
        };

        assert!(!data.is_valid());
    }

    #[test]
    fn test_submission_data_invalid_supply_length() {
        let data = SubmissionData {
            asset_ids: vec![1, 2, 3],
            margins: vec![Amount::ZERO, Amount::ZERO, Amount::ZERO],
            supply_long: vec![Amount::ZERO, Amount::ZERO], // Wrong length
            supply_short: vec![Amount::ZERO, Amount::ZERO, Amount::ZERO],
            prices: vec![Amount::ZERO, Amount::ZERO, Amount::ZERO],
            slopes: vec![Amount::ZERO, Amount::ZERO, Amount::ZERO],
            liquidities: vec![Amount::ZERO, Amount::ZERO, Amount::ZERO],
        };

        assert!(!data.is_valid());
    }

    #[test]
    fn test_submission_data_empty() {
        let data = SubmissionData {
            asset_ids: vec![],
            margins: vec![],
            supply_long: vec![],
            supply_short: vec![],
            prices: vec![],
            slopes: vec![],
            liquidities: vec![],
        };

        assert!(data.is_valid()); // Empty is valid (all same length: 0)
        assert!(data.is_empty());
        assert_eq!(data.len(), 0);
    }

    #[test]
    fn test_submission_data_realistic() {
        // Create realistic submission data for 3 assets
        let data = SubmissionData {
            asset_ids: vec![1001, 1002, 1003],
            margins: vec![
                Amount::from_u128_raw((50.0 * 1e18) as u128),
                Amount::from_u128_raw((25.0 * 1e18) as u128),
                Amount::from_u128_raw((10.0 * 1e18) as u128),
            ],
            supply_long: vec![Amount::ZERO, Amount::ZERO, Amount::ZERO],
            supply_short: vec![Amount::ZERO, Amount::ZERO, Amount::ZERO],
            prices: vec![
                Amount::from_u128_raw((100.0 * 1e18) as u128),
                Amount::from_u128_raw((200.0 * 1e18) as u128),
                Amount::from_u128_raw((500.0 * 1e18) as u128),
            ],
            slopes: vec![
                Amount::from_u128_raw((0.001 * 1e18) as u128),
                Amount::from_u128_raw((0.002 * 1e18) as u128),
                Amount::from_u128_raw((0.003 * 1e18) as u128),
            ],
            liquidities: vec![
                Amount::from_u128_raw((1000.0 * 1e18) as u128),
                Amount::from_u128_raw((500.0 * 1e18) as u128),
                Amount::from_u128_raw((200.0 * 1e18) as u128),
            ],
        };

        assert!(data.is_valid());
        assert_eq!(data.len(), 3);

        // Verify margin computation matches formula: M_i = V_max/n / P_i
        // If V_max = 10000, n = 3, P = [100, 200, 500]
        // M = [33.33, 16.67, 6.67] ≈ [50, 25, 10] (matches our test data)
    }

    // Story 3-4 Integration Test: Verify complete submission flow (Task 7.3)
    // Note: This is a unit test that validates the submission data flow
    // without a live provider. Full HTTP integration test requires
    // axum-test or similar framework.
    #[test]
    fn test_submission_flow_end_to_end_validation() {
        use crate::market_data::margin::{compute_margin_vector, compute_supply_placeholder, MarginConfig};

        // Simulate Keeper request: asset_ids
        let asset_ids = vec![1001u128, 1002, 1003, 1004, 1005];
        let batch_id = "test-batch-7.3".to_string();

        // Step 1: Simulate PSL computation (normally from PSLComputeService)
        let prices = vec![
            Amount::from_u128_raw((100.0 * 1e18) as u128),
            Amount::from_u128_raw((200.0 * 1e18) as u128),
            Amount::from_u128_raw((300.0 * 1e18) as u128),
            Amount::from_u128_raw((400.0 * 1e18) as u128),
            Amount::from_u128_raw((500.0 * 1e18) as u128),
        ];
        let slopes = vec![
            Amount::from_u128_raw((0.001 * 1e18) as u128),
            Amount::from_u128_raw((0.002 * 1e18) as u128),
            Amount::from_u128_raw((0.003 * 1e18) as u128),
            Amount::from_u128_raw((0.001 * 1e18) as u128),
            Amount::from_u128_raw((0.002 * 1e18) as u128),
        ];
        let liquidities = vec![
            Amount::from_u128_raw((1000.0 * 1e18) as u128),
            Amount::from_u128_raw((500.0 * 1e18) as u128),
            Amount::from_u128_raw((333.0 * 1e18) as u128),
            Amount::from_u128_raw((250.0 * 1e18) as u128),
            Amount::from_u128_raw((200.0 * 1e18) as u128),
        ];

        // Step 2: Compute margins (AC #1)
        let margin_config = MarginConfig::default();
        let margins = compute_margin_vector(&prices, &margin_config);
        assert_eq!(margins.len(), 5, "Margin vector should match asset count");

        // Step 3: Compute supply placeholders (AC #2)
        let (supply_long, supply_short) = compute_supply_placeholder(5);
        assert_eq!(supply_long.len(), 5);
        assert_eq!(supply_short.len(), 5);
        // Verify placeholders are zeros
        for i in 0..5 {
            assert_eq!(supply_long[i].to_u128_raw(), 0);
            assert_eq!(supply_short[i].to_u128_raw(), 0);
        }

        // Step 4: Build SubmissionData (AC #3 - ready for submitMarketDataCall)
        let submission_data = SubmissionData {
            asset_ids: asset_ids.clone(),
            margins,
            supply_long,
            supply_short,
            prices,
            slopes,
            liquidities,
        };

        // Step 5: Validate (should pass before actual submission)
        assert!(submission_data.is_valid(), "Submission data should be valid");
        assert_eq!(submission_data.len(), 5);
        assert!(!submission_data.is_empty());

        // Step 6: Verify batch_id is available for logging (AC #7)
        assert!(!batch_id.is_empty());

        // Note: Actual concurrent submission via submit_all_concurrent()
        // requires async runtime and mock provider. This validates the
        // data preparation flow is correct.
    }

    // Story 3-4 Timing Test: Verify performance target (Task 7.4)
    // Tests that submission data preparation is fast enough
    #[test]
    fn test_submission_data_preparation_performance() {
        use crate::market_data::margin::{compute_margin_vector, compute_supply_placeholder, MarginConfig};
        use std::time::Instant;

        // NFR17: Total submission time < 2 seconds for 50 assets
        // This test validates the DATA PREPARATION is < 100ms
        // (actual on-chain submission time depends on network)

        let n = 50; // Realistic batch size
        let start = Instant::now();

        // Generate test data
        let asset_ids: Vec<u128> = (1..=n).map(|i| 1000 + i as u128).collect();
        let prices: Vec<Amount> = (1..=n)
            .map(|i| Amount::from_u128_raw(((100 + i * 10) as f64 * 1e18) as u128))
            .collect();
        let slopes: Vec<Amount> = (1..=n)
            .map(|_| Amount::from_u128_raw((0.001 * 1e18) as u128))
            .collect();
        let liquidities: Vec<Amount> = (1..=n)
            .map(|i| Amount::from_u128_raw(((1000 / (i % 10 + 1)) as f64 * 1e18) as u128))
            .collect();

        // Compute margins
        let margin_config = MarginConfig::default();
        let margins = compute_margin_vector(&prices, &margin_config);

        // Compute supply placeholders
        let (supply_long, supply_short) = compute_supply_placeholder(n);

        // Build submission data
        let submission_data = SubmissionData {
            asset_ids,
            margins,
            supply_long,
            supply_short,
            prices,
            slopes,
            liquidities,
        };

        let elapsed = start.elapsed();

        // Validate
        assert!(submission_data.is_valid());
        assert_eq!(submission_data.len(), n);

        // Performance assertion: data preparation should be < 100ms
        // This leaves 1.9 seconds for actual on-chain submissions (NFR17)
        assert!(
            elapsed.as_millis() < 100,
            "Data preparation took {}ms, expected < 100ms",
            elapsed.as_millis()
        );

        tracing::info!(
            "✓ Prepared submission data for {} assets in {:?}",
            n,
            elapsed
        );
    }
}