//! Chain connectivity module for event watching
//!
//! This module provides:
//! - Event types for BuyOrder/SellOrder parsing
//! - Story 2.8: Extended event types for lifecycle events (Acquisition/Disposal/Claims)
//! - WebSocket event listener with automatic reconnection
//! - Polling fallback for RPC providers without WebSocket support
//! - Stewart client for Vault address discovery

mod event_types;
mod event_listener;
mod steward_client;
mod errors;

pub use event_types::{
    EventSignatures, VaultEvent, parse_log,
    BuyOrderEvent, SellOrderEvent,
    // Story 2.8: New event types for lifecycle events
    AcquisitionClaimEvent, DisposalClaimEvent,
};
pub use event_listener::{ChainEventListener, EventListenerConfig};
pub use steward_client::{StewardClient, StewardClientConfig};
pub use errors::ChainError;
