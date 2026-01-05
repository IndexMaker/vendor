use common::amount::Amount;
use serde::{Deserialize, Serialize};

/// Decision on whether to rebalance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RebalanceDecision {
    /// No rebalancing needed
    NoAction,
    
    /// Rebalancing needed
    Rebalance {
        symbol: String,
        side: RebalanceSide,
        quantity: Amount,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RebalanceSide {
    /// Need to BUY to cover short position
    Buy,
    
    /// Need to SELL to reduce long position
    Sell,
}

impl std::fmt::Display for RebalanceSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RebalanceSide::Buy => write!(f, "Buy"),
            RebalanceSide::Sell => write!(f, "Sell"),
        }
    }
}

/// Rebalancing order to be executed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebalanceOrder {
    pub symbol: String,
    pub side: RebalanceSide,
    pub quantity: Amount,
    pub reason: String,
}

impl RebalanceOrder {
    pub fn new(symbol: String, side: RebalanceSide, quantity: Amount, reason: String) -> Self {
        Self {
            symbol,
            side,
            quantity,
            reason,
        }
    }
}