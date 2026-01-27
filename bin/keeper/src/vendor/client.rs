use super::types::{
    AssetsQuote, HealthResponse, QuoteAssetsRequest, UpdateMarketDataRequest,
    UpdateMarketDataResponse, VendorError, RefreshQuoteRequest, RefreshQuoteResponse,
};
use reqwest::Client;
use std::time::{Duration, Instant};

/// Configuration for update_market_data endpoint
#[derive(Clone, Debug)]
pub struct UpdateMarketDataConfig {
    pub endpoint: String,
    pub timeout_secs: u64,
    pub retry_attempts: u32,
}

impl Default for UpdateMarketDataConfig {
    fn default() -> Self {
        Self {
            endpoint: "/update-market-data".to_string(),
            timeout_secs: 30,
            retry_attempts: 3,
        }
    }
}

#[derive(Clone)]
#[allow(dead_code)] // timeout field retained for future use
pub struct VendorClient {
    client: Client,
    base_url: String,
    timeout: Duration,
    retry_attempts: u32,
    // Story 2.5: Separate config for update_market_data endpoint
    update_market_data_config: UpdateMarketDataConfig,
}

impl VendorClient {
    #[allow(dead_code)] // Used in tests
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
            update_market_data_config: UpdateMarketDataConfig::default(),
        }
    }

    /// Create VendorClient with custom update_market_data configuration (Story 2.5)
    pub fn with_update_market_data_config(
        base_url: String,
        timeout_secs: u64,
        retry_attempts: u32,
        update_market_data_config: UpdateMarketDataConfig,
    ) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            base_url,
            timeout: Duration::from_secs(timeout_secs),
            retry_attempts,
            update_market_data_config,
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

    // =========================================================================
    // Story 2.5: Update Market Data via Vendor
    // =========================================================================

    /// Send sorted asset list to Vendor for market data update
    ///
    /// This method sends a POST request to the Vendor's /update-market-data endpoint
    /// with the sorted unique asset IDs extracted from indices (via Stewart in Story 2.4).
    /// The Vendor will fetch Bitget order books, compute P/S/L vectors, and submit
    /// on-chain calls. Keeper MUST wait for HTTP 200 before proceeding to Castle/Vault
    /// settlement calls (Story 2.6).
    ///
    /// # Arguments
    /// * `request` - Contains sorted asset IDs and batch correlation ID
    ///
    /// # Returns
    /// * `Ok(UpdateMarketDataResponse)` - On HTTP 200 success
    /// * `Err(VendorError)` - On failure (timeout, non-200, retry exhausted)
    pub async fn update_market_data(
        &self,
        request: UpdateMarketDataRequest,
    ) -> Result<UpdateMarketDataResponse, VendorError> {
        let config = &self.update_market_data_config;
        let url = format!("{}{}", self.base_url, config.endpoint);
        let start_time = Instant::now();

        tracing::info!(
            batch_id = %request.batch_id,
            assets = request.assets.len(),
            "📤 Sending market data update request to Vendor"
        );

        let mut last_error: Option<VendorError> = None;

        // Retry with exponential backoff: 500ms * 2^(attempt-1)
        for attempt in 1..=config.retry_attempts {
            tracing::debug!(
                batch_id = %request.batch_id,
                attempt = attempt,
                max_attempts = config.retry_attempts,
                "Attempt {} to update market data",
                attempt
            );

            match self
                .try_update_market_data_request(&url, &request, config.timeout_secs)
                .await
            {
                Ok(response) => {
                    let elapsed = start_time.elapsed();
                    tracing::info!(
                        batch_id = %request.batch_id,
                        status = %response.status,
                        assets_updated = response.assets_updated,
                        duration_ms = elapsed.as_millis() as u64,
                        "✓ Vendor market data update complete"
                    );
                    return Ok(response);
                }
                Err(e) => {
                    let elapsed = start_time.elapsed();
                    tracing::warn!(
                        batch_id = %request.batch_id,
                        attempt = attempt,
                        error = %e,
                        duration_ms = elapsed.as_millis() as u64,
                        "Market data update attempt {} failed: {}",
                        attempt,
                        e
                    );
                    last_error = Some(e);

                    // Exponential backoff: 500ms, 1000ms, 2000ms...
                    if attempt < config.retry_attempts {
                        let delay_ms = 500 * (1u64 << (attempt - 1)); // 500 * 2^(attempt-1)
                        tracing::debug!(
                            batch_id = %request.batch_id,
                            delay_ms = delay_ms,
                            "Waiting {}ms before retry",
                            delay_ms
                        );
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    }
                }
            }
        }

        // All retries exhausted
        let final_error = match last_error {
            Some(e) => VendorError::RetryExhausted {
                attempts: config.retry_attempts,
                last_error: e.to_string(),
            },
            None => VendorError::RetryExhausted {
                attempts: config.retry_attempts,
                last_error: "Unknown error".to_string(),
            },
        };

        tracing::error!(
            batch_id = %request.batch_id,
            attempts = config.retry_attempts,
            "❌ All retry attempts exhausted for market data update"
        );

        Err(final_error)
    }

    // =========================================================================
    // Story 0-1 AC6: Refresh Index Quote
    // =========================================================================

    /// Refresh index quote via Vendor (AC6)
    ///
    /// Called when quote is stale (>5 min) before processing BuyOrder/SellOrder.
    /// Triggers updateIndexQuote on-chain via Vendor.
    ///
    /// # Arguments
    /// * `index_id` - Index ID to refresh quote for
    /// * `correlation_id` - Optional correlation ID for tracing
    ///
    /// # Returns
    /// * `Ok(RefreshQuoteResponse)` - On success
    /// * `Err(VendorError)` - On failure
    pub async fn refresh_quote(
        &self,
        index_id: u128,
        correlation_id: Option<String>,
    ) -> Result<RefreshQuoteResponse, VendorError> {
        let url = format!("{}/refresh-quote", self.base_url);
        let trace_id = correlation_id.clone().unwrap_or_else(|| format!("refresh-{}", index_id));

        tracing::info!(
            index_id,
            correlation_id = %trace_id,
            "📤 Requesting quote refresh from Vendor"
        );

        let request = RefreshQuoteRequest {
            index_id,
            correlation_id,
        };

        let mut last_error: Option<VendorError> = None;

        // Retry with backoff
        for attempt in 1..=self.retry_attempts {
            match self.try_refresh_quote_request(&url, &request).await {
                Ok(response) => {
                    if response.success {
                        tracing::info!(
                            index_id,
                            correlation_id = %trace_id,
                            "✓ Quote refresh complete"
                        );
                    } else {
                        tracing::warn!(
                            index_id,
                            correlation_id = %trace_id,
                            error = ?response.error,
                            "Quote refresh returned failure"
                        );
                    }
                    return Ok(response);
                }
                Err(e) => {
                    tracing::warn!(
                        index_id,
                        correlation_id = %trace_id,
                        attempt,
                        error = %e,
                        "Quote refresh attempt {} failed",
                        attempt
                    );
                    last_error = Some(e);

                    if attempt < self.retry_attempts {
                        let delay_ms = 500 * (1u64 << (attempt - 1));
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    }
                }
            }
        }

        let final_error = VendorError::RetryExhausted {
            attempts: self.retry_attempts,
            last_error: last_error.map(|e| e.to_string()).unwrap_or_else(|| "Unknown".to_string()),
        };

        tracing::error!(
            index_id,
            correlation_id = %trace_id,
            "❌ All quote refresh attempts failed"
        );

        Err(final_error)
    }

    /// Internal helper for single refresh_quote request attempt
    async fn try_refresh_quote_request(
        &self,
        url: &str,
        request: &RefreshQuoteRequest,
    ) -> Result<RefreshQuoteResponse, VendorError> {
        let response = self
            .client
            .post(url)
            .json(request)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    VendorError::Timeout {
                        url: url.to_string(),
                        timeout_secs: self.timeout.as_secs(),
                    }
                } else {
                    VendorError::RequestFailed {
                        url: url.to_string(),
                        reason: e.to_string(),
                    }
                }
            })?;

        let status = response.status();

        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read response body".to_string());
            return Err(VendorError::NonSuccessStatus {
                status: status.as_u16(),
                body,
            });
        }

        let result: RefreshQuoteResponse = response.json().await.map_err(|e| {
            VendorError::SerdeError(format!("Failed to parse refresh response: {}", e))
        })?;

        Ok(result)
    }

    /// Internal helper for single update_market_data request attempt
    async fn try_update_market_data_request(
        &self,
        url: &str,
        request: &UpdateMarketDataRequest,
        timeout_secs: u64,
    ) -> Result<UpdateMarketDataResponse, VendorError> {
        // Create client with specific timeout for this endpoint
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .map_err(|e| VendorError::RequestFailed {
                url: url.to_string(),
                reason: e.to_string(),
            })?;

        let response = client
            .post(url)
            .json(request)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    VendorError::Timeout {
                        url: url.to_string(),
                        timeout_secs,
                    }
                } else {
                    VendorError::RequestFailed {
                        url: url.to_string(),
                        reason: e.to_string(),
                    }
                }
            })?;

        let status = response.status();

        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read response body".to_string());
            return Err(VendorError::NonSuccessStatus {
                status: status.as_u16(),
                body,
            });
        }

        let result: UpdateMarketDataResponse = response.json().await.map_err(|e| {
            VendorError::SerdeError(format!("Failed to parse response: {}", e))
        })?;

        Ok(result)
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

    // =========================================================================
    // Story 2.5: Update Market Data Tests
    // =========================================================================

    /// Test 6.1: Successful request/response flow with mock server
    #[tokio::test]
    async fn test_update_market_data_success() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path, body_json};

        let mock_server = MockServer::start().await;

        let expected_request = UpdateMarketDataRequest {
            assets: vec![101, 102, 103],
            batch_id: "time_window:keeper:1705123456789".to_string(),
        };

        Mock::given(method("POST"))
            .and(path("/update-market-data"))
            .and(body_json(&expected_request))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "success",
                "assets_updated": 3,
                "batch_id": "time_window:keeper:1705123456789",
                "timestamp": 1705123460
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = VendorClient::new(mock_server.uri(), 10, 3);

        let response = client.update_market_data(expected_request).await.unwrap();

        assert_eq!(response.status, "success");
        assert_eq!(response.assets_updated, 3);
        assert_eq!(response.batch_id, "time_window:keeper:1705123456789");
        assert!(response.is_success());
    }

    /// Test 6.2: Timeout handling - verifies error type on timeout
    #[tokio::test]
    async fn test_update_market_data_timeout() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path};

        let mock_server = MockServer::start().await;

        // Response with 5 second delay, but timeout is 1 second
        Mock::given(method("POST"))
            .and(path("/update-market-data"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(std::time::Duration::from_secs(5))
                    .set_body_json(serde_json::json!({
                        "status": "success",
                        "assets_updated": 3,
                        "batch_id": "test",
                        "timestamp": 1705123460
                    }))
            )
            .expect(1..=3) // May be called 1-3 times depending on retries
            .mount(&mock_server)
            .await;

        let config = UpdateMarketDataConfig {
            endpoint: "/update-market-data".to_string(),
            timeout_secs: 1, // Very short timeout
            retry_attempts: 1, // Only 1 attempt to speed up test
        };
        let client = VendorClient::with_update_market_data_config(
            mock_server.uri(),
            10,
            3,
            config,
        );

        let request = UpdateMarketDataRequest {
            assets: vec![101],
            batch_id: "timeout_test".to_string(),
        };

        let result = client.update_market_data(request).await;
        assert!(result.is_err());

        match result.unwrap_err() {
            VendorError::RetryExhausted { attempts, last_error } => {
                assert_eq!(attempts, 1);
                assert!(last_error.contains("timed out") || last_error.contains("timeout"));
            }
            other => panic!("Expected RetryExhausted error, got: {:?}", other),
        }
    }

    /// Test 6.3: Retry logic with exponential backoff
    #[tokio::test]
    async fn test_update_market_data_retry_exponential_backoff() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path};
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let mock_server = MockServer::start().await;
        let request_count = Arc::new(AtomicU32::new(0));
        let request_count_clone = request_count.clone();

        // First 2 attempts fail with 500, third succeeds
        Mock::given(method("POST"))
            .and(path("/update-market-data"))
            .respond_with(move |_req: &wiremock::Request| {
                let count = request_count_clone.fetch_add(1, Ordering::SeqCst);
                if count < 2 {
                    ResponseTemplate::new(500).set_body_json(serde_json::json!({
                        "error": "temporary failure"
                    }))
                } else {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "status": "success",
                        "assets_updated": 3,
                        "batch_id": "retry_test",
                        "timestamp": 1705123460
                    }))
                }
            })
            .expect(3)
            .mount(&mock_server)
            .await;

        let client = VendorClient::new(mock_server.uri(), 10, 3);

        let request = UpdateMarketDataRequest {
            assets: vec![101, 102, 103],
            batch_id: "retry_test".to_string(),
        };

        let start = Instant::now();
        let response = client.update_market_data(request).await.unwrap();
        let elapsed = start.elapsed();

        assert_eq!(response.status, "success");
        assert_eq!(request_count.load(Ordering::SeqCst), 3); // 3 total attempts

        // Verify exponential backoff occurred (at least some delay for retries)
        // Using 1000ms as threshold to account for CI timing variance
        assert!(elapsed >= Duration::from_millis(1000), "Expected backoff delay, got {:?}", elapsed);
    }

    /// Test 6.4: Non-200 status handling
    #[tokio::test]
    async fn test_update_market_data_non_200_status() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path};

        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/update-market-data"))
            .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
                "status": "failed",
                "error": "Bitget API timeout",
                "batch_id": "error_test"
            })))
            .expect(3) // All retry attempts
            .mount(&mock_server)
            .await;

        let client = VendorClient::new(mock_server.uri(), 10, 3);

        let request = UpdateMarketDataRequest {
            assets: vec![101],
            batch_id: "error_test".to_string(),
        };

        let result = client.update_market_data(request).await;
        assert!(result.is_err());

        match result.unwrap_err() {
            VendorError::RetryExhausted { attempts, last_error } => {
                assert_eq!(attempts, 3);
                assert!(last_error.contains("HTTP 500"));
            }
            other => panic!("Expected RetryExhausted error, got: {:?}", other),
        }
    }

    /// Test 6.5: Correlation ID propagation
    #[tokio::test]
    async fn test_update_market_data_correlation_id_propagation() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path, body_json};

        let mock_server = MockServer::start().await;
        let correlation_id = "time_window:keeper:1705123456789";

        let expected_request = UpdateMarketDataRequest {
            assets: vec![101, 102, 103],
            batch_id: correlation_id.to_string(),
        };

        Mock::given(method("POST"))
            .and(path("/update-market-data"))
            .and(body_json(&expected_request))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "success",
                "assets_updated": 3,
                "batch_id": correlation_id,
                "timestamp": 1705123460
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = VendorClient::new(mock_server.uri(), 10, 3);

        let response = client.update_market_data(expected_request).await.unwrap();

        // Verify correlation ID is echoed back
        assert_eq!(response.batch_id, correlation_id);
    }

    /// Test 6.6: Integration test - partial success response
    #[tokio::test]
    async fn test_update_market_data_partial_success() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path};

        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/update-market-data"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "partial",
                "assets_updated": 2,
                "batch_id": "partial_test",
                "timestamp": 1705123460
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = VendorClient::new(mock_server.uri(), 10, 3);

        let request = UpdateMarketDataRequest {
            assets: vec![101, 102, 103],
            batch_id: "partial_test".to_string(),
        };

        let response = client.update_market_data(request).await.unwrap();

        assert_eq!(response.status, "partial");
        assert_eq!(response.assets_updated, 2);
        assert!(!response.is_success());
        assert!(response.is_partial());
    }

    /// Test: All retry attempts exhausted
    #[tokio::test]
    async fn test_update_market_data_all_retries_exhausted() {
        use wiremock::{MockServer, Mock, ResponseTemplate};
        use wiremock::matchers::{method, path};

        let mock_server = MockServer::start().await;

        // All requests fail
        Mock::given(method("POST"))
            .and(path("/update-market-data"))
            .respond_with(ResponseTemplate::new(503).set_body_string("Service Unavailable"))
            .expect(3)
            .mount(&mock_server)
            .await;

        let client = VendorClient::new(mock_server.uri(), 10, 3);

        let request = UpdateMarketDataRequest {
            assets: vec![101],
            batch_id: "exhausted_test".to_string(),
        };

        let result = client.update_market_data(request).await;
        assert!(result.is_err());

        match result.unwrap_err() {
            VendorError::RetryExhausted { attempts, .. } => {
                assert_eq!(attempts, 3);
            }
            other => panic!("Expected RetryExhausted error, got: {:?}", other),
        }
    }
}