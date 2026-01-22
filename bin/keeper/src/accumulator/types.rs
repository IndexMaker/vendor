use alloy_primitives::{Address, B256};
use chrono::{DateTime, Utc};
use common::amount::Amount;
use std::collections::HashMap;

/// What triggered a batch flush
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)] // MaxBatchSize reserved for future use
pub enum FlushTrigger {
    /// Time window exceeded (100ms default)
    TimeWindow { age_ms: i64 },
    /// New block detected since batch started
    BlockChange { start_block: u64, current_block: u64 },
    /// Maximum batch size reached
    MaxBatchSize { count: usize },
    /// Manual flush requested
    Manual,
}

impl FlushTrigger {
    /// Get a human-readable description of the trigger
    pub fn as_str(&self) -> &'static str {
        match self {
            FlushTrigger::TimeWindow { .. } => "time-based",
            FlushTrigger::BlockChange { .. } => "block-based",
            FlushTrigger::MaxBatchSize { .. } => "max-size",
            FlushTrigger::Manual => "manual",
        }
    }
}

/// Represents a user action (deposit/withdrawal)
#[derive(Debug, Clone)]
#[allow(dead_code)] // user_address fields used for tracing/debugging
pub enum OrderAction {
    Deposit {
        user_address: String,
        /// Collateral amount in USD (USDC, 6 decimals)
        amount_usd: Amount,
    },
    Withdraw {
        user_address: String,
        /// Amount in ITP tokens (18 decimals) - NOT USD
        /// Downstream processors must handle ITP→USD conversion
        amount_itp: Amount,
    },
}

/// A single order for an index
#[derive(Debug, Clone)]
#[allow(dead_code)] // tx_hash field used for tracing/debugging
pub struct IndexOrder {
    pub index_id: u128,
    pub action: OrderAction,
    pub timestamp: DateTime<Utc>,
    /// Source vault contract address (for processPending calls)
    pub vault_address: Option<Address>,
    /// Trader address for processPending calls (Story 2.6)
    /// This is the actual Address type needed for contract calls
    pub trader_address: Option<Address>,
    /// Original transaction hash for correlation
    pub tx_hash: Option<B256>,
    /// Correlation ID for end-to-end tracing (AC: Task 5)
    pub correlation_id: Option<String>,
}

impl IndexOrder {
    /// Get correlation ID (tx_hash if available, otherwise correlation_id)
    #[allow(dead_code)] // Reserved for future tracing
    pub fn get_correlation_id(&self) -> String {
        if let Some(hash) = &self.tx_hash {
            format!("tx:{}", hash)
        } else if let Some(id) = &self.correlation_id {
            id.clone()
        } else {
            format!("idx:{}-{}", self.index_id, self.timestamp.timestamp_millis())
        }
    }
}

/// Individual order info for settlement (Story 2.6)
/// Preserves trader address for processPending* contract calls
#[derive(Debug, Clone)]
pub struct IndividualOrderInfo {
    /// Trader address for processPending calls
    pub trader_address: Address,
    /// Order amount (USD for deposits, ITP for withdrawals)
    pub amount: Amount,
    /// Whether this is a buy (deposit) or sell (withdraw) order
    pub is_buy: bool,
}

/// Aggregated state for a single index
#[derive(Debug, Clone)]
pub struct IndexState {
    pub index_id: u128,
    pub net_collateral_change: Amount, // Positive = deposits, Negative = withdrawals
    pub total_deposits: Amount,
    pub total_withdrawals: Amount,
    pub order_count: usize,
    /// Count of deposit orders only (for accurate ProcessableBatch reporting)
    pub deposit_count: usize,
    /// Count of withdrawal orders only (for accurate ProcessableBatch reporting)
    pub withdrawal_count: usize,
    pub first_order_time: DateTime<Utc>,
    pub last_order_time: DateTime<Utc>,
    /// Vault address for this index (for processPending calls)
    pub vault_address: Option<Address>,
    /// Individual orders for settlement (Story 2.6)
    /// Preserves trader addresses for processPending* calls
    pub individual_orders: Vec<IndividualOrderInfo>,
}

impl IndexState {
    pub fn new(index_id: u128) -> Self {
        Self {
            index_id,
            net_collateral_change: Amount::ZERO,
            total_deposits: Amount::ZERO,
            total_withdrawals: Amount::ZERO,
            order_count: 0,
            deposit_count: 0,
            withdrawal_count: 0,
            first_order_time: Utc::now(),
            last_order_time: Utc::now(),
            vault_address: None,
            individual_orders: Vec::new(),
        }
    }

    /// Add a deposit to this index
    pub fn add_deposit(&mut self, amount: Amount) {
        self.total_deposits = self.total_deposits.checked_add(amount).unwrap();
        self.net_collateral_change = self.net_collateral_change.checked_add(amount).unwrap();
        self.order_count += 1;
        self.deposit_count += 1;
        self.last_order_time = Utc::now();
    }

    /// Add a withdrawal to this index
    pub fn add_withdrawal(&mut self, amount: Amount) {
        self.total_withdrawals = self.total_withdrawals.checked_add(amount).unwrap();
        // Handle underflow gracefully: if withdrawal exceeds deposits, set net change to ZERO
        // (can happen if withdrawal event arrives before deposit is recorded)
        // The true values are preserved in total_deposits/total_withdrawals separately
        self.net_collateral_change = self
            .net_collateral_change
            .checked_sub(amount)
            .unwrap_or(Amount::ZERO);
        self.order_count += 1;
        self.withdrawal_count += 1;
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
    /// Block number when batch was created
    pub start_block: u64,
    /// Current block number (updated by block tracker)
    pub current_block: u64,
    /// What triggered the last flush check (set when should_flush returns true)
    pub flush_trigger: Option<FlushTrigger>,
}

impl OrderBatch {
    pub fn new(batch_window_ms: u64) -> Self {
        Self {
            indices: HashMap::new(),
            batch_window_ms,
            created_at: Utc::now(),
            start_block: 0,
            current_block: 0,
            flush_trigger: None,
        }
    }

    /// Create a new batch with block tracking
    pub fn new_with_block(batch_window_ms: u64, current_block: u64) -> Self {
        Self {
            indices: HashMap::new(),
            batch_window_ms,
            created_at: Utc::now(),
            start_block: current_block,
            current_block,
            flush_trigger: None,
        }
    }

    /// Update current block number for block-based flush detection
    #[allow(dead_code)] // Used in tests
    pub fn update_block(&mut self, block_number: u64) {
        self.current_block = block_number;
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

        // Track vault address if provided
        if let Some(vault_addr) = order.vault_address {
            state.vault_address = Some(vault_addr);
        }

        // Store individual order info for settlement (Story 2.6)
        // Use trader_address if available, otherwise fall back to Address::ZERO
        let trader = match order.trader_address {
            Some(addr) => addr,
            None => {
                tracing::warn!(
                    index_id = order.index_id,
                    correlation_id = ?order.correlation_id,
                    "Order missing trader_address - settlement will fail with Address::ZERO"
                );
                Address::ZERO
            }
        };

        match order.action {
            OrderAction::Deposit { amount_usd, .. } => {
                state.add_deposit(amount_usd);
                state.individual_orders.push(IndividualOrderInfo {
                    trader_address: trader,
                    amount: amount_usd,
                    is_buy: true,
                });
            }
            OrderAction::Withdraw { amount_itp, .. } => {
                state.add_withdrawal(amount_itp);
                state.individual_orders.push(IndividualOrderInfo {
                    trader_address: trader,
                    amount: amount_itp,
                    is_buy: false,
                });
            }
        }
    }

    /// Check if batch is ready to flush (oldest order exceeds window OR block changed)
    pub fn should_flush(&self) -> bool {
        self.get_flush_trigger().is_some()
    }

    /// Get the reason for flushing, if any
    /// Returns None if batch should not be flushed
    pub fn get_flush_trigger(&self) -> Option<FlushTrigger> {
        // First check block-based trigger (new block = immediate flush)
        if self.current_block > self.start_block && self.start_block > 0 {
            return Some(FlushTrigger::BlockChange {
                start_block: self.start_block,
                current_block: self.current_block,
            });
        }

        // Then check time-based trigger
        for state in self.indices.values() {
            let age = state.age_ms();
            if age >= self.batch_window_ms as i64 {
                return Some(FlushTrigger::TimeWindow { age_ms: age });
            }
        }

        None
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