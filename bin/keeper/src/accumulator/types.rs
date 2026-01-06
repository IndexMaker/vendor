use chrono::{DateTime, Utc};
use common::amount::Amount;
use std::collections::HashMap;

/// Represents a user action (deposit/withdrawal)
#[derive(Debug, Clone)]
pub enum OrderAction {
    Deposit {
        user_address: String,
        amount_usd: Amount,
    },
    Withdraw {
        user_address: String,
        amount_usd: Amount,
    },
}

/// A single order for an index
#[derive(Debug, Clone)]
pub struct IndexOrder {
    pub index_id: u128,
    pub action: OrderAction,
    pub timestamp: DateTime<Utc>,
}

/// Aggregated state for a single index
#[derive(Debug, Clone)]
pub struct IndexState {
    pub index_id: u128,
    pub net_collateral_change: Amount, // Positive = deposits, Negative = withdrawals
    pub total_deposits: Amount,
    pub total_withdrawals: Amount,
    pub order_count: usize,
    pub first_order_time: DateTime<Utc>,
    pub last_order_time: DateTime<Utc>,
}

impl IndexState {
    pub fn new(index_id: u128) -> Self {
        Self {
            index_id,
            net_collateral_change: Amount::ZERO,
            total_deposits: Amount::ZERO,
            total_withdrawals: Amount::ZERO,
            order_count: 0,
            first_order_time: Utc::now(),
            last_order_time: Utc::now(),
        }
    }

    /// Add a deposit to this index
    pub fn add_deposit(&mut self, amount: Amount) {
        self.total_deposits = self.total_deposits.checked_add(amount).unwrap();
        self.net_collateral_change = self.net_collateral_change.checked_add(amount).unwrap();
        self.order_count += 1;
        self.last_order_time = Utc::now();
    }

    /// Add a withdrawal to this index
    pub fn add_withdrawal(&mut self, amount: Amount) {
        self.total_withdrawals = self.total_withdrawals.checked_add(amount).unwrap();
        self.net_collateral_change = self.net_collateral_change.checked_sub(amount).unwrap();
        self.order_count += 1;
        self.last_order_time = Utc::now();
    }

    /// Check if this index needs processing (has orders)
    pub fn needs_processing(&self) -> bool {
        self.order_count > 0
    }

    /// Get the age of the oldest order in milliseconds
    pub fn age_ms(&self) -> i64 {
        Utc::now()
            .signed_duration_since(self.first_order_time)
            .num_milliseconds()
    }
}

/// Batch of orders ready for processing
#[derive(Debug, Clone)]
pub struct OrderBatch {
    pub indices: HashMap<u128, IndexState>,
    pub batch_window_ms: u64,
    pub created_at: DateTime<Utc>,
}

impl OrderBatch {
    pub fn new(batch_window_ms: u64) -> Self {
        Self {
            indices: HashMap::new(),
            batch_window_ms,
            created_at: Utc::now(),
        }
    }

    /// Add an order to the batch
    pub fn add_order(&mut self, order: IndexOrder) {
        let state = self
            .indices
            .entry(order.index_id)
            .or_insert_with(|| IndexState::new(order.index_id));

        if state.order_count == 0 {
            state.first_order_time = order.timestamp;
        }

        match order.action {
            OrderAction::Deposit { amount_usd, .. } => {
                state.add_deposit(amount_usd);
            }
            OrderAction::Withdraw { amount_usd, .. } => {
                state.add_withdrawal(amount_usd);
            }
        }
    }

    /// Check if batch is ready to flush (oldest order exceeds window)
    pub fn should_flush(&self) -> bool {
        for state in self.indices.values() {
            if state.age_ms() >= self.batch_window_ms as i64 {
                return true;
            }
        }
        false
    }

    /// Get indices that need processing
    pub fn get_active_indices(&self) -> Vec<u128> {
        self.indices
            .values()
            .filter(|state| state.needs_processing())
            .map(|state| state.index_id)
            .collect()
    }

    /// Check if batch is empty
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Get total order count across all indices
    pub fn total_order_count(&self) -> usize {
        self.indices.values().map(|s| s.order_count).sum()
    }
}