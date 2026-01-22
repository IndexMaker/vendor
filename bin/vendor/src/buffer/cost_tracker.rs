//! Acquisition cost tracker (Story 3-6, AC #4, #5)
//!
//! Tracks spread cost, fees, and slippage from actual fills.
//! Reports acquisition cost on-chain for transparency.

use common::amount::Amount;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Per-asset acquisition cost tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetCost {
    /// Total spread cost paid (bid-ask spread)
    pub spread_cost: Amount,
    /// Total exchange fees paid
    pub fees: Amount,
    /// Total slippage (deviation from mid-price)
    pub slippage: Amount,
    /// Total quantity traded
    pub quantity_traded: Amount,
    /// Number of fills
    pub fill_count: u64,
}

impl Default for AssetCost {
    fn default() -> Self {
        Self {
            spread_cost: Amount::ZERO,
            fees: Amount::ZERO,
            slippage: Amount::ZERO,
            quantity_traded: Amount::ZERO,
            fill_count: 0,
        }
    }
}

impl AssetCost {
    /// Get cost per unit for this asset
    pub fn cost_per_unit(&self) -> Amount {
        if self.quantity_traded == Amount::ZERO {
            return Amount::ZERO;
        }

        let total_cost = self.spread_cost
            .checked_add(self.fees)
            .and_then(|v| v.checked_add(self.slippage))
            .unwrap_or(Amount::ZERO);

        total_cost
            .checked_div(self.quantity_traded)
            .unwrap_or(Amount::ZERO)
    }
}

/// Order fill information for cost calculation
#[derive(Debug, Clone)]
pub struct OrderFill {
    /// Asset ID
    pub asset_id: u128,
    /// Symbol
    pub symbol: String,
    /// Average fill price
    pub avg_fill_price: Amount,
    /// Quantity filled
    pub quantity: Amount,
    /// Fee paid
    pub fee: Amount,
    /// Fee currency
    pub fee_currency: String,
}

/// Order book snapshot for calculating spread and slippage
#[derive(Debug, Clone)]
pub struct OrderBookSnapshot {
    /// Best bid price
    pub best_bid: Amount,
    /// Best ask price
    pub best_ask: Amount,
}

impl OrderBookSnapshot {
    /// Calculate mid-price
    pub fn mid_price(&self) -> Amount {
        self.best_bid
            .checked_add(self.best_ask)
            .and_then(|sum| sum.checked_div(Amount::from_u128_raw(2_000_000_000_000_000_000)))
            .unwrap_or(Amount::ZERO)
    }

    /// Calculate spread (ask - bid)
    pub fn spread(&self) -> Amount {
        self.best_ask
            .checked_sub(self.best_bid)
            .unwrap_or(Amount::ZERO)
    }

    /// Calculate half spread
    pub fn half_spread(&self) -> Amount {
        self.spread()
            .checked_div(Amount::from_u128_raw(2_000_000_000_000_000_000))
            .unwrap_or(Amount::ZERO)
    }
}

/// Acquisition cost tracker
pub struct AcquisitionCostTracker {
    /// Total spread cost paid
    pub total_spread_cost: RwLock<Amount>,
    /// Total fees paid
    pub total_fees: RwLock<Amount>,
    /// Total slippage
    pub total_slippage: RwLock<Amount>,
    /// Order count
    pub order_count: RwLock<u64>,
    /// Per-asset costs
    per_asset_costs: RwLock<HashMap<u128, AssetCost>>,
}

impl AcquisitionCostTracker {
    pub fn new() -> Self {
        Self {
            total_spread_cost: RwLock::new(Amount::ZERO),
            total_fees: RwLock::new(Amount::ZERO),
            total_slippage: RwLock::new(Amount::ZERO),
            order_count: RwLock::new(0),
            per_asset_costs: RwLock::new(HashMap::new()),
        }
    }

    /// Record a fill and calculate costs
    pub fn record_fill(&self, fill: &OrderFill, orderbook: &OrderBookSnapshot) {
        let mid_price = orderbook.mid_price();
        let half_spread = orderbook.half_spread();

        // Calculate slippage (deviation from mid-price)
        // For buys: actual > mid is negative slippage (paid more)
        // For sells: actual < mid is negative slippage (received less)
        let slippage = if fill.avg_fill_price > mid_price {
            fill.avg_fill_price
                .checked_sub(mid_price)
                .and_then(|diff| diff.checked_mul(fill.quantity))
                .unwrap_or(Amount::ZERO)
        } else {
            mid_price
                .checked_sub(fill.avg_fill_price)
                .and_then(|diff| diff.checked_mul(fill.quantity))
                .unwrap_or(Amount::ZERO)
        };

        // Spread cost (half spread per side)
        let spread_cost = half_spread
            .checked_mul(fill.quantity)
            .unwrap_or(Amount::ZERO);

        // Update totals
        {
            let mut total_spread = self.total_spread_cost.write();
            *total_spread = total_spread.checked_add(spread_cost).unwrap_or(*total_spread);
        }
        {
            let mut total_fees = self.total_fees.write();
            *total_fees = total_fees.checked_add(fill.fee).unwrap_or(*total_fees);
        }
        {
            let mut total_slippage = self.total_slippage.write();
            *total_slippage = total_slippage.checked_add(slippage).unwrap_or(*total_slippage);
        }
        {
            let mut count = self.order_count.write();
            *count += 1;
        }

        // Update per-asset costs
        {
            let mut per_asset = self.per_asset_costs.write();
            let asset_cost = per_asset.entry(fill.asset_id).or_default();

            asset_cost.spread_cost = asset_cost.spread_cost
                .checked_add(spread_cost)
                .unwrap_or(asset_cost.spread_cost);
            asset_cost.fees = asset_cost.fees
                .checked_add(fill.fee)
                .unwrap_or(asset_cost.fees);
            asset_cost.slippage = asset_cost.slippage
                .checked_add(slippage)
                .unwrap_or(asset_cost.slippage);
            asset_cost.quantity_traded = asset_cost.quantity_traded
                .checked_add(fill.quantity)
                .unwrap_or(asset_cost.quantity_traded);
            asset_cost.fill_count += 1;
        }

        tracing::debug!(
            "Recorded fill for {} (id={}): price=${:.4}, qty={:.6}, fee=${:.6}, slippage=${:.6}, spread_cost=${:.6}",
            fill.symbol,
            fill.asset_id,
            fill.avg_fill_price.to_u128_raw() as f64 / 1e18,
            fill.quantity.to_u128_raw() as f64 / 1e18,
            fill.fee.to_u128_raw() as f64 / 1e18,
            slippage.to_u128_raw() as f64 / 1e18,
            spread_cost.to_u128_raw() as f64 / 1e18,
        );
    }

    /// Get total acquisition cost per unit
    pub fn get_acquisition_cost_per_unit(&self) -> Amount {
        let count = *self.order_count.read();
        if count == 0 {
            return Amount::ZERO;
        }

        let total_spread = *self.total_spread_cost.read();
        let total_fees = *self.total_fees.read();
        let total_slippage = *self.total_slippage.read();

        let total_cost = total_spread
            .checked_add(total_fees)
            .and_then(|v| v.checked_add(total_slippage))
            .unwrap_or(Amount::ZERO);

        // Divide by order count (scale for Amount division)
        let count_scaled = Amount::from_u128_raw(count as u128 * 1_000_000_000_000_000_000);
        total_cost.checked_div(count_scaled).unwrap_or(Amount::ZERO)
    }

    /// Get per-asset costs
    pub fn get_per_asset_costs(&self) -> HashMap<u128, AssetCost> {
        self.per_asset_costs.read().clone()
    }

    /// Get cost for specific asset
    pub fn get_asset_cost(&self, asset_id: u128) -> Option<AssetCost> {
        self.per_asset_costs.read().get(&asset_id).cloned()
    }

    /// Get total cost summary
    pub fn summary(&self) -> CostSummary {
        CostSummary {
            total_spread_cost: *self.total_spread_cost.read(),
            total_fees: *self.total_fees.read(),
            total_slippage: *self.total_slippage.read(),
            order_count: *self.order_count.read(),
            cost_per_unit: self.get_acquisition_cost_per_unit(),
        }
    }

    /// Reset all counters
    pub fn reset(&self) {
        *self.total_spread_cost.write() = Amount::ZERO;
        *self.total_fees.write() = Amount::ZERO;
        *self.total_slippage.write() = Amount::ZERO;
        *self.order_count.write() = 0;
        self.per_asset_costs.write().clear();

        tracing::info!("Cost tracker reset");
    }
}

impl Default for AcquisitionCostTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary of acquisition costs
#[derive(Debug, Clone, Serialize)]
pub struct CostSummary {
    pub total_spread_cost: Amount,
    pub total_fees: Amount,
    pub total_slippage: Amount,
    pub order_count: u64,
    pub cost_per_unit: Amount,
}

impl std::fmt::Display for CostSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Cost Summary: {} orders, spread=${:.4}, fees=${:.4}, slippage=${:.4}, avg=${:.6}/unit",
            self.order_count,
            self.total_spread_cost.to_u128_raw() as f64 / 1e18,
            self.total_fees.to_u128_raw() as f64 / 1e18,
            self.total_slippage.to_u128_raw() as f64 / 1e18,
            self.cost_per_unit.to_u128_raw() as f64 / 1e18,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amount(val: f64) -> Amount {
        Amount::from_u128_raw((val * 1e18) as u128)
    }

    fn make_fill(asset_id: u128, symbol: &str, price: f64, qty: f64, fee: f64) -> OrderFill {
        OrderFill {
            asset_id,
            symbol: symbol.to_string(),
            avg_fill_price: amount(price),
            quantity: amount(qty),
            fee: amount(fee),
            fee_currency: "USDT".to_string(),
        }
    }

    fn make_orderbook(bid: f64, ask: f64) -> OrderBookSnapshot {
        OrderBookSnapshot {
            best_bid: amount(bid),
            best_ask: amount(ask),
        }
    }

    #[test]
    fn test_orderbook_mid_price() {
        let ob = make_orderbook(100.0, 102.0);
        let mid = ob.mid_price();
        let expected = 101.0 * 1e18;
        let actual = mid.to_u128_raw() as f64;
        // Should be 101.0 (mid of 100 and 102)
        assert!((actual - expected).abs() < 1e12);
    }

    #[test]
    fn test_orderbook_spread() {
        let ob = make_orderbook(100.0, 102.0);
        let spread = ob.spread();
        let expected = 2.0 * 1e18;
        let actual = spread.to_u128_raw() as f64;
        assert!((actual - expected).abs() < 1e12);
    }

    #[test]
    fn test_record_fill() {
        let tracker = AcquisitionCostTracker::new();

        let fill = make_fill(1001, "BTCUSDT", 50100.0, 0.1, 0.5);
        let ob = make_orderbook(50000.0, 50200.0);

        tracker.record_fill(&fill, &ob);

        assert_eq!(*tracker.order_count.read(), 1);
        assert!(*tracker.total_fees.read() > Amount::ZERO);
    }

    #[test]
    fn test_multiple_fills() {
        let tracker = AcquisitionCostTracker::new();

        let fill1 = make_fill(1001, "BTCUSDT", 50100.0, 0.1, 0.5);
        let fill2 = make_fill(1002, "ETHUSDT", 3100.0, 1.0, 0.3);

        let ob1 = make_orderbook(50000.0, 50200.0);
        let ob2 = make_orderbook(3050.0, 3150.0);

        tracker.record_fill(&fill1, &ob1);
        tracker.record_fill(&fill2, &ob2);

        assert_eq!(*tracker.order_count.read(), 2);

        let per_asset = tracker.get_per_asset_costs();
        assert_eq!(per_asset.len(), 2);
        assert!(per_asset.contains_key(&1001));
        assert!(per_asset.contains_key(&1002));
    }

    #[test]
    fn test_cost_per_unit() {
        let tracker = AcquisitionCostTracker::new();

        // No fills - should be zero
        assert_eq!(tracker.get_acquisition_cost_per_unit(), Amount::ZERO);

        // Add some fills
        let fill = make_fill(1001, "BTCUSDT", 50100.0, 0.1, 0.5);
        let ob = make_orderbook(50000.0, 50200.0);
        tracker.record_fill(&fill, &ob);

        // Should be non-zero now
        assert!(tracker.get_acquisition_cost_per_unit() > Amount::ZERO);
    }

    #[test]
    fn test_summary() {
        let tracker = AcquisitionCostTracker::new();

        let fill = make_fill(1001, "BTCUSDT", 50100.0, 0.1, 0.5);
        let ob = make_orderbook(50000.0, 50200.0);
        tracker.record_fill(&fill, &ob);

        let summary = tracker.summary();
        assert_eq!(summary.order_count, 1);
        assert!(summary.total_fees > Amount::ZERO);
    }

    #[test]
    fn test_reset() {
        let tracker = AcquisitionCostTracker::new();

        let fill = make_fill(1001, "BTCUSDT", 50100.0, 0.1, 0.5);
        let ob = make_orderbook(50000.0, 50200.0);
        tracker.record_fill(&fill, &ob);

        assert_eq!(*tracker.order_count.read(), 1);

        tracker.reset();

        assert_eq!(*tracker.order_count.read(), 0);
        assert_eq!(*tracker.total_fees.read(), Amount::ZERO);
        assert!(tracker.get_per_asset_costs().is_empty());
    }

    #[test]
    fn test_asset_cost_per_unit() {
        let mut cost = AssetCost::default();
        cost.spread_cost = amount(1.0);
        cost.fees = amount(0.5);
        cost.slippage = amount(0.3);
        cost.quantity_traded = amount(10.0);
        cost.fill_count = 2;

        let per_unit = cost.cost_per_unit();
        // (1.0 + 0.5 + 0.3) / 10.0 = 0.18
        let expected = 0.18 * 1e18;
        let actual = per_unit.to_u128_raw() as f64;
        assert!((actual - expected).abs() < 1e12);
    }
}
