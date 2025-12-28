use super::storage::InventorySnapshot;
use super::types::{IndexPosition, Order, OrderResult, OrderSide, Position};
use crate::basket::{BasketManager, Index};
use crate::onchain::PriceTracker;
use common::amount::Amount;
use eyre::{eyre, Result};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

pub struct InventoryManager {
    // Asset positions (BTC, ETH, SOL, etc.)
    positions: HashMap<String, Position>,

    // Index positions (SY100, SYAZ, etc.)
    index_positions: HashMap<String, IndexPosition>,

    // Reference to basket manager (for index composition)
    basket_manager: Arc<RwLock<BasketManager>>,

    // Reference to price tracker (for current prices)
    price_tracker: Arc<PriceTracker>,

    // Storage path
    storage_path: PathBuf,
}

impl InventoryManager {
    pub fn new(
        basket_manager: Arc<RwLock<BasketManager>>,
        price_tracker: Arc<PriceTracker>,
        storage_path: PathBuf,
    ) -> Self {
        Self {
            positions: HashMap::new(),
            index_positions: HashMap::new(),
            basket_manager,
            price_tracker,
            storage_path,
        }
    }

    pub async fn load_from_storage(
        basket_manager: Arc<RwLock<BasketManager>>,
        price_tracker: Arc<PriceTracker>,
        storage_path: PathBuf,
    ) -> Result<Self> {
        let snapshot = InventorySnapshot::load_from_file(&storage_path).await?;

        let mut manager = Self::new(basket_manager, price_tracker, storage_path);
        manager.positions = snapshot.positions;

        Ok(manager)
    }

    /// Process a buy order for an index
    pub async fn process_buy_order(&mut self, mut order: Order) -> Result<OrderResult> {
        tracing::info!(
            "Processing buy order: {} {} units of {}",
            order.order_id,
            order.quantity,
            order.index_symbol
        );

        // Validate order
        if order.side != OrderSide::Buy {
            order.mark_rejected();
            return Ok(OrderResult::rejected(
                order.order_id.clone(),
                "Only Buy orders are supported currently".to_string(),
            ));
        }

        // Get index composition
        let index = {
            let bm = self.basket_manager.read();
            bm.get_index(&order.index_symbol)
                .ok_or_else(|| eyre!("Index '{}' not found", order.index_symbol))?
                .clone()
        };

        // Calculate required quantities for each underlying asset
        let asset_quantities = self.calculate_asset_quantities(&index, order.quantity)?;

        // Verify prices are available
        for (symbol, _) in &asset_quantities {
            if self.price_tracker.get_price(symbol).is_none() {
                order.mark_failed();
                return Ok(OrderResult::failed(
                    order.order_id.clone(),
                    format!("Price not available for asset: {}", symbol),
                ));
            }
        }

        // Update asset positions
        for (symbol, quantity) in &asset_quantities {
            self.update_asset_position(symbol, *quantity, &order.order_id);
        }

        // Update index position
        self.update_index_position(&order.index_symbol, order.quantity);

        // Mark order as filled
        order.mark_filled();

        // Save to storage
        self.save_to_storage().await?;

        // Prepare result
        let index_position = self
            .index_positions
            .get(&order.index_symbol)
            .cloned()
            .ok_or_else(|| eyre!("Index position not found after update"))?;

        let asset_positions: Vec<Position> = asset_quantities
            .keys()
            .filter_map(|symbol| self.positions.get(symbol).cloned())
            .collect();

        tracing::info!(
            "✓ Order {} filled: {} units of {}",
            order.order_id,
            order.quantity,
            order.index_symbol
        );

        Ok(OrderResult::success(
            order.order_id,
            index_position,
            asset_positions,
        ))
    }

    /// Calculate required quantities of underlying assets for a given index quantity
    fn calculate_asset_quantities(
        &self,
        index: &Index,
        index_quantity: Amount,
    ) -> Result<HashMap<String, Amount>> {
        let mut quantities = HashMap::new();

        for asset in &index.assets {
            // Parse weight as Amount
            let weight_f64: f64 = asset
                .weights
                .parse()
                .map_err(|e| eyre!("Failed to parse weight: {}", e))?;
            let weight = Amount::from_u128_raw((weight_f64 * 1e18) as u128);

            // Calculate required quantity: index_quantity * weight * asset_quantity
            let asset_qty_f64: f64 = asset.quantity;
            let asset_base_qty = Amount::from_u128_raw((asset_qty_f64 * 1e18) as u128);
                    
            let required = index_quantity
                .checked_mul(weight)
                .ok_or_else(|| eyre!("Overflow in weight multiplication"))?
                .checked_mul(asset_base_qty)
                .ok_or_else(|| eyre!("Overflow in quantity multiplication"))?;

            quantities.insert(asset.pair.clone(), required);
        }

        Ok(quantities)
    }

    /// Update asset position (or create if doesn't exist)
    fn update_asset_position(&mut self, symbol: &str, delta: Amount, order_id: &str) {
        let position = self
            .positions
            .entry(symbol.to_string())
            .or_insert_with(|| Position::new(symbol.to_string(), OrderSide::Buy));

        position.add_balance(delta);

        // Add mock lot for tracking
        let lot = super::types::Lot::mock(order_id, symbol, delta);
        position.add_lot(lot);

        tracing::debug!("Updated position for {}: balance={}", symbol, position.balance);
    }

    /// Update index position (or create if doesn't exist)
    fn update_index_position(&mut self, index_symbol: &str, delta: Amount) {
        let position = self
            .index_positions
            .entry(index_symbol.to_string())
            .or_insert_with(|| IndexPosition::new(index_symbol.to_string()));

        position.add_units(delta);

        tracing::debug!(
            "Updated index position for {}: units={}",
            index_symbol,
            position.units_held
        );
    }

    /// Get asset position
    pub fn get_position(&self, symbol: &str) -> Option<&Position> {
        self.positions.get(symbol)
    }

    /// Get index position
    pub fn get_index_position(&self, index_symbol: &str) -> Option<&IndexPosition> {
        self.index_positions.get(index_symbol)
    }

    /// Get all positions
    pub fn get_all_positions(&self) -> &HashMap<String, Position> {
        &self.positions
    }

    /// Get all index positions
    pub fn get_all_index_positions(&self) -> &HashMap<String, IndexPosition> {
        &self.index_positions
    }

    /// Save current state to storage
    pub async fn save_to_storage(&self) -> Result<()> {
        let snapshot = InventorySnapshot {
            positions: self.positions.clone(),
        };

        snapshot.save_to_file(&self.storage_path).await?;
        Ok(())
    }

    /// Generate summary
    pub fn summary(&self) -> String {
        format!(
            "InventoryManager: {} asset position(s), {} index position(s)",
            self.positions.len(),
            self.index_positions.len()
        )
    }
}