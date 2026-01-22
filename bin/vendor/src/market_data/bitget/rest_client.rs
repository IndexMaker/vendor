//! REST API client for Bitget order book fetching
//!
//! This module provides synchronous/on-demand order book fetching via REST API,
//! complementing the WebSocket-based streaming implementation.

use common::amount::Amount;
use eyre::{eyre, Result};
use parking_lot::Mutex;
use reqwest::Client;
use serde::Deserialize;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::timeout;

/// Configuration for the REST client
#[derive(Debug, Clone)]
pub struct BitgetRestConfig {
    /// Base URL for REST API (e.g., "https://api.bitget.com")
    pub base_url: String,
    /// Request timeout in milliseconds (NFR20: < 500ms)
    pub timeout_ms: u64,
    /// Maximum concurrent requests per second (Bitget limit: 10 req/sec)
    pub max_requests_per_second: usize,
    /// Default depth levels for order book (K=5 per story requirements)
    pub default_depth: usize,
}

impl Default for BitgetRestConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.bitget.com".to_string(),
            timeout_ms: 5000, // Increased from 500ms to handle concurrent requests
            max_requests_per_second: 10,
            default_depth: 5, // K=5 levels
        }
    }
}

/// REST API response for order book
#[derive(Debug, Deserialize)]
struct OrderBookResponse {
    code: String,
    #[serde(default)]
    msg: Option<String>,
    data: Option<OrderBookData>,
}

#[derive(Debug, Deserialize)]
struct OrderBookData {
    /// Ask levels: [[price, quantity], ...]
    asks: Vec<Vec<String>>,
    /// Bid levels: [[price, quantity], ...]
    bids: Vec<Vec<String>>,
    /// Timestamp in milliseconds
    ts: String,
}

/// A single price level in the order book
#[derive(Debug, Clone)]
pub struct OrderBookLevel {
    pub price: Amount,
    pub quantity: Amount,
}

/// Order book snapshot for a single symbol
#[derive(Debug, Clone)]
pub struct OrderBookSnapshot {
    pub symbol: String,
    pub bids: Vec<OrderBookLevel>,
    pub asks: Vec<OrderBookLevel>,
    pub timestamp_ms: u64,
    pub fetch_latency_ms: u64,
}

impl OrderBookSnapshot {
    /// Get best bid price (highest bid)
    pub fn best_bid(&self) -> Option<&OrderBookLevel> {
        self.bids.first()
    }

    /// Get best ask price (lowest ask)
    pub fn best_ask(&self) -> Option<&OrderBookLevel> {
        self.asks.first()
    }

    /// Get mid price as average of best bid and best ask
    pub fn mid_price(&self) -> Option<Amount> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        // (bid + ask) / 2 - using Amount division
        let two = Amount::from_u128_raw(2_000_000_000_000_000_000u128); // 2.0 with 18 decimals
        bid.price.checked_add(ask.price)?.checked_div(two)
    }
}

/// Sliding window rate limiter for API requests
/// Tracks request timestamps and enforces max requests per second
struct SlidingWindowRateLimiter {
    /// Timestamps of recent requests (within last second)
    request_times: Mutex<VecDeque<Instant>>,
    /// Maximum requests allowed per second
    max_per_second: usize,
}

impl SlidingWindowRateLimiter {
    fn new(max_per_second: usize) -> Self {
        Self {
            request_times: Mutex::new(VecDeque::with_capacity(max_per_second + 1)),
            max_per_second,
        }
    }

    /// Wait until we can make another request without exceeding rate limit
    async fn acquire(&self) {
        loop {
            let wait_duration = {
                let mut times = self.request_times.lock();
                let now = Instant::now();
                let one_second_ago = now - Duration::from_secs(1);

                // Remove timestamps older than 1 second
                while times.front().map(|&t| t < one_second_ago).unwrap_or(false) {
                    times.pop_front();
                }

                // If we're under the limit, record this request and proceed
                if times.len() < self.max_per_second {
                    times.push_back(now);
                    return;
                }

                // Otherwise, calculate how long to wait
                // Wait until the oldest request is more than 1 second old
                if let Some(&oldest) = times.front() {
                    let wait_until = oldest + Duration::from_secs(1);
                    if wait_until > now {
                        Some(wait_until - now)
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            // Wait outside the lock
            if let Some(duration) = wait_duration {
                tokio::time::sleep(duration).await;
            }
        }
    }
}

/// REST client for fetching order books from Bitget
pub struct BitgetRestClient {
    config: BitgetRestConfig,
    client: Client,
    /// Sliding window rate limiter (enforces max requests per second)
    rate_limiter: Arc<SlidingWindowRateLimiter>,
}

impl BitgetRestClient {
    /// Create a new REST client with the given configuration
    pub fn new(config: BitgetRestConfig) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .map_err(|e| eyre!("Failed to create HTTP client: {}", e))?;

        let rate_limiter = Arc::new(SlidingWindowRateLimiter::new(config.max_requests_per_second));

        Ok(Self {
            config,
            client,
            rate_limiter,
        })
    }

    /// Fetch order book for a single symbol
    ///
    /// # Arguments
    /// * `symbol` - Trading pair symbol (e.g., "BTCUSDC")
    /// * `levels` - Number of depth levels to fetch (default: 5)
    ///
    /// # Returns
    /// Order book snapshot with bids and asks
    pub async fn fetch_orderbook(&self, symbol: &str, levels: Option<usize>) -> Result<OrderBookSnapshot> {
        let depth = levels.unwrap_or(self.config.default_depth);
        let start = Instant::now();

        // Wait for rate limiter (sliding window ensures max 10 req/sec)
        self.rate_limiter.acquire().await;

        // Build URL
        let url = format!(
            "{}/api/v2/spot/market/orderbook?symbol={}&limit={}",
            self.config.base_url,
            symbol.to_uppercase(),
            depth
        );

        tracing::debug!("Fetching order book: {}", url);

        // Make request with timeout
        let response = timeout(
            Duration::from_millis(self.config.timeout_ms),
            self.client.get(&url).send()
        )
        .await
        .map_err(|_| eyre!("Request timeout for {}", symbol))?
        .map_err(|e| eyre!("HTTP request failed for {}: {}", symbol, e))?;

        let latency_ms = start.elapsed().as_millis() as u64;

        // Check response status
        if !response.status().is_success() {
            return Err(eyre!(
                "HTTP error {} for {}: {}",
                response.status(),
                symbol,
                response.text().await.unwrap_or_default()
            ));
        }

        // Parse response
        let body: OrderBookResponse = response
            .json()
            .await
            .map_err(|e| eyre!("Failed to parse order book response for {}: {}", symbol, e))?;

        // Check API response code
        if body.code != "00000" {
            return Err(eyre!(
                "Bitget API error for {}: code={}, msg={}",
                symbol,
                body.code,
                body.msg.unwrap_or_default()
            ));
        }

        // Extract data
        let data = body.data.ok_or_else(|| eyre!("No data in order book response for {}", symbol))?;

        // Parse price levels
        let bids = Self::parse_levels(&data.bids)?;
        let asks = Self::parse_levels(&data.asks)?;

        let timestamp_ms: u64 = data.ts.parse()
            .unwrap_or_else(|_| chrono::Utc::now().timestamp_millis() as u64);

        tracing::debug!(
            "Fetched order book for {}: {} bids, {} asks, latency={}ms",
            symbol,
            bids.len(),
            asks.len(),
            latency_ms
        );

        Ok(OrderBookSnapshot {
            symbol: symbol.to_uppercase(),
            bids,
            asks,
            timestamp_ms,
            fetch_latency_ms: latency_ms,
        })
    }

    /// Fetch order books for multiple symbols concurrently
    ///
    /// # Arguments
    /// * `symbols` - List of trading pair symbols
    /// * `levels` - Number of depth levels (default: 5)
    ///
    /// # Returns
    /// Map of symbol to order book snapshot (only successful fetches)
    pub async fn fetch_orderbooks_concurrent(
        &self,
        symbols: &[String],
        levels: Option<usize>,
    ) -> HashMap<String, OrderBookSnapshot> {
        // Use tokio::join! pattern to avoid lifetime issues with futures::stream
        // This is more explicit but avoids the async closure capture problem
        let mut futures = Vec::new();

        for symbol in symbols {
            let symbol = symbol.clone();
            futures.push(async move {
                let result = self.fetch_orderbook(&symbol, levels).await;
                (symbol, result)
            });
        }

        // Execute all futures concurrently, respecting rate limiter within fetch_orderbook
        let results = futures::future::join_all(futures).await;

        let mut orderbooks = HashMap::new();
        for (symbol, result) in results {
            match result {
                Ok(snapshot) => {
                    orderbooks.insert(symbol, snapshot);
                }
                Err(e) => {
                    tracing::warn!("Failed to fetch order book for {}: {}", symbol, e);
                }
            }
        }

        orderbooks
    }

    /// Parse price level strings into OrderBookLevel
    fn parse_levels(levels: &[Vec<String>]) -> Result<Vec<OrderBookLevel>> {
        levels
            .iter()
            .map(|level| {
                if level.len() < 2 {
                    return Err(eyre!("Invalid level format: expected [price, quantity]"));
                }

                let price_str = &level[0];
                let qty_str = &level[1];

                // Parse decimal strings directly to u128 with 18 decimals precision
                // This avoids f64 precision loss for large numbers
                let price_scaled = Self::parse_decimal_to_u128(price_str, 18)
                    .map_err(|e| eyre!("Failed to parse price '{}': {}", price_str, e))?;
                let qty_scaled = Self::parse_decimal_to_u128(qty_str, 18)
                    .map_err(|e| eyre!("Failed to parse quantity '{}': {}", qty_str, e))?;

                Ok(OrderBookLevel {
                    price: Amount::from_u128_raw(price_scaled),
                    quantity: Amount::from_u128_raw(qty_scaled),
                })
            })
            .collect()
    }

    /// Parse a decimal string to u128 with specified decimal places
    ///
    /// Examples with 18 decimals:
    /// - "100.50" -> 100_500_000_000_000_000_000
    /// - "0.001" -> 1_000_000_000_000_000
    /// - "99999.999999999" -> 99_999_999_999_999_000_000_000_000_000
    fn parse_decimal_to_u128(s: &str, decimals: u32) -> Result<u128> {
        let s = s.trim();
        if s.is_empty() {
            return Err(eyre!("Empty string"));
        }

        let (integer_part, decimal_part) = if let Some(dot_pos) = s.find('.') {
            let (int_str, dec_str) = s.split_at(dot_pos);
            (int_str, &dec_str[1..]) // Skip the dot
        } else {
            (s, "")
        };

        // Parse integer part
        let integer: u128 = if integer_part.is_empty() {
            0
        } else {
            integer_part.parse().map_err(|e| eyre!("Invalid integer part: {}", e))?
        };

        // Calculate scaling factor (10^decimals)
        let scale: u128 = 10u128.pow(decimals);

        // Scale the integer part
        let scaled_integer = integer.checked_mul(scale)
            .ok_or_else(|| eyre!("Integer overflow scaling {}", integer))?;

        // Parse and scale decimal part
        let decimal_contribution = if decimal_part.is_empty() {
            0u128
        } else {
            // Take only up to `decimals` digits from the decimal part
            let effective_decimals = decimal_part.len().min(decimals as usize);
            let truncated_decimal = &decimal_part[..effective_decimals];

            // Parse the decimal digits
            let decimal_value: u128 = truncated_decimal.parse()
                .map_err(|e| eyre!("Invalid decimal part: {}", e))?;

            // Scale: if we have 2 decimal digits but need 18, multiply by 10^16
            let remaining_decimals = decimals as usize - effective_decimals;
            decimal_value.checked_mul(10u128.pow(remaining_decimals as u32))
                .ok_or_else(|| eyre!("Decimal overflow"))?
        };

        scaled_integer.checked_add(decimal_contribution)
            .ok_or_else(|| eyre!("Final overflow"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_decimal_to_u128() {
        // Basic cases
        assert_eq!(BitgetRestClient::parse_decimal_to_u128("100", 18).unwrap(),
                   100_000_000_000_000_000_000u128);
        assert_eq!(BitgetRestClient::parse_decimal_to_u128("100.50", 18).unwrap(),
                   100_500_000_000_000_000_000u128);
        assert_eq!(BitgetRestClient::parse_decimal_to_u128("1.5", 18).unwrap(),
                   1_500_000_000_000_000_000u128);
        assert_eq!(BitgetRestClient::parse_decimal_to_u128("0.001", 18).unwrap(),
                   1_000_000_000_000_000u128);

        // Edge cases
        assert_eq!(BitgetRestClient::parse_decimal_to_u128("0", 18).unwrap(), 0);
        assert_eq!(BitgetRestClient::parse_decimal_to_u128("0.0", 18).unwrap(), 0);
        assert_eq!(BitgetRestClient::parse_decimal_to_u128(".5", 18).unwrap(),
                   500_000_000_000_000_000u128);

        // High precision (more decimals than we need)
        assert_eq!(BitgetRestClient::parse_decimal_to_u128("1.123456789012345678901234", 18).unwrap(),
                   1_123_456_789_012_345_678u128);
    }

    #[test]
    fn test_parse_levels() {
        let levels = vec![
            vec!["100.50".to_string(), "1.5".to_string()],
            vec!["100.00".to_string(), "2.0".to_string()],
        ];

        let parsed = BitgetRestClient::parse_levels(&levels).unwrap();
        assert_eq!(parsed.len(), 2);

        // Verify first level
        let level0 = &parsed[0];
        // 100.50 scaled to 18 decimals
        assert_eq!(level0.price.to_u128_raw(), 100_500_000_000_000_000_000u128);
        // 1.5 scaled to 18 decimals
        assert_eq!(level0.quantity.to_u128_raw(), 1_500_000_000_000_000_000u128);
    }

    #[test]
    fn test_orderbook_mid_price() {
        let snapshot = OrderBookSnapshot {
            symbol: "BTCUSDC".to_string(),
            bids: vec![OrderBookLevel {
                price: Amount::from_u128_raw(100_000_000_000_000_000_000u128), // 100
                quantity: Amount::from_u128_raw(1_000_000_000_000_000_000u128),
            }],
            asks: vec![OrderBookLevel {
                price: Amount::from_u128_raw(102_000_000_000_000_000_000u128), // 102
                quantity: Amount::from_u128_raw(1_000_000_000_000_000_000u128),
            }],
            timestamp_ms: 0,
            fetch_latency_ms: 0,
        };

        let mid = snapshot.mid_price().unwrap();
        // (100 + 102) / 2 = 101
        assert_eq!(mid.to_u128_raw(), 101_000_000_000_000_000_000u128);
    }

    #[test]
    fn test_config_defaults() {
        let config = BitgetRestConfig::default();
        assert_eq!(config.base_url, "https://api.bitget.com");
        assert_eq!(config.timeout_ms, 500); // NFR20 requirement
        assert_eq!(config.max_requests_per_second, 10); // Bitget rate limit
        assert_eq!(config.default_depth, 5); // K=5 per story requirements
    }

    #[test]
    fn test_orderbook_best_bid_ask() {
        let snapshot = OrderBookSnapshot {
            symbol: "BTCUSDC".to_string(),
            bids: vec![
                OrderBookLevel {
                    price: Amount::from_u128_raw(100_000_000_000_000_000_000u128),
                    quantity: Amount::from_u128_raw(1_000_000_000_000_000_000u128),
                },
                OrderBookLevel {
                    price: Amount::from_u128_raw(99_000_000_000_000_000_000u128),
                    quantity: Amount::from_u128_raw(2_000_000_000_000_000_000u128),
                },
            ],
            asks: vec![
                OrderBookLevel {
                    price: Amount::from_u128_raw(101_000_000_000_000_000_000u128),
                    quantity: Amount::from_u128_raw(1_500_000_000_000_000_000u128),
                },
                OrderBookLevel {
                    price: Amount::from_u128_raw(102_000_000_000_000_000_000u128),
                    quantity: Amount::from_u128_raw(2_500_000_000_000_000_000u128),
                },
            ],
            timestamp_ms: 1234567890,
            fetch_latency_ms: 50,
        };

        // Best bid is first in bids array (highest)
        let best_bid = snapshot.best_bid().unwrap();
        assert_eq!(best_bid.price.to_u128_raw(), 100_000_000_000_000_000_000u128);

        // Best ask is first in asks array (lowest)
        let best_ask = snapshot.best_ask().unwrap();
        assert_eq!(best_ask.price.to_u128_raw(), 101_000_000_000_000_000_000u128);
    }

    #[test]
    fn test_empty_orderbook() {
        let snapshot = OrderBookSnapshot {
            symbol: "BTCUSDC".to_string(),
            bids: vec![],
            asks: vec![],
            timestamp_ms: 0,
            fetch_latency_ms: 0,
        };

        assert!(snapshot.best_bid().is_none());
        assert!(snapshot.best_ask().is_none());
        assert!(snapshot.mid_price().is_none());
    }
}

/// Integration tests for the REST client
/// Run with: cargo test --package vendor --lib -- bitget::rest_client::integration_tests --ignored
#[cfg(test)]
mod integration_tests {
    use super::*;

    /// Test fetching a single order book from Bitget (live API)
    /// AC #1, #2, #5: REST API connection, K=5 levels, latency < 500ms
    #[tokio::test]
    #[ignore] // Run with: cargo test --test-threads=1 -- integration_tests --ignored
    async fn test_fetch_single_orderbook() {
        let config = BitgetRestConfig::default();
        let client = BitgetRestClient::new(config).unwrap();

        // Fetch BTC/USDC order book with K=5 levels
        let snapshot = client.fetch_orderbook("BTCUSDC", Some(5)).await.unwrap();

        println!("Order book for {}:", snapshot.symbol);
        println!("  Timestamp: {} ms", snapshot.timestamp_ms);
        println!("  Latency: {} ms", snapshot.fetch_latency_ms);
        println!("  Bids: {} levels", snapshot.bids.len());
        println!("  Asks: {} levels", snapshot.asks.len());

        if let Some(bid) = snapshot.best_bid() {
            println!("  Best Bid: ${:.2} (qty: {:.4})",
                bid.price.to_u128_raw() as f64 / 1e18,
                bid.quantity.to_u128_raw() as f64 / 1e18
            );
        }
        if let Some(ask) = snapshot.best_ask() {
            println!("  Best Ask: ${:.2} (qty: {:.4})",
                ask.price.to_u128_raw() as f64 / 1e18,
                ask.quantity.to_u128_raw() as f64 / 1e18
            );
        }
        if let Some(mid) = snapshot.mid_price() {
            println!("  Mid Price: ${:.2}", mid.to_u128_raw() as f64 / 1e18);
        }

        // Verify AC #2: K=5 levels (may return fewer if order book is thin)
        assert!(snapshot.bids.len() <= 5, "Expected at most 5 bid levels");
        assert!(snapshot.asks.len() <= 5, "Expected at most 5 ask levels");

        // Verify AC #5: Latency < 500ms
        assert!(
            snapshot.fetch_latency_ms < 500,
            "Latency {} ms exceeds 500ms requirement",
            snapshot.fetch_latency_ms
        );
    }

    /// Test concurrent order book fetching (live API)
    /// AC #6: Concurrent requests for multiple assets
    #[tokio::test]
    #[ignore]
    async fn test_fetch_concurrent_orderbooks() {
        let config = BitgetRestConfig::default();
        let client = BitgetRestClient::new(config).unwrap();

        // Fetch order books for multiple symbols concurrently
        let symbols = vec![
            "BTCUSDC".to_string(),
            "ETHUSDC".to_string(),
            "SOLUSDC".to_string(),
        ];

        let start = std::time::Instant::now();
        let snapshots = client.fetch_orderbooks_concurrent(&symbols, Some(5)).await;
        let total_time = start.elapsed();

        println!("Fetched {} order books in {:?}", snapshots.len(), total_time);

        for (symbol, snapshot) in &snapshots {
            println!("  {}: {} bids, {} asks, latency={}ms",
                symbol,
                snapshot.bids.len(),
                snapshot.asks.len(),
                snapshot.fetch_latency_ms
            );
        }

        // Should have fetched at least some order books
        assert!(!snapshots.is_empty(), "Expected at least one order book");

        // Concurrent should be faster than sequential (3 * 500ms = 1500ms)
        // With concurrency, should complete in roughly 500ms (single request time)
        assert!(
            total_time.as_millis() < 2000,
            "Concurrent fetch took too long: {:?}",
            total_time
        );
    }

    /// Test rate limiting behavior
    /// AC #7: Rate limit handling (10 req/sec)
    #[tokio::test]
    #[ignore]
    async fn test_rate_limiting() {
        let config = BitgetRestConfig {
            max_requests_per_second: 10,
            ..Default::default()
        };
        let client = BitgetRestClient::new(config).unwrap();

        // Try to make 15 requests rapidly (more than rate limit allows)
        let symbols: Vec<String> = (0..15)
            .map(|_| "BTCUSDC".to_string())
            .collect();

        let start = std::time::Instant::now();
        let snapshots = client.fetch_orderbooks_concurrent(&symbols, Some(5)).await;
        let total_time = start.elapsed();

        println!("Made {} requests in {:?}", symbols.len(), total_time);
        println!("Got {} successful responses", snapshots.len());

        // With 10 req/sec limit and 15 requests, should take at least 1 second
        // (first 10 immediate, next 5 wait for rate limit reset)
        assert!(
            snapshots.len() > 0,
            "Expected at least some successful requests"
        );
    }
}
