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
pub async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        timestamp: Utc::now().to_rfc3339(),
    })
}

/// Create order endpoint
pub async fn create_order(
    State(state): State<AppState>,
    Json(request): Json<CreateOrderRequest>,
) -> (StatusCode, Json<CreateOrderResponse>) {
    tracing::info!(
        "Received order request: {} {} of {}",
        request.side,
        request.quantity,
        request.index_symbol
    );

    // Validate and parse quantity
    let quantity = match request.parse_quantity() {
        Ok(qty) => qty,
        Err(e) => {
            tracing::warn!("Invalid quantity in request: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                Json(CreateOrderResponse::error("".to_string(), e)),
            );
        }
    };

    // Generate order ID
    let order_id = format!("O-{}", Utc::now().timestamp_millis());

    // Create order
    let order = Order::new(
        order_id.clone(),
        request.side,
        request.index_symbol.clone(),
        quantity,
        request.client_id.clone(),
    );

    // Process order - tokio RwLock can be held across await
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

    tracing::info!("Order {} processed successfully: {:?}", order_id, result.status);

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