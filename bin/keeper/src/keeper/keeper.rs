use crate::accumulator::{OrderAccumulator, ProcessableBatch};
use crate::assets::{AssetExtractor, ExtractionResult};
use crate::chain::StewardClient;
use crate::onchain::{OnchainSubmitter, OrderType, PendingOrder, SettlementPayload};
use crate::processor::QuoteProcessor;
use crate::vendor::{UpdateMarketDataRequest, VendorClient};
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
    /// Asset extractor for Stewart queries (Story 2.4)
    asset_extractor: Option<Arc<AssetExtractor>>,
    /// Vendor client for market data updates (Story 2.5)
    vendor_client: Arc<VendorClient>,
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
        vendor_client: Arc<VendorClient>,
        config: KeeperLoopConfig,
    ) -> Self {
        Self {
            accumulator,
            processor,
            submitter,
            asset_extractor: None,
            vendor_client,
            config,
            cancel_token: CancellationToken::new(),
        }
    }

    /// Create KeeperLoop with AssetExtractor for Stewart integration (Story 2.4)
    #[allow(dead_code)] // Reserved for future Stewart integration
    pub fn with_asset_extractor(
        accumulator: Arc<OrderAccumulator>,
        processor: Arc<QuoteProcessor>,
        submitter: Arc<OnchainSubmitter<P>>,
        steward_client: Arc<StewardClient>,
        vendor_client: Arc<VendorClient>,
        config: KeeperLoopConfig,
    ) -> Self {
        let asset_extractor = Arc::new(AssetExtractor::new(steward_client));
        Self {
            accumulator,
            processor,
            submitter,
            asset_extractor: Some(asset_extractor),
            vendor_client,
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
        tracing::info!(poll_secs = self.config.polling_interval_secs, "Keeper loop started");

        // Spawn health check task
        let health_check_handle = self.spawn_health_check_task();

        // Spawn metrics task
        let metrics_handle = self.spawn_metrics_task();

        // Main polling loop
        let mut poll_interval = interval(Duration::from_secs(self.config.polling_interval_secs));

        loop {
            tokio::select! {
                _ = poll_interval.tick() => {
                    let stats = self.accumulator.get_stats();
                    if stats.should_flush {
                        if let Some(batch) = self.accumulator.flush_batch() {
                            let processable = ProcessableBatch::from_order_batch(batch.clone());
                            tracing::debug!(orders = stats.total_orders, "Flush");
                            self.process_batch_pipeline(batch, processable).await;
                        }
                    }
                }
                _ = self.cancel_token.cancelled() => {
                    break;
                }
            }
        }

        health_check_handle.abort();
        metrics_handle.abort();
        tracing::info!("Keeper loop stopped");

        Ok(())
    }

    /// Process a batch through the full pipeline
    /// Takes both OrderBatch (for processor) and ProcessableBatch (for logging/structure)
    async fn process_batch_pipeline(
        &self,
        _batch: crate::accumulator::OrderBatch,
        processable: ProcessableBatch,
    ) {
        tracing::debug!(indices = processable.unique_index_ids.len(), buys = processable.buy_count(), sells = processable.sell_count(), "Batch");

        let _processor = self.processor.clone(); // Reserved for future use
        let submitter = self.submitter.clone();
        let asset_extractor = self.asset_extractor.clone();
        let vendor_client = self.vendor_client.clone();

        tokio::spawn(async move {
            // Generate correlation ID for this batch (Story 2.5)
            let correlation_id = processable.generate_correlation_id();
            tracing::debug!(batch_id = %correlation_id, "Batch correlation ID");

            let extracted_assets: Option<ExtractionResult> = if let Some(ref extractor) = asset_extractor {
                match extractor.extract_assets_for_batch(&processable.unique_index_ids).await {
                    Ok(result) => {
                        tracing::debug!(assets = result.asset_ids.len(), failed = result.failed_index_ids.len(), "Extracted");
                        Some(result)
                    }
                    Err(e) => {
                        tracing::error!(%e, "Extraction failed");
                        None
                    }
                }
            } else {
                None
            };

            // =====================================================================
            // Story 2.5: Vendor HTTP Communication (Steps 6-7)
            // Send sorted asset list to Vendor and WAIT for HTTP 200 before
            // proceeding to Castle/Vault settlement calls (Story 2.6)
            // =====================================================================
            if let Some(ref result) = extracted_assets {
                if !result.asset_ids.is_empty() {
                    let request = UpdateMarketDataRequest {
                        assets: result.asset_ids.clone(),
                        batch_id: correlation_id.clone(),
                    };

                    match vendor_client.update_market_data(request).await {
                        Ok(response) => {
                            tracing::debug!(batch_id = %correlation_id, updated = response.assets_updated, "Vendor OK");

                            // =====================================================================
                            // Story 2.6: Castle/Vault Settlement Calls (Steps 8-9)
                            // Only proceeds AFTER Vendor HTTP 200 confirmed (AC: Task 4.2)
                            // =====================================================================

                            // Build settlement payload from processable batch (AC: Task 4.1)
                            // Story 2.6 FIX: Use settlement orders with actual trader addresses
                            let settlement_payload = SettlementPayload {
                                index_ids: processable.unique_index_ids.clone(),
                                buy_orders: processable
                                    .settlement_buy_orders
                                    .iter()
                                    .map(|order| PendingOrder {
                                        index_id: order.index_id,
                                        trader_address: order.trader_address,
                                        max_order_size: 0, // Use config default
                                        order_type: OrderType::Buy,
                                        vault_address: order.vault_address,
                                    })
                                    .collect(),
                                sell_orders: processable
                                    .settlement_sell_orders
                                    .iter()
                                    .map(|order| PendingOrder {
                                        index_id: order.index_id,
                                        trader_address: order.trader_address,
                                        max_order_size: 0, // Use config default
                                        order_type: OrderType::Sell,
                                        vault_address: order.vault_address,
                                    })
                                    .collect(),
                                batch_id: correlation_id.clone(),
                            };

                            match submitter.settle_batch(settlement_payload).await {
                                Ok(result) => {
                                    tracing::info!(batch_id = %correlation_id, ok = result.total_succeeded, err = result.total_failed, ms = result.elapsed_ms, "Settled");
                                    for (idx, res) in &result.buy_order_results {
                                        if let Err(e) = res {
                                            tracing::error!(batch_id = %correlation_id, index = idx, %e, "Buy failed");
                                        }
                                    }
                                    for (idx, res) in &result.sell_order_results {
                                        if let Err(e) = res {
                                            tracing::error!(batch_id = %correlation_id, index = idx, %e, "Sell failed");
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::error!(batch_id = %correlation_id, %e, "Settlement failed");
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!(batch_id = %correlation_id, %e, "Vendor failed");
                            return;
                        }
                    }
                }
            }

            // NOTE: The old submit_payload() flow is now replaced by settle_batch() above (AC: Task 4.4)
            // The settlement pipeline follows the correct sequence:
            // 1. updateMultipleIndexQuotes (Castle)
            // 2. processPendingBuyOrder (Vault) - for each buy order
            // 3. processPendingSellOrder (Vault) - for each sell order
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
                        if let Err(e) = processor.vendor_client.health_check().await {
                            tracing::warn!(?e, "Vendor health failed");
                        }
                    }
                    _ = cancel_token.cancelled() => break,
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
                        tracing::debug!(indices = stats.active_indices, orders = stats.total_orders, age_ms = stats.oldest_order_age_ms, "Metrics");
                    }
                    _ = cancel_token.cancelled() => break,
                }
            }
        })
    }
}