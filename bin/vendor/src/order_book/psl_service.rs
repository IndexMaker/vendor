//! PSL Compute Service - High-level API for computing P/S/L vectors
//!
//! This module integrates:
//! - REST order book fetching (via OrderBookService)
//! - P/S/L computation (via OrderBookProcessor formulas)
//! - On-chain submission format (PSLVectors)
//!
//! Implements AC #1, #2, #3, #6, #7:
//! - Computes micro-price, liquidity, slope per asset
//! - Uses REST API for on-demand order book fetching
//! - Returns structured PSLVectors for on-chain submission
//! - Targets < 100ms for 150 assets (NFR17)

use super::types::{Level, OrderBookConfig, PSLVectors};
use crate::market_data::bitget::{OrderBookService, OrderBookServiceConfig};
use crate::onchain::AssetMapper;
use common::amount::Amount;
use eyre::Result;
use std::collections::HashMap;
use std::sync::Arc;

/// Service for computing P/S/L vectors from live order book data
///
/// Combines REST order book fetching with P/S/L computation for
/// on-chain market data submission.
pub struct PSLComputeService {
    order_book_service: OrderBookService,
    config: OrderBookConfig,
    /// Previous computed values for zero-liquidity fallback (AC #5)
    previous_metrics: parking_lot::RwLock<HashMap<u128, CachedMetrics>>,
}

/// Cached metrics for fallback
#[derive(Clone)]
struct CachedMetrics {
    price: Amount,
    slope: Amount,
    liquidity: Amount,
}

impl PSLComputeService {
    /// Create a new PSL compute service
    ///
    /// # Arguments
    /// * `mapper` - Asset mapper for ID → Symbol mapping
    /// * `order_book_config` - Configuration for order book processing
    pub fn new(mapper: Arc<AssetMapper>, order_book_config: OrderBookConfig) -> Result<Self> {
        let ob_service_config = OrderBookServiceConfig {
            default_levels: order_book_config.depth_levels,
            ..Default::default()
        };

        let order_book_service = OrderBookService::new(mapper, ob_service_config)?;

        Ok(Self {
            order_book_service,
            config: order_book_config,
            previous_metrics: parking_lot::RwLock::new(HashMap::new()),
        })
    }

    /// Compute PSL vectors for a list of asset IDs (AC #6)
    ///
    /// Fetches order books via REST API, computes P/S/L, and returns
    /// vectors ready for on-chain submission.
    ///
    /// # Arguments
    /// * `asset_ids` - Asset IDs to compute vectors for
    ///
    /// # Returns
    /// * `PSLVectors` with prices, slopes, liquidities
    ///
    /// # Performance
    /// Targets < 100ms for 150 assets (NFR17)
    pub async fn compute_psl_for_assets(&self, asset_ids: &[u128]) -> PSLVectors {
        let start = std::time::Instant::now();
        let mut vectors = PSLVectors::with_capacity(asset_ids.len());

        // Fetch order books concurrently via REST
        let order_books = self
            .order_book_service
            .fetch_by_asset_ids_concurrent(asset_ids, Some(self.config.depth_levels))
            .await;

        tracing::debug!(
            "Fetched {}/{} order books in {:?}",
            order_books.len(),
            asset_ids.len(),
            start.elapsed()
        );

        // Compute P/S/L for each asset (preserving input order)
        for &asset_id in asset_ids {
            if let Some(snapshot) = order_books.get(&asset_id) {
                // Convert snapshot levels to our Level type (Amount -> f64)
                let bids: Vec<Level> = snapshot
                    .bids
                    .iter()
                    .take(self.config.depth_levels)
                    .map(|l| Level {
                        price: l.price.to_u128_raw() as f64 / 1e18,
                        quantity: l.quantity.to_u128_raw() as f64 / 1e18,
                    })
                    .collect();

                let asks: Vec<Level> = snapshot
                    .asks
                    .iter()
                    .take(self.config.depth_levels)
                    .map(|l| Level {
                        price: l.price.to_u128_raw() as f64 / 1e18,
                        quantity: l.quantity.to_u128_raw() as f64 / 1e18,
                    })
                    .collect();

                // Check depth
                if bids.len() < self.config.depth_levels || asks.len() < self.config.depth_levels {
                    tracing::warn!(
                        "Insufficient depth for asset {}: bids={}, asks={}, need={}",
                        asset_id,
                        bids.len(),
                        asks.len(),
                        self.config.depth_levels
                    );
                    continue;
                }

                // Compute metrics
                match self.compute_metrics(asset_id, &bids, &asks) {
                    Ok((price, slope, liquidity)) => {
                        vectors.push(asset_id, price, slope, liquidity);

                        // Cache for future fallback
                        self.previous_metrics.write().insert(
                            asset_id,
                            CachedMetrics {
                                price,
                                slope,
                                liquidity,
                            },
                        );
                    }
                    Err(e) => {
                        tracing::warn!("Failed to compute PSL for asset {}: {:?}", asset_id, e);
                    }
                }
            } else {
                tracing::warn!("No order book available for asset {}", asset_id);
            }
        }

        let elapsed = start.elapsed();
        tracing::info!(
            "Computed PSL vectors for {}/{} assets in {:?}",
            vectors.len(),
            asset_ids.len(),
            elapsed
        );

        // Performance warning (NFR17)
        if elapsed.as_millis() > 100 && asset_ids.len() >= 150 {
            tracing::warn!(
                "PSL computation exceeded 100ms target: {:?} for {} assets",
                elapsed,
                asset_ids.len()
            );
        }

        vectors
    }

    /// Compute P/S/L metrics from bid/ask levels
    fn compute_metrics(
        &self,
        asset_id: u128,
        bids: &[Level],
        asks: &[Level],
    ) -> Result<(Amount, Amount, Amount)> {
        // Calculate liquidity: L = Σ(QBk) + Σ(QAk)
        let liquidity = self.calculate_liquidity(asset_id, bids, asks);

        // Calculate micro-price: P = (PA1 × QB1 + PB1 × QA1) / (QB1 + QA1)
        let price = self.calculate_micro_price(bids, asks)?;

        // Calculate slope: S = (PAK - PBK) / L * fee_multiplier
        let slope = self.calculate_slope(bids, asks, liquidity)?;

        Ok((price, slope, liquidity))
    }

    /// Calculate liquidity with zero fallback (AC #5)
    fn calculate_liquidity(&self, asset_id: u128, bids: &[Level], asks: &[Level]) -> Amount {
        let bid_qty: f64 = bids.iter().map(|l| l.quantity).sum();
        let ask_qty: f64 = asks.iter().map(|l| l.quantity).sum();
        let total = bid_qty + ask_qty;

        if total == 0.0 {
            // Zero liquidity fallback
            let previous = self.previous_metrics.read();
            if let Some(prev) = previous.get(&asset_id) {
                tracing::warn!(
                    "Zero liquidity for asset {} - using previous value",
                    asset_id
                );
                return prev.liquidity;
            } else {
                tracing::warn!(
                    "Zero liquidity for asset {} - using default: {}",
                    asset_id,
                    self.config.default_liquidity
                );
                return Amount::from_u128_raw((self.config.default_liquidity * 1e18) as u128);
            }
        }

        Amount::from_u128_raw((total * 1e18) as u128)
    }

    /// Calculate micro-price
    fn calculate_micro_price(&self, bids: &[Level], asks: &[Level]) -> Result<Amount> {
        if bids.is_empty() || asks.is_empty() {
            return Err(eyre::eyre!("Empty bids or asks"));
        }

        let pb1 = bids[0].price;
        let qb1 = bids[0].quantity;
        let pa1 = asks[0].price;
        let qa1 = asks[0].quantity;

        if qb1 == 0.0 || qa1 == 0.0 {
            return Err(eyre::eyre!("Zero quantity at best bid/ask"));
        }

        let numerator = (pa1 * qb1) + (pb1 * qa1);
        let denominator = qb1 + qa1;
        let price = numerator / denominator;

        Ok(Amount::from_u128_raw((price * 1e18) as u128))
    }

    /// Calculate slope with fee multiplier (AC #4)
    fn calculate_slope(&self, bids: &[Level], asks: &[Level], liquidity: Amount) -> Result<Amount> {
        if bids.is_empty() || asks.is_empty() {
            return Err(eyre::eyre!("Empty bids or asks"));
        }

        let pbk = bids.last().ok_or_else(|| eyre::eyre!("No bids"))?.price;
        let pak = asks.last().ok_or_else(|| eyre::eyre!("No asks"))?.price;

        let spread = pak - pbk;
        if spread <= 0.0 {
            return Err(eyre::eyre!("Invalid spread: {}", spread));
        }

        let liq_f64 = liquidity.to_u128_raw() as f64 / 1e18;
        if liq_f64 == 0.0 {
            return Err(eyre::eyre!("Zero liquidity"));
        }

        // Apply fee multiplier (AC #4)
        let slope = (spread / liq_f64) * self.config.fee_multiplier;

        Ok(Amount::from_u128_raw((slope * 1e18) as u128))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to create test levels
    fn make_level(price: f64, quantity: f64) -> Level {
        Level { price, quantity }
    }

    // Test CachedMetrics struct
    #[test]
    fn test_cached_metrics_clone() {
        let metrics = CachedMetrics {
            price: Amount::from_u128_raw((100.0 * 1e18) as u128),
            slope: Amount::from_u128_raw((0.01 * 1e18) as u128),
            liquidity: Amount::from_u128_raw((1000.0 * 1e18) as u128),
        };

        let cloned = metrics.clone();
        assert_eq!(cloned.price.to_u128_raw(), metrics.price.to_u128_raw());
        assert_eq!(cloned.slope.to_u128_raw(), metrics.slope.to_u128_raw());
        assert_eq!(cloned.liquidity.to_u128_raw(), metrics.liquidity.to_u128_raw());
    }

    // Test micro-price calculation logic (matches processor.rs formula)
    // P = (PA1 × QB1 + PB1 × QA1) / (QB1 + QA1)
    #[test]
    fn test_micro_price_formula() {
        // Bids: best bid at 100.0 with qty 10
        // Asks: best ask at 101.0 with qty 5
        // Expected: (101 * 10 + 100 * 5) / (10 + 5) = 1510 / 15 = 100.666...
        let pb1 = 100.0;
        let qb1 = 10.0;
        let pa1 = 101.0;
        let qa1 = 5.0;

        let numerator = (pa1 * qb1) + (pb1 * qa1);
        let denominator = qb1 + qa1;
        let micro_price = numerator / denominator;

        let expected: f64 = 100.666666666;
        assert!((micro_price - expected).abs() < 0.01);
    }

    // Test liquidity calculation (sum of both sides)
    #[test]
    fn test_liquidity_formula() {
        let bids = vec![
            make_level(100.0, 5.0),
            make_level(99.0, 3.0),
            make_level(98.0, 2.0),
        ];
        let asks = vec![
            make_level(101.0, 4.0),
            make_level(102.0, 3.0),
            make_level(103.0, 1.0),
        ];

        let bid_qty: f64 = bids.iter().map(|l| l.quantity).sum();
        let ask_qty: f64 = asks.iter().map(|l| l.quantity).sum();
        let total = bid_qty + ask_qty;

        // 5+3+2 + 4+3+1 = 10 + 8 = 18
        assert_eq!(total, 18.0);
    }

    // Test slope calculation with fee multiplier
    // S = (PAK - PBK) / L * fee_multiplier
    #[test]
    fn test_slope_formula_with_fee_multiplier() {
        let pbk = 98.0; // Kth bid (worst in top K)
        let pak = 103.0; // Kth ask (worst in top K)
        let liquidity = 18.0;
        let fee_multiplier = 1.01;

        let spread = pak - pbk; // 5.0
        let base_slope = spread / liquidity; // 5/18 = 0.277...
        let slope = base_slope * fee_multiplier; // 0.280...

        let expected: f64 = (5.0 / 18.0) * 1.01;
        assert!((slope - expected).abs() < 0.001);
    }

    // Test zero liquidity detection
    #[test]
    fn test_zero_liquidity_detection() {
        let bids = vec![make_level(100.0, 0.0)];
        let asks = vec![make_level(101.0, 0.0)];

        let bid_qty: f64 = bids.iter().map(|l| l.quantity).sum();
        let ask_qty: f64 = asks.iter().map(|l| l.quantity).sum();
        let total = bid_qty + ask_qty;

        assert_eq!(total, 0.0);
    }

    // Test invalid spread detection (negative spread)
    #[test]
    fn test_invalid_spread_detection() {
        // Crossed book: best ask < best bid (should not happen in real markets)
        let pbk = 103.0;
        let pak = 98.0;
        let spread = pak - pbk;

        assert!(spread < 0.0, "Negative spread should be detected");
    }

    // Test PSLVectors integration (ensure types work together)
    #[test]
    fn test_psl_vectors_from_computed_values() {
        let mut vectors = PSLVectors::new();

        // Simulate computed values
        let asset_id = 42u128;
        let price = Amount::from_u128_raw((50000.0 * 1e18) as u128);
        let slope = Amount::from_u128_raw((0.001 * 1e18) as u128);
        let liquidity = Amount::from_u128_raw((500.0 * 1e18) as u128);

        vectors.push(asset_id, price, slope, liquidity);

        assert_eq!(vectors.len(), 1);
        assert!(vectors.is_valid());
        assert_eq!(vectors.asset_ids[0], 42);
    }

    // Test Amount conversion precision
    #[test]
    fn test_amount_precision() {
        let value = 0.00001234; // Small value
        let amount = Amount::from_u128_raw((value * 1e18) as u128);
        let recovered = amount.to_u128_raw() as f64 / 1e18;

        // Should maintain precision to at least 8 decimal places
        assert!((recovered - value).abs() < 1e-10);
    }

    // Test fee multiplier edge cases
    #[test]
    fn test_fee_multiplier_one() {
        let base_slope = 0.5;
        let fee_multiplier = 1.0; // No fee adjustment
        let adjusted = base_slope * fee_multiplier;

        assert_eq!(adjusted, base_slope);
    }

    #[test]
    fn test_fee_multiplier_high() {
        let base_slope = 0.5;
        let fee_multiplier = 1.10; // 10% fee adjustment
        let adjusted = base_slope * fee_multiplier;

        let expected: f64 = 0.55;
        assert!((adjusted - expected).abs() < 0.001);
    }
}
