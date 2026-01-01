use super::auth::BitgetAuth;
use super::types::*;
use eyre::{Context, Result};
use reqwest::Client;
use serde::de::DeserializeOwned;
use std::time::Duration;

pub struct BitgetClient {
    base_url: String,
    auth: BitgetAuth,
    client: Client,
}

impl BitgetClient {
    pub fn new(api_key: String, api_secret: String, passphrase: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            base_url: "https://api.bitget.com".to_string(),
            auth: BitgetAuth::new(api_key, api_secret, passphrase),
            client,
        }
    }

    /// Make authenticated GET request
    async fn get<T: DeserializeOwned>(&self, endpoint: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, endpoint);
        let timestamp = BitgetAuth::get_timestamp();
        let headers = self.auth.build_headers(&timestamp, "GET", endpoint, "");

        tracing::debug!("GET {}", url);

        let mut request = self.client.get(&url);
        for (key, value) in headers {
            request = request.header(key, value);
        }

        let response = request.send().await.context("Failed to send GET request")?;

        let status = response.status();
        let body = response
            .text()
            .await
            .context("Failed to read response body")?;

        if !status.is_success() {
            tracing::error!("Bitget API error: {} - {}", status, body);
            return Err(eyre::eyre!("Bitget API error: {}", body));
        }

        let parsed: BitgetResponse<T> =
            serde_json::from_str(&body).context(format!("Failed to parse response: {}", body))?;

        if !parsed.is_success() {
            return Err(eyre::eyre!(
                "Bitget API error: code={}, msg={}",
                parsed.code,
                parsed.msg
            ));
        }

        Ok(parsed.data)
    }

    /// Make authenticated POST request
    async fn post<T: DeserializeOwned, B: serde::Serialize>(
        &self,
        endpoint: &str,
        body: &B,
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, endpoint);
        let timestamp = BitgetAuth::get_timestamp();
        let body_str = serde_json::to_string(body).context("Failed to serialize request body")?;
        let headers = self
            .auth
            .build_headers(&timestamp, "POST", endpoint, &body_str);

        tracing::debug!("POST {} - Body: {}", url, body_str);

        let mut request = self.client.post(&url);
        for (key, value) in headers {
            request = request.header(key, value);
        }
        request = request.body(body_str);

        let response = request
            .send()
            .await
            .context("Failed to send POST request")?;

        let status = response.status();
        let response_body = response
            .text()
            .await
            .context("Failed to read response body")?;

        if !status.is_success() {
            tracing::error!("Bitget API error: {} - {}", status, response_body);
            return Err(eyre::eyre!("Bitget API error: {}", response_body));
        }

        let parsed: BitgetResponse<T> = serde_json::from_str(&response_body)
            .context(format!("Failed to parse response: {}", response_body))?;

        if !parsed.is_success() {
            return Err(eyre::eyre!(
                "Bitget API error: code={}, msg={}",
                parsed.code,
                parsed.msg
            ));
        }

        Ok(parsed.data)
    }

    /// Get account balances
    pub async fn get_balances(&self) -> Result<Vec<AccountAsset>> {
        self.get("/api/v2/spot/account/assets").await
    }

    /// Get orderbook ticker (best bid/ask)
    pub async fn get_orderbook_ticker(&self, symbol: &str) -> Result<OrderbookTicker> {
        let endpoint = format!("/api/v2/spot/market/tickers?symbol={}", symbol);
        let tickers: Vec<OrderbookTicker> = self.get(&endpoint).await?;

        tickers
            .into_iter()
            .find(|t| t.symbol == symbol)
            .ok_or_else(|| eyre::eyre!("Symbol {} not found in ticker response", symbol))
    }

    /// Place a limit order
    pub async fn place_limit_order(
        &self,
        symbol: &str,
        side: &str, // "buy" or "sell"
        quantity: &str,
        price: &str,
        client_order_id: Option<String>,
    ) -> Result<PlaceOrderResponse> {
        let request = PlaceOrderRequest {
            symbol: symbol.to_string(),
            side: side.to_string(),
            order_type: "limit".to_string(),
            force: "gtc".to_string(), // Good till cancelled
            price: price.to_string(),
            size: quantity.to_string(),
            client_order_id,
        };

        self.post("/api/v2/spot/trade/place-order", &request)
            .await
    }

    /// Place a market order
    pub async fn place_market_order(
        &self,
        symbol: &str,
        side: &str,
        quantity: &str,
        client_order_id: Option<String>,
    ) -> Result<PlaceOrderResponse> {
        let request = PlaceOrderRequest {
            symbol: symbol.to_string(),
            side: side.to_string(),
            order_type: "market".to_string(),
            force: "ioc".to_string(), // Immediate or cancel
            price: "0".to_string(),   // Not used for market orders
            size: quantity.to_string(),
            client_order_id,
        };

        self.post("/api/v2/spot/trade/place-order", &request)
            .await
    }

    /// Get order details
    pub async fn get_order(&self, order_id: &str) -> Result<OrderDetail> {
        let endpoint = format!("/api/v2/spot/trade/orderInfo?orderId={}", order_id);
        let orders: Vec<OrderDetail> = self.get(&endpoint).await?;

        orders
            .into_iter()
            .next()
            .ok_or_else(|| eyre::eyre!("Order {} not found", order_id))
    }

    /// Wait for order to be filled (with timeout)
    pub async fn wait_for_fill(
        &self,
        order_id: &str,
        timeout: Duration,
    ) -> Result<OrderDetail> {
        let start = std::time::Instant::now();
        let check_interval = Duration::from_millis(500);

        loop {
            let order = self.get_order(order_id).await?;

            if order.is_filled() {
                return Ok(order);
            }

            if start.elapsed() > timeout {
                return Err(eyre::eyre!(
                    "Order {} timeout after {:?}, status: {}",
                    order_id,
                    timeout,
                    order.status
                ));
            }

            tokio::time::sleep(check_interval).await;
        }
    }
}