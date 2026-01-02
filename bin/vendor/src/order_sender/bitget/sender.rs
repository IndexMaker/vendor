use super::super::traits::OrderSender;
use super::super::types::{AssetOrder, ExecutionResult, OrderSide, OrderStatus, OrderType};
use super::client::BitgetClient;
use super::pricing::PricingStrategy;
use common::amount::Amount;
use eyre::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;


/// Batch execution summary
pub struct BatchExecutionSummary {
    pub total: usize,
    pub successful: usize,
    pub failed: usize,
    pub partial: usize,
    pub results: Vec<ExecutionResult>,
}

impl BatchExecutionSummary {
    pub fn success_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            (self.successful as f64 / self.total as f64) * 100.0
        }
    }

    pub fn has_failures(&self) -> bool {
        self.failed > 0
    }
}

pub struct BitgetOrderSender {
    client: BitgetClient,
    pricing: PricingStrategy,
    trading_enabled: bool,
    retry_attempts: u8,
    active: bool,
    fill_timeout: Duration,
    consecutive_failures: Arc<AtomicU32>,
    max_consecutive_failures: u32,
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
            consecutive_failures: Arc::new(AtomicU32::new(0)),
            max_consecutive_failures: 5, // Circuit breaker threshold
        }
    }

    /// Check if circuit breaker is tripped
    fn is_circuit_open(&self) -> bool {
        self.consecutive_failures.load(Ordering::Relaxed) >= self.max_consecutive_failures
    }

    /// Reset circuit breaker
    fn reset_circuit(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
    }

    /// Increment failure counter
    fn record_failure(&self) {
        let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if failures >= self.max_consecutive_failures {
            tracing::error!(
                "🚨 Circuit breaker OPEN: {} consecutive failures!",
                failures
            );
        }
    }

    /// Record success
    fn record_success(&self) {
        self.reset_circuit();
    }

    /// Place a single order with intelligent retry logic
    async fn place_order_with_retry(&self, order: &AssetOrder) -> Result<ExecutionResult> {
        // Check circuit breaker
        if self.is_circuit_open() {
            tracing::error!(
                "Circuit breaker OPEN - rejecting order for {}",
                order.symbol
            );
            return Ok(ExecutionResult::failed(
                order.symbol.clone(),
                "CIRCUIT-OPEN".to_string(),
                "Too many consecutive failures - circuit breaker open".to_string(),
            ));
        }

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
        let mut last_error_type = super::errors::BitgetErrorType::Unknown;

        for attempt in 1..=self.retry_attempts {
            match self.execute_single_order(order).await {
                Ok(result) => {
                    if result.is_success() {
                        if attempt > 1 {
                            tracing::info!(
                                "Order succeeded on attempt {}/{} for {}",
                                attempt,
                                self.retry_attempts,
                                order.symbol
                            );
                        }
                        self.record_success();
                        return Ok(result);
                    } else {
                        // Order failed but no exception - don't retry
                        tracing::warn!(
                            "Order failed (non-retryable): {} - {:?}",
                            order.symbol,
                            result.error_message
                        );
                        self.record_failure();
                        return Ok(result);
                    }
                }
                Err(e) => {
                    // Classify error
                    let error_msg = format!("{:?}", e);
                    let error_type = super::errors::BitgetErrorType::from_error_message(&error_msg);

                    last_error = Some(e);
                    last_error_type = error_type.clone();

                    // Check if retryable
                    if !error_type.is_retryable() {
                        tracing::error!(
                            "Non-retryable error for {}: {:?} (type: {:?})",
                            order.symbol,
                            last_error,
                            error_type
                        );
                        self.record_failure();
                        break;
                    }

                    // Retry with appropriate delay
                    if attempt < self.retry_attempts {
                        let backoff = std::time::Duration::from_millis(
                            error_type.retry_delay_ms(attempt)
                        );

                        tracing::warn!(
                            "Order attempt {}/{} failed for {} (type: {:?}), retrying in {:?}",
                            attempt,
                            self.retry_attempts,
                            order.symbol,
                            error_type,
                            backoff
                        );

                        tokio::time::sleep(backoff).await;
                    } else {
                        tracing::error!(
                            "Order failed after {}/{} attempts for {}: {:?}",
                            attempt,
                            self.retry_attempts,
                            order.symbol,
                            last_error
                        );
                    }
                }
            }
        }

        // All retries exhausted or non-retryable error
        self.record_failure();

        let error_msg = format!(
            "Failed (type: {:?}): {:?}",
            last_error_type,
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
                    return self.handle_partial_fill(detail, order).await;
                } else {
                    tracing::warn!(
                        "Order {} not filled, status: {}",
                        place_response.order_id,
                        detail.status
                    );
                }
            
                detail
            }
        };

        // Check if filled or partially filled
        if order_detail.is_partially_filled() {
            return self.handle_partial_fill(order_detail, order).await;
        }

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

    /// Handle partially filled orders
    async fn handle_partial_fill(
        &self,
        order_detail: super::types::OrderDetail,
        order: &AssetOrder,
    ) -> Result<ExecutionResult> {
        let filled_qty_f64: f64 = order_detail.base_volume.parse().unwrap_or(0.0);
        let requested_qty_f64 = order.quantity.to_u128_raw() as f64 / 1e18;
        let fill_percentage = (filled_qty_f64 / requested_qty_f64) * 100.0;

        tracing::warn!(
            "Order {} partially filled: {:.4}/{:.4} ({:.1}%)",
            order_detail.order_id,
            filled_qty_f64,
            requested_qty_f64,
            fill_percentage
        );

        // Decision: Accept partial fill or cancel and retry?
        // For now, we accept partial fills
        if fill_percentage >= 90.0 {
            tracing::info!(
                "Accepting partial fill (>90%): {} - {:.4}",
                order_detail.order_id,
                filled_qty_f64
            );
        } else {
            tracing::warn!(
                "Low fill rate (<90%): {} - {:.4} - Consider canceling",
                order_detail.order_id,
                filled_qty_f64
            );
            // TODO: Implement cancel logic if needed
        }

        self.order_detail_to_execution_result(order_detail)
    }

    /// Send orders with better error aggregation
    pub async fn send_orders_with_summary(
        &mut self,
        orders: Vec<AssetOrder>,
    ) -> Result<BatchExecutionSummary> {
        if !self.active {
            return Err(eyre::eyre!("Bitget order sender not started"));
        }

        tracing::info!(
            "📤 Sending {} order(s) to Bitget (trading: {})",
            orders.len(),
            self.trading_enabled
        );

        let mut results = Vec::new();
        let mut successful = 0;
        let mut failed = 0;
        let mut partial = 0;

        for order in orders {
            tracing::info!(
                "  → {} {} {} (type: {:?})",
                order.side,
                order.quantity,
                order.symbol,
                order.order_type
            );

            let result = self.place_order_with_retry(&order).await?;

            match result.status {
                super::super::types::OrderStatus::Filled => successful += 1,
                super::super::types::OrderStatus::PartiallyFilled => partial += 1,
                _ => failed += 1,
            }

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

        Ok(BatchExecutionSummary {
            total: results.len(),
            successful,
            failed,
            partial,
            results,
        })
    }

    /// Log order execution with context
    fn log_order_result(&self, order: &AssetOrder, result: &ExecutionResult, attempt: u8) {
        let qty_str = format!("{:.8}", order.quantity.to_u128_raw() as f64 / 1e18);
        let price_str = if result.avg_price > Amount::ZERO {
            format!("{:.2}", result.avg_price.to_u128_raw() as f64 / 1e18)
        } else {
            "N/A".to_string()
        };

        let fee_str = format!("{:.4}", result.fees.to_u128_raw() as f64 / 1e18);

        match result.status {
            OrderStatus::Filled => {
                tracing::info!(
                    order_id = %result.order_id,
                    symbol = %order.symbol,
                    side = %order.side,
                    quantity = %qty_str,
                    avg_price = %price_str,
                    fee = %fee_str,
                    attempt = attempt,
                    "Order filled successfully"
                );
            }
            OrderStatus::Failed | OrderStatus::Rejected => {
                tracing::error!(
                    order_id = %result.order_id,
                    symbol = %order.symbol,
                    error = ?result.error_message,
                    attempt = attempt,
                    "Order failed"
                );
            }
            OrderStatus::PartiallyFilled => {
                tracing::warn!(
                    order_id = %result.order_id,
                    symbol = %order.symbol,
                    filled = %qty_str,
                    attempt = attempt,
                    "Order partially filled"
                );
            }
            _ => {}
        }
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