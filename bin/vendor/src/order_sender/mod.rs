pub mod adaptive_pricing;
pub mod bitget;
pub mod config;
pub mod fees;
pub mod index_orderbook_fetcher;
pub mod rate_limiter;
pub mod refill_manager;
pub mod simulated;
pub mod traits;
pub mod types;

pub use adaptive_pricing::AdaptivePricingStrategy;
pub use bitget::{BatchExecutionSummary, BitgetClient};
pub use config::{BitgetCredentials, OrderSenderConfig, OrderSenderMode};
pub use fees::FeeTracker;
pub use index_orderbook_fetcher::{IndexOrderbookFetcher, IndexOrderbookFetcherConfig, FetchStats};
pub use rate_limiter::RateLimiter;
pub use refill_manager::RefillManager;
pub use simulated::SimulatedOrderSender;
pub use traits::OrderSender;
pub use types::{AssetOrder, ExecutionResult, ExecutionMode, OrderSide, OrderStatus, OrderType};




#[cfg(test)]
mod tests {
    use super::*;
    use common::amount::Amount;

    #[tokio::test]
    async fn test_simulated_order_sender() {
        let config = OrderSenderConfig::builder()
            .mode(OrderSenderMode::Simulated { failure_rate: 0.0 })
            .build()
            .expect("Failed to build config");

        config.start().await.expect("Failed to start");

        let sender = config.get_sender().expect("No sender");

        // Create test order
        let order = AssetOrder::market(
            "BTCUSDT".to_string(),
            OrderSide::Buy,
            Amount::from_u128_raw(1_000_000_000_000_000_000), // 1.0
        );

        // Execute - scope the write lock
        let results = {
            let mut sender_guard = sender.write().await;
            sender_guard
                .send_orders(vec![order])
                .await
                .expect("Failed to send orders")
        }; // Lock released here

        assert_eq!(results.len(), 1);
        assert!(results[0].is_success());
        assert_eq!(results[0].symbol, "BTCUSDT");

        // Check balances - scope the read lock
        let balances = {
            let sender_guard = sender.read().await;
            sender_guard.get_balances().await.expect("Failed to get balances")
        }; // Lock released here

        assert!(balances.contains_key("BTCUSDT"));

        config.stop().await.expect("Failed to stop");
    }
}