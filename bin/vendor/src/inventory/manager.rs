use super::storage::InventorySnapshot;
use super::types::{IndexPosition, Order, OrderResult, OrderSide, Position};
use crate::basket::{BasketManager, Index};
use crate::supply::SupplyManager;
use crate::onchain::PriceTracker;
use crate::order_sender::{AssetOrder, ExecutionResult, OrderSender};
use common::amount::Amount;
use eyre::{eyre, Result};
use parking_lot::RwLock as SyncRwLock;
use rand::Rng;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock as TokioRwLock;


pub struct InventoryManager {
    // Asset positions (BTC, ETH, SOL, etc.)
    positions: HashMap<String, Position>,

    // Index positions (SY100, SYAZ, etc.)
    index_positions: HashMap<String, IndexPosition>,

    // Reference to basket manager (for index composition)
    basket_manager: Arc<SyncRwLock<BasketManager>>,

    // Reference to price tracker (for current prices)
    price_tracker: Arc<PriceTracker>,

    // Storage path
    storage_path: PathBuf,

    total_fees_paid: Amount,

    order_sender: Option<Arc<TokioRwLock<dyn OrderSender>>>,
    
    supply_manager: Option<Arc<SyncRwLock<SupplyManager>>>,
}

impl InventoryManager {
    pub fn new(
        basket_manager: Arc<SyncRwLock<BasketManager>>,
        price_tracker: Arc<PriceTracker>,
        storage_path: PathBuf,
        order_sender: Option<Arc<TokioRwLock<dyn OrderSender>>>,
        supply_manager: Option<Arc<SyncRwLock<SupplyManager>>>,
    ) -> Self {
        Self {
            positions: HashMap::new(),
            index_positions: HashMap::new(),
            basket_manager,
            price_tracker,
            storage_path,
            total_fees_paid: Amount::ZERO,
            order_sender,
            supply_manager,
        }
    }

    /// Get total fees paid
    pub fn get_total_fees(&self) -> Amount {
        self.total_fees_paid
    }

    /// Record fees from order execution
    fn record_fees(&mut self, fees: Amount) {
        self.total_fees_paid = self
            .total_fees_paid
            .checked_add(fees)
            .unwrap_or(self.total_fees_paid);
    }

    pub async fn load_from_storage(
        basket_manager: Arc<SyncRwLock<BasketManager>>,
        price_tracker: Arc<PriceTracker>,
        storage_path: PathBuf,
        order_sender: Option<Arc<TokioRwLock<dyn OrderSender>>>,
        supply_manager: Option<Arc<SyncRwLock<SupplyManager>>>,
    ) -> Result<Self> {
        let snapshot = InventorySnapshot::load_from_file(&storage_path).await?;

        let mut manager = Self::new(
            basket_manager,
            price_tracker,
            storage_path,
            order_sender,
            supply_manager,
        );
        manager.positions = snapshot.positions;

        Ok(manager)
    }

    /// Process a buy order for an index
    pub async fn process_buy_order(&mut self, mut order: Order) -> Result<OrderResult> {
        tracing::info!(
            "Processing buy order: {} ${} for {}",
            order.order_id,
            order.collateral_usd,
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

        // Calculate index price (weighted sum of asset prices)
        let index_price = self.calculate_index_price(&index)?;
        tracing::info!("Index '{}' price: ${}", order.index_symbol, index_price);

        // Calculate units to buy: collateral / index_price
        let units = self.calculate_units_from_collateral(order.collateral_usd, index_price)?;
        tracing::info!("Buying {} units of '{}'", units, order.index_symbol);

        // Calculate required quantities for each underlying asset
        let asset_quantities = self.calculate_asset_quantities(&index, units)?;

        // Execute orders via OrderSender if available
        let mut total_fees = Amount::ZERO;
        
        if let Some(ref sender) = self.order_sender {
            // Real order execution
            let execution_results = self.execute_asset_orders(&asset_quantities, &order.order_id, sender).await?;

            for result in &execution_results {
                if result.is_success() {
                    self.update_asset_position(&result.symbol, result.filled_quantity, &order.order_id);
                    total_fees = total_fees.checked_add(result.fees).unwrap_or(total_fees);

                    // Record in supply manager
                    // Remove "USDT" suffix to get base symbol (e.g., BTCUSDT -> BTC)
                    if let Some(ref supply_mgr) = self.supply_manager {
                        let base_symbol = result.symbol.trim_end_matches("USDT");
                        if let Err(e) = supply_mgr.write().record_buy(base_symbol, result.filled_quantity) {
                            tracing::error!("Failed to record supply for {}: {:?}", base_symbol, e);
                        }
                    }
                } else {
                    tracing::error!("Failed to execute order for {}: {:?}", result.symbol, result.error_message);
                }
            }

            self.record_fees(total_fees);
        } else {
            // Mock execution (no OrderSender)
            tracing::warn!("No OrderSender - using mock execution");
            for (symbol, quantity) in &asset_quantities {
                self.update_asset_position(symbol, *quantity, &order.order_id);
            }
        }

        // Update index position
        self.update_index_position(&order.index_symbol, units);

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
            "✓ Order {} filled: {} units of '{}' (cost: ${}, fees: ${})",
            order.order_id,
            units,
            order.index_symbol,
            order.collateral_usd,
            total_fees
        );

        Ok(OrderResult::success(
            order.order_id,
            index_position,
            asset_positions,
            total_fees,
        ))
    }

    /// Execute asset orders via OrderSender
    async fn execute_asset_orders(
        &self,
        asset_quantities: &HashMap<String, Amount>,
        parent_order_id: &str,
        sender: &Arc<TokioRwLock<dyn OrderSender>>,
    ) -> Result<Vec<ExecutionResult>> {
        let mut orders = Vec::new();

        for (symbol, quantity) in asset_quantities {
            // Convert symbol to trading pair (e.g., BTC -> BTCUSDT)
            let trading_symbol = format!("{}USDT", symbol);

            let order = AssetOrder::limit(
                trading_symbol,
                crate::order_sender::OrderSide::Buy,
                *quantity,
                Amount::ZERO, // Will use smart pricing
            );

            orders.push(order);
        }

        if orders.is_empty() {
            return Ok(vec![]);
        }

        tracing::info!(
            "Executing {} asset orders for parent order {}",
            orders.len(),
            parent_order_id
        );

        let mut sender_guard = sender.write().await;
        let results = sender_guard.send_orders(orders).await?;

        Ok(results)
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

    /// Generate mock price for an asset (temporary - will be replaced with real prices)
    fn get_mock_price(&self, _symbol: &str) -> Amount {
        let mut rng = rand::thread_rng();
        let price_f64 = rng.gen_range(1.0..=1000.0);
        Amount::from_u128_raw((price_f64 * 1e18) as u128)
    }

    /// Calculate index price as weighted sum of asset prices
    fn calculate_index_price(&self, index: &Index) -> Result<Amount> {
        let mut index_price = Amount::ZERO;

        for asset in &index.assets {
            // Get mock price (TODO: replace with real price from PriceTracker)
            let asset_price = self.get_mock_price(&asset.pair);

            // Parse weight
            let weight_f64: f64 = asset
                .weights
                .parse()
                .map_err(|e| eyre!("Failed to parse weight: {}", e))?;
            let weight = Amount::from_u128_raw((weight_f64 * 1e18) as u128);

            // Parse asset quantity
            let asset_qty_f64: f64 = asset.quantity;
            let asset_qty = Amount::from_u128_raw((asset_qty_f64 * 1e18) as u128);

            // Contribution = weight * asset_qty * asset_price
            let contribution = weight
                .checked_mul(asset_qty)
                .ok_or_else(|| eyre!("Overflow in weight * quantity"))?
                .checked_mul(asset_price)
                .ok_or_else(|| eyre!("Overflow in price calculation"))?;

            index_price = index_price
                .checked_add(contribution)
                .ok_or_else(|| eyre!("Overflow in index price sum"))?;
        }

        Ok(index_price)
    }

    /// Calculate units to buy from USD collateral
    fn calculate_units_from_collateral(
        &self,
        collateral_usd: Amount,
        index_price: Amount,
    ) -> Result<Amount> {
        collateral_usd
            .checked_div(index_price)
            .ok_or_else(|| eyre!("Failed to calculate units from collateral"))
    }

    pub fn position_count(&self) -> usize {
        self.positions.len()
    }
}