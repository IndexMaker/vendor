use alloy_primitives::Address;
use common::amount::Amount;

/// Transaction receipt information
#[derive(Debug, Clone)]
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
pub struct SubmissionSummary {
    pub market_data_assets: usize,
    pub buy_orders_count: usize,
    pub total_collateral: Amount,
}

/// Gas configuration
#[derive(Debug, Clone)]
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