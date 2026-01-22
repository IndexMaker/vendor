//! Minimum order size handler (Story 3-6, AC #3)
//!
//! Buffers sub-minimum orders until threshold is reached.
//! Min order sizes are loaded from Bitget API on startup.

use common::amount::Amount;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use chrono::{DateTime, Utc};

/// Action to take after processing an order
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderAction {
    /// Execute the given quantity (buffer exceeded minimum)
    Execute(Amount),
    /// Buffer the order and wait for more (below minimum)
    Buffer,
}

/// Position being buffered for a symbol
#[derive(Debug, Clone)]
struct BufferedPosition {
    /// Accumulated quantity
    quantity: Amount,
    /// When buffering started
    started_at: DateTime<Utc>,
    /// Number of orders accumulated
    order_count: u32,
}

impl BufferedPosition {
    fn new() -> Self {
        Self {
            quantity: Amount::ZERO,
            started_at: Utc::now(),
            order_count: 0,
        }
    }

    fn age_secs(&self) -> i64 {
        Utc::now().signed_duration_since(self.started_at).num_seconds()
    }
}

/// Handler for minimum order size enforcement
pub struct MinSizeHandler {
    /// Minimum order sizes per symbol (from Bitget)
    min_order_sizes: Arc<RwLock<HashMap<String, Amount>>>,
    /// Accumulated sub-minimum amounts per symbol
    buffer_positions: RwLock<HashMap<String, BufferedPosition>>,
    /// Default minimum if symbol not in map
    default_min_size: Amount,
    /// Stale buffer age threshold in seconds
    stale_threshold_secs: i64,
}

impl MinSizeHandler {
    /// Create a new handler with default minimum size
    pub fn new(default_min_size: Amount) -> Self {
        Self {
            min_order_sizes: Arc::new(RwLock::new(HashMap::new())),
            buffer_positions: RwLock::new(HashMap::new()),
            default_min_size,
            stale_threshold_secs: 300, // 5 minutes
        }
    }

    /// Create with custom stale threshold
    pub fn with_stale_threshold(mut self, secs: i64) -> Self {
        self.stale_threshold_secs = secs;
        self
    }

    /// Set minimum order sizes (call after loading from Bitget)
    pub fn set_min_order_sizes(&self, sizes: HashMap<String, Amount>) {
        let mut mins = self.min_order_sizes.write();
        *mins = sizes;
        tracing::info!("Loaded min order sizes for {} symbols", mins.len());
    }

    /// Get the minimum order size for a symbol
    pub fn get_min_size(&self, symbol: &str) -> Amount {
        self.min_order_sizes
            .read()
            .get(symbol)
            .copied()
            .unwrap_or(self.default_min_size)
    }

    /// Process an order and determine action
    ///
    /// Returns `OrderAction::Execute(quantity)` if buffer exceeds minimum,
    /// or `OrderAction::Buffer` if we should wait for more orders.
    pub fn process_order(&self, symbol: &str, quantity: Amount) -> OrderAction {
        let min_size = self.get_min_size(symbol);

        let mut positions = self.buffer_positions.write();

        // Get or create buffer position
        let position = positions
            .entry(symbol.to_string())
            .or_insert_with(BufferedPosition::new);

        // Add to buffer
        position.quantity = position.quantity
            .checked_add(quantity)
            .unwrap_or(position.quantity);
        position.order_count += 1;

        tracing::debug!(
            "Buffer {} += {} (total: {}, min: {}, orders: {})",
            symbol,
            quantity.to_u128_raw() as f64 / 1e18,
            position.quantity.to_u128_raw() as f64 / 1e18,
            min_size.to_u128_raw() as f64 / 1e18,
            position.order_count
        );

        // Check if buffer exceeds minimum
        if position.quantity >= min_size {
            let to_execute = position.quantity;

            // Reset buffer
            position.quantity = Amount::ZERO;
            position.order_count = 0;
            position.started_at = Utc::now();

            tracing::info!(
                "Buffer {} reached minimum - executing {} (min: {})",
                symbol,
                to_execute.to_u128_raw() as f64 / 1e18,
                min_size.to_u128_raw() as f64 / 1e18
            );

            OrderAction::Execute(to_execute)
        } else {
            OrderAction::Buffer
        }
    }

    /// Get stale buffers that should be flushed
    ///
    /// Returns symbols with buffered amounts that have been waiting too long.
    pub fn get_stale_buffers(&self) -> Vec<(String, Amount)> {
        let positions = self.buffer_positions.read();
        let mut stale = Vec::new();

        for (symbol, position) in positions.iter() {
            if position.quantity > Amount::ZERO && position.age_secs() >= self.stale_threshold_secs {
                stale.push((symbol.clone(), position.quantity));
            }
        }

        stale
    }

    /// Flush a stale buffer regardless of minimum
    pub fn flush_buffer(&self, symbol: &str) -> Option<Amount> {
        let mut positions = self.buffer_positions.write();

        if let Some(position) = positions.get_mut(symbol) {
            if position.quantity > Amount::ZERO {
                let quantity = position.quantity;
                position.quantity = Amount::ZERO;
                position.order_count = 0;
                position.started_at = Utc::now();

                tracing::info!(
                    "Flushed stale buffer for {} - {} (was waiting {} secs)",
                    symbol,
                    quantity.to_u128_raw() as f64 / 1e18,
                    position.age_secs()
                );

                return Some(quantity);
            }
        }

        None
    }

    /// Get current buffered quantities for persistence
    pub fn get_buffered_quantities(&self) -> HashMap<String, Amount> {
        self.buffer_positions
            .read()
            .iter()
            .filter(|(_, pos)| pos.quantity > Amount::ZERO)
            .map(|(symbol, pos)| (symbol.clone(), pos.quantity))
            .collect()
    }

    /// Restore buffered quantities (after crash recovery)
    pub fn restore_buffered_quantities(&self, quantities: HashMap<String, Amount>) {
        let mut positions = self.buffer_positions.write();

        for (symbol, quantity) in quantities {
            let position = positions
                .entry(symbol.clone())
                .or_insert_with(BufferedPosition::new);
            position.quantity = quantity;
            position.order_count = 1; // Assume at least one order
        }

        tracing::info!(
            "Restored buffered quantities for {} symbols",
            positions.len()
        );
    }

    /// Get number of assets being buffered
    pub fn buffered_asset_count(&self) -> usize {
        self.buffer_positions
            .read()
            .values()
            .filter(|pos| pos.quantity > Amount::ZERO)
            .count()
    }

    /// Get total buffered value (for monitoring)
    pub fn total_buffered(&self) -> Amount {
        self.buffer_positions
            .read()
            .values()
            .fold(Amount::ZERO, |acc, pos| {
                acc.checked_add(pos.quantity).unwrap_or(acc)
            })
    }
}

impl Default for MinSizeHandler {
    fn default() -> Self {
        // Default 0.0001 (typical BTC min size)
        Self::new(Amount::from_u128_raw(100_000_000_000_000)) // 0.0001 * 1e18
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amount(val: f64) -> Amount {
        Amount::from_u128_raw((val * 1e18) as u128)
    }

    #[test]
    fn test_buffer_until_minimum() {
        let handler = MinSizeHandler::default();

        // Set minimum for BTC to 0.001
        let mut mins = HashMap::new();
        mins.insert("BTCUSDT".to_string(), amount(0.001));
        handler.set_min_order_sizes(mins);

        // Add 0.0003 - should buffer
        let action1 = handler.process_order("BTCUSDT", amount(0.0003));
        assert_eq!(action1, OrderAction::Buffer);

        // Add 0.0003 more - still below minimum
        let action2 = handler.process_order("BTCUSDT", amount(0.0003));
        assert_eq!(action2, OrderAction::Buffer);

        // Add 0.0005 - now exceeds minimum
        let action3 = handler.process_order("BTCUSDT", amount(0.0005));
        match action3 {
            OrderAction::Execute(qty) => {
                // Should execute accumulated amount (0.0011)
                let expected = 0.0011 * 1e18;
                let actual = qty.to_u128_raw() as f64;
                assert!((actual - expected).abs() < 1e12); // Allow small precision loss
            }
            OrderAction::Buffer => panic!("Expected Execute"),
        }

        // Buffer should be empty now
        assert_eq!(handler.total_buffered(), Amount::ZERO);
    }

    #[test]
    fn test_default_minimum() {
        let handler = MinSizeHandler::default();

        // No specific min set - uses default
        let min = handler.get_min_size("UNKNOWN");
        assert_eq!(min, handler.default_min_size);
    }

    #[test]
    fn test_flush_stale_buffer() {
        let handler = MinSizeHandler::new(amount(0.001))
            .with_stale_threshold(0); // Immediately stale for testing

        // Add small amount
        let action = handler.process_order("BTCUSDT", amount(0.0001));
        assert_eq!(action, OrderAction::Buffer);

        // Get stale buffers
        let stale = handler.get_stale_buffers();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].0, "BTCUSDT");

        // Flush
        let flushed = handler.flush_buffer("BTCUSDT");
        assert!(flushed.is_some());
        assert_eq!(handler.total_buffered(), Amount::ZERO);
    }

    #[test]
    fn test_restore_quantities() {
        let handler = MinSizeHandler::default();

        let mut quantities = HashMap::new();
        quantities.insert("BTCUSDT".to_string(), amount(0.0005));
        quantities.insert("ETHUSDT".to_string(), amount(0.01));

        handler.restore_buffered_quantities(quantities);

        assert_eq!(handler.buffered_asset_count(), 2);
    }

    #[test]
    fn test_get_buffered_quantities() {
        let handler = MinSizeHandler::new(amount(1.0)); // High minimum

        handler.process_order("BTCUSDT", amount(0.1));
        handler.process_order("ETHUSDT", amount(0.2));

        let quantities = handler.get_buffered_quantities();
        assert_eq!(quantities.len(), 2);
        assert!(quantities.contains_key("BTCUSDT"));
        assert!(quantities.contains_key("ETHUSDT"));
    }

    #[test]
    fn test_multiple_symbols() {
        let handler = MinSizeHandler::default();

        let mut mins = HashMap::new();
        mins.insert("BTCUSDT".to_string(), amount(0.001));
        mins.insert("ETHUSDT".to_string(), amount(0.01));
        handler.set_min_order_sizes(mins);

        // BTC buffer
        handler.process_order("BTCUSDT", amount(0.0005));

        // ETH buffer
        handler.process_order("ETHUSDT", amount(0.005));

        assert_eq!(handler.buffered_asset_count(), 2);

        // ETH execute
        let action = handler.process_order("ETHUSDT", amount(0.006));
        match action {
            OrderAction::Execute(_) => {}
            OrderAction::Buffer => panic!("Expected Execute for ETH"),
        }

        // Only BTC still buffered
        assert_eq!(handler.buffered_asset_count(), 1);
    }
}
