use chrono::{DateTime, Utc};
use common::amount::Amount;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Margin information for a single asset
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetMargin {
    pub symbol: String,
    pub asset_id: u128,
    pub margin: Amount,
    pub last_price: Amount,
    pub last_updated: DateTime<Utc>,
}

impl AssetMargin {
    pub fn new(symbol: String, asset_id: u128, margin: Amount, price: Amount) -> Self {
        Self {
            symbol,
            asset_id,
            margin,
            last_price: price,
            last_updated: Utc::now(),
        }
    }
    
    /// Check if margin needs update based on price change
    pub fn needs_update(&self, current_price: Amount, threshold: f64) -> bool {
        let old_price_f64 = self.last_price.to_u128_raw() as f64;
        let new_price_f64 = current_price.to_u128_raw() as f64;
        
        if old_price_f64 == 0.0 {
            return true;
        }
        
        let change = (new_price_f64 - old_price_f64).abs() / old_price_f64;
        change >= threshold
    }
    
    /// Check if margin needs update based on time
    pub fn needs_update_by_time(&self, timeout_secs: u64) -> bool {
        let elapsed = Utc::now()
            .signed_duration_since(self.last_updated)
            .num_seconds();
        
        elapsed >= timeout_secs as i64
    }
}

/// Overall margin state tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarginState {
    /// Margin per asset (symbol -> AssetMargin)
    pub margins: HashMap<String, AssetMargin>,
    
    /// Last submission timestamp
    pub last_submission: Option<DateTime<Utc>>,
    
    /// Total number of assets
    pub total_assets: usize,
}

impl MarginState {
    pub fn new() -> Self {
        Self {
            margins: HashMap::new(),
            last_submission: None,
            total_assets: 0,
        }
    }
    
    /// Update margin for an asset
    pub fn update_margin(&mut self, margin: AssetMargin) {
        self.margins.insert(margin.symbol.clone(), margin);
        self.total_assets = self.margins.len();
    }
    
    /// Get assets that need margin update
    pub fn get_stale_margins(&self, price_threshold: f64, time_threshold: u64) -> Vec<String> {
        let mut stale = Vec::new();
        
        for (symbol, margin) in &self.margins {
            if margin.needs_update_by_time(time_threshold) {
                stale.push(symbol.clone());
            }
        }
        
        stale
    }
    
    /// Check if any margin needs update based on new prices
    pub fn check_price_changes(
        &self,
        current_prices: &HashMap<String, Amount>,
        threshold: f64,
    ) -> Vec<String> {
        let mut needs_update = Vec::new();
        
        for (symbol, margin) in &self.margins {
            if let Some(&current_price) = current_prices.get(symbol) {
                if margin.needs_update(current_price, threshold) {
                    needs_update.push(symbol.clone());
                }
            }
        }
        
        needs_update
    }
}

impl Default for MarginState {
    fn default() -> Self {
        Self::new()
    }
}