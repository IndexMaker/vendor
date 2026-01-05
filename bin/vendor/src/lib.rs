// bin/vendor/src/lib.rs

pub mod api;
pub mod basket;
pub mod config;
pub mod inventory;
pub mod market_data;
pub mod onchain;
pub mod order_sender;
pub mod margin;
pub mod supply;
pub mod rebalance;

// Re-export commonly used items
pub use order_sender::BitgetClient;