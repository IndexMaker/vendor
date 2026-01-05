use super::market_data_cache::{MarketDataCache, StalenessConfig};
use super::price_tracker::PriceTracker;
use super::AssetMapper;
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
pub struct StalenessManager {
    cache: MarketDataCache,
    price_tracker: Arc<PriceTracker>,
    asset_mapper: Arc<RwLock<AssetMapper>>,
}

impl StalenessManager {
    pub fn new(
        config: StalenessConfig,
        price_tracker: Arc<PriceTracker>,
        asset_mapper: Arc<RwLock<AssetMapper>>,
    ) -> Self {
        Self {
            cache: MarketDataCache::new(config),
            price_tracker,
            asset_mapper,
        }
    }
    
    /// Update cache with on-chain market data
    /// Called after fetching from IFactor::getMarketData()
    pub fn update_onchain_cache(
        &mut self,
        asset_ids: Vec<u128>,
        liquidity: Vec<Amount>,
        prices: Vec<Amount>,
        slopes: Vec<Amount>,
    ) -> Result<()> {
        if asset_ids.len() != liquidity.len() 
            || asset_ids.len() != prices.len() 
            || asset_ids.len() != slopes.len() 
        {
            return Err(eyre::eyre!("Mismatched vector lengths in market data"));
        }
        
        let mapper = self.asset_mapper.read();
        
        for i in 0..asset_ids.len() {
            let asset_id = asset_ids[i];
            
            // Find symbol for this asset_id
            if let Some(symbol) = mapper.get_symbol(asset_id) {
                self.cache.update_from_onchain(
                    symbol.clone(),
                    asset_id,
                    liquidity[i],
                    prices[i],
                    slopes[i],
                );
                
                tracing::debug!(
                    "Cached on-chain data for {}: L={}, P={}, S={}",
                    symbol,
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
    
    /// Get staleness config
    pub fn config(&self) -> &StalenessConfig {
        &self.cache.config
    }
}