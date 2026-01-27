//! On-chain synchronization service for ITP discovery and vendor sync.
//!
//! Provides a unified interface for:
//! - Discovering all ITPs from the Steward contract
//! - Getting ITP compositions (asset IDs and weights)
//! - Querying rebalance history from on-chain events
//! - Syncing vendor assets to match ITP requirements

mod types;

pub use types::*;

use alloy::providers::Provider;
use alloy::rpc::types::Filter;
use alloy_primitives::{Address, Bytes};
use alloy_sol_types::{sol, SolEvent};
use eyre::Result;

// Steward contract interface (uses Castle address as proxy)
sol! {
    #[sol(rpc)]
    interface ISteward {
        function getVault(uint128 index_id) external view returns (address);
        function getVaultCount() external view returns (uint128);
        function getIndexAssetsCount(uint128 index_id) external view returns (uint128);
        function getIndexAssets(uint128 index_id) external view returns (bytes memory);
        function getIndexWeights(uint128 index_id) external view returns (bytes memory);
        function getIndexQuote(uint128 index_id, uint128 vendor_id) external view returns (bytes memory);
        function getVendorAssets(uint128 vendor_id) external returns (bytes memory);
    }
}

// Events emitted through Castle proxy
sol! {
    // Alchemist events
    event IndexWeightsUpdated(uint128 index_id, address sender);
    event RebalanceExecuted(uint128 index_id, uint128 vendor_id, uint128 capacity_factor, address sender);
    // Guildmaster events
    event IndexCreated(uint128 index_id, string name, string symbol, address vault);
    // Banker events
    event IndexQuoteUpdated(uint128 index_id, address sender);
}

/// On-chain synchronization service for ITP discovery and vendor data sync.
///
/// Generic over the alloy Provider type to support both read-only and
/// wallet-backed providers.
pub struct OnChainSyncService<P: Provider + Clone> {
    provider: P,
    castle_address: Address,
    vendor_id: u128,
}

impl<P> OnChainSyncService<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    /// Create a new OnChainSyncService.
    ///
    /// # Arguments
    /// * `provider` - Alloy provider (read-only or with wallet)
    /// * `castle_address` - Castle contract address (Steward is accessed via Castle proxy)
    /// * `vendor_id` - Vendor ID for this deployment
    pub fn new(provider: P, castle_address: Address, vendor_id: u128) -> Self {
        Self {
            provider,
            castle_address,
            vendor_id,
        }
    }

    /// Discover all ITPs by scanning IndexCreated events from the Castle proxy.
    ///
    /// Uses event log scanning instead of getVaultCount() enumeration, since
    /// getVaultCount() doesn't exist on the deployed Castle contract.
    /// This is the same proven pattern used by enrich_with_creation_blocks().
    pub async fn discover_all_itps(&self) -> Result<Vec<ItpInfo>> {
        tracing::info!("Discovering ITPs via IndexCreated event scanning");

        let filter = Filter::new()
            .address(self.castle_address)
            .event_signature(IndexCreated::SIGNATURE_HASH)
            .from_block(0);

        let logs = self
            .provider
            .get_logs(&filter)
            .await
            .map_err(|e| eyre::eyre!("Failed to fetch IndexCreated logs: {}", e))?;

        tracing::info!(event_count = logs.len(), "Found IndexCreated events");

        let mut itps = Vec::new();

        for log in &logs {
            let decoded = match log.log_decode::<IndexCreated>() {
                Ok(d) => d,
                Err(e) => {
                    tracing::debug!(error = %e, "Skipping non-decodable IndexCreated log");
                    continue;
                }
            };

            let index_id: u128 = decoded.inner.index_id;
            let vault_address = decoded.inner.vault;
            let name = &decoded.inner.name;
            let symbol = &decoded.inner.symbol;

            tracing::info!(
                index_id,
                vault = %vault_address,
                name,
                symbol,
                "Found IndexCreated event"
            );

            // Get full ITP details via existing get_itp_info
            match self.get_itp_info(index_id).await {
                Ok(mut info) => {
                    // Enrich with creation block from the event log
                    info.creation_block = log.block_number;
                    tracing::info!(
                        index_id,
                        vault = %info.vault_address,
                        assets = info.asset_count,
                        creation_block = ?info.creation_block,
                        "Discovered ITP"
                    );
                    itps.push(info);
                }
                Err(e) => {
                    tracing::warn!(
                        index_id,
                        vault = %vault_address,
                        error = %e,
                        "IndexCreated event found but get_itp_info failed"
                    );
                }
            }
        }

        tracing::info!(count = itps.len(), "ITP discovery complete");
        Ok(itps)
    }

    /// Get detailed info for a single ITP.
    async fn get_itp_info(&self, index_id: u128) -> Result<ItpInfo> {
        let steward = ISteward::new(self.castle_address, &self.provider);

        // Get vault address
        let vault_address = steward
            .getVault(index_id)
            .call()
            .await
            .map_err(|e| eyre::eyre!("getVault({}) failed: {}", index_id, e))?;

        if vault_address == Address::ZERO {
            return Err(eyre::eyre!("No vault for index {}", index_id));
        }

        // Get asset count
        let asset_count = steward
            .getIndexAssetsCount(index_id)
            .call()
            .await
            .unwrap_or(0);

        // Get composition
        let composition = self.get_itp_composition(index_id).await.ok();

        Ok(ItpInfo {
            index_id,
            vault_address,
            asset_count: asset_count as usize,
            composition,
            status: ItpStatus::Active,
            collateral_address: None, // TODO: query from vault contract when interface available
            creation_block: None,     // Populated by enrich_with_creation_blocks()
        })
    }

    /// Get the current composition for an ITP (asset IDs and weights).
    ///
    /// Decodes Labels-format bytes (little-endian u128 arrays) from Steward.
    pub async fn get_itp_composition(&self, index_id: u128) -> Result<CompositionSnapshot> {
        let steward = ISteward::new(self.castle_address, &self.provider);

        // Get assets (Labels-encoded)
        let assets_bytes = steward
            .getIndexAssets(index_id)
            .call()
            .await
            .map_err(|e| eyre::eyre!("getIndexAssets({}) failed: {}", index_id, e))?;

        let asset_ids = decode_labels(&assets_bytes)?;

        // Get weights (Labels-encoded)
        let weights_bytes = steward
            .getIndexWeights(index_id)
            .call()
            .await
            .map_err(|e| eyre::eyre!("getIndexWeights({}) failed: {}", index_id, e))?;

        let weights = decode_labels(&weights_bytes)?;

        Ok(CompositionSnapshot {
            index_id,
            asset_ids,
            weights,
        })
    }

    /// Get assets currently registered for this vendor on-chain.
    pub async fn get_vendor_assets(&self) -> Result<Vec<u128>> {
        let steward = ISteward::new(self.castle_address, &self.provider);

        let assets_bytes = steward
            .getVendorAssets(self.vendor_id)
            .call()
            .await
            .map_err(|e| eyre::eyre!("getVendorAssets({}) failed: {}", self.vendor_id, e))?;

        decode_labels(&assets_bytes)
    }

    /// Determine which assets from an ITP composition are missing from the vendor's registration.
    ///
    /// Returns the list of asset IDs that need to be registered.
    pub async fn find_missing_assets(&self, composition: &CompositionSnapshot) -> Result<Vec<u128>> {
        let registered = self.get_vendor_assets().await.unwrap_or_default();

        let missing: Vec<u128> = composition
            .asset_ids
            .iter()
            .filter(|id| !registered.contains(id))
            .copied()
            .collect();

        if !missing.is_empty() {
            tracing::info!(
                index_id = composition.index_id,
                missing_count = missing.len(),
                registered_count = registered.len(),
                "Found missing vendor assets for ITP"
            );
        }

        Ok(missing)
    }

    /// Discover ITPs and filter to only those the vendor can support.
    ///
    /// An ITP is accepted only if:
    /// 1. All its assets are in `known_asset_ids` (vendor's registered asset list)
    /// 2. Its collateral is in `accepted_collaterals` (if provided)
    ///
    /// Returns (accepted, rejected) ITPs. Vendor must only operate on accepted.
    pub async fn discover_accepted_itps(
        &self,
        known_asset_ids: &[u128],
        accepted_collaterals: Option<&[Address]>,
    ) -> Result<(Vec<ItpInfo>, Vec<ItpInfo>)> {
        let all_itps = self.discover_all_itps().await?;

        let mut accepted = Vec::new();
        let mut rejected = Vec::new();

        for itp in all_itps {
            // Check 1: All ITP assets must be in vendor's asset list
            if let Some(ref comp) = itp.composition {
                let unknown = validate_itp_assets(comp, known_asset_ids);
                if !unknown.is_empty() {
                    tracing::warn!(
                        index_id = itp.index_id,
                        unknown_assets = ?unknown,
                        "Rejecting ITP: contains assets not in vendor's registry"
                    );
                    rejected.push(itp);
                    continue;
                }
            } else {
                // No composition data = cannot validate, reject
                tracing::warn!(
                    index_id = itp.index_id,
                    "Rejecting ITP: no composition data available"
                );
                rejected.push(itp);
                continue;
            }

            // Check 2: Collateral must be whitelisted (if whitelist provided)
            if let Some(collaterals) = accepted_collaterals {
                if let Some(ref collateral) = itp.collateral_address {
                    if !collaterals.contains(collateral) {
                        tracing::warn!(
                            index_id = itp.index_id,
                            collateral = %collateral,
                            "Rejecting ITP: collateral not in whitelist"
                        );
                        rejected.push(itp);
                        continue;
                    }
                }
                // No collateral info = allow (query may not support it yet)
            }

            tracing::info!(
                index_id = itp.index_id,
                asset_count = itp.asset_count,
                "Accepted ITP"
            );
            accepted.push(itp);
        }

        tracing::info!(
            accepted = accepted.len(),
            rejected = rejected.len(),
            "ITP validation complete"
        );

        Ok((accepted, rejected))
    }

    /// Get the vendor ID configured for this sync service.
    pub fn vendor_id(&self) -> u128 {
        self.vendor_id
    }

    /// Get the castle address configured for this sync service.
    pub fn castle_address(&self) -> Address {
        self.castle_address
    }

    /// Get the provider reference.
    pub fn provider(&self) -> &P {
        &self.provider
    }

    /// Enrich ITP list with creation block numbers by scanning IndexCreated events.
    ///
    /// Scans the Castle proxy for `IndexCreated` events and matches them to
    /// the provided ITPs by index_id.
    pub async fn enrich_with_creation_blocks(&self, itps: &mut [ItpInfo]) -> Result<()> {
        let filter = Filter::new()
            .address(self.castle_address)
            .event_signature(IndexCreated::SIGNATURE_HASH)
            .from_block(0);

        let logs = self
            .provider
            .get_logs(&filter)
            .await
            .map_err(|e| eyre::eyre!("Failed to fetch IndexCreated logs: {}", e))?;

        for log in &logs {
            let decoded = match log.log_decode::<IndexCreated>() {
                Ok(d) => d,
                Err(_) => continue,
            };

            let event_index_id: u128 = decoded.inner.index_id;
            if let Some(itp) = itps.iter_mut().find(|i| i.index_id == event_index_id) {
                itp.creation_block = log.block_number;
            }
        }

        Ok(())
    }

    /// Get rebalance history for an ITP from on-chain event logs.
    ///
    /// Scans `IndexWeightsUpdated` events (rebalance proposals) emitted by the
    /// Alchemist through the Castle proxy. Each event represents a composition
    /// change for the given index_id.
    ///
    /// Optional `from_block` sets the scan start; defaults to 0 if not provided.
    /// The RPC provider may limit how far back logs are available.
    pub async fn get_rebalance_history(
        &self,
        index_id: u128,
        from_block: Option<u64>,
    ) -> Result<Vec<RebalanceEvent>> {
        tracing::debug!(index_id, "Scanning IndexWeightsUpdated event logs");

        let filter = Filter::new()
            .address(self.castle_address)
            .event_signature(IndexWeightsUpdated::SIGNATURE_HASH)
            .from_block(from_block.unwrap_or(0));

        let logs = self
            .provider
            .get_logs(&filter)
            .await
            .map_err(|e| eyre::eyre!("Failed to fetch rebalance logs: {}", e))?;

        let mut events = Vec::new();

        for log in &logs {
            // Decode the event
            let decoded = match log.log_decode::<IndexWeightsUpdated>() {
                Ok(d) => d,
                Err(e) => {
                    tracing::debug!(error = %e, "Skipping non-decodable log");
                    continue;
                }
            };

            let event_index_id: u128 = decoded.inner.index_id;

            // Filter to requested index_id
            if event_index_id != index_id {
                continue;
            }

            let block_number = log.block_number;
            let tx_hash = log.transaction_hash.map(|h| format!("{:#x}", h));

            events.push(RebalanceEvent {
                index_id: event_index_id,
                block_number,
                tx_hash,
                sender: Some(format!("{:#x}", decoded.inner.sender)),
            });
        }

        tracing::info!(
            index_id,
            count = events.len(),
            "Rebalance history scan complete"
        );

        Ok(events)
    }

    /// Get the last quote update time for an index by scanning IndexQuoteUpdated events.
    ///
    /// Returns the block number of the most recent IndexQuoteUpdated event for the given index.
    /// Used by AC6 to determine if quote is stale before processing orders.
    ///
    /// # Arguments
    /// * `index_id` - The index ID to check
    /// * `lookback_blocks` - Number of blocks to scan backwards (e.g., 1000)
    ///
    /// # Returns
    /// * `Ok(Some(block_number))` - Block number of last quote update
    /// * `Ok(None)` - No quote update found in lookback range
    /// * `Err` - RPC error
    pub async fn get_quote_last_updated(
        &self,
        index_id: u128,
        lookback_blocks: u64,
    ) -> Result<Option<u64>> {
        tracing::debug!(index_id, lookback_blocks, "Checking quote staleness");

        // Get current block
        let current_block = self
            .provider
            .get_block_number()
            .await
            .map_err(|e| eyre::eyre!("Failed to get block number: {}", e))?;

        let from_block = current_block.saturating_sub(lookback_blocks);

        let filter = Filter::new()
            .address(self.castle_address)
            .event_signature(IndexQuoteUpdated::SIGNATURE_HASH)
            .from_block(from_block);

        let logs = self
            .provider
            .get_logs(&filter)
            .await
            .map_err(|e| eyre::eyre!("Failed to fetch IndexQuoteUpdated logs: {}", e))?;

        // Find the most recent event for this index
        let mut latest_block: Option<u64> = None;

        for log in &logs {
            let decoded = match log.log_decode::<IndexQuoteUpdated>() {
                Ok(d) => d,
                Err(_) => continue,
            };

            let event_index_id: u128 = decoded.inner.index_id;
            if event_index_id != index_id {
                continue;
            }

            if let Some(block) = log.block_number {
                latest_block = Some(latest_block.map_or(block, |b| b.max(block)));
            }
        }

        if let Some(block) = latest_block {
            tracing::debug!(
                index_id,
                last_update_block = block,
                current_block,
                blocks_since_update = current_block - block,
                "Quote last updated"
            );
        } else {
            tracing::debug!(
                index_id,
                lookback_blocks,
                "No quote update found in lookback range"
            );
        }

        Ok(latest_block)
    }

    /// Check if an index quote is stale based on block age.
    ///
    /// AC6: Quote is stale if >5 minutes old. At ~2 second block time (Orbit),
    /// 5 minutes = 150 blocks.
    ///
    /// # Arguments
    /// * `index_id` - The index ID to check
    /// * `max_blocks_age` - Maximum acceptable age in blocks (default: 150 for ~5 min on Orbit)
    ///
    /// # Returns
    /// * `Ok(true)` - Quote is stale (or never updated)
    /// * `Ok(false)` - Quote is fresh
    /// * `Err` - RPC error
    pub async fn is_quote_stale(
        &self,
        index_id: u128,
        max_blocks_age: u64,
    ) -> Result<bool> {
        // Check last 1000 blocks for quote updates
        let last_update = self.get_quote_last_updated(index_id, 1000).await?;

        let current_block = self
            .provider
            .get_block_number()
            .await
            .map_err(|e| eyre::eyre!("Failed to get block number: {}", e))?;

        match last_update {
            Some(update_block) => {
                let age = current_block.saturating_sub(update_block);
                let is_stale = age > max_blocks_age;

                if is_stale {
                    tracing::info!(
                        index_id,
                        last_update_block = update_block,
                        current_block,
                        age_blocks = age,
                        max_age = max_blocks_age,
                        "Quote is STALE - needs refresh"
                    );
                } else {
                    tracing::debug!(
                        index_id,
                        last_update_block = update_block,
                        age_blocks = age,
                        "Quote is fresh"
                    );
                }

                Ok(is_stale)
            }
            None => {
                tracing::warn!(
                    index_id,
                    "No quote update found - treating as STALE"
                );
                Ok(true) // No update found = stale
            }
        }
    }
}

/// Validate whether an ITP's assets are all in the vendor's known asset list.
///
/// Returns the list of unsupported asset IDs (empty = fully supported).
pub fn validate_itp_assets(composition: &CompositionSnapshot, known_asset_ids: &[u128]) -> Vec<u128> {
    composition
        .asset_ids
        .iter()
        .filter(|id| !known_asset_ids.contains(id))
        .copied()
        .collect()
}

/// Decode Labels-format bytes into a Vec<u128>.
///
/// Vaultworks uses little-endian encoding for Labels (u128 arrays).
/// Each 16 bytes represents one u128 value in little-endian format.
pub fn decode_labels(bytes: &Bytes) -> Result<Vec<u128>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }

    let mut values = Vec::new();
    for chunk in bytes.chunks(16) {
        if chunk.len() == 16 {
            let value = u128::from_le_bytes(chunk.try_into().unwrap());
            values.push(value);
        } else if !chunk.is_empty() {
            tracing::warn!(
                chunk_len = chunk.len(),
                "Partial Labels chunk (expected 16 bytes)"
            );
        }
    }

    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_labels_empty() {
        let bytes = Bytes::new();
        let result = decode_labels(&bytes).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_decode_labels_single() {
        let mut bytes_vec = vec![0u8; 16];
        bytes_vec[0] = 42; // LE: 42 is in first byte
        let bytes = Bytes::from(bytes_vec);

        let result = decode_labels(&bytes).unwrap();
        assert_eq!(result, vec![42]);
    }

    #[test]
    fn test_decode_labels_multiple() {
        let mut bytes_vec = vec![0u8; 48]; // 3 values
        bytes_vec[0] = 1;   // First u128 = 1
        bytes_vec[16] = 2;  // Second u128 = 2
        bytes_vec[32] = 128; // Third u128 = 128
        let bytes = Bytes::from(bytes_vec);

        let result = decode_labels(&bytes).unwrap();
        assert_eq!(result, vec![1, 2, 128]);
    }

    #[test]
    fn test_decode_labels_large_value() {
        // Test with value that uses multiple bytes
        let bytes_vec: Vec<u8> = (1..=16).collect();
        let bytes = Bytes::from(bytes_vec);

        let result = decode_labels(&bytes).unwrap();
        assert_eq!(result.len(), 1);
        let expected: u128 = 0x100F0E0D0C0B0A090807060504030201;
        assert_eq!(result[0], expected);
    }

    #[test]
    fn test_itp_info_creation() {
        let info = ItpInfo {
            index_id: 10000,
            vault_address: Address::repeat_byte(1),
            asset_count: 3,
            composition: Some(CompositionSnapshot {
                index_id: 10000,
                asset_ids: vec![1, 2, 3],
                weights: vec![3333, 3333, 3334],
            }),
            status: ItpStatus::Active,
            collateral_address: None,
            creation_block: None,
        };

        assert_eq!(info.index_id, 10000);
        assert_eq!(info.asset_count, 3);
        assert!(info.composition.is_some());
    }

    #[test]
    fn test_validate_itp_assets_all_known() {
        let comp = CompositionSnapshot {
            index_id: 10000,
            asset_ids: vec![1, 2, 3],
            weights: vec![3333, 3333, 3334],
        };
        let known = vec![1, 2, 3, 4, 5];
        let unknown = validate_itp_assets(&comp, &known);
        assert!(unknown.is_empty());
    }

    #[test]
    fn test_validate_itp_assets_some_unknown() {
        let comp = CompositionSnapshot {
            index_id: 10000,
            asset_ids: vec![1, 2, 99],
            weights: vec![3333, 3333, 3334],
        };
        let known = vec![1, 2, 3];
        let unknown = validate_itp_assets(&comp, &known);
        assert_eq!(unknown, vec![99]);
    }

    #[test]
    fn test_validate_itp_assets_none_known() {
        let comp = CompositionSnapshot {
            index_id: 10000,
            asset_ids: vec![50, 60],
            weights: vec![5000, 5000],
        };
        let known: Vec<u128> = vec![];
        let unknown = validate_itp_assets(&comp, &known);
        assert_eq!(unknown, vec![50, 60]);
    }

    #[test]
    fn test_composition_snapshot() {
        let snap = CompositionSnapshot {
            index_id: 10000,
            asset_ids: vec![128, 129, 130],
            weights: vec![3000, 3000, 4000],
        };

        assert_eq!(snap.asset_ids.len(), 3);
        assert_eq!(snap.weights.len(), 3);
    }

    // =========================================================================
    // Story 0-1 M4: Additional edge case tests
    // =========================================================================

    #[test]
    fn test_decode_labels_partial_chunk() {
        // Test with bytes that don't form complete chunks
        let bytes = Bytes::from(vec![1u8, 2, 3, 4, 5]); // 5 bytes (partial)
        let result = decode_labels(&bytes).unwrap();
        // Should be empty since we can't form a complete 16-byte chunk
        assert!(result.is_empty());
    }

    #[test]
    fn test_decode_labels_mixed_partial() {
        // 20 bytes = 1 complete chunk (16) + 4 partial
        let mut bytes_vec = vec![0u8; 20];
        bytes_vec[0] = 255; // First u128 = 255
        let bytes = Bytes::from(bytes_vec);

        let result = decode_labels(&bytes).unwrap();
        // Should decode only the complete chunk
        assert_eq!(result, vec![255]);
    }

    #[test]
    fn test_itp_info_empty_composition() {
        let info = ItpInfo {
            index_id: 10001,
            vault_address: Address::repeat_byte(2),
            asset_count: 0,
            composition: None,
            status: ItpStatus::Paused,
            collateral_address: None,
            creation_block: Some(12345678),
        };

        assert_eq!(info.index_id, 10001);
        assert_eq!(info.asset_count, 0);
        assert!(info.composition.is_none());
        assert_eq!(info.status, ItpStatus::Paused);
        assert_eq!(info.creation_block, Some(12345678));
    }

    #[test]
    fn test_rebalance_event_fields() {
        let event = RebalanceEvent {
            index_id: 10000,
            block_number: Some(9876543),
            tx_hash: Some("0x1234567890abcdef".to_string()),
            sender: Some("0xdeadbeef".to_string()),
        };

        assert_eq!(event.index_id, 10000);
        assert_eq!(event.block_number, Some(9876543));
        assert!(event.tx_hash.is_some());
        assert!(event.sender.is_some());
    }

    #[test]
    fn test_rebalance_event_minimal() {
        let event = RebalanceEvent {
            index_id: 10000,
            block_number: None,
            tx_hash: None,
            sender: None,
        };

        assert_eq!(event.index_id, 10000);
        assert!(event.block_number.is_none());
        assert!(event.tx_hash.is_none());
        assert!(event.sender.is_none());
    }

    #[test]
    fn test_itp_status_variants() {
        assert_eq!(ItpStatus::Active, ItpStatus::Active);
        assert_eq!(ItpStatus::Paused, ItpStatus::Paused);
        assert_ne!(ItpStatus::Active, ItpStatus::Paused);
    }

    #[test]
    fn test_validate_itp_assets_empty_composition() {
        let comp = CompositionSnapshot {
            index_id: 10000,
            asset_ids: vec![],
            weights: vec![],
        };
        let known = vec![1, 2, 3];
        let unknown = validate_itp_assets(&comp, &known);
        assert!(unknown.is_empty());
    }

    #[test]
    fn test_decode_labels_zero_values() {
        // All zeros
        let bytes = Bytes::from(vec![0u8; 32]); // 2 x 16 bytes = 2 zero u128s
        let result = decode_labels(&bytes).unwrap();
        assert_eq!(result, vec![0, 0]);
    }

    #[test]
    fn test_decode_labels_max_value() {
        // u128::MAX in little-endian
        let bytes = Bytes::from(vec![0xFFu8; 16]);
        let result = decode_labels(&bytes).unwrap();
        assert_eq!(result, vec![u128::MAX]);
    }
}
