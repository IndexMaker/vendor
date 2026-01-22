//! Shared event cache reader for cross-service access.
//!
//! This module provides thread-safe read access to the event cache,
//! allowing Keeper, Vendor, and Backend services to share cached events.

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::types::{CachedIndex, CachedPair, EventCache};

/// Trait for reading from the event cache.
///
/// This trait defines the read interface for accessing cached events.
/// Implementations must be thread-safe and suitable for concurrent access.
pub trait EventCacheReader: Send + Sync {
    /// Get an index by its ID.
    fn get_index(&self, index_id: u128) -> Option<CachedIndex>;

    /// Get all cached indices.
    fn get_all_indices(&self) -> Vec<CachedIndex>;

    /// Get a pair by its ID.
    fn get_pair(&self, pair_id: u128) -> Option<CachedPair>;

    /// Get a pair by its symbol.
    fn get_pair_by_symbol(&self, symbol: &str) -> Option<CachedPair>;

    /// Get all cached pairs.
    fn get_all_pairs(&self) -> Vec<CachedPair>;

    /// Get the number of cached indices.
    fn index_count(&self) -> usize;

    /// Get the number of cached pairs.
    fn pair_count(&self) -> usize;

    /// Get the last synced block for index events.
    fn last_synced_block_indices(&self) -> u64;

    /// Get the last synced block for pair events.
    fn last_synced_block_pairs(&self) -> u64;
}

/// Thread-safe shared event cache using RwLock.
///
/// This implementation wraps an EventCache in a RwLock for safe concurrent
/// access from multiple services or async tasks.
#[cfg(feature = "std")]
pub struct SharedEventCache {
    cache: Arc<std::sync::RwLock<EventCache>>,
}

#[cfg(feature = "std")]
impl SharedEventCache {
    /// Create a new shared cache with an empty EventCache.
    pub fn new() -> Self {
        Self {
            cache: Arc::new(std::sync::RwLock::new(EventCache::new())),
        }
    }

    /// Create a shared cache from an existing EventCache.
    pub fn from_cache(cache: EventCache) -> Self {
        Self {
            cache: Arc::new(std::sync::RwLock::new(cache)),
        }
    }

    /// Load a shared cache from a file path.
    pub fn from_file(path: &str) -> Result<Self, super::manager::CacheError> {
        use super::manager::{CacheConfig, EventCacheManager};

        let manager = EventCacheManager::new(CacheConfig::new(path.to_string()));
        let cache = manager.load()?;
        Ok(Self::from_cache(cache))
    }

    /// Get a clone of the Arc for sharing with other components.
    pub fn clone_arc(&self) -> Arc<std::sync::RwLock<EventCache>> {
        Arc::clone(&self.cache)
    }

    /// Update the cache with new data.
    ///
    /// This replaces the entire cache contents. Use with caution in
    /// concurrent scenarios.
    pub fn update(&self, new_cache: EventCache) {
        if let Ok(mut guard) = self.cache.write() {
            *guard = new_cache;
        }
    }

    /// Add a new index to the cache.
    pub fn add_index(&self, index: CachedIndex) -> bool {
        if let Ok(mut guard) = self.cache.write() {
            guard.add_index(index);
            true
        } else {
            false
        }
    }

    /// Add a new pair to the cache.
    pub fn add_pair(&self, pair: CachedPair) -> bool {
        if let Ok(mut guard) = self.cache.write() {
            guard.add_pair(pair);
            true
        } else {
            false
        }
    }
}

#[cfg(feature = "std")]
impl Default for SharedEventCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "std")]
impl Clone for SharedEventCache {
    fn clone(&self) -> Self {
        Self {
            cache: Arc::clone(&self.cache),
        }
    }
}

#[cfg(feature = "std")]
impl EventCacheReader for SharedEventCache {
    fn get_index(&self, index_id: u128) -> Option<CachedIndex> {
        self.cache
            .read()
            .ok()
            .and_then(|guard| guard.get_index(index_id).cloned())
    }

    fn get_all_indices(&self) -> Vec<CachedIndex> {
        self.cache
            .read()
            .ok()
            .map(|guard| guard.get_all_indices().to_vec())
            .unwrap_or_default()
    }

    fn get_pair(&self, pair_id: u128) -> Option<CachedPair> {
        self.cache
            .read()
            .ok()
            .and_then(|guard| guard.get_pair(pair_id).cloned())
    }

    fn get_pair_by_symbol(&self, symbol: &str) -> Option<CachedPair> {
        self.cache
            .read()
            .ok()
            .and_then(|guard| guard.get_pair_by_symbol(symbol).cloned())
    }

    fn get_all_pairs(&self) -> Vec<CachedPair> {
        self.cache
            .read()
            .ok()
            .map(|guard| guard.get_all_pairs().to_vec())
            .unwrap_or_default()
    }

    fn index_count(&self) -> usize {
        self.cache
            .read()
            .ok()
            .map(|guard| guard.index_count())
            .unwrap_or(0)
    }

    fn pair_count(&self) -> usize {
        self.cache
            .read()
            .ok()
            .map(|guard| guard.pair_count())
            .unwrap_or(0)
    }

    fn last_synced_block_indices(&self) -> u64 {
        self.cache
            .read()
            .ok()
            .map(|guard| guard.last_synced_block_indices)
            .unwrap_or(0)
    }

    fn last_synced_block_pairs(&self) -> u64 {
        self.cache
            .read()
            .ok()
            .map(|guard| guard.last_synced_block_pairs)
            .unwrap_or(0)
    }
}

/// Async-friendly shared event cache using tokio's RwLock.
///
/// This implementation is optimized for async runtimes and provides
/// non-blocking access to the cache.
#[cfg(feature = "std")]
pub struct AsyncSharedEventCache {
    cache: Arc<tokio::sync::RwLock<EventCache>>,
}

#[cfg(feature = "std")]
impl AsyncSharedEventCache {
    /// Create a new async shared cache with an empty EventCache.
    pub fn new() -> Self {
        Self {
            cache: Arc::new(tokio::sync::RwLock::new(EventCache::new())),
        }
    }

    /// Create an async shared cache from an existing EventCache.
    pub fn from_cache(cache: EventCache) -> Self {
        Self {
            cache: Arc::new(tokio::sync::RwLock::new(cache)),
        }
    }

    /// Load an async shared cache from a file path.
    pub fn from_file(path: &str) -> Result<Self, super::manager::CacheError> {
        use super::manager::{CacheConfig, EventCacheManager};

        let manager = EventCacheManager::new(CacheConfig::new(path.to_string()));
        let cache = manager.load()?;
        Ok(Self::from_cache(cache))
    }

    /// Get a clone of the Arc for sharing with other async tasks.
    pub fn clone_arc(&self) -> Arc<tokio::sync::RwLock<EventCache>> {
        Arc::clone(&self.cache)
    }

    /// Get an index by its ID.
    pub async fn get_index(&self, index_id: u128) -> Option<CachedIndex> {
        self.cache.read().await.get_index(index_id).cloned()
    }

    /// Get all cached indices.
    pub async fn get_all_indices(&self) -> Vec<CachedIndex> {
        self.cache.read().await.get_all_indices().to_vec()
    }

    /// Get a pair by its ID.
    pub async fn get_pair(&self, pair_id: u128) -> Option<CachedPair> {
        self.cache.read().await.get_pair(pair_id).cloned()
    }

    /// Get a pair by its symbol.
    pub async fn get_pair_by_symbol(&self, symbol: &str) -> Option<CachedPair> {
        self.cache.read().await.get_pair_by_symbol(symbol).cloned()
    }

    /// Get all cached pairs.
    pub async fn get_all_pairs(&self) -> Vec<CachedPair> {
        self.cache.read().await.get_all_pairs().to_vec()
    }

    /// Get the number of cached indices.
    pub async fn index_count(&self) -> usize {
        self.cache.read().await.index_count()
    }

    /// Get the number of cached pairs.
    pub async fn pair_count(&self) -> usize {
        self.cache.read().await.pair_count()
    }

    /// Get the last synced block for index events.
    pub async fn last_synced_block_indices(&self) -> u64 {
        self.cache.read().await.last_synced_block_indices
    }

    /// Get the last synced block for pair events.
    pub async fn last_synced_block_pairs(&self) -> u64 {
        self.cache.read().await.last_synced_block_pairs
    }

    /// Update the cache with new data.
    pub async fn update(&self, new_cache: EventCache) {
        *self.cache.write().await = new_cache;
    }

    /// Add a new index to the cache.
    pub async fn add_index(&self, index: CachedIndex) {
        self.cache.write().await.add_index(index);
    }

    /// Add a new pair to the cache.
    pub async fn add_pair(&self, pair: CachedPair) {
        self.cache.write().await.add_pair(pair);
    }
}

#[cfg(feature = "std")]
impl Default for AsyncSharedEventCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "std")]
impl Clone for AsyncSharedEventCache {
    fn clone(&self) -> Self {
        Self {
            cache: Arc::clone(&self.cache),
        }
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn test_shared_cache_new() {
        let cache = SharedEventCache::new();
        assert_eq!(cache.index_count(), 0);
        assert_eq!(cache.pair_count(), 0);
    }

    #[test]
    fn test_shared_cache_add_and_get() {
        let cache = SharedEventCache::new();

        let index = CachedIndex {
            index_id: 1,
            name: "Test".to_string(),
            symbol: "TST".to_string(),
            vault_address: "0x123".to_string(),
            block_number: 100,
            tx_hash: "0xabc".to_string(),
        };

        cache.add_index(index.clone());

        assert_eq!(cache.index_count(), 1);
        assert_eq!(cache.get_index(1).unwrap().name, "Test");
    }

    #[test]
    fn test_shared_cache_thread_safety() {
        use std::thread;

        let cache = SharedEventCache::new();

        // Spawn multiple threads to read and write
        let handles: Vec<_> = (0..10)
            .map(|i| {
                let cache = cache.clone();
                thread::spawn(move || {
                    let index = CachedIndex {
                        index_id: i,
                        name: format!("Index {}", i),
                        symbol: format!("IDX{}", i),
                        vault_address: format!("0x{}", i),
                        block_number: i as u64,
                        tx_hash: format!("0x{:x}", i),
                    };
                    cache.add_index(index);
                    cache.index_count()
                })
            })
            .collect();

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // All indices should have been added
        assert_eq!(cache.index_count(), 10);
    }

    #[test]
    fn test_event_cache_reader_trait() {
        // Ensure SharedEventCache implements EventCacheReader
        fn takes_reader<R: EventCacheReader>(_reader: &R) {}

        let cache = SharedEventCache::new();
        takes_reader(&cache);
    }

    #[tokio::test]
    async fn test_async_shared_cache() {
        let cache = AsyncSharedEventCache::new();

        let index = CachedIndex {
            index_id: 1,
            name: "Async Test".to_string(),
            symbol: "ASYNC".to_string(),
            vault_address: "0x456".to_string(),
            block_number: 200,
            tx_hash: "0xdef".to_string(),
        };

        cache.add_index(index).await;

        assert_eq!(cache.index_count().await, 1);
        assert_eq!(cache.get_index(1).await.unwrap().name, "Async Test");
    }

    #[tokio::test]
    async fn test_async_shared_cache_concurrent() {
        let cache = AsyncSharedEventCache::new();

        let handles: Vec<_> = (0..10)
            .map(|i| {
                let cache = cache.clone();
                tokio::spawn(async move {
                    let index = CachedIndex {
                        index_id: i,
                        name: format!("Async Index {}", i),
                        symbol: format!("A{}", i),
                        vault_address: format!("0x{}", i),
                        block_number: i as u64,
                        tx_hash: format!("0x{:x}", i),
                    };
                    cache.add_index(index).await;
                })
            })
            .collect();

        for handle in handles {
            handle.await.unwrap();
        }

        assert_eq!(cache.index_count().await, 10);
    }
}
