//! Multi-WebSocket Subscriber for Bitget
//!
//! Handles subscribing to 600+ assets by distributing them across multiple
//! WebSocket connections. Bitget recommends < 50 symbols per connection for stability.
//!
//! Architecture:
//! - Divides symbols into chunks of ~50 symbols each
//! - Creates one WebSocket connection per chunk
//! - All connections share the same Books state for data aggregation
//! - Each connection runs its own event loop for receiving updates

use super::books::Books;
use super::messages::{OrderbookData, TickerData};
use super::websocket::BitgetWebSocket;
use crate::market_data::types::MarketDataObserver;
use chrono::{Duration as ChronoDuration, Utc};
use eyre::{eyre, Result};
use parking_lot::RwLock as AtomicLock;
use std::sync::Arc;
use std::time::Duration;
use tokio::select;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;

/// Configuration for multi-websocket subscriber
#[derive(Clone)]
pub struct MultiSubscriberConfig {
    /// WebSocket URL
    pub websocket_url: String,
    /// Symbols per connection (recommended: 50 for stability)
    pub symbols_per_connection: usize,
    /// Stale check period
    pub stale_check_period: Duration,
    /// Stale timeout
    pub stale_timeout: ChronoDuration,
    /// Heartbeat interval
    pub heartbeat_interval: Duration,
}

impl Default for MultiSubscriberConfig {
    fn default() -> Self {
        Self {
            websocket_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
            symbols_per_connection: 50, // Bitget recommendation for stability
            stale_check_period: Duration::from_secs(30),
            stale_timeout: ChronoDuration::minutes(5),
            heartbeat_interval: Duration::from_secs(25),
        }
    }
}

/// Multi-WebSocket subscriber that distributes symbols across connections
pub struct MultiWebSocketSubscriber {
    config: MultiSubscriberConfig,
    books: Arc<AtomicLock<Books>>,
    cancel_token: CancellationToken,
}

impl MultiWebSocketSubscriber {
    /// Create a new multi-websocket subscriber
    pub fn new(config: MultiSubscriberConfig, observer: Arc<AtomicLock<MarketDataObserver>>) -> Self {
        let books = Arc::new(AtomicLock::new(Books::new_with_observer(observer)));

        Self {
            config,
            books,
            cancel_token: CancellationToken::new(),
        }
    }

    /// Start the subscriber with the given symbols
    /// Divides symbols into chunks and creates one WebSocket per chunk
    pub async fn start(&mut self, symbols: Vec<String>) -> Result<()> {
        if symbols.is_empty() {
            tracing::warn!("No symbols provided, subscriber not started");
            return Ok(());
        }

        let chunk_size = self.config.symbols_per_connection;
        let chunks: Vec<Vec<String>> = symbols
            .chunks(chunk_size)
            .map(|c| c.to_vec())
            .collect();

        let num_connections = chunks.len();
        tracing::info!(
            "Starting multi-websocket subscriber: {} symbols across {} connections ({} symbols/connection)",
            symbols.len(),
            num_connections,
            chunk_size
        );

        // Spawn a connection task for each chunk
        for (idx, chunk) in chunks.into_iter().enumerate() {
            let config = self.config.clone();
            let books = self.books.clone();
            let cancel_token = self.cancel_token.clone();
            let connection_id = idx + 1;

            tokio::spawn(async move {
                loop {
                    match Self::run_connection(
                        connection_id,
                        num_connections,
                        &config,
                        &chunk,
                        books.clone(),
                        cancel_token.clone(),
                    )
                    .await
                    {
                        Ok(()) => {
                            if cancel_token.is_cancelled() {
                                tracing::info!("Connection {} stopped (cancelled)", connection_id);
                                break;
                            }
                            tracing::warn!("Connection {} ended, reconnecting...", connection_id);
                        }
                        Err(e) => {
                            if cancel_token.is_cancelled() {
                                tracing::info!("Connection {} stopped (cancelled): {:?}", connection_id, e);
                                break;
                            }
                            tracing::error!("Connection {} failed: {:?}, reconnecting in 5s...", connection_id, e);
                            tokio::time::sleep(Duration::from_secs(5)).await;
                        }
                    }

                    if cancel_token.is_cancelled() {
                        break;
                    }
                }
            });
        }

        Ok(())
    }

    /// Run a single WebSocket connection for a chunk of symbols
    async fn run_connection(
        connection_id: usize,
        total_connections: usize,
        config: &MultiSubscriberConfig,
        symbols: &[String],
        books: Arc<AtomicLock<Books>>,
        cancel_token: CancellationToken,
    ) -> Result<()> {
        tracing::info!(
            "Connection {}/{}: Connecting to {} for {} symbols",
            connection_id,
            total_connections,
            config.websocket_url,
            symbols.len()
        );

        // Connect to WebSocket
        let mut ws = BitgetWebSocket::connect(&config.websocket_url).await?;

        // Add books for all symbols
        {
            let mut books_guard = books.write();
            for symbol in symbols {
                if let Err(e) = books_guard.add_book(symbol) {
                    tracing::warn!("Connection {}: Failed to add book for {}: {:?}", connection_id, symbol, e);
                }
            }
        }

        // Batch subscribe to all symbols at once
        ws.subscribe_batch(symbols).await?;
        tracing::info!(
            "Connection {}/{}: Subscribed to {} symbols",
            connection_id,
            total_connections,
            symbols.len()
        );

        // Setup timers
        let mut heartbeat_timer = interval(config.heartbeat_interval);
        let mut stale_check_timer = interval(config.stale_check_period);

        loop {
            select! {
                _ = cancel_token.cancelled() => {
                    tracing::info!("Connection {}: Cancelled", connection_id);
                    break;
                }

                _ = heartbeat_timer.tick() => {
                    if let Err(e) = ws.send_ping().await {
                        tracing::error!("Connection {}: Heartbeat failed: {:?}", connection_id, e);
                        return Err(e);
                    }

                    // Check pong timeout
                    let last_pong = ws.last_pong_time();
                    let pong_timeout = Utc::now() - ChronoDuration::minutes(2);
                    if last_pong < pong_timeout {
                        tracing::error!("Connection {}: No pong for 2 minutes, reconnecting", connection_id);
                        return Err(eyre!("Pong timeout"));
                    }
                }

                _ = stale_check_timer.tick() => {
                    let expiry_time = Utc::now() - config.stale_timeout;
                    match books.read().check_stale(expiry_time) {
                        Ok((num_stale, total)) => {
                            if num_stale > 0 {
                                tracing::debug!("Connection {}: {}/{} books stale", connection_id, num_stale, total);
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Connection {}: Stale check failed: {:?}", connection_id, e);
                        }
                    }
                }

                msg = ws.next_message() => {
                    match msg {
                        Ok(Some(msg)) => {
                            if let Err(e) = Self::handle_message(&msg, &books) {
                                tracing::trace!("Connection {}: Message handling: {:?}", connection_id, e);
                            }
                        }
                        Ok(None) => {
                            continue;
                        }
                        Err(e) => {
                            tracing::error!("Connection {}: WebSocket error: {:?}", connection_id, e);
                            return Err(e);
                        }
                    }
                }
            }
        }

        // Cleanup
        if let Err(e) = ws.close().await {
            tracing::warn!("Connection {}: Close failed: {:?}", connection_id, e);
        }

        Ok(())
    }

    /// Handle incoming WebSocket message
    fn handle_message(
        msg: &super::messages::BitgetWsMessage,
        books: &Arc<AtomicLock<Books>>,
    ) -> Result<()> {
        let action = msg.action.as_ref().ok_or_else(|| eyre!("Missing action"))?;
        let arg = msg.arg.as_ref().ok_or_else(|| eyre!("Missing arg"))?;
        let data_array = msg.data.as_ref().ok_or_else(|| eyre!("Missing data"))?;

        if data_array.is_empty() {
            return Ok(());
        }

        let symbol = &arg.inst_id;
        let channel = &arg.channel;

        match channel.as_str() {
            "books" => {
                let data: OrderbookData = serde_json::from_value(data_array[0].clone())
                    .map_err(|e| eyre!("Failed to parse orderbook: {}", e))?;

                let mut books_guard = books.write();
                let book = books_guard.get_book_mut(symbol)?;

                match action.as_str() {
                    "snapshot" => {
                        tracing::trace!("Snapshot for {}", symbol);
                        book.apply_snapshot(data)?;
                    }
                    "update" => {
                        book.apply_update(data)?;
                    }
                    _ => {}
                }
            }
            "ticker" => {
                let data: TickerData = serde_json::from_value(data_array[0].clone())
                    .map_err(|e| eyre!("Failed to parse ticker: {}", e))?;

                let mut books_guard = books.write();
                let book = books_guard.get_book_mut(symbol)?;
                book.apply_ticker(data)?;
            }
            _ => {}
        }

        Ok(())
    }

    /// Stop all connections
    pub async fn stop(&self) -> Result<()> {
        tracing::info!("Stopping multi-websocket subscriber");
        self.cancel_token.cancel();
        Ok(())
    }

    /// Get the shared books state
    pub fn get_books(&self) -> Arc<AtomicLock<Books>> {
        self.books.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = MultiSubscriberConfig::default();
        assert_eq!(config.symbols_per_connection, 50);
        assert_eq!(config.heartbeat_interval, Duration::from_secs(25));
    }

    #[test]
    fn test_chunk_calculation() {
        let symbols: Vec<String> = (0..637).map(|i| format!("SYM{}USDT", i)).collect();
        let chunk_size = 50;
        let chunks: Vec<Vec<String>> = symbols.chunks(chunk_size).map(|c| c.to_vec()).collect();

        assert_eq!(chunks.len(), 13); // 637 / 50 = 12.74, rounds up to 13
        assert_eq!(chunks[0].len(), 50);
        assert_eq!(chunks[12].len(), 37); // Last chunk has remainder
    }
}
