use super::books::Books;
use super::messages::{OrderbookData, TickerData};
use super::websocket::BitgetWebSocket;
use crate::market_data::types::{MarketDataObserver, Subscription};
use chrono::{Duration as ChronoDuration, Utc};
use eyre::{eyre, Result};
use parking_lot::RwLock as AtomicLock;
use std::sync::Arc;
use std::time::Duration;
use tokio::select;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{interval, sleep};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct BitgetSubscriberConfig {
    pub websocket_url: String,
    pub subscription_limit_rate: usize,
    pub stale_check_period: Duration,
    pub stale_timeout: ChronoDuration,
    pub heartbeat_interval: Duration,
}

pub struct BitgetSubscriber {
    config: BitgetSubscriberConfig,
    books: Arc<AtomicLock<Books>>,
    cancel_token: CancellationToken,
}

impl BitgetSubscriber {
    pub fn new(
        config: BitgetSubscriberConfig,
        observer: Arc<AtomicLock<MarketDataObserver>>,
    ) -> Self {
        let books = Arc::new(AtomicLock::new(Books::new_with_observer(observer)));

        Self {
            config,
            books,
            cancel_token: CancellationToken::new(),
        }
    }

    pub async fn start(
        &mut self,
        mut subscription_rx: UnboundedReceiver<Subscription>,
    ) -> Result<()> {
        let config = self.config.clone();
        let books = self.books.clone();
        let cancel_token = self.cancel_token.clone();

        tokio::spawn(async move {
            if let Err(e) = Self::run_subscriber_loop(config, subscription_rx, books, cancel_token).await {
                tracing::error!("Subscriber loop failed: {:?}", e);
            }
        });

        Ok(())
    }

    pub async fn stop(&self) -> Result<()> {
        tracing::info!("Stopping Bitget subscriber");
        self.cancel_token.cancel();
        Ok(())
    }

    async fn run_subscriber_loop(
        config: BitgetSubscriberConfig,
        mut subscription_rx: UnboundedReceiver<Subscription>,
        books: Arc<AtomicLock<Books>>,
        cancel_token: CancellationToken,
    ) -> Result<()> {
        // Connect to WebSocket
        let mut ws = BitgetWebSocket::connect(&config.websocket_url).await?;

        // Setup timers
        let mut heartbeat_timer = interval(config.heartbeat_interval);
        let mut stale_check_timer = interval(config.stale_check_period);

        // Simple rate limiter: track subscription count and reset time
        let mut subscription_count = 0usize;
        let mut rate_limit_reset_time = Utc::now() + ChronoDuration::hours(1);

        tracing::info!("Bitget subscriber loop started");

        loop {
            select! {
                _ = cancel_token.cancelled() => {
                    tracing::info!("Subscriber loop cancelled");
                    break;
                }

                _ = heartbeat_timer.tick() => {
                    if let Err(e) = ws.send_ping().await {
                        tracing::error!("Heartbeat failed: {:?}", e);
                        break; // Connection lost, exit loop
                    }

                    // Check if we haven't received pong in a while
                    let last_pong = ws.last_pong_time();
                    let pong_timeout = Utc::now() - ChronoDuration::minutes(2);
                    if last_pong < pong_timeout {
                        tracing::error!("No pong received for 2 minutes, reconnecting");
                        break;
                    }
                }

                _ = stale_check_timer.tick() => {
                    let expiry_time = Utc::now() - config.stale_timeout;
                    match books.read().check_stale(expiry_time) {
                        Ok((num_stale, total)) => {
                            if num_stale > 0 {
                                tracing::warn!("{}/{} books are stale", num_stale, total);
                            }
                            if total > 0 && num_stale == total {
                                tracing::error!("All books stale, reconnecting");
                                cancel_token.cancel();
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Stale check failed: {:?}", e);
                        }
                    }
                }

                Some(Subscription { ticker: symbol, exchange }) = subscription_rx.recv() => {
                    tracing::info!("Received subscription request for {} on {}", symbol, exchange);

                    // Rate limiting: 240 subscriptions per hour
                    let now = Utc::now();
                    if now >= rate_limit_reset_time {
                        subscription_count = 0;
                        rate_limit_reset_time = now + ChronoDuration::hours(1);
                    }

                    if subscription_count >= config.subscription_limit_rate {
                        let wait_duration = (rate_limit_reset_time - now).to_std().unwrap_or(Duration::from_secs(60));
                        tracing::warn!(
                            "Rate limit reached ({}/hour), waiting {:?}",
                            config.subscription_limit_rate,
                            wait_duration
                        );
                        sleep(wait_duration).await;
                        subscription_count = 0;
                        rate_limit_reset_time = Utc::now() + ChronoDuration::hours(1);
                    }

                    // Add book
                    if let Err(e) = books.write().add_book(&symbol) {
                        tracing::warn!("Failed to add book for {}: {:?}", symbol, e);
                        continue;
                    }

                    // Subscribe via WebSocket
                    if let Err(e) = ws.subscribe(&symbol).await {
                        tracing::error!("Failed to subscribe to {}: {:?}", symbol, e);
                        continue;
                    }

                    subscription_count += 1;
                    tracing::info!("Subscribed to {} ({}/{})", symbol, subscription_count, config.subscription_limit_rate);
                }

                msg = ws.next_message() => {
                    match msg {
                        Ok(Some(msg)) => {
                            if let Err(e) = Self::handle_message(&msg, &books) {
                                tracing::warn!("Failed to handle message: {:?}", e);
                            }
                        }
                        Ok(None) => {
                            // This is expected for pong, ping, etc.
                            continue;
                        }
                        Err(e) => {
                            tracing::error!("WebSocket error: {:?}", e);
                            break; // Connection lost, exit loop
                        }
                    }
                }
            }
        }

        // Cleanup
        if let Err(e) = ws.close().await {
            tracing::warn!("Failed to close WebSocket cleanly: {:?}", e);
        }

        tracing::info!("Bitget subscriber loop exited");
        Ok(())
    }

    fn handle_message(
        msg: &super::messages::BitgetWsMessage,
        books: &Arc<AtomicLock<Books>>,
    ) -> Result<()> {
        // Extract action and arg
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
                // Parse orderbook data
                let data: OrderbookData = serde_json::from_value(data_array[0].clone())
                    .map_err(|e| eyre!("Failed to parse orderbook data: {}", e))?;

                let mut books_guard = books.write();
                let book = books_guard.get_book_mut(symbol)?;

                match action.as_str() {
                    "snapshot" => {
                        tracing::debug!("Processing snapshot for {}", symbol);
                        book.apply_snapshot(data)?;
                    }
                    "update" => {
                        tracing::trace!("Processing update for {}", symbol);
                        book.apply_update(data)?;
                    }
                    _ => {
                        tracing::warn!("Unknown action for books channel: {}", action);
                    }
                }
            }
            "ticker" => {
                // Parse ticker data
                let data: TickerData = serde_json::from_value(data_array[0].clone())
                    .map_err(|e| eyre!("Failed to parse ticker data: {}", e))?;

                let mut books_guard = books.write();
                let book = books_guard.get_book_mut(symbol)?;
                book.apply_ticker(data)?;
            }
            _ => {
                tracing::debug!("Ignoring channel: {}", channel);
            }
        }

        Ok(())
    }

    pub fn get_books(&self) -> Arc<AtomicLock<Books>> {
        self.books.clone()
    }
}