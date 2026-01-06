use super::types::{AssetMetrics, OrderBook, OrderBookConfig, Level};
use crate::onchain::{AssetMapper, PriceTracker};
use common::amount::Amount;
use eyre::{eyre, Result};
use parking_lot::RwLock;
use std::sync::Arc;

/// Processes order book data to calculate market metrics (L, P, S)
/// 
/// Formulas from DeIndex paper (Section 5.1):
/// - Liquidity (L): Σ(QBk) + Σ(QAk) for k=1 to K
/// - Price (P): (PA1 × QB1 + PB1 × QA1) / (QB1 + QA1)  [Micro-Price]
/// - Slope (S): (PAK - PBK) / L
pub struct OrderBookProcessor {
    price_tracker: Arc<PriceTracker>,
    asset_mapper: Arc<RwLock<AssetMapper>>,
    config: OrderBookConfig,
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
        
        // Calculate metrics
        let liquidity = self.calculate_liquidity(bids, asks);
        let price = self.calculate_micro_price(bids, asks)?;
        let slope = self.calculate_slope(bids, asks, liquidity)?;
        
        tracing::trace!(
            "Calculated metrics for {}: L={:.2}, P={:.2}, S={:.6}",
            symbol,
            liquidity.to_u128_raw() as f64 / 1e18,
            price.to_u128_raw() as f64 / 1e18,
            slope.to_u128_raw() as f64 / 1e18
        );
        
        Ok(AssetMetrics {
            symbol: symbol.to_string(),
            asset_id,
            liquidity,
            price,
            slope,
            calculated_at: chrono::Utc::now(),
        })
    }
    
    /// Calculate liquidity: L = Σ(QBk) + Σ(QAk)
    /// 
    /// Sum of all quantities across K levels on both sides
    fn calculate_liquidity(&self, bids: &[Level], asks: &[Level]) -> Amount {
        let bid_qty: f64 = bids.iter().map(|l| l.quantity).sum();
        let ask_qty: f64 = asks.iter().map(|l| l.quantity).sum();
        let total = bid_qty + ask_qty;
        
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
    
    /// Calculate slope: S = (PAK - PBK) / L
    /// 
    /// Price impact measure: spread across K levels divided by total liquidity
    /// Higher slope = less liquidity = more price impact
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
        
        // Slope = spread / liquidity
        let slope = spread / liq_f64;
        
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
}

#[cfg(test)]
mod tests {
    use super::*;
    
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
        
        let config = OrderBookConfig::default();
        let processor = OrderBookProcessor {
            price_tracker: Arc::new(PriceTracker::new()),
            asset_mapper: Arc::new(RwLock::new(AssetMapper::default())),
            config,
        };
        
        let liquidity = processor.calculate_liquidity(&bids, &asks);
        
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
        
        let config = OrderBookConfig::default();
        let processor = OrderBookProcessor {
            price_tracker: Arc::new(PriceTracker::new()),
            asset_mapper: Arc::new(RwLock::new(AssetMapper::default())),
            config,
        };
        
        let price = processor.calculate_micro_price(&bids, &asks).unwrap();
        
        // (101 * 10 + 100 * 5) / (10 + 5) = (1010 + 500) / 15 = 1510 / 15 = 100.666...
        let expected = 100.666666666;
        let actual = price.to_u128_raw() as f64 / 1e18;
        
        assert!((actual - expected).abs() < 0.01);
    }
    
    #[test]
    fn test_calculate_slope() {
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
        
        let config = OrderBookConfig::default();
        let processor = OrderBookProcessor {
            price_tracker: Arc::new(PriceTracker::new()),
            asset_mapper: Arc::new(RwLock::new(AssetMapper::default())),
            config,
        };
        
        let liquidity = processor.calculate_liquidity(&bids, &asks);
        let slope = processor.calculate_slope(&bids, &asks, liquidity).unwrap();
        
        // Spread = 103 - 98 = 5
        // Liquidity = 18
        // Slope = 5 / 18 = 0.277...
        let expected = 5.0 / 18.0;
        let actual = slope.to_u128_raw() as f64 / 1e18;
        
        assert!((actual - expected).abs() < 0.01);
    }
}