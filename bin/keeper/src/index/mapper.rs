use common::event_cache::{CachedIndex, EventCacheReader, SharedEventCache};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Represents a trading index with its constituent assets
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexConfig {
    pub index_id: u128,
    pub name: String,
    pub assets: Vec<AssetWeight>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetWeight {
    pub asset_id: u128,
    pub weight: f64, // Normalized weight (should sum to 1.0 or be proportional)
}

/// Maps index IDs to their configurations
pub struct IndexMapper {
    indices: HashMap<u128, IndexConfig>,
    /// Optional event cache reader for discovering indices from chain events
    event_cache: Option<SharedEventCache>,
}

impl Default for IndexMapper {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexMapper {
    pub fn new() -> Self {
        Self {
            indices: HashMap::new(),
            event_cache: None,
        }
    }

    /// Load index configurations from a JSON file
    pub async fn load_from_file(path: &Path) -> eyre::Result<Self> {
        let contents = tokio::fs::read_to_string(path).await?;
        let configs: Vec<IndexConfig> = serde_json::from_str(&contents)?;

        let mut mapper = Self::new();
        for config in configs {
            mapper.add_index(config);
        }

        Ok(mapper)
    }

    /// Check if an index exists in the event cache
    pub fn has_cached_index(&self, index_id: u128) -> bool {
        self.event_cache
            .as_ref()
            .and_then(|c| c.get_index(index_id))
            .is_some()
    }

    /// Get a cached index by ID
    pub fn get_cached_index(&self, index_id: u128) -> Option<CachedIndex> {
        self.event_cache.as_ref()?.get_index(index_id)
    }

    /// Add an index configuration
    pub fn add_index(&mut self, config: IndexConfig) {
        self.indices.insert(config.index_id, config);
    }

    /// Get index configuration by ID
    pub fn get_index(&self, index_id: u128) -> Option<&IndexConfig> {
        self.indices.get(&index_id)
    }

    /// Get all asset IDs across all indices
    pub fn get_all_asset_ids(&self) -> Vec<u128> {
        let mut asset_ids: Vec<u128> = self
            .indices
            .values()
            .flat_map(|idx| idx.assets.iter().map(|a| a.asset_id))
            .collect();

        asset_ids.sort_unstable();
        asset_ids.dedup();
        asset_ids
    }

    /// Get all index IDs
    pub fn get_all_index_ids(&self) -> Vec<u128> {
        self.indices.keys().copied().collect()
    }

    /// Get asset IDs for a specific index
    pub fn get_index_assets(&self, index_id: u128) -> Option<Vec<u128>> {
        self.indices.get(&index_id).map(|idx| {
            idx.assets.iter().map(|a| a.asset_id).collect()
        })
    }

    pub fn len(&self) -> usize {
        self.indices.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_index_mapper() {
        let mut mapper = IndexMapper::new();

        let index = IndexConfig {
            index_id: 1001,
            name: "Test Index".to_string(),
            assets: vec![
                AssetWeight { asset_id: 101, weight: 0.5 },
                AssetWeight { asset_id: 102, weight: 0.3 },
                AssetWeight { asset_id: 103, weight: 0.2 },
            ],
        };

        mapper.add_index(index);

        assert_eq!(mapper.len(), 1);
        assert_eq!(mapper.get_all_asset_ids(), vec![101, 102, 103]);
        assert_eq!(mapper.get_index_assets(1001), Some(vec![101, 102, 103]));
    }
}