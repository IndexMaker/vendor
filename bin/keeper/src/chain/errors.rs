//! Error types for chain connectivity

/// Errors that can occur during chain operations
#[derive(Debug)]
#[allow(dead_code)] // VaultNotFound used in extractor From impl
pub enum ChainError {
    // Connection errors
    WebSocketError(String),
    RpcError(String),
    ConnectionLost { retry_count: u32 },

    // Event processing errors
    EventParseError { reason: String },
    SubscriptionError(String),

    // Vault discovery errors
    StewardCallFailed(String),
    VaultNotFound { index_id: u128 },
    InvalidVaultAddress(String),

    // Configuration errors
    InvalidConfig(String),
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChainError::WebSocketError(msg) => write!(f, "WebSocket error: {}", msg),
            ChainError::RpcError(msg) => write!(f, "RPC error: {}", msg),
            ChainError::ConnectionLost { retry_count } => {
                write!(f, "Connection lost after {} retries", retry_count)
            }
            ChainError::EventParseError { reason } => write!(f, "Event parse error: {}", reason),
            ChainError::SubscriptionError(msg) => write!(f, "Subscription error: {}", msg),
            ChainError::StewardCallFailed(msg) => write!(f, "Steward call failed: {}", msg),
            ChainError::VaultNotFound { index_id } => {
                write!(f, "Vault not found for index_id: {}", index_id)
            }
            ChainError::InvalidVaultAddress(msg) => write!(f, "Invalid vault address: {}", msg),
            ChainError::InvalidConfig(msg) => write!(f, "Invalid configuration: {}", msg),
        }
    }
}

impl std::error::Error for ChainError {}

impl From<tokio_tungstenite::tungstenite::Error> for ChainError {
    fn from(err: tokio_tungstenite::tungstenite::Error) -> Self {
        ChainError::WebSocketError(err.to_string())
    }
}
