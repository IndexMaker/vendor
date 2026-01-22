//! Margin computation for on-chain submissions (Story 3-4)
//!
//! Computes margin vector: M_i = (V_max / n) / P_i
//! Where:
//! - V_max = max volley size (default 10000 USD)
//! - n = number of assets
//! - P_i = price of asset i

use common::amount::Amount;

/// Configuration for margin computation
#[derive(Debug, Clone)]
pub struct MarginConfig {
    /// Maximum volley size in USD (V_max)
    pub max_volley_size: Amount,
}

impl Default for MarginConfig {
    fn default() -> Self {
        // Default: 10,000 USD
        Self {
            max_volley_size: Amount::from_u128_raw(10_000 * 1_000_000_000_000_000_000u128),
        }
    }
}

/// Compute margin vector for assets
///
/// Formula: M_i = (V_max / n) / P_i
///
/// # Arguments
/// * `prices` - Price vector (P) for each asset (in 18-decimal fixed point)
/// * `config` - Margin computation config
///
/// # Returns
/// * Margin vector with same length as prices
pub fn compute_margin_vector(prices: &[Amount], config: &MarginConfig) -> Vec<Amount> {
    if prices.is_empty() {
        return vec![];
    }

    let n = prices.len();
    let v_max = config.max_volley_size.to_u128_raw() as f64 / 1e18;
    let per_asset_volley = v_max / n as f64;

    prices
        .iter()
        .map(|price| {
            let p = price.to_u128_raw() as f64 / 1e18;
            if p == 0.0 {
                // Avoid division by zero - use minimum margin
                Amount::from_u128_raw(1_000_000_000_000_000_000u128) // 1.0
            } else {
                let margin = per_asset_volley / p;
                Amount::from_u128_raw((margin * 1e18) as u128)
            }
        })
        .collect()
}

/// Compute supply placeholder vectors (for Story 3-6 integration)
///
/// Returns zero vectors for now - actual Poisson PDE solver
/// will be implemented in Story 3-6.
///
/// # Arguments
/// * `n` - Number of assets
///
/// # Returns
/// * Tuple of (supply_long, supply_short) vectors (both zeros)
pub fn compute_supply_placeholder(n: usize) -> (Vec<Amount>, Vec<Amount>) {
    // TODO: Story 3-6 will implement actual supply computation via Poisson PDE solver
    let zeros = vec![Amount::ZERO; n];
    (zeros.clone(), zeros)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_margin_config_default() {
        let config = MarginConfig::default();
        let v_max = config.max_volley_size.to_u128_raw() as f64 / 1e18;
        assert_eq!(v_max, 10_000.0);
    }

    #[test]
    fn test_compute_margin_single_asset() {
        let config = MarginConfig::default();
        // Price = 100 USD
        let prices = vec![Amount::from_u128_raw((100.0 * 1e18) as u128)];

        let margins = compute_margin_vector(&prices, &config);

        assert_eq!(margins.len(), 1);
        // M = (10000 / 1) / 100 = 100
        let margin = margins[0].to_u128_raw() as f64 / 1e18;
        assert!((margin - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_compute_margin_multiple_assets() {
        let config = MarginConfig::default();
        // 2 assets: price = 100, 200 USD
        let prices = vec![
            Amount::from_u128_raw((100.0 * 1e18) as u128),
            Amount::from_u128_raw((200.0 * 1e18) as u128),
        ];

        let margins = compute_margin_vector(&prices, &config);

        assert_eq!(margins.len(), 2);
        // Per-asset volley = 10000 / 2 = 5000
        // M_0 = 5000 / 100 = 50
        // M_1 = 5000 / 200 = 25
        let m0 = margins[0].to_u128_raw() as f64 / 1e18;
        let m1 = margins[1].to_u128_raw() as f64 / 1e18;

        assert!((m0 - 50.0).abs() < 0.01);
        assert!((m1 - 25.0).abs() < 0.01);
    }

    #[test]
    fn test_compute_margin_empty() {
        let config = MarginConfig::default();
        let margins = compute_margin_vector(&[], &config);
        assert!(margins.is_empty());
    }

    #[test]
    fn test_compute_margin_zero_price() {
        let config = MarginConfig::default();
        let prices = vec![Amount::ZERO];

        let margins = compute_margin_vector(&prices, &config);

        // Should return minimum margin (1.0)
        assert_eq!(margins.len(), 1);
        let margin = margins[0].to_u128_raw() as f64 / 1e18;
        assert!((margin - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_compute_supply_placeholder() {
        let (long, short) = compute_supply_placeholder(5);

        assert_eq!(long.len(), 5);
        assert_eq!(short.len(), 5);

        for i in 0..5 {
            assert_eq!(long[i].to_u128_raw(), 0);
            assert_eq!(short[i].to_u128_raw(), 0);
        }
    }

    #[test]
    fn test_margin_preserves_order() {
        let config = MarginConfig::default();
        let prices = vec![
            Amount::from_u128_raw((50.0 * 1e18) as u128),
            Amount::from_u128_raw((100.0 * 1e18) as u128),
            Amount::from_u128_raw((200.0 * 1e18) as u128),
        ];

        let margins = compute_margin_vector(&prices, &config);

        // Higher price → lower margin
        assert!(margins[0].to_u128_raw() > margins[1].to_u128_raw());
        assert!(margins[1].to_u128_raw() > margins[2].to_u128_raw());
    }
}
