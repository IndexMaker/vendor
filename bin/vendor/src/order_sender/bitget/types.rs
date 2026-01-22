use serde::{Deserialize, Serialize};

/// Bitget API response wrapper
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitgetResponse<T> {
    pub code: String,
    pub msg: String,
    pub data: T,
    #[serde(rename = "requestTime")]
    pub request_time: i64,
}

impl<T> BitgetResponse<T> {
    pub fn is_success(&self) -> bool {
        self.code == "00000"
    }
}

/// Account balance response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountAsset {
    #[serde(rename = "coin")]
    pub coin: String,
    #[serde(rename = "available")]
    pub available: String,
    #[serde(rename = "frozen")]
    pub frozen: String,
    #[serde(rename = "locked")]
    pub locked: String,
}

/// Orderbook ticker (best bid/ask)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderbookTicker {
    pub symbol: String,
    #[serde(rename = "bidPr")]
    pub bid_price: String,
    #[serde(rename = "bidSz")]
    pub bid_size: String,
    #[serde(rename = "askPr")]
    pub ask_price: String,
    #[serde(rename = "askSz")]
    pub ask_size: String,
    pub ts: String,
}

/// Place order request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceOrderRequest {
    pub symbol: String,
    pub side: String, // "buy" or "sell"
    #[serde(rename = "orderType")]
    pub order_type: String, // "limit" or "market"
    pub force: String, // "gtc", "post_only", "fok", "ioc"
    pub price: String,
    pub size: String, // quantity
    #[serde(rename = "clientOid", skip_serializing_if = "Option::is_none")]
    pub client_order_id: Option<String>,
}

/// Place order response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceOrderResponse {
    #[serde(rename = "orderId")]
    pub order_id: String,
    #[serde(rename = "clientOid")]
    pub client_order_id: String,
}

/// Order detail
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderDetail {
    #[serde(rename = "orderId")]
    pub order_id: String,
    #[serde(rename = "clientOid")]
    pub client_order_id: String,
    pub symbol: String,
    pub side: String,
    #[serde(rename = "orderType")]
    pub order_type: String,
    pub price: String,
    pub size: String,
    #[serde(rename = "baseVolume")]
    pub base_volume: String, // filled quantity
    #[serde(rename = "quoteVolume")]
    pub quote_volume: String, // filled notional
    /// Fee amount - may be in feeDetail field for some order types
    #[serde(default)]
    pub fee: String,
    /// Fee detail string (Bitget returns this for some order types instead of fee)
    #[serde(rename = "feeDetail", default)]
    pub fee_detail: String,
    /// Fee currency - optional, may not be present for cancelled orders
    #[serde(rename = "feeCcy", default)]
    pub fee_currency: String,
    /// Quote coin (e.g., USDC)
    #[serde(rename = "quoteCoin", default)]
    pub quote_coin: String,
    /// Base coin (e.g., BTC)
    #[serde(rename = "baseCoin", default)]
    pub base_coin: String,
    #[serde(rename = "priceAvg")]
    pub avg_price: String,
    pub status: String, // "new", "partial_fill", "full_fill", "cancelled"
    #[serde(rename = "cTime")]
    pub create_time: String,
    #[serde(rename = "uTime")]
    pub update_time: String,
}

impl OrderDetail {
    pub fn is_filled(&self) -> bool {
        self.status == "full_fill" || self.status == "filled"
    }

    pub fn is_partially_filled(&self) -> bool {
        self.status == "partial_fill"
    }

    pub fn is_pending(&self) -> bool {
        self.status == "new" || self.status == "partial_fill"
    }
}