use serde::{Deserialize, Serialize};

/// Request to get quotes for specific assets
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QuoteAssetsRequest {
    /// Asset IDs (Labels) to quote
    pub assets: Vec<u128>,
}

/// Response with market data for stale assets only
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AssetsQuote {
    /// Asset IDs (stale subset only)
    pub assets: Vec<u128>,
    
    /// Liquidity for each asset (in base asset units)
    pub liquidity: Vec<f64>,
    
    /// Prices for each asset (in USD)
    pub prices: Vec<f64>,
    
    /// Slopes for each asset (price impact measure)
    pub slopes: Vec<f64>,
}

impl AssetsQuote {
    pub fn new() -> Self {
        Self {
            assets: Vec::new(),
            liquidity: Vec::new(),
            prices: Vec::new(),
            slopes: Vec::new(),
        }
    }
    
    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }
    
    pub fn len(&self) -> usize {
        self.assets.len()
    }
}

/// Health check response
#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub vendor_id: u128,
    pub tracked_assets: usize,
}

/// Error response
#[derive(Debug, Clone, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}