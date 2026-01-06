use super::types::{AssetsQuote, ErrorResponse, HealthResponse, QuoteAssetsRequest};
use crate::onchain::{AssetMapper, StalenessManager};
use crate::order_book::OrderBookProcessor;
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use parking_lot::RwLock;
use std::sync::Arc;

// This MUST match what ProviderBuilder::new().connect_http() returns in main.rs
pub type ConcreteProvider = alloy::providers::fillers::FillProvider<
    alloy::providers::fillers::JoinFill<
        alloy::providers::Identity,
        alloy::providers::fillers::JoinFill<
            alloy::providers::fillers::GasFiller,
            alloy::providers::fillers::JoinFill<
                alloy::providers::fillers::BlobGasFiller,
                alloy::providers::fillers::JoinFill<
                    alloy::providers::fillers::NonceFiller,
                    alloy::providers::fillers::ChainIdFiller,
                >,
            >,
        >,
    >,
    alloy::providers::RootProvider<alloy::network::Ethereum>,
    alloy::network::Ethereum,
>;

#[derive(Clone)]
pub struct AppState {
    pub vendor_id: u128,
    pub asset_mapper: Arc<RwLock<AssetMapper>>,
    pub staleness_manager: Arc<RwLock<StalenessManager<ConcreteProvider>>>,
    pub order_book_processor: Arc<OrderBookProcessor>,
}


/// Health check endpoint
pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    // Use spawn_blocking for parking_lot lock
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

/// Main quote_assets endpoint
pub async fn quote_assets(
    State(state): State<AppState>,
    Json(request): Json<QuoteAssetsRequest>,
) -> Result<Json<AssetsQuote>, (StatusCode, Json<ErrorResponse>)> {
    // Run everything in spawn_blocking since we use parking_lot locks
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

// Synchronous implementation
fn quote_assets_sync(
    state: AppState,
    request: QuoteAssetsRequest,
) -> Result<Json<AssetsQuote>, (StatusCode, Json<ErrorResponse>)> {
    tracing::info!("Received quote request for {} assets", request.assets.len());
    tracing::debug!("Requested assets: {:?}", request.assets);
    
    // Note: This is blocking, can't call async update_onchain_cache here
    // We'll need to handle this differently - skip on-chain update for now
    // or make it async differently
    
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