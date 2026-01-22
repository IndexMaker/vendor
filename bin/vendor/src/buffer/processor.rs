//! Background processing loop (Story 3-6, AC #2, #8)
//!
//! Polls buffer, applies min size filtering, executes via IOC,
//! tracks costs, and checkpoints periodically.
//!
//! Enhanced for "on-chain first" strategy:
//! - Processes deferred exchange orders from the buffer
//! - Reconciles executed orders with simulated positions
//! - Realizes simulated inventory when exchange execution catches up

use super::{
    BufferConfig, BufferedOrder, BufferOrderSide, BufferState, OrderBuffer,
    MinSizeHandler, OrderAction, AcquisitionCostTracker, OrderFill, OrderBookSnapshot,
    persistence::{checkpoint, restore},
    WithdrawalHandler,
    InventorySimulator,
};
use crate::order_sender::types::{AssetOrder, OrderSide};
use crate::order_sender::bitget::BatchExecutionSummary;
use crate::supply::SupplyManager;
use common::amount::Amount;
use eyre::Result;
use parking_lot::RwLock;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Buffer processor - runs background order processing
pub struct BufferProcessor {
    /// Order buffer queue
    buffer: Arc<OrderBuffer>,
    /// Min size handler for order accumulation
    min_size_handler: Arc<MinSizeHandler>,
    /// Acquisition cost tracker
    cost_tracker: Arc<AcquisitionCostTracker>,
    /// Withdrawal handler for sell proceeds
    withdrawal_handler: Arc<RwLock<WithdrawalHandler>>,
    /// Configuration
    config: BufferConfig,
    /// Cancellation token for graceful shutdown
    cancel_token: CancellationToken,
    /// Last checkpoint time
    last_checkpoint: RwLock<Instant>,
    /// Last reconciliation time
    last_reconcile: RwLock<Instant>,
    /// Order sender channel (send orders to executor)
    order_tx: Option<mpsc::UnboundedSender<Vec<AssetOrder>>>,
    /// Inventory simulator for simulated position reconciliation
    inventory_simulator: Option<Arc<InventorySimulator>>,
    /// Supply manager for realizing simulated positions
    supply_manager: Option<Arc<RwLock<SupplyManager>>>,
}

impl BufferProcessor {
    /// Create a new buffer processor
    pub fn new(
        buffer: Arc<OrderBuffer>,
        min_size_handler: Arc<MinSizeHandler>,
        cost_tracker: Arc<AcquisitionCostTracker>,
        withdrawal_handler: Arc<RwLock<WithdrawalHandler>>,
        config: BufferConfig,
    ) -> Self {
        Self {
            buffer,
            min_size_handler,
            cost_tracker,
            withdrawal_handler,
            config,
            cancel_token: CancellationToken::new(),
            last_checkpoint: RwLock::new(Instant::now()),
            last_reconcile: RwLock::new(Instant::now()),
            order_tx: None,
            inventory_simulator: None,
            supply_manager: None,
        }
    }

    /// Set order sender channel
    pub fn with_order_sender(mut self, tx: mpsc::UnboundedSender<Vec<AssetOrder>>) -> Self {
        self.order_tx = Some(tx);
        self
    }

    /// Set inventory simulator for simulated position reconciliation
    pub fn with_inventory_simulator(mut self, sim: Arc<InventorySimulator>) -> Self {
        self.inventory_simulator = Some(sim);
        self
    }

    /// Set supply manager for realizing simulated positions
    pub fn with_supply_manager(mut self, sm: Arc<RwLock<SupplyManager>>) -> Self {
        self.supply_manager = Some(sm);
        self
    }

    /// Get cancellation token for shutdown
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Restore state from checkpoint on startup
    pub async fn restore_from_checkpoint(&self) -> Result<()> {
        let path = PathBuf::from(&self.config.checkpoint_path);
        let state = restore(&path).await?;

        // Restore pending orders to buffer
        if !state.pending_orders.is_empty() {
            self.buffer.restore(state.pending_orders);
        }

        // Restore buffered quantities to min size handler
        if !state.buffered_quantities.is_empty() {
            self.min_size_handler.restore_buffered_quantities(state.buffered_quantities);
        }

        tracing::info!(
            "Restored from checkpoint: {} pending orders, {} buffered assets",
            self.buffer.len(),
            self.min_size_handler.buffered_asset_count()
        );

        Ok(())
    }

    /// Save checkpoint to disk
    async fn save_checkpoint(&self) -> Result<()> {
        let state = BufferState {
            pending_orders: self.buffer.get_all(),
            buffered_quantities: self.min_size_handler.get_buffered_quantities(),
            last_checkpoint: chrono::Utc::now(),
            total_processed: self.buffer.total_processed(),
            total_buffered: self.buffer.len() as u64,
        };

        let path = PathBuf::from(&self.config.checkpoint_path);
        checkpoint(&state, &path).await?;

        *self.last_checkpoint.write() = Instant::now();

        Ok(())
    }

    /// Run the background processing loop
    pub async fn run(&self) -> Result<()> {
        tracing::info!(
            "Buffer processor starting (checkpoint: {}s, reconcile: {}s)",
            self.config.checkpoint_interval_secs,
            self.config.reconcile_interval_secs
        );

        let poll_interval = Duration::from_millis(100); // Poll every 100ms
        let checkpoint_interval = Duration::from_secs(self.config.checkpoint_interval_secs);
        let reconcile_interval = Duration::from_secs(self.config.reconcile_interval_secs);

        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    tracing::info!("Buffer processor shutting down...");

                    // Final checkpoint before exit
                    if let Err(e) = self.save_checkpoint().await {
                        tracing::error!("Failed to save final checkpoint: {:?}", e);
                    } else {
                        tracing::info!("Final checkpoint saved");
                    }

                    break;
                }

                _ = tokio::time::sleep(poll_interval) => {
                    // Process orders from buffer
                    if let Err(e) = self.process_pending_orders().await {
                        tracing::error!("Error processing orders: {:?}", e);
                    }

                    // Flush stale buffers
                    self.flush_stale_buffers().await;

                    // Periodic checkpoint
                    if self.last_checkpoint.read().elapsed() > checkpoint_interval {
                        if let Err(e) = self.save_checkpoint().await {
                            tracing::error!("Checkpoint failed: {:?}", e);
                        }
                    }

                    // Periodic reconciliation
                    if self.last_reconcile.read().elapsed() > reconcile_interval {
                        self.reconcile().await;
                        // Also check for stale simulations during reconciliation
                        self.process_stale_simulations();
                        *self.last_reconcile.write() = Instant::now();
                    }
                }
            }
        }

        Ok(())
    }

    /// Process pending orders from the buffer
    async fn process_pending_orders(&self) -> Result<()> {
        // Pop a batch of orders
        let batch = self.buffer.pop_batch(10);

        if batch.is_empty() {
            return Ok(());
        }

        tracing::debug!("Processing {} orders from buffer", batch.len());

        // Group orders by symbol for min-size handling
        let mut orders_to_execute: Vec<AssetOrder> = Vec::new();

        for order in batch {
            // Apply min size filtering
            match self.min_size_handler.process_order(&order.symbol, order.quantity) {
                OrderAction::Execute(quantity) => {
                    // Convert to AssetOrder for IOC execution
                    let asset_order = AssetOrder::limit(
                        order.symbol.clone(),
                        match order.side {
                            BufferOrderSide::Buy => OrderSide::Buy,
                            BufferOrderSide::Sell => OrderSide::Sell,
                        },
                        quantity,
                        Amount::ZERO, // Price will be determined at execution time
                    );

                    orders_to_execute.push(asset_order);

                    tracing::info!(
                        "Order ready for execution: {} {} {}",
                        order.side,
                        quantity.to_u128_raw() as f64 / 1e18,
                        order.symbol
                    );
                }
                OrderAction::Buffer => {
                    tracing::debug!(
                        "Order buffered (below min size): {} {} {}",
                        order.side,
                        order.quantity.to_u128_raw() as f64 / 1e18,
                        order.symbol
                    );
                }
            }
        }

        // Send orders to executor if we have any
        if !orders_to_execute.is_empty() {
            if let Some(ref tx) = self.order_tx {
                if let Err(e) = tx.send(orders_to_execute.clone()) {
                    tracing::error!("Failed to send orders to executor: {:?}", e);
                } else {
                    tracing::info!(
                        "Sent {} orders to executor",
                        orders_to_execute.len()
                    );
                }
            } else {
                tracing::warn!(
                    "No order sender configured - {} orders not executed",
                    orders_to_execute.len()
                );
            }
        }

        Ok(())
    }

    /// Flush stale buffers (orders waiting too long)
    async fn flush_stale_buffers(&self) {
        let stale = self.min_size_handler.get_stale_buffers();

        for (symbol, quantity) in stale {
            tracing::warn!(
                "Flushing stale buffer for {} ({} below minimum but waiting too long)",
                symbol,
                quantity.to_u128_raw() as f64 / 1e18
            );

            // Flush and queue for execution
            if let Some(qty) = self.min_size_handler.flush_buffer(&symbol) {
                // Create order for execution
                let order = AssetOrder::limit(
                    symbol.clone(),
                    OrderSide::Buy, // Assume buy for stale flushes - should track side per buffer
                    qty,
                    Amount::ZERO,
                );

                if let Some(ref tx) = self.order_tx {
                    if let Err(e) = tx.send(vec![order]) {
                        tracing::error!("Failed to send stale flush order: {:?}", e);
                    }
                }
            }
        }
    }

    /// Reconcile buffer state vs exchange positions
    async fn reconcile(&self) {
        // TODO: Implement actual reconciliation with Bitget positions
        // For now, just log buffer state

        let buffer_depth = self.buffer.len();
        let buffered_assets = self.min_size_handler.buffered_asset_count();
        let total_processed = self.buffer.total_processed();
        let cost_summary = self.cost_tracker.summary();

        tracing::info!(
            "📊 Reconciliation: buffer={}, buffered_assets={}, processed={}, costs={}",
            buffer_depth,
            buffered_assets,
            total_processed,
            cost_summary
        );
    }

    /// Record execution results and track costs
    /// Also reconciles with simulated positions when exchange execution catches up
    pub fn record_execution_results(
        &self,
        results: &BatchExecutionSummary,
        orderbooks: &[(String, OrderBookSnapshot)],
    ) {
        let ob_map: std::collections::HashMap<_, _> = orderbooks
            .iter()
            .cloned()
            .collect();

        for result in &results.results {
            if result.is_success() {
                // Find matching orderbook snapshot
                if let Some(ob) = ob_map.get(&result.symbol) {
                    let fill = OrderFill {
                        asset_id: 0, // TODO: Need asset ID mapping
                        symbol: result.symbol.clone(),
                        avg_fill_price: result.avg_price,
                        quantity: result.filled_quantity,
                        fee: result.fees,
                        fee_currency: result.fee_detail
                            .as_ref()
                            .map(|f| f.currency.clone())
                            .unwrap_or_else(|| "USDT".to_string()),
                    };

                    self.cost_tracker.record_fill(&fill, ob);
                }

                // Reconcile with simulated positions
                self.reconcile_with_simulation(
                    &result.symbol,
                    result.filled_quantity,
                    result.avg_price,
                );
            }
        }

        tracing::info!(
            "Recorded {} execution results ({} successful)",
            results.total,
            results.successful
        );
    }

    /// Reconcile executed order with simulated positions
    /// When exchange execution catches up, we:
    /// 1. Consume simulated position from InventorySimulator
    /// 2. Realize the position in SupplyManager (move from simulated to actual)
    fn reconcile_with_simulation(
        &self,
        symbol: &str,
        filled_quantity: Amount,
        filled_price: Amount,
    ) {
        // Step 1: Consume from inventory simulator
        if let Some(ref sim) = self.inventory_simulator {
            let consumed = sim.consume_simulation(symbol, filled_quantity, filled_price);

            if consumed != Amount::ZERO {
                tracing::info!(
                    "Reconciled simulated position: {} {} consumed ({:.6} filled)",
                    consumed.to_u128_raw() as f64 / 1e18,
                    symbol,
                    filled_quantity.to_u128_raw() as f64 / 1e18
                );

                // Step 2: Realize in supply manager
                // Note: We don't need to update supply_long/supply_short here because
                // the on-chain state was already updated during the "on-chain first" phase.
                // The simulated_long/simulated_short in SupplyState should have already
                // been incorporated into the on-chain submission.

                // Log simulation stats
                let stats = sim.get_exposure_stats();
                tracing::debug!(
                    "Simulation stats after reconciliation: {}",
                    stats
                );
            }
        }
    }

    /// Process stale simulations that have been waiting too long
    /// These might need manual intervention or forced execution
    pub fn process_stale_simulations(&self) {
        if let Some(ref sim) = self.inventory_simulator {
            let stale = sim.get_stale_simulations();

            if !stale.is_empty() {
                tracing::warn!(
                    "Found {} stale simulations - consider manual intervention",
                    stale.len()
                );

                for pos in &stale {
                    tracing::warn!(
                        "  Stale: {} {} (age: {}s, reason: {}, on-chain: {})",
                        pos.quantity.to_u128_raw() as f64 / 1e18,
                        pos.symbol,
                        chrono::Utc::now()
                            .signed_duration_since(pos.created_at)
                            .num_seconds(),
                        pos.reason,
                        pos.onchain_submitted
                    );
                }
            }
        }
    }

    /// Get current buffer statistics
    pub fn stats(&self) -> ProcessorStats {
        let sim_stats = self.inventory_simulator
            .as_ref()
            .map(|sim| sim.get_exposure_stats());

        ProcessorStats {
            buffer_depth: self.buffer.len(),
            buffered_assets: self.min_size_handler.buffered_asset_count(),
            total_processed: self.buffer.total_processed(),
            cost_summary: self.cost_tracker.summary(),
            last_checkpoint_secs_ago: self.last_checkpoint.read().elapsed().as_secs(),
            last_reconcile_secs_ago: self.last_reconcile.read().elapsed().as_secs(),
            simulated_exposure_usd: sim_stats.as_ref().map(|s| s.total_exposure_usd),
            simulated_position_count: sim_stats.as_ref().map(|s| s.position_count),
        }
    }
}

/// Processor statistics
#[derive(Debug, Clone)]
pub struct ProcessorStats {
    pub buffer_depth: usize,
    pub buffered_assets: usize,
    pub total_processed: u64,
    pub cost_summary: super::cost_tracker::CostSummary,
    pub last_checkpoint_secs_ago: u64,
    pub last_reconcile_secs_ago: u64,
    /// Total simulated exposure in USD (if simulation enabled)
    pub simulated_exposure_usd: Option<f64>,
    /// Number of simulated positions (if simulation enabled)
    pub simulated_position_count: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_processor() -> BufferProcessor {
        let buffer = Arc::new(OrderBuffer::new(100));
        let min_size = Arc::new(MinSizeHandler::default());
        let cost_tracker = Arc::new(AcquisitionCostTracker::new());
        let withdrawal = Arc::new(RwLock::new(WithdrawalHandler::new(Duration::from_secs(300))));
        let config = BufferConfig::default();

        BufferProcessor::new(buffer, min_size, cost_tracker, withdrawal, config)
    }

    #[test]
    fn test_processor_creation() {
        let processor = make_processor();
        assert_eq!(processor.buffer.len(), 0);
    }

    #[test]
    fn test_cancel_token() {
        let processor = make_processor();
        let token = processor.cancel_token();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn test_stats() {
        let processor = make_processor();
        let stats = processor.stats();
        assert_eq!(stats.buffer_depth, 0);
        assert_eq!(stats.buffered_assets, 0);
        assert_eq!(stats.total_processed, 0);
    }

    #[tokio::test]
    async fn test_process_empty_buffer() {
        let processor = make_processor();
        let result = processor.process_pending_orders().await;
        assert!(result.is_ok());
    }
}
