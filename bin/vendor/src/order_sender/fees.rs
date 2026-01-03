use super::types::{FeeDetail, FeeType};
use common::amount::Amount;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeTracker {
    /// Total fees paid in USD equivalent
    total_fees_usd: Amount,

    /// Fees by currency
    fees_by_currency: HashMap<String, Amount>,

    /// Maker vs Taker fees
    maker_fees: Amount,
    taker_fees: Amount,

    /// Number of trades
    trade_count: u64,
    maker_count: u64,
    taker_count: u64,
}

impl FeeTracker {
    pub fn new() -> Self {
        Self {
            total_fees_usd: Amount::ZERO,
            fees_by_currency: HashMap::new(),
            maker_fees: Amount::ZERO,
            taker_fees: Amount::ZERO,
            trade_count: 0,
            maker_count: 0,
            taker_count: 0,
        }
    }

    /// Record a fee from an order
    pub fn record_fee(&mut self, fee_usd: Amount, fee_detail: Option<&FeeDetail>) {
        self.total_fees_usd = self
            .total_fees_usd
            .checked_add(fee_usd)
            .unwrap_or(self.total_fees_usd);

        self.trade_count += 1;

        if let Some(detail) = fee_detail {
            // Track by currency
            *self.fees_by_currency.entry(detail.currency.clone()).or_insert(Amount::ZERO) = 
                self.fees_by_currency
                    .get(&detail.currency)
                    .unwrap_or(&Amount::ZERO)
                    .checked_add(detail.amount)
                    .unwrap_or(Amount::ZERO);

            // Track maker vs taker
            match detail.fee_type {
                FeeType::Maker => {
                    self.maker_fees = self
                        .maker_fees
                        .checked_add(fee_usd)
                        .unwrap_or(self.maker_fees);
                    self.maker_count += 1;
                }
                FeeType::Taker => {
                    self.taker_fees = self
                        .taker_fees
                        .checked_add(fee_usd)
                        .unwrap_or(self.taker_fees);
                    self.taker_count += 1;
                }
                FeeType::Unknown => {}
            }
        }
    }

    /// Get total fees paid
    pub fn total_fees(&self) -> Amount {
        self.total_fees_usd
    }

    /// Get average fee per trade
    pub fn average_fee_per_trade(&self) -> Amount {
        if self.trade_count == 0 {
            return Amount::ZERO;
        }

        self.total_fees_usd
            .checked_div(Amount::from_u128_raw(self.trade_count as u128 * 1_000_000_000_000_000_000))
            .unwrap_or(Amount::ZERO)
    }

    /// Get maker/taker ratio
    pub fn maker_taker_ratio(&self) -> (f64, f64) {
        if self.trade_count == 0 {
            return (0.0, 0.0);
        }

        let maker_pct = (self.maker_count as f64 / self.trade_count as f64) * 100.0;
        let taker_pct = (self.taker_count as f64 / self.trade_count as f64) * 100.0;

        (maker_pct, taker_pct)
    }

    /// Get fees by currency
    pub fn fees_by_currency(&self) -> &HashMap<String, Amount> {
        &self.fees_by_currency
    }

    /// Get summary report
    pub fn summary(&self) -> String {
        let (maker_pct, taker_pct) = self.maker_taker_ratio();
        let avg_fee = self.average_fee_per_trade();

        format!(
            "Fee Summary: {} trades, ${:.2} total fees, ${:.4} avg/trade, Maker: {:.1}%, Taker: {:.1}%",
            self.trade_count,
            self.total_fees_usd.to_u128_raw() as f64 / 1e18,
            avg_fee.to_u128_raw() as f64 / 1e18,
            maker_pct,
            taker_pct
        )
    }

    /// Reset all counters
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for FeeTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fee_tracking() {
        let mut tracker = FeeTracker::new();

        // Record maker fee
        let maker_fee = FeeDetail::new(
            Amount::from_u128_raw(1_000_000_000_000_000_000), // 1.0
            "USDT".to_string(),
            FeeType::Maker,
        );
        tracker.record_fee(Amount::from_u128_raw(1_000_000_000_000_000_000), Some(&maker_fee));

        // Record taker fee
        let taker_fee = FeeDetail::new(
            Amount::from_u128_raw(2_000_000_000_000_000_000), // 2.0
            "USDT".to_string(),
            FeeType::Taker,
        );
        tracker.record_fee(Amount::from_u128_raw(2_000_000_000_000_000_000), Some(&taker_fee));

        assert_eq!(tracker.trade_count, 2);
        assert_eq!(tracker.maker_count, 1);
        assert_eq!(tracker.taker_count, 1);

        let (maker_pct, taker_pct) = tracker.maker_taker_ratio();
        assert_eq!(maker_pct, 50.0);
        assert_eq!(taker_pct, 50.0);

        assert!(tracker.total_fees() > Amount::ZERO);
    }
}