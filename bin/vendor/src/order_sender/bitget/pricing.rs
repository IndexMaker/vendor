use common::amount::Amount;
use eyre::Result;

pub struct PricingStrategy {
    price_limit_bps: u16, // Basis points spread (e.g., 5 = 0.05%)
}

impl PricingStrategy {
    pub fn new(price_limit_bps: u16) -> Self {
        Self { price_limit_bps }
    }

    /// Calculate limit price with spread from best ask/bid
    /// 
    /// For BUY orders: willing to pay slightly MORE than best ask
    /// For SELL orders: willing to accept slightly LESS than best bid
    pub fn calculate_limit_price(
        &self,
        best_price: Amount,
        side: super::super::types::OrderSide,
    ) -> Result<Amount> {
        // Convert basis points to decimal multiplier
        // 5 bps = 0.05% = 0.0005 = 5/10000
        let spread_bps = self.price_limit_bps as f64;
        let spread_decimal = spread_bps / 10000.0;

        let multiplier = match side {
            super::super::types::OrderSide::Buy => {
                // Buy: add spread (pay more)
                1.0 + spread_decimal
            }
            super::super::types::OrderSide::Sell => {
                // Sell: subtract spread (accept less)
                1.0 - spread_decimal
            }
        };

        // Convert to Amount calculation
        let multiplier_amount = Amount::from_u128_raw((multiplier * 1e18) as u128);
        
        best_price
            .checked_mul(multiplier_amount)
            .ok_or_else(|| eyre::eyre!("Overflow in price calculation"))?
            .checked_div(Amount::ONE)
            .ok_or_else(|| eyre::eyre!("Division error in price calculation"))
    }

    /// Get the spread in basis points
    pub fn get_spread_bps(&self) -> u16 {
        self.price_limit_bps
    }

    /// Format spread as percentage string
    pub fn spread_as_percentage(&self) -> String {
        format!("{:.3}%", self.price_limit_bps as f64 / 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buy_spread() {
        let strategy = PricingStrategy::new(5); // 5 bps = 0.05%
        
        // Best ask: $100
        let best_ask = Amount::from_u128_raw(100 * 1_000_000_000_000_000_000);
        
        let limit_price = strategy
            .calculate_limit_price(best_ask, super::super::super::types::OrderSide::Buy)
            .unwrap();
        
        // Should be ~$100.05 (100 * 1.0005)
        let expected = Amount::from_u128_raw(100_050_000_000_000_000_000);
        
        // Allow small rounding difference
        let diff = if limit_price > expected {
            limit_price.checked_sub(expected).unwrap()
        } else {
            expected.checked_sub(limit_price).unwrap()
        };
        
        assert!(diff < Amount::from_u128_raw(1_000_000_000_000_000)); // < 0.001 difference
    }

    #[test]
    fn test_sell_spread() {
        let strategy = PricingStrategy::new(10); // 10 bps = 0.10%
        
        // Best bid: $50
        let best_bid = Amount::from_u128_raw(50 * 1_000_000_000_000_000_000);
        
        let limit_price = strategy
            .calculate_limit_price(best_bid, super::super::super::types::OrderSide::Sell)
            .unwrap();
        
        // Should be ~$49.95 (50 * 0.999)
        let expected = Amount::from_u128_raw(49_950_000_000_000_000_000);
        
        let diff = if limit_price > expected {
            limit_price.checked_sub(expected).unwrap()
        } else {
            expected.checked_sub(limit_price).unwrap()
        };
        
        assert!(diff < Amount::from_u128_raw(1_000_000_000_000_000));
    }

    #[test]
    fn test_spread_percentage() {
        let strategy = PricingStrategy::new(5);
        assert_eq!(strategy.spread_as_percentage(), "0.050%");
        
        let strategy = PricingStrategy::new(25);
        assert_eq!(strategy.spread_as_percentage(), "0.250%");
    }
}