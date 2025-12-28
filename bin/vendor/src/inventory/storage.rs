use super::types::Position;
use eyre::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventorySnapshot {
    pub positions: HashMap<String, Position>,
}

impl InventorySnapshot {
    pub fn new() -> Self {
        Self {
            positions: HashMap::new(),
        }
    }

    pub async fn save_to_file(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)
            .context("Failed to serialize inventory snapshot")?;

        tokio::fs::write(path, json)
            .await
            .context(format!("Failed to write inventory to {:?}", path))?;

        tracing::debug!("Inventory snapshot saved to {:?}", path);
        Ok(())
    }

    pub async fn load_from_file(path: &Path) -> Result<Self> {
        if !path.exists() {
            tracing::info!("Inventory file not found, starting with empty inventory");
            return Ok(Self::new());
        }

        let content = tokio::fs::read_to_string(path)
            .await
            .context(format!("Failed to read inventory from {:?}", path))?;

        let snapshot: Self = serde_json::from_str(&content)
            .context("Failed to deserialize inventory snapshot")?;

        tracing::info!(
            "Inventory snapshot loaded from {:?} ({} positions)",
            path,
            snapshot.positions.len()
        );

        Ok(snapshot)
    }
}