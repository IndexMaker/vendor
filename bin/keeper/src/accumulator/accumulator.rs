use super::processable::ProcessableBatch;
use super::types::{FlushTrigger, IndexOrder, OrderBatch};
use parking_lot::RwLock;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;

/// Configuration for the accumulator
#[derive(Debug, Clone)]
#[allow(dead_code)] // max_batch_size reserved for future use
pub struct AccumulatorConfig {
    pub batch_window_ms: u64,
    pub max_batch_size: usize,
}

impl Default for AccumulatorConfig {
    fn default() -> Self {
        Self {
            batch_window_ms: 100, // NFR14: Keeper aggregates orders within 100ms window
            max_batch_size: 100,
        }
    }
}

/// Order accumulator with automatic batching
pub struct OrderAccumulator {
    config: AccumulatorConfig,
    /// Current batch - pub(crate) for integration testing
    pub(crate) current_batch: Arc<RwLock<OrderBatch>>,
    order_tx: mpsc::UnboundedSender<IndexOrder>,
    order_rx: Arc<RwLock<Option<mpsc::UnboundedReceiver<IndexOrder>>>>,
    /// Current block number for block-based flush detection
    current_block: Arc<AtomicU64>,
}

impl OrderAccumulator {
    pub fn new(config: AccumulatorConfig) -> Self {
        let (order_tx, order_rx) = mpsc::unbounded_channel();

        Self {
            current_batch: Arc::new(RwLock::new(OrderBatch::new(config.batch_window_ms))),
            config,
            order_tx,
            order_rx: Arc::new(RwLock::new(Some(order_rx))),
            current_block: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Update the current block number (call from block subscription)
    #[allow(dead_code)] // Used in tests and future block-based flush
    pub fn update_block(&self, block_number: u64) {
        let old_block = self.current_block.swap(block_number, Ordering::SeqCst);

        // Update the current batch's block tracking
        {
            let mut batch = self.current_batch.write();
            batch.update_block(block_number);
        }

        if block_number > old_block && old_block > 0 {
            tracing::debug!(
                "📊 Block updated: {} → {} (batch may flush)",
                old_block, block_number
            );
        }
    }

    /// Get current tracked block number
    #[allow(dead_code)] // Used in tests
    pub fn get_current_block(&self) -> u64 {
        self.current_block.load(Ordering::SeqCst)
    }

    /// Submit an order to the accumulator
    pub fn submit_order(&self, order: IndexOrder) -> eyre::Result<()> {
        tracing::debug!(
            "Submitting order for index {} (action: {:?})",
            order.index_id,
            match &order.action {
                super::types::OrderAction::Deposit { amount_usd, .. } =>
                    format!("Deposit ${:.2}", amount_usd.to_u128_raw() as f64 / 1e18),
                super::types::OrderAction::Withdraw { amount_itp, .. } =>
                    format!("Withdraw {} ITP", amount_itp.to_u128_raw() as f64 / 1e18),
            }
        );

        self.order_tx.send(order)?;
        Ok(())
    }

    /// Get current batch state (for monitoring)
    #[allow(dead_code)] // Used in tests
    pub fn get_batch_state(&self) -> OrderBatch {
        self.current_batch.read().clone()
    }

    /// Check if batch should be flushed
    pub fn should_flush(&self) -> bool {
        let batch = self.current_batch.read();
        batch.should_flush()
    }

    /// Flush current batch and return it with trigger info
    pub fn flush_batch(&self) -> Option<OrderBatch> {
        let current_block = self.current_block.load(Ordering::SeqCst);
        let mut batch_lock = self.current_batch.write();

        if batch_lock.is_empty() {
            return None;
        }

        // Determine and store the flush trigger
        let trigger = batch_lock.get_flush_trigger().unwrap_or(FlushTrigger::Manual);
        let trigger_str = trigger.as_str();

        let mut old_batch = batch_lock.clone();
        old_batch.flush_trigger = Some(trigger.clone());

        // Create new batch with current block as start block
        *batch_lock = OrderBatch::new_with_block(self.config.batch_window_ms, current_block);

        // Log with trigger info (AC: #5 - logs which trigger caused flush)
        tracing::info!(
            "🚀 Batch flushed: {} orders, {} indices, trigger={}",
            old_batch.total_order_count(),
            old_batch.indices.len(),
            trigger_str
        );

        // Additional detail logging based on trigger type
        match &trigger {
            FlushTrigger::TimeWindow { age_ms } => {
                tracing::debug!("  Time trigger: {}ms (window: {}ms)", age_ms, self.config.batch_window_ms);
            }
            FlushTrigger::BlockChange { start_block, current_block } => {
                tracing::debug!("  Block trigger: {} → {}", start_block, current_block);
            }
            _ => {}
        }

        Some(old_batch)
    }

    /// Flush current batch and convert to ProcessableBatch for downstream processing
    /// (Task 6: Integration with KeeperLoop)
    #[allow(dead_code)] // Reserved for future integration
    pub fn flush_processable_batch(&self) -> Option<ProcessableBatch> {
        self.flush_batch().map(ProcessableBatch::from_order_batch)
    }

    /// Start processing orders (run in background task)
    pub async fn start_processing<F>(self: Arc<Self>, mut on_flush: F)
    where
        F: FnMut(OrderBatch) + Send + 'static,
    {
        let mut order_rx = self.order_rx.write().take().expect("Receiver already taken");

        tracing::info!(
            "Starting order accumulator (window: {}ms)",
            self.config.batch_window_ms
        );

        // Spawn a task to process incoming orders
        let self_clone = self.clone();
        tokio::spawn(async move {
            while let Some(order) = order_rx.recv().await {
                // Add order to batch
                {
                    let mut batch = self_clone.current_batch.write();
                    batch.add_order(order);
                }

                // Check if we should flush
                if self_clone.should_flush() {
                    if let Some(batch) = self_clone.flush_batch() {
                        on_flush(batch);
                    }
                }
            }

            tracing::warn!("Order accumulator stopped (channel closed)");
        });

        // Note: Periodic flush is handled by KeeperLoop.run() which polls accumulator.should_flush()
        // and calls flush_batch() + process_batch_pipeline(). No duplicate timer needed here.
    }

    /// Get statistics about current accumulator state
    pub fn get_stats(&self) -> AccumulatorStats {
        let batch = self.current_batch.read();

        AccumulatorStats {
            active_indices: batch.indices.len(),
            total_orders: batch.total_order_count(),
            oldest_order_age_ms: batch
                .indices
                .values()
                .map(|s| s.age_ms())
                .max()
                .unwrap_or(0),
            should_flush: batch.should_flush(),
            current_block: self.current_block.load(Ordering::SeqCst),
            flush_trigger: batch.get_flush_trigger(),
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields used for monitoring/debugging via Debug output
pub struct AccumulatorStats {
    pub active_indices: usize,
    pub total_orders: usize,
    pub oldest_order_age_ms: i64,
    pub should_flush: bool,
    pub current_block: u64,
    pub flush_trigger: Option<FlushTrigger>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accumulator::types::OrderAction;
    use common::amount::Amount;

    fn make_deposit_order(index_id: u128, amount: u128, user: &str) -> IndexOrder {
        IndexOrder {
            index_id,
            action: OrderAction::Deposit {
                user_address: user.to_string(),
                amount_usd: Amount::from_u128_with_scale(amount, 0),
            },
            timestamp: chrono::Utc::now(),
            vault_address: None,
            trader_address: None,
            tx_hash: None,
            correlation_id: None,
        }
    }

    #[tokio::test]
    async fn test_accumulator_basic() {
        let config = AccumulatorConfig {
            batch_window_ms: 100,
            max_batch_size: 10,
        };

        let accumulator = Arc::new(OrderAccumulator::new(config));

        // Add orders directly to batch (bypass channel for deterministic testing)
        {
            let mut batch = accumulator.current_batch.write();
            batch.add_order(make_deposit_order(1001, 100, "0xUser1"));
            batch.add_order(make_deposit_order(1001, 50, "0xUser2"));
        }

        let stats = accumulator.get_stats();
        assert_eq!(stats.active_indices, 1);
        assert_eq!(stats.total_orders, 2);
    }

    #[tokio::test]
    async fn test_100ms_window_triggers_flush() {
        let config = AccumulatorConfig {
            batch_window_ms: 100,
            max_batch_size: 10,
        };

        let accumulator = Arc::new(OrderAccumulator::new(config));

        // Add order directly to batch (bypass channel for deterministic testing)
        {
            let mut batch = accumulator.current_batch.write();
            batch.add_order(make_deposit_order(1001, 100, "0xUser1"));
        }

        // Should not flush yet (too early)
        assert!(!accumulator.should_flush());

        // Wait for 100ms window to expire
        tokio::time::sleep(tokio::time::Duration::from_millis(110)).await;

        // Now should flush
        assert!(accumulator.should_flush());

        let stats = accumulator.get_stats();
        assert!(matches!(stats.flush_trigger, Some(FlushTrigger::TimeWindow { .. })));
    }

    #[tokio::test]
    async fn test_block_change_triggers_flush() {
        let config = AccumulatorConfig {
            batch_window_ms: 100,
            max_batch_size: 10,
        };

        let accumulator = Arc::new(OrderAccumulator::new(config));

        // Set initial block
        accumulator.update_block(100);

        // Add order directly to batch
        {
            let mut batch = accumulator.current_batch.write();
            // Set the batch start block
            batch.start_block = 100;
            batch.current_block = 100;
            batch.add_order(make_deposit_order(1001, 100, "0xUser1"));
        }

        // Should not flush yet (same block)
        assert!(!accumulator.should_flush());

        // Simulate new block
        accumulator.update_block(101);

        // Now should flush due to block change (regardless of time)
        assert!(accumulator.should_flush());

        let stats = accumulator.get_stats();
        assert!(matches!(stats.flush_trigger, Some(FlushTrigger::BlockChange { .. })));

        if let Some(FlushTrigger::BlockChange { start_block, current_block }) = stats.flush_trigger {
            assert_eq!(start_block, 100);
            assert_eq!(current_block, 101);
        }
    }

    #[tokio::test]
    async fn test_flush_batch_returns_trigger_info() {
        let config = AccumulatorConfig {
            batch_window_ms: 100,
            max_batch_size: 10,
        };

        let accumulator = Arc::new(OrderAccumulator::new(config));

        // Set initial block
        accumulator.update_block(100);

        // Add order directly to batch
        {
            let mut batch = accumulator.current_batch.write();
            batch.start_block = 100;
            batch.current_block = 100;
            batch.add_order(make_deposit_order(1001, 100, "0xUser1"));
        }

        // Trigger block-based flush
        accumulator.update_block(101);

        // Flush and verify trigger info
        let flushed = accumulator.flush_batch().expect("Should have batch to flush");
        assert!(matches!(flushed.flush_trigger, Some(FlushTrigger::BlockChange { .. })));
    }

    #[test]
    fn test_flush_trigger_as_str() {
        assert_eq!(FlushTrigger::TimeWindow { age_ms: 100 }.as_str(), "time-based");
        assert_eq!(FlushTrigger::BlockChange { start_block: 1, current_block: 2 }.as_str(), "block-based");
        assert_eq!(FlushTrigger::MaxBatchSize { count: 100 }.as_str(), "max-size");
        assert_eq!(FlushTrigger::Manual.as_str(), "manual");
    }

    #[tokio::test]
    async fn test_multi_vault_concurrent_orders() {
        // Tests AC: Multiple concurrent vault support (Task 4)
        let config = AccumulatorConfig {
            batch_window_ms: 100,
            max_batch_size: 10,
        };

        let accumulator = Arc::new(OrderAccumulator::new(config));

        // Add orders to multiple vaults concurrently
        {
            let mut batch = accumulator.current_batch.write();
            batch.add_order(make_deposit_order(1001, 100, "0xUser1"));
            batch.add_order(make_deposit_order(1002, 200, "0xUser2"));
            batch.add_order(make_deposit_order(1003, 300, "0xUser3"));
            batch.add_order(make_deposit_order(1001, 50, "0xUser4")); // Same vault
        }

        let stats = accumulator.get_stats();
        assert_eq!(stats.active_indices, 3, "Should track 3 separate vaults");
        assert_eq!(stats.total_orders, 4, "Should have 4 total orders");
    }

    #[tokio::test]
    async fn test_multi_vault_aggregates_correctly() {
        // Tests that orders for same vault are aggregated
        let config = AccumulatorConfig::default();
        let accumulator = Arc::new(OrderAccumulator::new(config));

        {
            let mut batch = accumulator.current_batch.write();
            batch.add_order(make_deposit_order(1001, 100, "0xUser1"));
            batch.add_order(make_deposit_order(1001, 50, "0xUser2"));
            batch.add_order(make_deposit_order(1002, 75, "0xUser3"));
        }

        let batch = accumulator.get_batch_state();

        // Check vault 1001 aggregation
        let state_1001 = batch.indices.get(&1001).expect("Index 1001 should exist");
        assert_eq!(state_1001.order_count, 2);
        assert_eq!(state_1001.total_deposits, Amount::from_u128_with_scale(150, 0));

        // Check vault 1002
        let state_1002 = batch.indices.get(&1002).expect("Index 1002 should exist");
        assert_eq!(state_1002.order_count, 1);
        assert_eq!(state_1002.total_deposits, Amount::from_u128_with_scale(75, 0));
    }
}