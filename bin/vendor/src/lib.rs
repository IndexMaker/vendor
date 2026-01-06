// bin/vendor/src/lib.rs

pub mod api;
pub mod config;
pub mod market_data;
pub mod onchain;
pub mod order_sender;
pub mod margin;
pub mod supply;
pub mod rebalance;
pub mod order_book;

// Re-export commonly used items
pub use order_sender::BitgetClient;