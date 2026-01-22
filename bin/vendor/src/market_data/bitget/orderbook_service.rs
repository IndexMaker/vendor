//! Order Book Service - High-level API for fetching order books by asset ID
//!
//! This module provides a convenient interface that combines:
//! - Asset ID → Symbol mapping (via AssetMapper)
//! - Order book fetching via REST API (via BitgetRestClient)
//! - Concurrent fetching with rate limiting
//!
//! Implements AC #3, #4, #6:
//! - Maps asset_id → Bitget symbol via local cache
//! - Handles missing pairs gracefully (log + use defaults)
//! - Concurrent requests for multiple assets

use crate::market_data::bitget::rest_client::{BitgetRestClient, BitgetRestConfig, OrderBookSnapshot};
use crate::onchain::AssetMapper;
use eyre::Result;
use std::collections::HashMap;
use std::sync::Arc;

/// Configuration for the OrderBookService
#[derive(Debug, Clone)]
pub struct OrderBookServiceConfig {
    /// REST client configuration
    pub rest_config: BitgetRestConfig,
    /// Default number of order book levels to fetch (K=5 per story requirements)
    pub default_levels: usize,
}

impl Default for OrderBookServiceConfig {
    fn default() -> Self {
        Self {
            rest_config: BitgetRestConfig::default(),
            default_levels: 5, // K=5 per story requirements
        }
    }
}

/// High-level service for fetching order books by asset ID
///
/// Combines asset mapping with order book fetching for a seamless API.
pub struct OrderBookService {
    client: BitgetRestClient,
    mapper: Arc<AssetMapper>,
    default_levels: usize,
}

impl OrderBookService {
    /// Create a new order book service
    ///
    /// # Arguments
    /// * `mapper` - Asset mapper for ID → Symbol mapping
    /// * `config` - Service configuration
    pub fn new(mapper: Arc<AssetMapper>, config: OrderBookServiceConfig) -> Result<Self> {
        let client = BitgetRestClient::new(config.rest_config)?;

        Ok(Self {
            client,
            mapper,
            default_levels: config.default_levels,
        })
    }

    /// Get the default number of order book levels (K=5 per story requirements)
    pub fn default_levels(&self) -> usize {
        self.default_levels
    }

    /// Fetch order book for a single asset ID
    ///
    /// # Arguments
    /// * `asset_id` - On-chain asset ID
    /// * `levels` - Number of depth levels (defaults to K=5)
    ///
    /// # Returns
    /// Order book snapshot if the asset has a valid mapping, None otherwise
    pub async fn fetch_by_asset_id(
        &self,
        asset_id: u128,
        levels: Option<usize>,
    ) -> Option<OrderBookSnapshot> {
        // Map asset ID to symbol
        let symbol = match self.mapper.get_bitget_symbol_with_fallback(asset_id) {
            Some(s) => s,
            None => return None,
        };

        // Fetch order book
        match self.client.fetch_orderbook(&symbol, levels).await {
            Ok(snapshot) => Some(snapshot),
            Err(e) => {
                tracing::warn!(
                    "Failed to fetch order book for asset_id={} (symbol={}): {}",
                    asset_id,
                    symbol,
                    e
                );
                None
            }
        }
    }

    /// Fetch order books for multiple asset IDs concurrently (AC #6)
    ///
    /// # Arguments
    /// * `asset_ids` - List of on-chain asset IDs
    /// * `levels` - Number of depth levels (defaults to K=5)
    ///
    /// # Returns
    /// Map of asset_id → OrderBookSnapshot (only includes successful fetches)
    pub async fn fetch_by_asset_ids_concurrent(
        &self,
        asset_ids: &[u128],
        levels: Option<usize>,
    ) -> HashMap<u128, OrderBookSnapshot> {
        // Map asset IDs to symbols, filtering out unmapped assets
        let mut id_to_symbol: HashMap<u128, String> = HashMap::new();
        let mut symbols: Vec<String> = Vec::new();

        for &asset_id in asset_ids {
            if let Some(symbol) = self.mapper.get_bitget_symbol_with_fallback(asset_id) {
                id_to_symbol.insert(asset_id, symbol.clone());
                symbols.push(symbol);
            }
        }

        if symbols.is_empty() {
            tracing::warn!("No valid symbol mappings found for any of the {} asset IDs", asset_ids.len());
            return HashMap::new();
        }

        tracing::debug!(
            "Fetching order books for {} assets ({} mapped, {} unmapped)",
            asset_ids.len(),
            symbols.len(),
            asset_ids.len() - symbols.len()
        );

        // Fetch order books concurrently
        let symbol_snapshots = self.client.fetch_orderbooks_concurrent(&symbols, levels).await;

        // Map back to asset IDs
        let mut result: HashMap<u128, OrderBookSnapshot> = HashMap::new();
        for (asset_id, symbol) in id_to_symbol {
            if let Some(snapshot) = symbol_snapshots.get(&symbol.to_uppercase()) {
                result.insert(asset_id, snapshot.clone());
            }
        }

        tracing::debug!(
            "Successfully fetched {} order books out of {} requested assets",
            result.len(),
            asset_ids.len()
        );

        result
    }

    /// Get the underlying asset mapper (for additional lookups)
    pub fn mapper(&self) -> &AssetMapper {
        &self.mapper
    }

    /// Get the number of assets that can be mapped
    pub fn mapped_asset_count(&self) -> usize {
        self.mapper.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_mapper() -> AssetMapper {
        let mut mapper = AssetMapper::new();
        mapper.add_mapping("BTCUSDC".to_string(), 1);
        mapper.add_mapping("ETHUSDC".to_string(), 2);
        mapper.add_mapping("SOLUSDC".to_string(), 3);
        mapper
    }

    #[test]
    fn test_service_config_defaults() {
        let config = OrderBookServiceConfig::default();
        assert_eq!(config.default_levels, 5);
        assert_eq!(config.rest_config.timeout_ms, 500);
        assert_eq!(config.rest_config.max_requests_per_second, 10);
    }

    #[test]
    fn test_mapper_integration() {
        let mapper = Arc::new(create_test_mapper());
        let config = OrderBookServiceConfig::default();

        let service = OrderBookService::new(mapper.clone(), config).unwrap();

        assert_eq!(service.mapped_asset_count(), 3);
        assert!(service.mapper().get_symbol(1).is_some());
        assert!(service.mapper().get_symbol(999).is_none());
    }
}
