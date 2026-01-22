//! Types for the async order buffer system (Story 3-6)

use chrono::{DateTime, Utc};
use common::amount::Amount;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A single order waiting in the buffer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferedOrder {
    /// Asset ID (on-chain label)
    pub asset_id: u128,
    /// Symbol for Bitget (e.g., "BTCUSDT")
    pub symbol: String,
    /// Buy or Sell
    pub side: BufferOrderSide,
    /// Quantity to buy/sell
    pub quantity: Amount,
    /// When the order was received
    pub received_at: DateTime<Utc>,
    /// Correlation ID for tracing
    pub correlation_id: String,
    /// Original batch ID from Keeper
    pub batch_id: Option<String>,
}

impl BufferedOrder {
    /// Create a new buffered order
    pub fn new(
        asset_id: u128,
        symbol: String,
        side: BufferOrderSide,
        quantity: Amount,
        correlation_id: String,
    ) -> Self {
        Self {
            asset_id,
            symbol,
            side,
            quantity,
            received_at: Utc::now(),
            correlation_id,
            batch_id: None,
        }
    }

    /// Create with batch ID
    pub fn with_batch_id(mut self, batch_id: String) -> Self {
        self.batch_id = Some(batch_id);
        self
    }

    /// Age of this order in milliseconds
    pub fn age_ms(&self) -> i64 {
        Utc::now().signed_duration_since(self.received_at).num_milliseconds()
    }
}

/// Order side
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BufferOrderSide {
    Buy,
    Sell,
}

impl std::fmt::Display for BufferOrderSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BufferOrderSide::Buy => write!(f, "Buy"),
            BufferOrderSide::Sell => write!(f, "Sell"),
        }
    }
}

/// Current state of the buffer (for persistence)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferState {
    /// Orders waiting to be processed
    pub pending_orders: Vec<BufferedOrder>,
    /// Accumulated quantities per asset (for min-size buffering)
    pub buffered_quantities: HashMap<String, Amount>,
    /// Last checkpoint timestamp
    pub last_checkpoint: DateTime<Utc>,
    /// Total orders processed since startup
    pub total_processed: u64,
    /// Total orders currently in buffer
    pub total_buffered: u64,
}

impl BufferState {
    pub fn new() -> Self {
        Self {
            pending_orders: Vec::new(),
            buffered_quantities: HashMap::new(),
            last_checkpoint: Utc::now(),
            total_processed: 0,
            total_buffered: 0,
        }
    }
}

impl Default for BufferState {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration for the buffer system
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferConfig {
    /// Minimum order sizes per asset symbol (from Bitget)
    pub min_order_sizes: HashMap<String, Amount>,
    /// Checkpoint interval in seconds
    pub checkpoint_interval_secs: u64,
    /// Reconciliation interval in seconds
    pub reconcile_interval_secs: u64,
    /// Maximum queue size before rejecting new orders
    pub max_queue_size: usize,
    /// Stale buffer flush timeout in seconds
    /// Orders buffered longer than this will be flushed even if below min size
    pub stale_buffer_flush_secs: u64,
    /// Path for checkpoint file
    pub checkpoint_path: String,
}

impl Default for BufferConfig {
    fn default() -> Self {
        Self {
            min_order_sizes: HashMap::new(),
            checkpoint_interval_secs: 30,
            reconcile_interval_secs: 30,
            max_queue_size: 1000,
            stale_buffer_flush_secs: 300, // 5 minutes
            checkpoint_path: "/var/vendor/buffer.json".to_string(),
        }
    }
}

impl BufferConfig {
    /// Create config from environment variables
    pub fn from_env() -> Self {
        let checkpoint_path = std::env::var("BUFFER_CHECKPOINT_PATH")
            .unwrap_or_else(|_| "/var/vendor/buffer.json".to_string());
        let checkpoint_interval_secs = std::env::var("BUFFER_CHECKPOINT_INTERVAL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30);
        let reconcile_interval_secs = std::env::var("BUFFER_RECONCILE_INTERVAL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30);
        let max_queue_size = std::env::var("BUFFER_MAX_QUEUE_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1000);
        let stale_buffer_flush_secs = std::env::var("BUFFER_STALE_FLUSH_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(300);

        Self {
            min_order_sizes: HashMap::new(), // Will be loaded from Bitget
            checkpoint_interval_secs,
            reconcile_interval_secs,
            max_queue_size,
            stale_buffer_flush_secs,
            checkpoint_path,
        }
    }
}

/// Statistics about the buffer
#[derive(Debug, Clone, Serialize)]
pub struct BufferStats {
    /// Number of orders in queue
    pub queue_depth: usize,
    /// Number of assets being buffered for min-size
    pub buffered_assets: usize,
    /// Oldest order age in milliseconds
    pub oldest_order_age_ms: i64,
    /// Total orders processed
    pub total_processed: u64,
    /// Last checkpoint timestamp
    pub last_checkpoint: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffered_order_creation() {
        let order = BufferedOrder::new(
            1001,
            "BTCUSDT".to_string(),
            BufferOrderSide::Buy,
            Amount::from_u128_raw(1_000_000_000_000_000_000),
            "corr-123".to_string(),
        );

        assert_eq!(order.asset_id, 1001);
        assert_eq!(order.symbol, "BTCUSDT");
        assert!(matches!(order.side, BufferOrderSide::Buy));
        assert!(order.age_ms() >= 0);
    }

    #[test]
    fn test_buffered_order_with_batch() {
        let order = BufferedOrder::new(
            1001,
            "BTCUSDT".to_string(),
            BufferOrderSide::Sell,
            Amount::from_u128_raw(1_000_000_000_000_000_000),
            "corr-123".to_string(),
        )
        .with_batch_id("batch-456".to_string());

        assert_eq!(order.batch_id, Some("batch-456".to_string()));
    }

    #[test]
    fn test_buffer_state_default() {
        let state = BufferState::default();
        assert!(state.pending_orders.is_empty());
        assert!(state.buffered_quantities.is_empty());
        assert_eq!(state.total_processed, 0);
    }

    #[test]
    fn test_buffer_config_default() {
        let config = BufferConfig::default();
        assert_eq!(config.checkpoint_interval_secs, 30);
        assert_eq!(config.reconcile_interval_secs, 30);
        assert_eq!(config.max_queue_size, 1000);
    }
}
