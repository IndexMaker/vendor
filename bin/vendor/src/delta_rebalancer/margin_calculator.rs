use super::types::{AssetMargin, RebalancerConfig};
use crate::onchain::{AssetMapper, PriceTracker};
use common::amount::Amount;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

pub struct MarginCalculator {
    price_tracker: Arc<PriceTracker>,
    asset_mapper: Arc<RwLock<AssetMapper>>,
    config: RebalancerConfig,
}

impl MarginCalculator {
    pub fn new(
        price_tracker: Arc<PriceTracker>,
        asset_mapper: Arc<RwLock<AssetMapper>>,
        config: RebalancerConfig,
    ) -> Self {
        Self {
            price_tracker,
            asset_mapper,
            config,
        }
    }

    /// Calculate margin (max tradeable quantity) for each asset
    /// 
    /// Formula:
    /// - exposure_per_asset = total_exposure / total_assets
    /// - margin = exposure_per_asset / current_price
    pub fn calculate_margins(&self, asset_ids: &[u128]) -> HashMap<u128, AssetMargin> {
        let total_assets = asset_ids.len() as f64;
        
        if total_assets == 0.0 {
            return HashMap::new();
        }

        // Calculate exposure per asset (equal allocation)
        let exposure_per_asset_usd = self.config.total_exposure_usd / total_assets;
        let exposure_per_asset = Amount::from_u128_with_scale(
            (exposure_per_asset_usd * 1e18) as u128,
            18,
        );

        let mut margins = HashMap::new();

        for asset_id in asset_ids {
            // Get symbol for this asset
            let symbol = match self.asset_mapper.read().get_symbol(*asset_id) {
                Some(s) => s.clone(),
                None => {
                    tracing::warn!("Asset {} not found in mapper", asset_id);
                    continue;
                }
            };

            // Get current price from order book
            // CHANGED: get_price() returns Amount directly, no conversion needed
            let price = match self.price_tracker.get_price(&symbol) {
                Some(p) => p,  // Already Amount type
                None => {
                    tracing::warn!("No price available for {}", symbol);
                    continue;
                }
            };

            // Calculate margin: max_quantity = exposure / price
            let max_quantity = exposure_per_asset
                .checked_div(price)
                .unwrap_or(Amount::ZERO);

            tracing::debug!(
                "  Margin for {} ({}): exposure=${:.2}, price=${:.2}, max_qty={:.8}",
                asset_id,
                symbol,
                exposure_per_asset_usd,
                price.to_u128_raw() as f64 / 1e18,
                max_quantity.to_u128_raw() as f64 / 1e18
            );

            margins.insert(
                *asset_id,
                AssetMargin {
                    asset_id: *asset_id,
                    max_quantity,
                    exposure_usd: exposure_per_asset,
                },
            );
        }

        margins
    }
}