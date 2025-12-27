use eyre::{eyre, Context, Result};
use serde_json;
use std::collections::HashMap;
use std::path::Path;

pub struct IndexMapper {
    index_to_id: HashMap<String, u128>,
}

impl IndexMapper {
    pub async fn load_from_file(path: &Path) -> Result<Self> {
        tracing::info!("Loading index ID mapping from: {:?}", path);

        let content = tokio::fs::read_to_string(path)
            .await
            .context(format!("Failed to read index mapping file: {:?}", path))?;

        let index_to_id: HashMap<String, u128> = serde_json::from_str(&content)
            .context("Failed to parse index_ids.json")?;

        tracing::info!("Loaded {} index ID mappings", index_to_id.len());

        Ok(Self { index_to_id })
    }

    pub fn get_index_id(&self, symbol: &str) -> Option<u128> {
        self.index_to_id.get(symbol).copied()
    }

    pub fn validate_index_exists(&self, symbol: &str) -> Result<u128> {
        self.get_index_id(symbol)
            .ok_or_else(|| eyre!("Index '{}' not found in index mapping", symbol))
    }

    pub fn validate_all_indices(&self, symbols: &[String]) -> Result<()> {
        let mut missing = Vec::new();

        for symbol in symbols {
            if !self.index_to_id.contains_key(symbol) {
                missing.push(symbol.clone());
            }
        }

        if !missing.is_empty() {
            return Err(eyre!(
                "The following indices are missing from index mapping: {:?}",
                missing
            ));
        }

        Ok(())
    }
}