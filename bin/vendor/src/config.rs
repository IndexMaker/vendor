use eyre::Result;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VendorConfig {
    pub market_data: MarketDataConfig,
    pub blockchain: BlockchainConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketDataConfig {
    pub bitget: BitgetConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitgetConfig {
    pub websocket_url: String,
    pub subscription_limit_rate: usize,
    pub stale_check_period_secs: u64,
    pub stale_timeout_secs: i64,
    pub heartbeat_interval_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockchainConfig {
    pub rpc_url: String,
    pub private_key: String,
    pub castle_address: Option<String>,
}

impl Default for VendorConfig {
    fn default() -> Self {
        Self {
            market_data: MarketDataConfig {
                bitget: BitgetConfig {
                    websocket_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
                    subscription_limit_rate: 240, // 240/hour
                    stale_check_period_secs: 10,
                    stale_timeout_secs: 30,
                    heartbeat_interval_secs: 30,
                },
            },
            blockchain: BlockchainConfig {
                rpc_url: "http://localhost:8547".to_string(),
                private_key: String::new(),
                castle_address: None,
            },
        }
    }
}

impl VendorConfig {
    pub fn from_file(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: VendorConfig = serde_json::from_str(&content)?;
        Ok(config)
    }

    pub fn from_env_and_args() -> Self {
        // Can be extended to read from environment variables
        Self::default()
    }
}