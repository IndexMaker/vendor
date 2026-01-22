//! AssetExtractor - Extracts asset IDs from indices via Stewart (Story 2.4)
//!
//! Implements Steps 3-5 from Keeper Architecture:
//! 3. Extract unique asset_ids from each index via ISteward::getIndexAssets()
//! 4. Create UNION of all asset_ids across all indexes
//! 5. Sort asset_ids (ascending for deterministic ordering)

use crate::chain::{ChainError, StewardClient};
use crate::index::IndexMapper;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

/// Errors that can occur during asset extraction (AC: #7)
#[derive(Debug)]
#[allow(dead_code)] // Variants reserved for future error handling
pub enum ExtractorError {
    /// Stewart contract call failed for a specific index
    StewardCallFailed { index_id: u128, reason: String },
    /// Index not found on chain
    IndexNotFound { index_id: u128 },
    /// Failed to decode asset bytes from Stewart
    ByteDecodeFailed { index_id: u128, reason: String },
    /// Underlying chain error
    ChainError(ChainError),
    /// All indices failed - no assets could be extracted
    AllIndicesFailed { failed_count: usize },
}

impl std::fmt::Display for ExtractorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractorError::StewardCallFailed { index_id, reason } => {
                write!(f, "Stewart call failed for index {}: {}", index_id, reason)
            }
            ExtractorError::IndexNotFound { index_id } => {
                write!(f, "Index {} not found on chain", index_id)
            }
            ExtractorError::ByteDecodeFailed { index_id, reason } => {
                write!(f, "Failed to decode assets for index {}: {}", index_id, reason)
            }
            ExtractorError::ChainError(e) => write!(f, "Chain error: {}", e),
            ExtractorError::AllIndicesFailed { failed_count } => {
                write!(f, "All {} indices failed during asset extraction", failed_count)
            }
        }
    }
}

impl std::error::Error for ExtractorError {}

impl From<ChainError> for ExtractorError {
    fn from(err: ChainError) -> Self {
        match &err {
            ChainError::VaultNotFound { index_id } => ExtractorError::IndexNotFound { index_id: *index_id },
            // Note: ChainError::StewardCallFailed doesn't include index_id, so we wrap as ChainError
            // to preserve the full error context. Use extract_assets_for_batch which provides
            // proper index_id context in its error handling.
            _ => ExtractorError::ChainError(err),
        }
    }
}

/// Result of asset extraction with detailed statistics
#[derive(Debug)]
#[allow(dead_code)] // Fields used for tracing/debugging via Debug output
pub struct ExtractionResult {
    /// Sorted unique asset IDs (union of all indices) - AC: #4, #5, #6
    pub asset_ids: Vec<u128>,
    /// Number of indices successfully processed
    pub successful_indices: usize,
    /// Number of indices that failed (skipped)
    pub failed_indices: usize,
    /// Index IDs that failed extraction
    pub failed_index_ids: Vec<u128>,
    /// Total extraction time in milliseconds
    pub extraction_time_ms: u64,
}

impl ExtractionResult {
    /// Create a new extraction result
    pub fn new(
        asset_ids: Vec<u128>,
        successful_indices: usize,
        failed_indices: usize,
        failed_index_ids: Vec<u128>,
        extraction_time_ms: u64,
    ) -> Self {
        Self {
            asset_ids,
            successful_indices,
            failed_indices,
            failed_index_ids,
            extraction_time_ms,
        }
    }

    /// Check if all indices succeeded
    #[allow(dead_code)] // Used in tests
    pub fn all_succeeded(&self) -> bool {
        self.failed_indices == 0
    }

    /// Check if result is empty (no assets extracted)
    #[allow(dead_code)] // Used in tests
    pub fn is_empty(&self) -> bool {
        self.asset_ids.is_empty()
    }

    /// Get log summary
    #[allow(dead_code)] // Used in tests
    pub fn log_summary(&self) -> String {
        format!(
            "{} unique assets from {} indices ({} failed) in {}ms",
            self.asset_ids.len(),
            self.successful_indices,
            self.failed_indices,
            self.extraction_time_ms
        )
    }
}

/// AssetExtractor - Extracts and unions asset IDs from multiple indices (AC: #4, #5, #6)
pub struct AssetExtractor {
    /// Stewart client for onchain queries
    steward_client: Arc<StewardClient>,
    /// Optional index mapper for event cache integration
    index_mapper: Option<Arc<IndexMapper>>,
}

impl AssetExtractor {
    /// Create a new AssetExtractor with StewardClient (AC: #1, #2)
    #[allow(dead_code)] // Used in tests
    pub fn new(steward_client: Arc<StewardClient>) -> Self {
        Self {
            steward_client,
            index_mapper: None,
        }
    }

    /// Create AssetExtractor with event cache integration (AC: #3)
    #[allow(dead_code)] // Reserved for future integration
    pub fn with_index_mapper(steward_client: Arc<StewardClient>, index_mapper: Arc<IndexMapper>) -> Self {
        Self {
            steward_client,
            index_mapper: Some(index_mapper),
        }
    }

    /// Set the index mapper for event cache integration
    #[allow(dead_code)] // Reserved for future integration
    pub fn set_index_mapper(&mut self, mapper: Arc<IndexMapper>) {
        self.index_mapper = Some(mapper);
    }

    /// Extract assets for a batch of index IDs (AC: #4, #5, #6, #7)
    ///
    /// Implements:
    /// - Query Stewart for each index's assets (AC: #1, #2)
    /// - Create union of all assets (AC: #4)
    /// - Sort ascending for deterministic ordering (AC: #5)
    /// - Return sorted unique array (AC: #6)
    /// - Handle failures gracefully - skip failed indices (AC: #7)
    pub async fn extract_assets_for_batch(
        &self,
        index_ids: &[u128],
    ) -> Result<ExtractionResult, ExtractorError> {
        let start = Instant::now();
        let correlation_id = format!("batch-{}", chrono::Utc::now().timestamp_millis());

        tracing::info!(
            correlation_id = %correlation_id,
            indices = ?index_ids,
            "Starting asset extraction for {} indices",
            index_ids.len()
        );

        if index_ids.is_empty() {
            tracing::warn!(correlation_id = %correlation_id, "Empty index_ids provided");
            return Ok(ExtractionResult::new(Vec::new(), 0, 0, Vec::new(), 0));
        }

        let mut all_assets: HashSet<u128> = HashSet::new();
        let mut failed_indices = Vec::new();
        let mut successful_count = 0;

        for &index_id in index_ids {
            // Check event cache first if available (AC: #3)
            if let Some(ref mapper) = self.index_mapper {
                if !mapper.has_cached_index(index_id) {
                    tracing::warn!(
                        correlation_id = %correlation_id,
                        index_id = index_id,
                        "Index not in event cache, attempting Stewart query anyway"
                    );
                } else if let Some(cached_index) = mapper.get_cached_index(index_id) {
                    tracing::debug!(
                        correlation_id = %correlation_id,
                        index_id = index_id,
                        vault_address = %cached_index.vault_address,
                        "Index found in event cache"
                    );
                }
            }

            // Query Stewart for assets (AC: #1, #2)
            match self.steward_client.get_index_assets(index_id).await {
                Ok(assets) => {
                    tracing::debug!(
                        correlation_id = %correlation_id,
                        index_id = index_id,
                        asset_count = assets.len(),
                        "Extracted {} assets from index {}",
                        assets.len(),
                        index_id
                    );

                    // Add to union (AC: #4)
                    all_assets.extend(assets);
                    successful_count += 1;
                }
                Err(e) => {
                    // Graceful handling - log and skip (AC: #7)
                    tracing::warn!(
                        correlation_id = %correlation_id,
                        index_id = index_id,
                        error = %e,
                        "Failed to extract assets for index {}, skipping",
                        index_id
                    );
                    failed_indices.push(index_id);
                }
            }
        }

        let elapsed = start.elapsed();

        // Check if all indices failed (AC: #7)
        if successful_count == 0 && !index_ids.is_empty() {
            tracing::error!(
                correlation_id = %correlation_id,
                failed_count = index_ids.len(),
                "All indices failed during asset extraction"
            );
            return Err(ExtractorError::AllIndicesFailed {
                failed_count: index_ids.len(),
            });
        }

        // Sort for deterministic ordering (AC: #5)
        let mut sorted_assets: Vec<u128> = all_assets.into_iter().collect();
        sorted_assets.sort_unstable();

        tracing::info!(
            correlation_id = %correlation_id,
            unique_assets = sorted_assets.len(),
            successful = successful_count,
            failed = failed_indices.len(),
            elapsed_ms = elapsed.as_millis(),
            "Asset extraction complete: {} unique assets from {} indices ({} failed)",
            sorted_assets.len(),
            successful_count,
            failed_indices.len()
        );

        Ok(ExtractionResult::new(
            sorted_assets,
            successful_count,
            failed_indices.len(),
            failed_indices,
            elapsed.as_millis() as u64,
        ))
    }

    /// Extract assets for a single index (helper method)
    #[allow(dead_code)] // Reserved for future use
    pub async fn extract_assets_for_index(&self, index_id: u128) -> Result<Vec<u128>, ExtractorError> {
        self.steward_client
            .get_index_assets(index_id)
            .await
            .map_err(|e| ExtractorError::StewardCallFailed {
                index_id,
                reason: e.to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::StewardClientConfig;

    fn make_test_client() -> Arc<StewardClient> {
        Arc::new(StewardClient::new(StewardClientConfig::default()))
    }

    #[test]
    fn test_extractor_creation() {
        let client = make_test_client();
        let extractor = AssetExtractor::new(client);
        assert!(extractor.index_mapper.is_none());
    }

    #[test]
    fn test_extraction_result_summary() {
        let result = ExtractionResult::new(
            vec![1, 2, 3, 4, 5],
            3,
            1,
            vec![999],
            150,
        );

        assert_eq!(result.asset_ids.len(), 5);
        assert!(!result.all_succeeded());
        assert!(!result.is_empty());

        let summary = result.log_summary();
        assert!(summary.contains("5 unique assets"));
        assert!(summary.contains("3 indices"));
        assert!(summary.contains("1 failed"));
        assert!(summary.contains("150ms"));
    }

    #[test]
    fn test_extraction_result_all_succeeded() {
        let result = ExtractionResult::new(
            vec![1, 2, 3],
            2,
            0,
            Vec::new(),
            50,
        );

        assert!(result.all_succeeded());
    }

    #[test]
    fn test_extraction_result_empty() {
        let result = ExtractionResult::new(
            Vec::new(),
            0,
            0,
            Vec::new(),
            0,
        );

        assert!(result.is_empty());
        assert!(result.all_succeeded());
    }

    #[tokio::test]
    async fn test_extract_empty_batch() {
        let client = make_test_client();
        let extractor = AssetExtractor::new(client);

        let result = extractor.extract_assets_for_batch(&[]).await.unwrap();

        assert!(result.is_empty());
        assert_eq!(result.successful_indices, 0);
        assert_eq!(result.failed_indices, 0);
    }

    #[test]
    fn test_extractor_error_display() {
        let err = ExtractorError::StewardCallFailed {
            index_id: 42,
            reason: "timeout".to_string(),
        };
        assert!(err.to_string().contains("42"));
        assert!(err.to_string().contains("timeout"));

        let err = ExtractorError::IndexNotFound { index_id: 99 };
        assert!(err.to_string().contains("99"));
        assert!(err.to_string().contains("not found"));

        let err = ExtractorError::AllIndicesFailed { failed_count: 5 };
        assert!(err.to_string().contains("5"));
    }
}
