//! Submission timing metrics for async on-chain submissions (Story 3-4)
//!
//! Tracks individual call times for debugging and performance monitoring.
//! Logs WARNING if total time exceeds 2 seconds (NFR17 requirement).

use std::time::{Duration, Instant};

/// Timing metrics for concurrent on-chain submissions (AC #6, #7)
#[derive(Debug, Clone)]
pub struct SubmissionMetrics {
    start: Instant,
    pub margin_ms: Option<u64>,
    pub supply_ms: Option<u64>,
    pub market_data_ms: Option<u64>,
    pub total_ms: Option<u64>,
    batch_id: Option<String>,
}

impl SubmissionMetrics {
    /// Start timing a new submission batch
    pub fn start(batch_id: Option<String>) -> Self {
        Self {
            start: Instant::now(),
            margin_ms: None,
            supply_ms: None,
            market_data_ms: None,
            total_ms: None,
            batch_id,
        }
    }

    /// Record margin submission completion time
    pub fn record_margin(&mut self, duration: Duration) {
        self.margin_ms = Some(duration.as_millis() as u64);
    }

    /// Record supply submission completion time
    pub fn record_supply(&mut self, duration: Duration) {
        self.supply_ms = Some(duration.as_millis() as u64);
    }

    /// Record market data submission completion time
    pub fn record_market_data(&mut self, duration: Duration) {
        self.market_data_ms = Some(duration.as_millis() as u64);
    }

    /// Finish timing and log results
    pub fn finish(&mut self) {
        let total = self.start.elapsed();
        self.total_ms = Some(total.as_millis() as u64);

        let batch_id = self.batch_id.as_deref().unwrap_or("unknown");

        tracing::info!(
            "📊 Submission metrics [batch_id={}]: margin={}ms, supply={}ms, market_data={}ms, total={}ms",
            batch_id,
            self.margin_ms.unwrap_or(0),
            self.supply_ms.unwrap_or(0),
            self.market_data_ms.unwrap_or(0),
            self.total_ms.unwrap_or(0)
        );

        // NFR17: Total submission time < 2 seconds
        if total.as_millis() > 2000 {
            tracing::warn!(
                "⚠️ Submission time exceeded 2s NFR17 target [batch_id={}]: {}ms",
                batch_id,
                total.as_millis()
            );
        }
    }

    /// Get total elapsed time
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn test_metrics_start() {
        let metrics = SubmissionMetrics::start(Some("test-batch".to_string()));
        assert!(metrics.margin_ms.is_none());
        assert!(metrics.supply_ms.is_none());
        assert!(metrics.market_data_ms.is_none());
        assert!(metrics.total_ms.is_none());
    }

    #[test]
    fn test_metrics_record() {
        let mut metrics = SubmissionMetrics::start(Some("test".to_string()));

        metrics.record_margin(Duration::from_millis(100));
        assert_eq!(metrics.margin_ms, Some(100));

        metrics.record_supply(Duration::from_millis(150));
        assert_eq!(metrics.supply_ms, Some(150));

        metrics.record_market_data(Duration::from_millis(200));
        assert_eq!(metrics.market_data_ms, Some(200));
    }

    #[test]
    fn test_metrics_finish() {
        let mut metrics = SubmissionMetrics::start(Some("test".to_string()));
        sleep(Duration::from_millis(10));
        metrics.finish();

        assert!(metrics.total_ms.is_some());
        assert!(metrics.total_ms.unwrap() >= 10);
    }

    #[test]
    fn test_metrics_elapsed() {
        let metrics = SubmissionMetrics::start(None);
        sleep(Duration::from_millis(5));
        let elapsed = metrics.elapsed();
        assert!(elapsed.as_millis() >= 5);
    }
}
