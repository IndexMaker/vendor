//! Types for the on-chain synchronization service.

use alloy_primitives::Address;
use serde::{Deserialize, Serialize};

/// Information about a discovered ITP (Index Token Product).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItpInfo {
    /// The index ID on-chain
    pub index_id: u128,
    /// The vault contract address for this ITP
    pub vault_address: Address,
    /// Number of assets in the ITP
    pub asset_count: usize,
    /// Current composition (if successfully queried)
    pub composition: Option<CompositionSnapshot>,
    /// ITP status
    pub status: ItpStatus,
    /// Collateral token address (e.g. wUSDC) if known
    pub collateral_address: Option<Address>,
    /// Block number when the ITP was created (from IndexCreated event)
    pub creation_block: Option<u64>,
}

/// Snapshot of an ITP's current composition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositionSnapshot {
    /// The index ID
    pub index_id: u128,
    /// Asset IDs in the composition (u128, decoded from Labels)
    pub asset_ids: Vec<u128>,
    /// Weights corresponding to each asset (u128, decoded from Labels)
    pub weights: Vec<u128>,
}

/// ITP lifecycle status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItpStatus {
    /// ITP is active and accepting orders
    Active,
    /// ITP is paused (not accepting new orders)
    Paused,
}

/// A rebalance event from on-chain history.
///
/// Built from `IndexWeightsUpdated` (proposals) and `RebalanceExecuted` (executions)
/// event logs emitted by the Alchemist through the Castle proxy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebalanceEvent {
    /// The index ID this rebalance applies to
    pub index_id: u128,
    /// Block number where the event occurred
    pub block_number: Option<u64>,
    /// Transaction hash
    pub tx_hash: Option<String>,
    /// Address that triggered the rebalance
    pub sender: Option<String>,
}
