//! Withdrawal handler (Story 3-6, AC #6)
//!
//! Manages USDC proceeds from sells back to users.

use alloy_primitives::Address;
use common::amount::Amount;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

/// A pending withdrawal
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingWithdrawal {
    /// Withdrawal ID
    pub id: String,
    /// User address
    pub user: String,
    /// Amount to withdraw (in USDC)
    pub amount: Amount,
    /// When the withdrawal was queued
    pub queued_at: DateTime<Utc>,
    /// Status
    pub status: WithdrawalStatus,
    /// Correlation ID from the sell order
    pub correlation_id: Option<String>,
    /// Number of retry attempts
    pub retry_count: u32,
    /// Last error message
    pub last_error: Option<String>,
}

/// Withdrawal status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WithdrawalStatus {
    /// Waiting to be processed
    Pending,
    /// Currently being processed
    Processing,
    /// Successfully completed
    Completed,
    /// Failed (will retry)
    Failed,
    /// Permanently failed
    PermanentlyFailed,
}

impl std::fmt::Display for WithdrawalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WithdrawalStatus::Pending => write!(f, "Pending"),
            WithdrawalStatus::Processing => write!(f, "Processing"),
            WithdrawalStatus::Completed => write!(f, "Completed"),
            WithdrawalStatus::Failed => write!(f, "Failed"),
            WithdrawalStatus::PermanentlyFailed => write!(f, "PermanentlyFailed"),
        }
    }
}

/// Result of processing a withdrawal
#[derive(Debug, Clone)]
pub struct WithdrawalResult {
    pub id: String,
    pub user: String,
    pub amount: Amount,
    pub status: WithdrawalStatus,
    pub tx_hash: Option<String>,
    pub error: Option<String>,
}

/// Handler for withdrawal processing
pub struct WithdrawalHandler {
    /// Pending withdrawals by ID
    pending: HashMap<String, PendingWithdrawal>,
    /// Withdrawal timeout duration
    timeout: Duration,
    /// Maximum retry attempts
    max_retries: u32,
    /// Counter for generating IDs
    next_id: u64,
}

impl WithdrawalHandler {
    /// Create a new withdrawal handler
    pub fn new(timeout: Duration) -> Self {
        Self {
            pending: HashMap::new(),
            timeout,
            max_retries: 3,
            next_id: 1,
        }
    }

    /// Queue a withdrawal for processing
    pub fn queue_withdrawal(
        &mut self,
        user: String,
        amount: Amount,
        correlation_id: Option<String>,
    ) -> String {
        let id = format!("W-{}", self.next_id);
        self.next_id += 1;

        let withdrawal = PendingWithdrawal {
            id: id.clone(),
            user: user.clone(),
            amount,
            queued_at: Utc::now(),
            status: WithdrawalStatus::Pending,
            correlation_id,
            retry_count: 0,
            last_error: None,
        };

        tracing::info!(
            "Queued withdrawal {}: {} USDC to {}",
            id,
            amount.to_u128_raw() as f64 / 1e18,
            user
        );

        self.pending.insert(id.clone(), withdrawal);
        id
    }

    /// Get pending withdrawals ready for processing
    pub fn get_pending(&self) -> Vec<&PendingWithdrawal> {
        self.pending
            .values()
            .filter(|w| w.status == WithdrawalStatus::Pending || w.status == WithdrawalStatus::Failed)
            .collect()
    }

    /// Mark a withdrawal as processing
    pub fn mark_processing(&mut self, id: &str) -> Option<&PendingWithdrawal> {
        if let Some(w) = self.pending.get_mut(id) {
            w.status = WithdrawalStatus::Processing;
            return Some(w);
        }
        None
    }

    /// Mark a withdrawal as completed
    pub fn mark_completed(&mut self, id: &str) -> Option<WithdrawalResult> {
        if let Some(w) = self.pending.remove(id) {
            tracing::info!(
                "Withdrawal {} completed: {} USDC to {}",
                id,
                w.amount.to_u128_raw() as f64 / 1e18,
                w.user
            );

            return Some(WithdrawalResult {
                id: w.id,
                user: w.user,
                amount: w.amount,
                status: WithdrawalStatus::Completed,
                tx_hash: None,
                error: None,
            });
        }
        None
    }

    /// Mark a withdrawal as failed (will retry)
    pub fn mark_failed(&mut self, id: &str, error: String) -> Option<&PendingWithdrawal> {
        if let Some(w) = self.pending.get_mut(id) {
            w.retry_count += 1;
            w.last_error = Some(error.clone());

            if w.retry_count >= self.max_retries {
                w.status = WithdrawalStatus::PermanentlyFailed;
                tracing::error!(
                    "Withdrawal {} permanently failed after {} attempts: {}",
                    id,
                    w.retry_count,
                    error
                );
            } else {
                w.status = WithdrawalStatus::Failed;
                tracing::warn!(
                    "Withdrawal {} failed (attempt {}/{}): {}",
                    id,
                    w.retry_count,
                    self.max_retries,
                    error
                );
            }

            return Some(w);
        }
        None
    }

    /// Process all pending withdrawals
    ///
    /// In a real implementation, this would:
    /// 1. Call Bitget withdrawal API
    /// 2. Wait for confirmation
    /// 3. Update status
    ///
    /// For now, we just return the pending list for external processing
    pub fn process_pending_withdrawals(&mut self) -> Vec<WithdrawalResult> {
        let mut results = Vec::new();
        let ids: Vec<String> = self.get_pending()
            .iter()
            .map(|w| w.id.clone())
            .collect();

        for id in ids {
            if let Some(w) = self.mark_processing(&id) {
                // In real implementation: call withdrawal API here

                // For now, simulate success
                results.push(WithdrawalResult {
                    id: w.id.clone(),
                    user: w.user.clone(),
                    amount: w.amount,
                    status: WithdrawalStatus::Processing,
                    tx_hash: None,
                    error: None,
                });
            }
        }

        results
    }

    /// Get withdrawal by ID
    pub fn get(&self, id: &str) -> Option<&PendingWithdrawal> {
        self.pending.get(id)
    }

    /// Get number of pending withdrawals
    pub fn pending_count(&self) -> usize {
        self.pending
            .values()
            .filter(|w| w.status == WithdrawalStatus::Pending)
            .count()
    }

    /// Get total pending amount
    pub fn total_pending_amount(&self) -> Amount {
        self.pending
            .values()
            .filter(|w| w.status == WithdrawalStatus::Pending || w.status == WithdrawalStatus::Processing)
            .fold(Amount::ZERO, |acc, w| {
                acc.checked_add(w.amount).unwrap_or(acc)
            })
    }

    /// Get timed out withdrawals
    pub fn get_timed_out(&self) -> Vec<&PendingWithdrawal> {
        let timeout_secs = self.timeout.as_secs() as i64;

        self.pending
            .values()
            .filter(|w| {
                w.status == WithdrawalStatus::Processing
                    && Utc::now().signed_duration_since(w.queued_at).num_seconds() > timeout_secs
            })
            .collect()
    }

    /// Get all withdrawals for persistence
    pub fn get_all(&self) -> Vec<PendingWithdrawal> {
        self.pending.values().cloned().collect()
    }

    /// Restore withdrawals from persistence
    pub fn restore(&mut self, withdrawals: Vec<PendingWithdrawal>) {
        let mut max_id = 0u64;

        for w in withdrawals {
            // Parse ID to track next_id
            if let Some(num_str) = w.id.strip_prefix("W-") {
                if let Ok(num) = num_str.parse::<u64>() {
                    if num > max_id {
                        max_id = num;
                    }
                }
            }

            self.pending.insert(w.id.clone(), w);
        }

        self.next_id = max_id + 1;

        tracing::info!(
            "Restored {} withdrawals (next_id: {})",
            self.pending.len(),
            self.next_id
        );
    }
}

impl Default for WithdrawalHandler {
    fn default() -> Self {
        Self::new(Duration::from_secs(300)) // 5 minute timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amount(val: f64) -> Amount {
        Amount::from_u128_raw((val * 1e18) as u128)
    }

    #[test]
    fn test_queue_withdrawal() {
        let mut handler = WithdrawalHandler::default();

        let id = handler.queue_withdrawal(
            "0x1234".to_string(),
            amount(100.0),
            Some("corr-1".to_string()),
        );

        assert!(id.starts_with("W-"));
        assert_eq!(handler.pending_count(), 1);
    }

    #[test]
    fn test_mark_completed() {
        let mut handler = WithdrawalHandler::default();

        let id = handler.queue_withdrawal(
            "0x1234".to_string(),
            amount(100.0),
            None,
        );

        let result = handler.mark_completed(&id).unwrap();
        assert!(matches!(result.status, WithdrawalStatus::Completed));
        assert_eq!(handler.pending_count(), 0);
    }

    #[test]
    fn test_mark_failed_retry() {
        let mut handler = WithdrawalHandler::default();

        let id = handler.queue_withdrawal(
            "0x1234".to_string(),
            amount(100.0),
            None,
        );

        // First failure - should retry
        handler.mark_failed(&id, "Network error".to_string());
        let w = handler.get(&id).unwrap();
        assert_eq!(w.status, WithdrawalStatus::Failed);
        assert_eq!(w.retry_count, 1);

        // Second failure
        handler.mark_failed(&id, "Network error".to_string());
        let w = handler.get(&id).unwrap();
        assert_eq!(w.retry_count, 2);

        // Third failure - permanent
        handler.mark_failed(&id, "Network error".to_string());
        let w = handler.get(&id).unwrap();
        assert_eq!(w.status, WithdrawalStatus::PermanentlyFailed);
    }

    #[test]
    fn test_total_pending_amount() {
        let mut handler = WithdrawalHandler::default();

        handler.queue_withdrawal("0x1".to_string(), amount(100.0), None);
        handler.queue_withdrawal("0x2".to_string(), amount(50.0), None);

        let total = handler.total_pending_amount();
        let expected = 150.0 * 1e18;
        let actual = total.to_u128_raw() as f64;
        assert!((actual - expected).abs() < 1e12);
    }

    #[test]
    fn test_restore() {
        let mut handler = WithdrawalHandler::default();

        let withdrawals = vec![
            PendingWithdrawal {
                id: "W-5".to_string(),
                user: "0x1234".to_string(),
                amount: amount(100.0),
                queued_at: Utc::now(),
                status: WithdrawalStatus::Pending,
                correlation_id: None,
                retry_count: 0,
                last_error: None,
            },
            PendingWithdrawal {
                id: "W-10".to_string(),
                user: "0x5678".to_string(),
                amount: amount(200.0),
                queued_at: Utc::now(),
                status: WithdrawalStatus::Pending,
                correlation_id: None,
                retry_count: 0,
                last_error: None,
            },
        ];

        handler.restore(withdrawals);

        assert_eq!(handler.pending_count(), 2);
        assert_eq!(handler.next_id, 11); // Next after 10
    }

    #[test]
    fn test_get_pending() {
        let mut handler = WithdrawalHandler::default();

        let id1 = handler.queue_withdrawal("0x1".to_string(), amount(100.0), None);
        let id2 = handler.queue_withdrawal("0x2".to_string(), amount(50.0), None);

        // Mark one as processing
        handler.mark_processing(&id1);

        let pending = handler.get_pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id2);
    }
}
