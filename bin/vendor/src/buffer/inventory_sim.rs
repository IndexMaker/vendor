//! Inventory Simulation Mode (Story 3-6 Enhancement)
//!
//! Tracks simulated positions when:
//! 1. Order size is below exchange min_order_size
//! 2. Vendor doesn't have enough USDC to cover the order
//!
//! The simulated position is submitted to on-chain AS IF we have inventory,
//! and tracked until exchange execution catches up.

use chrono::{DateTime, Utc};
use common::amount::Amount;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// A simulated position that hasn't been executed on exchange yet
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulatedPosition {
    /// Asset ID
    pub asset_id: u128,
    /// Symbol (e.g., "BTCUSDT")
    pub symbol: String,
    /// Simulated quantity (positive = long, we owe the market)
    pub quantity: Amount,
    /// When the simulation was created
    pub created_at: DateTime<Utc>,
    /// Correlation ID for tracing
    pub correlation_id: String,
    /// Reason for simulation
    pub reason: SimulationReason,
    /// Whether on-chain submission was completed
    pub onchain_submitted: bool,
    /// Price at time of simulation (for cost tracking)
    pub simulated_price: Amount,
}

/// Why we're simulating instead of executing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SimulationReason {
    /// Order size below exchange minimum
    BelowMinOrderSize,
    /// Insufficient USDC balance
    InsufficientBalance,
    /// Both conditions
    BelowMinAndInsufficientBalance,
}

impl std::fmt::Display for SimulationReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SimulationReason::BelowMinOrderSize => write!(f, "below_min_order_size"),
            SimulationReason::InsufficientBalance => write!(f, "insufficient_balance"),
            SimulationReason::BelowMinAndInsufficientBalance => write!(f, "below_min_and_insufficient"),
        }
    }
}

/// Configuration for inventory simulation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventorySimConfig {
    /// Enable inventory simulation mode
    pub enabled: bool,
    /// Maximum total simulated exposure in USD
    pub max_simulated_exposure_usd: f64,
    /// Maximum simulated exposure per asset in USD
    pub max_per_asset_exposure_usd: f64,
    /// How long to wait before flushing stale simulations (seconds)
    pub stale_simulation_secs: u64,
    /// Warn threshold for simulated exposure (percentage of max)
    pub warn_threshold_pct: f64,
}

impl Default for InventorySimConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_simulated_exposure_usd: 10_000.0,  // $10k max simulated
            max_per_asset_exposure_usd: 1_000.0,   // $1k per asset
            stale_simulation_secs: 600,            // 10 minutes
            warn_threshold_pct: 0.8,               // Warn at 80%
        }
    }
}

impl InventorySimConfig {
    /// Create from environment variables
    pub fn from_env() -> Self {
        Self {
            enabled: std::env::var("INVENTORY_SIM_ENABLED")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(true),
            max_simulated_exposure_usd: std::env::var("INVENTORY_SIM_MAX_EXPOSURE_USD")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(10_000.0),
            max_per_asset_exposure_usd: std::env::var("INVENTORY_SIM_MAX_PER_ASSET_USD")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1_000.0),
            stale_simulation_secs: std::env::var("INVENTORY_SIM_STALE_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(600),
            warn_threshold_pct: std::env::var("INVENTORY_SIM_WARN_PCT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.8),
        }
    }
}

/// Tracks simulated inventory positions
pub struct InventorySimulator {
    /// Configuration
    config: InventorySimConfig,
    /// Simulated positions by symbol
    positions: RwLock<HashMap<String, Vec<SimulatedPosition>>>,
    /// Total simulated exposure in USD (Amount for precision)
    total_exposure: RwLock<Amount>,
    /// Per-asset exposure in USD
    per_asset_exposure: RwLock<HashMap<String, Amount>>,
}

impl InventorySimulator {
    /// Create a new inventory simulator
    pub fn new(config: InventorySimConfig) -> Self {
        Self {
            config,
            positions: RwLock::new(HashMap::new()),
            total_exposure: RwLock::new(Amount::ZERO),
            per_asset_exposure: RwLock::new(HashMap::new()),
        }
    }

    /// Check if simulation mode is enabled
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Check if we can accept a simulated position
    /// Returns (can_accept, rejection_reason)
    pub fn can_accept_simulation(
        &self,
        symbol: &str,
        quantity: Amount,
        price: Amount,
    ) -> (bool, Option<String>) {
        if !self.config.enabled {
            return (false, Some("Inventory simulation disabled".to_string()));
        }

        // Calculate USD exposure for this order
        let order_exposure_raw = quantity
            .checked_mul(price)
            .unwrap_or(Amount::MAX);
        let order_exposure_usd = order_exposure_raw.to_u128_raw() as f64 / 1e18;

        // Check total exposure limit
        let current_total = self.total_exposure.read().to_u128_raw() as f64 / 1e18;
        if current_total + order_exposure_usd > self.config.max_simulated_exposure_usd {
            return (
                false,
                Some(format!(
                    "Would exceed max simulated exposure: ${:.2} + ${:.2} > ${:.2}",
                    current_total,
                    order_exposure_usd,
                    self.config.max_simulated_exposure_usd
                )),
            );
        }

        // Check per-asset exposure limit
        let asset_exposure = self
            .per_asset_exposure
            .read()
            .get(symbol)
            .map(|a| a.to_u128_raw() as f64 / 1e18)
            .unwrap_or(0.0);

        if asset_exposure + order_exposure_usd > self.config.max_per_asset_exposure_usd {
            return (
                false,
                Some(format!(
                    "Would exceed max per-asset exposure for {}: ${:.2} + ${:.2} > ${:.2}",
                    symbol,
                    asset_exposure,
                    order_exposure_usd,
                    self.config.max_per_asset_exposure_usd
                )),
            );
        }

        // Warn if approaching limits
        let new_total = current_total + order_exposure_usd;
        let threshold = self.config.max_simulated_exposure_usd * self.config.warn_threshold_pct;
        if new_total > threshold {
            tracing::warn!(
                "Simulated exposure approaching limit: ${:.2} / ${:.2} ({:.1}%)",
                new_total,
                self.config.max_simulated_exposure_usd,
                (new_total / self.config.max_simulated_exposure_usd) * 100.0
            );
        }

        (true, None)
    }

    /// Add a simulated position
    pub fn add_simulation(
        &self,
        asset_id: u128,
        symbol: String,
        quantity: Amount,
        price: Amount,
        reason: SimulationReason,
        correlation_id: String,
    ) -> Result<SimulatedPosition, String> {
        // Validate we can accept
        let (can_accept, rejection) = self.can_accept_simulation(&symbol, quantity, price);
        if !can_accept {
            return Err(rejection.unwrap_or_else(|| "Cannot accept simulation".to_string()));
        }

        // Calculate USD exposure
        let exposure_usd = quantity
            .checked_mul(price)
            .unwrap_or(Amount::ZERO);

        // Create position
        let position = SimulatedPosition {
            asset_id,
            symbol: symbol.clone(),
            quantity,
            created_at: Utc::now(),
            correlation_id,
            reason,
            onchain_submitted: false,
            simulated_price: price,
        };

        // Update tracking
        {
            let mut positions = self.positions.write();
            positions
                .entry(symbol.clone())
                .or_insert_with(Vec::new)
                .push(position.clone());
        }

        // Update exposure tracking
        {
            let mut total = self.total_exposure.write();
            *total = total.checked_add(exposure_usd).unwrap_or(*total);
        }
        {
            let mut per_asset = self.per_asset_exposure.write();
            let current = per_asset.get(&symbol).copied().unwrap_or(Amount::ZERO);
            per_asset.insert(symbol, current.checked_add(exposure_usd).unwrap_or(current));
        }

        tracing::info!(
            "Added simulated position: {} {} @ ${:.2} (reason: {}, corr: {})",
            quantity.to_u128_raw() as f64 / 1e18,
            position.symbol,
            price.to_u128_raw() as f64 / 1e18,
            reason,
            position.correlation_id
        );

        Ok(position)
    }

    /// Mark a position as submitted to on-chain
    pub fn mark_onchain_submitted(&self, symbol: &str, correlation_id: &str) {
        let mut positions = self.positions.write();
        if let Some(asset_positions) = positions.get_mut(symbol) {
            for pos in asset_positions.iter_mut() {
                if pos.correlation_id == correlation_id {
                    pos.onchain_submitted = true;
                    tracing::debug!(
                        "Marked simulated position as on-chain submitted: {} {}",
                        symbol,
                        correlation_id
                    );
                }
            }
        }
    }

    /// Consume simulated position when exchange execution catches up
    /// Returns the consumed quantity
    pub fn consume_simulation(
        &self,
        symbol: &str,
        executed_quantity: Amount,
        executed_price: Amount,
    ) -> Amount {
        let mut consumed = Amount::ZERO;
        let mut positions = self.positions.write();

        if let Some(asset_positions) = positions.get_mut(symbol) {
            let mut remaining = executed_quantity;

            // Consume oldest positions first (FIFO)
            asset_positions.retain(|pos| {
                if remaining == Amount::ZERO {
                    return true; // Keep remaining positions
                }

                if pos.quantity <= remaining {
                    // Fully consume this position
                    consumed = consumed.checked_add(pos.quantity).unwrap_or(consumed);
                    remaining = remaining.checked_sub(pos.quantity).unwrap_or(Amount::ZERO);

                    // Update exposure tracking
                    let exposure_removed = pos.quantity
                        .checked_mul(pos.simulated_price)
                        .unwrap_or(Amount::ZERO);

                    {
                        let mut total = self.total_exposure.write();
                        *total = total.saturating_sub(exposure_removed).unwrap_or(*total);
                    }
                    {
                        let mut per_asset = self.per_asset_exposure.write();
                        if let Some(current) = per_asset.get_mut(symbol) {
                            *current = current.saturating_sub(exposure_removed).unwrap_or(*current);
                        }
                    }

                    tracing::info!(
                        "Consumed simulated position: {} {} (corr: {})",
                        pos.quantity.to_u128_raw() as f64 / 1e18,
                        symbol,
                        pos.correlation_id
                    );

                    false // Remove from list
                } else {
                    // Partially consume this position (shouldn't happen often)
                    // For simplicity, keep the position and don't consume
                    true
                }
            });
        }

        consumed
    }

    /// Get total simulated quantity for an asset
    pub fn get_simulated_quantity(&self, symbol: &str) -> Amount {
        self.positions
            .read()
            .get(symbol)
            .map(|positions| {
                positions
                    .iter()
                    .fold(Amount::ZERO, |acc, p| acc.checked_add(p.quantity).unwrap_or(acc))
            })
            .unwrap_or(Amount::ZERO)
    }

    /// Get total simulated quantity by asset ID
    pub fn get_simulated_quantity_by_id(&self, asset_id: u128) -> Amount {
        self.positions
            .read()
            .values()
            .flat_map(|positions| positions.iter())
            .filter(|p| p.asset_id == asset_id)
            .fold(Amount::ZERO, |acc, p| acc.checked_add(p.quantity).unwrap_or(acc))
    }

    /// Get all pending simulations (not yet executed on exchange)
    pub fn get_pending_simulations(&self) -> Vec<SimulatedPosition> {
        self.positions
            .read()
            .values()
            .flat_map(|positions| positions.iter().cloned())
            .collect()
    }

    /// Get stale simulations that need to be flushed
    pub fn get_stale_simulations(&self) -> Vec<SimulatedPosition> {
        let threshold = chrono::Duration::seconds(self.config.stale_simulation_secs as i64);
        let now = Utc::now();

        self.positions
            .read()
            .values()
            .flat_map(|positions| {
                positions
                    .iter()
                    .filter(|p| now.signed_duration_since(p.created_at) > threshold)
                    .cloned()
            })
            .collect()
    }

    /// Get current exposure statistics
    pub fn get_exposure_stats(&self) -> InventorySimStats {
        let total_exposure = self.total_exposure.read().to_u128_raw() as f64 / 1e18;
        let per_asset: HashMap<String, f64> = self
            .per_asset_exposure
            .read()
            .iter()
            .map(|(k, v)| (k.clone(), v.to_u128_raw() as f64 / 1e18))
            .collect();

        let position_count: usize = self
            .positions
            .read()
            .values()
            .map(|v| v.len())
            .sum();

        InventorySimStats {
            total_exposure_usd: total_exposure,
            max_exposure_usd: self.config.max_simulated_exposure_usd,
            exposure_pct: (total_exposure / self.config.max_simulated_exposure_usd) * 100.0,
            per_asset_exposure_usd: per_asset,
            position_count,
        }
    }

    /// Clear all simulations (for testing/recovery)
    pub fn clear_all(&self) {
        self.positions.write().clear();
        *self.total_exposure.write() = Amount::ZERO;
        self.per_asset_exposure.write().clear();
        tracing::warn!("Cleared all inventory simulations");
    }
}

/// Statistics about inventory simulation
#[derive(Debug, Clone, Serialize)]
pub struct InventorySimStats {
    pub total_exposure_usd: f64,
    pub max_exposure_usd: f64,
    pub exposure_pct: f64,
    pub per_asset_exposure_usd: HashMap<String, f64>,
    pub position_count: usize,
}

impl std::fmt::Display for InventorySimStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SimStats(exposure=${:.2}/{:.2} ({:.1}%), positions={})",
            self.total_exposure_usd,
            self.max_exposure_usd,
            self.exposure_pct,
            self.position_count
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_simulator() -> InventorySimulator {
        InventorySimulator::new(InventorySimConfig {
            enabled: true,
            max_simulated_exposure_usd: 1000.0,
            max_per_asset_exposure_usd: 500.0,
            stale_simulation_secs: 60,
            warn_threshold_pct: 0.8,
        })
    }

    #[test]
    fn test_add_simulation() {
        let sim = make_simulator();

        let quantity = Amount::from_u128_raw((0.01 * 1e18) as u128); // 0.01 BTC
        let price = Amount::from_u128_raw((50000.0 * 1e18) as u128); // $50,000

        let result = sim.add_simulation(
            1001,
            "BTCUSDT".to_string(),
            quantity,
            price,
            SimulationReason::BelowMinOrderSize,
            "corr-123".to_string(),
        );

        assert!(result.is_ok());

        let pos = result.unwrap();
        assert_eq!(pos.symbol, "BTCUSDT");
        assert!(!pos.onchain_submitted);

        // Check exposure updated
        let stats = sim.get_exposure_stats();
        assert!(stats.total_exposure_usd > 0.0);
        assert_eq!(stats.position_count, 1);
    }

    #[test]
    fn test_exposure_limits() {
        let sim = make_simulator();

        // Try to add position that exceeds per-asset limit ($500)
        let quantity = Amount::from_u128_raw((0.02 * 1e18) as u128); // 0.02 BTC
        let price = Amount::from_u128_raw((50000.0 * 1e18) as u128); // $50,000 = $1000 exposure

        let (can_accept, reason) = sim.can_accept_simulation("BTCUSDT", quantity, price);

        assert!(!can_accept);
        assert!(reason.unwrap().contains("per-asset"));
    }

    #[test]
    fn test_consume_simulation() {
        let sim = make_simulator();

        let quantity = Amount::from_u128_raw((0.005 * 1e18) as u128);
        let price = Amount::from_u128_raw((50000.0 * 1e18) as u128);

        // Add simulation
        sim.add_simulation(
            1001,
            "BTCUSDT".to_string(),
            quantity,
            price,
            SimulationReason::BelowMinOrderSize,
            "corr-123".to_string(),
        ).unwrap();

        assert_eq!(sim.get_exposure_stats().position_count, 1);

        // Consume it
        let consumed = sim.consume_simulation("BTCUSDT", quantity, price);

        assert_eq!(consumed, quantity);
        assert_eq!(sim.get_exposure_stats().position_count, 0);
    }

    #[test]
    fn test_disabled_simulation() {
        let config = InventorySimConfig {
            enabled: false,
            ..Default::default()
        };
        let sim = InventorySimulator::new(config);

        let (can_accept, _) = sim.can_accept_simulation(
            "BTCUSDT",
            Amount::ZERO,
            Amount::ZERO,
        );

        assert!(!can_accept);
    }

    #[test]
    fn test_mark_onchain_submitted() {
        let sim = make_simulator();

        let quantity = Amount::from_u128_raw((0.001 * 1e18) as u128);
        let price = Amount::from_u128_raw((50000.0 * 1e18) as u128);

        sim.add_simulation(
            1001,
            "BTCUSDT".to_string(),
            quantity,
            price,
            SimulationReason::BelowMinOrderSize,
            "corr-123".to_string(),
        ).unwrap();

        sim.mark_onchain_submitted("BTCUSDT", "corr-123");

        let pending = sim.get_pending_simulations();
        assert!(pending[0].onchain_submitted);
    }
}
