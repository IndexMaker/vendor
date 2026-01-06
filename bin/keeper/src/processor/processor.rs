use super::types::{AssetAllocation, AssetMarketData, IndexBuyOrder, MarketDataSnapshot, SubmissionPayload};
use crate::accumulator::{IndexState, OrderBatch};
use crate::index::IndexMapper;
use crate::vendor::{AssetsQuote, VendorClient};
use common::amount::Amount;
use std::sync::Arc;

/// Configuration for quote processor
#[derive(Debug, Clone)]
pub struct ProcessorConfig {
    pub vendor_id: u128,
    pub max_order_size_multiplier: f64, // e.g., 2.0 = 200% of net collateral
}

impl Default for ProcessorConfig {
    fn default() -> Self {
        Self {
            vendor_id: 1,
            max_order_size_multiplier: 2.0,
        }
    }
}

/// Quote processor - converts batches + vendor quotes into on-chain payloads
pub struct QuoteProcessor {
    config: ProcessorConfig,
    pub(crate) index_mapper: Arc<IndexMapper>,
    vendor_client: Arc<VendorClient>,
}

impl QuoteProcessor {
    pub fn new(
        config: ProcessorConfig,
        index_mapper: Arc<IndexMapper>,
        vendor_client: Arc<VendorClient>,
    ) -> Self {
        Self {
            config,
            index_mapper,
            vendor_client,
        }
    }

    /// Process a batch of orders into a submission payload
    pub async fn process_batch(&self, batch: OrderBatch) -> eyre::Result<SubmissionPayload> {
        tracing::info!("Processing batch with {} indices", batch.indices.len());

        // Step 1: Collect all unique asset IDs from all indices in the batch
        let mut required_assets = std::collections::HashSet::new();
        
        for index_id in batch.get_active_indices() {
            if let Some(assets) = self.index_mapper.get_index_assets(index_id) {
                for asset_id in assets {
                    required_assets.insert(asset_id);
                }
            }
        }

        let required_assets: Vec<u128> = required_assets.into_iter().collect();
        tracing::debug!("Requesting quotes for {} unique assets", required_assets.len());

        // Step 2: Fetch fresh quotes from vendor
        let vendor_quote = self.vendor_client.quote_assets(required_assets.clone()).await?;

        // Step 3: Build market data snapshot
        let market_snapshot = self.build_market_snapshot(&vendor_quote)?;

        tracing::info!(
            "Received market data for {} assets ({}% of requested)",
            market_snapshot.assets.len(),
            if required_assets.is_empty() {
                0
            } else {
                market_snapshot.assets.len() * 100 / required_assets.len()
            }
        );

        // Step 4: Create submission payload
        let mut payload = SubmissionPayload::new(self.config.vendor_id, market_snapshot.clone());

        // Step 5: Process each index in the batch
        for (index_id, index_state) in &batch.indices {
            if !index_state.needs_processing() {
                continue;
            }

            match self.process_index(*index_id, index_state, &market_snapshot) {
                Ok(Some(buy_order)) => {
                    tracing::info!(
                        "Generated buy order for index {}: collateral_change=${:.2}",
                        index_id,
                        buy_order.net_collateral_change().to_u128_raw() as f64 / 1e18
                    );
                    payload.add_buy_order(buy_order);
                }
                Ok(None) => {
                    tracing::debug!("No buy order needed for index {}", index_id);
                }
                Err(e) => {
                    tracing::warn!("Failed to process index {}: {:?}", index_id, e);
                }
            }
        }

        tracing::info!(
            "Submission payload ready: {} buy orders, {} assets with market data",
            payload.buy_orders.len(),
            payload.market_data.assets.len()
        );

        Ok(payload)
    }

    /// Build market data snapshot from vendor quote
    fn build_market_snapshot(&self, quote: &AssetsQuote) -> eyre::Result<MarketDataSnapshot> {
        let mut snapshot = MarketDataSnapshot::new();

        for i in 0..quote.assets.len() {
            let asset_data = AssetMarketData {
                asset_id: quote.assets[i],
                liquidity: Amount::from_u128_with_scale(
                    (quote.liquidity[i] * 1e18) as u128,
                    18,
                ),
                price: Amount::from_u128_with_scale(
                    (quote.prices[i] * 1e18) as u128,
                    18,
                ),
                slope: Amount::from_u128_with_scale(
                    (quote.slopes[i] * 1e18) as u128,
                    18,
                ),
            };

            snapshot.add_asset(asset_data);
        }

        Ok(snapshot)
    }

    /// Process a single index into a buy order
    fn process_index(
        &self,
        index_id: u128,
        index_state: &IndexState,
        market_snapshot: &MarketDataSnapshot,
    ) -> eyre::Result<Option<IndexBuyOrder>> {
        // Get index configuration
        let index_config = self
            .index_mapper
            .get_index(index_id)
            .ok_or_else(|| eyre::eyre!("Index {} not found in mapper", index_id))?;

        // Check if we have market data for all required assets
        let asset_ids: Vec<u128> = index_config.assets.iter().map(|a| a.asset_id).collect();
        
        if !market_snapshot.has_all_assets(&asset_ids) {
            tracing::warn!(
                "Missing market data for some assets in index {}",
                index_id
            );
            return Ok(None);
        }

        // Calculate asset allocations based on index weights
        let mut allocations = Vec::new();
        let net_collateral = index_state.net_collateral_change;

        // Skip if net collateral change is zero or negative
        if net_collateral.is_not() || net_collateral < Amount::ZERO {
            tracing::debug!(
                "Skipping index {} - no positive collateral change",
                index_id
            );
            return Ok(None);
        }

        tracing::debug!(
            "Processing index {}: net_collateral=${:.2}",
            index_id,
            net_collateral.to_u128_raw() as f64 / 1e18
        );

        // Calculate total weight
        let total_weight: f64 = index_config.assets.iter().map(|a| a.weight).sum();

        for asset_weight in &index_config.assets {
            let asset_data = market_snapshot
                .get_asset(asset_weight.asset_id)
                .ok_or_else(|| eyre::eyre!("Missing market data for asset {}", asset_weight.asset_id))?;

            // Calculate target value in USD for this asset
            let weight_fraction = asset_weight.weight / total_weight;
            let target_value = net_collateral
                .checked_mul(Amount::from_u128_with_scale(
                    (weight_fraction * 1e18) as u128,
                    18,
                ))
                .ok_or_else(|| eyre::eyre!("Overflow calculating target value"))?;

            // Calculate quantity: target_value / price
            let quantity = target_value
                .checked_div(asset_data.price)
                .ok_or_else(|| eyre::eyre!("Division error calculating quantity"))?;

            tracing::debug!(
                "  Asset {}: weight={:.2}%, target_value=${:.2}, quantity={:.8}",
                asset_weight.asset_id,
                weight_fraction * 100.0,
                target_value.to_u128_raw() as f64 / 1e18,
                quantity.to_u128_raw() as f64 / 1e18
            );

            allocations.push(AssetAllocation {
                asset_id: asset_weight.asset_id,
                quantity,
                target_value_usd: target_value,
            });
        }

        // Calculate max order size (e.g., 2x net collateral for safety margin)
        let max_order_size = net_collateral
            .checked_mul(Amount::from_u128_with_scale(
                (self.config.max_order_size_multiplier * 1e18) as u128,
                18,
            ))
            .ok_or_else(|| eyre::eyre!("Overflow calculating max order size"))?;

        let buy_order = IndexBuyOrder {
            index_id,
            collateral_added: index_state.total_deposits,
            collateral_removed: index_state.total_withdrawals,
            max_order_size,
            asset_allocations: allocations,
        };

        Ok(Some(buy_order))
    }

    /// Get summary statistics for a submission payload
    pub fn get_payload_summary(&self, payload: &SubmissionPayload) -> PayloadSummary {
        let total_collateral: Amount = payload
            .buy_orders
            .iter()
            .map(|o| o.net_collateral_change())
            .fold(Amount::ZERO, |acc, x| acc.checked_add(x).unwrap());

        let unique_assets = payload
            .buy_orders
            .iter()
            .flat_map(|o| o.asset_allocations.iter().map(|a| a.asset_id))
            .collect::<std::collections::HashSet<_>>()
            .len();

        PayloadSummary {
            index_count: payload.buy_orders.len(),
            unique_assets,
            total_collateral_usd: total_collateral,
            market_data_assets: payload.market_data.assets.len(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PayloadSummary {
    pub index_count: usize,
    pub unique_assets: usize,
    pub total_collateral_usd: Amount,
    pub market_data_assets: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_market_snapshot() {
        let mut snapshot = MarketDataSnapshot::new();

        let asset_data = AssetMarketData {
            asset_id: 101,
            liquidity: Amount::from_u128_with_scale(25, 0),
            price: Amount::from_u128_with_scale(95000, 0),
            slope: Amount::from_u128_with_scale(3, 0),
        };

        snapshot.add_asset(asset_data);

        assert!(snapshot.get_asset(101).is_some());
        assert!(snapshot.get_asset(999).is_none());
        assert!(snapshot.has_all_assets(&[101]));
        assert!(!snapshot.has_all_assets(&[101, 102]));
    }
}