use super::types::{AssetsQuote, HealthResponse, QuoteAssetsRequest};
use reqwest::Client;
use std::time::Duration;

#[derive(Clone)]
pub struct VendorClient {
    client: Client,
    base_url: String,
    timeout: Duration,
    retry_attempts: u32,
}

impl VendorClient {
    pub fn new(base_url: String, timeout_secs: u64, retry_attempts: u32) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            base_url,
            timeout: Duration::from_secs(timeout_secs),
            retry_attempts,
        }
    }

    /// Check vendor health
    pub async fn health_check(&self) -> eyre::Result<HealthResponse> {
        let url = format!("{}/health", self.base_url);
        
        tracing::debug!("Health check: {}", url);
        
        let response = self.client.get(&url).send().await?;
        
        if !response.status().is_success() {
            eyre::bail!("Health check failed: HTTP {}", response.status());
        }
        
        let health: HealthResponse = response.json().await?;
        
        tracing::info!(
            "Vendor health: {} (vendor_id={}, tracked_assets={})",
            health.status,
            health.vendor_id,
            health.tracked_assets
        );
        
        Ok(health)
    }

    /// Request quotes for specific assets
    pub async fn quote_assets(&self, asset_ids: Vec<u128>) -> eyre::Result<AssetsQuote> {
        let url = format!("{}/quote_assets", self.base_url);
        
        let request = QuoteAssetsRequest {
            assets: asset_ids.clone(),
        };
        
        tracing::debug!("Requesting quotes for {} assets", asset_ids.len());
        
        let mut last_error = None;
        
        // Retry logic
        for attempt in 1..=self.retry_attempts {
            match self.try_quote_request(&url, &request).await {
                Ok(quote) => {
                    tracing::info!(
                        "Received {} stale assets out of {} requested ({}% reduction)",
                        quote.len(),
                        request.assets.len(),
                        if request.assets.is_empty() {
                            0
                        } else {
                            100 - (quote.len() * 100 / request.assets.len())
                        }
                    );
                    return Ok(quote);
                }
                Err(e) => {
                    tracing::warn!("Quote request attempt {}/{} failed: {:?}", attempt, self.retry_attempts, e);
                    last_error = Some(e);
                    
                    if attempt < self.retry_attempts {
                        tokio::time::sleep(Duration::from_millis(500 * attempt as u64)).await;
                    }
                }
            }
        }
        
        Err(last_error.unwrap_or_else(|| eyre::eyre!("All retry attempts failed")))
    }

    async fn try_quote_request(
        &self,
        url: &str,
        request: &QuoteAssetsRequest,
    ) -> eyre::Result<AssetsQuote> {
        let response = self
            .client
            .post(url)
            .json(request)
            .send()
            .await?;
        
        if !response.status().is_success() {
            eyre::bail!("Quote request failed: HTTP {}", response.status());
        }
        
        let quote: AssetsQuote = response.json().await?;
        
        Ok(quote)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires running Vendor
    async fn test_vendor_client() {
        let client = VendorClient::new(
            "http://localhost:8080".to_string(),
            10,
            3,
        );

        // Health check
        let health = client.health_check().await.unwrap();
        assert_eq!(health.status, "healthy");

        // Quote request
        let quote = client.quote_assets(vec![101, 102, 103]).await.unwrap();
        println!("Received quote for {} assets", quote.len());
    }
}