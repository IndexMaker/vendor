use super::types::{Index, IndexAsset};
use eyre::{eyre, Context, Result};
use serde_json;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub struct BasketManager {
    indices: HashMap<String, Index>,
}

impl BasketManager {
    pub fn new() -> Self {
        Self {
            indices: HashMap::new(),
        }
    }

    async fn load_index_file(&self, symbol: &str, path: &Path) -> Result<Index> {
        let content = tokio::fs::read_to_string(path)
            .await
            .context(format!("Failed to read index file: {:?}", path))?;

        let assets: Vec<IndexAsset> = serde_json::from_str(&content)
            .context(format!("Failed to parse index file: {:?}", path))?;

        tracing::info!(
            "Index '{}' contains {} asset(s)",
            symbol,
            assets.len()
        );

        Ok(Index::new(symbol.to_string(), assets))
    }   

    pub async fn load_from_config(config_dir: &str) -> Result<Self> {
        let mut manager = Self::new();

        // Load BasketManagerConfig.json
        let config_path = PathBuf::from(config_dir).join("BasketManagerConfig.json");

        tracing::info!("Loading basket configuration from: {:?}", config_path);

        let config_content = tokio::fs::read_to_string(&config_path)
            .await
            .context(format!("Failed to read config file: {:?}", config_path))?;

        let config: serde_json::Value = serde_json::from_str(&config_content)
            .context("Failed to parse BasketManagerConfig.json")?;

        // Get index_files array
        let index_files = config
            .get("index_files")
            .and_then(|v| v.as_array())
            .ok_or_else(|| eyre!("Missing or invalid 'index_files' in config"))?;

        // Load each index file
        for entry in index_files {
            let obj = entry
                .as_object()
                .ok_or_else(|| eyre!("Index file entry must be an object"))?;

            for (index_symbol, index_path_value) in obj {
                let index_path_str = index_path_value
                    .as_str()
                    .ok_or_else(|| eyre!("Index path must be a string"))?;

                // Use the path directly from the config (it's already complete)
                let index_path = PathBuf::from(index_path_str);

                tracing::info!(
                    "Loading index '{}' from: {:?}",
                    index_symbol,
                    index_path
                );

                let index = manager.load_index_file(index_symbol, &index_path).await?;
                manager.add_index(index);
            }
        }

        if manager.indices.is_empty() {
            return Err(eyre!("No indices loaded from configuration"));
        }

        tracing::info!("Loaded {} index(es)", manager.indices.len());

        Ok(manager)
    }   

    pub fn add_index(&mut self, index: Index) {
        self.indices.insert(index.symbol.clone(), index);
    }

    pub fn get_index(&self, symbol: &str) -> Option<&Index> {
        self.indices.get(symbol)
    }

    pub fn get_all_indices(&self) -> Vec<&Index> {
        self.indices.values().collect()
    }

    pub fn get_all_unique_symbols(&self) -> Vec<String> {
        let mut unique_symbols = HashSet::new();

        for index in self.indices.values() {
            for asset in &index.assets {
                unique_symbols.insert(asset.pair.clone());
            }
        }

        let mut symbols: Vec<String> = unique_symbols.into_iter().collect();
        symbols.sort();
        symbols
    }

    pub fn get_index_symbols(&self) -> Vec<String> {
        let mut symbols: Vec<String> = self.indices.keys().cloned().collect();
        symbols.sort();
        symbols
    }

    pub fn summary(&self) -> String {
        let index_count = self.indices.len();
        let asset_count = self.get_all_unique_symbols().len();
        format!(
            "BasketManager: {} index(es), {} unique asset(s)",
            index_count, asset_count
        )
    }
}