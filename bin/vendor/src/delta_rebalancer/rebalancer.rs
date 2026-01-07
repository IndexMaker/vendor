use super::delta_calculator::DeltaCalculator;
use super::margin_calculator::MarginCalculator;
use super::types::{Delta, RebalanceOrder, RebalancerConfig, VendorDemand};
use crate::onchain::{AssetMapper, OnchainReader};
use crate::order_sender::{AssetOrder, OrderSender, OrderSide, OrderType};
use crate::supply::SupplyManager;
use crate::PriceTracker;
use common::amount::Amount;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock as TokioRwLock;

pub struct DeltaRebalancer<P>
where
    P: alloy::providers::Provider + Clone,
{
    config: RebalancerConfig,
    onchain_reader: Arc<OnchainReader<P>>,
    supply_manager: Arc<parking_lot::RwLock<SupplyManager>>,
    margin_calculator: Arc<MarginCalculator>,
    order_sender: Option<Arc<TokioRwLock<dyn OrderSender>>>,  // CHANGED TYPE
    asset_mapper: Arc<parking_lot::RwLock<AssetMapper>>,
    price_tracker: Arc<PriceTracker>,
}

impl<P> DeltaRebalancer<P>
where
    P: alloy::providers::Provider + Clone + Send + Sync + 'static,
{
    pub fn new(
        config: RebalancerConfig,
        onchain_reader: Arc<OnchainReader<P>>,
        supply_manager: Arc<parking_lot::RwLock<SupplyManager>>,
        price_tracker: Arc<PriceTracker>,
        asset_mapper: Arc<parking_lot::RwLock<AssetMapper>>,
        order_sender: Option<Arc<TokioRwLock<dyn OrderSender>>>,  // CHANGED TYPE
    ) -> Self {
        let margin_calculator = Arc::new(MarginCalculator::new(
            price_tracker.clone(),
            asset_mapper.clone(),
            config.clone(),
        ));

        Self {
            config,
            onchain_reader,
            supply_manager,
            margin_calculator,
            order_sender,
            asset_mapper,
            price_tracker,
        }
    }

    /// Start the rebalance loop
    pub async fn run(self: Arc<Self>) {
        let mut interval = tokio::time::interval(Duration::from_secs(
            self.config.rebalance_interval_secs,
        ));

        tracing::info!(
            "🔄 DeltaRebalancer started (interval: {}s)",
            self.config.rebalance_interval_secs
        );

        loop {
            interval.tick().await;

            if let Err(e) = self.rebalance_once().await {
                tracing::error!("Rebalance cycle failed: {:?}", e);
            }
        }
    }

    /// Execute one rebalance cycle
    async fn rebalance_once(&self) -> eyre::Result<()> {
        tracing::info!("🔄 Rebalance cycle starting");

        // Step 1: Fetch on-chain Demand
        tracing::debug!("  Fetching on-chain demand...");
        let demand = self.fetch_vendor_demand().await?;

        if demand.assets.is_empty() {
            tracing::info!("  No demand data - skipping cycle");
            return Ok(());
        }

        tracing::info!("  On-chain demand: {} assets", demand.assets.len());

        // Step 2: Get internal Supply state (using your SupplyState)
        let supply = self.supply_manager.read().get_state();

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

        // Step 5: Generate orders
        let orders = self.generate_orders(&delta, &margins)?;

        if orders.is_empty() {
            tracing::info!("  No orders generated (all below min_order_size)");
            return Ok(());
        }

        tracing::info!("  Generated {} rebalance orders", orders.len());

        // Step 6: Execute orders
        let executed = self.execute_orders(orders).await?;

        tracing::info!("  ✓ Executed {} orders", executed);

        // Step 7: Check if supply needs on-chain submission
        if self.config.enable_onchain_submit {
            let supply_state = self.supply_manager.read().get_state();
            
            if supply_state.needs_submission() {
                tracing::info!("  Supply changed - needs on-chain submission");
                // TODO: Implement submitSupply call
                tracing::debug!("  Skipping on-chain supply submission (not implemented yet)");
            }
        }

        Ok(())
    }

    /// Fetch vendor demand from on-chain
    async fn fetch_vendor_demand(&self) -> eyre::Result<VendorDemand> {
        // TODO: Implement IFactor::getVendorDemand() call
        // For now, return empty demand
        tracing::warn!("getVendorDemand() not implemented yet - returning empty");
        Ok(VendorDemand {
            assets: std::collections::HashMap::new(),
        })
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