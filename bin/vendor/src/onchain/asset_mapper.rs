use eyre::{eyre, Context, Result};
use serde_json;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct AssetMapper {
    symbol_to_id: HashMap<String, u128>,
    id_to_symbol: HashMap<u128, String>,
}

impl AssetMapper {
    pub async fn load_from_file(path: &Path) -> Result<Self> {
        tracing::info!("Loading asset ID mapping from: {:?}", path);

        let content = tokio::fs::read_to_string(path)
            .await
            .context(format!("Failed to read asset mapping file: {:?}", path))?;

        let symbol_to_id: HashMap<String, u128> = serde_json::from_str(&content)
            .context("Failed to parse assets.json")?;

        // Create reverse mapping
        let id_to_symbol: HashMap<u128, String> = symbol_to_id
            .iter()
            .map(|(k, v)| (*v, k.clone()))
            .collect();

        tracing::info!("Loaded {} asset ID mappings", symbol_to_id.len());

        Ok(Self {
            symbol_to_id,
            id_to_symbol,
        })
    }

    pub fn get_id(&self, symbol: &str) -> Option<u128> {
        self.symbol_to_id.get(symbol).copied()
    }

    pub fn get_symbol(&self, id: u128) -> Option<&String> {
        self.id_to_symbol.get(&id)
    }

    pub fn get_sorted_ids(&self, symbols: &[String]) -> Result<Vec<u128>> {
        let mut ids: Vec<u128> = Vec::new();

        for symbol in symbols {
            let id = self
                .get_id(symbol)
                .ok_or_else(|| eyre!("Asset '{}' not found in asset mapping", symbol))?;
            ids.push(id);
        }

        ids.sort();
        Ok(ids)
    }

    pub fn validate_all_mapped(&self, symbols: &[String]) -> Result<()> {
        let mut missing = Vec::new();

        for symbol in symbols {
            if !self.symbol_to_id.contains_key(symbol) {
                missing.push(symbol.clone());
            }
        }

        if !missing.is_empty() {
            return Err(eyre!(
                "The following assets are missing from asset mapping: {:?}",
                missing
            ));
        }

        Ok(())
    }

    pub fn get_all_symbols(&self) -> Vec<String> {
        let mut symbols: Vec<String> = self.symbol_to_id.keys().cloned().collect();
        symbols.sort();
        symbols
    }
}