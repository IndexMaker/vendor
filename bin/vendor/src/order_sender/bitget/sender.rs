use super::super::traits::OrderSender;
use super::super::types::{AssetOrder, ExecutionResult, OrderSide, OrderStatus, OrderType};
use super::client::BitgetClient;
use super::pricing::PricingStrategy;
use common::amount::Amount;
use eyre::{Context, Result};
use std::collections::HashMap;
use std::time::Duration;

pub struct BitgetOrderSender {
    client: BitgetClient,
    pricing: PricingStrategy,
    trading_enabled: bool,
    retry_attempts: u8,
    active: bool,
    fill_timeout: Duration,
}

impl BitgetOrderSender {
    pub fn new(
        api_key: String,
        api_secret: String,
        passphrase: String,
        price_limit_bps: u16,
        retry_attempts: u8,
        trading_enabled: bool,
    ) -> Self {
        Self {
            client: BitgetClient::new(api_key, api_secret, passphrase),
            pricing: PricingStrategy::new(price_limit_bps),
            trading_enabled,
            retry_attempts,
            active: false,
            fill_timeout: Duration::from_secs(30), // Wait up to 30s for order fill
        }
    }

    /// Place a single order with retry logic
    async fn place_order_with_retry(&self, order: &AssetOrder) -> Result<ExecutionResult> {
        if !self.active {
            return Ok(ExecutionResult::failed(
                order.symbol.clone(),
                "N/A".to_string(),
                "Order sender not started".to_string(),
            ));
        }

        if !self.trading_enabled {
            tracing::warn!("Trading disabled - order not executed: {:?}", order);
            return Ok(ExecutionResult::failed(
                order.symbol.clone(),
                "DRY-RUN".to_string(),
                "Trading disabled (dry-run mode)".to_string(),
            ));
        }

        let mut last_error = None;

        for attempt in 1..=self.retry_attempts {
            match self.execute_single_order(order).await {
                Ok(result) => {
                    if result.is_success() {
                        return Ok(result);
                    } else {
                        // Order failed but no exception - don't retry
                        return Ok(result);
                    }
                }
                Err(e) => {
                    last_error = Some(e);
                    
                    if attempt < self.retry_attempts {
                        let backoff = Duration::from_millis(500 * attempt as u64);
                        tracing::warn!(
                            "Order attempt {}/{} failed for {}: {:?}, retrying in {:?}",
                            attempt,
                            self.retry_attempts,
                            order.symbol,
                            last_error,
                            backoff
                        );
                        tokio::time::sleep(backoff).await;
                    }
                }
            }
        }

        // All retries exhausted
        let error_msg = format!(
            "Failed after {} attempts: {:?}",
            self.retry_attempts,
            last_error.unwrap()
        );

        Ok(ExecutionResult::failed(
            order.symbol.clone(),
            "RETRY-EXHAUSTED".to_string(),
            error_msg,
        ))
    }

    /// Execute a single order (no retry logic)
    async fn execute_single_order(&self, order: &AssetOrder) -> Result<ExecutionResult> {
        match order.order_type {
            OrderType::Limit => self.execute_limit_order(order).await,
            OrderType::Market => self.execute_market_order(order).await,
        }
    }

    /// Execute a limit order with smart pricing
    async fn execute_limit_order(&self, order: &AssetOrder) -> Result<ExecutionResult> {
        // Get current best price
        let best_price = match order.side {
            OrderSide::Buy => self.client.get_best_ask(&order.symbol).await?,
            OrderSide::Sell => self.client.get_best_bid(&order.symbol).await?,
        };

        // Calculate limit price with spread
        let limit_price = if let Some(custom_price) = order.limit_price {
            custom_price
        } else {
            self.pricing.calculate_limit_price(best_price, order.side)?
        };

        tracing::info!(
            "Executing limit order: {} {} {} @ ${} (best: ${}, spread: {})",
            order.side,
            order.quantity,
            order.symbol,
            limit_price.to_u128_raw() as f64 / 1e18,
            best_price.to_u128_raw() as f64 / 1e18,
            self.pricing.spread_as_percentage()
        );

        // Place order on Bitget
        let client_order_id = format!("VW-{}", chrono::Utc::now().timestamp_millis());
        let place_response = self
            .client
            .place_limit_order_amount(
                &order.symbol,
                order.side,
                order.quantity,
                limit_price,
                Some(client_order_id.clone()),
            )
            .await
            .context("Failed to place limit order")?;

        tracing::info!(
            "Order placed: {} (client: {})",
            place_response.order_id,
            client_order_id
        );

        // Wait for fill (with timeout)
        let order_detail = match self
            .client
            .wait_for_fill(&place_response.order_id, self.fill_timeout)
            .await
        {
            Ok(detail) => detail,
            Err(e) => {
                tracing::warn!(
                    "Order {} did not fill within {:?}: {:?}",
                    place_response.order_id,
                    self.fill_timeout,
                    e
                );

                // Try to get current order status
                let detail = self
                    .client
                    .get_order(&place_response.order_id)
                    .await
                    .context("Failed to get order status after timeout")?;

                if detail.is_partially_filled() {
                    tracing::warn!("Order {} partially filled: {}", place_response.order_id, detail.base_volume);
                } else {
                    tracing::warn!("Order {} not filled, status: {}", place_response.order_id, detail.status);
                }

                detail
            }
        };

        // Convert to ExecutionResult
        self.order_detail_to_execution_result(order_detail)
    }

    /// Execute a market order (immediate execution)
    async fn execute_market_order(&self, order: &AssetOrder) -> Result<ExecutionResult> {
        tracing::info!(
            "Executing market order: {} {} {}",
            order.side,
            order.quantity,
            order.symbol
        );

        let client_order_id = format!("VW-{}", chrono::Utc::now().timestamp_millis());
        let place_response = self
            .client
            .place_market_order(
                &order.symbol,
                match order.side {
                    OrderSide::Buy => "buy",
                    OrderSide::Sell => "sell",
                },
                &BitgetClient::amount_to_quantity_string(order.quantity),
                Some(client_order_id.clone()),
            )
            .await
            .context("Failed to place market order")?;

        tracing::info!(
            "Market order placed: {} (client: {})",
            place_response.order_id,
            client_order_id
        );

        // Market orders should fill quickly
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Get order status
        let order_detail = self
            .client
            .get_order(&place_response.order_id)
            .await
            .context("Failed to get order status")?;

        self.order_detail_to_execution_result(order_detail)
    }

    /// Convert Bitget OrderDetail to ExecutionResult
    fn order_detail_to_execution_result(
        &self,
        detail: super::types::OrderDetail,
    ) -> Result<ExecutionResult> {
        let filled_qty_f64: f64 = detail.base_volume.parse().unwrap_or(0.0);
        let avg_price_f64: f64 = detail.avg_price.parse().unwrap_or(0.0);
        let fee_f64: f64 = detail.fee.parse().unwrap_or(0.0);

        let filled_quantity = Amount::from_u128_raw((filled_qty_f64 * 1e18) as u128);
        let avg_price = Amount::from_u128_raw((avg_price_f64 * 1e18) as u128);
        let fees = Amount::from_u128_raw((fee_f64 * 1e18) as u128);

        let status = if detail.is_filled() {
            OrderStatus::Filled
        } else if detail.is_partially_filled() {
            OrderStatus::PartiallyFilled
        } else if detail.is_pending() {
            OrderStatus::Pending
        } else if detail.status == "cancelled" {
            OrderStatus::Cancelled
        } else {
            OrderStatus::Failed
        };

        let error_message = if status == OrderStatus::Failed || status == OrderStatus::Cancelled {
            Some(format!("Order status: {}", detail.status))
        } else {
            None
        };

        Ok(ExecutionResult {
            symbol: detail.symbol,
            order_id: detail.order_id,
            filled_quantity,
            avg_price,
            fees,
            status,
            error_message,
        })
    }
}

#[async_trait::async_trait]
impl OrderSender for BitgetOrderSender {
    async fn send_orders(&mut self, orders: Vec<AssetOrder>) -> Result<Vec<ExecutionResult>> {
        if !self.active {
            return Err(eyre::eyre!("Bitget order sender not started"));
        }

        tracing::info!(
            "📤 Sending {} order(s) to Bitget (trading: {})",
            orders.len(),
            self.trading_enabled
        );

        let mut results = Vec::new();

        for order in orders {
            tracing::info!(
                "  → {} {} {} (type: {:?})",
                order.side,
                order.quantity,
                order.symbol,
                order.order_type
            );

            let result = self.place_order_with_retry(&order).await?;

            if result.is_success() {
                tracing::info!(
                    "  ✓ Filled: {} - {} @ ${} (fee: ${})",
                    result.order_id,
                    result.filled_quantity,
                    result.avg_price.to_u128_raw() as f64 / 1e18,
                    result.fees.to_u128_raw() as f64 / 1e18
                );
            } else {
                tracing::warn!(
                    "  ✗ Failed: {} - {:?}",
                    result.order_id,
                    result.error_message.as_ref().unwrap_or(&"Unknown error".to_string())
                );
            }

            results.push(result);
        }

        Ok(results)
    }

    async fn get_balances(&self) -> Result<HashMap<String, Amount>> {
        if !self.active {
            return Err(eyre::eyre!("Bitget order sender not started"));
        }

        let balances = self.client.get_balances().await?;
        let mut result = HashMap::new();

        for balance in balances {
            let available_f64: f64 = balance.available.parse().unwrap_or(0.0);
            
            if available_f64 > 0.0 {
                let amount = Amount::from_u128_raw((available_f64 * 1e18) as u128);
                result.insert(balance.coin, amount);
            }
        }

        Ok(result)
    }

    async fn start(&mut self) -> Result<()> {
        if self.active {
            return Err(eyre::eyre!("Bitget order sender already started"));
        }

        self.active = true;

        if self.trading_enabled {
            tracing::info!(
                "🔥 Bitget order sender started (LIVE TRADING - spread: {})",
                self.pricing.spread_as_percentage()
            );
        } else {
            tracing::warn!("🎮 Bitget order sender started (DRY-RUN mode - no real trades)");
        }

        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        if !self.active {
            return Err(eyre::eyre!("Bitget order sender not active"));
        }

        self.active = false;
        tracing::info!("🔥 Bitget order sender stopped");
        Ok(())
    }

    fn is_active(&self) -> bool {
        self.active
    }
}