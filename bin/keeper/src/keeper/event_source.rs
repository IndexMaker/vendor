use crate::accumulator::{IndexOrder, OrderAction};
use common::amount::Amount;
use tokio::sync::mpsc;

/// Event source for keeper orders
/// This is a placeholder for future event-driven architecture
pub struct EventSource {
    order_tx: mpsc::UnboundedSender<IndexOrder>,
}

impl EventSource {
    pub fn new(order_tx: mpsc::UnboundedSender<IndexOrder>) -> Self {
        Self { order_tx }
    }

    /// Submit an order event
    pub fn submit_order(&self, order: IndexOrder) -> eyre::Result<()> {
        self.order_tx.send(order)?;
        Ok(())
    }

    /// Example: Create a deposit order
    pub fn create_deposit(
        &self,
        index_id: u128,
        user_address: String,
        amount_usd: Amount,
    ) -> eyre::Result<()> {
        let order = IndexOrder {
            index_id,
            action: OrderAction::Deposit {
                user_address,
                amount_usd,
            },
            timestamp: chrono::Utc::now(),
        };

        self.submit_order(order)
    }

    /// Example: Create a withdrawal order
    pub fn create_withdrawal(
        &self,
        index_id: u128,
        user_address: String,
        amount_usd: Amount,
    ) -> eyre::Result<()> {
        let order = IndexOrder {
            index_id,
            action: OrderAction::Withdraw {
                user_address,
                amount_usd,
            },
            timestamp: chrono::Utc::now(),
        };

        self.submit_order(order)
    }
}