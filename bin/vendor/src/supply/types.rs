use chrono::{DateTime, Utc};
use common::amount::Amount;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Supply information for a single asset
/// Tracks both long and short positions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetSupply {
    pub symbol: String,
    pub asset_id: u128,
    
    /// Long position (positive exposure)
    pub supply_long: Amount,
    
    /// Short position (negative exposure)
    pub supply_short: Amount,
    
    /// Last update timestamp
    pub last_updated: DateTime<Utc>,
}

impl AssetSupply {
    pub fn new(symbol: String, asset_id: u128) -> Self {
        Self {
            symbol,
            asset_id,
            supply_long: Amount::ZERO,
            supply_short: Amount::ZERO,
            last_updated: Utc::now(),
        }
    }
    
    /// Add to long position (buy)
    /// Delta + X:
    ///   supply_short = supply_short.saturating_sub(X)
    ///   R = X.saturating_sub(supply_short)
    ///   supply_long = supply_long.checked_add(R)
    pub fn add_long(&mut self, amount: Amount) -> Result<(), String> {
        // First reduce short position
        self.supply_short = self.supply_short.saturating_sub(amount).unwrap_or(self.supply_short);
        
        // Remainder goes to long
        let remainder = amount.saturating_sub(self.supply_short).unwrap_or(amount);
        self.supply_long = self.supply_long
            .checked_add(remainder)
            .ok_or_else(|| "Overflow in supply_long".to_string())?;
        
        self.last_updated = Utc::now();
        Ok(())
    }
    
    /// Add to short position (sell)
    /// Delta - X:
    ///   supply_long = supply_long.saturating_sub(X)
    ///   R = X.saturating_sub(supply_long)
    ///   supply_short = supply_short.checked_add(R)
    pub fn add_short(&mut self, amount: Amount) -> Result<(), String> {
        // First reduce long position
        self.supply_long = self.supply_long.saturating_sub(amount).unwrap_or(self.supply_long);
        
        // Remainder goes to short
        let remainder = amount.saturating_sub(self.supply_long).unwrap_or(amount);
        self.supply_short = self.supply_short
            .checked_add(remainder)
            .ok_or_else(|| "Overflow in supply_short".to_string())?;
        
        self.last_updated = Utc::now();
        Ok(())
    }
    
    /// Get net position (long - short)
    /// Returns (is_long, net_amount)
    pub fn net_position(&self) -> (bool, Amount) {
        if self.supply_long >= self.supply_short {
            let net = self.supply_long.checked_sub(self.supply_short).unwrap_or(Amount::ZERO);
            (true, net)
        } else {
            let net = self.supply_short.checked_sub(self.supply_long).unwrap_or(Amount::ZERO);
            (false, net)
        }
    }
    
    /// Check if position has changed since last submission
    pub fn has_changed(&self, last_submission: Option<DateTime<Utc>>) -> bool {
        if let Some(last_sub) = last_submission {
            self.last_updated > last_sub
        } else {
            // Never submitted
            self.supply_long != Amount::ZERO || self.supply_short != Amount::ZERO
        }
    }
}

/// Overall supply state tracking for all assets
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupplyState {
    /// Supply per asset (symbol -> AssetSupply)
    pub supplies: HashMap<String, AssetSupply>,
    
    /// Last on-chain submission timestamp
    pub last_submission: Option<DateTime<Utc>>,
    
    /// Dirty flag - true if any supply changed since last submission
    pub dirty: bool,
}

impl SupplyState {
    pub fn new() -> Self {
        Self {
            supplies: HashMap::new(),
            last_submission: None,
            dirty: false,
        }
    }
    
    /// Initialize supply for an asset
    pub fn init_asset(&mut self, symbol: String, asset_id: u128) {
        if !self.supplies.contains_key(&symbol) {
            self.supplies.insert(symbol.clone(), AssetSupply::new(symbol, asset_id));
        }
    }
    
    /// Update supply for an asset
    pub fn update_supply(&mut self, symbol: &str, supply: AssetSupply) {
        self.supplies.insert(symbol.to_string(), supply);
        self.dirty = true;
    }
    
    /// Record a buy order execution (increases long position)
    pub fn record_buy(&mut self, symbol: &str, quantity: Amount) -> Result<(), String> {
        let supply = self
            .supplies
            .get_mut(symbol)
            .ok_or_else(|| format!("Asset {} not initialized in supply state", symbol))?;
        
        supply.add_long(quantity)?;
        self.dirty = true;
        Ok(())
    }
    
    /// Record a sell order execution (increases short position)
    pub fn record_sell(&mut self, symbol: &str, quantity: Amount) -> Result<(), String> {
        let supply = self
            .supplies
            .get_mut(symbol)
            .ok_or_else(|| format!("Asset {} not initialized in supply state", symbol))?;
        
        supply.add_short(quantity)?;
        self.dirty = true;
        Ok(())
    }
    
    /// Get assets that have changed since last submission
    pub fn get_changed_assets(&self) -> Vec<String> {
        self.supplies
            .values()
            .filter(|s| s.has_changed(self.last_submission))
            .map(|s| s.symbol.clone())
            .collect()
    }
    
    /// Check if submission is needed
    pub fn needs_submission(&self) -> bool {
        self.dirty && !self.get_changed_assets().is_empty()
    }
    
    /// Mark submission complete
    pub fn mark_submitted(&mut self) {
        self.last_submission = Some(Utc::now());
        self.dirty = false;
    }
    
    /// Get supply for a specific asset
    pub fn get_supply(&self, symbol: &str) -> Option<&AssetSupply> {
        self.supplies.get(symbol)
    }

    /// Get long quantity for a specific asset by asset_id
    pub fn get_long_quantity(&self, asset_id: u128) -> Amount {
        self.supplies
            .values()
            .find(|s| s.asset_id == asset_id)
            .map(|s| s.supply_long)
            .unwrap_or(Amount::ZERO)
    }

    /// Get short quantity for a specific asset by asset_id
    pub fn get_short_quantity(&self, asset_id: u128) -> Amount {
        self.supplies
            .values()
            .find(|s| s.asset_id == asset_id)
            .map(|s| s.supply_short)
            .unwrap_or(Amount::ZERO)
    }

    /// Get supply for a specific asset by asset_id
    pub fn get_supply_by_id(&self, asset_id: u128) -> Option<&AssetSupply> {
        self.supplies.values().find(|s| s.asset_id == asset_id)
    }

    /// Add long position by asset_id
    pub fn add_long_by_id(&mut self, asset_id: u128, amount: Amount) -> Result<(), String> {
        // Find the symbol for this asset_id
        let symbol = self
            .supplies
            .values()
            .find(|s| s.asset_id == asset_id)
            .map(|s| s.symbol.clone())
            .ok_or_else(|| format!("Asset {} not found in supply state", asset_id))?;

        // Use existing record_buy method
        self.record_buy(&symbol, amount)
    }

    /// Add short position by asset_id
    pub fn add_short_by_id(&mut self, asset_id: u128, amount: Amount) -> Result<(), String> {
        // Find the symbol for this asset_id
        let symbol = self
            .supplies
            .values()
            .find(|s| s.asset_id == asset_id)
            .map(|s| s.symbol.clone())
            .ok_or_else(|| format!("Asset {} not found in supply state", asset_id))?;

        // Use existing record_sell method
        self.record_sell(&symbol, amount)
    }

    /// Get a clone of the entire state (for rebalancer)
    pub fn clone_state(&self) -> Self {
        self.clone()
    }
}

impl Default for SupplyState {
    fn default() -> Self {
        Self::new()
    }
}