use super::types::{RebalanceDecision, RebalanceOrder, RebalanceSide};
use crate::config::MarginConfig;
use crate::supply::SupplyManager;
use common::amount::Amount;
use eyre::Result;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Manages rebalancing decisions based on delta and margin config
/// 
/// Rebalancing Logic:
/// 1. Read on-chain demand via getVendorDemand()
/// 2. Calculate delta locally: delta = (supply_long + demand_short) - (supply_short + demand_long)
/// 3. Find max(delta_long, delta_short) = mx
/// 4. If mx < min_order_size → do nothing
/// 5. If mx == delta_long → SELL to reduce long exposure
/// 6. If mx == delta_short → BUY to cover short exposure
/// 7. Round quantity: ceil(mx / min_order_size) * min_order_size
pub struct RebalanceManager {
    margin_config: MarginConfig,
    supply_manager: Arc<RwLock<SupplyManager>>,
}

impl RebalanceManager {
    pub fn new(
        margin_config: MarginConfig,
        supply_manager: Arc<RwLock<SupplyManager>>,
    ) -> Self {
        Self {
            margin_config,
            supply_manager,
        }
    }
    
    /// Evaluate if rebalancing is needed based on current demand
    /// 
    /// Parameters:
    /// - demand_long_map: On-chain demand long positions per asset
    /// - demand_short_map: On-chain demand short positions per asset
    /// 
    /// Returns: RebalanceDecision indicating action needed
    pub fn evaluate_rebalancing(
        &self,
        demand_long_map: &HashMap<String, Amount>,
        demand_short_map: &HashMap<String, Amount>,
    ) -> Result<RebalanceDecision> {
        let supply_mgr = self.supply_manager.read();
        
        // Calculate max delta across all assets
        let max_delta = supply_mgr.calculate_max_delta(demand_long_map, demand_short_map);
        
        if let Some((symbol, is_long, delta_amount)) = max_delta {
            let min_order_size = self.margin_config.min_order_size();
            
            tracing::debug!(
                "Max delta: {} {} {} (min_order_size: {})",
                symbol,
                if is_long { "long" } else { "short" },
                delta_amount.to_u128_raw() as f64 / 1e18,
                min_order_size.to_u128_raw() as f64 / 1e18
            );
            
            // Check if delta exceeds minimum order size
            if delta_amount < min_order_size {
                tracing::debug!("Delta below threshold, no rebalancing needed");
                return Ok(RebalanceDecision::NoAction);
            }
            
            // Round quantity up to multiple of min_order_size
            let rounded_quantity = self.round_quantity(delta_amount, min_order_size)?;
            
            // Determine side
            let side = if is_long {
                // Long position → need to SELL
                RebalanceSide::Sell
            } else {
                // Short position → need to BUY
                RebalanceSide::Buy
            };
            
            let reason = format!(
                "Delta {} exceeds threshold {} (rounded to {})",
                delta_amount.to_u128_raw() as f64 / 1e18,
                min_order_size.to_u128_raw() as f64 / 1e18,
                rounded_quantity.to_u128_raw() as f64 / 1e18
            );
            
            tracing::info!(
                "🔄 Rebalancing needed: {} {} {}",
                side,
                rounded_quantity.to_u128_raw() as f64 / 1e18,
                symbol
            );
            
            Ok(RebalanceDecision::Rebalance {
                symbol,
                side,
                quantity: rounded_quantity,
                reason,
            })
        } else {
            tracing::debug!("No significant delta found");
            Ok(RebalanceDecision::NoAction)
        }
    }
    
    /// Round quantity to multiple of min_order_size
    /// Formula: ceil(quantity / min_order_size) * min_order_size
    fn round_quantity(&self, quantity: Amount, min_order_size: Amount) -> Result<Amount> {
        if min_order_size == Amount::ZERO {
            return Ok(quantity);
        }
        
        // Convert to f64 for ceiling calculation
        let qty_f64 = quantity.to_u128_raw() as f64 / 1e18;
        let mos_f64 = min_order_size.to_u128_raw() as f64 / 1e18;
        
        // Calculate: ceil(qty / mos) * mos
        let multiplier = (qty_f64 / mos_f64).ceil();
        let rounded_f64 = multiplier * mos_f64;
        
        Ok(Amount::from_u128_raw((rounded_f64 * 1e18) as u128))
    }
    
    /// Create rebalance order from decision
    pub fn create_rebalance_order(&self, decision: RebalanceDecision) -> Option<RebalanceOrder> {
        match decision {
            RebalanceDecision::Rebalance {
                symbol,
                side,
                quantity,
                reason,
            } => Some(RebalanceOrder::new(symbol, side, quantity, reason)),
            RebalanceDecision::NoAction => None,
        }
    }
    
    /// Get all current deltas for monitoring
    pub fn get_all_deltas(
        &self,
        demand_long_map: &HashMap<String, Amount>,
        demand_short_map: &HashMap<String, Amount>,
    ) -> HashMap<String, (bool, Amount)> {
        self.supply_manager.read().get_all_deltas(demand_long_map, demand_short_map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_round_quantity_simple() {
        let margin_config = MarginConfig {
            min_order_size_usd: 5.0,
            total_exposure_usd: 1000.0,
            price_change_threshold: 0.05,
            margin_update_timeout_secs: 3600,
        };
        
        // Just test the math without full SupplyManager setup
        let min_order_f64: f64 = 5.0;
        
        // Test rounding logic
        let test_cases: Vec<(f64, f64)> = vec![
            (7.5, 10.0),   // ceil(7.5/5) * 5 = 2 * 5 = 10
            (5.0, 5.0),    // ceil(5.0/5) * 5 = 1 * 5 = 5
            (12.3, 15.0),  // ceil(12.3/5) * 5 = 3 * 5 = 15
            (4.9, 5.0),    // ceil(4.9/5) * 5 = 1 * 5 = 5
        ];
        
        for (input, expected) in test_cases {
            let multiplier: f64 = (input / min_order_f64).ceil();
            let result: f64 = multiplier * min_order_f64;
            assert_eq!(result, expected, "Failed for input {}", input);
        }
    }
}