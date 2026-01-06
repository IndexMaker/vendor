use common::amount::Amount;
use serde::{Deserialize, Serialize};

/// A single level in the order book (bid or ask)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Level {
    pub price: f64,
    pub quantity: f64,
}

/// Order book snapshot for a symbol
#[derive(Debug, Clone)]
pub struct OrderBook {
    pub symbol: String,
    pub bids: Vec<Level>,  // Sorted descending (best bid first)
    pub asks: Vec<Level>,  // Sorted ascending (best ask first)
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl OrderBook {
    pub fn new(symbol: String) -> Self {
        Self {
            symbol,
            bids: Vec::new(),
            asks: Vec::new(),
            timestamp: chrono::Utc::now(),
        }
    }
    
    /// Check if order book has sufficient depth
    pub fn has_sufficient_depth(&self, min_levels: usize) -> bool {
        self.bids.len() >= min_levels && self.asks.len() >= min_levels
    }
}

/// Calculated metrics for an asset from order book
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetMetrics {
    pub symbol: String,
    pub asset_id: u128,
    
    /// Liquidity (L): Sum of quantities across K levels
    pub liquidity: Amount,
    
    /// Price (P): Micro-price (weighted mid-price)
    pub price: Amount,
    
    /// Slope (S): Price impact measure
    pub slope: Amount,
    
    /// When these metrics were calculated
    pub calculated_at: chrono::DateTime<chrono::Utc>,
}

/// Configuration for order book processing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookConfig {
    /// Number of levels to scan (K)
    pub depth_levels: usize,
}

impl Default for OrderBookConfig {
    fn default() -> Self {
        Self {
            depth_levels: 5,  // K = 5 (top 5 levels)
        }
    }
}