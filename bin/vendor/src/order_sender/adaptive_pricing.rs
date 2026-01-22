//! Adaptive Pricing Strategy - Dynamic pricing based on orderbook depth
//!
//! This module provides intelligent order pricing that adapts to market liquidity
//! by analyzing orderbook depth instead of using static spreads.
//!
//! Story 3-8: Adaptive Pricing with Orderbook Depth Analysis

use crate::market_data::bitget::rest_client::{OrderBookLevel, OrderBookSnapshot};
use crate::order_sender::types::OrderSide;
use common::amount::Amount;
use eyre::{eyre, Result};

/// Default configuration constants for adaptive pricing
pub const DEFAULT_BASE_SPREAD_BPS: u32 = 5;
pub const DEFAULT_SLIPPAGE_TOLERANCE_BPS: u32 = 50;
pub const DEFAULT_DEPTH_LEVELS: usize = 5;
pub const DEFAULT_SIZE_IMPACT_FACTOR: f64 = 1.5;

/// Adaptive pricing strategy that computes limit prices based on orderbook depth.
///
/// Unlike the static `PricingStrategy`, this strategy:
/// - Walks orderbook levels to find volume-weighted average price (VWAP)
/// - Adds slippage buffer based on order size relative to available liquidity
/// - Adapts to thin/thick markets automatically
///
/// # Example
/// ```ignore
/// let strategy = AdaptivePricingStrategy::default();
/// let limit_price = strategy.compute_limit_price(&snapshot, OrderSide::Buy, quantity)?;
/// ```
#[derive(Debug, Clone)]
pub struct AdaptivePricingStrategy {
    /// Minimum spread in basis points (default: 5 bps)
    /// Applied on top of VWAP calculation
    base_spread_bps: u32,

    /// Maximum slippage tolerance in basis points (default: 50 bps)
    /// Caps the maximum price deviation from mid price
    slippage_tolerance_bps: u32,

    /// Number of orderbook levels to analyze (default: 5)
    depth_levels: usize,

    /// Factor for how much order size affects pricing (default: 1.5)
    /// Higher values add more buffer for larger orders
    size_impact_factor: f64,
}

impl Default for AdaptivePricingStrategy {
    fn default() -> Self {
        Self {
            base_spread_bps: DEFAULT_BASE_SPREAD_BPS,
            slippage_tolerance_bps: DEFAULT_SLIPPAGE_TOLERANCE_BPS,
            depth_levels: DEFAULT_DEPTH_LEVELS,
            size_impact_factor: DEFAULT_SIZE_IMPACT_FACTOR,
        }
    }
}

impl AdaptivePricingStrategy {
    /// Create a new adaptive pricing strategy with custom parameters
    pub fn new(
        base_spread_bps: u32,
        slippage_tolerance_bps: u32,
        depth_levels: usize,
        size_impact_factor: f64,
    ) -> Self {
        Self {
            base_spread_bps,
            slippage_tolerance_bps,
            depth_levels,
            size_impact_factor,
        }
    }

    /// Builder pattern: set base spread
    pub fn with_base_spread_bps(mut self, bps: u32) -> Self {
        self.base_spread_bps = bps;
        self
    }

    /// Builder pattern: set slippage tolerance
    pub fn with_slippage_tolerance_bps(mut self, bps: u32) -> Self {
        self.slippage_tolerance_bps = bps;
        self
    }

    /// Builder pattern: set depth levels
    pub fn with_depth_levels(mut self, levels: usize) -> Self {
        self.depth_levels = levels;
        self
    }

    /// Builder pattern: set size impact factor
    pub fn with_size_impact_factor(mut self, factor: f64) -> Self {
        self.size_impact_factor = factor;
        self
    }

    /// Get the base spread in basis points
    pub fn base_spread_bps(&self) -> u32 {
        self.base_spread_bps
    }

    /// Get the slippage tolerance in basis points
    pub fn slippage_tolerance_bps(&self) -> u32 {
        self.slippage_tolerance_bps
    }

    /// Compute limit price based on orderbook depth.
    ///
    /// For BUY orders: Walk up ask levels until quantity filled, compute VWAP + buffer
    /// For SELL orders: Walk down bid levels until quantity filled, compute VWAP - buffer
    ///
    /// # Arguments
    /// * `snapshot` - Current orderbook snapshot
    /// * `side` - Order side (Buy or Sell)
    /// * `quantity` - Order quantity to fill
    ///
    /// # Returns
    /// Computed limit price with slippage buffer
    pub fn compute_limit_price(
        &self,
        snapshot: &OrderBookSnapshot,
        side: OrderSide,
        quantity: Amount,
    ) -> Result<Amount> {
        match side {
            OrderSide::Buy => self.compute_buy_limit_price(&snapshot.asks, quantity),
            OrderSide::Sell => self.compute_sell_limit_price(&snapshot.bids, quantity),
        }
    }

    /// Compute limit price for BUY orders by walking up ask levels.
    ///
    /// Algorithm:
    /// 1. Walk ask levels from best (lowest) to worst (highest)
    /// 2. Accumulate volume-weighted price until order quantity is filled
    /// 3. If not enough liquidity, use worst ask + buffer for remainder
    /// 4. Add slippage buffer to final VWAP
    fn compute_buy_limit_price(
        &self,
        asks: &[OrderBookLevel],
        order_quantity: Amount,
    ) -> Result<Amount> {
        if asks.is_empty() {
            return Err(eyre!("No asks in orderbook"));
        }

        let order_qty_f64 = order_quantity.to_u128_raw() as f64 / 1e18;
        if order_qty_f64 <= 0.0 {
            return Err(eyre!("Order quantity must be positive"));
        }

        let mut remaining = order_qty_f64;
        let mut vwap_numerator = 0.0f64;
        let mut levels_used = 0;

        // Walk through ask levels (sorted by price ascending - best ask first)
        for level in asks.iter().take(self.depth_levels) {
            let price_f64 = level.price.to_u128_raw() as f64 / 1e18;
            let qty_f64 = level.quantity.to_u128_raw() as f64 / 1e18;

            let fill_qty = remaining.min(qty_f64);
            vwap_numerator += price_f64 * fill_qty;
            remaining -= fill_qty;
            levels_used += 1;

            if remaining <= 0.0 {
                break;
            }
        }

        // If not enough liquidity in visible levels, use worst ask + buffer for remainder
        if remaining > 0.0 {
            let worst_ask = asks
                .iter()
                .take(self.depth_levels)
                .last()
                .map(|l| l.price.to_u128_raw() as f64 / 1e18)
                .unwrap_or(0.0);

            if worst_ask > 0.0 {
                // Add size impact for the unfilled portion
                let size_penalty = 1.0 + (self.size_impact_factor * 0.001); // +0.15% per factor
                vwap_numerator += worst_ask * size_penalty * remaining;
                tracing::debug!(
                    "Insufficient liquidity: {:.6} unfilled, using worst ask ${:.4} + penalty",
                    remaining,
                    worst_ask
                );
            } else {
                return Err(eyre!("Cannot price order: no valid ask prices"));
            }
        }

        // Compute VWAP
        let vwap = vwap_numerator / order_qty_f64;

        // Add slippage buffer
        let buffer_bps = self.compute_buffer_bps(levels_used, remaining > 0.0);
        let limit_price_f64 = vwap * (1.0 + buffer_bps as f64 / 10_000.0);

        tracing::debug!(
            "BUY adaptive price: VWAP=${:.4}, buffer={}bps, limit=${:.4}",
            vwap,
            buffer_bps,
            limit_price_f64
        );

        Ok(Amount::from_u128_raw((limit_price_f64 * 1e18) as u128))
    }

    /// Compute limit price for SELL orders by walking down bid levels.
    ///
    /// Algorithm:
    /// 1. Walk bid levels from best (highest) to worst (lowest)
    /// 2. Accumulate volume-weighted price until order quantity is filled
    /// 3. If not enough liquidity, use worst bid - buffer for remainder
    /// 4. Subtract slippage buffer from final VWAP
    fn compute_sell_limit_price(
        &self,
        bids: &[OrderBookLevel],
        order_quantity: Amount,
    ) -> Result<Amount> {
        if bids.is_empty() {
            return Err(eyre!("No bids in orderbook"));
        }

        let order_qty_f64 = order_quantity.to_u128_raw() as f64 / 1e18;
        if order_qty_f64 <= 0.0 {
            return Err(eyre!("Order quantity must be positive"));
        }

        let mut remaining = order_qty_f64;
        let mut vwap_numerator = 0.0f64;
        let mut levels_used = 0;

        // Walk through bid levels (sorted by price descending - best bid first)
        for level in bids.iter().take(self.depth_levels) {
            let price_f64 = level.price.to_u128_raw() as f64 / 1e18;
            let qty_f64 = level.quantity.to_u128_raw() as f64 / 1e18;

            let fill_qty = remaining.min(qty_f64);
            vwap_numerator += price_f64 * fill_qty;
            remaining -= fill_qty;
            levels_used += 1;

            if remaining <= 0.0 {
                break;
            }
        }

        // If not enough liquidity in visible levels, use worst bid - buffer for remainder
        if remaining > 0.0 {
            let worst_bid = bids
                .iter()
                .take(self.depth_levels)
                .last()
                .map(|l| l.price.to_u128_raw() as f64 / 1e18)
                .unwrap_or(0.0);

            if worst_bid > 0.0 {
                // Subtract size impact for the unfilled portion
                let size_penalty = 1.0 - (self.size_impact_factor * 0.001); // -0.15% per factor
                vwap_numerator += worst_bid * size_penalty * remaining;
                tracing::debug!(
                    "Insufficient liquidity: {:.6} unfilled, using worst bid ${:.4} - penalty",
                    remaining,
                    worst_bid
                );
            } else {
                return Err(eyre!("Cannot price order: no valid bid prices"));
            }
        }

        // Compute VWAP
        let vwap = vwap_numerator / order_qty_f64;

        // Subtract slippage buffer
        let buffer_bps = self.compute_buffer_bps(levels_used, remaining > 0.0);
        let limit_price_f64 = vwap * (1.0 - buffer_bps as f64 / 10_000.0);

        tracing::debug!(
            "SELL adaptive price: VWAP=${:.4}, buffer={}bps, limit=${:.4}",
            vwap,
            buffer_bps,
            limit_price_f64
        );

        Ok(Amount::from_u128_raw((limit_price_f64 * 1e18) as u128))
    }

    /// Compute buffer in basis points based on market conditions.
    ///
    /// The buffer increases when:
    /// - More orderbook levels were needed (thinner market)
    /// - Order couldn't be fully filled from visible liquidity
    fn compute_buffer_bps(&self, levels_used: usize, has_unfilled: bool) -> u32 {
        let mut buffer = self.base_spread_bps;

        // Add buffer for each level beyond the first
        if levels_used > 1 {
            buffer += (levels_used as u32 - 1) * 2; // +2 bps per level
        }

        // Add extra buffer if there's unfilled quantity
        if has_unfilled {
            buffer += 10; // +10 bps for low liquidity situations
        }

        // Cap at max slippage tolerance
        buffer.min(self.slippage_tolerance_bps)
    }

    /// Estimate probability of fill at a given price.
    ///
    /// Returns a value from 0.0 to 1.0 based on how much liquidity
    /// exists at or better than the target price.
    ///
    /// # Arguments
    /// * `snapshot` - Current orderbook snapshot
    /// * `price` - Target limit price
    /// * `side` - Order side
    /// * `quantity` - Order quantity
    ///
    /// # Returns
    /// Estimated fill probability (0.0 = unlikely, 1.0 = very likely)
    pub fn estimate_fill_probability(
        &self,
        snapshot: &OrderBookSnapshot,
        price: Amount,
        side: OrderSide,
        quantity: Amount,
    ) -> f64 {
        let price_f64 = price.to_u128_raw() as f64 / 1e18;
        let qty_f64 = quantity.to_u128_raw() as f64 / 1e18;

        if qty_f64 <= 0.0 || price_f64 <= 0.0 {
            return 0.0;
        }

        let levels = match side {
            OrderSide::Buy => &snapshot.asks,
            OrderSide::Sell => &snapshot.bids,
        };

        // Sum available liquidity at or better than target price
        let mut available_qty = 0.0f64;

        for level in levels.iter().take(self.depth_levels) {
            let level_price = level.price.to_u128_raw() as f64 / 1e18;
            let level_qty = level.quantity.to_u128_raw() as f64 / 1e18;

            let price_acceptable = match side {
                OrderSide::Buy => level_price <= price_f64, // Buy: lower is better
                OrderSide::Sell => level_price >= price_f64, // Sell: higher is better
            };

            if price_acceptable {
                available_qty += level_qty;
            }
        }

        // Calculate fill probability
        let fill_ratio = available_qty / qty_f64;

        // Convert to probability (sigmoid-like curve)
        // fill_ratio >= 1.5 -> ~100% probability
        // fill_ratio == 1.0 -> ~75% probability
        // fill_ratio == 0.5 -> ~40% probability
        // fill_ratio == 0.0 -> ~10% probability (still some chance due to hidden liquidity)
        let probability = if fill_ratio >= 2.0 {
            1.0
        } else if fill_ratio >= 1.0 {
            0.75 + 0.25 * (fill_ratio - 1.0) / 1.0
        } else if fill_ratio > 0.0 {
            0.10 + 0.65 * fill_ratio
        } else {
            0.10 // Base probability for hidden liquidity
        };

        probability.min(1.0).max(0.0)
    }

    /// Calculate price improvement compared to static pricing.
    ///
    /// # Arguments
    /// * `adaptive_price` - Price computed by adaptive strategy
    /// * `static_price` - Price from static spread strategy
    /// * `side` - Order side
    ///
    /// # Returns
    /// Price improvement in basis points (positive = better, negative = worse)
    pub fn calculate_price_improvement(
        &self,
        adaptive_price: Amount,
        static_price: Amount,
        side: OrderSide,
    ) -> i32 {
        let adaptive_f64 = adaptive_price.to_u128_raw() as f64;
        let static_f64 = static_price.to_u128_raw() as f64;

        if static_f64 <= 0.0 {
            return 0;
        }

        // For BUY: lower price is better
        // For SELL: higher price is better
        let improvement_ratio = match side {
            OrderSide::Buy => (static_f64 - adaptive_f64) / static_f64,
            OrderSide::Sell => (adaptive_f64 - static_f64) / static_f64,
        };

        // Convert to basis points
        (improvement_ratio * 10_000.0).round() as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_snapshot(
        bid_prices: &[(f64, f64)],
        ask_prices: &[(f64, f64)],
    ) -> OrderBookSnapshot {
        let bids = bid_prices
            .iter()
            .map(|(price, qty)| OrderBookLevel {
                price: Amount::from_u128_raw((*price * 1e18) as u128),
                quantity: Amount::from_u128_raw((*qty * 1e18) as u128),
            })
            .collect();

        let asks = ask_prices
            .iter()
            .map(|(price, qty)| OrderBookLevel {
                price: Amount::from_u128_raw((*price * 1e18) as u128),
                quantity: Amount::from_u128_raw((*qty * 1e18) as u128),
            })
            .collect();

        OrderBookSnapshot {
            symbol: "BTCUSDC".to_string(),
            bids,
            asks,
            timestamp_ms: 1234567890,
            fetch_latency_ms: 50,
        }
    }

    #[test]
    fn test_default_strategy() {
        let strategy = AdaptivePricingStrategy::default();
        assert_eq!(strategy.base_spread_bps, DEFAULT_BASE_SPREAD_BPS);
        assert_eq!(strategy.slippage_tolerance_bps, DEFAULT_SLIPPAGE_TOLERANCE_BPS);
        assert_eq!(strategy.depth_levels, DEFAULT_DEPTH_LEVELS);
    }

    #[test]
    fn test_builder_pattern() {
        let strategy = AdaptivePricingStrategy::default()
            .with_base_spread_bps(10)
            .with_slippage_tolerance_bps(100)
            .with_depth_levels(10)
            .with_size_impact_factor(2.0);

        assert_eq!(strategy.base_spread_bps, 10);
        assert_eq!(strategy.slippage_tolerance_bps, 100);
        assert_eq!(strategy.depth_levels, 10);
        assert!((strategy.size_impact_factor - 2.0).abs() < 0.001);
    }

    #[test]
    fn test_buy_limit_price_single_level() {
        let strategy = AdaptivePricingStrategy::default();

        // Orderbook with plenty of liquidity at best ask
        let snapshot = create_test_snapshot(
            &[(99.0, 10.0)], // bids
            &[(100.0, 10.0), (101.0, 10.0)], // asks
        );

        let quantity = Amount::from_u128_raw((1.0 * 1e18) as u128); // 1 BTC
        let limit_price = strategy
            .compute_limit_price(&snapshot, OrderSide::Buy, quantity)
            .unwrap();

        // Should be close to 100.0 + base spread (5 bps = 0.05%)
        let price_f64 = limit_price.to_u128_raw() as f64 / 1e18;
        assert!(price_f64 > 100.0, "Price should be above best ask");
        assert!(price_f64 < 100.10, "Price should have small spread: {}", price_f64);
    }

    #[test]
    fn test_buy_limit_price_multiple_levels() {
        let strategy = AdaptivePricingStrategy::default();

        // Orderbook with thin liquidity - needs multiple levels
        let snapshot = create_test_snapshot(
            &[(99.0, 1.0)],
            &[(100.0, 0.5), (101.0, 0.5), (102.0, 1.0)], // Need all 3 for 1.0 BTC
        );

        let quantity = Amount::from_u128_raw((1.0 * 1e18) as u128);
        let limit_price = strategy
            .compute_limit_price(&snapshot, OrderSide::Buy, quantity)
            .unwrap();

        // VWAP should be around (100*0.5 + 101*0.5) / 1.0 = 100.5
        // Plus buffer for multiple levels
        let price_f64 = limit_price.to_u128_raw() as f64 / 1e18;
        assert!(price_f64 > 100.5, "Price should reflect VWAP across levels");
        assert!(price_f64 < 101.5, "Price should be reasonable: {}", price_f64);
    }

    #[test]
    fn test_sell_limit_price_single_level() {
        let strategy = AdaptivePricingStrategy::default();

        let snapshot = create_test_snapshot(
            &[(99.0, 10.0), (98.0, 10.0)], // bids
            &[(100.0, 10.0)], // asks
        );

        let quantity = Amount::from_u128_raw((1.0 * 1e18) as u128);
        let limit_price = strategy
            .compute_limit_price(&snapshot, OrderSide::Sell, quantity)
            .unwrap();

        // Should be close to 99.0 - base spread
        let price_f64 = limit_price.to_u128_raw() as f64 / 1e18;
        assert!(price_f64 < 99.0, "Price should be below best bid");
        assert!(price_f64 > 98.90, "Price should have small spread: {}", price_f64);
    }

    #[test]
    fn test_sell_limit_price_multiple_levels() {
        let strategy = AdaptivePricingStrategy::default();

        // Thin bid liquidity
        let snapshot = create_test_snapshot(
            &[(99.0, 0.3), (98.0, 0.4), (97.0, 0.5)],
            &[(100.0, 1.0)],
        );

        let quantity = Amount::from_u128_raw((1.0 * 1e18) as u128);
        let limit_price = strategy
            .compute_limit_price(&snapshot, OrderSide::Sell, quantity)
            .unwrap();

        // VWAP should be around (99*0.3 + 98*0.4 + 97*0.3) / 1.0 = 98.0
        let price_f64 = limit_price.to_u128_raw() as f64 / 1e18;
        assert!(price_f64 < 98.5, "Price should reflect VWAP: {}", price_f64);
        assert!(price_f64 > 96.0, "Price should be reasonable");
    }

    #[test]
    fn test_estimate_fill_probability_high() {
        let strategy = AdaptivePricingStrategy::default();

        // Plenty of liquidity
        let snapshot = create_test_snapshot(
            &[(99.0, 10.0)],
            &[(100.0, 10.0)],
        );

        let price = Amount::from_u128_raw((101.0 * 1e18) as u128); // Above best ask
        let quantity = Amount::from_u128_raw((1.0 * 1e18) as u128);

        let prob = strategy.estimate_fill_probability(&snapshot, price, OrderSide::Buy, quantity);
        assert!(prob > 0.9, "Should have high fill probability: {}", prob);
    }

    #[test]
    fn test_estimate_fill_probability_low() {
        let strategy = AdaptivePricingStrategy::default();

        // Very little liquidity
        let snapshot = create_test_snapshot(
            &[(99.0, 0.1)],
            &[(100.0, 0.1)],
        );

        let price = Amount::from_u128_raw((100.5 * 1e18) as u128);
        let quantity = Amount::from_u128_raw((1.0 * 1e18) as u128);

        let prob = strategy.estimate_fill_probability(&snapshot, price, OrderSide::Buy, quantity);
        assert!(prob < 0.3, "Should have low fill probability: {}", prob);
    }

    #[test]
    fn test_calculate_price_improvement_buy() {
        let strategy = AdaptivePricingStrategy::default();

        // Adaptive price lower than static = improvement for buy
        let adaptive = Amount::from_u128_raw(100_000_000_000_000_000_000u128); // $100
        let static_price = Amount::from_u128_raw(100_100_000_000_000_000_000u128); // $100.10

        let improvement = strategy.calculate_price_improvement(adaptive, static_price, OrderSide::Buy);
        assert!(improvement > 0, "Should show positive improvement: {}", improvement);
    }

    #[test]
    fn test_calculate_price_improvement_sell() {
        let strategy = AdaptivePricingStrategy::default();

        // Adaptive price higher than static = improvement for sell
        let adaptive = Amount::from_u128_raw(99_000_000_000_000_000_000u128); // $99
        let static_price = Amount::from_u128_raw(98_900_000_000_000_000_000u128); // $98.90

        let improvement = strategy.calculate_price_improvement(adaptive, static_price, OrderSide::Sell);
        assert!(improvement > 0, "Should show positive improvement: {}", improvement);
    }

    #[test]
    fn test_empty_orderbook_error() {
        let strategy = AdaptivePricingStrategy::default();

        let snapshot = create_test_snapshot(&[], &[]);
        let quantity = Amount::from_u128_raw((1.0 * 1e18) as u128);

        let result = strategy.compute_limit_price(&snapshot, OrderSide::Buy, quantity);
        assert!(result.is_err());
    }

    #[test]
    fn test_zero_quantity_error() {
        let strategy = AdaptivePricingStrategy::default();

        let snapshot = create_test_snapshot(&[(99.0, 1.0)], &[(100.0, 1.0)]);
        let quantity = Amount::ZERO;

        let result = strategy.compute_limit_price(&snapshot, OrderSide::Buy, quantity);
        assert!(result.is_err());
    }

    #[test]
    fn test_buffer_caps_at_slippage_tolerance() {
        let strategy = AdaptivePricingStrategy::default()
            .with_slippage_tolerance_bps(20); // Low cap

        // Even with unfilled quantity, buffer should cap
        let buffer = strategy.compute_buffer_bps(5, true); // 5 levels, has unfilled
        assert_eq!(buffer, 20, "Buffer should cap at slippage tolerance");
    }
}
