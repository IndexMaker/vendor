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
    pub fees: Amount,                    // Total fee in USD equivalent
    pub fee_detail: Option<FeeDetail>,   // Add this - detailed fee breakdown
    pub status: OrderStatus,
    pub error_message: Option<String>,
}

impl ExecutionResult {
    pub fn success(
        order_id: String,
        symbol: String,
        filled_quantity: Amount,
        avg_price: Amount,
        fees: Amount,
        fee_detail: Option<FeeDetail>,
    ) -> Self {
        Self {
            symbol,
            order_id,
            filled_quantity,
            avg_price,
            fees,
            fee_detail,
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
            fee_detail: None,
            status: OrderStatus::Failed,
            error_message: Some(error),
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(self.status, OrderStatus::Filled | OrderStatus::PartiallyFilled)
    }

    /// Calculate fee as percentage of notional value
    pub fn fee_percentage(&self) -> f64 {
        if self.filled_quantity == Amount::ZERO || self.avg_price == Amount::ZERO {
            return 0.0;
        }

        let notional = self
            .filled_quantity
            .checked_mul(self.avg_price)
            .unwrap_or(Amount::ZERO);

        if notional == Amount::ZERO {
            return 0.0;
        }

        let fee_f64 = self.fees.to_u128_raw() as f64;
        let notional_f64 = notional.to_u128_raw() as f64;

        (fee_f64 / notional_f64) * 100.0
    }
}

/// Fee breakdown for an order
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeDetail {
    pub amount: Amount,
    pub currency: String,
    pub fee_type: FeeType,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FeeType {
    Maker,  // Provide liquidity (limit order that sits in orderbook)
    Taker,  // Take liquidity (market order or limit that executes immediately)
    Unknown,
}

impl std::fmt::Display for FeeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeeType::Maker => write!(f, "Maker"),
            FeeType::Taker => write!(f, "Taker"),
            FeeType::Unknown => write!(f, "Unknown"),
        }
    }
}

impl FeeDetail {
    pub fn new(amount: Amount, currency: String, fee_type: FeeType) -> Self {
        Self {
            amount,
            currency,
            fee_type,
        }
    }

    pub fn zero(currency: String) -> Self {
        Self {
            amount: Amount::ZERO,
            currency,
            fee_type: FeeType::Unknown,
        }
    }
}