use super::types::{AssetSupply, SupplyState};
use crate::onchain::AssetMapper;
use common::amount::Amount;
use eyre::{eyre, Result};
use parking_lot::RwLock;
use std::sync::Arc;

/// Manages vendor's supply state (long/short positions per asset)
/// Supply is tracked off-chain and submitted to on-chain when needed
pub struct SupplyManager {
    state: SupplyState,
    asset_mapper: Arc<RwLock<AssetMapper>>,
}

impl SupplyManager {
    pub fn new(asset_mapper: Arc<RwLock<AssetMapper>>) -> Self {
        Self {
            state: SupplyState::new(),
            asset_mapper,
        }
    }
    
    /// Initialize supply tracking for all assets
    /// Called once at startup
    pub fn initialize(&mut self, asset_symbols: &[String]) -> Result<()> {
        tracing::info!("Initializing supply tracking for {} assets", asset_symbols.len());
        
        for symbol in asset_symbols {
            let asset_id = self
                .asset_mapper
                .read()
                .get_id(symbol)
                .ok_or_else(|| eyre!("Asset {} not found in mapper", symbol))?;
            
            self.state.init_asset(symbol.clone(), asset_id);
            
            tracing::debug!("  Initialized supply for {} (ID: {})", symbol, asset_id);
        }
        
        tracing::info!("✓ Supply tracking initialized for {} assets", self.state.supplies.len());
        Ok(())
    }
    
    /// Record a buy order execution
    /// Increases long position (or reduces short position)
    pub fn record_buy(&mut self, symbol: &str, quantity: Amount) -> Result<()> {
        tracing::debug!("Recording buy: {} qty={}", symbol, quantity);
        
        self.state
            .record_buy(symbol, quantity)
            .map_err(|e| eyre!("Failed to record buy for {}: {}", symbol, e))?;
        
        // Log new position
        if let Some(supply) = self.state.get_supply(symbol) {
            let (is_long, net) = supply.net_position();
            tracing::info!(
                "  {} position: {} {} (long={}, short={})",
                symbol,
                if is_long { "+" } else { "-" },
                net.to_u128_raw() as f64 / 1e18,
                supply.supply_long.to_u128_raw() as f64 / 1e18,
                supply.supply_short.to_u128_raw() as f64 / 1e18
            );
        }
        
        Ok(())
    }
    
    /// Record a sell order execution
    /// Increases short position (or reduces long position)
    pub fn record_sell(&mut self, symbol: &str, quantity: Amount) -> Result<()> {
        tracing::debug!("Recording sell: {} qty={}", symbol, quantity);
        
        self.state
            .record_sell(symbol, quantity)
            .map_err(|e| eyre!("Failed to record sell for {}: {}", symbol, e))?;
        
        // Log new position
        if let Some(supply) = self.state.get_supply(symbol) {
            let (is_long, net) = supply.net_position();
            tracing::info!(
                "  {} position: {} {} (long={}, short={})",
                symbol,
                if is_long { "+" } else { "-" },
                net.to_u128_raw() as f64 / 1e18,
                supply.supply_long.to_u128_raw() as f64 / 1e18,
                supply.supply_short.to_u128_raw() as f64 / 1e18
            );
        }
        
        Ok(())
    }
    
    /// Get submission data for on-chain submitSupply() call
    /// Returns (asset_ids, supply_short, supply_long)
    pub fn get_submission_data(&self) -> (Vec<u128>, Vec<Amount>, Vec<Amount>) {
        let mut asset_ids = Vec::new();
        let mut supply_short = Vec::new();
        let mut supply_long = Vec::new();
        
        // Only include assets with non-zero positions
        for supply in self.state.supplies.values() {
            if supply.supply_long != Amount::ZERO || supply.supply_short != Amount::ZERO {
                asset_ids.push(supply.asset_id);
                supply_short.push(supply.supply_short);
                supply_long.push(supply.supply_long);
            }
        }
        
        (asset_ids, supply_short, supply_long)
    }
    
    /// Check if submission is needed
    pub fn needs_submission(&self) -> bool {
        self.state.needs_submission()
    }
    
    /// Mark submission complete
    pub fn mark_submitted(&mut self) {
        self.state.mark_submitted();
        tracing::debug!("Supply submission marked complete");
    }
    
    /// Get current state (for debugging/inspection)
    pub fn state(&self) -> &SupplyState {
        &self.state
    }
    
    /// Get supply for specific asset
    pub fn get_supply(&self, symbol: &str) -> Option<&AssetSupply> {
        self.state.get_supply(symbol)
    }
    
    /// Get all assets with positions
    pub fn get_assets_with_positions(&self) -> Vec<String> {
        self.state
            .supplies
            .values()
            .filter(|s| s.supply_long != Amount::ZERO || s.supply_short != Amount::ZERO)
            .map(|s| s.symbol.clone())
            .collect()
    }
}