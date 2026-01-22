use super::types::{AssetMetrics, OrderBook, OrderBookConfig, Level, PSLVectors};
use crate::onchain::{AssetMapper, PriceTracker};
use common::amount::Amount;
use eyre::{eyre, Result};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Processes order book data to calculate market metrics (L, P, S)
///
/// Formulas from DeIndex paper (Section 5.1):
/// - Liquidity (L): Σ(QBk) + Σ(QAk) for k=1 to K
/// - Price (P): (PA1 × QB1 + PB1 × QA1) / (QB1 + QA1)  [Micro-Price]
/// - Slope (S): (PAK - PBK) / L
///
/// Handles zero liquidity gracefully per AC #5:
/// - Uses previous metrics cache when current liquidity is zero
/// - Falls back to configurable default when no previous value exists
pub struct OrderBookProcessor {
    price_tracker: Arc<PriceTracker>,
    asset_mapper: Arc<RwLock<AssetMapper>>,
    config: OrderBookConfig,
    /// Cache of previous metrics per symbol for zero liquidity fallback (AC #5)
    previous_metrics: RwLock<HashMap<String, AssetMetrics>>,
}

impl OrderBookProcessor {
    pub fn new(
        price_tracker: Arc<PriceTracker>,
        asset_mapper: Arc<RwLock<AssetMapper>>,
        config: OrderBookConfig,
    ) -> Self {
        Self {
            price_tracker,
            asset_mapper,
            config,
            previous_metrics: RwLock::new(HashMap::new()),
        }
    }
    
    /// Calculate L, P, S metrics from order book for a single asset
    pub fn calculate_metrics(&self, symbol: &str) -> Result<AssetMetrics> {
        // Get order book
        let book = self
            .price_tracker
            .get_order_book(symbol)
            .ok_or_else(|| eyre!("Order book not available for {}", symbol))?;

        // Get asset ID
        let asset_id = self
            .asset_mapper
            .read()
            .get_id(symbol)
            .ok_or_else(|| eyre!("Asset ID not found for {}", symbol))?;

        // Check depth
        let k = self.config.depth_levels;
        if !book.has_sufficient_depth(k) {
            return Err(eyre!(
                "Insufficient order book depth for {} (need {}, have bids={}, asks={})",
                symbol,
                k,
                book.bids.len(),
                book.asks.len()
            ));
        }

        // Get top K levels
        let bids = &book.bids[..k];
        let asks = &book.asks[..k];

        // Calculate metrics (liquidity now handles zero fallback per AC #5)
        let liquidity = self.calculate_liquidity(symbol, bids, asks);
        let price = self.calculate_micro_price(bids, asks)?;
        let slope = self.calculate_slope(bids, asks, liquidity)?;

        tracing::trace!(
            "Calculated metrics for {}: L={:.2}, P={:.2}, S={:.6}",
            symbol,
            liquidity.to_u128_raw() as f64 / 1e18,
            price.to_u128_raw() as f64 / 1e18,
            slope.to_u128_raw() as f64 / 1e18
        );

        let metrics = AssetMetrics {
            symbol: symbol.to_string(),
            asset_id,
            liquidity,
            price,
            slope,
            calculated_at: chrono::Utc::now(),
        };

        // Cache successful metrics for future zero liquidity fallback (AC #5)
        self.previous_metrics
            .write()
            .insert(symbol.to_string(), metrics.clone());

        Ok(metrics)
    }
    
    /// Calculate liquidity: L = Σ(QBk) + Σ(QAk)
    ///
    /// Sum of all quantities across K levels on both sides
    /// Per AC #5: If zero liquidity, uses previous value or configurable default
    fn calculate_liquidity(&self, symbol: &str, bids: &[Level], asks: &[Level]) -> Amount {
        let bid_qty: f64 = bids.iter().map(|l| l.quantity).sum();
        let ask_qty: f64 = asks.iter().map(|l| l.quantity).sum();
        let total = bid_qty + ask_qty;

        if total == 0.0 {
            // Zero liquidity - use fallback (AC #5)
            let previous = self.previous_metrics.read();
            if let Some(prev_metrics) = previous.get(symbol) {
                tracing::warn!(
                    "Zero liquidity for {} - using previous value: {:.4}",
                    symbol,
                    prev_metrics.liquidity.to_u128_raw() as f64 / 1e18
                );
                return prev_metrics.liquidity;
            } else {
                tracing::warn!(
                    "Zero liquidity for {} - using default: {:.4}",
                    symbol,
                    self.config.default_liquidity
                );
                return Amount::from_u128_raw((self.config.default_liquidity * 1e18) as u128);
            }
        }

        Amount::from_u128_raw((total * 1e18) as u128)
    }
    
    /// Calculate micro-price: P = (PA1 × QB1 + PB1 × QA1) / (QB1 + QA1)
    /// 
    /// Weighted mid-price using best bid/ask quantities as weights
    /// This is more stable than simple mid-price
    fn calculate_micro_price(&self, bids: &[Level], asks: &[Level]) -> Result<Amount> {
        if bids.is_empty() || asks.is_empty() {
            return Err(eyre!("Empty bids or asks"));
        }
        
        let pb1 = bids[0].price;  // Best bid price
        let qb1 = bids[0].quantity;  // Best bid quantity
        let pa1 = asks[0].price;  // Best ask price
        let qa1 = asks[0].quantity;  // Best ask quantity
        
        if qb1 == 0.0 || qa1 == 0.0 {
            return Err(eyre!("Zero quantity at best bid/ask"));
        }
        
        // Numerator: PA1 × QB1 + PB1 × QA1
        let numerator = (pa1 * qb1) + (pb1 * qa1);
        
        // Denominator: QB1 + QA1
        let denominator = qb1 + qa1;
        
        // Micro-price
        let price = numerator / denominator;
        
        Ok(Amount::from_u128_raw((price * 1e18) as u128))
    }
    
    /// Calculate slope: S = (PAK - PBK) / L * fee_multiplier
    ///
    /// Price impact measure: spread across K levels divided by total liquidity
    /// Higher slope = less liquidity = more price impact
    /// Fee multiplier (k > 1, e.g., 1.01) accounts for exchange fees per AC #4
    fn calculate_slope(&self, bids: &[Level], asks: &[Level], liquidity: Amount) -> Result<Amount> {
        if bids.is_empty() || asks.is_empty() {
            return Err(eyre!("Empty bids or asks"));
        }

        // Get Kth level prices (last in our slices)
        let pbk = bids.last().ok_or_else(|| eyre!("No bids"))?.price;
        let pak = asks.last().ok_or_else(|| eyre!("No asks"))?.price;

        // Spread across K levels
        let spread = pak - pbk;

        if spread <= 0.0 {
            return Err(eyre!("Invalid spread: {} (PAK={}, PBK={})", spread, pak, pbk));
        }

        // Liquidity in f64
        let liq_f64 = liquidity.to_u128_raw() as f64 / 1e18;

        if liq_f64 == 0.0 {
            return Err(eyre!("Zero liquidity"));
        }

        // Slope = (spread / liquidity) * fee_multiplier
        // Fee multiplier accounts for exchange fees (AC #4)
        let slope = (spread / liq_f64) * self.config.fee_multiplier;

        Ok(Amount::from_u128_raw((slope * 1e18) as u128))
    }
    
    /// Calculate metrics for multiple assets
    /// Returns only successful calculations, logs errors for failures
    pub fn calculate_metrics_batch(&self, symbols: &[String]) -> Vec<AssetMetrics> {
        let mut results = Vec::new();

        for symbol in symbols {
            match self.calculate_metrics(symbol) {
                Ok(metrics) => results.push(metrics),
                Err(e) => {
                    tracing::warn!("Failed to calculate metrics for {}: {:?}", symbol, e);
                }
            }
        }

        results
    }

    /// Compute PSL vectors for on-chain submission (AC #6)
    ///
    /// Takes asset IDs and returns P/S/L vectors in matching order.
    /// Skips assets that fail computation (logs warning) rather than failing entire batch.
    ///
    /// # Arguments
    /// * `asset_ids` - Asset IDs to compute vectors for
    ///
    /// # Returns
    /// * `PSLVectors` with prices, slopes, liquidities in asset_id order
    pub fn compute_psl_vectors(&self, asset_ids: &[u128]) -> PSLVectors {
        let start = std::time::Instant::now();
        let mut vectors = PSLVectors::with_capacity(asset_ids.len());

        for &asset_id in asset_ids {
            // Look up symbol from asset ID
            let symbol = {
                let mapper = self.asset_mapper.read();
                mapper.get_bitget_symbol(asset_id)
            };

            let Some(symbol) = symbol else {
                tracing::warn!("No symbol mapping for asset_id {} - skipping", asset_id);
                continue;
            };

            match self.calculate_metrics(&symbol) {
                Ok(metrics) => {
                    vectors.push_metrics(&metrics);
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to compute PSL for asset {} ({}): {:?}",
                        asset_id,
                        symbol,
                        e
                    );
                }
            }
        }

        let elapsed = start.elapsed();
        tracing::debug!(
            "Computed PSL vectors for {}/{} assets in {:?}",
            vectors.len(),
            asset_ids.len(),
            elapsed
        );

        // Log warning if we exceeded NFR17 (100ms for 150 assets)
        if elapsed.as_millis() > 100 && asset_ids.len() >= 150 {
            tracing::warn!(
                "PSL computation exceeded 100ms target: {:?} for {} assets",
                elapsed,
                asset_ids.len()
            );
        }

        vectors
    }

    /// Compute PSL vectors for assets by symbol (convenience method)
    ///
    /// Converts symbols to asset IDs and delegates to `compute_psl_vectors`
    pub fn compute_psl_vectors_by_symbols(&self, symbols: &[String]) -> PSLVectors {
        let mapper = self.asset_mapper.read();
        let asset_ids: Vec<u128> = symbols
            .iter()
            .filter_map(|s| mapper.get_id(s))
            .collect();
        drop(mapper);

        self.compute_psl_vectors(&asset_ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_processor(config: OrderBookConfig) -> OrderBookProcessor {
        OrderBookProcessor {
            price_tracker: Arc::new(PriceTracker::new()),
            asset_mapper: Arc::new(RwLock::new(AssetMapper::default())),
            config,
            previous_metrics: RwLock::new(HashMap::new()),
        }
    }

    #[test]
    fn test_calculate_liquidity() {
        let bids = vec![
            Level { price: 100.0, quantity: 5.0 },
            Level { price: 99.0, quantity: 3.0 },
        ];

        let asks = vec![
            Level { price: 101.0, quantity: 4.0 },
            Level { price: 102.0, quantity: 2.0 },
        ];

        let processor = create_test_processor(OrderBookConfig::default());
        let liquidity = processor.calculate_liquidity("TEST", &bids, &asks);

        // Total = 5 + 3 + 4 + 2 = 14
        assert_eq!(liquidity.to_u128_raw() as f64 / 1e18, 14.0);
    }

    #[test]
    fn test_calculate_micro_price() {
        let bids = vec![
            Level { price: 100.0, quantity: 10.0 },
        ];

        let asks = vec![
            Level { price: 101.0, quantity: 5.0 },
        ];

        let processor = create_test_processor(OrderBookConfig::default());
        let price = processor.calculate_micro_price(&bids, &asks).unwrap();

        // (101 * 10 + 100 * 5) / (10 + 5) = (1010 + 500) / 15 = 1510 / 15 = 100.666...
        let expected = 100.666666666;
        let actual = price.to_u128_raw() as f64 / 1e18;

        assert!((actual - expected).abs() < 0.01);
    }

    #[test]
    fn test_calculate_slope_with_fee_multiplier() {
        let bids = vec![
            Level { price: 100.0, quantity: 5.0 },
            Level { price: 99.0, quantity: 3.0 },
            Level { price: 98.0, quantity: 2.0 },
        ];

        let asks = vec![
            Level { price: 101.0, quantity: 4.0 },
            Level { price: 102.0, quantity: 3.0 },
            Level { price: 103.0, quantity: 1.0 },
        ];

        let processor = create_test_processor(OrderBookConfig::default());
        let liquidity = processor.calculate_liquidity("TEST", &bids, &asks);
        let slope = processor.calculate_slope(&bids, &asks, liquidity).unwrap();

        // Spread = 103 - 98 = 5
        // Liquidity = 18
        // Base slope = 5 / 18 = 0.277...
        // With fee_multiplier 1.01: 0.277... * 1.01 = 0.280...
        let base_slope = 5.0 / 18.0;
        let expected = base_slope * 1.01; // Default fee_multiplier
        let actual = slope.to_u128_raw() as f64 / 1e18;

        assert!((actual - expected).abs() < 0.001, "Expected {}, got {}", expected, actual);
    }

    #[test]
    fn test_calculate_slope_custom_fee_multiplier() {
        let bids = vec![
            Level { price: 100.0, quantity: 5.0 },
            Level { price: 99.0, quantity: 3.0 },
            Level { price: 98.0, quantity: 2.0 },
        ];

        let asks = vec![
            Level { price: 101.0, quantity: 4.0 },
            Level { price: 102.0, quantity: 3.0 },
            Level { price: 103.0, quantity: 1.0 },
        ];

        let config = OrderBookConfig {
            depth_levels: 5,
            fee_multiplier: 1.05,
            default_liquidity: 1.0,
        };
        let processor = create_test_processor(config);
        let liquidity = processor.calculate_liquidity("TEST", &bids, &asks);
        let slope = processor.calculate_slope(&bids, &asks, liquidity).unwrap();

        // Spread = 103 - 98 = 5
        // Liquidity = 18
        // Base slope = 5 / 18 = 0.277...
        // With fee_multiplier 1.05: 0.277... * 1.05 = 0.291...
        let base_slope = 5.0 / 18.0;
        let expected = base_slope * 1.05;
        let actual = slope.to_u128_raw() as f64 / 1e18;

        assert!((actual - expected).abs() < 0.001, "Expected {}, got {}", expected, actual);
    }

    #[test]
    fn test_zero_liquidity_uses_default() {
        // Test AC #5: Zero liquidity returns configurable default
        let bids: Vec<Level> = vec![
            Level { price: 100.0, quantity: 0.0 },
        ];
        let asks: Vec<Level> = vec![
            Level { price: 101.0, quantity: 0.0 },
        ];

        let config = OrderBookConfig {
            depth_levels: 5,
            fee_multiplier: 1.01,
            default_liquidity: 42.0, // Custom default
        };
        let processor = create_test_processor(config);
        let liquidity = processor.calculate_liquidity("BTCUSDC", &bids, &asks);

        // Should return default_liquidity (42.0)
        let expected = 42.0;
        let actual = liquidity.to_u128_raw() as f64 / 1e18;

        assert!((actual - expected).abs() < 0.001, "Expected default {}, got {}", expected, actual);
    }

    #[test]
    fn test_zero_liquidity_uses_previous_value() {
        // Test AC #5: Zero liquidity returns previous cached value if available
        let config = OrderBookConfig {
            depth_levels: 5,
            fee_multiplier: 1.01,
            default_liquidity: 1.0,
        };
        let processor = create_test_processor(config);

        // First, cache a previous metrics value
        let prev_metrics = AssetMetrics {
            symbol: "ETHUSDC".to_string(),
            asset_id: 123,
            liquidity: Amount::from_u128_raw((99.0 * 1e18) as u128), // Previous liquidity = 99
            price: Amount::from_u128_raw((100.0 * 1e18) as u128),
            slope: Amount::from_u128_raw((0.1 * 1e18) as u128),
            calculated_at: chrono::Utc::now(),
        };
        processor.previous_metrics.write().insert("ETHUSDC".to_string(), prev_metrics);

        // Now calculate with zero liquidity
        let bids: Vec<Level> = vec![
            Level { price: 100.0, quantity: 0.0 },
        ];
        let asks: Vec<Level> = vec![
            Level { price: 101.0, quantity: 0.0 },
        ];

        let liquidity = processor.calculate_liquidity("ETHUSDC", &bids, &asks);

        // Should return previous value (99.0), not default (1.0)
        let expected = 99.0;
        let actual = liquidity.to_u128_raw() as f64 / 1e18;

        assert!((actual - expected).abs() < 0.001, "Expected previous {}, got {}", expected, actual);
    }
}