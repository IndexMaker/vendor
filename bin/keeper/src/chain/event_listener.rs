//! WebSocket and Polling event listener for Vault events
//!
//! Provides real-time event watching with WebSocket as primary
//! and polling fallback when WebSocket is unavailable.

#![allow(dead_code)] // state() method and ConnectionState reserved for monitoring

use crate::chain::{ChainError, EventSignatures, VaultEvent, parse_log};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::Filter;
use alloy_primitives::{Address, B256, Log as PrimitiveLog};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::{interval, sleep};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Connection state for the event listener
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting { attempt: u32 },
}

/// Configuration for the event listener
#[derive(Debug, Clone)]
pub struct EventListenerConfig {
    /// HTTP RPC URL for polling
    pub rpc_url: String,
    /// WebSocket URL (optional, derived from rpc_url if not provided)
    pub ws_url: Option<String>,
    /// Polling interval in milliseconds (fallback mode)
    pub polling_interval_ms: u64,
    /// Maximum reconnection delay in milliseconds
    pub reconnect_max_delay_ms: u64,
    /// Number of consecutive WebSocket failures before switching to polling
    pub ws_failure_threshold: u32,
    /// List of vault addresses to watch
    pub vault_addresses: Vec<Address>,
    /// Pong timeout in seconds (triggers reconnect if no pong received)
    pub pong_timeout_secs: u64,
}

impl Default for EventListenerConfig {
    fn default() -> Self {
        Self {
            rpc_url: "https://index.rpc.zeeve.net".to_string(),
            ws_url: None,
            polling_interval_ms: 3000,
            reconnect_max_delay_ms: 30000,
            ws_failure_threshold: 3,
            vault_addresses: Vec::new(),
            pong_timeout_secs: 60, // 60 seconds without pong triggers reconnect
        }
    }
}

/// Chain event listener with WebSocket primary and polling fallback
pub struct ChainEventListener {
    config: EventListenerConfig,
    /// Dynamic vault addresses - can be updated at runtime
    vault_addresses: tokio::sync::RwLock<Vec<Address>>,
    state: tokio::sync::RwLock<ConnectionState>,
    event_tx: mpsc::UnboundedSender<VaultEvent>,
    cancel_token: CancellationToken,
    consecutive_ws_failures: tokio::sync::RwLock<u32>,
    last_processed_block: tokio::sync::RwLock<u64>,
    /// Last time a pong was received (for health monitoring)
    last_pong_time: tokio::sync::RwLock<Option<Instant>>,
}

impl ChainEventListener {
    /// Create a new event listener
    pub fn new(
        config: EventListenerConfig,
        event_tx: mpsc::UnboundedSender<VaultEvent>,
    ) -> Arc<Self> {
        let initial_vaults = config.vault_addresses.clone();
        Arc::new(Self {
            config,
            vault_addresses: tokio::sync::RwLock::new(initial_vaults),
            state: tokio::sync::RwLock::new(ConnectionState::Disconnected),
            event_tx,
            cancel_token: CancellationToken::new(),
            consecutive_ws_failures: tokio::sync::RwLock::new(0),
            last_processed_block: tokio::sync::RwLock::new(0),
            last_pong_time: tokio::sync::RwLock::new(None),
        })
    }

    /// Add a vault to the watch list (thread-safe)
    pub async fn add_vault(&self, vault: Address) -> bool {
        let mut vaults = self.vault_addresses.write().await;
        if !vaults.contains(&vault) {
            tracing::info!("🆕 Adding vault {} to watch list", vault);
            vaults.push(vault);
            true
        } else {
            false
        }
    }

    /// Add multiple vaults to the watch list, returns count of newly added
    pub async fn add_vaults(&self, new_vaults: Vec<Address>) -> usize {
        let mut vaults = self.vault_addresses.write().await;
        let mut added = 0;
        for vault in new_vaults {
            if !vaults.contains(&vault) {
                tracing::info!("🆕 Adding vault {} to watch list", vault);
                vaults.push(vault);
                added += 1;
            }
        }
        if added > 0 {
            tracing::info!("Added {} new vaults to watch list (total: {})", added, vaults.len());
        }
        added
    }

    /// Get current vault count
    pub async fn vault_count(&self) -> usize {
        self.vault_addresses.read().await.len()
    }

    /// Get current vaults (cloned)
    pub async fn get_vaults(&self) -> Vec<Address> {
        self.vault_addresses.read().await.clone()
    }

    /// Backtrack historical events from a specific block
    /// This is called on startup to catch any pending orders that were missed
    pub async fn backtrack_events(&self, from_block: u64) -> Result<usize, ChainError> {
        let provider = ProviderBuilder::new()
            .connect_http(self.config.rpc_url.parse().map_err(|_| {
                ChainError::InvalidConfig("Invalid RPC URL".to_string())
            })?);

        let current_block = provider
            .get_block_number()
            .await
            .map_err(|e| ChainError::RpcError(e.to_string()))?;

        let vault_addresses = self.vault_addresses.read().await.clone();
        if vault_addresses.is_empty() {
            tracing::info!("⏪ No vaults to backtrack");
            return Ok(0);
        }

        tracing::info!(
            "⏪ Backtracking events from block {} to {} ({} vaults)",
            from_block,
            current_block,
            vault_addresses.len()
        );

        // Build filter for all event types
        let filter = Filter::new()
            .address(vault_addresses)
            .event_signature(EventSignatures::all())
            .from_block(from_block)
            .to_block(current_block);

        let logs = provider
            .get_logs(&filter)
            .await
            .map_err(|e| ChainError::RpcError(e.to_string()))?;

        let mut event_count = 0;
        for log in logs {
            let vault_address = log.address();
            let tx_hash = log.transaction_hash.unwrap_or(B256::ZERO);
            let block_number = log.block_number.unwrap_or(0);

            let primitive_log = PrimitiveLog::new(
                vault_address,
                log.topics().to_vec(),
                log.data().data.clone(),
            )
            .ok_or_else(|| ChainError::EventParseError {
                reason: "Failed to create log".to_string(),
            })?;

            match parse_log(&primitive_log, vault_address, tx_hash, block_number) {
                Ok(event) => {
                    tracing::info!(
                        "⏪ Backtrack: Found historical {} at block {}",
                        match &event {
                            VaultEvent::Buy(_) => "BuyOrder",
                            VaultEvent::Sell(_) => "SellOrder",
                            VaultEvent::Acquisition(_) => "Acquisition",
                            VaultEvent::Disposal(_) => "Disposal",
                            VaultEvent::AcquisitionClaim(_) => "AcquisitionClaim",
                            VaultEvent::DisposalClaim(_) => "DisposalClaim",
                        },
                        block_number
                    );
                    self.emit_event(event).await;
                    event_count += 1;
                }
                Err(e) => {
                    tracing::warn!("Backtrack: Failed to parse log: {:?}", e);
                }
            }
        }

        // Update last processed block
        *self.last_processed_block.write().await = current_block;

        tracing::info!(
            "⏪ Backtrack complete: processed {} historical events",
            event_count
        );

        Ok(event_count)
    }

    /// Get the cancellation token for graceful shutdown
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Get current connection state
    pub async fn state(&self) -> ConnectionState {
        self.state.read().await.clone()
    }

    /// Start the event listener (main entry point)
    pub async fn start(self: Arc<Self>) -> Result<(), ChainError> {
        let vault_count = self.vault_count().await;
        tracing::info!("🚀 Starting chain event listener");
        tracing::info!("  RPC URL: {}", self.config.rpc_url);
        tracing::info!("  Watching {} vaults (dynamic)", vault_count);

        // Try WebSocket first, fall back to polling
        if self.config.ws_url.is_some() || self.try_derive_ws_url().is_some() {
            tracing::info!("🔌 Attempting WebSocket connection...");
            if let Err(e) = self.clone().run_websocket_loop().await {
                tracing::warn!("WebSocket failed: {:?}, switching to polling", e);
            }
        }

        // Fall back to polling
        tracing::info!("📊 Using polling mode");
        self.run_polling_loop().await
    }

    /// Derive WebSocket URL from HTTP RPC URL
    fn try_derive_ws_url(&self) -> Option<String> {
        let url = &self.config.rpc_url;
        if url.starts_with("https://") {
            Some(url.replace("https://", "wss://"))
        } else if url.starts_with("http://") {
            Some(url.replace("http://", "ws://"))
        } else {
            None
        }
    }

    /// Run the WebSocket event loop
    async fn run_websocket_loop(self: Arc<Self>) -> Result<(), ChainError> {
        let ws_url = self
            .config
            .ws_url
            .clone()
            .or_else(|| self.try_derive_ws_url())
            .ok_or_else(|| ChainError::InvalidConfig("No WebSocket URL available".to_string()))?;

        let mut retry_delay = Duration::from_millis(1000);
        let max_delay = Duration::from_millis(self.config.reconnect_max_delay_ms);

        loop {
            if self.cancel_token.is_cancelled() {
                break;
            }

            // Check if we should switch to polling
            let failures = *self.consecutive_ws_failures.read().await;
            if failures >= self.config.ws_failure_threshold {
                tracing::warn!(
                    "🔄 {} consecutive WebSocket failures, switching to polling",
                    failures
                );
                return Err(ChainError::ConnectionLost {
                    retry_count: failures,
                });
            }

            // Update state
            {
                let mut state = self.state.write().await;
                *state = if failures > 0 {
                    ConnectionState::Reconnecting { attempt: failures }
                } else {
                    ConnectionState::Connecting
                };
            }

            tracing::info!("🔌 Connecting to WebSocket: {}", ws_url);

            match self.clone().connect_and_subscribe(&ws_url).await {
                Ok(()) => {
                    // Reset failures on successful connection
                    *self.consecutive_ws_failures.write().await = 0;
                    retry_delay = Duration::from_millis(1000);
                }
                Err(e) => {
                    tracing::error!("❌ WebSocket error: {:?}", e);
                    *self.consecutive_ws_failures.write().await += 1;

                    // Exponential backoff
                    tracing::info!("⏳ Retrying in {:?}...", retry_delay);
                    sleep(retry_delay).await;
                    retry_delay = std::cmp::min(retry_delay * 2, max_delay);
                }
            }

            // Update state to disconnected
            *self.state.write().await = ConnectionState::Disconnected;
        }

        Ok(())
    }

    /// Connect to WebSocket and subscribe to events
    async fn connect_and_subscribe(self: Arc<Self>, ws_url: &str) -> Result<(), ChainError> {
        let (ws_stream, _) = connect_async(ws_url).await?;
        let (mut write, mut read) = ws_stream.split();

        // Update state to connected
        *self.state.write().await = ConnectionState::Connected;
        tracing::info!("🔌 WebSocket connected");

        // Create subscription filter using dynamic vault list
        let vault_addresses = self.vault_addresses.read().await.clone();
        let addresses: Vec<String> = vault_addresses
            .iter()
            .map(|a| format!("{:?}", a))
            .collect();

        let topics: Vec<String> = EventSignatures::all()
            .iter()
            .map(|s| format!("{:?}", s))
            .collect();

        let subscribe_msg = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_subscribe",
            "params": ["logs", {
                "address": addresses,
                "topics": [topics]
            }]
        });

        write
            .send(Message::Text(subscribe_msg.to_string().into()))
            .await
            .map_err(|e| ChainError::SubscriptionError(e.to_string()))?;

        tracing::info!("📡 Subscribed to all 6 event types (Orders + Lifecycle)");

        // Set up ping interval for connection health
        let mut ping_interval = interval(Duration::from_secs(30));
        let mut pong_check_interval = interval(Duration::from_secs(10));
        let mut subscription_id: Option<String> = None;

        // Initialize last pong time to now (connection just established)
        *self.last_pong_time.write().await = Some(Instant::now());

        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    tracing::info!("🛑 Shutdown signal received");
                    break;
                }
                _ = ping_interval.tick() => {
                    // Send ping to keep connection alive
                    if write.send(Message::Ping(vec![].into())).await.is_err() {
                        tracing::warn!("Failed to send ping");
                        break;
                    }
                    tracing::trace!("Ping sent");
                }
                _ = pong_check_interval.tick() => {
                    // Check if we've received a pong recently
                    if let Some(last_pong) = *self.last_pong_time.read().await {
                        let elapsed = last_pong.elapsed();
                        if elapsed > Duration::from_secs(self.config.pong_timeout_secs) {
                            tracing::warn!(
                                "⚠️ No pong received for {}s (timeout: {}s), triggering reconnect",
                                elapsed.as_secs(),
                                self.config.pong_timeout_secs
                            );
                            break;
                        }
                    }
                }
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            if let Err(e) = self.handle_ws_message(&text, &mut subscription_id).await {
                                tracing::error!("Error handling message: {:?}", e);
                            }
                        }
                        Some(Ok(Message::Pong(_))) => {
                            *self.last_pong_time.write().await = Some(Instant::now());
                            tracing::trace!("Pong received, connection healthy");
                        }
                        Some(Ok(Message::Close(_))) => {
                            tracing::info!("WebSocket closed by server");
                            break;
                        }
                        Some(Err(e)) => {
                            tracing::error!("WebSocket error: {:?}", e);
                            break;
                        }
                        None => {
                            tracing::info!("WebSocket stream ended");
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(())
    }

    /// Handle incoming WebSocket message
    async fn handle_ws_message(
        &self,
        text: &str,
        subscription_id: &mut Option<String>,
    ) -> Result<(), ChainError> {
        let json: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| ChainError::EventParseError { reason: e.to_string() })?;

        // Check if this is a subscription confirmation
        if let Some(result) = json.get("result") {
            if let Some(id) = result.as_str() {
                *subscription_id = Some(id.to_string());
                tracing::info!("📋 Subscription confirmed: {}", id);
                return Ok(());
            }
        }

        // Check if this is an event notification
        if let Some(params) = json.get("params") {
            if let Some(result) = params.get("result") {
                self.process_log_result(result).await?;
            }
        }

        Ok(())
    }

    /// Process a log result from WebSocket
    async fn process_log_result(&self, result: &serde_json::Value) -> Result<(), ChainError> {
        // Extract log fields
        let address_str = result
            .get("address")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ChainError::EventParseError {
                reason: "Missing address".to_string(),
            })?;

        let vault_address: Address = address_str
            .parse()
            .map_err(|_| ChainError::InvalidVaultAddress(address_str.to_string()))?;

        let tx_hash_str = result
            .get("transactionHash")
            .and_then(|v| v.as_str())
            .unwrap_or("0x0000000000000000000000000000000000000000000000000000000000000000");

        let tx_hash: B256 = tx_hash_str
            .parse()
            .map_err(|_| ChainError::EventParseError {
                reason: "Invalid tx hash".to_string(),
            })?;

        let block_number_str = result
            .get("blockNumber")
            .and_then(|v| v.as_str())
            .unwrap_or("0x0");

        let block_number = u64::from_str_radix(block_number_str.trim_start_matches("0x"), 16)
            .unwrap_or(0);

        // Parse topics
        let topics: Vec<B256> = result
            .get("topics")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str())
                    .filter_map(|s| s.parse().ok())
                    .collect()
            })
            .unwrap_or_default();

        // Parse data
        let data_str = result
            .get("data")
            .and_then(|v| v.as_str())
            .unwrap_or("0x");

        let data: Vec<u8> = hex::decode(data_str.trim_start_matches("0x")).unwrap_or_default();

        // Build Log struct
        let log = PrimitiveLog::new(vault_address, topics, data.into())
            .ok_or_else(|| ChainError::EventParseError {
                reason: "Failed to create log".to_string(),
            })?;

        // Parse into VaultEvent
        match parse_log(&log, vault_address, tx_hash, block_number) {
            Ok(event) => {
                self.emit_event(event).await;
            }
            Err(e) => {
                tracing::warn!("Failed to parse log: {:?}", e);
            }
        }

        Ok(())
    }

    /// Run the polling fallback loop
    async fn run_polling_loop(self: Arc<Self>) -> Result<(), ChainError> {
        let provider = ProviderBuilder::new()
            .connect_http(self.config.rpc_url.parse().map_err(|_| {
                ChainError::InvalidConfig("Invalid RPC URL".to_string())
            })?);

        // Get current block number
        let current_block = provider
            .get_block_number()
            .await
            .map_err(|e| ChainError::RpcError(e.to_string()))?;

        *self.last_processed_block.write().await = current_block;

        tracing::info!(
            "📊 Starting polling from block {} (interval: {}ms)",
            current_block,
            self.config.polling_interval_ms
        );

        let mut poll_interval = interval(Duration::from_millis(self.config.polling_interval_ms));

        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    tracing::info!("🛑 Shutdown signal received");
                    break;
                }
                _ = poll_interval.tick() => {
                    if let Err(e) = self.poll_events(&provider).await {
                        tracing::error!("Polling error: {:?}", e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Poll for new events
    async fn poll_events<P: Provider>(&self, provider: &P) -> Result<(), ChainError> {
        let from_block = *self.last_processed_block.read().await;
        let to_block = provider
            .get_block_number()
            .await
            .map_err(|e| ChainError::RpcError(e.to_string()))?;

        if to_block <= from_block {
            return Ok(()); // No new blocks
        }

        // Get current dynamic vault list
        let vault_addresses = self.vault_addresses.read().await.clone();
        if vault_addresses.is_empty() {
            tracing::trace!("No vaults to monitor, skipping poll");
            return Ok(());
        }

        tracing::debug!("📊 Polling blocks {} to {} ({} vaults)", from_block + 1, to_block, vault_addresses.len());

        // Build filter for both event types
        let filter = Filter::new()
            .address(vault_addresses)
            .event_signature(EventSignatures::all())
            .from_block(from_block + 1)
            .to_block(to_block);

        let logs = provider
            .get_logs(&filter)
            .await
            .map_err(|e| ChainError::RpcError(e.to_string()))?;

        for log in logs {
            let vault_address = log.address();
            let tx_hash = log.transaction_hash.unwrap_or(B256::ZERO);
            let block_number = log.block_number.unwrap_or(0);

            // Build primitive Log from the RPC Log
            let primitive_log = PrimitiveLog::new(vault_address, log.topics().to_vec(), log.data().data.clone())
                .ok_or_else(|| ChainError::EventParseError {
                    reason: "Failed to create log".to_string(),
                })?;

            match parse_log(&primitive_log, vault_address, tx_hash, block_number) {
                Ok(event) => {
                    self.emit_event(event).await;
                }
                Err(e) => {
                    tracing::warn!("Failed to parse log: {:?}", e);
                }
            }
        }

        // Update last processed block
        *self.last_processed_block.write().await = to_block;

        Ok(())
    }

    /// Generate a correlation ID for an event
    /// Format: {tx_hash}:{operation_type}:{sequence} or UUID fallback
    /// Story 2.8: Extended to handle all 6 event types
    fn generate_correlation_id(&self, event: &VaultEvent) -> String {
        let tx_hash = event.tx_hash();
        let op_type = match event {
            VaultEvent::Buy(_) => "buy",
            VaultEvent::Sell(_) => "sell",
            VaultEvent::Acquisition(_) => "acquisition",
            VaultEvent::Disposal(_) => "disposal",
            VaultEvent::AcquisitionClaim(_) => "acq_claim",
            VaultEvent::DisposalClaim(_) => "disp_claim",
        };

        // Use tx_hash if available, otherwise generate UUID
        if tx_hash != B256::ZERO {
            // Truncate hash to first 16 chars for readability
            let hash_short = &format!("{:?}", tx_hash)[2..18];
            format!("{}:{}:001", hash_short, op_type)
        } else {
            // UUID fallback when tx_hash is unavailable
            format!("{}:{}:001", Uuid::new_v4().to_string()[..8].to_string(), op_type)
        }
    }

    /// Emit an event to the channel with structured logging
    /// Story 2.8: Extended to log all 6 event types
    async fn emit_event(&self, event: VaultEvent) {
        let correlation_id = self.generate_correlation_id(&event);

        match &event {
            VaultEvent::Buy(e) => {
                tracing::info!(
                    correlation_id = %correlation_id,
                    index_id = %e.index_id,
                    collateral_amount = %e.collateral_amount,
                    trader = %e.trader,
                    vault = %e.vault_address,
                    "📥 BuyOrder received"
                );
            }
            VaultEvent::Sell(e) => {
                tracing::info!(
                    correlation_id = %correlation_id,
                    index_id = %e.index_id,
                    itp_amount = %e.itp_amount,
                    trader = %e.trader,
                    vault = %e.vault_address,
                    "📥 SellOrder received"
                );
            }
            // =========================================================================
            // Story 2.8: Logging for lifecycle events
            // =========================================================================
            VaultEvent::Acquisition(e) => {
                tracing::info!(
                    correlation_id = %correlation_id,
                    index_id = %e.index_id,
                    remain = %e.remain,
                    spent = %e.spent,
                    itp_minted = %e.itp_minted,
                    controller = %e.controller,
                    vault = %e.vault_address,
                    "📦 Acquisition event received"
                );
            }
            VaultEvent::Disposal(e) => {
                tracing::info!(
                    correlation_id = %correlation_id,
                    index_id = %e.index_id,
                    itp_remain = %e.itp_remain,
                    itp_burned = %e.itp_burned,
                    gains = %e.gains,
                    controller = %e.controller,
                    vault = %e.vault_address,
                    "📦 Disposal event received"
                );
            }
            VaultEvent::AcquisitionClaim(e) => {
                tracing::info!(
                    correlation_id = %correlation_id,
                    index_id = %e.index_id,
                    remain = %e.remain,
                    spent = %e.spent,
                    itp_minted = %e.itp_minted,
                    controller = %e.controller,
                    vault = %e.vault_address,
                    "🔔 AcquisitionClaim event received (remain={})",
                    e.remain
                );
            }
            VaultEvent::DisposalClaim(e) => {
                tracing::info!(
                    correlation_id = %correlation_id,
                    index_id = %e.index_id,
                    itp_remain = %e.itp_remain,
                    itp_burned = %e.itp_burned,
                    gains = %e.gains,
                    controller = %e.controller,
                    vault = %e.vault_address,
                    "🔔 DisposalClaim event received (itp_remain={})",
                    e.itp_remain
                );
            }
        }

        if self.event_tx.send(event).is_err() {
            tracing::error!(
                correlation_id = %correlation_id,
                "Failed to send event to channel (receiver dropped)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::{BuyOrderEvent, SellOrderEvent};
    use chrono::Utc;

    #[test]
    fn test_derive_ws_url() {
        let config = EventListenerConfig {
            rpc_url: "https://index.rpc.zeeve.net".to_string(),
            ..Default::default()
        };

        let (tx, _rx) = mpsc::unbounded_channel();
        let listener = ChainEventListener::new(config, tx);

        let ws_url = listener.try_derive_ws_url();
        assert_eq!(ws_url, Some("wss://index.rpc.zeeve.net".to_string()));
    }

    #[test]
    fn test_derive_ws_url_http() {
        let config = EventListenerConfig {
            rpc_url: "http://localhost:8545".to_string(),
            ..Default::default()
        };

        let (tx, _rx) = mpsc::unbounded_channel();
        let listener = ChainEventListener::new(config, tx);

        let ws_url = listener.try_derive_ws_url();
        assert_eq!(ws_url, Some("ws://localhost:8545".to_string()));
    }

    #[test]
    fn test_connection_state() {
        assert_eq!(ConnectionState::Disconnected, ConnectionState::Disconnected);
        assert_ne!(ConnectionState::Connected, ConnectionState::Disconnected);
    }

    #[test]
    fn test_connection_state_reconnecting() {
        let state1 = ConnectionState::Reconnecting { attempt: 1 };
        let state2 = ConnectionState::Reconnecting { attempt: 2 };
        assert_ne!(state1, state2);
    }

    #[test]
    fn test_default_config() {
        let config = EventListenerConfig::default();
        assert_eq!(config.polling_interval_ms, 3000);
        assert_eq!(config.reconnect_max_delay_ms, 30000);
        assert_eq!(config.ws_failure_threshold, 3);
        assert_eq!(config.pong_timeout_secs, 60);
        assert!(config.vault_addresses.is_empty());
    }

    #[test]
    fn test_correlation_id_with_tx_hash() {
        let config = EventListenerConfig::default();
        let (tx, _rx) = mpsc::unbounded_channel();
        let listener = ChainEventListener::new(config, tx);

        let buy_event = VaultEvent::Buy(BuyOrderEvent {
            keeper: Address::ZERO,
            trader: Address::repeat_byte(1),
            index_id: 42,
            vendor_id: 1,
            collateral_amount: 1_000_000,
            vault_address: Address::repeat_byte(2),
            tx_hash: "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
                .parse()
                .unwrap(),
            block_number: 100,
            received_at: Utc::now(),
        });

        let correlation_id = listener.generate_correlation_id(&buy_event);
        assert!(correlation_id.contains(":buy:001"));
        assert!(correlation_id.starts_with("1234567890abcdef"));
    }

    #[test]
    fn test_correlation_id_uuid_fallback() {
        let config = EventListenerConfig::default();
        let (tx, _rx) = mpsc::unbounded_channel();
        let listener = ChainEventListener::new(config, tx);

        let sell_event = VaultEvent::Sell(SellOrderEvent {
            keeper: Address::ZERO,
            trader: Address::repeat_byte(1),
            index_id: 42,
            vendor_id: 1,
            itp_amount: 1_000_000,
            vault_address: Address::repeat_byte(2),
            tx_hash: B256::ZERO, // Zero hash triggers UUID fallback
            block_number: 100,
            received_at: Utc::now(),
        });

        let correlation_id = listener.generate_correlation_id(&sell_event);
        assert!(correlation_id.contains(":sell:001"));
        // UUID should be 8 chars followed by :sell:001
        assert_eq!(correlation_id.len(), 8 + 9); // uuid:sell:001
    }

    #[tokio::test]
    async fn test_initial_state_is_disconnected() {
        let config = EventListenerConfig::default();
        let (tx, _rx) = mpsc::unbounded_channel();
        let listener = ChainEventListener::new(config, tx);

        assert_eq!(listener.state().await, ConnectionState::Disconnected);
    }

    #[tokio::test]
    async fn test_last_pong_time_initially_none() {
        let config = EventListenerConfig::default();
        let (tx, _rx) = mpsc::unbounded_channel();
        let listener = ChainEventListener::new(config, tx);

        let last_pong = listener.last_pong_time.read().await;
        assert!(last_pong.is_none());
    }

    #[test]
    fn test_config_with_custom_pong_timeout() {
        let config = EventListenerConfig {
            pong_timeout_secs: 120,
            ..Default::default()
        };
        assert_eq!(config.pong_timeout_secs, 120);
    }
}
