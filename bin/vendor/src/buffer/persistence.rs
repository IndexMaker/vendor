//! Persistence for buffer state (Story 3-6, AC #7)
//!
//! Atomic checkpoint/restore for crash recovery using temp file + rename.

use super::types::{BufferedOrder, BufferState};
use chrono::{DateTime, Utc};
use eyre::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use common::amount::Amount;

/// Checkpoint data for persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferCheckpoint {
    /// Version for forward compatibility
    pub version: u32,
    /// Timestamp of checkpoint
    pub timestamp: DateTime<Utc>,
    /// Pending orders
    pub pending_orders: Vec<BufferedOrder>,
    /// Buffered quantities per asset (for min-size handling)
    pub buffered_quantities: HashMap<String, u128>, // Stored as raw u128 for serialization
    /// Total orders processed
    pub total_processed: u64,
}

impl BufferCheckpoint {
    pub const CURRENT_VERSION: u32 = 1;

    /// Create checkpoint from current state
    pub fn from_state(
        pending_orders: Vec<BufferedOrder>,
        buffered_quantities: HashMap<String, Amount>,
        total_processed: u64,
    ) -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            timestamp: Utc::now(),
            pending_orders,
            buffered_quantities: buffered_quantities
                .into_iter()
                .map(|(k, v)| (k, v.to_u128_raw()))
                .collect(),
            total_processed,
        }
    }

    /// Convert buffered quantities back to Amount
    pub fn get_buffered_quantities(&self) -> HashMap<String, Amount> {
        self.buffered_quantities
            .iter()
            .map(|(k, v)| (k.clone(), Amount::from_u128_raw(*v)))
            .collect()
    }
}

/// Checkpoint current buffer state to disk (atomic write)
///
/// Uses temp file + rename pattern for atomic writes to prevent corruption.
pub async fn checkpoint(state: &BufferState, path: &Path) -> Result<()> {
    let checkpoint = BufferCheckpoint::from_state(
        state.pending_orders.clone(),
        state.buffered_quantities.clone(),
        state.total_processed,
    );

    // Serialize to JSON
    let json = serde_json::to_string_pretty(&checkpoint)
        .context("Failed to serialize checkpoint")?;

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("Failed to create checkpoint directory")?;
    }

    // Write to temp file first
    let temp_path = path.with_extension("tmp");
    tokio::fs::write(&temp_path, &json)
        .await
        .context("Failed to write temp checkpoint")?;

    // Atomic rename
    tokio::fs::rename(&temp_path, path)
        .await
        .context("Failed to rename checkpoint")?;

    tracing::debug!(
        "Checkpoint saved: {} orders, {} buffered assets",
        checkpoint.pending_orders.len(),
        checkpoint.buffered_quantities.len()
    );

    Ok(())
}

/// Restore buffer state from disk
pub async fn restore(path: &Path) -> Result<BufferState> {
    // Check if file exists
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        tracing::info!("No checkpoint file found at {:?}, starting fresh", path);
        return Ok(BufferState::new());
    }

    // Read file
    let json = tokio::fs::read_to_string(path)
        .await
        .context("Failed to read checkpoint file")?;

    // Deserialize
    let checkpoint: BufferCheckpoint = serde_json::from_str(&json)
        .context("Failed to parse checkpoint file")?;

    // Check version
    if checkpoint.version > BufferCheckpoint::CURRENT_VERSION {
        tracing::warn!(
            "Checkpoint version {} is newer than supported version {}",
            checkpoint.version,
            BufferCheckpoint::CURRENT_VERSION
        );
    }

    // Convert to BufferState
    let buffered_quantities = checkpoint.get_buffered_quantities();
    let state = BufferState {
        pending_orders: checkpoint.pending_orders,
        buffered_quantities,
        last_checkpoint: checkpoint.timestamp,
        total_processed: checkpoint.total_processed,
        total_buffered: 0, // Will be updated on next push
    };

    tracing::info!(
        "Restored checkpoint: {} orders, {} buffered assets (from {})",
        state.pending_orders.len(),
        state.buffered_quantities.len(),
        checkpoint.timestamp
    );

    Ok(state)
}

/// Delete checkpoint file (on clean shutdown)
pub async fn delete_checkpoint(path: &Path) -> Result<()> {
    if tokio::fs::try_exists(path).await.unwrap_or(false) {
        tokio::fs::remove_file(path)
            .await
            .context("Failed to delete checkpoint")?;
        tracing::debug!("Deleted checkpoint file: {:?}", path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::types::BufferOrderSide;
    use tempfile::tempdir;

    fn make_order(symbol: &str) -> BufferedOrder {
        BufferedOrder::new(
            1001,
            symbol.to_string(),
            BufferOrderSide::Buy,
            Amount::from_u128_raw(1_000_000_000_000_000_000),
            format!("corr-{}", symbol),
        )
    }

    #[tokio::test]
    async fn test_checkpoint_and_restore() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("buffer.json");

        // Create state with orders
        let mut state = BufferState::new();
        state.pending_orders.push(make_order("BTCUSDT"));
        state.pending_orders.push(make_order("ETHUSDT"));
        state.buffered_quantities.insert(
            "SOLUSDT".to_string(),
            Amount::from_u128_raw(500_000_000_000_000_000),
        );
        state.total_processed = 42;

        // Checkpoint
        checkpoint(&state, &path).await.unwrap();

        // Verify file exists
        assert!(path.exists());

        // Restore
        let restored = restore(&path).await.unwrap();

        assert_eq!(restored.pending_orders.len(), 2);
        assert_eq!(restored.pending_orders[0].symbol, "BTCUSDT");
        assert_eq!(restored.buffered_quantities.len(), 1);
        assert_eq!(restored.total_processed, 42);
    }

    #[tokio::test]
    async fn test_restore_missing_file() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("nonexistent.json");

        // Restore from missing file should return fresh state
        let state = restore(&path).await.unwrap();

        assert!(state.pending_orders.is_empty());
        assert_eq!(state.total_processed, 0);
    }

    #[tokio::test]
    async fn test_delete_checkpoint() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("buffer.json");

        // Create checkpoint
        let state = BufferState::new();
        checkpoint(&state, &path).await.unwrap();
        assert!(path.exists());

        // Delete
        delete_checkpoint(&path).await.unwrap();
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn test_atomic_write() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("buffer.json");

        let state = BufferState::new();
        checkpoint(&state, &path).await.unwrap();

        // Temp file should not exist after atomic rename
        let temp_path = path.with_extension("tmp");
        assert!(!temp_path.exists());
    }

    #[test]
    fn test_checkpoint_version() {
        let checkpoint = BufferCheckpoint::from_state(
            vec![],
            HashMap::new(),
            0,
        );
        assert_eq!(checkpoint.version, BufferCheckpoint::CURRENT_VERSION);
    }
}
