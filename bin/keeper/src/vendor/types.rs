use serde::{Deserialize, Serialize};
use std::fmt;

/// Request to get quotes for specific assets
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteAssetsRequest {
    pub assets: Vec<u128>,
}

// =============================================================================
// Story 2.5: Update Market Data Types
// =============================================================================

/// Request to update market data for assets via Vendor
/// Sent from Keeper to Vendor after extracting assets via Stewart (Story 2.4)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateMarketDataRequest {
    /// Sorted unique asset IDs extracted from indices
    pub assets: Vec<u128>,
    /// Correlation ID for tracing across services
    pub batch_id: String,
}

/// Response from Vendor after processing market data update
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateMarketDataResponse {
    /// Status: "success", "partial", or "failed"
    pub status: String,
    /// Number of assets with market data submitted on-chain
    pub assets_updated: usize,
    /// Echo back correlation ID
    pub batch_id: String,
    /// Server timestamp (Unix seconds)
    pub timestamp: u64,
}

impl UpdateMarketDataResponse {
    /// Check if the update was fully successful
    #[allow(dead_code)] // Used in tests
    pub fn is_success(&self) -> bool {
        self.status == "success"
    }

    /// Check if the update was at least partially successful
    #[allow(dead_code)] // Used in tests
    pub fn is_partial(&self) -> bool {
        self.status == "partial"
    }
}

/// Error type for Vendor communication failures
#[derive(Debug, Clone)]
pub enum VendorError {
    /// HTTP request failed to send
    RequestFailed { url: String, reason: String },
    /// Vendor returned non-200 status
    NonSuccessStatus { status: u16, body: String },
    /// Request timed out
    Timeout { url: String, timeout_secs: u64 },
    /// All retry attempts exhausted
    RetryExhausted { attempts: u32, last_error: String },
    /// Serialization/deserialization error
    SerdeError(String),
}

impl fmt::Display for VendorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RequestFailed { url, reason } => {
                write!(f, "Request to {} failed: {}", url, reason)
            }
            Self::NonSuccessStatus { status, body } => {
                write!(f, "Vendor returned HTTP {}: {}", status, body)
            }
            Self::Timeout { url, timeout_secs } => {
                write!(f, "Request to {} timed out after {}s", url, timeout_secs)
            }
            Self::RetryExhausted { attempts, last_error } => {
                write!(
                    f,
                    "All {} retry attempts failed. Last error: {}",
                    attempts, last_error
                )
            }
            Self::SerdeError(msg) => {
                write!(f, "Serialization error: {}", msg)
            }
        }
    }
}

impl std::error::Error for VendorError {}

impl From<reqwest::Error> for VendorError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            Self::Timeout {
                url: err.url().map(|u| u.to_string()).unwrap_or_default(),
                timeout_secs: 0, // Unknown from reqwest error
            }
        } else if err.is_request() {
            Self::RequestFailed {
                url: err.url().map(|u| u.to_string()).unwrap_or_default(),
                reason: err.to_string(),
            }
        } else {
            Self::RequestFailed {
                url: err.url().map(|u| u.to_string()).unwrap_or_default(),
                reason: err.to_string(),
            }
        }
    }
}

impl From<serde_json::Error> for VendorError {
    fn from(err: serde_json::Error) -> Self {
        Self::SerdeError(err.to_string())
    }
}

/// Response containing market data for stale assets only
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetsQuote {
    pub assets: Vec<u128>,
    pub liquidity: Vec<f64>,
    pub prices: Vec<f64>,
    pub slopes: Vec<f64>,
}

impl AssetsQuote {
    pub fn len(&self) -> usize {
        self.assets.len()
    }
}

/// Health check response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub vendor_id: u128,
    pub tracked_assets: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_update_market_data_request_serialization() {
        let request = UpdateMarketDataRequest {
            assets: vec![101, 102, 103],
            batch_id: "time_window:keeper:1705123456789".to_string(),
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"assets\":[101,102,103]"));
        assert!(json.contains("\"batch_id\":\"time_window:keeper:1705123456789\""));

        let deserialized: UpdateMarketDataRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.assets, vec![101, 102, 103]);
        assert_eq!(deserialized.batch_id, "time_window:keeper:1705123456789");
    }

    #[test]
    fn test_update_market_data_response_serialization() {
        let response = UpdateMarketDataResponse {
            status: "success".to_string(),
            assets_updated: 15,
            batch_id: "time_window:keeper:1705123456789".to_string(),
            timestamp: 1705123456,
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"status\":\"success\""));
        assert!(json.contains("\"assets_updated\":15"));

        let deserialized: UpdateMarketDataResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.status, "success");
        assert_eq!(deserialized.assets_updated, 15);
        assert!(deserialized.is_success());
        assert!(!deserialized.is_partial());
    }

    #[test]
    fn test_update_market_data_response_partial() {
        let response = UpdateMarketDataResponse {
            status: "partial".to_string(),
            assets_updated: 10,
            batch_id: "batch123".to_string(),
            timestamp: 1705123456,
        };

        assert!(!response.is_success());
        assert!(response.is_partial());
    }

    #[test]
    fn test_vendor_error_display() {
        let err = VendorError::RequestFailed {
            url: "http://localhost:8081".to_string(),
            reason: "connection refused".to_string(),
        };
        assert!(err.to_string().contains("http://localhost:8081"));
        assert!(err.to_string().contains("connection refused"));

        let err = VendorError::NonSuccessStatus {
            status: 500,
            body: "Internal error".to_string(),
        };
        assert!(err.to_string().contains("HTTP 500"));

        let err = VendorError::Timeout {
            url: "http://vendor:8081".to_string(),
            timeout_secs: 30,
        };
        assert!(err.to_string().contains("timed out after 30s"));

        let err = VendorError::RetryExhausted {
            attempts: 3,
            last_error: "connection failed".to_string(),
        };
        assert!(err.to_string().contains("3 retry attempts"));
    }
}