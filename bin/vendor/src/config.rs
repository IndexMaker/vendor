use common::amount::Amount;
use eyre::Result;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::onchain::StalenessConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VendorConfig {
    pub market_data: MarketDataConfig,
    pub blockchain: BlockchainConfig,
    pub margin: MarginConfig,
    pub staleness: StalenessConfig,
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
    /// Symbols per WebSocket connection (default: 50, recommended by Bitget for stability)
    /// For 637 assets this creates ~13 parallel connections
    #[serde(default)]
    pub symbols_per_connection: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockchainConfig {
    pub rpc_url: String,
    pub private_key: String,
    pub castle_address: Option<String>,
    pub vendor_id: u128,
}

// Add this new struct
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarginConfig {
    /// Minimum order size in USD
    pub min_order_size_usd: f64,
    
    /// Total exposure across all assets in USD
    pub total_exposure_usd: f64,
    
    /// Price change threshold to trigger margin re-submission (0.05 = 5%)
    pub price_change_threshold: f64,
    
    /// Time threshold to force margin re-submission (seconds)
    pub margin_update_timeout_secs: u64,
}

impl Default for MarginConfig {
    fn default() -> Self {
        Self {
            min_order_size_usd: 5.0,
            total_exposure_usd: 1000.0,
            price_change_threshold: 0.05,
            margin_update_timeout_secs: 3600,
        }
    }
}

impl MarginConfig {
    /// Calculate exposure per asset given total number of assets
    pub fn exposure_per_asset(&self, total_assets: usize) -> f64 {
        if total_assets == 0 {
            0.0
        } else {
            self.total_exposure_usd / total_assets as f64
        }
    }
    
    /// Calculate margin for a specific asset
    /// margin = exposure_per_asset / asset_price
    pub fn calculate_margin(&self, total_assets: usize, asset_price: Amount) -> Amount {
        let epa = self.exposure_per_asset(total_assets);
        let price_f64 = asset_price.to_u128_raw() as f64 / 1e18;
        
        if price_f64 == 0.0 {
            return Amount::ZERO;
        }
        
        let margin_f64 = epa / price_f64;
        Amount::from_u128_raw((margin_f64 * 1e18) as u128)
    }
    
    pub fn min_order_size(&self) -> Amount {
        Amount::from_u128_raw((self.min_order_size_usd * 1e18) as u128)
    }
    
    pub fn total_exposure(&self) -> Amount {
        Amount::from_u128_raw((self.total_exposure_usd * 1e18) as u128)
    }
}

impl Default for VendorConfig {
    fn default() -> Self {
        Self {
            market_data: MarketDataConfig {
                bitget: BitgetConfig {
                    websocket_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
                    subscription_limit_rate: 240,
                    stale_check_period_secs: 10,
                    stale_timeout_secs: 30,
                    heartbeat_interval_secs: 30,
                    symbols_per_connection: Some(50), // Bitget recommends < 50 for stability
                },
            },
            blockchain: BlockchainConfig {
                rpc_url: "http://localhost:8547".to_string(),
                private_key: String::new(),
                castle_address: None,
                vendor_id: 1,
            },
            margin: MarginConfig::default(),
            staleness: StalenessConfig::default(),
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
        let mut config = Self::default();

        // Override blockchain config from environment variables (global.env)
        // RPC URL: TESTNET_RPC or ORBIT_RPC_URL
        if let Ok(rpc_url) = std::env::var("TESTNET_RPC")
            .or_else(|_| std::env::var("ORBIT_RPC_URL"))
        {
            config.blockchain.rpc_url = rpc_url;
        }

        // Castle address
        if let Ok(castle_address) = std::env::var("CASTLE_ADDRESS") {
            config.blockchain.castle_address = Some(castle_address);
        }

        // Private key: NEW_VENDOR (preferred), TESTNET_PRIVATE_KEY, or DEPLOY_PRIVATE_KEY
        if let Ok(mut private_key) = std::env::var("NEW_VENDOR")
            .or_else(|_| std::env::var("TESTNET_PRIVATE_KEY"))
            .or_else(|_| std::env::var("DEPLOY_PRIVATE_KEY"))
        {
            // Ensure 0x prefix
            if !private_key.starts_with("0x") {
                private_key = format!("0x{}", private_key);
            }
            config.blockchain.private_key = private_key;
        }

        // Vendor ID
        if let Ok(vendor_id) = std::env::var("VENDOR_ID") {
            if let Ok(id) = vendor_id.parse() {
                config.blockchain.vendor_id = id;
            }
        }

        // Margin config from environment
        if let Ok(min_order) = std::env::var("MIN_ORDER_SIZE_USD") {
            if let Ok(v) = min_order.parse() {
                config.margin.min_order_size_usd = v;
            }
        }
        if let Ok(total_exp) = std::env::var("TOTAL_EXPOSURE_USD") {
            if let Ok(v) = total_exp.parse() {
                config.margin.total_exposure_usd = v;
            }
        }

        config
    }
}