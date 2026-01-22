use crate::order_sender::FeeTracker;
use crate::order_sender::rate_limiter::RateLimiter;
use crate::order_sender::adaptive_pricing::AdaptivePricingStrategy;
use crate::order_sender::refill_manager::RefillManager;
use crate::market_data::bitget::rest_client::OrderBookSnapshot;

use super::super::traits::OrderSender;
use super::super::types::{AssetOrder, ExecutionResult, ExecutionMode, OrderSide, OrderStatus, OrderType};
use super::client::BitgetClient;
use super::pricing::PricingStrategy;
use common::amount::Amount;
use eyre::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};


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
    ioc_timeout: Duration,            // Shorter timeout for IOC orders
    rate_limit_per_sec: usize,        // Rate limit for batch IOC execution
    consecutive_failures: Arc<AtomicU32>,
    max_consecutive_failures: u32,
    fee_tracker: Arc<parking_lot::RwLock<FeeTracker>>,
    // Story 3-8: Adaptive pricing and refill support
    adaptive_pricing: Option<AdaptivePricingStrategy>,
    refill_manager: Option<RefillManager>,
    execution_mode: ExecutionMode,
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
            ioc_timeout: Duration::from_secs(5),   // IOC orders should complete quickly
            rate_limit_per_sec: 10,                // Bitget's rate limit
            consecutive_failures: Arc::new(AtomicU32::new(0)),
            max_consecutive_failures: 5, // Circuit breaker threshold
            fee_tracker: Arc::new(parking_lot::RwLock::new(FeeTracker::new())),
            // Story 3-8: Default to static pricing for backward compatibility
            adaptive_pricing: None,
            refill_manager: None,
            execution_mode: ExecutionMode::StaticPricing,
        }
    }

    /// Create with custom IOC timeout and rate limit
    pub fn with_ioc_config(mut self, ioc_timeout_ms: u64, rate_limit_per_sec: usize) -> Self {
        self.ioc_timeout = Duration::from_millis(ioc_timeout_ms);
        self.rate_limit_per_sec = rate_limit_per_sec;
        self
    }

    /// Enable adaptive pricing strategy (Story 3-8).
    ///
    /// When enabled, order prices are computed based on orderbook depth
    /// instead of static spread.
    ///
    /// # Arguments
    /// * `strategy` - The adaptive pricing strategy to use
    pub fn with_adaptive_pricing(mut self, strategy: AdaptivePricingStrategy) -> Self {
        self.adaptive_pricing = Some(strategy);
        // Auto-enable adaptive mode when pricing strategy is set
        if self.execution_mode == ExecutionMode::StaticPricing {
            self.execution_mode = ExecutionMode::AdaptiveNoRefill;
        }
        self
    }

    /// Enable refill manager for partial fill handling (Story 3-8).
    ///
    /// When enabled, partial fills will be automatically retried with
    /// adjusted pricing.
    ///
    /// # Arguments
    /// * `manager` - The refill manager to use
    pub fn with_refill_manager(mut self, manager: RefillManager) -> Self {
        self.refill_manager = Some(manager);
        // Auto-enable full adaptive mode when refill is set
        if self.adaptive_pricing.is_some() {
            self.execution_mode = ExecutionMode::AdaptiveWithRefill;
        }
        self
    }

    /// Set execution mode directly (Story 3-8).
    ///
    /// # Arguments
    /// * `mode` - The execution mode to use
    pub fn with_execution_mode(mut self, mode: ExecutionMode) -> Self {
        self.execution_mode = mode;
        self
    }

    /// Get current execution mode
    pub fn execution_mode(&self) -> ExecutionMode {
        self.execution_mode
    }

    /// Check if adaptive pricing is available
    pub fn has_adaptive_pricing(&self) -> bool {
        self.adaptive_pricing.is_some()
    }

    /// Check if refill manager is available
    pub fn has_refill_manager(&self) -> bool {
        self.refill_manager.is_some()
    }

    /// Get fee tracker for reporting
    pub fn get_fee_tracker(&self) -> Arc<parking_lot::RwLock<FeeTracker>> {
        self.fee_tracker.clone()
    }

    /// Get fee summary
    pub fn fee_summary(&self) -> String {
        self.fee_tracker.read().summary()
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

    /// Execute an IOC (Immediate or Cancel) order.
    ///
    /// IOC orders differ from regular limit orders:
    /// - They execute immediately at limit price or better
    /// - Any unfilled portion is immediately cancelled
    /// - Can result in partial fills (filled some, cancelled rest)
    /// - Response time should be faster (no orderbook placement wait)
    ///
    /// # Arguments
    /// * `order` - The asset order to execute (must have limit_price set)
    ///
    /// # Returns
    /// ExecutionResult with status:
    /// - `Filled` - 100% of order filled
    /// - `PartiallyFilled` - Some filled, remainder cancelled
    /// - `Cancelled` - Nothing filled (price moved away)
    /// - `Failed` - API error
    pub async fn execute_ioc_order(&self, order: &AssetOrder) -> Result<ExecutionResult> {
        let start_time = Instant::now();

        // Get current best price for limit price calculation
        let best_price = match order.side {
            OrderSide::Buy => self.client.get_best_ask(&order.symbol).await?,
            OrderSide::Sell => self.client.get_best_bid(&order.symbol).await?,
        };

        // Use provided limit price or calculate from best price
        let limit_price = if let Some(custom_price) = order.limit_price {
            custom_price
        } else {
            self.pricing.calculate_limit_price(best_price, order.side)?
        };

        let client_order_id = format!("VW-IOC-{}", chrono::Utc::now().timestamp_millis());

        tracing::info!(
            "Executing IOC order: {} {} {} @ ${} (best: ${})",
            order.side,
            order.quantity,
            order.symbol,
            limit_price.to_u128_raw() as f64 / 1e18,
            best_price.to_u128_raw() as f64 / 1e18,
        );

        // Place IOC order
        let place_response = self
            .client
            .place_ioc_order(
                &order.symbol,
                order.side,
                order.quantity,
                limit_price,
                Some(client_order_id.clone()),
            )
            .await
            .context("Failed to place IOC order")?;

        tracing::info!(
            "IOC order placed: {} (client: {}) in {:?}",
            place_response.order_id,
            client_order_id,
            start_time.elapsed()
        );

        // IOC orders execute immediately - short wait then check status.
        // Unlike GTC orders, IOC either fills immediately or cancels.
        //
        // The 500ms delay is based on Bitget API behavior:
        // - Order placement returns immediately with order_id
        // - Order status update takes ~100-300ms to propagate
        // - 500ms provides margin for network latency and API processing
        // - Shorter delays risk getting stale "new" status; longer delays waste time
        const IOC_STATUS_POLL_DELAY_MS: u64 = 500;
        tokio::time::sleep(Duration::from_millis(IOC_STATUS_POLL_DELAY_MS)).await;

        // Get order status
        let order_detail = self
            .client
            .get_order(&place_response.order_id)
            .await
            .context("Failed to get IOC order status")?;

        // Log IOC-specific metrics
        let elapsed = start_time.elapsed();
        if elapsed > self.ioc_timeout {
            tracing::warn!(
                "IOC order {} took {:?} (exceeds {:?} timeout)",
                place_response.order_id,
                elapsed,
                self.ioc_timeout
            );
        }

        // Convert to execution result with IOC-specific handling
        self.ioc_order_detail_to_execution_result(order_detail, order)
    }

    /// Execute an IOC order with adaptive pricing (Story 3-8).
    ///
    /// Uses orderbook depth to compute optimal limit price instead of static spread.
    ///
    /// # Arguments
    /// * `order` - The asset order to execute
    /// * `snapshot` - Current orderbook snapshot for the asset
    ///
    /// # Returns
    /// ExecutionResult with adaptive pricing metrics
    pub async fn execute_ioc_order_adaptive(
        &self,
        order: &AssetOrder,
        snapshot: &OrderBookSnapshot,
    ) -> Result<ExecutionResult> {
        let start_time = Instant::now();

        // Get adaptive pricing strategy (required for this method)
        let adaptive_pricing = self.adaptive_pricing.as_ref()
            .ok_or_else(|| eyre::eyre!("Adaptive pricing not configured"))?;

        // Compute adaptive limit price from orderbook
        let adaptive_price = adaptive_pricing.compute_limit_price(snapshot, order.side, order.quantity)?;

        // Estimate fill probability
        let estimated_fill_prob = adaptive_pricing.estimate_fill_probability(
            snapshot,
            adaptive_price,
            order.side,
            order.quantity,
        );

        // For comparison, compute what static pricing would have used
        let static_price = if let Some(best) = match order.side {
            OrderSide::Buy => snapshot.best_ask(),
            OrderSide::Sell => snapshot.best_bid(),
        } {
            self.pricing.calculate_limit_price(best.price, order.side).ok()
        } else {
            None
        };

        // Log price comparison
        let price_improvement = static_price.map(|static_p| {
            adaptive_pricing.calculate_price_improvement(adaptive_price, static_p, order.side)
        });

        tracing::info!(
            "Adaptive IOC {} {} {}: adaptive=${:.4}, est_fill={:.0}%, improvement={}bps",
            order.side,
            order.quantity,
            order.symbol,
            adaptive_price.to_u128_raw() as f64 / 1e18,
            estimated_fill_prob * 100.0,
            price_improvement.unwrap_or(0)
        );

        // Create order with adaptive price
        let adaptive_order = AssetOrder {
            symbol: order.symbol.clone(),
            side: order.side,
            quantity: order.quantity,
            order_type: OrderType::Limit,
            limit_price: Some(adaptive_price),
        };

        // Execute the IOC order
        let result = self.execute_ioc_order(&adaptive_order).await?;

        // Log actual vs estimated fill
        let actual_fill_pct = if order.quantity.to_u128_raw() > 0 {
            (result.filled_quantity.to_u128_raw() as f64 / order.quantity.to_u128_raw() as f64) * 100.0
        } else {
            0.0
        };

        let elapsed = start_time.elapsed();
        tracing::info!(
            "Adaptive IOC result: {:.1}% filled (est: {:.0}%), improvement: {}bps, time: {:?}",
            actual_fill_pct,
            estimated_fill_prob * 100.0,
            price_improvement.unwrap_or(0),
            elapsed
        );

        Ok(result)
    }

    /// Execute an IOC order with adaptive pricing and refill logic (Story 3-8).
    ///
    /// Combines adaptive pricing with automatic retry for partial fills.
    ///
    /// # Arguments
    /// * `order` - The asset order to execute
    /// * `snapshot` - Current orderbook snapshot (will be re-fetched for refills)
    /// * `refetch_orderbook` - Async function to re-fetch orderbook for refill attempts
    ///
    /// # Returns
    /// ExecutionResult with aggregated fills across all attempts
    pub async fn execute_ioc_order_with_refill<F, Fut>(
        &self,
        order: &AssetOrder,
        snapshot: &OrderBookSnapshot,
        refetch_orderbook: F,
    ) -> Result<ExecutionResult>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<OrderBookSnapshot>>,
    {
        let start_time = Instant::now();

        // Get refill manager (required for this method)
        let refill_manager = self.refill_manager.as_ref()
            .ok_or_else(|| eyre::eyre!("Refill manager not configured"))?;

        let mut all_results: Vec<ExecutionResult> = Vec::new();
        let mut current_order = order.clone();
        let mut current_snapshot = snapshot.clone();
        let mut attempt = 0;

        loop {
            // Execute with adaptive pricing
            let result = self.execute_ioc_order_adaptive(&current_order, &current_snapshot).await?;
            let is_success = result.is_success();
            all_results.push(result.clone());

            // Check if we should refill
            if !refill_manager.should_refill(&result, current_order.quantity, attempt) {
                break;
            }

            // Increment attempt counter
            attempt += 1;

            // Wait before refill
            tokio::time::sleep(refill_manager.refill_delay()).await;

            // Re-fetch orderbook for updated pricing
            match refetch_orderbook().await {
                Ok(new_snapshot) => {
                    current_snapshot = new_snapshot;
                }
                Err(e) => {
                    tracing::warn!("Failed to refetch orderbook for refill: {:?}", e);
                    // Fall back to static pricing for refill
                    break;
                }
            }

            // Compute refill order
            current_order = refill_manager.compute_refill_order(&current_order, &result, attempt - 1);

            tracing::info!(
                "🔄 Refill attempt {}/{}: {} remaining {:.6}",
                attempt,
                refill_manager.max_retries(),
                order.symbol,
                current_order.quantity.to_u128_raw() as f64 / 1e18
            );
        }

        // Aggregate all results
        let (total_filled, total_fees, avg_price) = refill_manager.aggregate_fills(&all_results);

        let final_status = if total_filled >= order.quantity {
            OrderStatus::Filled
        } else if total_filled.to_u128_raw() > 0 {
            OrderStatus::PartiallyFilled
        } else {
            OrderStatus::Cancelled
        };

        let elapsed = start_time.elapsed();
        let fill_pct = (total_filled.to_u128_raw() as f64 / order.quantity.to_u128_raw() as f64) * 100.0;

        tracing::info!(
            "🎯 Refill complete: {:.1}% filled across {} attempt(s) in {:?}",
            fill_pct,
            all_results.len(),
            elapsed
        );

        // Create aggregated result
        Ok(ExecutionResult {
            symbol: order.symbol.clone(),
            order_id: all_results.last()
                .map(|r| r.order_id.clone())
                .unwrap_or_else(|| "AGGREGATE".to_string()),
            filled_quantity: total_filled,
            avg_price,
            fees: total_fees,
            fee_detail: all_results.last().and_then(|r| r.fee_detail.clone()),
            status: final_status,
            error_message: None,
        })
    }

    /// Convert IOC order detail to execution result with IOC-specific status handling.
    ///
    /// IOC orders have different status semantics than GTC orders:
    /// - "full_fill" -> 100% filled
    /// - "partial_fill" + cancelled -> Some filled, rest cancelled (successful partial)
    /// - "cancelled" with filled > 0 -> Partial fill
    /// - "cancelled" with filled == 0 -> Nothing filled (price moved)
    fn ioc_order_detail_to_execution_result(
        &self,
        detail: super::types::OrderDetail,
        original_order: &AssetOrder,
    ) -> Result<ExecutionResult> {
        let filled_qty_f64: f64 = detail.base_volume.parse().unwrap_or(0.0);
        let avg_price_f64: f64 = detail.avg_price.parse().unwrap_or(0.0);
        let requested_qty_f64 = original_order.quantity.to_u128_raw() as f64 / 1e18;

        let filled_quantity = Amount::from_u128_raw((filled_qty_f64 * 1e18) as u128);
        let avg_price = Amount::from_u128_raw((avg_price_f64 * 1e18) as u128);

        // Parse fee from feeDetail JSON string if present
        // Format: {"BTC":{"totalFee":-6E-8,"feeCoinCode":"BTC",...}}
        let (fee_f64, fee_currency) = if !detail.fee_detail.is_empty() && detail.fee_detail != "{}" {
            // Try to parse the feeDetail JSON
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&detail.fee_detail) {
                // Get the first coin's fee info (usually baseCoin like BTC)
                if let Some(obj) = json.as_object() {
                    if let Some((coin, coin_info)) = obj.iter().find(|(k, _)| *k != "newFees") {
                        let total_fee = coin_info.get("totalFee")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0)
                            .abs(); // Fees are negative in API response
                        let fee_coin = coin_info.get("feeCoinCode")
                            .and_then(|v| v.as_str())
                            .unwrap_or(coin)
                            .to_string();
                        (total_fee, fee_coin)
                    } else {
                        (0.0, detail.base_coin.clone())
                    }
                } else {
                    (0.0, detail.base_coin.clone())
                }
            } else {
                tracing::warn!("Failed to parse feeDetail JSON: {}", detail.fee_detail);
                (0.0, detail.base_coin.clone())
            }
        } else {
            // Fallback to simple fee field
            let fee: f64 = detail.fee.parse::<f64>().unwrap_or(0.0).abs();
            (fee, detail.fee_currency.clone())
        };

        let fees = Amount::from_u128_raw((fee_f64 * 1e18) as u128);

        // Calculate fill percentage for logging
        let fill_percentage = if requested_qty_f64 > 0.0 {
            (filled_qty_f64 / requested_qty_f64) * 100.0
        } else {
            0.0
        };

        // Extract fee detail
        let fee_detail = if fee_f64 > 0.0 {
            // IOC orders are always takers (they execute against resting orders)
            tracing::debug!(
                "IOC order fee: {:.8} {} (parsed from feeDetail)",
                fee_f64,
                fee_currency
            );
            Some(super::super::types::FeeDetail::new(
                fees,
                fee_currency,
                super::super::types::FeeType::Taker,
            ))
        } else {
            None
        };

        // Record fee in tracker
        if let Some(ref fee_detail) = fee_detail {
            self.fee_tracker.write().record_fee(fees, Some(fee_detail));
        }

        // Determine IOC-specific status
        // Bitget API status values: "init", "new", "partial_fill", "full_fill", "cancelled"
        // Some API versions may use "filled" instead of "full_fill"
        tracing::debug!(
            "IOC order {} raw status: '{}', filled: {:.6}",
            detail.order_id,
            detail.status,
            filled_qty_f64
        );

        let status = match detail.status.as_str() {
            "full_fill" | "filled" => {
                tracing::info!(
                    "IOC order {} fully filled: {:.6} @ ${:.4}",
                    detail.order_id,
                    filled_qty_f64,
                    avg_price_f64
                );
                OrderStatus::Filled
            }
            "partial_fill" | "cancelled" => {
                // For IOC, check if anything was filled
                if filled_qty_f64 > 0.0 {
                    tracing::info!(
                        "IOC order {} partially filled: {:.6}/{:.6} ({:.1}%) @ ${:.4}",
                        detail.order_id,
                        filled_qty_f64,
                        requested_qty_f64,
                        fill_percentage,
                        avg_price_f64
                    );
                    OrderStatus::PartiallyFilled
                } else {
                    tracing::warn!(
                        "IOC order {} cancelled with no fill (price likely moved)",
                        detail.order_id
                    );
                    OrderStatus::Cancelled
                }
            }
            // If status is unknown but we have a fill, treat as filled
            _ if filled_qty_f64 > 0.0 && fill_percentage >= 99.0 => {
                tracing::warn!(
                    "IOC order {} unknown status '{}' but 100% filled - treating as Filled",
                    detail.order_id,
                    detail.status
                );
                OrderStatus::Filled
            }
            _ if filled_qty_f64 > 0.0 => {
                tracing::warn!(
                    "IOC order {} unknown status '{}' but partially filled - treating as PartiallyFilled",
                    detail.order_id,
                    detail.status
                );
                OrderStatus::PartiallyFilled
            }
            _ => {
                tracing::error!(
                    "IOC order {} unexpected status: '{}'",
                    detail.order_id,
                    detail.status
                );
                OrderStatus::Failed
            }
        };

        let error_message = if status == OrderStatus::Cancelled && filled_qty_f64 == 0.0 {
            Some("IOC order cancelled: no immediate fill available at limit price".to_string())
        } else if status == OrderStatus::Failed {
            Some(format!("Unexpected IOC status: {}", detail.status))
        } else {
            None
        };

        Ok(ExecutionResult {
            symbol: detail.symbol,
            order_id: detail.order_id,
            filled_quantity,
            avg_price,
            fees,
            fee_detail,
            status,
            error_message,
        })
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

        // Extract fee detail
        let fee_detail = if fee_f64 > 0.0 {
            // Determine if maker or taker based on order type
            // Bitget doesn't always provide this, so we infer:
            // - Limit orders that sit in book = Maker
            // - Market orders = Taker
            // - Limit orders that execute immediately = Taker
            let fee_type = if detail.order_type == "market" {
                super::super::types::FeeType::Taker
            } else if detail.status == "full_fill" && detail.order_type == "limit" {
                // If filled immediately, likely taker
                super::super::types::FeeType::Taker
            } else {
                // Otherwise maker
                super::super::types::FeeType::Maker
            };

            Some(super::super::types::FeeDetail::new(
                fees,
                detail.fee_currency.clone(),
                fee_type,
            ))
        } else {
            None
        };

        // Record fee in tracker
        if let Some(ref fee_detail) = fee_detail {
            self.fee_tracker.write().record_fee(fees, Some(fee_detail));
        }

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
            fee_detail,
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

    /// Send orders with better error aggregation (NO rate limiting).
    ///
    /// # When to Use
    /// Use this method for:
    /// - GTC (Good Till Cancelled) limit orders that don't need rate limiting
    /// - Small batches where rate limiting overhead is unnecessary
    /// - Testing/debugging scenarios
    ///
    /// # When NOT to Use
    /// For IOC orders or large batches, use `send_ioc_orders_rate_limited()` instead,
    /// which enforces Bitget's 10 orders/second rate limit.
    ///
    /// # Warning
    /// This method does NOT enforce rate limiting. Sending too many orders too quickly
    /// may result in Bitget API throttling (HTTP 429 errors).
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
                    "  ✓ Filled: {} - {} @ ${} (fee: ${} - {})",
                    result.order_id,
                    result.filled_quantity,
                    result.avg_price.to_u128_raw() as f64 / 1e18,
                    result.fees.to_u128_raw() as f64 / 1e18,
                    result.fee_detail
                        .as_ref()
                        .map(|f| f.fee_type.to_string())
                        .unwrap_or_else(|| "Unknown".to_string())
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

    /// Send IOC orders with rate limiting.
    ///
    /// Executes multiple IOC orders while respecting Bitget's 10 orders/second rate limit.
    /// Uses a sliding window rate limiter to prevent API throttling.
    ///
    /// # Arguments
    /// * `orders` - Vector of orders to execute as IOC
    ///
    /// # Returns
    /// BatchExecutionSummary with results for all orders
    ///
    /// # Rate Limiting
    /// - Default: 10 orders per second (Bitget's limit)
    /// - Uses sliding window approach
    /// - Will sleep if at capacity
    ///
    /// # Performance
    /// - NFR21: Order confirmation within 10 seconds
    /// - Warns if any order exceeds 10 second threshold
    pub async fn send_ioc_orders_rate_limited(
        &mut self,
        orders: Vec<AssetOrder>,
    ) -> Result<BatchExecutionSummary> {
        let batch_start = Instant::now();

        if !self.active {
            return Err(eyre::eyre!("Bitget order sender not started"));
        }

        if orders.is_empty() {
            tracing::warn!("send_ioc_orders_rate_limited called with empty order list");
            return Ok(BatchExecutionSummary {
                total: 0,
                successful: 0,
                failed: 0,
                partial: 0,
                results: Vec::new(),
            });
        }

        // Check circuit breaker
        if self.is_circuit_open() {
            tracing::error!(
                "Circuit breaker OPEN - rejecting batch of {} IOC orders",
                orders.len()
            );
            let results: Vec<ExecutionResult> = orders
                .iter()
                .map(|o| {
                    ExecutionResult::failed(
                        o.symbol.clone(),
                        "CIRCUIT-OPEN".to_string(),
                        "Circuit breaker open - too many failures".to_string(),
                    )
                })
                .collect();
            return Ok(BatchExecutionSummary {
                total: results.len(),
                successful: 0,
                failed: results.len(),
                partial: 0,
                results,
            });
        }

        if !self.trading_enabled {
            tracing::warn!(
                "Trading disabled (dry-run) - {} IOC orders not executed",
                orders.len()
            );
            let results: Vec<ExecutionResult> = orders
                .iter()
                .map(|o| {
                    ExecutionResult::failed(
                        o.symbol.clone(),
                        "DRY-RUN".to_string(),
                        "Trading disabled (dry-run mode)".to_string(),
                    )
                })
                .collect();
            return Ok(BatchExecutionSummary {
                total: results.len(),
                successful: 0,
                failed: results.len(),
                partial: 0,
                results,
            });
        }

        tracing::info!(
            "📤 Sending {} IOC order(s) with rate limit {}/sec",
            orders.len(),
            self.rate_limit_per_sec
        );

        let mut rate_limiter = RateLimiter::new(self.rate_limit_per_sec);
        let mut results = Vec::with_capacity(orders.len());
        let mut successful = 0;
        let mut failed = 0;
        let mut partial = 0;
        let mut slowest_order_time = Duration::ZERO;

        for (i, order) in orders.iter().enumerate() {
            let order_start = Instant::now();

            // Wait for rate limit slot
            rate_limiter.wait_for_slot().await;

            tracing::info!(
                "  [{}/{}] IOC {} {} {} (limit: {:?})",
                i + 1,
                orders.len(),
                order.side,
                order.quantity,
                order.symbol,
                order.limit_price
            );

            // Execute IOC order
            let result = match self.execute_ioc_order(order).await {
                Ok(result) => result,
                Err(e) => {
                    tracing::error!("IOC order execution error for {}: {:?}", order.symbol, e);
                    self.record_failure();
                    ExecutionResult::failed(
                        order.symbol.clone(),
                        "IOC-ERROR".to_string(),
                        format!("{:?}", e),
                    )
                }
            };

            // Record the operation for rate limiting
            rate_limiter.record_operation();

            // Track order execution time
            let order_elapsed = order_start.elapsed();
            if order_elapsed > slowest_order_time {
                slowest_order_time = order_elapsed;
            }

            // Check NFR21: Order confirmation within timeout threshold
            // Uses 2x ioc_timeout as the NFR21 threshold (default: 10s when ioc_timeout=5s)
            let nfr21_threshold = self.ioc_timeout * 2;
            if order_elapsed > nfr21_threshold {
                tracing::warn!(
                    "⚠️ IOC order {} exceeded NFR21 threshold ({:?}): {:?}",
                    result.order_id,
                    nfr21_threshold,
                    order_elapsed
                );
            }

            // Update counters
            match result.status {
                OrderStatus::Filled => {
                    successful += 1;
                    self.record_success();
                    tracing::info!(
                        "  ✓ [{}/{}] Filled: {} - {:.6} @ ${:.4} (fee: ${:.6}) in {:?}",
                        i + 1,
                        orders.len(),
                        result.order_id,
                        result.filled_quantity.to_u128_raw() as f64 / 1e18,
                        result.avg_price.to_u128_raw() as f64 / 1e18,
                        result.fees.to_u128_raw() as f64 / 1e18,
                        order_elapsed
                    );
                }
                OrderStatus::PartiallyFilled => {
                    partial += 1;
                    self.record_success(); // Partial is still success
                    tracing::info!(
                        "  ◐ [{}/{}] Partial: {} - {:.6} @ ${:.4} in {:?}",
                        i + 1,
                        orders.len(),
                        result.order_id,
                        result.filled_quantity.to_u128_raw() as f64 / 1e18,
                        result.avg_price.to_u128_raw() as f64 / 1e18,
                        order_elapsed
                    );
                }
                _ => {
                    failed += 1;
                    self.record_failure();
                    tracing::warn!(
                        "  ✗ [{}/{}] Failed: {} - {:?} in {:?}",
                        i + 1,
                        orders.len(),
                        result.order_id,
                        result.error_message.as_ref().unwrap_or(&"Unknown".to_string()),
                        order_elapsed
                    );
                }
            }

            results.push(result);
        }

        let batch_elapsed = batch_start.elapsed();

        // Log batch summary
        tracing::info!(
            "📊 IOC Batch Summary: {}/{} successful, {} partial, {} failed in {:?} (slowest: {:?})",
            successful,
            results.len(),
            partial,
            failed,
            batch_elapsed,
            slowest_order_time
        );

        if failed > 0 && successful == 0 {
            tracing::error!(
                "⚠️ All {} IOC orders failed! Check circuit breaker status.",
                failed
            );
        }

        Ok(BatchExecutionSummary {
            total: results.len(),
            successful,
            failed,
            partial,
            results,
        })
    }

    /// Send index orders with adaptive pricing (Story 3-8).
    ///
    /// This is the main entry point for executing orders against a multi-asset index
    /// with intelligent pricing based on orderbook depth.
    ///
    /// # Arguments
    /// * `orders` - Vector of orders to execute
    /// * `orderbooks` - Pre-fetched orderbook snapshots keyed by symbol
    ///
    /// # Returns
    /// BatchExecutionSummary with results for all orders
    ///
    /// # Features
    /// - Adaptive pricing using orderbook depth (when configured)
    /// - Falls back to static pricing when orderbook unavailable
    /// - Rate limiting to respect Bitget API limits
    ///
    /// # Limitations
    /// **Refill logic is NOT applied in batch mode.** While `ExecutionMode::AdaptiveWithRefill`
    /// is respected for adaptive pricing, automatic refill for partial fills requires
    /// orderbook re-fetching which is not practical for batch operations.
    ///
    /// For orders requiring refill logic, use `execute_ioc_order_with_refill()` for
    /// single-order execution with a custom orderbook refetch function.
    pub async fn send_index_orders_adaptive(
        &mut self,
        orders: Vec<AssetOrder>,
        orderbooks: &HashMap<String, OrderBookSnapshot>,
    ) -> Result<BatchExecutionSummary> {
        let batch_start = Instant::now();

        if !self.active {
            return Err(eyre::eyre!("Bitget order sender not started"));
        }

        if orders.is_empty() {
            tracing::warn!("send_index_orders_adaptive called with empty order list");
            return Ok(BatchExecutionSummary {
                total: 0,
                successful: 0,
                failed: 0,
                partial: 0,
                results: Vec::new(),
            });
        }

        // Check circuit breaker
        if self.is_circuit_open() {
            tracing::error!(
                "Circuit breaker OPEN - rejecting batch of {} adaptive orders",
                orders.len()
            );
            let results: Vec<ExecutionResult> = orders
                .iter()
                .map(|o| {
                    ExecutionResult::failed(
                        o.symbol.clone(),
                        "CIRCUIT-OPEN".to_string(),
                        "Circuit breaker open - too many failures".to_string(),
                    )
                })
                .collect();
            return Ok(BatchExecutionSummary {
                total: results.len(),
                successful: 0,
                failed: results.len(),
                partial: 0,
                results,
            });
        }

        if !self.trading_enabled {
            tracing::warn!(
                "Trading disabled (dry-run) - {} adaptive orders not executed",
                orders.len()
            );
            let results: Vec<ExecutionResult> = orders
                .iter()
                .map(|o| {
                    ExecutionResult::failed(
                        o.symbol.clone(),
                        "DRY-RUN".to_string(),
                        "Trading disabled (dry-run mode)".to_string(),
                    )
                })
                .collect();
            return Ok(BatchExecutionSummary {
                total: results.len(),
                successful: 0,
                failed: results.len(),
                partial: 0,
                results,
            });
        }

        let total_orders = orders.len();
        let orderbook_count = orderbooks.len();
        let has_adaptive = self.adaptive_pricing.is_some();
        let has_refill = self.refill_manager.is_some();

        tracing::info!(
            "📤 Sending {} adaptive orders (mode: {}, orderbooks: {}/{})",
            total_orders,
            self.execution_mode,
            orderbook_count,
            total_orders
        );

        let mut rate_limiter = RateLimiter::new(self.rate_limit_per_sec);
        let mut results = Vec::with_capacity(total_orders);
        let mut successful = 0;
        let mut failed = 0;
        let mut partial = 0;
        let mut adaptive_count = 0;
        let mut fallback_count = 0;

        for (i, order) in orders.iter().enumerate() {
            let order_start = Instant::now();

            // Wait for rate limit slot
            rate_limiter.wait_for_slot().await;

            // Try to get orderbook for this symbol
            let snapshot = orderbooks.get(&order.symbol.to_uppercase());

            let result = match (snapshot, has_adaptive, &self.execution_mode) {
                // Adaptive pricing available and orderbook found
                (Some(ob), true, ExecutionMode::AdaptiveWithRefill) if has_refill => {
                    adaptive_count += 1;
                    // With refill - but we don't have a refetch function here
                    // So we use adaptive without refill for batch execution
                    // Refill with re-fetch is better suited for single-order execution
                    match self.execute_ioc_order_adaptive(&order, ob).await {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::error!("Adaptive IOC error for {}: {:?}", order.symbol, e);
                            self.record_failure();
                            ExecutionResult::failed(
                                order.symbol.clone(),
                                "ADAPTIVE-ERROR".to_string(),
                                format!("{:?}", e),
                            )
                        }
                    }
                }
                (Some(ob), true, ExecutionMode::AdaptiveNoRefill) => {
                    adaptive_count += 1;
                    match self.execute_ioc_order_adaptive(&order, ob).await {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::error!("Adaptive IOC error for {}: {:?}", order.symbol, e);
                            self.record_failure();
                            ExecutionResult::failed(
                                order.symbol.clone(),
                                "ADAPTIVE-ERROR".to_string(),
                                format!("{:?}", e),
                            )
                        }
                    }
                }
                (Some(ob), true, ExecutionMode::AdaptiveWithRefill) => {
                    // Has adaptive but no refill manager - use adaptive without refill
                    adaptive_count += 1;
                    match self.execute_ioc_order_adaptive(&order, ob).await {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::error!("Adaptive IOC error for {}: {:?}", order.symbol, e);
                            self.record_failure();
                            ExecutionResult::failed(
                                order.symbol.clone(),
                                "ADAPTIVE-ERROR".to_string(),
                                format!("{:?}", e),
                            )
                        }
                    }
                }
                // Fall back to static pricing
                _ => {
                    fallback_count += 1;
                    if snapshot.is_none() {
                        tracing::warn!(
                            "No orderbook for {} - using static pricing",
                            order.symbol
                        );
                    }
                    match self.execute_ioc_order(&order).await {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::error!("IOC error for {}: {:?}", order.symbol, e);
                            self.record_failure();
                            ExecutionResult::failed(
                                order.symbol.clone(),
                                "IOC-ERROR".to_string(),
                                format!("{:?}", e),
                            )
                        }
                    }
                }
            };

            // Record the operation for rate limiting
            rate_limiter.record_operation();

            let order_elapsed = order_start.elapsed();

            // Update counters
            match result.status {
                OrderStatus::Filled => {
                    successful += 1;
                    self.record_success();
                    tracing::debug!(
                        "  ✓ [{}/{}] {} filled {:.6} @ ${:.4} in {:?}",
                        i + 1,
                        total_orders,
                        order.symbol,
                        result.filled_quantity.to_u128_raw() as f64 / 1e18,
                        result.avg_price.to_u128_raw() as f64 / 1e18,
                        order_elapsed
                    );
                }
                OrderStatus::PartiallyFilled => {
                    partial += 1;
                    self.record_success();
                    let fill_pct = (result.filled_quantity.to_u128_raw() as f64
                        / order.quantity.to_u128_raw() as f64) * 100.0;
                    tracing::debug!(
                        "  ◐ [{}/{}] {} partial {:.1}% in {:?}",
                        i + 1,
                        total_orders,
                        order.symbol,
                        fill_pct,
                        order_elapsed
                    );
                }
                _ => {
                    failed += 1;
                    self.record_failure();
                    tracing::warn!(
                        "  ✗ [{}/{}] {} failed: {:?}",
                        i + 1,
                        total_orders,
                        order.symbol,
                        result.error_message.as_ref().unwrap_or(&"Unknown".to_string())
                    );
                }
            }

            results.push(result);
        }

        let batch_elapsed = batch_start.elapsed();

        // Log batch summary
        tracing::info!(
            "📊 Adaptive Batch Summary: {}/{} successful, {} partial, {} failed in {:?}",
            successful,
            total_orders,
            partial,
            failed,
            batch_elapsed
        );
        tracing::info!(
            "   Pricing: {} adaptive, {} fallback to static",
            adaptive_count,
            fallback_count
        );

        if failed > 0 && successful == 0 {
            tracing::error!(
                "⚠️ All {} orders failed! Check circuit breaker status.",
                failed
            );
        }

        Ok(BatchExecutionSummary {
            total: results.len(),
            successful,
            failed,
            partial,
            results,
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
                    "  ✓ Filled: {} - {} @ ${} (fee: ${} - {})",
                    result.order_id,
                    result.filled_quantity,
                    result.avg_price.to_u128_raw() as f64 / 1e18,
                    result.fees.to_u128_raw() as f64 / 1e18,
                    result.fee_detail
                        .as_ref()
                        .map(|f| f.fee_type.to_string())
                        .unwrap_or_else(|| "Unknown".to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_batch_execution_summary_success_rate() {
        let summary = BatchExecutionSummary {
            total: 10,
            successful: 8,
            failed: 1,
            partial: 1,
            results: Vec::new(),
        };

        assert!((summary.success_rate() - 80.0).abs() < 0.01);
    }

    #[test]
    fn test_batch_execution_summary_empty() {
        let summary = BatchExecutionSummary {
            total: 0,
            successful: 0,
            failed: 0,
            partial: 0,
            results: Vec::new(),
        };

        assert!((summary.success_rate() - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_batch_execution_summary_has_failures() {
        let summary_with_failures = BatchExecutionSummary {
            total: 5,
            successful: 3,
            failed: 2,
            partial: 0,
            results: Vec::new(),
        };
        assert!(summary_with_failures.has_failures());

        let summary_without_failures = BatchExecutionSummary {
            total: 5,
            successful: 4,
            failed: 0,
            partial: 1,
            results: Vec::new(),
        };
        assert!(!summary_without_failures.has_failures());
    }

    #[test]
    fn test_bitget_order_sender_new_defaults() {
        let sender = BitgetOrderSender::new(
            "key".to_string(),
            "secret".to_string(),
            "pass".to_string(),
            5,  // price_limit_bps
            3,  // retry_attempts
            false, // trading_enabled
        );

        assert!(!sender.is_active());
        assert_eq!(sender.ioc_timeout, Duration::from_secs(5));
        assert_eq!(sender.rate_limit_per_sec, 10);
    }

    #[test]
    fn test_bitget_order_sender_with_ioc_config() {
        let sender = BitgetOrderSender::new(
            "key".to_string(),
            "secret".to_string(),
            "pass".to_string(),
            5,
            3,
            false,
        )
        .with_ioc_config(3000, 15);

        assert_eq!(sender.ioc_timeout, Duration::from_millis(3000));
        assert_eq!(sender.rate_limit_per_sec, 15);
    }

    #[test]
    fn test_circuit_breaker_starts_closed() {
        let sender = BitgetOrderSender::new(
            "key".to_string(),
            "secret".to_string(),
            "pass".to_string(),
            5,
            3,
            false,
        );

        assert!(!sender.is_circuit_open());
    }

    #[test]
    fn test_circuit_breaker_opens_after_failures() {
        let sender = BitgetOrderSender::new(
            "key".to_string(),
            "secret".to_string(),
            "pass".to_string(),
            5,
            3,
            false,
        );

        // Record enough failures to trip the circuit breaker (5 by default)
        for _ in 0..5 {
            sender.record_failure();
        }

        assert!(sender.is_circuit_open());
    }

    #[test]
    fn test_circuit_breaker_resets_on_success() {
        let sender = BitgetOrderSender::new(
            "key".to_string(),
            "secret".to_string(),
            "pass".to_string(),
            5,
            3,
            false,
        );

        // Record some failures
        for _ in 0..4 {
            sender.record_failure();
        }
        assert!(!sender.is_circuit_open());

        // Record success - should reset
        sender.record_success();
        assert!(!sender.is_circuit_open());

        // Verify counter was reset
        for _ in 0..4 {
            sender.record_failure();
        }
        assert!(!sender.is_circuit_open()); // Still not open, only 4 failures
    }

    #[test]
    fn test_adaptive_pricing_configuration() {
        use crate::order_sender::adaptive_pricing::AdaptivePricingStrategy;

        let sender = BitgetOrderSender::new(
            "key".to_string(),
            "secret".to_string(),
            "pass".to_string(),
            5,
            3,
            false,
        );

        // Verify default is StaticPricing (not AdaptiveWithRefill)
        assert_eq!(sender.execution_mode(), ExecutionMode::StaticPricing);
        assert!(!sender.has_adaptive_pricing());
        assert!(!sender.has_refill_manager());

        // Configure adaptive pricing
        let adaptive_strategy = AdaptivePricingStrategy::default()
            .with_base_spread_bps(10)
            .with_slippage_tolerance_bps(100);

        let sender_with_adaptive = sender.with_adaptive_pricing(adaptive_strategy);

        assert!(sender_with_adaptive.has_adaptive_pricing());
        assert_eq!(sender_with_adaptive.execution_mode(), ExecutionMode::AdaptiveNoRefill);
    }

    #[test]
    fn test_refill_manager_configuration() {
        use crate::order_sender::adaptive_pricing::AdaptivePricingStrategy;
        use crate::order_sender::refill_manager::RefillManager;

        let sender = BitgetOrderSender::new(
            "key".to_string(),
            "secret".to_string(),
            "pass".to_string(),
            5,
            3,
            false,
        );

        // Configure both adaptive pricing and refill manager
        let adaptive_strategy = AdaptivePricingStrategy::default();
        let refill_manager = RefillManager::default()
            .with_max_retries(3)
            .with_price_adjustment_bps(15);

        let sender_configured = sender
            .with_adaptive_pricing(adaptive_strategy)
            .with_refill_manager(refill_manager);

        assert!(sender_configured.has_adaptive_pricing());
        assert!(sender_configured.has_refill_manager());
        assert_eq!(sender_configured.execution_mode(), ExecutionMode::AdaptiveWithRefill);
    }

    #[test]
    fn test_execution_mode_methods() {
        // Test ExecutionMode helper methods
        assert!(ExecutionMode::AdaptiveWithRefill.is_adaptive());
        assert!(ExecutionMode::AdaptiveWithRefill.has_refill());

        assert!(ExecutionMode::AdaptiveNoRefill.is_adaptive());
        assert!(!ExecutionMode::AdaptiveNoRefill.has_refill());

        assert!(!ExecutionMode::StaticPricing.is_adaptive());
        assert!(!ExecutionMode::StaticPricing.has_refill());
    }

    #[test]
    fn test_execution_mode_display() {
        assert_eq!(format!("{}", ExecutionMode::AdaptiveWithRefill), "Adaptive+Refill");
        assert_eq!(format!("{}", ExecutionMode::AdaptiveNoRefill), "Adaptive");
        assert_eq!(format!("{}", ExecutionMode::StaticPricing), "Static");
    }

    // Integration test with #[ignore] - requires live Bitget connection
    // Run with: BITGET_TRADING_ENABLED=1 cargo test --test vendor_integration test_ioc -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "Requires live Bitget API credentials and BITGET_TRADING_ENABLED=1"]
    async fn test_ioc_order_execution_live() {
        use super::super::super::config::BitgetCredentials;

        // Load credentials from environment
        let creds = BitgetCredentials::from_env().expect("BITGET_* env vars required");
        let trading_enabled = BitgetCredentials::trading_enabled_from_env();

        if !trading_enabled {
            println!("⚠️  BITGET_TRADING_ENABLED=0 (dry-run mode)");
            println!("    Set BITGET_TRADING_ENABLED=1 to execute real IOC orders");
            println!("    This test will verify dry-run behavior only.");
        } else {
            println!("🔥 BITGET_TRADING_ENABLED=1 - LIVE TRADING MODE!");
            println!("    This will execute REAL orders on Bitget!");
            println!("    Press Ctrl+C within 3 seconds to cancel...");
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }

        let mut sender = BitgetOrderSender::new(
            creds.api_key,
            creds.api_secret,
            creds.passphrase,
            5,  // 5 bps spread
            3,  // 3 retries
            trading_enabled, // Respect BITGET_TRADING_ENABLED env var
        );

        sender.start().await.expect("Failed to start sender");

        // Create a small test order using USDC pair
        // Use $97,000 limit (Bitget caps at ~2% above market, currently ~$95k)
        // This will fill at market price immediately
        let limit_price = Amount::from_u128_raw(97_000_000_000_000_000_000_000_u128); // $97,000 limit
        let order = AssetOrder::limit(
            "BTCUSDC".to_string(),
            OrderSide::Buy,
            Amount::from_u128_raw(60_000_000_000_000), // 0.00006 BTC (~$5.7 at $95k)
            limit_price,
        );
        println!("Placing IOC BUY: 0.00006 BTC @ limit $97,000 (will fill at market ~$95k)");

        let result = sender.execute_ioc_order(&order).await;

        match result {
            Ok(exec_result) => {
                println!("✓ IOC order executed!");
                println!("  Order ID: {}", exec_result.order_id);
                println!("  Status: {:?}", exec_result.status);
                println!("  Filled: {}", exec_result.filled_quantity);
                println!("  Avg Price: {}", exec_result.avg_price);
                println!("  Fees: {}", exec_result.fees);

                if trading_enabled {
                    // In live mode with low price, should be Cancelled (no fill) or PartiallyFilled
                    assert!(
                        exec_result.status == OrderStatus::Cancelled
                        || exec_result.status == OrderStatus::PartiallyFilled
                        || exec_result.status == OrderStatus::Filled,
                        "Unexpected status in live mode: {:?}",
                        exec_result.status
                    );
                } else {
                    // In dry-run mode, should be Failed with dry-run message
                    assert_eq!(exec_result.status, OrderStatus::Failed);
                    assert!(exec_result.error_message.as_ref().unwrap().contains("dry-run"));
                }
            }
            Err(e) => {
                if trading_enabled {
                    panic!("IOC execution failed in live mode: {:?}", e);
                } else {
                    println!("✓ Dry-run mode: {:?}", e);
                }
            }
        }

        sender.stop().await.expect("Failed to stop sender");
        println!("\n✅ Test complete!");
    }
}