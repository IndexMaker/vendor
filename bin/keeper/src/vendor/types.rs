use serde::{Deserialize, Serialize};

/// Request to get quotes for specific assets
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteAssetsRequest {
    pub assets: Vec<u128>,
}

/// Response containing market data for stale assets only
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetsQuote {
    pub assets: Vec<u128>,
    pub liquidity: Vec<f64>,
    pub prices: Vec<f64>,
    pub slopes: Vec<f64>,
}

impl AssetsQuote {
    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }

    pub fn len(&self) -> usize {
        self.assets.len()
    }
}

/// Health check response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub vendor_id: u128,
    pub tracked_assets: usize,
}