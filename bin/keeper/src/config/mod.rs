use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeeperConfig {
    pub keeper_id: u128,
    pub vendor: VendorConfig,
    pub blockchain: BlockchainConfig,
    pub polling: PollingConfig,
    #[serde(default)]
    pub event_listener: EventListenerConfig,
    #[serde(default)]
    pub contracts: ContractsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VendorConfig {
    pub url: String,
    pub timeout_secs: u64,
    pub retry_attempts: u32,
    // Story 2.5: Update market data endpoint configuration
    #[serde(default = "default_update_market_data_endpoint")]
    pub update_market_data_endpoint: String,
    #[serde(default = "default_update_market_data_timeout_secs")]
    pub update_market_data_timeout_secs: u64,
    #[serde(default = "default_update_market_data_retry_attempts")]
    pub update_market_data_retry_attempts: u32,
}

fn default_update_market_data_endpoint() -> String {
    "/update-market-data".to_string()
}

fn default_update_market_data_timeout_secs() -> u64 {
    30
}

fn default_update_market_data_retry_attempts() -> u32 {
    3
}

impl VendorConfig {
    /// Apply environment variable overrides
    pub fn with_env_overrides(mut self) -> Self {
        // VENDOR_UPDATE_MARKET_DATA_TIMEOUT_SECS override
        if let Ok(val) = std::env::var("VENDOR_UPDATE_MARKET_DATA_TIMEOUT_SECS") {
            if let Ok(secs) = val.parse::<u64>() {
                self.update_market_data_timeout_secs = secs;
            }
        }
        // VENDOR_UPDATE_MARKET_DATA_RETRY_ATTEMPTS override
        if let Ok(val) = std::env::var("VENDOR_UPDATE_MARKET_DATA_RETRY_ATTEMPTS") {
            if let Ok(attempts) = val.parse::<u32>() {
                self.update_market_data_retry_attempts = attempts;
            }
        }
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockchainConfig {
    pub rpc_url: String,
    pub castle_address: String,
    pub vendor_id: u128,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollingConfig {
    pub interval_secs: u64,
    pub batch_window_ms: u64,
}

impl PollingConfig {
    /// Apply environment variable overrides
    pub fn with_env_overrides(mut self) -> Self {
        // KEEPER_AGGREGATION_WINDOW_MS overrides batch_window_ms
        if let Ok(val) = std::env::var("KEEPER_AGGREGATION_WINDOW_MS") {
            if let Ok(ms) = val.parse::<u64>() {
                self.batch_window_ms = ms;
            }
        }
        self
    }
}

/// Event listener configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventListenerConfig {
    /// Mode: "websocket", "polling", or "auto"
    #[serde(default = "default_mode")]
    pub mode: String,
    /// WebSocket URL (optional, derived from rpc_url if not set)
    pub ws_url: Option<String>,
    /// Polling interval in milliseconds
    #[serde(default = "default_polling_interval")]
    pub polling_interval_ms: u64,
    /// Maximum reconnection delay in milliseconds
    #[serde(default = "default_reconnect_delay")]
    pub reconnect_max_delay_ms: u64,
    /// Enable Stewart contract for vault discovery
    #[serde(default = "default_true")]
    pub use_stewart_discovery: bool,
}

fn default_mode() -> String {
    "auto".to_string()
}

fn default_polling_interval() -> u64 {
    3000
}

fn default_reconnect_delay() -> u64 {
    30000
}

fn default_true() -> bool {
    true
}

impl Default for EventListenerConfig {
    fn default() -> Self {
        Self {
            mode: "auto".to_string(),
            ws_url: None,
            polling_interval_ms: 3000,
            reconnect_max_delay_ms: 30000,
            use_stewart_discovery: true,
        }
    }
}

/// Contract addresses configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractsConfig {
    /// Vault orders contract address (for BuyOrder/SellOrder events)
    #[serde(default)]
    pub vault_orders: String,
    /// Stewart contract address (for vault discovery)
    #[serde(default)]
    pub steward: String,
}

impl Default for ContractsConfig {
    fn default() -> Self {
        Self {
            vault_orders: "0x4d856a5b7529edfd15ffaa7a36d2c7cfd52ac598".to_string(),
            steward: "0xd96ef47747e1eb4baccb56f28a71795570aa4e01".to_string(),
        }
    }
}

impl KeeperConfig {
    pub async fn load_from_file(path: &Path) -> eyre::Result<Self> {
        let contents = tokio::fs::read_to_string(path).await?;
        let config: Self = serde_json::from_str(&contents)?;
        Ok(config)
    }
}

// =============================================================================
// Story 2.8: Claim handling configuration
// =============================================================================

/// Configuration for event-driven claim handling
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimHandlerConfig {
    /// Threshold for claim remainder (claims with remain <= threshold are skipped)
    /// Default: 100 units (from Conveyor reference implementation)
    #[serde(default = "default_claim_threshold")]
    pub claim_remainder_threshold: u128,
    /// Enable event-driven claim handling
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_claim_threshold() -> u128 {
    100
}

impl ClaimHandlerConfig {
    /// Apply environment variable overrides
    pub fn with_env_overrides(mut self) -> Self {
        // CLAIM_REMAINDER_THRESHOLD override
        if let Ok(val) = std::env::var("CLAIM_REMAINDER_THRESHOLD") {
            if let Ok(threshold) = val.parse::<u128>() {
                self.claim_remainder_threshold = threshold;
            }
        }
        // CLAIM_HANDLER_ENABLED override
        if let Ok(val) = std::env::var("CLAIM_HANDLER_ENABLED") {
            self.enabled = val.to_lowercase() == "true" || val == "1";
        }
        self
    }
}

impl Default for ClaimHandlerConfig {
    fn default() -> Self {
        Self {
            claim_remainder_threshold: 100,
            enabled: true,
        }
    }
}

impl Default for KeeperConfig {
    fn default() -> Self {
        Self {
            keeper_id: 1,
            vendor: VendorConfig {
                url: "http://localhost:8080".to_string(),
                timeout_secs: 10,
                retry_attempts: 3,
                update_market_data_endpoint: "/update-market-data".to_string(),
                update_market_data_timeout_secs: 30,
                update_market_data_retry_attempts: 3,
            },
            blockchain: BlockchainConfig {
                rpc_url: "http://localhost:8545".to_string(),
                castle_address: "0x0000000000000000000000000000000000000000".to_string(),
                vendor_id: 1,
                dry_run: true,
            },
            polling: PollingConfig {
                interval_secs: 5,
                batch_window_ms: 500,
            },
            event_listener: EventListenerConfig::default(),
            contracts: ContractsConfig::default(),
        }
    }
}