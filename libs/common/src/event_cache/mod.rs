//! Event cache module for caching blockchain events locally.
//!
//! This module provides:
//! - Data structures for caching IndexCreated and PairRegistry events
//! - Persistence via JSON file storage
//! - Thread-safe shared access via EventCacheReader trait
//! - Indexer for syncing and watching events from chain
//!
//! # Architecture
//!
//! The event cache is designed to prevent repeated RPC calls for immutable
//! blockchain events. Once an event is emitted, it never changes, making it
//! perfect for caching.
//!
//! # Usage
//!
//! ```ignore
//! use common::event_cache::{EventCache, EventCacheManager, CachedIndex, CachedPair};
//!
//! // Load existing cache or create new
//! let manager = EventCacheManager::from_env();
//! let cache = manager.load()?;
//!
//! // Access cached data
//! for index in cache.get_all_indices() {
//!     println!("Index: {} ({})", index.name, index.symbol);
//! }
//! ```

pub mod types;

#[cfg(feature = "std")]
pub mod manager;

#[cfg(feature = "std")]
pub mod indexer;

#[cfg(feature = "std")]
pub mod watcher;

#[cfg(feature = "std")]
pub mod reader;

// Re-export types for convenience
pub use types::{CachedIndex, CachedPair, EventCache};

#[cfg(feature = "std")]
pub use manager::{CacheConfig, CacheError, EventCacheManager, CACHE_PATH_ENV_VAR, DEFAULT_CACHE_PATH};

#[cfg(feature = "std")]
pub use indexer::{
    EventIndexer, IndexerConfig, IndexerError, SyncResult,
    GUILDMASTER_ADDRESS_ENV, ORBIT_RPC_URL_ENV, PAIR_REGISTRY_ADDRESS_ENV,
};

#[cfg(feature = "std")]
pub use watcher::{
    EventWatcher, PollResult, SharedEventWatcher, WatcherConfig, WatcherError,
    DEFAULT_POLL_INTERVAL_SECS,
};

#[cfg(feature = "std")]
pub use reader::{
    AsyncSharedEventCache, EventCacheReader, SharedEventCache,
};
