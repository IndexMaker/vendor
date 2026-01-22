//! Order buffer queue (Story 3-6, AC #2)
//!
//! Thread-safe queue for buffered orders using RwLock<VecDeque>.

use super::types::{BufferedOrder, BufferStats};
use chrono::Utc;
use parking_lot::RwLock;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

/// Thread-safe order buffer queue
pub struct OrderBuffer {
    /// Queue of pending orders
    queue: RwLock<VecDeque<BufferedOrder>>,
    /// Maximum queue size (for backpressure)
    max_size: usize,
    /// Total orders processed counter
    total_processed: AtomicU64,
}

impl OrderBuffer {
    /// Create a new order buffer with maximum size
    pub fn new(max_size: usize) -> Self {
        Self {
            queue: RwLock::new(VecDeque::with_capacity(max_size.min(1000))),
            max_size,
            total_processed: AtomicU64::new(0),
        }
    }

    /// Push an order to the queue
    ///
    /// Returns Ok(()) if pushed, Err if queue is full (backpressure)
    pub fn push(&self, order: BufferedOrder) -> Result<(), BufferFullError> {
        let mut queue = self.queue.write();

        if queue.len() >= self.max_size {
            tracing::warn!(
                "Buffer full ({} orders) - rejecting order for {}",
                queue.len(),
                order.symbol
            );
            return Err(BufferFullError {
                queue_depth: queue.len(),
                max_size: self.max_size,
            });
        }

        tracing::debug!(
            "Buffered order: {} {} {} (queue depth: {})",
            order.side,
            order.quantity,
            order.symbol,
            queue.len() + 1
        );

        queue.push_back(order);
        Ok(())
    }

    /// Push multiple orders to the queue
    ///
    /// Returns number of orders successfully pushed
    pub fn push_batch(&self, orders: Vec<BufferedOrder>) -> usize {
        let total_orders = orders.len();
        let mut queue = self.queue.write();
        let mut pushed = 0;

        for order in orders {
            if queue.len() >= self.max_size {
                tracing::warn!(
                    "Buffer full during batch push - {} of {} orders rejected",
                    total_orders - pushed,
                    total_orders
                );
                break;
            }
            queue.push_back(order);
            pushed += 1;
        }

        pushed
    }

    /// Pop a batch of orders from the front of the queue
    pub fn pop_batch(&self, max: usize) -> Vec<BufferedOrder> {
        let mut queue = self.queue.write();
        let count = max.min(queue.len());
        let batch: Vec<BufferedOrder> = queue.drain(..count).collect();

        if !batch.is_empty() {
            let processed = self.total_processed.fetch_add(batch.len() as u64, Ordering::Relaxed);
            tracing::debug!(
                "Popped {} orders from buffer (remaining: {}, total processed: {})",
                batch.len(),
                queue.len(),
                processed + batch.len() as u64
            );
        }

        batch
    }

    /// Pop a single order from the front
    pub fn pop(&self) -> Option<BufferedOrder> {
        let mut queue = self.queue.write();
        let order = queue.pop_front();

        if order.is_some() {
            self.total_processed.fetch_add(1, Ordering::Relaxed);
        }

        order
    }

    /// Peek at the front order without removing it
    pub fn peek(&self) -> Option<BufferedOrder> {
        self.queue.read().front().cloned()
    }

    /// Get current queue depth
    pub fn len(&self) -> usize {
        self.queue.read().len()
    }

    /// Check if queue is empty
    pub fn is_empty(&self) -> bool {
        self.queue.read().is_empty()
    }

    /// Check if queue is at capacity
    pub fn is_full(&self) -> bool {
        self.queue.read().len() >= self.max_size
    }

    /// Get available capacity
    pub fn available_capacity(&self) -> usize {
        let len = self.queue.read().len();
        self.max_size.saturating_sub(len)
    }

    /// Get total orders processed
    pub fn total_processed(&self) -> u64 {
        self.total_processed.load(Ordering::Relaxed)
    }

    /// Get all orders (for persistence)
    pub fn get_all(&self) -> Vec<BufferedOrder> {
        self.queue.read().iter().cloned().collect()
    }

    /// Clear the queue and return all orders
    pub fn drain_all(&self) -> Vec<BufferedOrder> {
        let mut queue = self.queue.write();
        queue.drain(..).collect()
    }

    /// Restore orders (after crash recovery)
    pub fn restore(&self, orders: Vec<BufferedOrder>) {
        let mut queue = self.queue.write();
        queue.clear();

        for order in orders {
            if queue.len() < self.max_size {
                queue.push_back(order);
            }
        }

        tracing::info!("Restored {} orders to buffer", queue.len());
    }

    /// Get buffer statistics
    pub fn stats(&self, last_checkpoint: chrono::DateTime<Utc>) -> BufferStats {
        let queue = self.queue.read();

        let oldest_age = queue.front()
            .map(|o| o.age_ms())
            .unwrap_or(0);

        BufferStats {
            queue_depth: queue.len(),
            buffered_assets: 0, // Will be set by min_size_handler
            oldest_order_age_ms: oldest_age,
            total_processed: self.total_processed.load(Ordering::Relaxed),
            last_checkpoint,
        }
    }
}

impl Default for OrderBuffer {
    fn default() -> Self {
        Self::new(1000)
    }
}

/// Error when buffer is full
#[derive(Debug, Clone)]
pub struct BufferFullError {
    pub queue_depth: usize,
    pub max_size: usize,
}

impl std::fmt::Display for BufferFullError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Buffer full: {}/{} orders",
            self.queue_depth,
            self.max_size
        )
    }
}

impl std::error::Error for BufferFullError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::types::BufferOrderSide;
    use common::amount::Amount;

    fn make_order(symbol: &str) -> BufferedOrder {
        BufferedOrder::new(
            1001,
            symbol.to_string(),
            BufferOrderSide::Buy,
            Amount::from_u128_raw(1_000_000_000_000_000_000),
            format!("corr-{}", symbol),
        )
    }

    #[test]
    fn test_push_and_pop() {
        let buffer = OrderBuffer::new(100);

        assert!(buffer.is_empty());
        assert_eq!(buffer.len(), 0);

        buffer.push(make_order("BTCUSDT")).unwrap();
        assert_eq!(buffer.len(), 1);
        assert!(!buffer.is_empty());

        let order = buffer.pop().unwrap();
        assert_eq!(order.symbol, "BTCUSDT");
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_pop_batch() {
        let buffer = OrderBuffer::new(100);

        buffer.push(make_order("BTCUSDT")).unwrap();
        buffer.push(make_order("ETHUSDT")).unwrap();
        buffer.push(make_order("SOLUSDT")).unwrap();

        let batch = buffer.pop_batch(2);
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0].symbol, "BTCUSDT");
        assert_eq!(batch[1].symbol, "ETHUSDT");
        assert_eq!(buffer.len(), 1);

        // Pop more than available
        let batch2 = buffer.pop_batch(10);
        assert_eq!(batch2.len(), 1);
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_buffer_full() {
        let buffer = OrderBuffer::new(2);

        buffer.push(make_order("BTCUSDT")).unwrap();
        buffer.push(make_order("ETHUSDT")).unwrap();

        assert!(buffer.is_full());

        let result = buffer.push(make_order("SOLUSDT"));
        assert!(result.is_err());
    }

    #[test]
    fn test_push_batch() {
        let buffer = OrderBuffer::new(5);

        let orders = vec![
            make_order("BTCUSDT"),
            make_order("ETHUSDT"),
            make_order("SOLUSDT"),
        ];

        let pushed = buffer.push_batch(orders);
        assert_eq!(pushed, 3);
        assert_eq!(buffer.len(), 3);
    }

    #[test]
    fn test_push_batch_partial() {
        let buffer = OrderBuffer::new(2);

        let orders = vec![
            make_order("BTCUSDT"),
            make_order("ETHUSDT"),
            make_order("SOLUSDT"),
        ];

        let pushed = buffer.push_batch(orders);
        assert_eq!(pushed, 2); // Only 2 fit
        assert_eq!(buffer.len(), 2);
    }

    #[test]
    fn test_peek() {
        let buffer = OrderBuffer::new(100);

        buffer.push(make_order("BTCUSDT")).unwrap();

        let peeked = buffer.peek().unwrap();
        assert_eq!(peeked.symbol, "BTCUSDT");
        assert_eq!(buffer.len(), 1); // Still in queue

        let popped = buffer.pop().unwrap();
        assert_eq!(popped.symbol, "BTCUSDT");
        assert!(buffer.peek().is_none());
    }

    #[test]
    fn test_restore() {
        let buffer = OrderBuffer::new(100);

        let orders = vec![
            make_order("BTCUSDT"),
            make_order("ETHUSDT"),
        ];

        buffer.restore(orders);
        assert_eq!(buffer.len(), 2);
    }

    #[test]
    fn test_drain_all() {
        let buffer = OrderBuffer::new(100);

        buffer.push(make_order("BTCUSDT")).unwrap();
        buffer.push(make_order("ETHUSDT")).unwrap();

        let all = buffer.drain_all();
        assert_eq!(all.len(), 2);
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_total_processed() {
        let buffer = OrderBuffer::new(100);

        assert_eq!(buffer.total_processed(), 0);

        buffer.push(make_order("BTCUSDT")).unwrap();
        buffer.push(make_order("ETHUSDT")).unwrap();
        buffer.pop();

        assert_eq!(buffer.total_processed(), 1);

        buffer.pop_batch(5);
        assert_eq!(buffer.total_processed(), 2);
    }
}
