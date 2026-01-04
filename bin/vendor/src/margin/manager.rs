use super::types::{AssetMargin, MarginState};
use crate::config::MarginConfig;
use crate::onchain::{AssetMapper, PriceTracker};
use common::amount::Amount;
use eyre::{eyre, Result};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

pub struct MarginManager {
    config: MarginConfig,
    state: MarginState,
    price_tracker: Arc<PriceTracker>,
    asset_mapper: Arc<RwLock<AssetMapper>>,
}

impl MarginManager {
    pub fn new(
        config: MarginConfig,
        price_tracker: Arc<PriceTracker>,
        asset_mapper: Arc<RwLock<AssetMapper>>,
    ) -> Self {
        Self {
            config,
            state: MarginState::new(),
            price_tracker,
            asset_mapper,
        }
    }
    
    /// Initialize margins for all tracked assets
    /// Called once at startup
    pub fn initialize(&mut self, asset_symbols: &[String]) -> Result<()> {
        tracing::info!("Initializing margins for {} assets", asset_symbols.len());
        
        for symbol in asset_symbols {
            let price = self.price_tracker.get_price(symbol).unwrap_or(Amount::ZERO);  // Fixed
            
            if price == Amount::ZERO {
                tracing::warn!("No price data for {}, skipping margin calculation", symbol);
                continue;
            }
            
            // Calculate margin: exposure_per_asset / price
            let margin = self.config.calculate_margin(asset_symbols.len(), price);
            
            // Get asset ID from mapper
            let asset_id = self
                .asset_mapper
                .read()
                .get_id(symbol)
                .ok_or_else(|| eyre!("Asset {} not found in mapper", symbol))?;
            
            let asset_margin = AssetMargin::new(symbol.clone(), asset_id, margin, price);
            
            self.state.update_margin(asset_margin);
            
            tracing::info!(
                "  {} (ID: {}): margin = {}, price = ${}",
                symbol,
                asset_id,
                margin,
                price.to_u128_raw() as f64 / 1e18  // Fixed
            );
        }
        
        tracing::info!("✓ Margins initialized for {} assets", self.state.total_assets);
        Ok(())
    }
    
    /// Check if any margins need updating based on current prices
    pub fn check_for_updates(&self) -> Vec<String> {
        // Get current prices from tracker
        let mut current_prices = HashMap::new();
        
        for symbol in self.state.margins.keys() {
            let price = self.price_tracker.get_price(symbol).unwrap_or(Amount::ZERO);  // Fixed
            if price != Amount::ZERO {
                current_prices.insert(symbol.clone(), price);
            }
        }
        
        // Check price-based updates
        let price_updates = self.state.check_price_changes(
            &current_prices,
            self.config.price_change_threshold,
        );
        
        // Check time-based updates
        let time_updates = self.state.get_stale_margins(
            self.config.price_change_threshold,
            self.config.margin_update_timeout_secs,
        );
        
        // Combine and deduplicate
        let mut all_updates = price_updates;
        for symbol in time_updates {
            if !all_updates.contains(&symbol) {
                all_updates.push(symbol);
            }
        }
        
        if !all_updates.is_empty() {
            tracing::info!(
                "Margins need update for {} assets: {:?}",
                all_updates.len(),
                all_updates
            );
        }
        
        all_updates
    }
    
    /// Update margins for specific assets with new prices
    pub fn update_margins(&mut self, symbols: &[String]) -> Result<()> {
        tracing::info!("Updating margins for {} assets", symbols.len());
        
        for symbol in symbols {
            let price = self.price_tracker.get_price(symbol).unwrap_or(Amount::ZERO);  // Fixed
            
            if price == Amount::ZERO {
                tracing::warn!("No price for {}, skipping update", symbol);
                continue;
            }
            
            let margin = self.config.calculate_margin(self.state.total_assets, price);
            
            let asset_id = self
                .asset_mapper
                .read()
                .get_id(symbol)
                .ok_or_else(|| eyre!("Asset {} not found", symbol))?;
            
            let asset_margin = AssetMargin::new(symbol.clone(), asset_id, margin, price);
            
            self.state.update_margin(asset_margin);
            
            tracing::debug!(
                "  Updated {}: margin = {}, price = ${}",
                symbol,
                margin,
                price.to_u128_raw() as f64 / 1e18  // Fixed
            );
        }
        
        Ok(())
    }
    
    /// Get margin data for on-chain submission
    /// Returns (asset_ids, margins) as vectors
    pub fn get_submission_data(&self) -> (Vec<u128>, Vec<Amount>) {
        let mut asset_ids = Vec::new();
        let mut margins = Vec::new();
        
        for margin_info in self.state.margins.values() {
            asset_ids.push(margin_info.asset_id);
            margins.push(margin_info.margin);
        }
        
        (asset_ids, margins)
    }
    
    /// Mark submission complete
    pub fn mark_submitted(&mut self) {
        self.state.last_submission = Some(chrono::Utc::now());
    }
    
    /// Get config
    pub fn config(&self) -> &MarginConfig {
        &self.config
    }
    
    /// Get state (for inspection/debugging)
    pub fn state(&self) -> &MarginState {
        &self.state
    }
}