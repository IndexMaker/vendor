//! Async Order Buffer and Inventory Management (Story 3-6)
//!
//! This module provides immediate HTTP response with async order processing:
//! - Orders are queued immediately (< 100ms response)
//! - Background processor handles Bitget execution
//! - Min order size enforcement with buffering
//! - Acquisition cost tracking
//! - Persistent state with crash recovery
//! - **Inventory simulation mode**: Accept orders without exchange execution,
//!   submit to on-chain AS IF we have inventory, track simulated positions

pub mod types;
pub mod queue;
pub mod persistence;
pub mod min_size_handler;
pub mod cost_tracker;
pub mod processor;
pub mod withdrawals;
pub mod inventory_sim;

pub use types::*;
pub use queue::OrderBuffer;
pub use persistence::{checkpoint, restore, BufferCheckpoint};
pub use min_size_handler::{MinSizeHandler, OrderAction};
pub use cost_tracker::{AcquisitionCostTracker, OrderFill, OrderBookSnapshot, CostSummary};
pub use processor::BufferProcessor;
pub use withdrawals::WithdrawalHandler;
pub use inventory_sim::{
    InventorySimulator, InventorySimConfig, SimulatedPosition, SimulationReason, InventorySimStats
};
