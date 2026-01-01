use crate::inventory::{OrderResult, OrderSide};
use common::amount::Amount;
use serde::{Deserialize, Serialize};

/// Request to create a new order
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateOrderRequest {
    pub side: OrderSide,
    pub index_symbol: String,
    pub collateral_usd: String,  // Changed from quantity!
    pub client_id: String,
}

impl CreateOrderRequest {
    pub fn parse_collateral(&self) -> Result<Amount, String> {  // Renamed
        let collateral_f64: f64 = self
            .collateral_usd
            .parse()
            .map_err(|e| format!("Invalid collateral: {}", e))?;

        if collateral_f64 <= 0.0 {
            return Err("Collateral must be positive".to_string());
        }

        Ok(Amount::from_u128_raw((collateral_f64 * 1e18) as u128))
    }
}

/// Response for order creation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateOrderResponse {
    pub success: bool,
    pub order_id: String,
    pub status: String,
    pub message: String,
    pub result: Option<OrderResult>,
}

impl CreateOrderResponse {
    pub fn success(order_id: String, result: OrderResult) -> Self {
        Self {
            success: true,
            order_id: order_id.clone(),
            status: format!("{:?}", result.status),
            message: result.message.clone(),
            result: Some(result),
        }
    }

    pub fn error(order_id: String, message: String) -> Self {
        Self {
            success: false,
            order_id,
            status: "Failed".to_string(),
            message,
            result: None,
        }
    }
}

/// Health check response
#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub timestamp: String,
}