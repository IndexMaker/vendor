use crate::accumulator::OrderAccumulator;
use crate::onchain::OnchainSubmitter;
use crate::processor::QuoteProcessor;
use alloy::providers::Provider;
use std::sync::Arc;
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;

/// Keeper orchestrator configuration
#[derive(Debug, Clone)]
pub struct KeeperLoopConfig {
    pub polling_interval_secs: u64,
    pub health_check_interval_secs: u64,
}

impl Default for KeeperLoopConfig {
    fn default() -> Self {
        Self {
            polling_interval_secs: 5,
            health_check_interval_secs: 30,
        }
    }
}

/// Main Keeper orchestrator
pub struct KeeperLoop<P>
where
    P: Provider + Clone,
{
    accumulator: Arc<OrderAccumulator>,
    processor: Arc<QuoteProcessor>,
    submitter: Arc<OnchainSubmitter<P>>,
    config: KeeperLoopConfig,
    cancel_token: CancellationToken,
}

impl<P> KeeperLoop<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    pub fn new(
        accumulator: Arc<OrderAccumulator>,
        processor: Arc<QuoteProcessor>,
        submitter: Arc<OnchainSubmitter<P>>,
        config: KeeperLoopConfig,
    ) -> Self {
        Self {
            accumulator,
            processor,
            submitter,
            config,
            cancel_token: CancellationToken::new(),
        }
    }

    /// Get cancellation token for graceful shutdown
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Start the main keeper loop
    pub async fn run(self: Arc<Self>) -> eyre::Result<()> {
        tracing::info!("🚀 Starting Keeper main loop");
        tracing::info!("  Polling interval: {}s", self.config.polling_interval_secs);
        tracing::info!("  Health check interval: {}s", self.config.health_check_interval_secs);

        // Spawn health check task
        let health_check_handle = self.spawn_health_check_task();

        // Spawn metrics task
        let metrics_handle = self.spawn_metrics_task();

        // Main polling loop
        let mut poll_interval = interval(Duration::from_secs(self.config.polling_interval_secs));

        loop {
            tokio::select! {
                _ = poll_interval.tick() => {
                    tracing::debug!("⏰ Polling tick");
                    
                    // Check if there are pending batches
                    let stats = self.accumulator.get_stats();
                    
                    if stats.should_flush {
                        tracing::info!("📦 Flushing batch with {} orders", stats.total_orders);
                        
                        if let Some(batch) = self.accumulator.flush_batch() {
                            self.process_batch_pipeline(batch).await;
                        }
                    } else {
                        tracing::trace!("No pending batches (active_indices={}, orders={})", 
                            stats.active_indices, stats.total_orders);
                    }
                }
                _ = self.cancel_token.cancelled() => {
                    tracing::info!("🛑 Shutdown signal received");
                    break;
                }
            }
        }

        // Cleanup
        tracing::info!("Shutting down keeper loop...");
        health_check_handle.abort();
        metrics_handle.abort();
        
        tracing::info!("✓ Keeper loop stopped");

        Ok(())
    }

    /// Process a batch through the full pipeline
    async fn process_batch_pipeline(&self, batch: crate::accumulator::OrderBatch) {
        tracing::info!("🔥 Processing batch:");
        tracing::info!("  Indices: {}", batch.indices.len());
        tracing::info!("  Total orders: {}", batch.total_order_count());

        let processor = self.processor.clone();
        let submitter = self.submitter.clone();

        tokio::spawn(async move {
            // Step 1: Process batch into submission payload
            let payload = match processor.process_batch(batch).await {
                Ok(p) => {
                    let summary = processor.get_payload_summary(&p);
                    
                    tracing::info!("📊 Payload generated:");
                    tracing::info!("  Buy orders: {}", summary.index_count);
                    tracing::info!("  Assets: {}", summary.unique_assets);
                    tracing::info!(
                        "  Total collateral: ${:.2}",
                        summary.total_collateral_usd.to_u128_raw() as f64 / 1e18
                    );
                    
                    p
                }
                Err(e) => {
                    tracing::error!("❌ Failed to process batch: {:?}", e);
                    return;
                }
            };

            // Step 2: Submit to blockchain
            match submitter.submit_payload(payload).await {
                Ok(result) => {
                    match result {
                        crate::onchain::SubmissionResult::Success { market_data_tx, buy_order_txs } => {
                            tracing::info!("✅ Submission successful!");
                            
                            if let Some(tx) = market_data_tx {
                                tracing::info!("  Market data: {} (block: {})", 
                                    tx.tx_hash, tx.block_number);
                            }
                            
                            for (index_id, tx) in buy_order_txs {
                                tracing::info!("  Buy order {}: {} (block: {})", 
                                    index_id, tx.tx_hash, tx.block_number);
                            }
                        }
                        crate::onchain::SubmissionResult::DryRun { would_submit } => {
                            tracing::info!("🔍 Dry run complete:");
                            tracing::info!("  Would submit {} assets", would_submit.market_data_assets);
                            tracing::info!("  Would submit {} orders", would_submit.buy_orders_count);
                        }
                        crate::onchain::SubmissionResult::Failed { error } => {
                            tracing::error!("❌ Submission failed: {}", error);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("❌ Submission error: {:?}", e);
                }
            }
        });
    }

    /// Spawn health check task
    fn spawn_health_check_task(&self) -> tokio::task::JoinHandle<()> {
        let interval_secs = self.config.health_check_interval_secs;
        let cancel_token = self.cancel_token.clone();
        let processor = self.processor.clone();

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(interval_secs));

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        tracing::debug!("💚 Health check");
                        
                        // Check vendor connectivity
                        match processor.vendor_client.health_check().await {
                            Ok(health) => {
                                tracing::debug!("  Vendor: OK (tracked_assets={})", 
                                    health.tracked_assets);
                            }
                            Err(e) => {
                                tracing::warn!("  Vendor: FAILED - {:?}", e);
                            }
                        }
                    }
                    _ = cancel_token.cancelled() => {
                        break;
                    }
                }
            }
        })
    }

    /// Spawn metrics logging task
    fn spawn_metrics_task(&self) -> tokio::task::JoinHandle<()> {
        let cancel_token = self.cancel_token.clone();
        let accumulator = self.accumulator.clone();

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(30));

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let stats = accumulator.get_stats();
                        
                        tracing::info!("📈 Keeper Metrics:");
                        tracing::info!("  Active indices: {}", stats.active_indices);
                        tracing::info!("  Pending orders: {}", stats.total_orders);
                        tracing::info!("  Oldest order age: {}ms", stats.oldest_order_age_ms);
                    }
                    _ = cancel_token.cancelled() => {
                        break;
                    }
                }
            }
        })
    }
}