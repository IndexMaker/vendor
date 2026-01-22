use super::types::{
    AssetsQuote, ErrorResponse, HealthResponse, QuoteAssetsRequest,
    ProcessAssetsRequest, ProcessAssetsResponse, ProcessMetrics,
    QueueOrdersRequest, QueueOrdersResponse, BufferStatus, HealthResponseWithBuffer,
    UpdateMarketDataRequest, UpdateMarketDataResponse,
};
// Story 3-6: Buffer types - use full path since api is a sibling module
pub use crate::buffer::{OrderBuffer, BufferedOrder, BufferOrderSide, MinSizeHandler};
use crate::market_data::{compute_margin_vector, compute_supply_placeholder, MarginConfig};
use crate::onchain::{AssetMapper, StalenessManager, VendorSubmitter, SubmissionData};
use crate::order_book::{OrderBookProcessor, PSLComputeService};
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use common::amount::Amount;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Instant;

/// Generic AppState - works with any Provider type
#[derive(Clone)]
pub struct AppState<P>
where
    P: alloy::providers::Provider + Clone,
{
    pub vendor_id: u128,
    pub asset_mapper: Arc<RwLock<AssetMapper>>,
    pub staleness_manager: Arc<RwLock<StalenessManager<P>>>,
    pub order_book_processor: Arc<OrderBookProcessor>,
    /// PSL compute service for processing assets (Story 3-4)
    pub psl_service: Option<Arc<PSLComputeService>>,
    /// Vendor submitter for on-chain calls (Story 3-4)
    pub vendor_submitter: Option<Arc<VendorSubmitter<P>>>,
    /// Margin computation config (Story 3-4)
    pub margin_config: Option<MarginConfig>,
    /// Order buffer for async processing (Story 3-6)
    pub order_buffer: Option<Arc<OrderBuffer>>,
    /// Min size handler for async processing (Story 3-6)
    pub min_size_handler: Option<Arc<MinSizeHandler>>,
}

/// Health check endpoint - generic over Provider
pub async fn health<P>(State(state): State<AppState<P>>) -> impl IntoResponse
where
    P: alloy::providers::Provider + Clone + Send + Sync + 'static,
{
    let result = tokio::task::spawn_blocking(move || {
        let tracked_assets = state.asset_mapper.read().get_all_symbols().len();
        
        HealthResponse {
            status: "healthy".to_string(),
            vendor_id: state.vendor_id,
            tracked_assets,
        }
    })
    .await
    .unwrap();
    
    Json(result)
}

/// Main quote_assets endpoint - generic over Provider
pub async fn quote_assets<P>(
    State(state): State<AppState<P>>,
    Json(request): Json<QuoteAssetsRequest>,
) -> Result<Json<AssetsQuote>, (StatusCode, Json<ErrorResponse>)>
where
    P: alloy::providers::Provider + Clone + Send + Sync + 'static,
{
    tokio::task::spawn_blocking(move || {
        quote_assets_sync(state, request)
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Task panicked: {}", e),
            }),
        )
    })?
}

// Synchronous implementation - generic over Provider
fn quote_assets_sync<P>(
    state: AppState<P>,
    request: QuoteAssetsRequest,
) -> Result<Json<AssetsQuote>, (StatusCode, Json<ErrorResponse>)>
where
    P: alloy::providers::Provider + Clone,
{
    tracing::info!("Received quote request for {} assets", request.assets.len());
    tracing::debug!("Requested assets: {:?}", request.assets);
    
    tracing::warn!("Skipping on-chain cache update in sync context");
    
    // 2. Calculate live metrics
    let mut live_metrics = Vec::new();
    
    for asset_id in &request.assets {
        let symbol = {
            let mapper = state.asset_mapper.read();
            match mapper.get_symbol(*asset_id) {
                Some(s) => s.clone(),
                None => {
                    tracing::warn!("Asset ID {} not found in mapper", asset_id);
                    continue;
                }
            }
        };
        
        match state.order_book_processor.calculate_metrics(&symbol) {
            Ok(metrics) => {
                tracing::trace!(
                    "Calculated live metrics for {}: L={:.2}, P={:.2}, S={:.6}",
                    symbol,
                    metrics.liquidity.to_u128_raw() as f64 / 1e18,
                    metrics.price.to_u128_raw() as f64 / 1e18,
                    metrics.slope.to_u128_raw() as f64 / 1e18
                );
                live_metrics.push((*asset_id, metrics));
            }
            Err(e) => {
                tracing::warn!("Failed to calculate metrics for {}: {:?}", symbol, e);
            }
        }
    }
    
    if live_metrics.is_empty() {
        tracing::warn!("No live metrics calculated");
        return Ok(Json(AssetsQuote::new()));
    }
    
    // 3. Find stale assets
    let stale_asset_ids = {
        let staleness_mgr = state.staleness_manager.read();
        
        let mut stale = Vec::new();
        
        for (asset_id, metrics) in &live_metrics {
            let symbol = &metrics.symbol;
            
            if let Some(cached) = staleness_mgr.get_cached_data(symbol) {
                let (is_stale, reason) = cached.is_stale(
                    metrics.liquidity,
                    metrics.price,
                    metrics.slope,
                    staleness_mgr.config().price_threshold,
                    staleness_mgr.config().liquidity_threshold,
                    staleness_mgr.config().time_threshold_secs,
                );
                
                if is_stale {
                    tracing::info!("Asset {} is stale: {}", symbol, reason);
                    stale.push(*asset_id);
                }
            } else {
                tracing::info!("Asset {} not in cache", symbol);
                stale.push(*asset_id);
            }
        }
        
        stale
    };
    
    // 4. Build response
    let mut response = AssetsQuote::new();
    
    for asset_id in stale_asset_ids {
        if let Some((_, metrics)) = live_metrics.iter().find(|(id, _)| *id == asset_id) {
            response.assets.push(asset_id);
            response.liquidity.push(metrics.liquidity.to_u128_raw() as f64 / 1e18);
            response.prices.push(metrics.price.to_u128_raw() as f64 / 1e18);
            response.slopes.push(metrics.slope.to_u128_raw() as f64 / 1e18);
        }
    }
    
    // 5. Mark submitted
    if !response.assets.is_empty() {
        let symbols: Vec<String> = {
            let mapper = state.asset_mapper.read();
            response
                .assets
                .iter()
                .filter_map(|id| mapper.get_symbol(*id).cloned())
                .collect()
        };
        
        state.staleness_manager.write().mark_submitted(&symbols);
    }
    
    let reduction_pct = if request.assets.is_empty() {
        0
    } else {
        100 - (response.assets.len() * 100 / request.assets.len())
    };
    
    tracing::info!(
        "Quote response: {} requested, {} stale ({}% reduction)",
        request.assets.len(),
        response.assets.len(),
        reduction_pct
    );
    
    Ok(Json(response))
}

/// POST /process-assets endpoint for Keeper integration (Story 3-4, AC #4, #5)
///
/// Computes P/S/L vectors, margin, and supply, then submits all three
/// calls concurrently to on-chain. Returns HTTP 200 only after all
/// submissions are confirmed.
///
/// # Request
/// ```json
/// { "asset_ids": [1, 2, 3], "batch_id": "batch-123" }
/// ```
///
/// # Response
/// - 200: All submissions succeeded with timing metrics
/// - 400: Invalid request (empty assets)
/// - 500: Submission failed (or endpoint not configured)
///
/// NOTE: This endpoint requires PSL service and VendorSubmitter to be configured
/// in AppState. If not configured, returns 501 Not Implemented.
pub async fn process_assets<P>(
    State(state): State<AppState<P>>,
    Json(request): Json<ProcessAssetsRequest>,
) -> Result<Json<ProcessAssetsResponse>, (StatusCode, Json<ErrorResponse>)>
where
    P: alloy::providers::Provider + Clone + Send + Sync + 'static,
{
    let correlation_id = request.batch_id.clone();
    tracing::info!(
        "📥 Processing assets request [batch_id={}]: {} assets",
        correlation_id,
        request.asset_ids.len()
    );

    // Validate request (AC: empty asset_ids → HTTP 400)
    if request.asset_ids.is_empty() {
        tracing::warn!("Empty asset_ids in request [batch_id={}]", correlation_id);
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "asset_ids cannot be empty".to_string(),
            }),
        ));
    }

    // Check if the endpoint is properly configured
    // Return 501 if PSL service or VendorSubmitter not configured
    let (psl_service, vendor_submitter) = match (state.psl_service.clone(), state.vendor_submitter.clone()) {
        (Some(psl), Some(vs)) => (psl, vs),
        _ => {
            tracing::warn!(
                "Process-assets endpoint not configured [batch_id={}]",
                correlation_id
            );
            return Err((
                StatusCode::NOT_IMPLEMENTED,
                Json(ErrorResponse {
                    error: "Process-assets endpoint not configured. PSL service or VendorSubmitter missing.".to_string(),
                }),
            ));
        }
    };

    let margin_config = state.margin_config.clone().unwrap_or_default();
    let asset_ids = request.asset_ids.clone();

    // Execute processing in a spawned task to handle lifetime issues
    let result = process_assets_impl(
        psl_service,
        vendor_submitter,
        margin_config,
        asset_ids,
        correlation_id.clone(),
    ).await;

    result.map(Json).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: e }),
        )
    })
}

/// Internal implementation of process_assets logic
async fn process_assets_impl<P>(
    psl_service: Arc<PSLComputeService>,
    vendor_submitter: Arc<VendorSubmitter<P>>,
    margin_config: MarginConfig,
    asset_ids: Vec<u128>,
    correlation_id: String,
) -> Result<ProcessAssetsResponse, String>
where
    P: alloy::providers::Provider + Clone + Send + Sync + 'static,
{
    // Step 0: Ensure all requested assets are registered with vendor
    // This prevents MathUnderflow errors from JUPD instruction in VIL
    tracing::debug!("Ensuring assets are registered [batch_id={}]", correlation_id);
    vendor_submitter
        .ensure_assets_registered(&asset_ids)
        .await
        .map_err(|e| {
            tracing::error!(
                "❌ Failed to register missing assets [batch_id={}]: {}",
                correlation_id,
                e
            );
            format!("Asset registration failed: {}", e)
        })?;

    // Step 0.5: Filter to only registered assets (safety net)
    let filtered_asset_ids = vendor_submitter
        .filter_to_registered(&asset_ids)
        .await
        .map_err(|e| {
            tracing::error!(
                "❌ Failed to filter assets [batch_id={}]: {}",
                correlation_id,
                e
            );
            format!("Asset filter failed: {}", e)
        })?;

    if filtered_asset_ids.is_empty() {
        tracing::warn!("No registered assets to process [batch_id={}]", correlation_id);
        return Ok(ProcessAssetsResponse {
            batch_id: correlation_id,
            assets_processed: 0,
            metrics: ProcessMetrics {
                margin_ms: 0,
                supply_ms: 0,
                market_data_ms: 0,
                total_ms: 0,
            },
        });
    }

    // Step 1: Compute P/S/L vectors (Story 3-3 integration)
    tracing::debug!("Computing P/S/L vectors for {} assets [batch_id={}]", filtered_asset_ids.len(), correlation_id);
    let psl_vectors = psl_service.compute_psl_for_assets(&filtered_asset_ids).await;

    if psl_vectors.is_empty() {
        tracing::error!("No P/S/L vectors computed [batch_id={}]", correlation_id);
        return Err("Failed to compute P/S/L vectors".to_string());
    }

    tracing::debug!(
        "Computed P/S/L for {} assets [batch_id={}]",
        psl_vectors.len(),
        correlation_id
    );

    // Step 2: Compute margin vector (AC #1: M_i = V_max/n / P_i)
    let margins = compute_margin_vector(&psl_vectors.prices, &margin_config);

    // Step 3: Compute supply vectors (placeholder for Story 3-6)
    let (supply_long, supply_short) = compute_supply_placeholder(psl_vectors.len());
    // TODO: Story 3-6 will implement actual Poisson PDE solver

    // Step 4: Prepare submission data
    let submission_data = SubmissionData {
        asset_ids: psl_vectors.asset_ids.clone(),
        margins,
        supply_long,
        supply_short,
        prices: psl_vectors.prices.clone(),
        slopes: psl_vectors.slopes.clone(),
        liquidities: psl_vectors.liquidities.clone(),
    };

    if !submission_data.is_valid() {
        tracing::error!("Submission data validation failed [batch_id={}]", correlation_id);
        return Err("Submission data vector length mismatch".to_string());
    }

    // Step 5: Submit all three calls concurrently (AC #4, #5)
    tracing::info!("Submitting all data concurrently [batch_id={}]", correlation_id);
    let metrics = vendor_submitter
        .submit_all_concurrent(&submission_data, Some(correlation_id.clone()))
        .await
        .map_err(|e| {
            tracing::error!(
                "❌ Concurrent submission failed [batch_id={}]: {}",
                correlation_id,
                e
            );
            format!("Submission failed: {}", e)
        })?;

    // Step 6: Build success response with metrics
    let response = ProcessAssetsResponse {
        batch_id: correlation_id.clone(),
        assets_processed: submission_data.len(),
        metrics: ProcessMetrics {
            margin_ms: metrics.margin_ms.unwrap_or(0),
            supply_ms: metrics.supply_ms.unwrap_or(0),
            market_data_ms: metrics.market_data_ms.unwrap_or(0),
            total_ms: metrics.total_ms.unwrap_or(0),
        },
    };

    tracing::info!(
        "✅ Process assets complete [batch_id={}]: {} assets in {}ms",
        correlation_id,
        response.assets_processed,
        response.metrics.total_ms
    );

    Ok(response)
}

// ============================================================================
// Story 3-6: Async Buffer Endpoints
// ============================================================================

/// POST /queue-orders endpoint for immediate order acceptance (Story 3-6, AC #1)
///
/// Accepts orders immediately (< 100ms) and queues them for async processing.
/// Orders are processed in the background via BufferProcessor.
///
/// # Request
/// ```json
/// {
///     "orders": [
///         { "asset_id": 1, "symbol": "BTCUSDT", "side": "buy", "quantity": 0.001 }
///     ],
///     "batch_id": "batch-123"
/// }
/// ```
///
/// # Response
/// - 200: Orders accepted and queued
/// - 400: Invalid request
/// - 429: Buffer full (backpressure)
/// - 501: Buffer not configured
pub async fn queue_orders<P>(
    State(state): State<AppState<P>>,
    Json(request): Json<QueueOrdersRequest>,
) -> Result<Json<QueueOrdersResponse>, (StatusCode, Json<ErrorResponse>)>
where
    P: alloy::providers::Provider + Clone + Send + Sync + 'static,
{
    let start = Instant::now();
    let correlation_id = request.batch_id.clone();

    tracing::info!(
        "📥 Queue orders request [batch_id={}]: {} orders",
        correlation_id,
        request.orders.len()
    );

    // Check if buffer is configured
    let buffer = match &state.order_buffer {
        Some(b) => b.clone(),
        None => {
            tracing::warn!("Queue-orders endpoint not configured [batch_id={}]", correlation_id);
            return Err((
                StatusCode::NOT_IMPLEMENTED,
                Json(ErrorResponse {
                    error: "Async buffer not configured".to_string(),
                }),
            ));
        }
    };

    // Validate request
    if request.orders.is_empty() {
        tracing::warn!("Empty orders in request [batch_id={}]", correlation_id);
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "orders cannot be empty".to_string(),
            }),
        ));
    }

    // Check backpressure
    if buffer.is_full() {
        tracing::warn!(
            "Buffer full - rejecting {} orders [batch_id={}]",
            request.orders.len(),
            correlation_id
        );
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(ErrorResponse {
                error: format!(
                    "Buffer full ({} orders). Try again later.",
                    buffer.len()
                ),
            }),
        ));
    }

    // Convert and queue orders
    let mut queued_count = 0;

    for order in &request.orders {
        let side = match order.side.to_lowercase().as_str() {
            "buy" => BufferOrderSide::Buy,
            "sell" => BufferOrderSide::Sell,
            _ => {
                tracing::warn!(
                    "Invalid side '{}' for {} [batch_id={}]",
                    order.side,
                    order.symbol,
                    correlation_id
                );
                continue;
            }
        };

        let quantity = Amount::from_u128_raw((order.quantity * 1e18) as u128);

        let buffered_order = BufferedOrder::new(
            order.asset_id,
            order.symbol.clone(),
            side,
            quantity,
            format!("{}-{}", correlation_id, queued_count),
        )
        .with_batch_id(correlation_id.clone());

        match buffer.push(buffered_order) {
            Ok(()) => queued_count += 1,
            Err(e) => {
                tracing::warn!(
                    "Failed to queue order for {} [batch_id={}]: {:?}",
                    order.symbol,
                    correlation_id,
                    e
                );
                break; // Buffer full, stop queuing
            }
        }
    }

    let elapsed = start.elapsed();
    let queue_depth = buffer.len();

    // Estimate processing time based on queue depth and rate limit (10 orders/sec)
    let estimated_ms = ((queue_depth as f64 / 10.0) * 1000.0) as u64;

    tracing::info!(
        "✅ Queued {} orders in {:?} [batch_id={}] (queue depth: {})",
        queued_count,
        elapsed,
        correlation_id,
        queue_depth
    );

    // Warn if we took too long (should be < 100ms)
    if elapsed.as_millis() > 100 {
        tracing::warn!(
            "⚠️ Queue-orders took {:?} - exceeds 100ms target [batch_id={}]",
            elapsed,
            correlation_id
        );
    }

    Ok(Json(QueueOrdersResponse {
        accepted: queued_count > 0,
        queued_count,
        batch_id: correlation_id,
        queue_depth,
        estimated_processing_ms: Some(estimated_ms),
    }))
}

/// GET /health with buffer status (Story 3-6, AC #6.3)
pub async fn health_with_buffer<P>(State(state): State<AppState<P>>) -> impl IntoResponse
where
    P: alloy::providers::Provider + Clone + Send + Sync + 'static,
{
    let result = tokio::task::spawn_blocking(move || {
        let tracked_assets = state.asset_mapper.read().get_all_symbols().len();

        let buffer_status = state.order_buffer.as_ref().map(|buffer| {
            let min_size_handler = state.min_size_handler.as_ref();

            BufferStatus {
                queue_depth: buffer.len(),
                buffered_assets: min_size_handler
                    .map(|h| h.buffered_asset_count())
                    .unwrap_or(0),
                total_processed: buffer.total_processed(),
                oldest_order_age_ms: buffer
                    .peek()
                    .map(|o| o.age_ms())
                    .unwrap_or(0),
            }
        });

        HealthResponseWithBuffer {
            status: "healthy".to_string(),
            vendor_id: state.vendor_id,
            tracked_assets,
            buffer: buffer_status,
        }
    })
    .await
    .unwrap();

    Json(result)
}

// ============================================================================
// Keeper Integration: Update Market Data Endpoint
// ============================================================================

/// POST /update-market-data endpoint for Keeper integration
///
/// This endpoint is called by the Keeper after detecting BuyOrder/SellOrder events.
/// It accepts the keeper's request format and delegates to process_assets_impl.
///
/// # Request
/// ```json
/// { "assets": [1, 2, 3], "batch_id": "batch-123" }
/// ```
///
/// # Response
/// ```json
/// { "status": "success", "assets_updated": 3, "batch_id": "batch-123", "timestamp": 1705123456 }
/// ```
pub async fn update_market_data<P>(
    State(state): State<AppState<P>>,
    Json(request): Json<UpdateMarketDataRequest>,
) -> Result<Json<UpdateMarketDataResponse>, (StatusCode, Json<ErrorResponse>)>
where
    P: alloy::providers::Provider + Clone + Send + Sync + 'static,
{
    let correlation_id = request.batch_id.clone();
    tracing::info!(
        "📥 Update market data request from Keeper [batch_id={}]: {} assets",
        correlation_id,
        request.assets.len()
    );

    // Validate request
    if request.assets.is_empty() {
        tracing::warn!("Empty assets in request [batch_id={}]", correlation_id);
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "assets cannot be empty".to_string(),
            }),
        ));
    }

    // Check if the endpoint is properly configured
    let (psl_service, vendor_submitter) = match (state.psl_service.clone(), state.vendor_submitter.clone()) {
        (Some(psl), Some(vs)) => (psl, vs),
        _ => {
            tracing::warn!(
                "Update-market-data endpoint not configured [batch_id={}]",
                correlation_id
            );
            return Err((
                StatusCode::NOT_IMPLEMENTED,
                Json(ErrorResponse {
                    error: "Update-market-data endpoint not configured. PSL service or VendorSubmitter missing.".to_string(),
                }),
            ));
        }
    };

    let margin_config = state.margin_config.clone().unwrap_or_default();
    let asset_ids = request.assets.clone();

    // Delegate to process_assets_impl
    let result = process_assets_impl(
        psl_service,
        vendor_submitter,
        margin_config,
        asset_ids,
        correlation_id.clone(),
    ).await;

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    match result {
        Ok(process_response) => {
            tracing::info!(
                "✅ Market data update complete [batch_id={}]: {} assets in {}ms",
                correlation_id,
                process_response.assets_processed,
                process_response.metrics.total_ms
            );
            Ok(Json(UpdateMarketDataResponse {
                status: "success".to_string(),
                assets_updated: process_response.assets_processed,
                batch_id: correlation_id,
                timestamp,
            }))
        }
        Err(error_msg) => {
            tracing::error!(
                "❌ Market data update failed [batch_id={}]: {}",
                correlation_id,
                error_msg
            );
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: error_msg }),
            ))
        }
    }
}