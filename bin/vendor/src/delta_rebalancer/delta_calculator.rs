use super::types::{Delta};
use crate::{onchain::VendorDemand, supply::SupplyState};
use common::amount::Amount;

pub struct DeltaCalculator;

impl DeltaCalculator {
    /// Calculate Delta = Demand - Supply
    /// 
    /// Positive Delta (long) = Need to BUY
    /// Negative Delta (short) = Need to SELL
    pub fn calculate_delta(demand: &VendorDemand, supply: &SupplyState) -> Delta {
        let mut delta = Delta::new();

        // Process each asset in demand
        for (asset_id, demand_qty) in &demand.assets {
            // Get supply quantities for this asset using the new helper methods
            let supply_long = supply.get_long_quantity(*asset_id);
            let supply_short = supply.get_short_quantity(*asset_id);
            
            // Net supply = long - short
            let net_supply = supply_long
                .checked_sub(supply_short)
                .unwrap_or(Amount::ZERO);

            // Delta = Demand - Net Supply
            if *demand_qty > net_supply {
                // Need to BUY (long position)
                let diff = demand_qty.checked_sub(net_supply).unwrap();
                
                tracing::debug!(
                    "  Asset {}: Demand={:.4}, NetSupply={:.4} (L={:.4}, S={:.4}) → LONG {:.4}",
                    asset_id,
                    demand_qty.to_u128_raw() as f64 / 1e18,
                    net_supply.to_u128_raw() as f64 / 1e18,
                    supply_long.to_u128_raw() as f64 / 1e18,
                    supply_short.to_u128_raw() as f64 / 1e18,
                    diff.to_u128_raw() as f64 / 1e18
                );
                
                delta.long_positions.insert(*asset_id, diff);
            } else if *demand_qty < net_supply {
                // Need to SELL (short position)
                let diff = net_supply.checked_sub(*demand_qty).unwrap();
                
                tracing::debug!(
                    "  Asset {}: Demand={:.4}, NetSupply={:.4} (L={:.4}, S={:.4}) → SHORT {:.4}",
                    asset_id,
                    demand_qty.to_u128_raw() as f64 / 1e18,
                    net_supply.to_u128_raw() as f64 / 1e18,
                    supply_long.to_u128_raw() as f64 / 1e18,
                    supply_short.to_u128_raw() as f64 / 1e18,
                    diff.to_u128_raw() as f64 / 1e18
                );
                
                delta.short_positions.insert(*asset_id, diff);
            } else {
                // Perfectly balanced
                tracing::trace!(
                    "  Asset {}: Perfectly balanced (L={:.4}, S={:.4})",
                    asset_id,
                    supply_long.to_u128_raw() as f64 / 1e18,
                    supply_short.to_u128_raw() as f64 / 1e18
                );
            }
        }

        delta
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_delta_calculation_long() {
        let mut demand = VendorDemand {
            assets: HashMap::new(),
        };
        demand.assets.insert(101, Amount::from_u128_with_scale(100, 0)); // 100 units

        let mut supply = SupplyState::new();
        supply.init_asset("BTCUSDT".to_string(), 101);
        supply.record_buy("BTCUSDT", Amount::from_u128_with_scale(50, 0)).unwrap(); // 50 units long

        let delta = DeltaCalculator::calculate_delta(&demand, &supply);

        // Should have long position of 50 (need to buy 50 more)
        assert_eq!(delta.long_positions.len(), 1);
        assert_eq!(
            delta.long_positions.get(&101).unwrap().to_u128_raw(),
            Amount::from_u128_with_scale(50, 0).to_u128_raw()
        );
    }

    #[test]
    fn test_delta_calculation_short() {
        let mut demand = VendorDemand {
            assets: HashMap::new(),
        };
        demand.assets.insert(101, Amount::from_u128_with_scale(50, 0)); // 50 units

        let mut supply = SupplyState::new();
        supply.init_asset("BTCUSDT".to_string(), 101);
        supply.record_buy("BTCUSDT", Amount::from_u128_with_scale(100, 0)).unwrap(); // 100 units long

        let delta = DeltaCalculator::calculate_delta(&demand, &supply);

        // Should have short position of 50 (need to sell 50)
        assert_eq!(delta.short_positions.len(), 1);
        assert_eq!(
            delta.short_positions.get(&101).unwrap().to_u128_raw(),
            Amount::from_u128_with_scale(50, 0).to_u128_raw()
        );
    }

    #[test]
    fn test_delta_calculation_with_short_positions() {
        let mut demand = VendorDemand {
            assets: HashMap::new(),
        };
        demand.assets.insert(101, Amount::from_u128_with_scale(100, 0)); // 100 units

        let mut supply = SupplyState::new();
        supply.init_asset("BTCUSDT".to_string(), 101);
        supply.record_buy("BTCUSDT", Amount::from_u128_with_scale(80, 0)).unwrap(); // 80 long
        supply.record_sell("BTCUSDT", Amount::from_u128_with_scale(30, 0)).unwrap(); // 30 short
        // Net = 80 - 30 = 50

        let delta = DeltaCalculator::calculate_delta(&demand, &supply);

        // Should need to buy 50 more (demand 100 - net supply 50)
        assert_eq!(delta.long_positions.len(), 1);
        assert_eq!(
            delta.long_positions.get(&101).unwrap().to_u128_raw(),
            Amount::from_u128_with_scale(50, 0).to_u128_raw()
        );
    }
}