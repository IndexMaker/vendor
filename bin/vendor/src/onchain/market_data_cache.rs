use chrono::{DateTime, Utc};
use common::amount::Amount;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Cached market data for a single asset
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedMarketData {
    pub symbol: String,
    pub asset_id: u128,
    
    /// Liquidity (L)
    pub liquidity: Amount,
    
    /// Price (P)
    pub price: Amount,
    
    /// Slope (S)
    pub slope: Amount,
    
    /// When this data was fetched from on-chain
    pub fetched_at: DateTime<Utc>,
    
    /// When this data was last submitted to on-chain
    pub last_submitted: Option<DateTime<Utc>>,
}

impl CachedMarketData {
    pub fn new(
        symbol: String,
        asset_id: u128,
        liquidity: Amount,
        price: Amount,
        slope: Amount,
    ) -> Self {
        Self {
            symbol,
            asset_id,
            liquidity,
            price,
            slope,
            fetched_at: Utc::now(),
            last_submitted: None,
        }
    }
    
    /// Check if this data is stale compared to real-time data
    /// 
    /// Staleness criteria:
    /// 1. Price difference > threshold (e.g., 0.5%)
    /// 2. Liquidity difference > threshold (e.g., 10%)
    /// 3. Time since last submission > timeout (e.g., 5 minutes)
    pub fn is_stale(
        &self,
        current_liquidity: Amount,
        current_price: Amount,
        current_slope: Amount,
        price_threshold: f64,
        liquidity_threshold: f64,
        time_threshold_secs: i64,
    ) -> (bool, String) {
        // Check price difference
        let price_diff = self.calculate_percentage_diff(self.price, current_price);
        if price_diff >= price_threshold {
            return (
                true,
                format!("Price changed by {:.2}% (threshold: {:.2}%)", 
                    price_diff * 100.0, 
                    price_threshold * 100.0)
            );
        }
        
        // Check liquidity difference
        let liquidity_diff = self.calculate_percentage_diff(self.liquidity, current_liquidity);
        if liquidity_diff >= liquidity_threshold {
            return (
                true,
                format!("Liquidity changed by {:.2}% (threshold: {:.2}%)", 
                    liquidity_diff * 100.0, 
                    liquidity_threshold * 100.0)
            );
        }
        
        // Check time threshold
        let elapsed = Utc::now()
            .signed_duration_since(self.last_submitted.unwrap_or(self.fetched_at))
            .num_seconds();
        
        if elapsed >= time_threshold_secs {
            return (
                true,
                format!("Time threshold exceeded: {}s (threshold: {}s)", 
                    elapsed, 
                    time_threshold_secs)
            );
        }
        
        (false, String::new())
    }
    
    /// Calculate percentage difference between two amounts
    fn calculate_percentage_diff(&self, old: Amount, new: Amount) -> f64 {
        let old_f64 = old.to_u128_raw() as f64;
        let new_f64 = new.to_u128_raw() as f64;
        
        if old_f64 == 0.0 {
            if new_f64 == 0.0 {
                return 0.0;
            } else {
                return 1.0; // 100% change
            }
        }
        
        ((new_f64 - old_f64).abs() / old_f64)
    }
    
    /// Mark this data as submitted
    pub fn mark_submitted(&mut self) {
        self.last_submitted = Some(Utc::now());
    }
}

/// Configuration for staleness detection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StalenessConfig {
    /// Price change threshold (e.g., 0.005 = 0.5%)
    pub price_threshold: f64,
    
    /// Liquidity change threshold (e.g., 0.10 = 10%)
    pub liquidity_threshold: f64,
    
    /// Time threshold in seconds (e.g., 300 = 5 minutes)
    pub time_threshold_secs: i64,
}

impl Default for StalenessConfig {
    fn default() -> Self {
        Self {
            price_threshold: 0.005,     // 0.5%
            liquidity_threshold: 0.10,  // 10%
            time_threshold_secs: 300,   // 5 minutes
        }
    }
}

/// Market data cache manager
#[derive(Debug)]
pub struct MarketDataCache {
    /// Cached data per asset (symbol -> CachedMarketData)
    cache: HashMap<String, CachedMarketData>,
    
    /// Staleness detection config
    pub(crate) config: StalenessConfig,
}

impl MarketDataCache {
    pub fn new(config: StalenessConfig) -> Self {
        Self {
            cache: HashMap::new(),
            config,
        }
    }
    
    /// Update cache with on-chain data
    pub fn update_from_onchain(
        &mut self,
        symbol: String,
        asset_id: u128,
        liquidity: Amount,
        price: Amount,
        slope: Amount,
    ) {
        let cached = CachedMarketData::new(symbol.clone(), asset_id, liquidity, price, slope);
        self.cache.insert(symbol, cached);
    }
    
    /// Check which assets have stale data
    /// Returns list of (symbol, reason) for stale assets
    pub fn find_stale_assets(
        &self,
        current_data: &HashMap<String, (Amount, Amount, Amount)>, // (liquidity, price, slope)
    ) -> Vec<(String, String)> {
        let mut stale = Vec::new();
        
        for (symbol, (liquidity, price, slope)) in current_data {
            if let Some(cached) = self.cache.get(symbol) {
                let (is_stale, reason) = cached.is_stale(
                    *liquidity,
                    *price,
                    *slope,
                    self.config.price_threshold,
                    self.config.liquidity_threshold,
                    self.config.time_threshold_secs,
                );
                
                if is_stale {
                    stale.push((symbol.clone(), reason));
                }
            } else {
                // Not in cache = never submitted before = stale
                stale.push((symbol.clone(), "Not yet submitted".to_string()));
            }
        }
        
        stale
    }
    
    /// Mark assets as submitted
    pub fn mark_submitted(&mut self, symbols: &[String]) {
        for symbol in symbols {
            if let Some(cached) = self.cache.get_mut(symbol) {
                cached.mark_submitted();
            }
        }
    }
    
    /// Get cached data for an asset
    pub fn get(&self, symbol: &str) -> Option<&CachedMarketData> {
        self.cache.get(symbol)
    }
    
    /// Get all cached symbols
    pub fn get_all_symbols(&self) -> Vec<String> {
        self.cache.keys().cloned().collect()
    }
    
    /// Get config reference
    pub fn config(&self) -> &StalenessConfig {
        &self.config
    }
}