//! Event cache persistence manager.
//!
//! Handles loading and saving the event cache to/from a JSON file.
//! Provides atomic writes to prevent corruption.

use alloc::string::{String, ToString};

use super::types::EventCache;

/// Default path for the event cache file
pub const DEFAULT_CACHE_PATH: &str = "./event-cache.json";

/// Environment variable name for cache path configuration
pub const CACHE_PATH_ENV_VAR: &str = "EVENT_CACHE_PATH";

/// Errors that can occur during cache operations
#[derive(Debug)]
pub enum CacheError {
    /// Failed to read cache file
    ReadError { path: String, reason: String },
    /// Failed to write cache file
    WriteError { path: String, reason: String },
    /// Failed to parse cache JSON
    ParseError { reason: String },
    /// Failed to serialize cache to JSON
    SerializeError { reason: String },
    /// Failed to create temporary file for atomic write
    TempFileError { reason: String },
    /// Failed to rename temporary file
    RenameError { from: String, to: String, reason: String },
}

impl core::fmt::Display for CacheError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CacheError::ReadError { path, reason } => {
                write!(f, "Failed to read cache file '{}': {}", path, reason)
            }
            CacheError::WriteError { path, reason } => {
                write!(f, "Failed to write cache file '{}': {}", path, reason)
            }
            CacheError::ParseError { reason } => {
                write!(f, "Failed to parse cache JSON: {}", reason)
            }
            CacheError::SerializeError { reason } => {
                write!(f, "Failed to serialize cache: {}", reason)
            }
            CacheError::TempFileError { reason } => {
                write!(f, "Failed to create temp file: {}", reason)
            }
            CacheError::RenameError { from, to, reason } => {
                write!(f, "Failed to rename '{}' to '{}': {}", from, to, reason)
            }
        }
    }
}

/// Configuration for the EventCacheManager
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Path to the cache file
    pub cache_path: String,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            cache_path: DEFAULT_CACHE_PATH.to_string(),
        }
    }
}

impl CacheConfig {
    /// Create a new configuration with the specified cache path
    pub fn new(cache_path: String) -> Self {
        Self { cache_path }
    }

    /// Create configuration from environment variable, falling back to default
    #[cfg(feature = "std")]
    pub fn from_env() -> Self {
        let cache_path = std::env::var(CACHE_PATH_ENV_VAR)
            .unwrap_or_else(|_| DEFAULT_CACHE_PATH.to_string());
        Self { cache_path }
    }
}

/// Manages loading and saving of the event cache.
///
/// Provides atomic writes to prevent corruption during save operations.
/// The cache is stored as a JSON file.
#[derive(Debug)]
pub struct EventCacheManager {
    config: CacheConfig,
}

impl EventCacheManager {
    /// Create a new cache manager with the given configuration
    pub fn new(config: CacheConfig) -> Self {
        Self { config }
    }

    /// Create a new cache manager with default configuration
    pub fn with_default_config() -> Self {
        Self::new(CacheConfig::default())
    }

    /// Create a new cache manager from environment configuration
    #[cfg(feature = "std")]
    pub fn from_env() -> Self {
        Self::new(CacheConfig::from_env())
    }

    /// Get the cache file path
    pub fn cache_path(&self) -> &str {
        &self.config.cache_path
    }

    /// Load the event cache from disk.
    ///
    /// If the file doesn't exist, returns an empty cache.
    /// If the file exists but is invalid, returns an error.
    #[cfg(feature = "std")]
    pub fn load(&self) -> Result<EventCache, CacheError> {
        use std::fs;
        use std::path::Path;

        let path = Path::new(&self.config.cache_path);

        // If file doesn't exist, return empty cache
        if !path.exists() {
            return Ok(EventCache::new());
        }

        // Read the file contents
        let contents = fs::read_to_string(path).map_err(|e| CacheError::ReadError {
            path: self.config.cache_path.clone(),
            reason: e.to_string(),
        })?;

        // Parse JSON
        serde_json::from_str(&contents).map_err(|e| CacheError::ParseError {
            reason: e.to_string(),
        })
    }

    /// Save the event cache to disk atomically.
    ///
    /// Uses write-to-temp-then-rename pattern to prevent corruption.
    #[cfg(feature = "std")]
    pub fn save(&self, cache: &EventCache) -> Result<(), CacheError> {
        use std::fs;
        use std::io::Write;
        use std::path::Path;

        let path = Path::new(&self.config.cache_path);

        // Create parent directories if they don't exist
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| CacheError::WriteError {
                    path: self.config.cache_path.clone(),
                    reason: format!("Failed to create parent directories: {}", e),
                })?;
            }
        }

        // Serialize to JSON with pretty printing for readability
        let json = serde_json::to_string_pretty(cache).map_err(|e| CacheError::SerializeError {
            reason: e.to_string(),
        })?;

        // Create temp file path
        let temp_path = format!("{}.tmp", self.config.cache_path);
        let temp_path_ref = Path::new(&temp_path);

        // Write to temp file
        let mut file = fs::File::create(temp_path_ref).map_err(|e| CacheError::TempFileError {
            reason: e.to_string(),
        })?;

        file.write_all(json.as_bytes())
            .map_err(|e| CacheError::WriteError {
                path: temp_path.clone(),
                reason: e.to_string(),
            })?;

        // Sync to ensure all data is written
        file.sync_all().map_err(|e| CacheError::WriteError {
            path: temp_path.clone(),
            reason: format!("Failed to sync file: {}", e),
        })?;

        // Atomic rename
        fs::rename(temp_path_ref, path).map_err(|e| CacheError::RenameError {
            from: temp_path.clone(),
            to: self.config.cache_path.clone(),
            reason: e.to_string(),
        })?;

        Ok(())
    }

    /// Load the cache, returning an empty cache on any error
    #[cfg(feature = "std")]
    pub fn load_or_default(&self) -> EventCache {
        self.load().unwrap_or_default()
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::event_cache::types::{CachedIndex, CachedPair};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Counter for unique test file paths
    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_temp_cache_path() -> String {
        let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "event_cache_test_{}_{}.json",
            std::process::id(),
            counter
        ));
        path.to_string_lossy().to_string()
    }

    fn cleanup_temp_file(path: &str) {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(format!("{}.tmp", path));
    }

    #[test]
    fn test_load_nonexistent_file() {
        let path = unique_temp_cache_path();
        cleanup_temp_file(&path);

        let manager = EventCacheManager::new(CacheConfig::new(path.clone()));
        let cache = manager.load().unwrap();

        assert_eq!(cache.index_count(), 0);
        assert_eq!(cache.pair_count(), 0);

        cleanup_temp_file(&path);
    }

    #[test]
    fn test_save_and_load() {
        let path = unique_temp_cache_path();
        cleanup_temp_file(&path);

        let manager = EventCacheManager::new(CacheConfig::new(path.clone()));

        // Create cache with data
        let mut cache = EventCache::new();
        cache.add_index(CachedIndex {
            index_id: 1,
            name: "Test Index".into(),
            symbol: "TEST".into(),
            vault_address: "0x123".into(),
            block_number: 100,
            tx_hash: "0xabc".into(),
        });
        cache.add_pair(CachedPair {
            pair_id: 1,
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            block_number: 150,
            tx_hash: "0xdef".into(),
        });

        // Save
        manager.save(&cache).unwrap();

        // Verify file exists
        assert!(PathBuf::from(&path).exists());

        // Load and verify
        let loaded = manager.load().unwrap();
        assert_eq!(loaded.index_count(), 1);
        assert_eq!(loaded.pair_count(), 1);
        assert_eq!(loaded.get_index(1).unwrap().name, "Test Index");
        assert_eq!(loaded.get_pair(1).unwrap().symbol, "BTCUSDT");

        cleanup_temp_file(&path);
    }

    #[test]
    fn test_atomic_save() {
        let path = unique_temp_cache_path();
        cleanup_temp_file(&path);

        let manager = EventCacheManager::new(CacheConfig::new(path.clone()));

        // First save
        let mut cache = EventCache::new();
        cache.add_index(CachedIndex {
            index_id: 1,
            name: "First".into(),
            symbol: "F".into(),
            vault_address: "0x1".into(),
            block_number: 100,
            tx_hash: "0xa".into(),
        });
        manager.save(&cache).unwrap();

        // Second save (update)
        cache.add_index(CachedIndex {
            index_id: 2,
            name: "Second".into(),
            symbol: "S".into(),
            vault_address: "0x2".into(),
            block_number: 200,
            tx_hash: "0xb".into(),
        });
        manager.save(&cache).unwrap();

        // Verify temp file doesn't exist (was renamed)
        assert!(!PathBuf::from(format!("{}.tmp", path)).exists());

        // Verify final file has both entries
        let loaded = manager.load().unwrap();
        assert_eq!(loaded.index_count(), 2);

        cleanup_temp_file(&path);
    }

    #[test]
    fn test_load_or_default() {
        let path = unique_temp_cache_path();
        cleanup_temp_file(&path);

        let manager = EventCacheManager::new(CacheConfig::new(path.clone()));

        // Should return empty cache for nonexistent file
        let cache = manager.load_or_default();
        assert_eq!(cache.index_count(), 0);

        cleanup_temp_file(&path);
    }

    #[test]
    fn test_cache_path_getter() {
        let path = "/custom/path/cache.json";
        let manager = EventCacheManager::new(CacheConfig::new(path.to_string()));
        assert_eq!(manager.cache_path(), path);
    }
}
