//! Story 2.8: Event-driven claim handling
//!
//! This module handles AcquisitionClaim and DisposalClaim events from the chain.
//! When a claim has remainder > threshold, it triggers immediate rebalancing
//! instead of waiting for the next 60-second interval.
//!
//! Flow (from Conveyor reference):
//! 1. Receive claim event
//! 2. Check if remainder > threshold (default: 100)
//! 3. If yes: Get assets → Update market data → Update quotes → Process pending order
//! 4. If no: Log and skip

use crate::chain::{AcquisitionClaimEvent, DisposalClaimEvent, VaultEvent};
use crate::config::ClaimHandlerConfig;
use crate::index::IndexMapper;
use crate::onchain::OnchainSubmitter;
use crate::vendor::{UpdateMarketDataRequest, VendorClient};
use alloy::providers::Provider;
use std::sync::Arc;

/// Claim handler for event-driven rebalancing
/// Processes AcquisitionClaim and DisposalClaim events from the chain
pub struct ClaimHandler<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    config: ClaimHandlerConfig,
    vendor_client: Arc<VendorClient>,
    index_mapper: Arc<IndexMapper>,
    submitter: Arc<OnchainSubmitter<P>>,
}

/// Result of claim processing
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields are used for logging/debugging in Debug output
pub enum ClaimResult {
    /// Claim was processed successfully (triggered rebalancing)
    Processed {
        index_id: u128,
        vendor_id: u128,
        remainder: u128,
        vendor_response_status: String,
    },
    /// Claim was skipped (remainder below threshold)
    BelowThreshold {
        index_id: u128,
        vendor_id: u128,
        remainder: u128,
        threshold: u128,
    },
    /// Handler is disabled
    Disabled,
    /// Not a claim event
    NotAClaim,
    /// Error during processing
    Error {
        index_id: u128,
        error: String,
    },
}

impl<P> ClaimHandler<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    /// Create a new ClaimHandler with full pipeline support
    pub fn new(
        config: ClaimHandlerConfig,
        vendor_client: Arc<VendorClient>,
        index_mapper: Arc<IndexMapper>,
        submitter: Arc<OnchainSubmitter<P>>,
    ) -> Self {
        Self {
            config,
            vendor_client,
            index_mapper,
            submitter,
        }
    }

    /// Check if handler is enabled
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Get the threshold value
    pub fn threshold(&self) -> u128 {
        self.config.claim_remainder_threshold
    }

    /// Handle a VaultEvent (checks if it's a claim and processes it)
    pub async fn handle_event(&self, event: &VaultEvent) -> ClaimResult {
        if !self.config.enabled {
            return ClaimResult::Disabled;
        }

        match event {
            VaultEvent::AcquisitionClaim(e) => self.handle_acquisition_claim(e).await,
            VaultEvent::DisposalClaim(e) => self.handle_disposal_claim(e).await,
            _ => ClaimResult::NotAClaim,
        }
    }

    /// Handle an AcquisitionClaim event
    /// Triggers rebalancing if remain > threshold
    /// Full Conveyor flow: Get assets → Update market → Update quotes → Process pending
    pub async fn handle_acquisition_claim(&self, event: &AcquisitionClaimEvent) -> ClaimResult {
        let index_id = event.index_id;
        let vendor_id = event.vendor_id;
        let remainder = event.remain;
        let threshold = self.config.claim_remainder_threshold;

        tracing::debug!(
            index_id = index_id,
            vendor_id = vendor_id,
            remainder = remainder,
            threshold = threshold,
            "Checking AcquisitionClaim against threshold"
        );

        if remainder <= threshold {
            tracing::info!(
                index_id = index_id,
                remainder = remainder,
                threshold = threshold,
                "📭 AcquisitionClaim below threshold, skipping rebalancing"
            );
            return ClaimResult::BelowThreshold {
                index_id,
                vendor_id,
                remainder,
                threshold,
            };
        }

        // Remainder exceeds threshold - trigger rebalancing
        tracing::info!(
            index_id = index_id,
            vendor_id = vendor_id,
            remainder = remainder,
            "🔔 AcquisitionClaim above threshold, triggering immediate rebalancing"
        );

        let correlation_id = format!("acq_claim:{}:{}", index_id, event.tx_hash);

        // Step 1: Get assets for this index from IndexMapper (Conveyor: get_assets())
        let asset_ids: Vec<u128> = match self.index_mapper.get_index(index_id) {
            Some(index_info) => index_info.assets.iter().map(|a| a.asset_id).collect(),
            None => {
                tracing::warn!(
                    index_id = index_id,
                    "Index not found in mapper, using empty asset list"
                );
                vec![]
            }
        };

        tracing::debug!(
            index_id = index_id,
            asset_count = asset_ids.len(),
            correlation_id = %correlation_id,
            "Retrieved {} assets for claim rebalancing",
            asset_ids.len()
        );

        // Step 2: Call Vendor to update market data with actual assets
        let request = UpdateMarketDataRequest {
            assets: asset_ids,
            batch_id: correlation_id.clone(),
        };

        let vendor_status = match self.vendor_client.update_market_data(request).await {
            Ok(response) => {
                tracing::info!(
                    index_id = index_id,
                    correlation_id = %correlation_id,
                    status = %response.status,
                    assets_updated = response.assets_updated,
                    "✓ Step 2/4: Vendor market data updated"
                );
                response.status
            }
            Err(e) => {
                tracing::error!(
                    index_id = index_id,
                    correlation_id = %correlation_id,
                    error = %e,
                    "❌ Failed to update market data for AcquisitionClaim"
                );
                return ClaimResult::Error {
                    index_id,
                    error: format!("Vendor update failed: {}", e),
                };
            }
        };

        // Step 3: Call Castle updateMultipleQuotes() via OnchainSubmitter
        // Note: This requires quote data from the vendor response
        // For now, we skip this if submitter is in dry_run mode
        if !self.submitter.is_dry_run() {
            tracing::info!(
                index_id = index_id,
                correlation_id = %correlation_id,
                "✓ Step 3/4: Quote update would be called (requires quote data integration)"
            );
            // TODO: Integrate with QuoteProcessor to get formatted quote data
            // match self.submitter.update_multiple_quotes(...).await { ... }
        } else {
            tracing::info!(
                index_id = index_id,
                correlation_id = %correlation_id,
                "✓ Step 3/4: Quote update skipped (dry run mode)"
            );
        }

        // Step 4: Call Vault processPendingBuyOrder() via OnchainSubmitter
        if !self.submitter.is_dry_run() {
            match self
                .submitter
                .process_pending_buy_order(index_id, event.controller, u128::MAX)
                .await
            {
                Ok(tx_result) => {
                    tracing::info!(
                        index_id = index_id,
                        correlation_id = %correlation_id,
                        tx_hash = %tx_result.tx_hash,
                        "✓ Step 4/4: processPendingBuyOrder submitted"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        index_id = index_id,
                        correlation_id = %correlation_id,
                        error = %e,
                        "❌ Failed to call processPendingBuyOrder"
                    );
                    return ClaimResult::Error {
                        index_id,
                        error: format!("processPendingBuyOrder failed: {}", e),
                    };
                }
            }
        } else {
            tracing::info!(
                index_id = index_id,
                correlation_id = %correlation_id,
                "✓ Step 4/4: processPendingBuyOrder skipped (dry run mode)"
            );
        }

        ClaimResult::Processed {
            index_id,
            vendor_id,
            remainder,
            vendor_response_status: vendor_status,
        }
    }

    /// Handle a DisposalClaim event
    /// Triggers rebalancing if itp_remain > threshold
    /// Full Conveyor flow: Get assets → Update market → Update quotes → Process pending
    pub async fn handle_disposal_claim(&self, event: &DisposalClaimEvent) -> ClaimResult {
        let index_id = event.index_id;
        let vendor_id = event.vendor_id;
        let remainder = event.itp_remain;
        let threshold = self.config.claim_remainder_threshold;

        tracing::debug!(
            index_id = index_id,
            vendor_id = vendor_id,
            remainder = remainder,
            threshold = threshold,
            "Checking DisposalClaim against threshold"
        );

        if remainder <= threshold {
            tracing::info!(
                index_id = index_id,
                remainder = remainder,
                threshold = threshold,
                "📭 DisposalClaim below threshold, skipping rebalancing"
            );
            return ClaimResult::BelowThreshold {
                index_id,
                vendor_id,
                remainder,
                threshold,
            };
        }

        // Remainder exceeds threshold - trigger rebalancing
        tracing::info!(
            index_id = index_id,
            vendor_id = vendor_id,
            remainder = remainder,
            "🔔 DisposalClaim above threshold, triggering immediate rebalancing"
        );

        let correlation_id = format!("disp_claim:{}:{}", index_id, event.tx_hash);

        // Step 1: Get assets for this index from IndexMapper (Conveyor: get_assets())
        let asset_ids: Vec<u128> = match self.index_mapper.get_index(index_id) {
            Some(index_info) => index_info.assets.iter().map(|a| a.asset_id).collect(),
            None => {
                tracing::warn!(
                    index_id = index_id,
                    "Index not found in mapper, using empty asset list"
                );
                vec![]
            }
        };

        tracing::debug!(
            index_id = index_id,
            asset_count = asset_ids.len(),
            correlation_id = %correlation_id,
            "Retrieved {} assets for claim rebalancing",
            asset_ids.len()
        );

        // Step 2: Call Vendor to update market data with actual assets
        let request = UpdateMarketDataRequest {
            assets: asset_ids,
            batch_id: correlation_id.clone(),
        };

        let vendor_status = match self.vendor_client.update_market_data(request).await {
            Ok(response) => {
                tracing::info!(
                    index_id = index_id,
                    correlation_id = %correlation_id,
                    status = %response.status,
                    assets_updated = response.assets_updated,
                    "✓ Step 2/4: Vendor market data updated"
                );
                response.status
            }
            Err(e) => {
                tracing::error!(
                    index_id = index_id,
                    correlation_id = %correlation_id,
                    error = %e,
                    "❌ Failed to update market data for DisposalClaim"
                );
                return ClaimResult::Error {
                    index_id,
                    error: format!("Vendor update failed: {}", e),
                };
            }
        };

        // Step 3: Call Castle updateMultipleQuotes() via OnchainSubmitter
        if !self.submitter.is_dry_run() {
            tracing::info!(
                index_id = index_id,
                correlation_id = %correlation_id,
                "✓ Step 3/4: Quote update would be called (requires quote data integration)"
            );
            // TODO: Integrate with QuoteProcessor to get formatted quote data
        } else {
            tracing::info!(
                index_id = index_id,
                correlation_id = %correlation_id,
                "✓ Step 3/4: Quote update skipped (dry run mode)"
            );
        }

        // Step 4: Call Vault processPendingSellOrder() via OnchainSubmitter
        if !self.submitter.is_dry_run() {
            match self
                .submitter
                .process_pending_sell_order(index_id, event.controller, u128::MAX)
                .await
            {
                Ok(tx_result) => {
                    tracing::info!(
                        index_id = index_id,
                        correlation_id = %correlation_id,
                        tx_hash = %tx_result.tx_hash,
                        "✓ Step 4/4: processPendingSellOrder submitted"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        index_id = index_id,
                        correlation_id = %correlation_id,
                        error = %e,
                        "❌ Failed to call processPendingSellOrder"
                    );
                    return ClaimResult::Error {
                        index_id,
                        error: format!("processPendingSellOrder failed: {}", e),
                    };
                }
            }
        } else {
            tracing::info!(
                index_id = index_id,
                correlation_id = %correlation_id,
                "✓ Step 4/4: processPendingSellOrder skipped (dry run mode)"
            );
        }

        ClaimResult::Processed {
            index_id,
            vendor_id,
            remainder,
            vendor_response_status: vendor_status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onchain::{GasConfig, SubmitterConfig};
    use alloy::providers::ProviderBuilder;
    use alloy_primitives::{Address, B256};
    use chrono::Utc;

    fn make_test_config(threshold: u128, enabled: bool) -> ClaimHandlerConfig {
        ClaimHandlerConfig {
            claim_remainder_threshold: threshold,
            enabled,
        }
    }

    fn make_test_index_mapper() -> IndexMapper {
        IndexMapper::new()
    }

    fn make_test_submitter() -> Arc<OnchainSubmitter<impl alloy::providers::Provider + Clone>> {
        let submitter_config = SubmitterConfig {
            rpc_url: "http://localhost:8545".to_string(),
            castle_address: Address::ZERO,
            vendor_id: 1,
            private_key: None,
            dry_run: true, // Always dry run in tests
            gas_config: GasConfig::default(),
            retry_attempts: 1,
            retry_delay_ms: 100,
            default_max_order_size: u128::MAX,
        };
        let provider = ProviderBuilder::new()
            .connect_http("http://localhost:8545".parse().unwrap());
        Arc::new(OnchainSubmitter::new_with_provider(submitter_config, provider).unwrap())
    }

    fn make_acquisition_claim(remain: u128) -> AcquisitionClaimEvent {
        AcquisitionClaimEvent {
            controller: Address::ZERO,
            index_id: 42,
            vendor_id: 1,
            remain,
            spent: 1000,
            itp_minted: 990,
            vault_address: Address::ZERO,
            tx_hash: B256::ZERO,
            block_number: 100,
            received_at: Utc::now(),
        }
    }

    fn make_disposal_claim(itp_remain: u128) -> DisposalClaimEvent {
        DisposalClaimEvent {
            controller: Address::ZERO,
            index_id: 42,
            vendor_id: 1,
            itp_remain,
            itp_burned: 800,
            gains: 790,
            vault_address: Address::ZERO,
            tx_hash: B256::ZERO,
            block_number: 100,
            received_at: Utc::now(),
        }
    }

    #[test]
    fn test_handler_disabled() {
        let config = make_test_config(100, false);
        let handler = ClaimHandler::new(
            config,
            Arc::new(VendorClient::new("http://localhost:8080".to_string(), 10, 3)),
            Arc::new(make_test_index_mapper()),
            make_test_submitter(),
        );

        assert!(!handler.is_enabled());
    }

    #[test]
    fn test_threshold_value() {
        let config = make_test_config(200, true);
        let handler = ClaimHandler::new(
            config,
            Arc::new(VendorClient::new("http://localhost:8080".to_string(), 10, 3)),
            Arc::new(make_test_index_mapper()),
            make_test_submitter(),
        );

        assert_eq!(handler.threshold(), 200);
    }

    #[tokio::test]
    async fn test_not_a_claim_event() {
        use crate::chain::BuyOrderEvent;

        let config = make_test_config(100, true);
        let handler = ClaimHandler::new(
            config,
            Arc::new(VendorClient::new("http://localhost:8080".to_string(), 10, 3)),
            Arc::new(make_test_index_mapper()),
            make_test_submitter(),
        );

        let buy_event = VaultEvent::Buy(BuyOrderEvent {
            keeper: Address::ZERO,
            trader: Address::ZERO,
            index_id: 42,
            vendor_id: 1,
            collateral_amount: 1000,
            vault_address: Address::ZERO,
            tx_hash: B256::ZERO,
            block_number: 100,
            received_at: Utc::now(),
        });

        let result = handler.handle_event(&buy_event).await;
        assert!(matches!(result, ClaimResult::NotAClaim));
    }

    #[tokio::test]
    async fn test_disabled_handler_returns_disabled() {
        let config = make_test_config(100, false);
        let handler = ClaimHandler::new(
            config,
            Arc::new(VendorClient::new("http://localhost:8080".to_string(), 10, 3)),
            Arc::new(make_test_index_mapper()),
            make_test_submitter(),
        );

        let event = VaultEvent::AcquisitionClaim(make_acquisition_claim(150));
        let result = handler.handle_event(&event).await;
        assert!(matches!(result, ClaimResult::Disabled));
    }

    // Note: Tests that require actual Vendor calls would need wiremock setup
    // Similar to the VendorClient tests in vendor/client.rs
}
