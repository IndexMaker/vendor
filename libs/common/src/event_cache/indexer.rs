//! Event indexer for syncing blockchain events to local cache.
//!
//! This module provides the `EventIndexer` which queries historical events
//! from the blockchain and stores them in the local event cache.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::interfaces::guildmaster::IGuildmaster::IndexCreated;
use crate::interfaces::pair_registry::PairRegistered;

use super::types::{CachedIndex, CachedPair, EventCache};

/// Errors that can occur during event indexing
#[derive(Debug)]
pub enum IndexerError {
    /// Failed to connect or communicate with RPC
    RpcError(String),
    /// WebSocket connection was disconnected
    WebSocketDisconnect,
    /// Failed to write cache to disk
    CacheWriteFailed { path: String, reason: String },
    /// Failed to parse event data
    ParseError { event_type: String, reason: String },
    /// Configuration error
    ConfigError(String),
}

impl core::fmt::Display for IndexerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            IndexerError::RpcError(msg) => write!(f, "RPC error: {}", msg),
            IndexerError::WebSocketDisconnect => write!(f, "WebSocket disconnected"),
            IndexerError::CacheWriteFailed { path, reason } => {
                write!(f, "Failed to write cache to '{}': {}", path, reason)
            }
            IndexerError::ParseError { event_type, reason } => {
                write!(f, "Failed to parse {} event: {}", event_type, reason)
            }
            IndexerError::ConfigError(msg) => write!(f, "Configuration error: {}", msg),
        }
    }
}

/// Environment variable for Guildmaster contract address
pub const GUILDMASTER_ADDRESS_ENV: &str = "GUILDMASTER_ADDRESS";

/// Environment variable for PairRegistry contract address
pub const PAIR_REGISTRY_ADDRESS_ENV: &str = "PAIR_REGISTRY_ADDRESS";

/// Environment variable for Orbit RPC URL
pub const ORBIT_RPC_URL_ENV: &str = "ORBIT_RPC_URL";

/// Configuration for the EventIndexer
#[derive(Debug, Clone)]
pub struct IndexerConfig {
    /// RPC URL for the Orbit chain
    pub rpc_url: String,
    /// Address of the Guildmaster contract
    pub guildmaster_address: String,
    /// Address of the PairRegistry contract
    pub pair_registry_address: String,
    /// Block to start syncing from (0 for genesis)
    pub start_block: u64,
}

impl IndexerConfig {
    /// Create a new indexer configuration
    pub fn new(
        rpc_url: String,
        guildmaster_address: String,
        pair_registry_address: String,
    ) -> Self {
        Self {
            rpc_url,
            guildmaster_address,
            pair_registry_address,
            start_block: 0,
        }
    }

    /// Create configuration from environment variables
    #[cfg(feature = "std")]
    pub fn from_env() -> Result<Self, IndexerError> {
        let rpc_url = std::env::var(ORBIT_RPC_URL_ENV)
            .map_err(|_| IndexerError::ConfigError(format!("{} not set", ORBIT_RPC_URL_ENV)))?;

        let guildmaster_address = std::env::var(GUILDMASTER_ADDRESS_ENV)
            .map_err(|_| IndexerError::ConfigError(format!("{} not set", GUILDMASTER_ADDRESS_ENV)))?;

        let pair_registry_address = std::env::var(PAIR_REGISTRY_ADDRESS_ENV)
            .map_err(|_| IndexerError::ConfigError(format!("{} not set", PAIR_REGISTRY_ADDRESS_ENV)))?;

        Ok(Self::new(rpc_url, guildmaster_address, pair_registry_address))
    }

    /// Set the start block for syncing
    pub fn with_start_block(mut self, block: u64) -> Self {
        self.start_block = block;
        self
    }
}

/// Event indexer that syncs blockchain events to local cache.
///
/// Watches for `IndexCreated` events from Guildmaster and
/// `PairRegistered` events from PairRegistry contracts.
#[derive(Debug)]
pub struct EventIndexer {
    config: IndexerConfig,
}

impl EventIndexer {
    /// Create a new event indexer with the given configuration
    pub fn new(config: IndexerConfig) -> Self {
        Self { config }
    }

    /// Get the indexer configuration
    pub fn config(&self) -> &IndexerConfig {
        &self.config
    }

    /// Sync all IndexCreated events from the blockchain.
    ///
    /// Queries events from `start_block` (or cache's last synced block) to current block.
    /// Returns the parsed events without modifying the cache - caller should add them.
    #[cfg(feature = "std")]
    pub async fn sync_index_created_events(
        &self,
        from_block: u64,
    ) -> Result<Vec<CachedIndex>, IndexerError> {
        use alloy::primitives::Address;
        use alloy::providers::{Provider, ProviderBuilder};
        use alloy::rpc::types::Filter;
        use alloy_sol_types::SolEvent;

        // Parse contract address
        let guildmaster_addr: Address = self
            .config
            .guildmaster_address
            .parse()
            .map_err(|e| IndexerError::ConfigError(format!("Invalid Guildmaster address: {}", e)))?;

        // Create provider - use connect_http for HTTP RPC endpoints
        let provider = ProviderBuilder::new()
            .connect_http(
                self.config
                    .rpc_url
                    .parse()
                    .map_err(|e| IndexerError::ConfigError(format!("Invalid RPC URL: {}", e)))?,
            );

        // Get current block number
        let current_block = provider
            .get_block_number()
            .await
            .map_err(|e| IndexerError::RpcError(format!("Failed to get block number: {}", e)))?;

        // Build event filter
        let filter = Filter::new()
            .address(guildmaster_addr)
            .event_signature(IndexCreated::SIGNATURE_HASH)
            .from_block(from_block)
            .to_block(current_block);

        // Query logs
        let logs = provider
            .get_logs(&filter)
            .await
            .map_err(|e| IndexerError::RpcError(format!("Failed to get logs: {}", e)))?;

        // Parse logs into CachedIndex
        let mut indices = Vec::new();
        for log in logs {
            // Decode the event - alloy 1.x uses decode_log with single argument
            let decoded = IndexCreated::decode_log(&log.inner)
                .map_err(|e| IndexerError::ParseError {
                    event_type: "IndexCreated".to_string(),
                    reason: e.to_string(),
                })?;

            let block_number = log.block_number.unwrap_or(0);
            let tx_hash = log
                .transaction_hash
                .map(|h| format!("{:?}", h))
                .unwrap_or_else(|| "unknown".to_string());

            indices.push(CachedIndex {
                index_id: decoded.data.index_id,
                name: decoded.data.name.clone(),
                symbol: decoded.data.symbol.clone(),
                vault_address: format!("{:?}", decoded.data.vault),
                block_number,
                tx_hash,
            });
        }

        Ok(indices)
    }

    /// Sync all PairRegistered events from the blockchain.
    ///
    /// Queries events from `start_block` (or cache's last synced block) to current block.
    /// Returns the parsed events without modifying the cache - caller should add them.
    #[cfg(feature = "std")]
    pub async fn sync_pair_registry_events(
        &self,
        from_block: u64,
    ) -> Result<Vec<CachedPair>, IndexerError> {
        use alloy::primitives::Address;
        use alloy::providers::{Provider, ProviderBuilder};
        use alloy::rpc::types::Filter;
        use alloy_sol_types::SolEvent;

        // Parse contract address
        let pair_registry_addr: Address = self
            .config
            .pair_registry_address
            .parse()
            .map_err(|e| IndexerError::ConfigError(format!("Invalid PairRegistry address: {}", e)))?;

        // Create provider - use connect_http for HTTP RPC endpoints
        let provider = ProviderBuilder::new()
            .connect_http(
                self.config
                    .rpc_url
                    .parse()
                    .map_err(|e| IndexerError::ConfigError(format!("Invalid RPC URL: {}", e)))?,
            );

        // Get current block number
        let current_block = provider
            .get_block_number()
            .await
            .map_err(|e| IndexerError::RpcError(format!("Failed to get block number: {}", e)))?;

        // Build event filter
        let filter = Filter::new()
            .address(pair_registry_addr)
            .event_signature(PairRegistered::SIGNATURE_HASH)
            .from_block(from_block)
            .to_block(current_block);

        // Query logs
        let logs = provider
            .get_logs(&filter)
            .await
            .map_err(|e| IndexerError::RpcError(format!("Failed to get logs: {}", e)))?;

        // Parse logs into CachedPair
        let mut pairs = Vec::new();
        for log in logs {
            // Decode the event - alloy 1.x uses decode_log with single argument
            let decoded = PairRegistered::decode_log(&log.inner)
                .map_err(|e| IndexerError::ParseError {
                    event_type: "PairRegistered".to_string(),
                    reason: e.to_string(),
                })?;

            let block_number = log.block_number.unwrap_or(0);
            let tx_hash = log
                .transaction_hash
                .map(|h| format!("{:?}", h))
                .unwrap_or_else(|| "unknown".to_string());

            pairs.push(CachedPair {
                pair_id: decoded.data.pair_id,
                symbol: decoded.data.symbol.clone(),
                base_asset: decoded.data.base_asset.clone(),
                quote_asset: decoded.data.quote_asset.clone(),
                block_number,
                tx_hash,
            });
        }

        Ok(pairs)
    }

    /// Perform a full sync of all events from the blockchain.
    ///
    /// This queries both IndexCreated and PairRegistered events from the
    /// configured start block (or cache's last synced block) and adds them
    /// to the provided cache.
    #[cfg(feature = "std")]
    pub async fn full_sync(&self, cache: &mut EventCache) -> Result<SyncResult, IndexerError> {
        // Determine start blocks based on cache state
        let index_start_block = if cache.last_synced_block_indices > 0 {
            cache.last_synced_block_indices + 1
        } else {
            self.config.start_block
        };

        let pair_start_block = if cache.last_synced_block_pairs > 0 {
            cache.last_synced_block_pairs + 1
        } else {
            self.config.start_block
        };

        // Sync index events
        let new_indices = self.sync_index_created_events(index_start_block).await?;
        let indices_added = new_indices.len();
        for index in new_indices {
            cache.add_index(index);
        }

        // Sync pair events
        let new_pairs = self.sync_pair_registry_events(pair_start_block).await?;
        let pairs_added = new_pairs.len();
        for pair in new_pairs {
            cache.add_pair(pair);
        }

        Ok(SyncResult {
            indices_added,
            pairs_added,
            last_index_block: cache.last_synced_block_indices,
            last_pair_block: cache.last_synced_block_pairs,
        })
    }
}

/// Result of a sync operation
#[derive(Debug, Clone)]
pub struct SyncResult {
    /// Number of new indices added
    pub indices_added: usize,
    /// Number of new pairs added
    pub pairs_added: usize,
    /// Last synced block for index events
    pub last_index_block: u64,
    /// Last synced block for pair events
    pub last_pair_block: u64,
}

impl core::fmt::Display for SyncResult {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Synced {} indices (to block {}), {} pairs (to block {})",
            self.indices_added,
            self.last_index_block,
            self.pairs_added,
            self.last_pair_block
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_indexer_config_new() {
        let config = IndexerConfig::new(
            "https://rpc.example.com".to_string(),
            "0x1234".to_string(),
            "0x5678".to_string(),
        );

        assert_eq!(config.rpc_url, "https://rpc.example.com");
        assert_eq!(config.guildmaster_address, "0x1234");
        assert_eq!(config.pair_registry_address, "0x5678");
        assert_eq!(config.start_block, 0);
    }

    #[test]
    fn test_indexer_config_with_start_block() {
        let config = IndexerConfig::new(
            "https://rpc.example.com".to_string(),
            "0x1234".to_string(),
            "0x5678".to_string(),
        )
        .with_start_block(1000);

        assert_eq!(config.start_block, 1000);
    }

    #[test]
    fn test_indexer_error_display() {
        let error = IndexerError::RpcError("connection refused".to_string());
        assert!(error.to_string().contains("RPC error"));
        assert!(error.to_string().contains("connection refused"));

        let error = IndexerError::ParseError {
            event_type: "IndexCreated".to_string(),
            reason: "invalid data".to_string(),
        };
        assert!(error.to_string().contains("IndexCreated"));
        assert!(error.to_string().contains("invalid data"));
    }

    #[test]
    fn test_sync_result_display() {
        let result = SyncResult {
            indices_added: 5,
            pairs_added: 10,
            last_index_block: 1000,
            last_pair_block: 1500,
        };

        let display = result.to_string();
        assert!(display.contains("5 indices"));
        assert!(display.contains("10 pairs"));
        assert!(display.contains("1000"));
        assert!(display.contains("1500"));
    }
}
