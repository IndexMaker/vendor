use common::amount::Amount;
use std::collections::HashMap;

/// Delta represents the difference between Demand and Supply
#[derive(Debug, Clone)]
pub struct Delta {
    pub long_positions: HashMap<u128, Amount>,   // Need to BUY (Demand > Supply)
    pub short_positions: HashMap<u128, Amount>,  // Need to SELL (Supply > Demand)
}

impl Delta {
    pub fn new() -> Self {
        Self {
            long_positions: HashMap::new(),
            short_positions: HashMap::new(),
        }
    }

    /// Get all unique asset IDs from both long and short positions
    pub fn all_assets(&self) -> Vec<u128> {
        let mut assets: Vec<u128> = self
            .long_positions
            .keys()
            .chain(self.short_positions.keys())
            .copied()
            .collect();
        
        assets.sort_unstable();
        assets.dedup();
        assets
    }

    /// Check if delta is empty (no rebalancing needed)
    pub fn is_empty(&self) -> bool {
        self.long_positions.is_empty() && self.short_positions.is_empty()
    }
}

/// Margin calculation result per asset
#[derive(Debug, Clone)]
pub struct AssetMargin {
    pub asset_id: u128,
    pub max_quantity: Amount,  // Maximum quantity we can trade
    pub exposure_usd: Amount,  // USD exposure allocated to this asset
}

/// Order decision after delta and margin analysis
#[derive(Debug, Clone)]
pub struct RebalanceOrder {
    pub asset_id: u128,
    pub symbol: String,
    pub side: crate::order_sender::OrderSide,
    pub quantity: Amount,  // In asset units (e.g., BTC)
    pub quantity_usd: Amount,  // In USD
}

/// Rebalancer configuration
#[derive(Debug, Clone)]
pub struct RebalancerConfig {
    pub vendor_id: u128,
    pub min_order_size_usd: f64,    // e.g., 5.0 USD
    pub total_exposure_usd: f64,    // e.g., 1000.0 USD
    pub rebalance_interval_secs: u64,  // e.g., 60 seconds
    pub enable_onchain_submit: bool,  // Submit updated supply to Castle

    // On-chain first strategy configuration
    /// Enable "on-chain first" mode: submit to blockchain immediately, exchange later
    pub onchain_first_enabled: bool,
    /// Enable inventory simulation: accept orders even when can't cover exchange min_order_size
    pub inventory_simulation_enabled: bool,
    /// USDC balance threshold below which we switch to simulation mode
    pub usdc_balance_threshold: f64,
}

impl Default for RebalancerConfig {
    fn default() -> Self {
        Self {
            vendor_id: 1,
            min_order_size_usd: 5.0,
            total_exposure_usd: 1000.0,
            rebalance_interval_secs: 60,
            enable_onchain_submit: false,
            onchain_first_enabled: true,       // NEW: On-chain first is default
            inventory_simulation_enabled: true, // NEW: Simulation mode enabled by default
            usdc_balance_threshold: 10.0,      // NEW: Minimum USDC to require
        }
    }
}

/// Result of a rebalance decision with execution mode
#[derive(Debug, Clone)]
pub struct RebalanceDecision {
    /// The order to execute
    pub order: RebalanceOrder,
    /// How to execute this order
    pub execution_mode: ExecutionMode,
    /// Reason for the execution mode choice
    pub mode_reason: String,
}

/// How an order should be executed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Normal flow: execute on exchange, then submit supply to on-chain
    Normal,
    /// On-chain first: submit supply immediately, defer exchange execution
    OnchainFirst,
    /// Simulation: accept without exchange execution, track simulated position
    Simulated,
}