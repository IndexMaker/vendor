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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockchainConfig {
    pub rpc_url: String,
    pub private_key: String,
    pub castle_address: Option<String>,
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
                },
            },
            blockchain: BlockchainConfig {
                rpc_url: "http://localhost:8547".to_string(),
                private_key: String::new(),
                castle_address: None,
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
        Self::default()
    }
}