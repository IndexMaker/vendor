//! Rate-limited order queue for respecting Bitget's 10 orders/sec limit.
//!
//! This module provides a queue-based rate limiter that ensures we don't
//! exceed Bitget's API rate limits when sending multiple orders.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// A rate limiter that enforces a maximum number of operations per second.
///
/// Uses a sliding window approach to track the last N operation timestamps
/// and delays if we're at capacity.
///
/// # Example
/// ```ignore
/// let mut limiter = RateLimiter::new(10); // 10 ops/sec
/// for _ in 0..20 {
///     limiter.wait_for_slot().await;
///     // do_operation()
///     limiter.record_operation();
/// }
/// ```
pub struct RateLimiter {
    /// Timestamps of recent operations within the sliding window
    timestamps: VecDeque<Instant>,
    /// Maximum operations allowed per second
    rate_limit: usize,
    /// Window duration (1 second by default)
    window: Duration,
}

impl RateLimiter {
    /// Create a new rate limiter with the specified limit per second.
    ///
    /// # Arguments
    /// * `rate_limit` - Maximum operations allowed per second (default: 10 for Bitget)
    pub fn new(rate_limit: usize) -> Self {
        Self {
            timestamps: VecDeque::with_capacity(rate_limit),
            rate_limit,
            window: Duration::from_secs(1),
        }
    }

    /// Create a rate limiter with a custom window duration.
    ///
    /// # Arguments
    /// * `rate_limit` - Maximum operations allowed within the window
    /// * `window` - The sliding window duration
    pub fn with_window(rate_limit: usize, window: Duration) -> Self {
        Self {
            timestamps: VecDeque::with_capacity(rate_limit),
            rate_limit,
            window,
        }
    }

    /// Wait until a rate limit slot is available.
    ///
    /// If we're at capacity, this will sleep until the oldest operation
    /// falls outside the sliding window.
    pub async fn wait_for_slot(&mut self) {
        // Clean up old timestamps outside the window
        self.cleanup_old_timestamps();

        // If we're at capacity, wait for the oldest to expire
        if self.timestamps.len() >= self.rate_limit {
            if let Some(oldest) = self.timestamps.front() {
                let elapsed = oldest.elapsed();
                if elapsed < self.window {
                    let sleep_duration = self.window - elapsed;
                    tracing::debug!(
                        "Rate limit: waiting {:?} ({}/{} slots used)",
                        sleep_duration,
                        self.timestamps.len(),
                        self.rate_limit
                    );
                    tokio::time::sleep(sleep_duration).await;
                    // Clean up again after sleeping
                    self.cleanup_old_timestamps();
                }
            }
        }
    }

    /// Record that an operation was performed.
    ///
    /// Call this immediately after performing the rate-limited operation.
    pub fn record_operation(&mut self) {
        self.timestamps.push_back(Instant::now());
        // Keep the deque bounded
        while self.timestamps.len() > self.rate_limit {
            self.timestamps.pop_front();
        }
    }

    /// Check if we can perform an operation without waiting.
    pub fn can_proceed(&mut self) -> bool {
        self.cleanup_old_timestamps();
        self.timestamps.len() < self.rate_limit
    }

    /// Get the number of available slots.
    pub fn available_slots(&mut self) -> usize {
        self.cleanup_old_timestamps();
        self.rate_limit.saturating_sub(self.timestamps.len())
    }

    /// Get the current rate limit.
    pub fn rate_limit(&self) -> usize {
        self.rate_limit
    }

    /// Clean up timestamps outside the sliding window.
    fn cleanup_old_timestamps(&mut self) {
        let now = Instant::now();
        while let Some(oldest) = self.timestamps.front() {
            if now.duration_since(*oldest) >= self.window {
                self.timestamps.pop_front();
            } else {
                break;
            }
        }
    }

    /// Reset the rate limiter, clearing all recorded timestamps.
    pub fn reset(&mut self) {
        self.timestamps.clear();
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(10) // Bitget's default rate limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_rate_limiter() {
        let limiter = RateLimiter::new(10);
        assert_eq!(limiter.rate_limit(), 10);
    }

    #[test]
    fn test_default_rate_limiter() {
        let limiter = RateLimiter::default();
        assert_eq!(limiter.rate_limit(), 10);
    }

    #[test]
    fn test_can_proceed_when_empty() {
        let mut limiter = RateLimiter::new(10);
        assert!(limiter.can_proceed());
        assert_eq!(limiter.available_slots(), 10);
    }

    #[test]
    fn test_record_operation() {
        let mut limiter = RateLimiter::new(10);
        limiter.record_operation();
        assert_eq!(limiter.available_slots(), 9);
    }

    #[test]
    fn test_at_capacity() {
        let mut limiter = RateLimiter::new(3);
        for _ in 0..3 {
            limiter.record_operation();
        }
        assert!(!limiter.can_proceed());
        assert_eq!(limiter.available_slots(), 0);
    }

    #[test]
    fn test_reset() {
        let mut limiter = RateLimiter::new(3);
        for _ in 0..3 {
            limiter.record_operation();
        }
        assert!(!limiter.can_proceed());

        limiter.reset();
        assert!(limiter.can_proceed());
        assert_eq!(limiter.available_slots(), 3);
    }

    #[tokio::test]
    async fn test_wait_for_slot_no_wait_when_available() {
        let mut limiter = RateLimiter::new(10);
        let start = Instant::now();
        limiter.wait_for_slot().await;
        let elapsed = start.elapsed();
        // Should complete immediately
        assert!(elapsed < Duration::from_millis(10));
    }

    #[tokio::test]
    async fn test_rate_limiting_enforced() {
        // Use a very short window for testing
        let mut limiter = RateLimiter::with_window(2, Duration::from_millis(100));

        // Fill up the slots
        limiter.record_operation();
        limiter.record_operation();

        let start = Instant::now();
        limiter.wait_for_slot().await;
        let elapsed = start.elapsed();

        // Should have waited approximately 100ms (the window duration)
        assert!(elapsed >= Duration::from_millis(90), "Expected wait of ~100ms, got {:?}", elapsed);
        assert!(elapsed < Duration::from_millis(200), "Wait took too long: {:?}", elapsed);
    }

    #[tokio::test]
    async fn test_sliding_window_cleanup() {
        let mut limiter = RateLimiter::with_window(2, Duration::from_millis(50));

        // Record an operation
        limiter.record_operation();
        assert_eq!(limiter.available_slots(), 1);

        // Wait for the window to expire
        tokio::time::sleep(Duration::from_millis(60)).await;

        // Old timestamp should be cleaned up
        assert_eq!(limiter.available_slots(), 2);
    }
}
