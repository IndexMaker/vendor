
use eyre::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OrderSide {
    Buy,
    Sell,
}

impl std::fmt::Display for OrderSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrderSide::Buy => write!(f, "Buy"),
            OrderSide::Sell => write!(f, "Sell"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateOrderRequest {
    pub side: OrderSide,
    pub index_symbol: String,
    pub quantity: String,
    pub client_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateOrderResponse {
    pub success: bool,
    pub order_id: String,
    pub status: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub timestamp: String,
}

pub struct VendorClient {
    base_url: String,
    client: reqwest::Client,
}

impl VendorClient {
    pub fn new(base_url: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        Self { base_url, client }
    }

    /// Check vendor health
    pub async fn health_check(&self) -> Result<HealthResponse> {
        let url = format!("{}/health", self.base_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to send health check request")?;

        let health = response
            .json::<HealthResponse>()
            .await
            .context("Failed to parse health response")?;

        Ok(health)
    }

    /// Submit an order to the vendor
    pub async fn submit_order(&self, request: CreateOrderRequest) -> Result<CreateOrderResponse> {
        let url = format!("{}/api/v1/orders", self.base_url);

        tracing::debug!("Submitting order to {}: {:?}", url, request);

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .context("Failed to send order request")?;

        let status = response.status();
        let response_text = response
            .text()
            .await
            .context("Failed to read response body")?;

        if !status.is_success() {
            tracing::error!("Order submission failed: {} - {}", status, response_text);
            return Err(eyre::eyre!(
                "Order submission failed with status {}: {}",
                status,
                response_text
            ));
        }

        let order_response: CreateOrderResponse = serde_json::from_str(&response_text)
            .context(format!("Failed to parse order response: {}", response_text))?;

        Ok(order_response)
    }

    /// Get inventory summary from vendor
    pub async fn get_inventory(&self) -> Result<serde_json::Value> {
        let url = format!("{}/api/v1/inventory", self.base_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to send inventory request")?;

        let inventory = response
            .json::<serde_json::Value>()
            .await
            .context("Failed to parse inventory response")?;

        Ok(inventory)
    }
}