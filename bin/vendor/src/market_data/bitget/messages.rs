use common::amount::Amount;
use eyre::{eyre, Result};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BitgetWsMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arg: Option<ChannelArg>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelArg {
    pub inst_type: String,
    pub channel: String,
    pub inst_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrderbookData {
    pub asks: Vec<Vec<String>>,
    pub bids: Vec<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<i32>,
    pub ts: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TickerData {
    pub inst_id: String,
    pub last_pr: String,
    pub bid_pr: String,
    pub ask_pr: String,
    pub bid_sz: String,
    pub ask_sz: String,
    pub ts: String,
}

#[derive(Debug, Clone)]
pub struct PriceLevel {
    pub price: Amount,
    pub quantity: Amount,
}

impl PriceLevel {
    pub fn from_strings(price_str: &str, qty_str: &str) -> Result<Self> {
        // Parse as f64 first, then convert to Amount
        let price_f64 = f64::from_str(price_str)
            .map_err(|e| eyre!("Failed to parse price '{}': {}", price_str, e))?;
        let qty_f64 = f64::from_str(qty_str)
            .map_err(|e| eyre!("Failed to parse quantity '{}': {}", qty_str, e))?;

        // Convert to Amount - assuming 18 decimals precision
        // We'll multiply by 10^18 to get the raw u128 value
        let price_scaled = (price_f64 * 1e18) as u128;
        let qty_scaled = (qty_f64 * 1e18) as u128;

        Ok(Self {
            price: Amount::from_u128_raw(price_scaled),
            quantity: Amount::from_u128_raw(qty_scaled),
        })
    }
}

impl OrderbookData {
    pub fn parse_levels(levels: &[Vec<String>]) -> Result<Vec<PriceLevel>> {
        levels
            .iter()
            .map(|level| {
                if level.len() != 2 {
                    return Err(eyre!("Invalid price level format: expected 2 elements"));
                }
                PriceLevel::from_strings(&level[0], &level[1])
            })
            .collect()
    }

    pub fn get_bids(&self) -> Result<Vec<PriceLevel>> {
        Self::parse_levels(&self.bids)
    }

    pub fn get_asks(&self) -> Result<Vec<PriceLevel>> {
        Self::parse_levels(&self.asks)
    }
}

impl TickerData {
    pub fn get_bid_price(&self) -> Result<Amount> {
        let price_f64 = f64::from_str(&self.bid_pr)?;
        Ok(Amount::from_u128_raw((price_f64 * 1e18) as u128))
    }

    pub fn get_ask_price(&self) -> Result<Amount> {
        let price_f64 = f64::from_str(&self.ask_pr)?;
        Ok(Amount::from_u128_raw((price_f64 * 1e18) as u128))
    }

    pub fn get_bid_size(&self) -> Result<Amount> {
        let size_f64 = f64::from_str(&self.bid_sz)?;
        Ok(Amount::from_u128_raw((size_f64 * 1e18) as u128))
    }

    pub fn get_ask_size(&self) -> Result<Amount> {
        let size_f64 = f64::from_str(&self.ask_sz)?;
        Ok(Amount::from_u128_raw((size_f64 * 1e18) as u128))
    }
}

// Subscription message
#[derive(Debug, Clone, Serialize)]
pub struct SubscribeMessage {
    pub op: String,
    pub args: Vec<SubscribeArg>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeArg {
    pub inst_type: String,
    pub channel: String,
    pub inst_id: String,
}

impl SubscribeMessage {
    pub fn new(symbol: &str) -> Self {
        Self {
            op: "subscribe".to_string(),
            args: vec![
                SubscribeArg {
                    inst_type: "SPOT".to_string(),
                    channel: "books".to_string(),
                    inst_id: symbol.to_uppercase(),
                },
                SubscribeArg {
                    inst_type: "SPOT".to_string(),
                    channel: "ticker".to_string(),
                    inst_id: symbol.to_uppercase(),
                },
            ],
        }
    }

    /// Create a batch subscribe message for multiple symbols
    /// Each symbol gets both "books" and "ticker" channels
    pub fn new_batch(symbols: &[String]) -> Self {
        let mut args = Vec::with_capacity(symbols.len() * 2);
        for symbol in symbols {
            args.push(SubscribeArg {
                inst_type: "SPOT".to_string(),
                channel: "books".to_string(),
                inst_id: symbol.to_uppercase(),
            });
            args.push(SubscribeArg {
                inst_type: "SPOT".to_string(),
                channel: "ticker".to_string(),
                inst_id: symbol.to_uppercase(),
            });
        }
        Self {
            op: "subscribe".to_string(),
            args,
        }
    }
}