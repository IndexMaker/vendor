//! Event cache data structures for storing blockchain events locally.
//!
//! These types are used by Keeper, Vendor, and Backend services to cache
//! IndexCreated and PairRegistry events, avoiding repeated RPC calls.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// Cached representation of an IndexCreated event from Guildmaster contract.
///
/// Emitted when a new ITP index is created via `submitIndex()`.
/// Contract: Guildmaster (called via Castle)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedIndex {
    /// Unique identifier for the index
    pub index_id: u128,
    /// Human-readable name of the index
    pub name: String,
    /// Trading symbol for the index (e.g., "ITP-BTC5")
    pub symbol: String,
    /// Address of the vault contract for this index
    pub vault_address: String,
    /// Block number where the event was emitted
    pub block_number: u64,
    /// Transaction hash where the event was emitted
    pub tx_hash: String,
}

/// Cached representation of a PairRegistered event from PairRegistry contract.
///
/// Emitted when a Bitget trading pair is registered in the system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedPair {
    /// Unique identifier for the pair
    pub pair_id: u128,
    /// Trading symbol (e.g., "BTCUSDT")
    pub symbol: String,
    /// Base asset identifier (e.g., "BTC")
    pub base_asset: String,
    /// Quote asset identifier (e.g., "USDT")
    pub quote_asset: String,
    /// Block number where the event was emitted
    pub block_number: u64,
    /// Transaction hash where the event was emitted
    pub tx_hash: String,
}

/// Main event cache container holding all cached events.
///
/// This struct is serialized to JSON for persistence across restarts.
/// Events are immutable on-chain, so no cache invalidation is needed.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EventCache {
    /// All cached IndexCreated events
    pub indices: Vec<CachedIndex>,
    /// All cached PairRegistered events
    pub pairs: Vec<CachedPair>,
    /// Last synced block number for IndexCreated events
    pub last_synced_block_indices: u64,
    /// Last synced block number for PairRegistry events
    pub last_synced_block_pairs: u64,
}

impl EventCache {
    /// Create a new empty event cache
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a new index to the cache if not already present (deduplicate by tx_hash)
    pub fn add_index(&mut self, index: CachedIndex) {
        if !self.indices.iter().any(|i| i.tx_hash == index.tx_hash) {
            // Update last synced block if this event is newer
            if index.block_number > self.last_synced_block_indices {
                self.last_synced_block_indices = index.block_number;
            }
            self.indices.push(index);
        }
    }

    /// Add a new pair to the cache if not already present (deduplicate by tx_hash)
    pub fn add_pair(&mut self, pair: CachedPair) {
        if !self.pairs.iter().any(|p| p.tx_hash == pair.tx_hash) {
            // Update last synced block if this event is newer
            if pair.block_number > self.last_synced_block_pairs {
                self.last_synced_block_pairs = pair.block_number;
            }
            self.pairs.push(pair);
        }
    }

    /// Get an index by its ID
    pub fn get_index(&self, index_id: u128) -> Option<&CachedIndex> {
        self.indices.iter().find(|i| i.index_id == index_id)
    }

    /// Get all cached indices
    pub fn get_all_indices(&self) -> &[CachedIndex] {
        &self.indices
    }

    /// Get a pair by its ID
    pub fn get_pair(&self, pair_id: u128) -> Option<&CachedPair> {
        self.pairs.iter().find(|p| p.pair_id == pair_id)
    }

    /// Get a pair by its symbol
    pub fn get_pair_by_symbol(&self, symbol: &str) -> Option<&CachedPair> {
        self.pairs.iter().find(|p| p.symbol == symbol)
    }

    /// Get all cached pairs
    pub fn get_all_pairs(&self) -> &[CachedPair] {
        &self.pairs
    }

    /// Get the number of cached indices
    pub fn index_count(&self) -> usize {
        self.indices.len()
    }

    /// Get the number of cached pairs
    pub fn pair_count(&self) -> usize {
        self.pairs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cached_index_serialization() {
        let index = CachedIndex {
            index_id: 1,
            name: "Test Index".into(),
            symbol: "TEST".into(),
            vault_address: "0x1234567890abcdef1234567890abcdef12345678".into(),
            block_number: 12345,
            tx_hash: "0xabcdef".into(),
        };

        let json = serde_json::to_string(&index).unwrap();
        let deserialized: CachedIndex = serde_json::from_str(&json).unwrap();
        assert_eq!(index, deserialized);
    }

    #[test]
    fn test_cached_pair_serialization() {
        let pair = CachedPair {
            pair_id: 1,
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            block_number: 12345,
            tx_hash: "0xabcdef".into(),
        };

        let json = serde_json::to_string(&pair).unwrap();
        let deserialized: CachedPair = serde_json::from_str(&json).unwrap();
        assert_eq!(pair, deserialized);
    }

    #[test]
    fn test_event_cache_deduplication() {
        let mut cache = EventCache::new();

        let index1 = CachedIndex {
            index_id: 1,
            name: "Test".into(),
            symbol: "TEST".into(),
            vault_address: "0x123".into(),
            block_number: 100,
            tx_hash: "0xabc".into(),
        };

        // Add same index twice
        cache.add_index(index1.clone());
        cache.add_index(index1.clone());

        // Should only have one entry
        assert_eq!(cache.index_count(), 1);
    }

    #[test]
    fn test_event_cache_last_synced_block() {
        let mut cache = EventCache::new();

        cache.add_index(CachedIndex {
            index_id: 1,
            name: "Test1".into(),
            symbol: "T1".into(),
            vault_address: "0x1".into(),
            block_number: 100,
            tx_hash: "0xa".into(),
        });

        cache.add_index(CachedIndex {
            index_id: 2,
            name: "Test2".into(),
            symbol: "T2".into(),
            vault_address: "0x2".into(),
            block_number: 200,
            tx_hash: "0xb".into(),
        });

        assert_eq!(cache.last_synced_block_indices, 200);
    }

    #[test]
    fn test_event_cache_full_serialization() {
        let mut cache = EventCache::new();

        cache.add_index(CachedIndex {
            index_id: 1,
            name: "Index1".into(),
            symbol: "IDX1".into(),
            vault_address: "0x111".into(),
            block_number: 100,
            tx_hash: "0xaaa".into(),
        });

        cache.add_pair(CachedPair {
            pair_id: 1,
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            block_number: 150,
            tx_hash: "0xbbb".into(),
        });

        let json = serde_json::to_string_pretty(&cache).unwrap();
        let deserialized: EventCache = serde_json::from_str(&json).unwrap();

        assert_eq!(cache.index_count(), deserialized.index_count());
        assert_eq!(cache.pair_count(), deserialized.pair_count());
        assert_eq!(cache.last_synced_block_indices, deserialized.last_synced_block_indices);
        assert_eq!(cache.last_synced_block_pairs, deserialized.last_synced_block_pairs);
    }
}
