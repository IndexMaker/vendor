use super::delta_calculator::DeltaCalculator;
use super::margin_calculator::MarginCalculator;
use super::types::{Delta, RebalanceOrder, RebalancerConfig, RebalanceDecision, ExecutionMode};
use crate::buffer::{
    OrderBuffer, BufferedOrder, BufferOrderSide, InventorySimulator, SimulationReason,
};
use crate::onchain::{AssetMapper, VendorSubmitter, VendorDemand};
use crate::order_sender::{AssetOrder, OrderSender, OrderSide, OrderType};
use crate::supply::SupplyManager;
use crate::onchain::PriceTracker;
use common::amount::Amount;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock as TokioRwLock;

pub struct DeltaRebalancer<P>
where
    P: alloy::providers::Provider + Clone,
{
    config: RebalancerConfig,
    vendor_submitter: Arc<VendorSubmitter<P>>,
    supply_manager: Arc<RwLock<SupplyManager>>,
    margin_calculator: Arc<MarginCalculator>,
    order_sender: Option<Arc<TokioRwLock<dyn OrderSender>>>,
    asset_mapper: Arc<RwLock<AssetMapper>>,
    price_tracker: Arc<PriceTracker>,
    /// Async buffer for deferred exchange orders (on-chain first strategy)
    order_buffer: Option<Arc<OrderBuffer>>,
    /// Inventory simulator for simulation mode
    inventory_simulator: Option<Arc<InventorySimulator>>,
    /// Current USDC balance (updated externally)
    usdc_balance: Arc<RwLock<Amount>>,
}

impl<P> DeltaRebalancer<P>
where
    P: alloy::providers::Provider + Clone + Send + Sync + 'static,
{
    pub fn new(
        config: RebalancerConfig,
        vendor_submitter: Arc<VendorSubmitter<P>>,
        supply_manager: Arc<RwLock<SupplyManager>>,
        price_tracker: Arc<PriceTracker>,
        asset_mapper: Arc<RwLock<AssetMapper>>,
        order_sender: Option<Arc<TokioRwLock<dyn OrderSender>>>,
    ) -> Self {
        let margin_calculator = Arc::new(MarginCalculator::new(
            price_tracker.clone(),
            asset_mapper.clone(),
            config.clone(),
        ));

        Self {
            config,
            vendor_submitter,
            supply_manager,
            margin_calculator,
            order_sender,
            asset_mapper,
            price_tracker,
            order_buffer: None,
            inventory_simulator: None,
            usdc_balance: Arc::new(RwLock::new(Amount::ZERO)),
        }
    }

    /// Configure the order buffer for deferred exchange execution
    pub fn with_order_buffer(mut self, buffer: Arc<OrderBuffer>) -> Self {
        self.order_buffer = Some(buffer);
        self
    }

    /// Configure the inventory simulator for simulation mode
    pub fn with_inventory_simulator(mut self, simulator: Arc<InventorySimulator>) -> Self {
        self.inventory_simulator = Some(simulator);
        self
    }

    /// Update the USDC balance (call this periodically from balance monitoring)
    pub fn update_usdc_balance(&self, balance: Amount) {
        *self.usdc_balance.write() = balance;
    }

    /// Get current USDC balance
    pub fn get_usdc_balance(&self) -> Amount {
        *self.usdc_balance.read()
    }

    /// Determine execution mode for an order
    fn determine_execution_mode(
        &self,
        order: &RebalanceOrder,
        exchange_min_order_size: Amount,
    ) -> (ExecutionMode, String) {
        let usdc_balance = self.get_usdc_balance();
        let usdc_balance_f64 = usdc_balance.to_u128_raw() as f64 / 1e18;
        let order_usd_f64 = order.quantity_usd.to_u128_raw() as f64 / 1e18;

        // Check conditions for simulation mode
        let below_min_size = order.quantity < exchange_min_order_size;
        let insufficient_balance = usdc_balance_f64 < self.config.usdc_balance_threshold
            || usdc_balance_f64 < order_usd_f64;

        // Inventory simulation mode takes precedence
        if self.config.inventory_simulation_enabled {
            if below_min_size && insufficient_balance {
                return (
                    ExecutionMode::Simulated,
                    format!(
                        "Simulation: below min size ({:.6} < {:.6}) AND insufficient USDC (${:.2} < ${:.2})",
                        order.quantity.to_u128_raw() as f64 / 1e18,
                        exchange_min_order_size.to_u128_raw() as f64 / 1e18,
                        usdc_balance_f64,
                        order_usd_f64
                    ),
                );
            }
            if below_min_size {
                return (
                    ExecutionMode::Simulated,
                    format!(
                        "Simulation: below min order size ({:.6} < {:.6})",
                        order.quantity.to_u128_raw() as f64 / 1e18,
                        exchange_min_order_size.to_u128_raw() as f64 / 1e18,
                    ),
                );
            }
            if insufficient_balance {
                return (
                    ExecutionMode::Simulated,
                    format!(
                        "Simulation: insufficient USDC (${:.2} < ${:.2})",
                        usdc_balance_f64,
                        order_usd_f64.max(self.config.usdc_balance_threshold)
                    ),
                );
            }
        }

        // On-chain first mode (normal path with sufficient resources)
        if self.config.onchain_first_enabled {
            return (
                ExecutionMode::OnchainFirst,
                "On-chain first: submit to blockchain immediately, defer exchange".to_string(),
            );
        }

        // Normal mode (legacy behavior)
        (ExecutionMode::Normal, "Normal: execute on exchange first".to_string())
    }

    /// Fetch vendor demand from on-chain
    async fn fetch_vendor_demand(&self) -> eyre::Result<VendorDemand> {
        // CHANGED: Now uses real implementation via VendorSubmitter
        self.vendor_submitter.get_vendor_demand(self.config.vendor_id).await
    }

    /// Start the rebalance loop
    pub async fn run(self: Arc<Self>) {
        let mut interval = tokio::time::interval(Duration::from_secs(
            self.config.rebalance_interval_secs,
        ));

        tracing::info!("🔄 DeltaRebalancer started");
        tracing::info!("  Interval: {}s", self.config.rebalance_interval_secs);
        tracing::info!("  Min order size: ${}", self.config.min_order_size_usd);
        tracing::info!("  Total exposure: ${}", self.config.total_exposure_usd);
        tracing::info!("  On-chain submit: {}", self.config.enable_onchain_submit);

        loop {
            interval.tick().await;

            if let Err(e) = self.rebalance_once().await {
                tracing::error!("Rebalance cycle failed: {:?}", e);
            }
        }
    }

    /// Execute one rebalance cycle with "on-chain first" strategy
    ///
    /// Flow:
    /// 1. Fetch on-chain demand
    /// 2. Calculate delta
    /// 3. Generate orders with execution mode decisions
    /// 4. **ON-CHAIN FIRST**: Submit supply to blockchain IMMEDIATELY
    /// 5. **DEFERRED**: Queue exchange orders for later execution
    /// 6. **SIMULATION**: Track simulated positions when can't execute
    async fn rebalance_once(&self) -> eyre::Result<()> {
        tracing::info!("🔄 Rebalance cycle starting (on-chain first mode: {})",
            self.config.onchain_first_enabled);

        // Step 1: Fetch on-chain Demand
        tracing::debug!("  Fetching on-chain demand...");
        let demand = self.fetch_vendor_demand().await?;

        if demand.assets.is_empty() {
            tracing::info!("  No demand data - skipping cycle");
            return Ok(());
        }

        tracing::info!("  On-chain demand: {} assets", demand.assets.len());

        // Step 2: Get internal Supply state (including simulated positions)
        let mut supply = self.supply_manager.read().get_state();

        // Add simulated positions to supply calculation
        if let Some(ref sim) = self.inventory_simulator {
            for pos in sim.get_pending_simulations() {
                // Add simulated quantity to supply (we're pretending we have it)
                let _ = supply.add_long_by_id(pos.asset_id, pos.quantity);
            }
        }

        tracing::debug!(
            "  Current supply tracking: {} assets, dirty={}",
            supply.supplies.len(),
            supply.dirty
        );

        // Step 3: Calculate Delta
        tracing::debug!("  Calculating delta...");
        let delta = DeltaCalculator::calculate_delta(&demand, &supply);

        if delta.is_empty() {
            tracing::info!("  ✓ Portfolio balanced - no rebalancing needed");
            return Ok(());
        }

        tracing::info!(
            "  Delta: {} long, {} short",
            delta.long_positions.len(),
            delta.short_positions.len()
        );

        // Step 4: Calculate Margin per asset
        tracing::debug!("  Calculating margins...");
        let margins = self.margin_calculator.calculate_margins(&delta.all_assets());

        // Step 5: Generate orders with execution mode decisions
        let decisions = self.generate_orders_with_decisions(&delta, &margins)?;

        if decisions.is_empty() {
            tracing::info!("  No orders generated (all below min_order_size)");
            return Ok(());
        }

        tracing::info!("  Generated {} rebalance decisions", decisions.len());

        // =========================================================================
        // ON-CHAIN FIRST STRATEGY: Submit to blockchain IMMEDIATELY
        // =========================================================================
        if self.config.onchain_first_enabled || self.config.enable_onchain_submit {
            // Update supply state with ALL orders (including simulated)
            for decision in &decisions {
                match decision.execution_mode {
                    ExecutionMode::OnchainFirst | ExecutionMode::Simulated => {
                        // Update supply state AS IF we executed
                        self.update_supply_from_decision(decision)?;
                    }
                    ExecutionMode::Normal => {
                        // Normal mode: will update after actual execution
                    }
                }
            }

            // SUPER FAST: Submit to blockchain NOW
            let supply_state = self.supply_manager.read().get_state();

            if supply_state.needs_submission() {
                tracing::info!("  🚀 ON-CHAIN FIRST: Submitting supply to Castle IMMEDIATELY");

                let changed_symbols = supply_state.get_changed_assets();
                let mut asset_ids = Vec::new();
                let mut supply_long = Vec::new();
                let mut supply_short = Vec::new();

                for symbol in &changed_symbols {
                    if let Some(supply) = supply_state.get_supply(symbol) {
                        asset_ids.push(supply.asset_id);
                        supply_long.push(supply.supply_long);
                        supply_short.push(supply.supply_short);
                    }
                }

                if !asset_ids.is_empty() {
                    tracing::info!(
                        "  Submitting supply for {} changed assets",
                        asset_ids.len()
                    );

                    match self.vendor_submitter
                        .submit_supply(self.config.vendor_id, asset_ids, supply_long, supply_short)
                        .await
                    {
                        Ok(_) => {
                            self.supply_manager.write().state.mark_submitted();
                            tracing::info!("  ✓ Supply submitted to Castle successfully");

                            // Mark simulated positions as on-chain submitted
                            if let Some(ref sim) = self.inventory_simulator {
                                for decision in &decisions {
                                    if decision.execution_mode == ExecutionMode::Simulated {
                                        sim.mark_onchain_submitted(
                                            &decision.order.symbol,
                                            &format!("rebalance-{}", decision.order.asset_id),
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("  ✗ Failed to submit supply: {:?}", e);
                            // Continue anyway - we'll retry next cycle
                        }
                    }
                }
            }
        }

        // =========================================================================
        // DEFERRED EXECUTION: Queue exchange orders for later
        // =========================================================================
        let mut normal_orders = Vec::new();
        let mut deferred_count = 0;
        let mut simulated_count = 0;

        for decision in decisions {
            match decision.execution_mode {
                ExecutionMode::Normal => {
                    // Execute immediately (legacy flow)
                    normal_orders.push(decision.order);
                }
                ExecutionMode::OnchainFirst => {
                    // Queue for deferred exchange execution
                    if let Some(ref buffer) = self.order_buffer {
                        let buffered_order = BufferedOrder::new(
                            decision.order.asset_id,
                            decision.order.symbol.clone(),
                            match decision.order.side {
                                OrderSide::Buy => BufferOrderSide::Buy,
                                OrderSide::Sell => BufferOrderSide::Sell,
                            },
                            decision.order.quantity,
                            format!("rebalance-{}", decision.order.asset_id),
                        );

                        match buffer.push(buffered_order) {
                            Ok(()) => {
                                tracing::info!(
                                    "  📦 Deferred: {} {} {} ({})",
                                    decision.order.side,
                                    decision.order.quantity.to_u128_raw() as f64 / 1e18,
                                    decision.order.symbol,
                                    decision.mode_reason
                                );
                                deferred_count += 1;
                            }
                            Err(e) => {
                                tracing::error!(
                                    "  ✗ Failed to buffer order for {}: {:?}",
                                    decision.order.symbol,
                                    e
                                );
                            }
                        }
                    } else {
                        // No buffer configured - fall back to immediate execution
                        normal_orders.push(decision.order);
                    }
                }
                ExecutionMode::Simulated => {
                    // Track simulated position (no exchange execution)
                    if let Some(ref sim) = self.inventory_simulator {
                        let price = self.price_tracker
                            .get_price(&decision.order.symbol)
                            .unwrap_or(Amount::ZERO);

                        let reason = if decision.mode_reason.contains("insufficient") {
                            SimulationReason::InsufficientBalance
                        } else if decision.mode_reason.contains("min") {
                            SimulationReason::BelowMinOrderSize
                        } else {
                            SimulationReason::BelowMinAndInsufficientBalance
                        };

                        match sim.add_simulation(
                            decision.order.asset_id,
                            decision.order.symbol.clone(),
                            decision.order.quantity,
                            price,
                            reason,
                            format!("rebalance-{}", decision.order.asset_id),
                        ) {
                            Ok(_) => {
                                tracing::info!(
                                    "  🎭 Simulated: {} {} {} ({})",
                                    decision.order.side,
                                    decision.order.quantity.to_u128_raw() as f64 / 1e18,
                                    decision.order.symbol,
                                    decision.mode_reason
                                );
                                simulated_count += 1;
                            }
                            Err(e) => {
                                tracing::error!(
                                    "  ✗ Failed to simulate order for {}: {}",
                                    decision.order.symbol,
                                    e
                                );
                            }
                        }
                    }
                }
            }
        }

        // Execute any normal orders immediately
        if !normal_orders.is_empty() {
            let executed = self.execute_orders(normal_orders).await?;
            tracing::info!("  ✓ Executed {} orders immediately", executed);
        }

        tracing::info!(
            "  📊 Summary: {} deferred, {} simulated",
            deferred_count,
            simulated_count
        );

        Ok(())
    }

    /// Generate orders with execution mode decisions
    fn generate_orders_with_decisions(
        &self,
        delta: &Delta,
        margins: &std::collections::HashMap<u128, super::types::AssetMargin>,
    ) -> eyre::Result<Vec<RebalanceDecision>> {
        let mut decisions = Vec::new();

        // For simulation mode, we accept orders even below min_order_size
        let min_order_usd = if self.config.inventory_simulation_enabled {
            Amount::ZERO // Accept all orders in simulation mode
        } else {
            Amount::from_u128_with_scale(
                (self.config.min_order_size_usd * 1e18) as u128,
                18,
            )
        };

        // Exchange min order size (for mode decision)
        let exchange_min_order_size = Amount::from_u128_with_scale(
            (self.config.min_order_size_usd * 1e18) as u128,
            18,
        );

        // Process LONG positions (BUY orders)
        for (asset_id, delta_qty) in &delta.long_positions {
            let order = self.create_order(
                *asset_id,
                *delta_qty,
                OrderSide::Buy,
                margins,
                min_order_usd,
            )?;

            if let Some(order) = order {
                let (execution_mode, mode_reason) =
                    self.determine_execution_mode(&order, exchange_min_order_size);

                decisions.push(RebalanceDecision {
                    order,
                    execution_mode,
                    mode_reason,
                });
            }
        }

        // Process SHORT positions (SELL orders)
        for (asset_id, delta_qty) in &delta.short_positions {
            let order = self.create_order(
                *asset_id,
                *delta_qty,
                OrderSide::Sell,
                margins,
                min_order_usd,
            )?;

            if let Some(order) = order {
                let (execution_mode, mode_reason) =
                    self.determine_execution_mode(&order, exchange_min_order_size);

                decisions.push(RebalanceDecision {
                    order,
                    execution_mode,
                    mode_reason,
                });
            }
        }

        Ok(decisions)
    }

    /// Update supply state from a rebalance decision (before exchange execution)
    fn update_supply_from_decision(&self, decision: &RebalanceDecision) -> eyre::Result<()> {
        let mut supply_mgr = self.supply_manager.write();

        match decision.order.side {
            OrderSide::Buy => {
                supply_mgr.add_long(decision.order.asset_id, decision.order.quantity)?;
                tracing::debug!(
                    "      Pre-updated supply: +{:.8} long for {} (mode: {:?})",
                    decision.order.quantity.to_u128_raw() as f64 / 1e18,
                    decision.order.symbol,
                    decision.execution_mode
                );
            }
            OrderSide::Sell => {
                supply_mgr.add_short(decision.order.asset_id, decision.order.quantity)?;
                tracing::debug!(
                    "      Pre-updated supply: +{:.8} short for {} (mode: {:?})",
                    decision.order.quantity.to_u128_raw() as f64 / 1e18,
                    decision.order.symbol,
                    decision.execution_mode
                );
            }
        }

        Ok(())
    }

    /// Generate rebalance orders from delta and margins
    fn generate_orders(
        &self,
        delta: &Delta,
        margins: &std::collections::HashMap<u128, super::types::AssetMargin>,
    ) -> eyre::Result<Vec<RebalanceOrder>> {
        let mut orders = Vec::new();

        let min_order_usd = Amount::from_u128_with_scale(
            (self.config.min_order_size_usd * 1e18) as u128,
            18,
        );

        // Process LONG positions (BUY orders)
        for (asset_id, delta_qty) in &delta.long_positions {
            let order = self.create_order(
                *asset_id,
                *delta_qty,
                OrderSide::Buy,
                margins,
                min_order_usd,
            )?;

            if let Some(o) = order {
                orders.push(o);
            }
        }

        // Process SHORT positions (SELL orders)
        for (asset_id, delta_qty) in &delta.short_positions {
            let order = self.create_order(
                *asset_id,
                *delta_qty,
                OrderSide::Sell,
                margins,
                min_order_usd,
            )?;

            if let Some(o) = order {
                orders.push(o);
            }
        }

        Ok(orders)
    }

    /// Create a single order with rounding and margin capping
    fn create_order(
        &self,
        asset_id: u128,
        delta_qty: Amount,
        side: OrderSide,
        margins: &std::collections::HashMap<u128, super::types::AssetMargin>,
        min_order_usd: Amount,
    ) -> eyre::Result<Option<RebalanceOrder>> {
        // Get symbol
        let symbol = match self.asset_mapper.read().get_symbol(asset_id) {
            Some(s) => s.clone(),
            None => {
                tracing::warn!("Symbol not found for asset {}", asset_id);
                return Ok(None);
            }
        };

        // Get current price
        // CHANGED: get_price() returns Amount directly
        let price = match self.price_tracker.get_price(&symbol) {
            Some(p) => p,  // Already Amount type
            None => {
                tracing::warn!("No price for {}", symbol);
                return Ok(None);
            }
        };

        // Convert delta quantity to USD
        let delta_usd = delta_qty.checked_mul(price).unwrap_or(Amount::ZERO);

        // Check minimum order size
        if delta_usd < min_order_usd {
            tracing::debug!(
                "  Skipping {} - delta ${:.2} below min ${:.2}",
                symbol,
                delta_usd.to_u128_raw() as f64 / 1e18,
                min_order_usd.to_u128_raw() as f64 / 1e18
            );
            return Ok(None);
        }

        // Round up to min_order_size increments (in USD)
        let min_order_f64 = self.config.min_order_size_usd;
        let delta_usd_f64 = delta_usd.to_u128_raw() as f64 / 1e18;
        let rounded_usd = (delta_usd_f64 / min_order_f64).ceil() * min_order_f64;
        let rounded_usd_amount = Amount::from_u128_with_scale((rounded_usd * 1e18) as u128, 18);

        // Cap at margin
        let margin = margins.get(&asset_id);
        let max_qty = margin.map(|m| m.max_quantity).unwrap_or(Amount::MAX);
        let max_usd = max_qty.checked_mul(price).unwrap_or(Amount::MAX);

        let final_usd = rounded_usd_amount.min(max_usd);
        let final_qty = final_usd.checked_div(price).unwrap_or(Amount::ZERO);

        tracing::info!(
            "  Order: {} {} {:.8} @ ${:.2} (${:.2})",
            side,
            symbol,
            final_qty.to_u128_raw() as f64 / 1e18,
            price.to_u128_raw() as f64 / 1e18,
            final_usd.to_u128_raw() as f64 / 1e18
        );

        Ok(Some(RebalanceOrder {
            asset_id,
            symbol,
            side,
            quantity: final_qty,
            quantity_usd: final_usd,
        }))
    }

    /// Execute all orders
    async fn execute_orders(&self, orders: Vec<RebalanceOrder>) -> eyre::Result<usize> {
        let order_sender = match &self.order_sender {
            Some(sender) => sender,
            None => {
                tracing::warn!("OrderSender not available - skipping order execution");
                return Ok(0);
            }
        };

        let mut executed_count = 0;

        for order in orders {
            let asset_order = AssetOrder {
                symbol: order.symbol.clone(),
                side: order.side,
                quantity: order.quantity,
                order_type: OrderType::Market,
                limit_price: None,
            };

            match order_sender.write().await.send_orders(vec![asset_order]).await {
                Ok(results) => {
                    // Take the first (and only) result
                    if let Some(result) = results.into_iter().next() {
                        tracing::info!(
                            "    ✓ Filled: {} {:.8} {} @ ${:.2} (fee: ${:.4})",
                            order.side,
                            result.filled_quantity.to_u128_raw() as f64 / 1e18,
                            order.symbol,
                            result.avg_price.to_u128_raw() as f64 / 1e18,
                            result.fees.to_u128_raw() as f64 / 1e18
                        );

                        self.update_supply_from_fill(&order, result.filled_quantity)?;
                        executed_count += 1;
                    }
                }
                Err(e) => {
                    tracing::error!("    ✗ Order failed for {}: {:?}", order.symbol, e);
                }
            }
        }

        Ok(executed_count)
    }

    /// Update internal supply state after order fill
    fn update_supply_from_fill(
        &self,
        order: &RebalanceOrder,
        filled_qty: Amount,  // Changed from f64 to Amount
    ) -> eyre::Result<()> {
        let mut supply_mgr = self.supply_manager.write();

        match order.side {
            OrderSide::Buy => {
                // Increase long position
                supply_mgr.add_long(order.asset_id, filled_qty)?;
                tracing::debug!(
                    "      Updated supply: +{:.8} long",
                    filled_qty.to_u128_raw() as f64 / 1e18
                );
            }
            OrderSide::Sell => {
                // Increase short position (or reduce long)
                supply_mgr.add_short(order.asset_id, filled_qty)?;
                tracing::debug!(
                    "      Updated supply: +{:.8} short",
                    filled_qty.to_u128_raw() as f64 / 1e18
                );
            }
        }

        Ok(())
    }
}