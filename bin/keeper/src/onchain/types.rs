use alloy::primitives::Address;
use common::amount::Amount;
use std::fmt;

/// Order type for settlement processing
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderType {
    Buy,
    Sell,
}

impl fmt::Display for OrderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderType::Buy => write!(f, "Buy"),
            OrderType::Sell => write!(f, "Sell"),
        }
    }
}

/// A pending order for settlement (Story 2.6)
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields used for tracing/debugging via Debug output
pub struct PendingOrder {
    /// Index ID for this order
    pub index_id: u128,
    /// Trader/user address who placed the order
    pub trader_address: Address,
    /// Maximum order size for processPending calls
    pub max_order_size: u128,
    /// Type of order (Buy or Sell)
    pub order_type: OrderType,
    /// Vault address for this order (optional, for tracing)
    pub vault_address: Option<Address>,
}

/// Payload for Castle/Vault settlement (Story 2.6)
///
/// Contains all data needed to settle a batch after Vendor HTTP 200:
/// 1. Index IDs for updateMultipleIndexQuotes
/// 2. Buy orders for processPendingBuyOrder
/// 3. Sell orders for processPendingSellOrder
#[derive(Debug, Clone)]
pub struct SettlementPayload {
    /// Unique index IDs requiring quote updates (from ProcessableBatch)
    pub index_ids: Vec<u128>,
    /// Buy orders to process (from ProcessableBatch.buy_orders)
    pub buy_orders: Vec<PendingOrder>,
    /// Sell orders to process (from ProcessableBatch.sell_orders)
    pub sell_orders: Vec<PendingOrder>,
    /// Correlation ID from Vendor communication (Story 2.5)
    pub batch_id: String,
}

impl SettlementPayload {
    /// Total order count
    #[allow(dead_code)] // Used for logging/monitoring
    pub fn total_orders(&self) -> usize {
        self.buy_orders.len() + self.sell_orders.len()
    }
}

/// Result of Castle/Vault settlement (Story 2.6)
#[derive(Debug, Clone)]
pub struct SettlementResult {
    /// Receipt for updateMultipleIndexQuotes transaction (if any indices)
    pub quote_update_tx: Option<TxReceipt>,
    /// Results for each buy order: (index_id, result)
    pub buy_order_results: Vec<(u128, Result<TxReceipt, String>)>,
    /// Results for each sell order: (index_id, result)
    pub sell_order_results: Vec<(u128, Result<TxReceipt, String>)>,
    /// Number of successfully processed orders
    pub total_succeeded: usize,
    /// Number of failed orders
    pub total_failed: usize,
    /// Correlation ID for tracing
    pub batch_id: String,
    /// Time taken for settlement in milliseconds
    pub elapsed_ms: u128,
}

impl SettlementResult {
    /// Check if settlement was fully successful (no failures)
    #[allow(dead_code)] // Used in tests
    pub fn is_fully_successful(&self) -> bool {
        self.total_failed == 0
    }

    /// Check if quote update succeeded
    #[allow(dead_code)] // Used in tests
    pub fn quote_update_succeeded(&self) -> bool {
        self.quote_update_tx.as_ref().is_some_and(|tx| tx.status)
    }
}

impl fmt::Display for SettlementResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Settlement[batch_id={}, succeeded={}, failed={}, elapsed_ms={}]",
            self.batch_id, self.total_succeeded, self.total_failed, self.elapsed_ms
        )?;

        if let Some(ref tx) = self.quote_update_tx {
            write!(f, " quote_tx={}", tx.tx_hash)?;
        }

        if self.total_failed > 0 {
            write!(f, " (PARTIAL FAILURE)")?;
        }

        Ok(())
    }
}

/// Transaction receipt information
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields used for tracing/logging via Debug output
pub struct TxReceipt {
    pub tx_hash: String,
    pub block_number: u64,
    pub gas_used: u128,
    pub status: bool,
}

/// Submission result
#[derive(Debug, Clone)]
pub enum SubmissionResult {
    Success {
        market_data_tx: Option<TxReceipt>,
        buy_order_txs: Vec<(u128, TxReceipt)>, // (index_id, receipt)
    },
    DryRun {
        would_submit: SubmissionSummary,
    },
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields used for tracing/logging via Debug output
pub struct SubmissionSummary {
    pub market_data_assets: usize,
    pub buy_orders_count: usize,
    pub total_collateral: Amount,
}

/// Gas configuration
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields used for configuration even if not all are actively read
pub struct GasConfig {
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    pub gas_limit_multiplier: f64,
}

impl Default for GasConfig {
    fn default() -> Self {
        Self {
            max_fee_per_gas: 50_000_000_000, // 50 gwei
            max_priority_fee_per_gas: 2_000_000_000, // 2 gwei
            gas_limit_multiplier: 1.2, // 20% buffer
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // Story 2.6 Unit Tests - Task 6
    // =========================================================================

    fn make_test_pending_order(index_id: u128, order_type: OrderType) -> PendingOrder {
        PendingOrder {
            index_id,
            trader_address: Address::repeat_byte(1),
            max_order_size: 1000,
            order_type,
            vault_address: Some(Address::repeat_byte(2)),
        }
    }

    fn make_test_tx_receipt(index_id: u128, status: bool) -> TxReceipt {
        TxReceipt {
            tx_hash: format!("0x{:064x}", index_id),
            block_number: 12345,
            gas_used: 21000,
            status,
        }
    }

    // --- Test 6.4: SettlementPayload tests ---

    #[test]
    fn test_settlement_payload_is_empty() {
        let payload = SettlementPayload {
            index_ids: vec![],
            buy_orders: vec![],
            sell_orders: vec![],
            batch_id: "test-batch".to_string(),
        };

        assert!(payload.is_empty());
        assert_eq!(payload.total_orders(), 0);
    }

    #[test]
    fn test_settlement_payload_with_orders() {
        let payload = SettlementPayload {
            index_ids: vec![1001, 1002],
            buy_orders: vec![make_test_pending_order(1001, OrderType::Buy)],
            sell_orders: vec![make_test_pending_order(1002, OrderType::Sell)],
            batch_id: "test-batch".to_string(),
        };

        assert!(!payload.is_empty());
        assert_eq!(payload.total_orders(), 2);
    }

    #[test]
    fn test_settlement_payload_buy_only() {
        let payload = SettlementPayload {
            index_ids: vec![1001],
            buy_orders: vec![
                make_test_pending_order(1001, OrderType::Buy),
                make_test_pending_order(1002, OrderType::Buy),
            ],
            sell_orders: vec![],
            batch_id: "test-batch".to_string(),
        };

        assert!(!payload.is_empty());
        assert_eq!(payload.total_orders(), 2);
    }

    #[test]
    fn test_settlement_payload_sell_only() {
        let payload = SettlementPayload {
            index_ids: vec![1001],
            buy_orders: vec![],
            sell_orders: vec![make_test_pending_order(1001, OrderType::Sell)],
            batch_id: "test-batch".to_string(),
        };

        assert!(!payload.is_empty());
        assert_eq!(payload.total_orders(), 1);
    }

    // --- Test 6.5: SettlementResult tests ---

    #[test]
    fn test_settlement_result_fully_successful() {
        let result = SettlementResult {
            quote_update_tx: Some(make_test_tx_receipt(0, true)),
            buy_order_results: vec![
                (1001, Ok(make_test_tx_receipt(1001, true))),
                (1002, Ok(make_test_tx_receipt(1002, true))),
            ],
            sell_order_results: vec![(1003, Ok(make_test_tx_receipt(1003, true)))],
            total_succeeded: 3,
            total_failed: 0,
            batch_id: "test-batch".to_string(),
            elapsed_ms: 1500,
        };

        assert!(result.is_fully_successful());
        assert!(result.quote_update_succeeded());
    }

    #[test]
    fn test_settlement_result_partial_failure() {
        let result = SettlementResult {
            quote_update_tx: Some(make_test_tx_receipt(0, true)),
            buy_order_results: vec![
                (1001, Ok(make_test_tx_receipt(1001, true))),
                (1002, Err("Transaction reverted".to_string())),
            ],
            sell_order_results: vec![(1003, Ok(make_test_tx_receipt(1003, true)))],
            total_succeeded: 2,
            total_failed: 1,
            batch_id: "test-batch".to_string(),
            elapsed_ms: 2500,
        };

        assert!(!result.is_fully_successful());
        assert!(result.quote_update_succeeded());
    }

    #[test]
    fn test_settlement_result_quote_update_failed() {
        let result = SettlementResult {
            quote_update_tx: Some(make_test_tx_receipt(0, false)),
            buy_order_results: vec![(1001, Ok(make_test_tx_receipt(1001, true)))],
            sell_order_results: vec![],
            total_succeeded: 1,
            total_failed: 0,
            batch_id: "test-batch".to_string(),
            elapsed_ms: 1000,
        };

        assert!(!result.quote_update_succeeded());
    }

    #[test]
    fn test_settlement_result_no_quote_update() {
        let result = SettlementResult {
            quote_update_tx: None,
            buy_order_results: vec![(1001, Ok(make_test_tx_receipt(1001, true)))],
            sell_order_results: vec![],
            total_succeeded: 1,
            total_failed: 0,
            batch_id: "test-batch".to_string(),
            elapsed_ms: 500,
        };

        assert!(!result.quote_update_succeeded());
    }

    // --- Test 6.6: SettlementResult Display tests ---

    #[test]
    fn test_settlement_result_display_success() {
        let result = SettlementResult {
            quote_update_tx: Some(TxReceipt {
                tx_hash: "0xabc123".to_string(),
                block_number: 12345,
                gas_used: 21000,
                status: true,
            }),
            buy_order_results: vec![],
            sell_order_results: vec![],
            total_succeeded: 2,
            total_failed: 0,
            batch_id: "batch-123".to_string(),
            elapsed_ms: 1500,
        };

        let display = format!("{}", result);
        assert!(display.contains("batch_id=batch-123"));
        assert!(display.contains("succeeded=2"));
        assert!(display.contains("failed=0"));
        assert!(display.contains("elapsed_ms=1500"));
        assert!(display.contains("quote_tx=0xabc123"));
        assert!(!display.contains("PARTIAL FAILURE"));
    }

    #[test]
    fn test_settlement_result_display_partial_failure() {
        let result = SettlementResult {
            quote_update_tx: None,
            buy_order_results: vec![],
            sell_order_results: vec![],
            total_succeeded: 1,
            total_failed: 2,
            batch_id: "batch-456".to_string(),
            elapsed_ms: 3000,
        };

        let display = format!("{}", result);
        assert!(display.contains("failed=2"));
        assert!(display.contains("PARTIAL FAILURE"));
    }

    // --- OrderType tests ---

    #[test]
    fn test_order_type_display() {
        assert_eq!(format!("{}", OrderType::Buy), "Buy");
        assert_eq!(format!("{}", OrderType::Sell), "Sell");
    }

    #[test]
    fn test_order_type_equality() {
        assert_eq!(OrderType::Buy, OrderType::Buy);
        assert_eq!(OrderType::Sell, OrderType::Sell);
        assert_ne!(OrderType::Buy, OrderType::Sell);
    }
}