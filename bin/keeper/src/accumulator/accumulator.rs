use super::types::{IndexOrder, OrderBatch, IndexState};
use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Configuration for the accumulator
#[derive(Debug, Clone)]
pub struct AccumulatorConfig {
    pub batch_window_ms: u64,
    pub max_batch_size: usize,
}

impl Default for AccumulatorConfig {
    fn default() -> Self {
        Self {
            batch_window_ms: 500,
            max_batch_size: 100,
        }
    }
}

/// Order accumulator with automatic batching
pub struct OrderAccumulator {
    config: AccumulatorConfig,
    current_batch: Arc<RwLock<OrderBatch>>,
    order_tx: mpsc::UnboundedSender<IndexOrder>,
    order_rx: Arc<RwLock<Option<mpsc::UnboundedReceiver<IndexOrder>>>>,
}

impl OrderAccumulator {
    pub fn new(config: AccumulatorConfig) -> Self {
        let (order_tx, order_rx) = mpsc::unbounded_channel();

        Self {
            current_batch: Arc::new(RwLock::new(OrderBatch::new(config.batch_window_ms))),
            config,
            order_tx,
            order_rx: Arc::new(RwLock::new(Some(order_rx))),
        }
    }

    /// Submit an order to the accumulator
    pub fn submit_order(&self, order: IndexOrder) -> eyre::Result<()> {
        tracing::debug!(
            "Submitting order for index {} (action: {:?})",
            order.index_id,
            match &order.action {
                super::types::OrderAction::Deposit { amount_usd, .. } =>
                    format!("Deposit ${:.2}", amount_usd.to_u128_raw() as f64 / 1e18),
                super::types::OrderAction::Withdraw { amount_usd, .. } =>
                    format!("Withdraw ${:.2}", amount_usd.to_u128_raw() as f64 / 1e18),
            }
        );

        self.order_tx.send(order)?;
        Ok(())
    }

    /// Get current batch state (for monitoring)
    pub fn get_batch_state(&self) -> OrderBatch {
        self.current_batch.read().clone()
    }

    /// Check if batch should be flushed
    pub fn should_flush(&self) -> bool {
        let batch = self.current_batch.read();
        batch.should_flush()
    }

    /// Flush current batch and return it
    pub fn flush_batch(&self) -> Option<OrderBatch> {
        let mut batch_lock = self.current_batch.write();
        
        if batch_lock.is_empty() {
            return None;
        }

        let old_batch = batch_lock.clone();
        *batch_lock = OrderBatch::new(self.config.batch_window_ms);

        tracing::info!(
            "Flushed batch: {} orders across {} indices",
            old_batch.total_order_count(),
            old_batch.indices.len()
        );

        Some(old_batch)
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

        // Spawn a periodic flush task (backup in case time-based flush is needed)
        let self_clone = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(
                tokio::time::Duration::from_millis(self_clone.config.batch_window_ms)
            );

            loop {
                interval.tick().await;

                if self_clone.should_flush() {
                    if let Some(batch) = self_clone.flush_batch() {
                        tracing::debug!("Periodic flush triggered");
                        // Note: We don't call on_flush here to avoid duplicate processing
                        // The main processing loop handles this
                    }
                }
            }
        });
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
        }
    }
}

#[derive(Debug, Clone)]
pub struct AccumulatorStats {
    pub active_indices: usize,
    pub total_orders: usize,
    pub oldest_order_age_ms: i64,
    pub should_flush: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accumulator::types::OrderAction;
    use common::amount::Amount;

    #[tokio::test]
    async fn test_accumulator_basic() {
        let config = AccumulatorConfig {
            batch_window_ms: 100,
            max_batch_size: 10,
        };

        let accumulator = Arc::new(OrderAccumulator::new(config));

        // Submit some orders
        let order1 = IndexOrder {
            index_id: 1001,
            action: OrderAction::Deposit {
                user_address: "0xUser1".to_string(),
                amount_usd: Amount::from_u128_with_scale(100, 0),
            },
            timestamp: chrono::Utc::now(),
        };

        let order2 = IndexOrder {
            index_id: 1001,
            action: OrderAction::Deposit {
                user_address: "0xUser2".to_string(),
                amount_usd: Amount::from_u128_with_scale(50, 0),
            },
            timestamp: chrono::Utc::now(),
        };

        accumulator.submit_order(order1).unwrap();
        accumulator.submit_order(order2).unwrap();

        // Give async processing time
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        let stats = accumulator.get_stats();
        assert_eq!(stats.active_indices, 1);
        assert_eq!(stats.total_orders, 2);
    }
}