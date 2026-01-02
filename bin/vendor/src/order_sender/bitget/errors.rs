use eyre::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BitgetErrorType {
    /// Network errors - should retry
    Network,
    
    /// Rate limit hit - should retry with longer delay
    RateLimit,
    
    /// Insufficient balance - should NOT retry
    InsufficientBalance,
    
    /// Invalid parameters (symbol, quantity, etc.) - should NOT retry
    InvalidParameters,
    
    /// Order rejected by exchange - should NOT retry
    OrderRejected,
    
    /// Authentication error - should NOT retry
    Authentication,
    
    /// Unknown error - retry with caution
    Unknown,
}

impl BitgetErrorType {
    /// Determine if this error type is retryable
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            BitgetErrorType::Network | BitgetErrorType::RateLimit | BitgetErrorType::Unknown
        )
    }

    /// Get recommended retry delay for this error type
    pub fn retry_delay_ms(&self, attempt: u8) -> u64 {
        match self {
            BitgetErrorType::Network => {
                // Exponential backoff: 500ms, 1000ms, 2000ms
                500 * (2_u64.pow(attempt as u32 - 1))
            }
            BitgetErrorType::RateLimit => {
                // Longer delays for rate limits: 2s, 4s, 8s
                2000 * (2_u64.pow(attempt as u32 - 1))
            }
            BitgetErrorType::Unknown => {
                // Conservative: 1s, 2s, 4s
                1000 * (2_u64.pow(attempt as u32 - 1))
            }
            _ => 0, // Non-retryable
        }
    }

    /// Classify error from error message
    pub fn from_error_message(error_msg: &str) -> Self {
        let lower = error_msg.to_lowercase();

        // Network errors
        if lower.contains("timeout")
            || lower.contains("connection")
            || lower.contains("network")
            || lower.contains("dns")
            || lower.contains("unreachable")
        {
            return BitgetErrorType::Network;
        }

        // Rate limiting
        if lower.contains("rate limit")
            || lower.contains("too many requests")
            || lower.contains("429")
        {
            return BitgetErrorType::RateLimit;
        }

        // Insufficient balance
        if lower.contains("insufficient")
            || lower.contains("balance")
            || lower.contains("not enough")
        {
            return BitgetErrorType::InsufficientBalance;
        }

        // Invalid parameters
        if lower.contains("invalid")
            || lower.contains("parameter")
            || lower.contains("symbol")
            || lower.contains("quantity")
            || lower.contains("price")
            || lower.contains("minimum")
            || lower.contains("maximum")
        {
            return BitgetErrorType::InvalidParameters;
        }

        // Order rejected
        if lower.contains("reject") || lower.contains("not allowed") {
            return BitgetErrorType::OrderRejected;
        }

        // Authentication
        if lower.contains("auth")
            || lower.contains("signature")
            || lower.contains("key")
            || lower.contains("40006")
            || lower.contains("40012")
        {
            return BitgetErrorType::Authentication;
        }

        // Default to unknown
        BitgetErrorType::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_classification() {
        assert_eq!(
            BitgetErrorType::from_error_message("Connection timeout"),
            BitgetErrorType::Network
        );
        assert_eq!(
            BitgetErrorType::from_error_message("Rate limit exceeded"),
            BitgetErrorType::RateLimit
        );
        assert_eq!(
            BitgetErrorType::from_error_message("Insufficient balance"),
            BitgetErrorType::InsufficientBalance
        );
        assert_eq!(
            BitgetErrorType::from_error_message("Invalid symbol"),
            BitgetErrorType::InvalidParameters
        );
    }

    #[test]
    fn test_retryable() {
        assert!(BitgetErrorType::Network.is_retryable());
        assert!(BitgetErrorType::RateLimit.is_retryable());
        assert!(!BitgetErrorType::InsufficientBalance.is_retryable());
        assert!(!BitgetErrorType::Authentication.is_retryable());
    }

    #[test]
    fn test_retry_delays() {
        let net = BitgetErrorType::Network;
        assert_eq!(net.retry_delay_ms(1), 500);
        assert_eq!(net.retry_delay_ms(2), 1000);
        assert_eq!(net.retry_delay_ms(3), 2000);

        let rate = BitgetErrorType::RateLimit;
        assert_eq!(rate.retry_delay_ms(1), 2000);
        assert_eq!(rate.retry_delay_ms(2), 4000);
        assert_eq!(rate.retry_delay_ms(3), 8000);
    }
}