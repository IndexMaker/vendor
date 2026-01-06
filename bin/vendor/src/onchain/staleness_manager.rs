use crate::onchain::OnchainReader;
use crate::onchain::market_data_cache::CachedMarketData;

use super::market_data_cache::{MarketDataCache, StalenessConfig};
use super::price_tracker::PriceTracker;
use super::AssetMapper;
use alloy::providers::Provider;
use common::amount::Amount;
use eyre::Result;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Manages market data staleness detection
/// 
/// Workflow:
/// 1. Cache on-chain market data (from getMarketData() calls)
/// 2. Compare with real-time data from PriceTracker
/// 3. Identify stale assets based on thresholds
/// 4. Prepare subset for Keeper submission
pub struct StalenessManager<P> {
    cache: MarketDataCache,
    price_tracker: Arc<PriceTracker>,
    asset_mapper: Arc<RwLock<AssetMapper>>,
    onchain_reader: OnchainReader<P>,
    
    /// Track which assets we submitted and in what order
    /// This matches the order returned by getMarketData()
    submitted_asset_order: Vec<u128>,  // Asset IDs in submission order
}

impl<P> StalenessManager<P>
where
    P: Provider + Clone,
{
    pub fn new(
        config: StalenessConfig,
        price_tracker: Arc<PriceTracker>,
        asset_mapper: Arc<RwLock<AssetMapper>>,
        onchain_reader: OnchainReader<P>,
    ) -> Self {
        Self {
            cache: MarketDataCache::new(config),
            price_tracker,
            asset_mapper,
            onchain_reader,
            submitted_asset_order: Vec::new(),
        }
    }

    /// Set the asset order (called after submitAssets)
    pub fn set_submitted_asset_order(&mut self, asset_ids: Vec<u128>) {
        self.submitted_asset_order = asset_ids;
        tracing::info!("Set submitted asset order: {:?}", self.submitted_asset_order);
    }
    
    /// Update cache with on-chain market data
    /// Reads from chain and updates cache
    pub async fn update_onchain_cache(&mut self) -> Result<()> {
        // Read from chain
        let (liquidity, prices, slopes) = self.onchain_reader.get_market_data().await?;
        
        // Verify lengths match
        if liquidity.len() != prices.len() || liquidity.len() != slopes.len() {
            return Err(eyre::eyre!("Mismatched vector lengths in market data"));
        }
        
        if liquidity.len() != self.submitted_asset_order.len() {
            tracing::warn!(
                "Market data length ({}) doesn't match submitted assets ({})",
                liquidity.len(),
                self.submitted_asset_order.len()
            );
        }
        
        let mapper = self.asset_mapper.read();
        
        // Update cache using position-based matching
        for i in 0..liquidity.len().min(self.submitted_asset_order.len()) {
            let asset_id = self.submitted_asset_order[i];
            
            if let Some(symbol) = mapper.get_symbol(asset_id) {
                self.cache.update_from_onchain(
                    symbol.clone(),
                    asset_id,
                    liquidity[i],
                    prices[i],
                    slopes[i],  // Now we have slopes!
                );
                
                tracing::debug!(
                    "Cached on-chain data for {} ({}): L={}, P={}, S={}",
                    symbol,
                    asset_id,
                    liquidity[i].to_u128_raw() as f64 / 1e18,
                    prices[i].to_u128_raw() as f64 / 1e18,
                    slopes[i].to_u128_raw() as f64 / 1e18
                );
            }
        }
        
        Ok(())
    }
    
    /// Get current real-time market data from PriceTracker
    fn get_current_data(&self, symbols: &[String]) -> HashMap<String, (Amount, Amount, Amount)> {
        let mut current = HashMap::new();
        
        for symbol in symbols {
            let price = self.price_tracker.get_price(symbol).unwrap_or(Amount::ZERO);
            
            // TODO: Get real liquidity and slope from market data
            // For now, use mock values
            let liquidity = Amount::from_u128_raw(1_000_000 * 1_000_000_000_000_000_000); // Mock: 1M
            let slope = Amount::from_u128_raw(1 * 1_000_000_000_000_000_000); // Mock: 1.0
            
            current.insert(symbol.clone(), (liquidity, price, slope));
        }
        
        current
    }
    
    /// Check which assets need market data update
    /// Returns (stale_symbols, stale_reasons)
    pub fn check_staleness(&self, symbols: &[String]) -> (Vec<String>, Vec<String>) {
        let current_data = self.get_current_data(symbols);
        let stale_assets = self.cache.find_stale_assets(&current_data);
        
        if !stale_assets.is_empty() {
            tracing::info!(
                "Found {} stale asset(s) requiring market data update",
                stale_assets.len()
            );
            
            for (symbol, reason) in &stale_assets {
                tracing::debug!("  {}: {}", symbol, reason);
            }
        }
        
        let (symbols, reasons): (Vec<_>, Vec<_>) = stale_assets.into_iter().unzip();
        (symbols, reasons)
    }
    
    /// Prepare market data for submission (only stale subset)
    /// Returns (asset_ids, liquidity, prices, slopes) for stale assets only
    pub fn prepare_stale_market_data(
        &self,
        stale_symbols: &[String],
    ) -> Result<(Vec<u128>, Vec<Amount>, Vec<Amount>, Vec<Amount>)> {
        let mut asset_ids = Vec::new();
        let mut liquidity = Vec::new();
        let mut prices = Vec::new();
        let mut slopes = Vec::new();
        
        let current_data = self.get_current_data(stale_symbols);
        let mapper = self.asset_mapper.read();
        
        for symbol in stale_symbols {
            if let Some(asset_id) = mapper.get_id(symbol) {
                if let Some((liq, price, slope)) = current_data.get(symbol) {
                    asset_ids.push(asset_id);
                    liquidity.push(*liq);
                    prices.push(*price);
                    slopes.push(*slope);
                }
            }
        }
        
        Ok((asset_ids, liquidity, prices, slopes))
    }
    
    /// Mark assets as submitted
    pub fn mark_submitted(&mut self, symbols: &[String]) {
        self.cache.mark_submitted(symbols);
        tracing::debug!("Marked {} assets as submitted", symbols.len());
    }
    
    /// Get cached data for a symbol
    pub fn get_cached_data(&self, symbol: &str) -> Option<&CachedMarketData> {
        self.cache.get(symbol)
    }

    /// Get staleness config
    pub fn config(&self) -> &StalenessConfig {
        &self.cache.config
    }
}