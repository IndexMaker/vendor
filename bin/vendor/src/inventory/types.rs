use chrono::{DateTime, Utc};
use common::amount::Amount;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrderSide {
    Buy,
    #[allow(dead_code)]
    Sell, // TODO: Not implemented yet
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
pub enum OrderStatus {
    Pending,
    Filled,
    PartiallyFilled,
    Rejected,
    Failed,
}

impl std::fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrderStatus::Pending => write!(f, "Pending"),
            OrderStatus::Filled => write!(f, "Filled"),
            OrderStatus::PartiallyFilled => write!(f, "PartiallyFilled"),
            OrderStatus::Rejected => write!(f, "Rejected"),
            OrderStatus::Failed => write!(f, "Failed"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub order_id: String,
    pub side: OrderSide,
    pub index_symbol: String,
    pub collateral_usd: Amount,  // Changed from quantity!
    pub client_id: String,
    pub status: OrderStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Order {
    pub fn new(
        order_id: String,
        side: OrderSide,
        index_symbol: String,
        collateral_usd: Amount,  // Changed
        client_id: String,
    ) -> Self {
        let now = Utc::now();
        Self {
            order_id,
            side,
            index_symbol,
            collateral_usd,  // Changed
            client_id,
            status: OrderStatus::Pending,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn mark_filled(&mut self) {
        self.status = OrderStatus::Filled;
        self.updated_at = Utc::now();
    }

    pub fn mark_rejected(&mut self) {
        self.status = OrderStatus::Rejected;
        self.updated_at = Utc::now();
    }

    pub fn mark_failed(&mut self) {
        self.status = OrderStatus::Failed;
        self.updated_at = Utc::now();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LotAssignment {
    pub address: String,
    pub assigned_fee: String,
    pub assigned_quantity: String,
    pub assigned_timestamp: DateTime<Utc>,
    pub chain_id: u64,
    pub client_order_id: String,
    pub seq_num: String,
    pub side: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lot {
    pub lot_id: String,
    pub original_order_id: String,
    pub original_batch_order_id: String,
    pub original_price: String,
    pub original_quantity: String,
    pub original_fee: String,
    pub remaining_quantity: String,
    pub created_timestamp: DateTime<Utc>,
    pub last_update_timestamp: DateTime<Utc>,
    pub lot_assignments: Vec<LotAssignment>,
    pub lot_transactions: Vec<serde_json::Value>, // Placeholder for now
}

impl Lot {
    // Mock lot for now - returns empty/placeholder data
    pub fn mock(order_id: &str, symbol: &str, quantity: Amount) -> Self {
        let now = Utc::now();
        Self {
            lot_id: format!("{}-L-0", order_id),
            original_order_id: order_id.to_string(),
            original_batch_order_id: format!("B-{}", now.timestamp()),
            original_price: "0.0".to_string(),
            original_quantity: quantity.to_string(),
            original_fee: "0.0".to_string(),
            remaining_quantity: quantity.to_string(),
            created_timestamp: now,
            last_update_timestamp: now,
            lot_assignments: vec![],
            lot_transactions: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub symbol: String,
    pub balance: String,
    pub side: String,
    pub created_timestamp: DateTime<Utc>,
    pub last_update_timestamp: DateTime<Utc>,
    pub open_lots: Vec<Lot>,
    pub closed_lots: Vec<Lot>,
}

impl Position {
    pub fn new(symbol: String, side: OrderSide) -> Self {
        let now = Utc::now();
        Self {
            symbol,
            balance: "0.0".to_string(),
            side: side.to_string(),
            created_timestamp: now,
            last_update_timestamp: now,
            open_lots: vec![],
            closed_lots: vec![],
        }
    }

    pub fn get_balance(&self) -> Amount {
        self.balance
            .parse::<f64>()
            .map(|v| Amount::from_u128_raw((v * 1e18) as u128))
            .unwrap_or(Amount::ZERO)
    }

    pub fn add_balance(&mut self, delta: Amount) {
        let current = self.get_balance();
        let new_balance = current.checked_add(delta).unwrap_or(current);
        self.balance = format!("{}", new_balance);
        self.last_update_timestamp = Utc::now();
    }

    pub fn add_lot(&mut self, lot: Lot) {
        self.open_lots.push(lot);
        self.last_update_timestamp = Utc::now();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexPosition {
    pub index_symbol: String,
    pub units_held: String,
    pub created_at: DateTime<Utc>,
    pub last_update_at: DateTime<Utc>,
}

impl IndexPosition {
    pub fn new(index_symbol: String) -> Self {
        let now = Utc::now();
        Self {
            index_symbol,
            units_held: "0.0".to_string(),
            created_at: now,
            last_update_at: now,
        }
    }

    pub fn get_units(&self) -> Amount {
        self.units_held
            .parse::<f64>()
            .map(|v| Amount::from_u128_raw((v * 1e18) as u128))
            .unwrap_or(Amount::ZERO)
    }

    pub fn add_units(&mut self, delta: Amount) {
        let current = self.get_units();
        let new_units = current.checked_add(delta).unwrap_or(current);
        self.units_held = format!("{}", new_units);
        self.last_update_at = Utc::now();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderResult {
    pub order_id: String,
    pub status: OrderStatus,
    pub message: String,
    pub index_position: Option<IndexPosition>,
    pub asset_positions: Vec<Position>,
    pub total_fees: Amount,
}

impl OrderResult {
    pub fn success(
        order_id: String,
        index_position: IndexPosition,
        asset_positions: Vec<Position>,
        total_fees: Amount,
    ) -> Self {
        Self {
            order_id,
            status: OrderStatus::Filled,
            message: "Order successfully filled".to_string(),
            index_position: Some(index_position),
            asset_positions,
            total_fees,
        }
    }

    pub fn rejected(order_id: String, reason: String) -> Self {
        Self {
            order_id,
            status: OrderStatus::Rejected,
            message: reason,
            index_position: None,
            asset_positions: vec![],
            total_fees: Amount::ZERO,
        }
    }

    // pub fn failed(order_id: String, error: String) -> Self {
    //     Self {
    //         order_id,
    //         status: OrderStatus::Failed,
    //         message: error,
    //         index_position: None,
    //         asset_positions: vec![],
    //     }
    // }
}