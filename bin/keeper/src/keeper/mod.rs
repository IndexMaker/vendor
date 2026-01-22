mod keeper;
// Story 2.8: Event-driven claim handling
mod claim_handler;

pub use keeper::{KeeperLoop, KeeperLoopConfig};
// Story 2.8: Export claim handler
pub use claim_handler::{ClaimHandler, ClaimResult};