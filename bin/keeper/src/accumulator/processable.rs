//! ProcessableBatch - Batch ready for Vendor communication and settlement
//!
//! Separates buy/sell orders and provides unique index IDs for downstream processing.

use super::types::{FlushTrigger, OrderBatch};
use alloy_primitives::Address;
use chrono::{DateTime, Utc};
use common::amount::Amount;
use std::collections::HashMap;

/// A single buy order entry aggregated by index
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields used for tracing/debugging via Debug output
pub struct BuyOrderEntry {
    /// Index ID for this buy order
    pub index_id: u128,
    /// Total USD amount for all deposits to this index
    pub total_amount: Amount,
    /// Number of individual orders aggregated
    pub order_count: usize,
    /// Vault address for processPending calls
    pub vault_address: Option<Address>,
}

/// A single sell order entry aggregated by index
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields used for tracing/debugging via Debug output
pub struct SellOrderEntry {
    /// Index ID for this sell order
    pub index_id: u128,
    /// Total USD amount for all withdrawals from this index
    pub total_amount: Amount,
    /// Number of individual orders aggregated
    pub order_count: usize,
    /// Vault address for processPending calls
    pub vault_address: Option<Address>,
}

/// Individual order for settlement with trader address (Story 2.6)
/// Used for processPendingBuyOrder/processPendingSellOrder contract calls
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields used for tracing/debugging via Debug output
pub struct SettlementOrder {
    /// Index ID for this order
    pub index_id: u128,
    /// Trader address for processPending calls
    pub trader_address: Address,
    /// Order amount
    pub amount: Amount,
    /// Vault address for this order
    pub vault_address: Option<Address>,
}

/// Batch ready for Vendor communication and settlement
/// (AC: #2, #5 - separates buy/sell orders and provides unique index IDs)
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields used for tracing/debugging via Debug output
pub struct ProcessableBatch {
    /// Buy orders grouped by index
    pub buy_orders: Vec<BuyOrderEntry>,
    /// Sell orders grouped by index
    pub sell_orders: Vec<SellOrderEntry>,
    /// Unique index IDs for asset list extraction (AC: #2)
    pub unique_index_ids: Vec<u128>,
    /// Vault addresses for processPending calls (keyed by index_id)
    pub vault_addresses: HashMap<u128, Address>,
    /// When the batch was created
    pub created_at: DateTime<Utc>,
    /// When the batch was flushed
    pub flushed_at: DateTime<Utc>,
    /// What triggered the flush
    pub flush_trigger: FlushTrigger,
    /// Total order count across all indices
    pub total_orders: usize,
    /// Individual buy orders for settlement with trader addresses (Story 2.6)
    pub settlement_buy_orders: Vec<SettlementOrder>,
    /// Individual sell orders for settlement with trader addresses (Story 2.6)
    pub settlement_sell_orders: Vec<SettlementOrder>,
}

impl ProcessableBatch {
    /// Convert an OrderBatch into a ProcessableBatch (AC: #3, #5)
    /// Splits orders by type (Deposit → buy, Withdraw → sell)
    pub fn from_order_batch(batch: OrderBatch) -> Self {
        let mut buy_orders = Vec::new();
        let mut sell_orders = Vec::new();
        let mut unique_index_ids = Vec::new();
        let mut vault_addresses = HashMap::new();
        let mut total_orders = 0;
        let mut settlement_buy_orders = Vec::new();
        let mut settlement_sell_orders = Vec::new();

        for (index_id, state) in batch.indices.iter() {
            unique_index_ids.push(*index_id);
            total_orders += state.order_count;

            // Track vault address if available
            if let Some(vault_addr) = state.vault_address {
                vault_addresses.insert(*index_id, vault_addr);
            }

            // Deposits become buy orders
            if state.total_deposits > Amount::ZERO {
                buy_orders.push(BuyOrderEntry {
                    index_id: *index_id,
                    total_amount: state.total_deposits,
                    order_count: state.deposit_count, // Correctly counts only deposit orders
                    vault_address: state.vault_address,
                });
            }

            // Withdrawals become sell orders
            if state.total_withdrawals > Amount::ZERO {
                sell_orders.push(SellOrderEntry {
                    index_id: *index_id,
                    total_amount: state.total_withdrawals,
                    order_count: state.withdrawal_count, // Correctly counts only withdrawal orders
                    vault_address: state.vault_address,
                });
            }

            // Story 2.6: Preserve individual orders with trader addresses for settlement
            for order_info in &state.individual_orders {
                let settlement_order = SettlementOrder {
                    index_id: *index_id,
                    trader_address: order_info.trader_address,
                    amount: order_info.amount,
                    vault_address: state.vault_address,
                };
                if order_info.is_buy {
                    settlement_buy_orders.push(settlement_order);
                } else {
                    settlement_sell_orders.push(settlement_order);
                }
            }
        }

        // Sort unique_index_ids for deterministic ordering (architecture requirement)
        unique_index_ids.sort();

        Self {
            buy_orders,
            sell_orders,
            unique_index_ids,
            vault_addresses,
            created_at: batch.created_at,
            flushed_at: Utc::now(),
            flush_trigger: batch.flush_trigger.unwrap_or(FlushTrigger::Manual),
            total_orders,
            settlement_buy_orders,
            settlement_sell_orders,
        }
    }

    /// Check if batch is empty
    #[allow(dead_code)] // Used in tests
    pub fn is_empty(&self) -> bool {
        self.buy_orders.is_empty() && self.sell_orders.is_empty()
    }

    /// Get total buy order count
    #[allow(dead_code)] // Used in tests
    pub fn buy_count(&self) -> usize {
        self.buy_orders.len()
    }

    /// Get total sell order count
    #[allow(dead_code)] // Used in tests
    pub fn sell_count(&self) -> usize {
        self.sell_orders.len()
    }

    /// Get log summary for batch statistics (AC: #6)
    #[allow(dead_code)] // Used in tests
    pub fn log_summary(&self) -> String {
        format!(
            "{} orders, {} indices ({} buy, {} sell), trigger={}",
            self.total_orders,
            self.unique_index_ids.len(),
            self.buy_orders.len(),
            self.sell_orders.len(),
            self.flush_trigger.as_str()
        )
    }

    /// Generate correlation ID for Vendor communication (Story 2.5)
    /// Format: {flush_trigger}:keeper:{timestamp_ms}
    pub fn generate_correlation_id(&self) -> String {
        let trigger_str = match &self.flush_trigger {
            FlushTrigger::TimeWindow { .. } => "time_window",
            FlushTrigger::BlockChange { .. } => "block_change",
            FlushTrigger::MaxBatchSize { .. } => "max_batch_size",
            FlushTrigger::Manual => "manual",
        };
        format!("{}:keeper:{}", trigger_str, self.flushed_at.timestamp_millis())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accumulator::types::{IndexOrder, OrderAction, OrderBatch};
    use common::amount::Amount;

    fn make_deposit_order(index_id: u128, amount: u128) -> IndexOrder {
        IndexOrder {
            index_id,
            action: OrderAction::Deposit {
                user_address: "0xUser".to_string(),
                amount_usd: Amount::from_u128_with_scale(amount, 0),
            },
            timestamp: chrono::Utc::now(),
            vault_address: Some(Address::repeat_byte(1)),
            trader_address: Some(Address::repeat_byte(0xAB)),
            tx_hash: None,
            correlation_id: None,
        }
    }

    fn make_withdraw_order(index_id: u128, amount: u128) -> IndexOrder {
        IndexOrder {
            index_id,
            action: OrderAction::Withdraw {
                user_address: "0xUser".to_string(),
                amount_itp: Amount::from_u128_with_scale(amount, 0),
            },
            timestamp: chrono::Utc::now(),
            vault_address: Some(Address::repeat_byte(2)),
            trader_address: Some(Address::repeat_byte(0xCD)),
            tx_hash: None,
            correlation_id: None,
        }
    }

    #[test]
    fn test_processable_batch_separates_buy_sell() {
        let mut batch = OrderBatch::new(100);
        // Buy orders for index 1001
        batch.add_order(make_deposit_order(1001, 100));
        batch.add_order(make_deposit_order(1001, 50));
        // Withdraw from index 1002 - need deposit first to avoid underflow
        batch.add_order(make_deposit_order(1002, 200)); // Add deposit first
        batch.add_order(make_withdraw_order(1002, 75));

        let processable = ProcessableBatch::from_order_batch(batch);

        assert_eq!(processable.buy_orders.len(), 2); // Both indices have deposits
        assert_eq!(processable.sell_orders.len(), 1);
        assert_eq!(processable.unique_index_ids.len(), 2);

        // Check buy order for 1001
        let buy_1001 = processable.buy_orders.iter().find(|b| b.index_id == 1001).unwrap();
        assert_eq!(buy_1001.total_amount, Amount::from_u128_with_scale(150, 0));

        // Check buy order for 1002 (200)
        let buy_1002 = processable.buy_orders.iter().find(|b| b.index_id == 1002).unwrap();
        assert_eq!(buy_1002.total_amount, Amount::from_u128_with_scale(200, 0));

        // Check sell order for 1002 (75)
        let sell = &processable.sell_orders[0];
        assert_eq!(sell.index_id, 1002);
        assert_eq!(sell.total_amount, Amount::from_u128_with_scale(75, 0));
    }

    #[test]
    fn test_processable_batch_unique_index_ids_sorted() {
        let mut batch = OrderBatch::new(100);
        batch.add_order(make_deposit_order(3000, 100));
        batch.add_order(make_deposit_order(1000, 50));
        batch.add_order(make_deposit_order(2000, 75));

        let processable = ProcessableBatch::from_order_batch(batch);

        // Should be sorted in ascending order
        assert_eq!(processable.unique_index_ids, vec![1000, 2000, 3000]);
    }

    #[test]
    fn test_processable_batch_vault_addresses() {
        let mut batch = OrderBatch::new(100);
        batch.add_order(make_deposit_order(1001, 100));
        // Need to deposit before withdraw to avoid underflow
        batch.add_order(make_deposit_order(1002, 100));
        batch.add_order(make_withdraw_order(1002, 75));

        let processable = ProcessableBatch::from_order_batch(batch);

        assert!(processable.vault_addresses.contains_key(&1001));
        assert!(processable.vault_addresses.contains_key(&1002));
        assert_eq!(processable.vault_addresses.len(), 2);
    }

    #[test]
    fn test_processable_batch_log_summary() {
        let mut batch = OrderBatch::new(100);
        batch.add_order(make_deposit_order(1001, 100));
        // Need to deposit before withdraw to avoid underflow
        batch.add_order(make_deposit_order(1002, 100));
        batch.add_order(make_withdraw_order(1002, 75));
        batch.flush_trigger = Some(FlushTrigger::TimeWindow { age_ms: 100 });

        let processable = ProcessableBatch::from_order_batch(batch);

        let summary = processable.log_summary();
        assert!(summary.contains("3 orders")); // 3 orders total now
        assert!(summary.contains("2 indices"));
        assert!(summary.contains("2 buy")); // Both indices have deposits
        assert!(summary.contains("1 sell"));
        assert!(summary.contains("time-based"));
    }

    #[test]
    fn test_empty_batch() {
        let batch = OrderBatch::new(100);
        let processable = ProcessableBatch::from_order_batch(batch);

        assert!(processable.is_empty());
        assert_eq!(processable.buy_count(), 0);
        assert_eq!(processable.sell_count(), 0);
    }

    // =========================================================================
    // Story 2.6: Settlement order tests - verify trader addresses preserved
    // =========================================================================

    #[test]
    fn test_settlement_orders_preserve_trader_addresses() {
        let mut batch = OrderBatch::new(100);
        batch.add_order(make_deposit_order(1001, 100));
        batch.add_order(make_deposit_order(1002, 200));
        batch.add_order(make_withdraw_order(1002, 75));

        let processable = ProcessableBatch::from_order_batch(batch);

        // Should have 2 settlement buy orders and 1 settlement sell order
        assert_eq!(processable.settlement_buy_orders.len(), 2);
        assert_eq!(processable.settlement_sell_orders.len(), 1);

        // Verify trader addresses are preserved (not Address::ZERO)
        for buy_order in &processable.settlement_buy_orders {
            assert_eq!(buy_order.trader_address, Address::repeat_byte(0xAB));
            assert_ne!(buy_order.trader_address, Address::ZERO);
        }

        for sell_order in &processable.settlement_sell_orders {
            assert_eq!(sell_order.trader_address, Address::repeat_byte(0xCD));
            assert_ne!(sell_order.trader_address, Address::ZERO);
        }
    }

    #[test]
    fn test_settlement_orders_include_vault_addresses() {
        let mut batch = OrderBatch::new(100);
        batch.add_order(make_deposit_order(1001, 100));

        let processable = ProcessableBatch::from_order_batch(batch);

        assert_eq!(processable.settlement_buy_orders.len(), 1);
        let order = &processable.settlement_buy_orders[0];
        assert_eq!(order.vault_address, Some(Address::repeat_byte(1)));
    }
}
