use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeeperConfig {
    pub keeper_id: u128,
    pub vendor: VendorConfig,
    pub blockchain: BlockchainConfig,
    pub polling: PollingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VendorConfig {
    pub url: String,
    pub timeout_secs: u64,
    pub retry_attempts: u32,
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

impl KeeperConfig {
    pub async fn load_from_file(path: &Path) -> eyre::Result<Self> {
        let contents = tokio::fs::read_to_string(path).await?;
        let config: Self = serde_json::from_str(&contents)?;
        Ok(config)
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
        }
    }
}