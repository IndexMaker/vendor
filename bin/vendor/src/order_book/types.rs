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
    /// Fee multiplier for slope calculation (k > 1, e.g., 1.01)
    /// Applied to slope to account for exchange fees
    pub fee_multiplier: f64,
    /// Default liquidity value when zero liquidity is encountered
    /// Used as fallback when no previous value is available
    pub default_liquidity: f64,
}

impl Default for OrderBookConfig {
    fn default() -> Self {
        Self {
            depth_levels: 5,  // K = 5 (top 5 levels)
            fee_multiplier: 1.01,  // 1% fee adjustment for slope
            default_liquidity: 1.0,  // Fallback for zero liquidity
        }
    }
}

/// P/S/L vectors for on-chain submission (AC #6)
///
/// Output format: `{P: [...], S: [...], L: [...]}` vectors
/// Vector ordering matches input asset_ids ordering for deterministic on-chain processing
#[derive(Debug, Clone)]
pub struct PSLVectors {
    /// Asset IDs in submission order
    pub asset_ids: Vec<u128>,
    /// Price (P) micro-prices for each asset
    pub prices: Vec<Amount>,
    /// Slope (S) values for each asset (with fee multiplier applied)
    pub slopes: Vec<Amount>,
    /// Liquidity (L) values for each asset
    pub liquidities: Vec<Amount>,
    /// When these vectors were computed
    pub computed_at: chrono::DateTime<chrono::Utc>,
}

impl PSLVectors {
    /// Create a new empty PSLVectors instance
    pub fn new() -> Self {
        Self {
            asset_ids: Vec::new(),
            prices: Vec::new(),
            slopes: Vec::new(),
            liquidities: Vec::new(),
            computed_at: chrono::Utc::now(),
        }
    }

    /// Create PSLVectors with pre-allocated capacity
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            asset_ids: Vec::with_capacity(capacity),
            prices: Vec::with_capacity(capacity),
            slopes: Vec::with_capacity(capacity),
            liquidities: Vec::with_capacity(capacity),
            computed_at: chrono::Utc::now(),
        }
    }

    /// Add metrics for a single asset
    pub fn push(&mut self, asset_id: u128, price: Amount, slope: Amount, liquidity: Amount) {
        self.asset_ids.push(asset_id);
        self.prices.push(price);
        self.slopes.push(slope);
        self.liquidities.push(liquidity);
    }

    /// Add metrics from an AssetMetrics struct
    pub fn push_metrics(&mut self, metrics: &AssetMetrics) {
        self.asset_ids.push(metrics.asset_id);
        self.prices.push(metrics.price);
        self.slopes.push(metrics.slope);
        self.liquidities.push(metrics.liquidity);
    }

    /// Number of assets in the vectors
    pub fn len(&self) -> usize {
        self.asset_ids.len()
    }

    /// Check if vectors are empty
    pub fn is_empty(&self) -> bool {
        self.asset_ids.is_empty()
    }

    /// Verify all vectors have matching lengths (invariant check)
    pub fn is_valid(&self) -> bool {
        let len = self.asset_ids.len();
        self.prices.len() == len
            && self.slopes.len() == len
            && self.liquidities.len() == len
    }
}

impl Default for PSLVectors {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_psl_vectors_new() {
        let vectors = PSLVectors::new();
        assert!(vectors.is_empty());
        assert_eq!(vectors.len(), 0);
        assert!(vectors.is_valid());
    }

    #[test]
    fn test_psl_vectors_with_capacity() {
        let vectors = PSLVectors::with_capacity(10);
        assert!(vectors.is_empty());
        assert!(vectors.is_valid());
    }

    #[test]
    fn test_psl_vectors_push() {
        let mut vectors = PSLVectors::new();

        vectors.push(
            1, // asset_id
            Amount::from_u128_raw((100.0 * 1e18) as u128), // price
            Amount::from_u128_raw((0.01 * 1e18) as u128),  // slope
            Amount::from_u128_raw((1000.0 * 1e18) as u128), // liquidity
        );

        assert_eq!(vectors.len(), 1);
        assert!(!vectors.is_empty());
        assert!(vectors.is_valid());

        assert_eq!(vectors.asset_ids[0], 1);
        assert_eq!(vectors.prices[0].to_u128_raw() as f64 / 1e18, 100.0);
        assert_eq!(vectors.slopes[0].to_u128_raw() as f64 / 1e18, 0.01);
        assert_eq!(vectors.liquidities[0].to_u128_raw() as f64 / 1e18, 1000.0);
    }

    #[test]
    fn test_psl_vectors_push_metrics() {
        let mut vectors = PSLVectors::new();

        let metrics = AssetMetrics {
            symbol: "BTCUSDC".to_string(),
            asset_id: 42,
            liquidity: Amount::from_u128_raw((500.0 * 1e18) as u128),
            price: Amount::from_u128_raw((50000.0 * 1e18) as u128),
            slope: Amount::from_u128_raw((0.001 * 1e18) as u128),
            calculated_at: chrono::Utc::now(),
        };

        vectors.push_metrics(&metrics);

        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors.asset_ids[0], 42);
        // Use approximate comparison for floating point
        let price = vectors.prices[0].to_u128_raw() as f64 / 1e18;
        assert!((price - 50000.0).abs() < 0.01, "Price should be approximately 50000");
    }

    #[test]
    fn test_psl_vectors_multiple_assets() {
        let mut vectors = PSLVectors::new();

        // Add 3 assets
        for i in 1..=3 {
            vectors.push(
                i as u128,
                Amount::from_u128_raw((100.0 * i as f64 * 1e18) as u128),
                Amount::from_u128_raw((0.01 * 1e18) as u128),
                Amount::from_u128_raw((1000.0 * 1e18) as u128),
            );
        }

        assert_eq!(vectors.len(), 3);
        assert!(vectors.is_valid());

        // Verify order is preserved
        assert_eq!(vectors.asset_ids, vec![1, 2, 3]);
    }

    #[test]
    fn test_order_book_config_default() {
        let config = OrderBookConfig::default();
        assert_eq!(config.depth_levels, 5);
        assert_eq!(config.fee_multiplier, 1.01);
        assert_eq!(config.default_liquidity, 1.0);
    }
}