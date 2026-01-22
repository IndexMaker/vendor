//! Refill Manager - Automatic retry logic for partial/unfilled IOC orders
//!
//! This module provides intelligent refill logic that detects partial fills
//! and automatically retries with adjusted pricing.
//!
//! Story 3-8: Adaptive Pricing with Orderbook Depth Analysis and Refill Logic

use crate::order_sender::types::{AssetOrder, ExecutionResult, OrderSide, OrderStatus, OrderType};
use common::amount::Amount;

/// Default configuration constants for refill logic
pub const DEFAULT_MAX_REFILL_RETRIES: usize = 3;
pub const DEFAULT_PRICE_ADJUSTMENT_BPS: u32 = 10;
pub const DEFAULT_MIN_FILL_THRESHOLD: f64 = 0.90;
pub const DEFAULT_REFILL_DELAY_MS: u64 = 500;

/// Refill manager for handling partial fills and cancelled IOC orders.
///
/// When an IOC order doesn't fully fill, the refill manager decides whether
/// to retry and computes the adjusted order parameters.
///
/// # Strategy
/// - Partial fills below threshold: Retry with more aggressive pricing
/// - Cancelled (0% fill): Retry with even more aggressive pricing
/// - Full fills or fills above threshold: Accept and don't retry
///
/// # Example
/// ```ignore
/// let manager = RefillManager::default();
/// if manager.should_refill(&result, attempt) {
///     let refill_order = manager.compute_refill_order(&original, &result, attempt);
///     // Execute refill_order
/// }
/// ```
#[derive(Debug, Clone)]
pub struct RefillManager {
    /// Maximum number of refill attempts (default: 3)
    max_retries: usize,

    /// Price adjustment per retry in basis points (default: 10 bps)
    /// For BUY: increase limit price
    /// For SELL: decrease limit price
    price_adjustment_bps: u32,

    /// Minimum fill percentage to accept without refill (default: 0.90 = 90%)
    min_fill_threshold: f64,

    /// Delay between refill attempts in milliseconds (default: 500ms)
    refill_delay_ms: u64,
}

impl Default for RefillManager {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_REFILL_RETRIES,
            price_adjustment_bps: DEFAULT_PRICE_ADJUSTMENT_BPS,
            min_fill_threshold: DEFAULT_MIN_FILL_THRESHOLD,
            refill_delay_ms: DEFAULT_REFILL_DELAY_MS,
        }
    }
}

impl RefillManager {
    /// Create a new refill manager with custom parameters
    pub fn new(
        max_retries: usize,
        price_adjustment_bps: u32,
        min_fill_threshold: f64,
        refill_delay_ms: u64,
    ) -> Self {
        Self {
            max_retries,
            price_adjustment_bps,
            min_fill_threshold,
            refill_delay_ms,
        }
    }

    /// Builder pattern: set max retries
    pub fn with_max_retries(mut self, retries: usize) -> Self {
        self.max_retries = retries;
        self
    }

    /// Builder pattern: set price adjustment
    pub fn with_price_adjustment_bps(mut self, bps: u32) -> Self {
        self.price_adjustment_bps = bps;
        self
    }

    /// Builder pattern: set minimum fill threshold
    pub fn with_min_fill_threshold(mut self, threshold: f64) -> Self {
        self.min_fill_threshold = threshold.clamp(0.0, 1.0);
        self
    }

    /// Builder pattern: set refill delay
    pub fn with_refill_delay_ms(mut self, delay_ms: u64) -> Self {
        self.refill_delay_ms = delay_ms;
        self
    }

    /// Get max retries
    pub fn max_retries(&self) -> usize {
        self.max_retries
    }

    /// Get price adjustment in basis points
    pub fn price_adjustment_bps(&self) -> u32 {
        self.price_adjustment_bps
    }

    /// Get minimum fill threshold
    pub fn min_fill_threshold(&self) -> f64 {
        self.min_fill_threshold
    }

    /// Get refill delay in milliseconds
    pub fn refill_delay_ms(&self) -> u64 {
        self.refill_delay_ms
    }

    /// Determine if an order should be refilled.
    ///
    /// # Decision Logic
    /// - Return true if:
    ///   - Order was partially filled below threshold AND attempts < max_retries
    ///   - Order was cancelled (0% fill) AND attempts < max_retries
    /// - Return false if:
    ///   - Order was fully filled
    ///   - Fill percentage >= threshold (accept as good enough)
    ///   - Max retries exceeded
    ///   - Order failed with error (not just unfilled)
    ///
    /// # Arguments
    /// * `result` - Execution result from the order
    /// * `original_quantity` - Original requested quantity
    /// * `attempt` - Current attempt number (0-indexed)
    pub fn should_refill(
        &self,
        result: &ExecutionResult,
        original_quantity: Amount,
        attempt: usize,
    ) -> bool {
        // Check retry limit
        if attempt >= self.max_retries {
            tracing::debug!(
                "Refill denied for {}: max retries ({}) exceeded",
                result.symbol,
                self.max_retries
            );
            return false;
        }

        // Don't refill errors
        if result.status == OrderStatus::Failed || result.status == OrderStatus::Rejected {
            tracing::debug!(
                "Refill denied for {}: order failed with {:?}",
                result.symbol,
                result.status
            );
            return false;
        }

        // Calculate fill percentage
        let original_qty_f64 = original_quantity.to_u128_raw() as f64;
        let filled_qty_f64 = result.filled_quantity.to_u128_raw() as f64;

        if original_qty_f64 <= 0.0 {
            return false;
        }

        let fill_percentage = filled_qty_f64 / original_qty_f64;

        // Fully filled - no refill needed
        if result.status == OrderStatus::Filled || fill_percentage >= 0.9999 {
            tracing::debug!(
                "Refill denied for {}: fully filled ({:.2}%)",
                result.symbol,
                fill_percentage * 100.0
            );
            return false;
        }

        // Check if fill percentage meets threshold
        if fill_percentage >= self.min_fill_threshold {
            tracing::debug!(
                "Refill denied for {}: fill {:.2}% meets threshold {:.2}%",
                result.symbol,
                fill_percentage * 100.0,
                self.min_fill_threshold * 100.0
            );
            return false;
        }

        // Need refill: partial fill below threshold or cancelled
        let refill_reason = if fill_percentage == 0.0 {
            "cancelled (0% fill)"
        } else {
            "partial fill below threshold"
        };

        tracing::info!(
            "Refill approved for {}: {} - {:.2}% filled, attempt {}/{}",
            result.symbol,
            refill_reason,
            fill_percentage * 100.0,
            attempt + 1,
            self.max_retries
        );

        true
    }

    /// Compute a refill order for the unfilled portion.
    ///
    /// # Adjustments Made
    /// - Quantity: Only the unfilled remainder
    /// - Price: Adjusted by `price_adjustment_bps * attempt` for more aggressive pricing
    /// - Client Order ID: Updated with retry suffix
    ///
    /// # Arguments
    /// * `original` - Original order that was partially filled
    /// * `result` - Execution result from the previous attempt
    /// * `attempt` - Current attempt number (0-indexed, used for price adjustment)
    ///
    /// # Returns
    /// New order for the unfilled quantity with adjusted price
    pub fn compute_refill_order(
        &self,
        original: &AssetOrder,
        result: &ExecutionResult,
        attempt: usize,
    ) -> AssetOrder {
        // Calculate remaining quantity
        let original_qty_raw = original.quantity.to_u128_raw();
        let filled_qty_raw = result.filled_quantity.to_u128_raw();
        let remaining_qty_raw = original_qty_raw.saturating_sub(filled_qty_raw);
        let remaining_quantity = Amount::from_u128_raw(remaining_qty_raw);

        // Calculate adjusted price (more aggressive with each attempt)
        let adjusted_price = self.compute_adjusted_price(original, attempt);

        tracing::debug!(
            "Refill order: {} {} remaining {:.6} @ ${:.4} (adjustment: +{}bps * {})",
            original.side,
            original.symbol,
            remaining_qty_raw as f64 / 1e18,
            adjusted_price.to_u128_raw() as f64 / 1e18,
            self.price_adjustment_bps,
            attempt + 1
        );

        AssetOrder {
            symbol: original.symbol.clone(),
            side: original.side,
            quantity: remaining_quantity,
            order_type: OrderType::Limit,
            limit_price: Some(adjusted_price),
        }
    }

    /// Compute adjusted limit price for refill attempt.
    ///
    /// For each retry attempt, the price becomes more aggressive:
    /// - BUY: Increase limit price (willing to pay more)
    /// - SELL: Decrease limit price (willing to accept less)
    ///
    /// The adjustment is cumulative: attempt 2 has 2x the adjustment of attempt 1.
    fn compute_adjusted_price(&self, original: &AssetOrder, attempt: usize) -> Amount {
        // Get base price (original limit or some reasonable default)
        let base_price_raw = original
            .limit_price
            .map(|p| p.to_u128_raw())
            .unwrap_or(0);

        if base_price_raw == 0 {
            // If no limit price, we can't adjust - return zero (caller should handle)
            return Amount::ZERO;
        }

        // Calculate total adjustment for this attempt
        // attempt 0 (first refill) = 1x adjustment
        // attempt 1 (second refill) = 2x adjustment
        // etc.
        let total_adjustment_bps = self.price_adjustment_bps * (attempt + 1) as u32;
        let adjustment_multiplier = total_adjustment_bps as f64 / 10_000.0;

        let base_price_f64 = base_price_raw as f64;

        let adjusted_price_f64 = match original.side {
            OrderSide::Buy => {
                // BUY: Increase price (more aggressive)
                base_price_f64 * (1.0 + adjustment_multiplier)
            }
            OrderSide::Sell => {
                // SELL: Decrease price (more aggressive)
                base_price_f64 * (1.0 - adjustment_multiplier)
            }
        };

        Amount::from_u128_raw(adjusted_price_f64 as u128)
    }

    /// Calculate the cumulative fill across multiple execution results.
    ///
    /// # Arguments
    /// * `results` - Vector of execution results from all attempts
    ///
    /// # Returns
    /// Tuple of (total_filled_quantity, total_fees, average_price)
    pub fn aggregate_fills(&self, results: &[ExecutionResult]) -> (Amount, Amount, Amount) {
        let mut total_filled_raw = 0u128;
        let mut total_fees_raw = 0u128;
        let mut weighted_price_sum = 0.0f64;

        for result in results {
            if result.is_success() {
                let filled_raw = result.filled_quantity.to_u128_raw();
                let price_f64 = result.avg_price.to_u128_raw() as f64;
                let filled_f64 = filled_raw as f64;

                total_filled_raw += filled_raw;
                total_fees_raw += result.fees.to_u128_raw();
                weighted_price_sum += price_f64 * filled_f64;
            }
        }

        let total_filled = Amount::from_u128_raw(total_filled_raw);
        let total_fees = Amount::from_u128_raw(total_fees_raw);

        let avg_price = if total_filled_raw > 0 {
            let avg_price_f64 = weighted_price_sum / (total_filled_raw as f64);
            Amount::from_u128_raw(avg_price_f64 as u128)
        } else {
            Amount::ZERO
        };

        (total_filled, total_fees, avg_price)
    }

    /// Get the delay duration for async sleep between refills.
    pub fn refill_delay(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.refill_delay_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_order(side: OrderSide, quantity_f64: f64, price_f64: f64) -> AssetOrder {
        AssetOrder {
            symbol: "BTCUSDC".to_string(),
            side,
            quantity: Amount::from_u128_raw((quantity_f64 * 1e18) as u128),
            order_type: OrderType::Limit,
            limit_price: Some(Amount::from_u128_raw((price_f64 * 1e18) as u128)),
        }
    }

    fn create_execution_result(
        status: OrderStatus,
        filled_f64: f64,
        price_f64: f64,
    ) -> ExecutionResult {
        ExecutionResult {
            symbol: "BTCUSDC".to_string(),
            order_id: "TEST-123".to_string(),
            filled_quantity: Amount::from_u128_raw((filled_f64 * 1e18) as u128),
            avg_price: Amount::from_u128_raw((price_f64 * 1e18) as u128),
            fees: Amount::from_u128_raw((0.001 * 1e18) as u128),
            fee_detail: None,
            status,
            error_message: None,
        }
    }

    #[test]
    fn test_default_manager() {
        let manager = RefillManager::default();
        assert_eq!(manager.max_retries, DEFAULT_MAX_REFILL_RETRIES);
        assert_eq!(manager.price_adjustment_bps, DEFAULT_PRICE_ADJUSTMENT_BPS);
        assert!((manager.min_fill_threshold - DEFAULT_MIN_FILL_THRESHOLD).abs() < 0.001);
        assert_eq!(manager.refill_delay_ms, DEFAULT_REFILL_DELAY_MS);
    }

    #[test]
    fn test_builder_pattern() {
        let manager = RefillManager::default()
            .with_max_retries(5)
            .with_price_adjustment_bps(20)
            .with_min_fill_threshold(0.85)
            .with_refill_delay_ms(1000);

        assert_eq!(manager.max_retries, 5);
        assert_eq!(manager.price_adjustment_bps, 20);
        assert!((manager.min_fill_threshold - 0.85).abs() < 0.001);
        assert_eq!(manager.refill_delay_ms, 1000);
    }

    #[test]
    fn test_should_refill_fully_filled() {
        let manager = RefillManager::default();
        let original_qty = Amount::from_u128_raw((1.0 * 1e18) as u128);
        let result = create_execution_result(OrderStatus::Filled, 1.0, 100.0);

        assert!(!manager.should_refill(&result, original_qty, 0));
    }

    #[test]
    fn test_should_refill_partial_below_threshold() {
        let manager = RefillManager::default().with_min_fill_threshold(0.90);
        let original_qty = Amount::from_u128_raw((1.0 * 1e18) as u128);

        // 50% fill - below 90% threshold
        let result = create_execution_result(OrderStatus::PartiallyFilled, 0.5, 100.0);

        assert!(manager.should_refill(&result, original_qty, 0));
    }

    #[test]
    fn test_should_refill_partial_above_threshold() {
        let manager = RefillManager::default().with_min_fill_threshold(0.90);
        let original_qty = Amount::from_u128_raw((1.0 * 1e18) as u128);

        // 95% fill - above 90% threshold
        let result = create_execution_result(OrderStatus::PartiallyFilled, 0.95, 100.0);

        assert!(!manager.should_refill(&result, original_qty, 0));
    }

    #[test]
    fn test_should_refill_cancelled() {
        let manager = RefillManager::default();
        let original_qty = Amount::from_u128_raw((1.0 * 1e18) as u128);

        // 0% fill (cancelled)
        let result = create_execution_result(OrderStatus::Cancelled, 0.0, 0.0);

        assert!(manager.should_refill(&result, original_qty, 0));
    }

    #[test]
    fn test_should_refill_max_retries_exceeded() {
        let manager = RefillManager::default().with_max_retries(3);
        let original_qty = Amount::from_u128_raw((1.0 * 1e18) as u128);
        let result = create_execution_result(OrderStatus::PartiallyFilled, 0.5, 100.0);

        // attempt 0, 1, 2 should be allowed
        assert!(manager.should_refill(&result, original_qty, 0));
        assert!(manager.should_refill(&result, original_qty, 1));
        assert!(manager.should_refill(&result, original_qty, 2));

        // attempt 3 should be denied (exceeded max)
        assert!(!manager.should_refill(&result, original_qty, 3));
    }

    #[test]
    fn test_should_refill_failed_order() {
        let manager = RefillManager::default();
        let original_qty = Amount::from_u128_raw((1.0 * 1e18) as u128);

        let mut result = create_execution_result(OrderStatus::Failed, 0.0, 0.0);
        result.error_message = Some("API error".to_string());

        assert!(!manager.should_refill(&result, original_qty, 0));
    }

    #[test]
    fn test_compute_refill_order_quantity() {
        let manager = RefillManager::default();

        let original = create_test_order(OrderSide::Buy, 1.0, 100.0);
        let result = create_execution_result(OrderStatus::PartiallyFilled, 0.6, 100.0);

        let refill = manager.compute_refill_order(&original, &result, 0);

        // Should be 0.4 remaining (1.0 - 0.6)
        let remaining_f64 = refill.quantity.to_u128_raw() as f64 / 1e18;
        assert!((remaining_f64 - 0.4).abs() < 0.001);
    }

    #[test]
    fn test_compute_refill_order_buy_price_adjustment() {
        let manager = RefillManager::default().with_price_adjustment_bps(10);

        let original = create_test_order(OrderSide::Buy, 1.0, 100.0);
        let result = create_execution_result(OrderStatus::PartiallyFilled, 0.5, 100.0);

        // First refill (attempt 0): +10 bps = +0.10%
        let refill_0 = manager.compute_refill_order(&original, &result, 0);
        let price_0 = refill_0.limit_price.unwrap().to_u128_raw() as f64 / 1e18;
        assert!(price_0 > 100.0, "BUY refill price should increase");
        assert!((price_0 - 100.10).abs() < 0.01, "Should be ~100.10: {}", price_0);

        // Second refill (attempt 1): +20 bps = +0.20%
        let refill_1 = manager.compute_refill_order(&original, &result, 1);
        let price_1 = refill_1.limit_price.unwrap().to_u128_raw() as f64 / 1e18;
        assert!((price_1 - 100.20).abs() < 0.01, "Should be ~100.20: {}", price_1);
    }

    #[test]
    fn test_compute_refill_order_sell_price_adjustment() {
        let manager = RefillManager::default().with_price_adjustment_bps(10);

        let original = create_test_order(OrderSide::Sell, 1.0, 100.0);
        let result = create_execution_result(OrderStatus::PartiallyFilled, 0.5, 100.0);

        // First refill (attempt 0): -10 bps = -0.10%
        let refill_0 = manager.compute_refill_order(&original, &result, 0);
        let price_0 = refill_0.limit_price.unwrap().to_u128_raw() as f64 / 1e18;
        assert!(price_0 < 100.0, "SELL refill price should decrease");
        assert!((price_0 - 99.90).abs() < 0.01, "Should be ~99.90: {}", price_0);

        // Second refill (attempt 1): -20 bps = -0.20%
        let refill_1 = manager.compute_refill_order(&original, &result, 1);
        let price_1 = refill_1.limit_price.unwrap().to_u128_raw() as f64 / 1e18;
        assert!((price_1 - 99.80).abs() < 0.01, "Should be ~99.80: {}", price_1);
    }

    #[test]
    fn test_aggregate_fills_single() {
        let manager = RefillManager::default();

        let results = vec![create_execution_result(OrderStatus::Filled, 1.0, 100.0)];

        let (total_filled, total_fees, avg_price) = manager.aggregate_fills(&results);

        let filled_f64 = total_filled.to_u128_raw() as f64 / 1e18;
        let fees_f64 = total_fees.to_u128_raw() as f64 / 1e18;
        let price_f64 = avg_price.to_u128_raw() as f64 / 1e18;

        assert!((filled_f64 - 1.0).abs() < 0.001);
        assert!((fees_f64 - 0.001).abs() < 0.0001);
        assert!((price_f64 - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_aggregate_fills_multiple() {
        let manager = RefillManager::default();

        let results = vec![
            create_execution_result(OrderStatus::PartiallyFilled, 0.5, 100.0),
            create_execution_result(OrderStatus::PartiallyFilled, 0.3, 101.0),
            create_execution_result(OrderStatus::Filled, 0.2, 102.0),
        ];

        let (total_filled, total_fees, avg_price) = manager.aggregate_fills(&results);

        let filled_f64 = total_filled.to_u128_raw() as f64 / 1e18;
        let fees_f64 = total_fees.to_u128_raw() as f64 / 1e18;
        let price_f64 = avg_price.to_u128_raw() as f64 / 1e18;

        // Total: 0.5 + 0.3 + 0.2 = 1.0
        assert!((filled_f64 - 1.0).abs() < 0.001);

        // Fees: 0.001 * 3 = 0.003
        assert!((fees_f64 - 0.003).abs() < 0.0001);

        // VWAP: (0.5*100 + 0.3*101 + 0.2*102) / 1.0 = 100.7
        assert!((price_f64 - 100.7).abs() < 0.01, "Avg price: {}", price_f64);
    }

    #[test]
    fn test_aggregate_fills_with_failures() {
        let manager = RefillManager::default();

        let mut failed_result = create_execution_result(OrderStatus::Failed, 0.0, 0.0);
        failed_result.error_message = Some("Error".to_string());

        let results = vec![
            create_execution_result(OrderStatus::Filled, 0.5, 100.0),
            failed_result, // Should be ignored
            create_execution_result(OrderStatus::Filled, 0.5, 101.0),
        ];

        let (total_filled, _total_fees, _avg_price) = manager.aggregate_fills(&results);

        let filled_f64 = total_filled.to_u128_raw() as f64 / 1e18;

        // Should only count successful fills
        assert!((filled_f64 - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_refill_delay_duration() {
        let manager = RefillManager::default().with_refill_delay_ms(1000);

        let delay = manager.refill_delay();
        assert_eq!(delay, std::time::Duration::from_millis(1000));
    }

    #[test]
    fn test_min_fill_threshold_clamped() {
        // Test that threshold is clamped to [0, 1]
        let manager_high = RefillManager::default().with_min_fill_threshold(1.5);
        assert!((manager_high.min_fill_threshold - 1.0).abs() < 0.001);

        let manager_low = RefillManager::default().with_min_fill_threshold(-0.5);
        assert!((manager_low.min_fill_threshold - 0.0).abs() < 0.001);
    }
}
