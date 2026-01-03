use super::types::{CreateOrderRequest, CreateOrderResponse, HealthResponse};
use crate::inventory::{InventoryManager, Order};
use axum::{
    extract::State,
    http::StatusCode,
    Json,
};
use chrono::Utc;
use std::sync::Arc;
use tokio::sync::RwLock; // Changed to tokio::sync::RwLock

/// Shared application state
#[derive(Clone)]
pub struct AppState {
    pub inventory: Arc<RwLock<InventoryManager>>,
}

/// Health check endpoint
pub async fn health_check(State(state): State<AppState>) -> Json<serde_json::Value> {
    let inventory = state.inventory.read().await;

    Json(serde_json::json!({
        "status": "healthy",
        "version": env!("CARGO_PKG_VERSION"),
        "positions_count": inventory.position_count(),
        "total_fees_paid": format!("{:.4}", inventory.get_total_fees().to_u128_raw() as f64 / 1e18),
        "timestamp": chrono::Utc::now().to_rfc3339(),
    }))
}

/// Create order endpoint
pub async fn create_order(
    State(state): State<AppState>,
    Json(request): Json<CreateOrderRequest>,
) -> (StatusCode, Json<CreateOrderResponse>) {
    tracing::info!(
        "Received order request: {} ${} for {}",  // Changed log
        request.side,
        request.collateral_usd,
        request.index_symbol
    );

    // Validate and parse collateral (renamed)
    let collateral = match request.parse_collateral() {
        Ok(col) => col,
        Err(e) => {
            tracing::warn!("Invalid collateral in request: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                Json(CreateOrderResponse::error("".to_string(), e)),
            );
        }
    };

    // Generate order ID
    let order_id = format!("O-{}", Utc::now().timestamp_millis());

    // Create order (with collateral)
    let order = Order::new(
        order_id.clone(),
        request.side,
        request.index_symbol.clone(),
        collateral,  // Changed
        request.client_id.clone(),
    );

    // Process order
    let result = {
        let mut inventory = state.inventory.write().await;
        match inventory.process_buy_order(order).await {
            Ok(result) => result,
            Err(e) => {
                tracing::error!("Failed to process order {}: {:?}", order_id, e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(CreateOrderResponse::error(
                        order_id,
                        format!("Failed to process order: {}", e),
                    )),
                );
            }
        }
    };

    tracing::info!(
        "Order {} processed: {} (fees: ${})",
        order_id,
        result.status,
        result.total_fees.to_u128_raw() as f64 / 1e18
    );

    (
        StatusCode::OK,
        Json(CreateOrderResponse::success(order_id, result)),
    )
}

/// Get inventory summary endpoint
pub async fn get_inventory_summary(State(state): State<AppState>) -> Json<serde_json::Value> {
    let inventory = state.inventory.read().await;

    let positions: Vec<_> = inventory
        .get_all_positions()
        .iter()
        .map(|(symbol, pos)| {
            serde_json::json!({
                "symbol": symbol,
                "balance": pos.balance,
                "side": pos.side,
            })
        })
        .collect();

    let index_positions: Vec<_> = inventory
        .get_all_index_positions()
        .iter()
        .map(|(symbol, pos)| {
            serde_json::json!({
                "index_symbol": symbol,
                "units_held": pos.units_held,
            })
        })
        .collect();

    Json(serde_json::json!({
        "asset_positions": positions,
        "index_positions": index_positions,
        "summary": inventory.summary(),
    }))
}

/// Get fee statistics
pub async fn get_fee_stats(State(state): State<AppState>) -> Json<serde_json::Value> {
    let inventory = state.inventory.read().await;
    let total_fees = inventory.get_total_fees();

    Json(serde_json::json!({
        "total_fees_usd": format!("{:.2}", total_fees.to_u128_raw() as f64 / 1e18),
        "currency": "USD"
    }))
}