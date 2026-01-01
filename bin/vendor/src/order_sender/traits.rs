use super::types::{AssetOrder, ExecutionResult};
use common::amount::Amount;
use eyre::Result;
use std::collections::HashMap;

#[async_trait::async_trait]
pub trait OrderSender: Send + Sync {
    /// Send multiple orders (batch execution)
    async fn send_orders(&mut self, orders: Vec<AssetOrder>) -> Result<Vec<ExecutionResult>>;

    /// Get current balances for all assets
    async fn get_balances(&self) -> Result<HashMap<String, Amount>>;

    /// Start the sender (connect, authenticate, etc.)
    async fn start(&mut self) -> Result<()>;

    /// Stop the sender (disconnect, cleanup)
    async fn stop(&mut self) -> Result<()>;

    /// Check if the sender is currently active
    fn is_active(&self) -> bool;
}