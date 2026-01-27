//! Stewart contract client for vault discovery
//!
//! Queries the Stewart contract to discover Vault addresses for ITPs.

#![allow(dead_code)] // Many fields and methods reserved for future integration

use crate::chain::ChainError;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::Filter;
use alloy::transports::http::reqwest::Url;
use alloy_primitives::Address;
use alloy_sol_types::{sol, SolEvent};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

/// Asset ID scaling factor - on-chain stores asset IDs multiplied by 10^18
const ASSET_ID_SCALE: u128 = 1_000_000_000_000_000_000;

/// Unscale asset IDs from on-chain format (divide by 10^18)
fn unscale_asset_ids(asset_ids: &[u128]) -> Vec<u128> {
    asset_ids.iter().map(|id| id / ASSET_ID_SCALE).collect()
}

// Define Stewart contract interface
sol! {
    /// Stewart contract for vault management
    #[sol(rpc)]
    interface ISteward {
        /// Get the vault address for an index
        function getVault(uint128 index_id) external view returns (address);

        /// Check if a vault exists for an index
        function hasVault(uint128 index_id) external view returns (bool);

        /// Get the number of assets in an index (Story 2.4)
        function getIndexAssetsCount(uint128 index_id) external view returns (uint128);

        /// Get the assets in an index as packed bytes (Story 2.4)
        function getIndexAssets(uint128 index_id) external view returns (bytes memory);
    }
}

// Guildmaster events emitted through Castle proxy
sol! {
    event IndexCreated(uint128 index_id, string name, string symbol, address vault);
}

/// Cached vault address entry
struct CacheEntry {
    address: Address,
    cached_at: Instant,
}

/// Cached asset list entry (Story 2.4)
struct AssetCacheEntry {
    asset_ids: Vec<u128>,
    cached_at: Instant,
}

/// Configuration for the Stewart client
#[derive(Debug, Clone)]
pub struct StewardClientConfig {
    /// Stewart contract address
    pub steward_address: Address,
    /// RPC URL for chain queries
    pub rpc_url: String,
    /// Cache TTL in seconds
    pub cache_ttl_secs: u64,
    /// Refresh interval for full vault list in seconds
    pub refresh_interval_secs: u64,
}

impl Default for StewardClientConfig {
    fn default() -> Self {
        Self {
            steward_address: Address::ZERO,
            rpc_url: "https://index.rpc.zeeve.net".to_string(),
            cache_ttl_secs: 300, // 5 minutes
            refresh_interval_secs: 300, // 5 minutes
        }
    }
}

/// Client for interacting with the Stewart contract
/// Queries the on-chain Stewart contract for vault discovery.
pub struct StewardClient {
    config: StewardClientConfig,
    vault_cache: Arc<RwLock<HashMap<u128, CacheEntry>>>,
    /// Cache for asset lists by index_id (Story 2.4)
    asset_cache: Arc<RwLock<HashMap<u128, AssetCacheEntry>>>,
    known_vaults: Arc<RwLock<Vec<Address>>>,
    last_refresh: Arc<RwLock<Option<Instant>>>,
    cancel_token: CancellationToken,
}

impl StewardClient {
    /// Create a new Stewart client
    pub fn new(config: StewardClientConfig) -> Self {
        Self {
            config,
            vault_cache: Arc::new(RwLock::new(HashMap::new())),
            asset_cache: Arc::new(RwLock::new(HashMap::new())),
            known_vaults: Arc::new(RwLock::new(Vec::new())),
            last_refresh: Arc::new(RwLock::new(None)),
            cancel_token: CancellationToken::new(),
        }
    }

    /// Get the cancellation token for graceful shutdown
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Get the vault address for an index ID
    /// First checks cache, then queries chain if cache miss or stale
    pub async fn get_vault(&self, index_id: u128) -> Result<Address, ChainError> {
        // Check cache first
        {
            let cache = self.vault_cache.read().await;
            if let Some(entry) = cache.get(&index_id) {
                let age = entry.cached_at.elapsed();
                if age < Duration::from_secs(self.config.cache_ttl_secs) {
                    tracing::debug!("Cache hit for index_id {}", index_id);
                    return Ok(entry.address);
                }
            }
        }

        // Cache miss or stale - query chain
        tracing::debug!("Cache miss for index_id {}, querying chain", index_id);
        self.query_vault_from_chain(index_id).await
    }

    /// Query vault address directly from the Stewart contract
    async fn query_vault_from_chain(&self, index_id: u128) -> Result<Address, ChainError> {
        let url: Url = self.config.rpc_url.parse().map_err(|_| {
            ChainError::InvalidConfig(format!("Invalid RPC URL: {}", self.config.rpc_url))
        })?;

        let provider = ProviderBuilder::new().connect_http(url);

        let steward = ISteward::new(self.config.steward_address, &provider);

        // Call getVault on the contract - returns Address directly
        let vault_address = steward
            .getVault(index_id)
            .call()
            .await
            .map_err(|e| ChainError::StewardCallFailed(format!("getVault failed: {}", e)))?;

        // Check if vault exists (address is not zero)
        if vault_address == Address::ZERO {
            return Err(ChainError::VaultNotFound { index_id });
        }

        // Update cache
        {
            let mut cache = self.vault_cache.write().await;
            cache.insert(
                index_id,
                CacheEntry {
                    address: vault_address,
                    cached_at: Instant::now(),
                },
            );
        }

        // Add to known vaults if not already present
        {
            let mut known = self.known_vaults.write().await;
            if !known.contains(&vault_address) {
                known.push(vault_address);
                tracing::info!("Discovered new vault: {} for index {}", vault_address, index_id);
            }
        }

        Ok(vault_address)
    }

    /// Get all known vault addresses
    pub async fn get_all_vaults(&self) -> Vec<Address> {
        self.known_vaults.read().await.clone()
    }

    /// Get the number of assets in an index (Story 2.4 - AC: #1)
    pub async fn get_index_asset_count(&self, index_id: u128) -> Result<u128, ChainError> {
        let url: Url = self.config.rpc_url.parse().map_err(|_| {
            ChainError::InvalidConfig(format!("Invalid RPC URL: {}", self.config.rpc_url))
        })?;

        let provider = ProviderBuilder::new().connect_http(url);
        let steward = ISteward::new(self.config.steward_address, &provider);

        let count = steward
            .getIndexAssetsCount(index_id)
            .call()
            .await
            .map_err(|e| {
                ChainError::StewardCallFailed(format!(
                    "getIndexAssetsCount failed for index {}: {}",
                    index_id, e
                ))
            })?;

        tracing::debug!("Index {} has {} assets", index_id, count);
        Ok(count)
    }

    /// Get asset IDs for an index (Story 2.4 - AC: #1, #2)
    /// First checks cache, then queries chain if cache miss or stale
    pub async fn get_index_assets(&self, index_id: u128) -> Result<Vec<u128>, ChainError> {
        // Check cache first
        {
            let cache = self.asset_cache.read().await;
            if let Some(entry) = cache.get(&index_id) {
                let age = entry.cached_at.elapsed();
                if age < Duration::from_secs(self.config.cache_ttl_secs) {
                    tracing::debug!("Asset cache hit for index_id {}", index_id);
                    return Ok(entry.asset_ids.clone());
                }
            }
        }

        // Cache miss or stale - query chain
        tracing::debug!("Asset cache miss for index_id {}, querying chain", index_id);
        self.query_assets_from_chain(index_id).await
    }

    /// Query asset IDs directly from the Stewart contract (Story 2.4)
    async fn query_assets_from_chain(&self, index_id: u128) -> Result<Vec<u128>, ChainError> {
        let url: Url = self.config.rpc_url.parse().map_err(|_| {
            ChainError::InvalidConfig(format!("Invalid RPC URL: {}", self.config.rpc_url))
        })?;

        let provider = ProviderBuilder::new().connect_http(url);
        let steward = ISteward::new(self.config.steward_address, &provider);

        // Get the raw bytes from Stewart
        let assets_bytes = steward
            .getIndexAssets(index_id)
            .call()
            .await
            .map_err(|e| {
                ChainError::StewardCallFailed(format!(
                    "getIndexAssets failed for index {}: {}",
                    index_id, e
                ))
            })?;

        // Decode the bytes as u128 array (Stewart returns ABI-encoded uint128[])
        let asset_ids = Self::decode_asset_ids(&assets_bytes)?;

        tracing::debug!(
            "Decoded {} assets for index {}: {:?}",
            asset_ids.len(),
            index_id,
            asset_ids
        );

        // Update cache
        {
            let mut cache = self.asset_cache.write().await;
            cache.insert(
                index_id,
                AssetCacheEntry {
                    asset_ids: asset_ids.clone(),
                    cached_at: Instant::now(),
                },
            );
        }

        Ok(asset_ids)
    }

    /// Decode asset IDs from Steward's bytes response (Story 2.4 - Subtask 1.5)
    /// Steward returns packed bytes where each 16 bytes is a little-endian u128 (Labels format)
    /// Asset IDs are stored as plain values (e.g., 101, 102) not scaled by 10^18
    ///
    /// IMPORTANT: Vaultworks uses little-endian encoding for Labels (u128 arrays).
    /// See vaultworks/libs/common/src/uint.rs - from_le_bytes/to_le_bytes
    fn decode_asset_ids(bytes: &alloy_primitives::Bytes) -> Result<Vec<u128>, ChainError> {
        if bytes.is_empty() {
            return Ok(Vec::new());
        }

        // Raw bytes parsing: 16 bytes per uint128, little-endian (Vaultworks Labels format)
        let mut assets = Vec::new();
        for chunk in bytes.chunks(16) {
            if chunk.len() == 16 {
                let value = u128::from_le_bytes(chunk.try_into().unwrap());
                assets.push(value);
            } else if !chunk.is_empty() {
                // Partial chunk at end - log warning but continue
                tracing::warn!(
                    "Partial asset chunk of {} bytes (expected 16)",
                    chunk.len()
                );
            }
        }

        // Return raw asset IDs without unscaling - Steward stores plain asset IDs
        Ok(assets)
    }

    /// Invalidate asset cache for a specific index (Story 2.4)
    pub async fn invalidate_asset_cache(&self, index_id: u128) {
        let mut cache = self.asset_cache.write().await;
        cache.remove(&index_id);
    }

    /// Clear entire asset cache (Story 2.4)
    pub async fn clear_asset_cache(&self) {
        let mut cache = self.asset_cache.write().await;
        cache.clear();
    }

    /// Refresh the full vault list by scanning IndexCreated events from the Castle proxy.
    ///
    /// Uses event log scanning instead of getVaultCount() enumeration, since
    /// getVaultCount() doesn't exist on the deployed Castle contract.
    pub async fn refresh_vault_list(&self) -> Result<Vec<Address>, ChainError> {
        tracing::info!("Refreshing vault list via IndexCreated event scanning...");

        let url: Url = self.config.rpc_url.parse().map_err(|_| {
            ChainError::InvalidConfig(format!("Invalid RPC URL: {}", self.config.rpc_url))
        })?;

        let provider = ProviderBuilder::new().connect_http(url);

        let filter = Filter::new()
            .address(self.config.steward_address)
            .event_signature(IndexCreated::SIGNATURE_HASH)
            .from_block(0);

        let logs = provider
            .get_logs(&filter)
            .await
            .map_err(|e| {
                ChainError::StewardCallFailed(format!("Failed to fetch IndexCreated logs: {}", e))
            })?;

        tracing::info!("Found {} IndexCreated events", logs.len());

        let mut discovered_vaults = Vec::new();

        for log in &logs {
            let decoded = match log.log_decode::<IndexCreated>() {
                Ok(d) => d,
                Err(e) => {
                    tracing::debug!("Skipping non-decodable IndexCreated log: {}", e);
                    continue;
                }
            };

            let index_id: u128 = decoded.inner.index_id;
            let vault_address = decoded.inner.vault;
            let name = &decoded.inner.name;
            let symbol = &decoded.inner.symbol;

            tracing::info!(
                "Found IndexCreated: index_id={}, name={}, symbol={}, vault={}",
                index_id, name, symbol, vault_address
            );

            // Cache the vault via existing query path
            match self.query_vault_from_chain(index_id).await {
                Ok(vault_addr) => {
                    discovered_vaults.push(vault_addr);
                }
                Err(e) => {
                    // Vault address from event is still valid even if getVault fails
                    tracing::warn!(
                        "getVault failed for index {} but event has vault {}: {:?}",
                        index_id, vault_address, e
                    );
                    // Use the vault address directly from the event
                    {
                        let mut cache = self.vault_cache.write().await;
                        cache.insert(
                            index_id,
                            CacheEntry {
                                address: vault_address,
                                cached_at: Instant::now(),
                            },
                        );
                    }
                    {
                        let mut known = self.known_vaults.write().await;
                        if !known.contains(&vault_address) {
                            known.push(vault_address);
                            tracing::info!(
                                "Discovered vault from event: {} for index {}",
                                vault_address, index_id
                            );
                        }
                    }
                    discovered_vaults.push(vault_address);
                }
            }
        }

        // Update last refresh time
        *self.last_refresh.write().await = Some(Instant::now());

        tracing::info!("Vault refresh complete: {} vaults discovered", discovered_vaults.len());

        Ok(discovered_vaults)
    }

    /// Start the periodic vault refresh loop
    /// Runs in background, refreshing vault list every refresh_interval_secs
    pub async fn start_periodic_refresh(self: Arc<Self>) {
        let refresh_interval = Duration::from_secs(self.config.refresh_interval_secs);

        tracing::info!(
            "Starting periodic vault refresh (interval: {}s)",
            self.config.refresh_interval_secs
        );

        // Do initial refresh
        if let Err(e) = self.refresh_vault_list().await {
            tracing::error!("Initial vault refresh failed: {:?}", e);
        }

        let mut interval = tokio::time::interval(refresh_interval);

        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    tracing::info!("Periodic vault refresh stopped");
                    break;
                }
                _ = interval.tick() => {
                    if let Err(e) = self.refresh_vault_list().await {
                        tracing::error!("Periodic vault refresh failed: {:?}", e);
                    }
                }
            }
        }
    }

    /// Trigger an immediate vault refresh (e.g., on new ITP creation)
    pub async fn trigger_refresh(&self) -> Result<Vec<Address>, ChainError> {
        tracing::info!("Triggering immediate vault refresh");
        self.refresh_vault_list().await
    }

    /// Register a vault address for an index (used for manual setup / testing)
    pub async fn register_vault(&self, index_id: u128, vault_address: Address) {
        let mut cache = self.vault_cache.write().await;
        cache.insert(
            index_id,
            CacheEntry {
                address: vault_address,
                cached_at: Instant::now(),
            },
        );

        let mut known = self.known_vaults.write().await;
        if !known.contains(&vault_address) {
            known.push(vault_address);
        }
    }

    /// Set all known vault addresses directly (for manual configuration)
    pub async fn set_vault_addresses(&self, addresses: Vec<Address>) {
        let mut known = self.known_vaults.write().await;
        *known = addresses;
        *self.last_refresh.write().await = Some(Instant::now());
    }

    /// Invalidate cache for a specific index
    pub async fn invalidate_cache(&self, index_id: u128) {
        let mut cache = self.vault_cache.write().await;
        cache.remove(&index_id);
    }

    /// Clear entire cache
    pub async fn clear_cache(&self) {
        let mut cache = self.vault_cache.write().await;
        cache.clear();
        *self.last_refresh.write().await = None;
    }

    /// Check if a refresh is needed based on last refresh time
    pub async fn needs_refresh(&self) -> bool {
        let last = self.last_refresh.read().await;
        match *last {
            None => true,
            Some(instant) => {
                instant.elapsed() >= Duration::from_secs(self.config.refresh_interval_secs)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = StewardClientConfig::default();
        assert_eq!(config.cache_ttl_secs, 300);
        assert_eq!(config.refresh_interval_secs, 300);
    }

    #[tokio::test]
    async fn test_vault_registration() {
        let client = StewardClient::new(StewardClientConfig::default());
        let vault_addr = Address::repeat_byte(1);

        client.register_vault(1, vault_addr).await;

        let result = client.get_vault(1).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), vault_addr);
    }

    #[tokio::test]
    async fn test_set_vault_addresses() {
        let client = StewardClient::new(StewardClientConfig::default());
        let vaults = vec![Address::repeat_byte(1), Address::repeat_byte(2)];

        client.set_vault_addresses(vaults.clone()).await;

        let result = client.get_all_vaults().await;
        assert_eq!(result.len(), 2);
        assert!(result.contains(&Address::repeat_byte(1)));
        assert!(result.contains(&Address::repeat_byte(2)));
    }

    #[tokio::test]
    async fn test_needs_refresh() {
        let config = StewardClientConfig {
            refresh_interval_secs: 1, // 1 second for testing
            ..Default::default()
        };
        let client = StewardClient::new(config);

        // Should need refresh initially
        assert!(client.needs_refresh().await);

        // After setting addresses, should not need immediate refresh
        client.set_vault_addresses(vec![]).await;
        assert!(!client.needs_refresh().await);

        // Wait for interval to pass
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(client.needs_refresh().await);
    }

    #[tokio::test]
    async fn test_cache_invalidation() {
        let client = StewardClient::new(StewardClientConfig::default());
        let vault_addr = Address::repeat_byte(1);

        client.register_vault(1, vault_addr).await;
        assert!(client.get_vault(1).await.is_ok());

        client.invalidate_cache(1).await;
        // After invalidation, get_vault will fail because there's no real chain to query
        assert!(client.get_vault(1).await.is_err());
    }

    // Story 2.4 Tests - Asset Query Methods

    #[test]
    fn test_decode_asset_ids_empty() {
        let bytes = alloy_primitives::Bytes::new();
        let result = StewardClient::decode_asset_ids(&bytes).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_decode_asset_ids_single() {
        // Single u128 value: 42 in little-endian (16 bytes) - Vaultworks Labels format
        let mut bytes_vec = vec![0u8; 16];
        bytes_vec[0] = 42; // LSB in little-endian is first byte
        let bytes = alloy_primitives::Bytes::from(bytes_vec);

        let result = StewardClient::decode_asset_ids(&bytes).unwrap();
        assert_eq!(result, vec![42]);
    }

    #[test]
    fn test_decode_asset_ids_multiple() {
        // Two u128 values: 1 and 2 in little-endian (Vaultworks Labels format)
        let mut bytes_vec = vec![0u8; 32];
        bytes_vec[0] = 1;  // First u128 = 1 (LSB first)
        bytes_vec[16] = 2; // Second u128 = 2 (LSB first)
        let bytes = alloy_primitives::Bytes::from(bytes_vec);

        let result = StewardClient::decode_asset_ids(&bytes).unwrap();
        assert_eq!(result, vec![1, 2]);
    }

    #[test]
    fn test_decode_asset_ids_large_value() {
        // Test with a larger value to verify little-endian decoding
        // Value: 0x100F0E0D0C0B0A090807060504030201 (little-endian bytes 01 02 03... 10)
        let bytes_vec: Vec<u8> = (1..=16).collect();
        let bytes = alloy_primitives::Bytes::from(bytes_vec);

        let result = StewardClient::decode_asset_ids(&bytes).unwrap();
        assert_eq!(result.len(), 1);

        // Expected value: little-endian interpretation of 01 02 03... 10
        // LSB=01, next=02, ... MSB=10
        let expected: u128 = 0x100F0E0D0C0B0A090807060504030201;
        assert_eq!(result[0], expected);
    }

    #[tokio::test]
    async fn test_asset_cache_operations() {
        let client = StewardClient::new(StewardClientConfig::default());

        // Direct cache population for testing
        {
            let mut cache = client.asset_cache.write().await;
            cache.insert(
                1,
                AssetCacheEntry {
                    asset_ids: vec![101, 102, 103],
                    cached_at: Instant::now(),
                },
            );
        }

        // Cache hit should return the cached values
        let result = client.get_index_assets(1).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), vec![101, 102, 103]);

        // Invalidate cache
        client.invalidate_asset_cache(1).await;

        // After invalidation, should attempt chain query (which will fail in test)
        let result = client.get_index_assets(1).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_clear_asset_cache() {
        let client = StewardClient::new(StewardClientConfig::default());

        // Populate cache
        {
            let mut cache = client.asset_cache.write().await;
            cache.insert(
                1,
                AssetCacheEntry {
                    asset_ids: vec![101],
                    cached_at: Instant::now(),
                },
            );
            cache.insert(
                2,
                AssetCacheEntry {
                    asset_ids: vec![201, 202],
                    cached_at: Instant::now(),
                },
            );
        }

        // Clear all cache
        client.clear_asset_cache().await;

        // Cache should be empty
        let cache = client.asset_cache.read().await;
        assert!(cache.is_empty());
    }
}
