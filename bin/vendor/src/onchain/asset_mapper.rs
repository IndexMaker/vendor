use common::event_cache::types::EventCache;
use eyre::{eyre, Context, Result};
use serde_json;
use std::collections::HashMap;
use std::path::Path;

/// Maps asset IDs to Bitget trading symbols and vice versa.
///
/// Supports multiple data sources:
/// 1. Local event cache from PairRegistry (Story 1.4)
/// 2. JSON file fallback for testing/development
/// 3. Manual additions for overrides
#[derive(Debug, Clone)]
pub struct AssetMapper {
    /// Symbol -> Asset ID mapping (e.g., "BTCUSDC" -> 123)
    symbol_to_id: HashMap<String, u128>,
    /// Asset ID -> Symbol mapping (e.g., 123 -> "BTCUSDC")
    id_to_symbol: HashMap<u128, String>,
    /// Preferred quote asset (USDC > USDT per PRD FR60)
    prefer_usdc: bool,
}

impl Default for AssetMapper {
    fn default() -> Self {
        Self {
            symbol_to_id: HashMap::new(),
            id_to_symbol: HashMap::new(),
            prefer_usdc: true,
        }
    }
}

impl AssetMapper {
    /// Create a new empty asset mapper
    pub fn new() -> Self {
        Self::default()
    }

    /// Load asset mappings from a local event cache (from Story 1.4 PairRegistry)
    ///
    /// This integrates with the event indexer which caches PairRegistered events.
    /// When prefer_usdc is enabled (default), USDC pairs take precedence over USDT pairs
    /// for the same base asset (per PRD FR60).
    pub fn load_from_event_cache(cache: &EventCache) -> Self {
        let mut mapper = Self::new();

        // Track which base assets have USDC pairs (for preference logic)
        let mut usdc_base_assets: std::collections::HashSet<String> = std::collections::HashSet::new();

        // First pass: identify all USDC pairs
        for pair in cache.get_all_pairs() {
            if pair.quote_asset.to_uppercase() == "USDC" {
                usdc_base_assets.insert(pair.base_asset.clone());
            }
        }

        // Second pass: load pairs with USDC preference
        let mut skipped_usdt = 0;
        for pair in cache.get_all_pairs() {
            // If this is a USDT pair and we have a USDC pair for the same base asset, skip it
            if mapper.prefer_usdc
                && pair.quote_asset.to_uppercase() == "USDT"
                && usdc_base_assets.contains(&pair.base_asset)
            {
                tracing::debug!(
                    "Skipping USDT pair {} (prefer USDC): USDC pair available for {}",
                    pair.symbol,
                    pair.base_asset
                );
                skipped_usdt += 1;
                continue;
            }

            // Use pair_id as the asset_id
            mapper.symbol_to_id.insert(pair.symbol.clone(), pair.pair_id);
            mapper.id_to_symbol.insert(pair.pair_id, pair.symbol.clone());

            tracing::debug!(
                "Loaded pair from cache: {} (id={}), base={}, quote={}",
                pair.symbol,
                pair.pair_id,
                pair.base_asset,
                pair.quote_asset
            );
        }

        tracing::info!(
            "Loaded {} asset mappings from event cache (skipped {} USDT pairs due to USDC preference)",
            mapper.symbol_to_id.len(),
            skipped_usdt
        );

        mapper
    }

    /// Load asset mappings from a JSON file (fallback for development/testing)
    ///
    /// File format: { "BTCUSDC": 123, "ETHUSDC": 456, ... }
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

        tracing::info!("Loaded {} asset ID mappings from file", symbol_to_id.len());

        Ok(Self {
            symbol_to_id,
            id_to_symbol,
            prefer_usdc: true,
        })
    }

    /// Load asset mappings from bitget-pairs.json format (from list-bitget-pairs script)
    ///
    /// File format: [{ "id": 1, "symbol": "BTC/USDT", "baseCoin": "BTC", "quoteCoin": "USDT", ... }, ...]
    /// Converts "BTC/USDT" to "BTCUSDT" for Bitget API subscription
    pub async fn load_from_bitget_pairs_file(path: &Path) -> Result<Self> {
        tracing::info!("Loading Bitget pairs from: {:?}", path);

        let content = tokio::fs::read_to_string(path)
            .await
            .context(format!("Failed to read bitget pairs file: {:?}", path))?;

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct BitgetPair {
            id: u32,
            #[allow(dead_code)]
            symbol: String,  // "BTC/USDT" format
            base_coin: String,
            quote_coin: String,
        }

        let pairs: Vec<BitgetPair> = serde_json::from_str(&content)
            .context("Failed to parse bitget-pairs.json")?;

        let mut symbol_to_id = HashMap::new();
        let mut id_to_symbol = HashMap::new();

        for pair in pairs {
            // Convert "BTC/USDT" format to "BTCUSDT" for Bitget API
            let api_symbol = format!("{}{}", pair.base_coin, pair.quote_coin);
            let id = pair.id as u128;

            symbol_to_id.insert(api_symbol.clone(), id);
            id_to_symbol.insert(id, api_symbol);
        }

        tracing::info!("Loaded {} asset mappings from bitget-pairs.json", symbol_to_id.len());

        Ok(Self {
            symbol_to_id,
            id_to_symbol,
            prefer_usdc: true,
        })
    }

    /// Merge additional mappings from an event cache (useful for incremental updates)
    pub fn merge_from_event_cache(&mut self, cache: &EventCache) {
        for pair in cache.get_all_pairs() {
            if !self.symbol_to_id.contains_key(&pair.symbol) {
                self.symbol_to_id.insert(pair.symbol.clone(), pair.pair_id);
                self.id_to_symbol.insert(pair.pair_id, pair.symbol.clone());
            }
        }
    }

    /// Add a manual mapping (useful for testing or overrides)
    pub fn add_mapping(&mut self, symbol: String, id: u128) {
        self.symbol_to_id.insert(symbol.clone(), id);
        self.id_to_symbol.insert(id, symbol);
    }

    /// Get asset ID for a symbol
    pub fn get_id(&self, symbol: &str) -> Option<u128> {
        self.symbol_to_id.get(symbol).copied()
    }

    /// Get symbol for an asset ID
    pub fn get_symbol(&self, id: u128) -> Option<&String> {
        self.id_to_symbol.get(&id)
    }

    /// Get Bitget trading symbol for an asset ID
    ///
    /// USDC preference is applied during load_from_event_cache() - USDT pairs are
    /// filtered out if a USDC pair exists for the same base asset (per PRD FR60).
    /// Returns None if no mapping exists (graceful handling per AC #4).
    pub fn get_bitget_symbol(&self, id: u128) -> Option<String> {
        self.id_to_symbol.get(&id).cloned()
    }

    /// Get Bitget symbol with fallback logging (per AC #4)
    ///
    /// If the asset has no Bitget symbol, logs a warning and returns None.
    pub fn get_bitget_symbol_with_fallback(&self, id: u128) -> Option<String> {
        match self.get_bitget_symbol(id) {
            Some(symbol) => Some(symbol),
            None => {
                tracing::warn!(
                    "No Bitget symbol mapping for asset_id={}, using default/skipping",
                    id
                );
                None
            }
        }
    }

    /// Get sorted asset IDs for a list of symbols
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

    /// Validate that all symbols have mappings
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

    /// Get all known symbols sorted alphabetically
    pub fn get_all_symbols(&self) -> Vec<String> {
        let mut symbols: Vec<String> = self.symbol_to_id.keys().cloned().collect();
        symbols.sort();
        symbols
    }

    /// Get all known asset IDs sorted
    pub fn get_all_ids(&self) -> Vec<u128> {
        let mut ids: Vec<u128> = self.id_to_symbol.keys().copied().collect();
        ids.sort();
        ids
    }

    /// Get the number of mapped assets
    pub fn len(&self) -> usize {
        self.symbol_to_id.len()
    }

    /// Check if the mapper is empty
    pub fn is_empty(&self) -> bool {
        self.symbol_to_id.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_asset_mapper_manual_mapping() {
        let mut mapper = AssetMapper::new();
        mapper.add_mapping("BTCUSDC".to_string(), 1);
        mapper.add_mapping("ETHUSDC".to_string(), 2);

        assert_eq!(mapper.get_id("BTCUSDC"), Some(1));
        assert_eq!(mapper.get_id("ETHUSDC"), Some(2));
        assert_eq!(mapper.get_symbol(1), Some(&"BTCUSDC".to_string()));
        assert_eq!(mapper.get_symbol(2), Some(&"ETHUSDC".to_string()));
    }

    #[test]
    fn test_asset_mapper_unknown_symbol() {
        let mapper = AssetMapper::new();
        assert_eq!(mapper.get_id("UNKNOWN"), None);
        assert_eq!(mapper.get_symbol(999), None);
    }

    #[test]
    fn test_get_bitget_symbol_with_fallback() {
        let mut mapper = AssetMapper::new();
        mapper.add_mapping("BTCUSDC".to_string(), 1);

        // Known symbol returns Some
        assert_eq!(mapper.get_bitget_symbol_with_fallback(1), Some("BTCUSDC".to_string()));

        // Unknown symbol returns None (and logs warning)
        assert_eq!(mapper.get_bitget_symbol_with_fallback(999), None);
    }

    #[test]
    fn test_get_sorted_ids() {
        let mut mapper = AssetMapper::new();
        mapper.add_mapping("ETHUSDC".to_string(), 5);
        mapper.add_mapping("BTCUSDC".to_string(), 3);
        mapper.add_mapping("SOLUSDC".to_string(), 1);

        let ids = mapper.get_sorted_ids(&["ETHUSDC".to_string(), "BTCUSDC".to_string(), "SOLUSDC".to_string()]).unwrap();
        assert_eq!(ids, vec![1, 3, 5]); // Sorted by ID
    }

    #[test]
    fn test_validate_all_mapped() {
        let mut mapper = AssetMapper::new();
        mapper.add_mapping("BTCUSDC".to_string(), 1);

        // Valid - all mapped
        assert!(mapper.validate_all_mapped(&["BTCUSDC".to_string()]).is_ok());

        // Invalid - missing mapping
        assert!(mapper.validate_all_mapped(&["BTCUSDC".to_string(), "UNKNOWN".to_string()]).is_err());
    }

    #[test]
    fn test_usdc_preference_in_event_cache() {
        use common::event_cache::types::{CachedPair, EventCache};

        let mut cache = EventCache::new();

        // Add USDC pair for BTC
        cache.add_pair(CachedPair {
            pair_id: 1,
            symbol: "BTCUSDC".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDC".into(),
            block_number: 100,
            tx_hash: "0xaaa".into(),
        });

        // Add USDT pair for BTC (should be skipped due to USDC preference)
        cache.add_pair(CachedPair {
            pair_id: 2,
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            block_number: 101,
            tx_hash: "0xbbb".into(),
        });

        // Add USDT pair for ETH (should be included, no USDC alternative)
        cache.add_pair(CachedPair {
            pair_id: 3,
            symbol: "ETHUSDT".into(),
            base_asset: "ETH".into(),
            quote_asset: "USDT".into(),
            block_number: 102,
            tx_hash: "0xccc".into(),
        });

        let mapper = AssetMapper::load_from_event_cache(&cache);

        // BTCUSDC should be loaded (USDC pair)
        assert_eq!(mapper.get_id("BTCUSDC"), Some(1));
        assert_eq!(mapper.get_symbol(1), Some(&"BTCUSDC".to_string()));

        // BTCUSDT should NOT be loaded (USDT skipped due to USDC preference)
        assert_eq!(mapper.get_id("BTCUSDT"), None);
        assert_eq!(mapper.get_symbol(2), None);

        // ETHUSDT should be loaded (no USDC alternative exists)
        assert_eq!(mapper.get_id("ETHUSDT"), Some(3));
        assert_eq!(mapper.get_symbol(3), Some(&"ETHUSDT".to_string()));

        // Total should be 2 (BTCUSDC + ETHUSDT)
        assert_eq!(mapper.len(), 2);
    }
}