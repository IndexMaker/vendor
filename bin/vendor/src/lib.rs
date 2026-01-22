// bin/vendor/src/lib.rs

pub mod api;
pub mod buffer;
pub mod config;
pub mod market_data;
pub mod onchain;
pub mod order_sender;
pub mod margin;
pub mod supply;
pub mod rebalance;
pub mod order_book;
pub mod delta_rebalancer;

// Re-export commonly used items
pub use order_sender::BitgetClient;
// Story 3-6: Buffer re-exports
pub use buffer::{OrderBuffer, BufferedOrder, BufferOrderSide, MinSizeHandler};
// Story 3-6 Enhancement: Inventory simulation re-exports
pub use buffer::{InventorySimulator, InventorySimConfig, SimulatedPosition, SimulationReason};
// Delta rebalancer re-exports
pub use delta_rebalancer::{DeltaRebalancer, RebalancerConfig, ExecutionMode, RebalanceDecision};