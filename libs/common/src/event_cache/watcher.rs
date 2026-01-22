//! Real-time event watcher for monitoring new blockchain events.
//!
//! This module provides a polling-based event watcher that monitors for new
//! IndexCreated and PairRegistered events and updates the cache in real-time.
//!
//! The watcher uses a polling approach (configurable interval, default 12 seconds)
//! which is more reliable than WebSocket subscriptions for chains that may not
//! support them consistently.

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::time::Duration;

use super::indexer::{EventIndexer, IndexerConfig, IndexerError};
use super::manager::{CacheError, EventCacheManager};
use super::types::EventCache;

/// Default polling interval in seconds
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 12;

/// Configuration for the EventWatcher
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// Indexer configuration (RPC URL, contract addresses)
    pub indexer_config: IndexerConfig,
    /// Polling interval in seconds
    pub poll_interval_secs: u64,
    /// Path to the cache file
    pub cache_path: String,
}

impl WatcherConfig {
    /// Create a new watcher configuration
    pub fn new(indexer_config: IndexerConfig, cache_path: String) -> Self {
        Self {
            indexer_config,
            poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,
            cache_path,
        }
    }

    /// Set the polling interval
    pub fn with_poll_interval(mut self, secs: u64) -> Self {
        self.poll_interval_secs = secs;
        self
    }

    /// Create configuration from environment variables
    #[cfg(feature = "std")]
    pub fn from_env() -> Result<Self, IndexerError> {
        use super::manager::CACHE_PATH_ENV_VAR;

        let indexer_config = IndexerConfig::from_env()?;

        let cache_path = std::env::var(CACHE_PATH_ENV_VAR)
            .unwrap_or_else(|_| super::manager::DEFAULT_CACHE_PATH.to_string());

        Ok(Self::new(indexer_config, cache_path))
    }
}

/// Result of a single poll cycle
#[derive(Debug, Clone, Default)]
pub struct PollResult {
    /// Number of new indices found and cached
    pub new_indices: usize,
    /// Number of new pairs found and cached
    pub new_pairs: usize,
    /// Latest block synced for indices
    pub last_index_block: u64,
    /// Latest block synced for pairs
    pub last_pair_block: u64,
}

impl PollResult {
    /// Returns true if any new events were found
    pub fn has_new_events(&self) -> bool {
        self.new_indices > 0 || self.new_pairs > 0
    }
}

/// Errors that can occur during watching
#[derive(Debug)]
pub enum WatcherError {
    /// Error during indexing
    IndexerError(IndexerError),
    /// Error during cache operations
    CacheError(CacheError),
    /// Watcher was stopped
    Stopped,
}

impl core::fmt::Display for WatcherError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WatcherError::IndexerError(e) => write!(f, "Indexer error: {}", e),
            WatcherError::CacheError(e) => write!(f, "Cache error: {}", e),
            WatcherError::Stopped => write!(f, "Watcher stopped"),
        }
    }
}

impl From<IndexerError> for WatcherError {
    fn from(e: IndexerError) -> Self {
        WatcherError::IndexerError(e)
    }
}

impl From<CacheError> for WatcherError {
    fn from(e: CacheError) -> Self {
        WatcherError::CacheError(e)
    }
}

/// Real-time event watcher that polls for new events and updates the cache.
///
/// Uses polling (default: every 12 seconds) to check for new events.
/// Events are automatically persisted to the cache file after each poll.
#[cfg(feature = "std")]
pub struct EventWatcher {
    config: WatcherConfig,
    indexer: EventIndexer,
    cache_manager: EventCacheManager,
    cache: EventCache,
}

#[cfg(feature = "std")]
impl EventWatcher {
    /// Create a new event watcher with the given configuration.
    ///
    /// Loads existing cache from disk if available.
    pub fn new(config: WatcherConfig) -> Result<Self, WatcherError> {
        use super::manager::CacheConfig;

        let indexer = EventIndexer::new(config.indexer_config.clone());
        let cache_manager =
            EventCacheManager::new(CacheConfig::new(config.cache_path.clone()));
        let cache = cache_manager.load_or_default();

        Ok(Self {
            config,
            indexer,
            cache_manager,
            cache,
        })
    }

    /// Create a new event watcher from environment configuration.
    pub fn from_env() -> Result<Self, WatcherError> {
        let config = WatcherConfig::from_env()?;
        Self::new(config)
    }

    /// Get a reference to the current cache state.
    pub fn cache(&self) -> &EventCache {
        &self.cache
    }

    /// Get the watcher configuration.
    pub fn config(&self) -> &WatcherConfig {
        &self.config
    }

    /// Perform initial sync of all historical events.
    ///
    /// This should be called once when the watcher starts to catch up
    /// on any events that occurred while the service was down.
    pub async fn initial_sync(&mut self) -> Result<PollResult, WatcherError> {
        let sync_result = self.indexer.full_sync(&mut self.cache).await?;

        // Persist the cache after initial sync
        self.cache_manager.save(&self.cache)?;

        Ok(PollResult {
            new_indices: sync_result.indices_added,
            new_pairs: sync_result.pairs_added,
            last_index_block: sync_result.last_index_block,
            last_pair_block: sync_result.last_pair_block,
        })
    }

    /// Poll for new events once and update the cache.
    ///
    /// Returns the result of the poll, including counts of new events found.
    pub async fn poll_once(&mut self) -> Result<PollResult, WatcherError> {
        let initial_indices = self.cache.index_count();
        let initial_pairs = self.cache.pair_count();

        // Use incremental sync (from last synced block + 1)
        let sync_result = self.indexer.full_sync(&mut self.cache).await?;

        let new_indices = self.cache.index_count() - initial_indices;
        let new_pairs = self.cache.pair_count() - initial_pairs;

        // Only persist if we found new events
        if new_indices > 0 || new_pairs > 0 {
            self.cache_manager.save(&self.cache)?;
        }

        Ok(PollResult {
            new_indices,
            new_pairs,
            last_index_block: sync_result.last_index_block,
            last_pair_block: sync_result.last_pair_block,
        })
    }

    /// Run the watcher in a polling loop.
    ///
    /// This method runs forever, polling for new events at the configured interval.
    /// Use `run_with_callback` to receive notifications of new events.
    pub async fn run_forever(&mut self) -> Result<(), WatcherError> {
        self.run_with_callback(|_| {}).await
    }

    /// Run the watcher in a polling loop with a callback for new events.
    ///
    /// The callback is invoked after each poll that finds new events.
    pub async fn run_with_callback<F>(&mut self, mut on_new_events: F) -> Result<(), WatcherError>
    where
        F: FnMut(&PollResult),
    {
        let interval = Duration::from_secs(self.config.poll_interval_secs);

        loop {
            match self.poll_once().await {
                Ok(result) => {
                    if result.has_new_events() {
                        on_new_events(&result);
                    }
                }
                Err(e) => {
                    // Log error but continue polling
                    tracing::warn!(
                        target: "event_watcher",
                        error = %e,
                        "Poll error (will retry)"
                    );
                }
            }

            tokio::time::sleep(interval).await;
        }
    }
}

/// A shared, thread-safe event watcher that can be used across async tasks.
///
/// Wraps EventWatcher with Arc<tokio::sync::RwLock> for safe concurrent access.
#[cfg(feature = "std")]
pub struct SharedEventWatcher {
    inner: Arc<tokio::sync::RwLock<EventWatcher>>,
}

#[cfg(feature = "std")]
impl SharedEventWatcher {
    /// Create a new shared event watcher.
    pub fn new(config: WatcherConfig) -> Result<Self, WatcherError> {
        let watcher = EventWatcher::new(config)?;
        Ok(Self {
            inner: Arc::new(tokio::sync::RwLock::new(watcher)),
        })
    }

    /// Create a shared event watcher from environment configuration.
    pub fn from_env() -> Result<Self, WatcherError> {
        let watcher = EventWatcher::from_env()?;
        Ok(Self {
            inner: Arc::new(tokio::sync::RwLock::new(watcher)),
        })
    }

    /// Get a clone of the Arc for sharing with other tasks.
    pub fn clone_inner(&self) -> Arc<tokio::sync::RwLock<EventWatcher>> {
        Arc::clone(&self.inner)
    }

    /// Read the current cache state.
    pub async fn cache(&self) -> EventCache {
        self.inner.read().await.cache().clone()
    }

    /// Perform initial sync.
    pub async fn initial_sync(&self) -> Result<PollResult, WatcherError> {
        self.inner.write().await.initial_sync().await
    }

    /// Poll for new events once.
    pub async fn poll_once(&self) -> Result<PollResult, WatcherError> {
        self.inner.write().await.poll_once().await
    }

    /// Spawn a background task to poll for new events.
    ///
    /// Returns a handle that can be used to abort the task.
    pub fn spawn_polling_task(
        &self,
    ) -> tokio::task::JoinHandle<()> {
        let inner = Arc::clone(&self.inner);

        tokio::spawn(async move {
            loop {
                let interval = {
                    let watcher = inner.read().await;
                    Duration::from_secs(watcher.config().poll_interval_secs)
                };

                {
                    let mut watcher = inner.write().await;
                    if let Err(e) = watcher.poll_once().await {
                        tracing::warn!(
                            target: "event_watcher",
                            error = %e,
                            "Polling error"
                        );
                    }
                }

                tokio::time::sleep(interval).await;
            }
        })
    }
}

#[cfg(feature = "std")]
impl Clone for SharedEventWatcher {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_watcher_config_new() {
        let indexer_config = IndexerConfig::new(
            "https://rpc.example.com".to_string(),
            "0x1234".to_string(),
            "0x5678".to_string(),
        );

        let config = WatcherConfig::new(indexer_config, "/tmp/cache.json".to_string());

        assert_eq!(config.poll_interval_secs, DEFAULT_POLL_INTERVAL_SECS);
        assert_eq!(config.cache_path, "/tmp/cache.json");
    }

    #[test]
    fn test_watcher_config_with_poll_interval() {
        let indexer_config = IndexerConfig::new(
            "https://rpc.example.com".to_string(),
            "0x1234".to_string(),
            "0x5678".to_string(),
        );

        let config = WatcherConfig::new(indexer_config, "/tmp/cache.json".to_string())
            .with_poll_interval(30);

        assert_eq!(config.poll_interval_secs, 30);
    }

    #[test]
    fn test_poll_result_has_new_events() {
        let empty = PollResult::default();
        assert!(!empty.has_new_events());

        let with_indices = PollResult {
            new_indices: 1,
            ..Default::default()
        };
        assert!(with_indices.has_new_events());

        let with_pairs = PollResult {
            new_pairs: 2,
            ..Default::default()
        };
        assert!(with_pairs.has_new_events());
    }

    #[test]
    fn test_watcher_error_display() {
        let error = WatcherError::Stopped;
        assert!(error.to_string().contains("stopped"));

        let indexer_err = WatcherError::IndexerError(IndexerError::RpcError("test".to_string()));
        assert!(indexer_err.to_string().contains("Indexer"));
    }
}
