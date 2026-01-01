use common::amount::Amount;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrderSide {
    Buy,
    Sell,
}

impl std::fmt::Display for OrderSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrderSide::Buy => write!(f, "Buy"),
            OrderSide::Sell => write!(f, "Sell"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrderType {
    Market,
    Limit,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrderStatus {
    Filled,
    PartiallyFilled,
    Pending,
    Rejected,
    Failed,
    Cancelled,
}

impl std::fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrderStatus::Filled => write!(f, "Filled"),
            OrderStatus::PartiallyFilled => write!(f, "PartiallyFilled"),
            OrderStatus::Pending => write!(f, "Pending"),
            OrderStatus::Rejected => write!(f, "Rejected"),
            OrderStatus::Failed => write!(f, "Failed"),
            OrderStatus::Cancelled => write!(f, "Cancelled"),
        }
    }
}

/// Represents a single asset order to be executed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetOrder {
    pub symbol: String,
    pub side: OrderSide,
    pub quantity: Amount,
    pub order_type: OrderType,
    pub limit_price: Option<Amount>,
}

impl AssetOrder {
    pub fn market(symbol: String, side: OrderSide, quantity: Amount) -> Self {
        Self {
            symbol,
            side,
            quantity,
            order_type: OrderType::Market,
            limit_price: None,
        }
    }

    pub fn limit(symbol: String, side: OrderSide, quantity: Amount, limit_price: Amount) -> Self {
        Self {
            symbol,
            side,
            quantity,
            order_type: OrderType::Limit,
            limit_price: Some(limit_price),
        }
    }
}

/// Result of executing an order
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub symbol: String,
    pub order_id: String,
    pub filled_quantity: Amount,
    pub avg_price: Amount,
    pub fees: Amount,
    pub status: OrderStatus,
    pub error_message: Option<String>,
}

impl ExecutionResult {
    pub fn success(
        symbol: String,
        order_id: String,
        filled_quantity: Amount,
        avg_price: Amount,
        fees: Amount,
    ) -> Self {
        Self {
            symbol,
            order_id,
            filled_quantity,
            avg_price,
            fees,
            status: OrderStatus::Filled,
            error_message: None,
        }
    }

    pub fn failed(symbol: String, order_id: String, error: String) -> Self {
        Self {
            symbol,
            order_id,
            filled_quantity: Amount::ZERO,
            avg_price: Amount::ZERO,
            fees: Amount::ZERO,
            status: OrderStatus::Failed,
            error_message: Some(error),
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(self.status, OrderStatus::Filled | OrderStatus::PartiallyFilled)
    }
}