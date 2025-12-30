use super::asset_mapper::AssetMapper;
use super::index_mapper::IndexMapper;
use super::price_tracker::PriceTracker;
use crate::basket::{BasketManager, Index};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy_primitives::Address;
use alloy_sol_types::SolCall;
use common::amount::Amount;
use common::interfaces::banker::IBanker;
use common::interfaces::factor::IFactor;
use common::interfaces::guildmaster::IGuildmaster;
use eyre::{eyre, Result};
use parking_lot::RwLock;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;

pub struct OnchainSubmitterConfig {
    pub vendor_id: u128,
    pub castle_address: Address,
    pub submission_interval: Duration,
    pub sync_check_interval: Duration,
    pub default_liquidity: f64,
    pub default_slope: f64,
}

pub struct OnchainSubmitter<P> {
    config: OnchainSubmitterConfig,
    provider: P,
    asset_mapper: Arc<AssetMapper>,
    index_mapper: Arc<IndexMapper>,
    basket_manager: Arc<RwLock<BasketManager>>,
    price_tracker: Arc<PriceTracker>,
    
    // Track what's been initialized on-chain
    initialized_assets: Arc<RwLock<HashSet<u128>>>,
    initialized_indices: Arc<RwLock<HashSet<String>>>,
    
    cancel_token: CancellationToken,
}

impl<P> OnchainSubmitter<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    pub fn new(
        config: OnchainSubmitterConfig,
        provider: P,
        asset_mapper: Arc<AssetMapper>,
        index_mapper: Arc<IndexMapper>,
        basket_manager: Arc<RwLock<BasketManager>>,
        price_tracker: Arc<PriceTracker>,
    ) -> Self {
        Self {
            config,
            provider,
            asset_mapper,
            index_mapper,
            basket_manager,
            price_tracker,
            initialized_assets: Arc::new(RwLock::new(HashSet::new())),
            initialized_indices: Arc::new(RwLock::new(HashSet::new())),
            cancel_token: CancellationToken::new(),
        }
    }

    /// Initialize: Submit all current assets and indices (called once at startup)
    pub async fn initialize(&self) -> Result<()> {
        tracing::info!("=== Starting On-chain Initialization ===");

        // Get all current assets and indices
        let basket_manager = self.basket_manager.read();
        let asset_symbols = basket_manager.get_all_unique_symbols();
        let indices = basket_manager.get_all_indices();

        // Submit all assets
        let asset_ids = self.submit_assets_internal(&asset_symbols).await?;
        
        // Mark assets as initialized
        {
            let mut initialized = self.initialized_assets.write();
            for id in asset_ids {
                initialized.insert(id);
            }
        }
        
        tracing::info!("✓ Assets initialized: {} assets", asset_symbols.len());

        // Submit all indices
        for index in &indices {
            self.submit_index_internal(index).await?;
            
            // Mark index as initialized
            self.initialized_indices.write().insert(index.symbol.clone());
        }
        
        tracing::info!("✓ Indices initialized: {} indices", indices.len());
        
        tracing::info!("=== On-chain Initialization Complete ===");

        Ok(())
    }

    /// Sync new additions: Check for and submit only NEW assets/indices
    pub async fn sync_new_additions(&self) -> Result<()> {
        tracing::debug!("Checking for new assets/indices...");

        // Get current state and release lock immediately
        let (current_asset_symbols, current_asset_ids, current_indices) = {
            let basket_manager = self.basket_manager.read();
            let current_asset_symbols = basket_manager.get_all_unique_symbols();
            let current_asset_ids = self.asset_mapper.get_sorted_ids(&current_asset_symbols)?;
            let current_indices = basket_manager.get_all_indices()
                .into_iter()
                .map(|idx| idx.clone())
                .collect::<Vec<_>>();

            (current_asset_symbols, current_asset_ids, current_indices)
        }; // Lock released here

        // Check for new assets
        let new_asset_ids: Vec<u128> = {
            let initialized = self.initialized_assets.read();
            current_asset_ids
                .into_iter()
                .filter(|id| !initialized.contains(id))
                .collect()
        }; // Lock released here

        if !new_asset_ids.is_empty() {
            tracing::info!("Found {} new asset(s) to submit: {:?}", new_asset_ids.len(), new_asset_ids);

            // Get symbols for new IDs
            let new_symbols: Vec<String> = new_asset_ids
                .iter()
                .filter_map(|id| self.asset_mapper.get_symbol(*id).cloned())
                .collect();

            // Submit new assets (can await now)
            let submitted_ids = self.submit_assets_internal(&new_symbols).await?;

            // Mark as initialized
            let mut initialized = self.initialized_assets.write();
            for id in submitted_ids {
                initialized.insert(id);
            }

            tracing::info!("✓ New assets submitted and initialized");
        }

        // Check for new indices
        let new_indices: Vec<Index> = {
            let initialized = self.initialized_indices.read();
            current_indices
                .into_iter()
                .filter(|index| !initialized.contains(&index.symbol))
                .collect()
        }; // Lock released here

        if !new_indices.is_empty() {
            tracing::info!("Found {} new index/indices to submit", new_indices.len());

            for index in &new_indices {
                self.submit_index_internal(index).await?;

                // Mark as initialized
                self.initialized_indices.write().insert(index.symbol.clone());
            }

            tracing::info!("✓ New indices submitted and initialized");
        }

        if new_asset_ids.is_empty() && new_indices.is_empty() {
            tracing::debug!("No new assets or indices found");
        }

        Ok(())
    }

    /// Start: Run periodic market data submissions (every 20s) with background sync checks
    pub async fn start(& self) -> Result<()> {
        let config = self.config.clone();
        let provider = self.provider.clone();
        let asset_mapper = self.asset_mapper.clone();
        let basket_manager = self.basket_manager.clone();
        let price_tracker = self.price_tracker.clone();
        let initialized_assets = self.initialized_assets.clone();
        let cancel_token = self.cancel_token.clone();

        tokio::spawn(async move {
            if let Err(e) = Self::run_submission_loop(
                config,
                provider,
                asset_mapper,
                basket_manager,
                price_tracker,
                initialized_assets,
                cancel_token,
            )
            .await
            {
                tracing::error!("Submission loop failed: {:?}", e);
            }
        });

        Ok(())
    }

    pub async fn stop(&self) {
        tracing::info!("Stopping onchain submitter");
        self.cancel_token.cancel();
    }

    async fn run_submission_loop(
        config: OnchainSubmitterConfig,
        provider: P,
        asset_mapper: Arc<AssetMapper>,
        basket_manager: Arc<RwLock<BasketManager>>,
        price_tracker: Arc<PriceTracker>,
        initialized_assets: Arc<RwLock<HashSet<u128>>>,
        cancel_token: CancellationToken,
    ) -> Result<()> {
        let mut submission_timer = interval(config.submission_interval);
        let mut sync_timer = interval(config.sync_check_interval);

        tracing::info!(
            "Market data submission loop started (interval: {:?}, sync check: {:?})",
            config.submission_interval,
            config.sync_check_interval
        );

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    tracing::info!("Submission loop cancelled");
                    break;
                }
                
                _ = submission_timer.tick() => {
                    // Submit market data only
                    if let Err(e) = Self::submit_market_data_only(
                        &config,
                        &provider,
                        &asset_mapper,
                        &basket_manager,
                        &price_tracker,
                        &initialized_assets,
                    ).await {
                        tracing::error!("Market data submission failed: {:?}", e);
                    }
                }
                
                _ = sync_timer.tick() => {
                    tracing::debug!("Sync check interval reached (checking for new assets/indices)");
                    // Note: sync_new_additions would need to be called from main
                    // or we'd need to restructure to have access to the submitter instance
                    // For now, we'll log this as a placeholder
                }
            }
        }

        Ok(())
    }

    async fn submit_market_data_only(
        config: &OnchainSubmitterConfig,
        provider: &P,
        asset_mapper: &AssetMapper,
        basket_manager: &Arc<RwLock<BasketManager>>,
        price_tracker: &Arc<PriceTracker>,
        initialized_assets: &Arc<RwLock<HashSet<u128>>>,
    ) -> Result<()> {
        // Get data from locked sections first, then release locks
        let (asset_symbols, asset_ids) = {
            let basket_manager = basket_manager.read();
            let asset_symbols = basket_manager.get_all_unique_symbols();

            // Only submit for initialized assets
            let initialized = initialized_assets.read();
            let asset_ids: Vec<u128> = asset_mapper
                .get_sorted_ids(&asset_symbols)?
                .into_iter()
                .filter(|id| initialized.contains(id))
                .collect();

            // Locks are dropped here when the scope ends
            (asset_symbols, asset_ids)
        };

        if asset_ids.is_empty() {
            tracing::warn!("No initialized assets to submit market data for");
            return Ok(());
        }

        // Check if all prices are available
        if !price_tracker.all_prices_available(&asset_symbols) {
            tracing::warn!("Not all prices available yet, skipping market data submission");
            return Ok(());
        }

        // Get prices in the same order as asset_symbols
        let mut prices = Vec::new();
        let mut liquidity = Vec::new();
        let mut slopes = Vec::new();

        for symbol in &asset_symbols {
            let price = price_tracker
                .get_price(symbol)
                .ok_or_else(|| eyre!("Price not available for {}", symbol))?;

            prices.push(price);

            // Use constant values for now
            let liq = Amount::from_u128_raw((config.default_liquidity * 1e18) as u128);
            let slope = Amount::from_u128_raw((config.default_slope * 1e18) as u128);

            liquidity.push(liq);
            slopes.push(slope);
        }

        tracing::info!("📊 Submitting market data for {} assets", asset_ids.len());

        let asset_names = common::labels::Labels::from_vec_u128(asset_ids.clone());
        let asset_liquidity = common::vector::Vector::from_vec_u128(
            liquidity.iter().map(|l| l.to_u128_raw()).collect(),
        );
        let asset_prices = common::vector::Vector::from_vec_u128(
            prices.iter().map(|p| p.to_u128_raw()).collect(),
        );
        let asset_slopes = common::vector::Vector::from_vec_u128(
            slopes.iter().map(|s| s.to_u128_raw()).collect(),
        );

        let call = IFactor::submitMarketDataCall {
            vendor_id: config.vendor_id,
            asset_names: asset_names.to_vec(),
            asset_liquidity: asset_liquidity.to_vec(),
            asset_prices: asset_prices.to_vec(),
            asset_slopes: asset_slopes.to_vec(),
        };

        let tx = TransactionRequest::default()
            .to(config.castle_address)
            .input(call.abi_encode().into());

        // Now we can await without holding any locks
        let receipt = provider.send_transaction(tx).await?.get_receipt().await?;

        tracing::info!("✓ Market data submitted: {:?}", receipt.transaction_hash);

        Ok(())
    }

    // Internal method to submit assets
    async fn submit_assets_internal(&self, asset_symbols: &[String]) -> Result<Vec<u128>> {
        let asset_ids = self.asset_mapper.get_sorted_ids(asset_symbols)?;

        tracing::info!("📦 Submitting {} assets: {:?}", asset_ids.len(), asset_ids);

        let asset_names = common::labels::Labels::from_vec_u128(asset_ids.clone());

        let call = IBanker::submitAssetsCall {
            vendor_id: self.config.vendor_id,
            market_asset_names: asset_names.to_vec(),
        };

        let tx = TransactionRequest::default()
            .to(self.config.castle_address)
            .input(call.abi_encode().into());

        let receipt = self.provider.send_transaction(tx).await?.get_receipt().await?;

        tracing::info!("✓ Assets submitted: {:?}", receipt.transaction_hash);

        Ok(asset_ids)
    }

    // Internal method to submit a single index
    async fn submit_index_internal(&self, index: &Index) -> Result<()> {
        let index_id = self.index_mapper.validate_index_exists(&index.symbol)?;

        let asset_symbols: Vec<String> = index.get_asset_symbols();
        let mut asset_ids = Vec::new();
        let mut weights = Vec::new();

        for asset in &index.assets {
            let id = self
                .asset_mapper
                .get_id(&asset.pair)
                .ok_or_else(|| eyre!("Asset '{}' not found in mapping", asset.pair))?;
            asset_ids.push(id);

            let weight_f64: f64 = asset.weights.parse()?;
            let weight = Amount::from_u128_raw((weight_f64 * 1e18) as u128);
            weights.push(weight);
        }

        tracing::info!(
            "📋 Submitting index '{}' (ID: {}) with {} assets",
            index.symbol,
            index_id,
            asset_ids.len()
        );

        let asset_names = common::labels::Labels::from_vec_u128(asset_ids);
        let asset_weights = common::vector::Vector::from_vec_u128(
            weights.iter().map(|w| w.to_u128_raw()).collect(),
        );
        let info = format!("Index {}", index.symbol).into_bytes();

        let call = IGuildmaster::submitIndexCall {
            index: index_id,
            asset_names: asset_names.to_vec(),
            asset_weights: asset_weights.to_vec(),
            info,
        };

        let tx = TransactionRequest::default()
            .to(self.config.castle_address)
            .input(call.abi_encode().into());

        // Handle potential revert (index already exists)
        match self.provider.send_transaction(tx).await {
            Ok(pending_tx) => {
                match pending_tx.get_receipt().await {
                    Ok(receipt) => {
                        tracing::info!("✓ Index '{}' submitted: {:?}", index.symbol, receipt.transaction_hash);
                    }
                    Err(e) => {
                        // Check if it's a revert error (index already exists)
                        let error_msg = format!("{:?}", e);
                        if error_msg.contains("revert") || error_msg.contains("already") {
                            tracing::info!("ℹ️  Index '{}' already exists on-chain, skipping", index.symbol);
                            // Continue without error - this is expected behavior
                        } else {
                            tracing::error!("Failed to get receipt for index '{}': {:?}", index.symbol, e);
                            return Err(e.into());
                        }
                    }
                }
            }
            Err(e) => {
                // Check if it's a revert error
                let error_msg = format!("{:?}", e);
                if error_msg.contains("revert") || error_msg.contains("already") {
                    tracing::info!("ℹ️  Index '{}' already exists on-chain, skipping", index.symbol);
                    // Continue without error
                } else {
                    tracing::error!("Failed to send transaction for index '{}': {:?}", index.symbol, e);
                    return Err(e.into());
                }
            }
        }

        // Submit vote
        let vote_call = IGuildmaster::submitVoteCall {
            index: index_id,
            vote: vec![],
        };

        let vote_tx = TransactionRequest::default()
            .to(self.config.castle_address)
            .input(vote_call.abi_encode().into());

        // Handle vote submission errors similarly
        match self.provider.send_transaction(vote_tx).await {
            Ok(pending_tx) => {
                match pending_tx.get_receipt().await {
                    Ok(receipt) => {
                        tracing::info!("✓ Vote for '{}' submitted: {:?}", index.symbol, receipt.transaction_hash);
                    }
                    Err(e) => {
                        let error_msg = format!("{:?}", e);
                        if error_msg.contains("revert") || error_msg.contains("already") {
                            tracing::debug!("Vote for '{}' may have already been submitted", index.symbol);
                        } else {
                            tracing::warn!("Failed to get receipt for vote on '{}': {:?}", index.symbol, e);
                        }
                    }
                }
            }
            Err(e) => {
                let error_msg = format!("{:?}", e);
                if error_msg.contains("revert") || error_msg.contains("already") {
                    tracing::debug!("Vote for '{}' may have already been submitted", index.symbol);
                } else {
                    tracing::warn!("Failed to send vote transaction for '{}': {:?}", index.symbol, e);
                }
            }
        }

        Ok(())
    }
}

// Make config cloneable
impl Clone for OnchainSubmitterConfig {
    fn clone(&self) -> Self {
        Self {
            vendor_id: self.vendor_id,
            castle_address: self.castle_address,
            submission_interval: self.submission_interval,
            sync_check_interval: self.sync_check_interval,
            default_liquidity: self.default_liquidity,
            default_slope: self.default_slope,
        }
    }
}