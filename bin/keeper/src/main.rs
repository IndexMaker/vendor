use clap::Parser;
use common::amount::Amount;
use eyre::Result;
use std::{path::PathBuf, sync::Arc};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod accumulator;
mod assets;
mod chain;
mod config;
mod index;
mod keeper;
mod onchain;
mod processor;
mod vendor;

use accumulator::{AccumulatorConfig, IndexOrder, OrderAccumulator, OrderAction};
use chain::{StewardClient, StewardClientConfig};
use config::{ClaimHandlerConfig, KeeperConfig};
use index::IndexMapper;
use keeper::{ClaimHandler, KeeperLoop, KeeperLoopConfig};
use onchain::{GasConfig, OnchainSubmitter, SubmitterConfig};
use processor::{ProcessorConfig, QuoteProcessor};
use vendor::{UpdateMarketDataConfig, VendorClient};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Path to configuration directory
    #[arg(long, default_value = "./configs/dev")]
    config_path: String,

    /// Log level (trace, debug, info, warn, error)
    #[arg(short, long, default_value = "info")]
    log_level: String,

    /// Run in test mode (submit test orders and exit)
    #[arg(long)]
    test_mode: bool,

    /// Disable event listener (manual order submission only)
    #[arg(long)]
    no_event_listener: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| cli.log_level.clone().into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("Starting VaultWorks Keeper");

    // Load configuration
    let config_dir = PathBuf::from(&cli.config_path);
    let keeper_config_path = config_dir.join("keeper.json");
    let indices_config_path = config_dir.join("indices.json");

    let config = if keeper_config_path.exists() {
        let mut cfg = KeeperConfig::load_from_file(&keeper_config_path).await?;
        cfg.polling = cfg.polling.with_env_overrides();
        cfg
    } else {
        tracing::warn!("keeper.json not found, using defaults");
        let mut cfg = KeeperConfig::default();
        cfg.polling = cfg.polling.with_env_overrides();
        cfg
    };

    tracing::info!(keeper_id = %config.keeper_id, vendor_url = %config.vendor.url, poll_secs = config.polling.interval_secs, "Config loaded");

    // Load index mapper
    let index_mapper = if indices_config_path.exists() {
        IndexMapper::load_from_file(&indices_config_path).await?
    } else {
        tracing::warn!("indices.json not found, using empty mapper");
        IndexMapper::new()
    };

    tracing::info!(count = index_mapper.len(), "Loaded indices");

    // Create vendor client with Story 2.5 update_market_data config
    // Apply environment variable overrides first (Story 2.5)
    let vendor_config = config.vendor.clone().with_env_overrides();
    let update_market_data_config = UpdateMarketDataConfig {
        endpoint: vendor_config.update_market_data_endpoint.clone(),
        timeout_secs: vendor_config.update_market_data_timeout_secs,
        retry_attempts: vendor_config.update_market_data_retry_attempts,
    };
    let vendor_client = VendorClient::with_update_market_data_config(
        config.vendor.url.clone(),
        config.vendor.timeout_secs,
        config.vendor.retry_attempts,
        update_market_data_config,
    );

    // Test vendor connection
    match vendor_client.health_check().await {
        Ok(health) => {
            tracing::info!(status = %health.status, vendor_id = health.vendor_id, assets = health.tracked_assets, "Vendor connected");
        }
        Err(e) => {
            tracing::error!(?e, "Vendor connection failed");
        }
    }

    // Test quote request
    let all_assets = index_mapper.get_all_asset_ids();
    if !all_assets.is_empty() {
        match vendor_client.quote_assets(all_assets.clone()).await {
            Ok(quote) => {
                tracing::debug!(requested = all_assets.len(), stale = quote.len(), "Quote test OK");
            }
            Err(e) => {
                tracing::error!(?e, "Quote request failed");
            }
        }
    }

    // Create quote processor
    let processor_config = ProcessorConfig {
        vendor_id: config.blockchain.vendor_id,
        max_order_size_multiplier: 2.0,
    };

    let quote_processor = Arc::new(QuoteProcessor::new(
        processor_config,
        Arc::new(index_mapper),
        Arc::new(vendor_client),
    ));
    
    let submitter_config = SubmitterConfig {
        rpc_url: config.blockchain.rpc_url.clone(),
        castle_address: config.blockchain.castle_address.parse()?,
        vendor_id: config.blockchain.vendor_id,
        private_key: std::env::var("PRIVATE_KEY").ok(),
        dry_run: config.blockchain.dry_run,
        gas_config: GasConfig::default(),
        retry_attempts: 3,
        retry_delay_ms: 1000,
        // Story 2.6: Default max order size for processPending* calls
        // Can be overridden via DEFAULT_MAX_ORDER_SIZE env var
        default_max_order_size: std::env::var("DEFAULT_MAX_ORDER_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(u128::MAX),
    };
    
    // Always create with provider, use dry_run flag to skip actual submission
    use alloy::providers::ProviderBuilder;
    use alloy::network::EthereumWallet;
    use alloy::signers::local::PrivateKeySigner;

    // Create provider with wallet for signing transactions
    // Keeper REQUIRES a wallet to sign transactions
    let private_key = submitter_config.private_key.as_ref()
        .ok_or_else(|| eyre::eyre!("PRIVATE_KEY environment variable is required for keeper"))?;
    let signer: PrivateKeySigner = private_key.parse()?;
    let wallet = EthereumWallet::from(signer.clone());
    tracing::info!(from = %signer.address(), castle = %config.blockchain.castle_address, "Wallet loaded for signing transactions");

    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(config.blockchain.rpc_url.parse()?);

    let submitter = Arc::new(OnchainSubmitter::new_with_provider(submitter_config, provider)?);
    
    tracing::info!(dry_run = config.blockchain.dry_run, "Submitter ready");

    // Full pipeline integration
    
    let accumulator_config = AccumulatorConfig {
        batch_window_ms: config.polling.batch_window_ms,
        max_batch_size: 100,
    };
    
    let accumulator = Arc::new(OrderAccumulator::new(accumulator_config));
    
    // Start processing with full pipeline
    let accumulator_clone = accumulator.clone();
    let processor_clone = quote_processor.clone();
    let submitter_clone = submitter.clone();
    
    accumulator_clone.start_processing(move |batch| {
        tracing::info!(indices = batch.indices.len(), orders = batch.total_order_count(), "Processing batch");
        
        // Full pipeline: Process → Submit
        let processor = processor_clone.clone();
        let submitter = submitter_clone.clone();
        
        tokio::spawn(async move {
            // Step 1: Process batch
            match processor.process_batch(batch).await {
                Ok(payload) => {
                    let summary = processor.get_payload_summary(&payload);
                    tracing::debug!(orders = summary.index_count, assets = summary.unique_assets, "Payload ready");
                    
                    // Step 2: Submit to blockchain
                    // WARNING: This uses deprecated submit_payload() which has incorrect Address::ZERO.
                    // In production, KeeperLoop.run() uses settle_batch() with correct trader addresses.
                    // This callback is only for legacy/manual batch processing.
                    #[allow(deprecated)]
                    match submitter.submit_payload(payload).await {
                        Ok(result) => {
                            match result {
                                onchain::SubmissionResult::Success { market_data_tx, buy_order_txs } => {
                                    tracing::info!(market_tx = ?market_data_tx.map(|t| t.tx_hash), orders = buy_order_txs.len(), "Submitted");
                                }
                                onchain::SubmissionResult::DryRun { would_submit } => {
                                    tracing::debug!(assets = would_submit.market_data_assets, orders = would_submit.buy_orders_count, "Dry run");
                                }
                                onchain::SubmissionResult::Failed { error } => {
                                    tracing::error!(%error, "Submission failed");
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!(?e, "Submit error");
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(?e, "Batch processing failed");
                }
            }
        });
    }).await;
    
    if cli.test_mode {
        tracing::info!("Test mode");
        let test_index_ids = quote_processor.index_mapper.get_all_index_ids();
        
        for index_id in test_index_ids.iter().take(2) {
            let order1 = IndexOrder {
                index_id: *index_id,
                action: OrderAction::Deposit {
                    user_address: "0xUser1".to_string(),
                    amount_usd: Amount::from_u128_with_scale(1000, 0),
                },
                timestamp: chrono::Utc::now(),
                vault_address: None,
                trader_address: Some(alloy_primitives::Address::repeat_byte(0x01)),
                tx_hash: None,
                correlation_id: Some(format!("test-order-1-idx-{}", index_id)),
            };

            let order2 = IndexOrder {
                index_id: *index_id,
                action: OrderAction::Deposit {
                    user_address: "0xUser2".to_string(),
                    amount_usd: Amount::from_u128_with_scale(500, 0),
                },
                timestamp: chrono::Utc::now(),
                vault_address: None,
                trader_address: Some(alloy_primitives::Address::repeat_byte(0x02)),
                tx_hash: None,
                correlation_id: Some(format!("test-order-2-idx-{}", index_id)),
            };

            accumulator.submit_order(order1)?;
            accumulator.submit_order(order2)?;
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(config.polling.batch_window_ms + 1000)).await;
        tracing::info!("Test complete");
    } else {

        // Create main keeper loop
        let keeper_loop_config = KeeperLoopConfig {
            polling_interval_secs: config.polling.interval_secs,
            health_check_interval_secs: 30,
        };

        // Create a separate vendor client for KeeperLoop (Story 2.5)
        // This allows KeeperLoop to make update_market_data calls independently
        let keeper_vendor_client = Arc::new(VendorClient::with_update_market_data_config(
            config.vendor.url.clone(),
            config.vendor.timeout_secs,
            config.vendor.retry_attempts,
            UpdateMarketDataConfig {
                endpoint: config.vendor.update_market_data_endpoint.clone(),
                timeout_secs: config.vendor.update_market_data_timeout_secs,
                retry_attempts: config.vendor.update_market_data_retry_attempts,
            },
        ));

        // Create StewardClient for asset extraction (Story 2.4)
        // Steward is a Castle Officer, so we use Castle address
        let steward_config = StewardClientConfig {
            steward_address: config.blockchain.castle_address.parse().unwrap_or_default(),
            rpc_url: config.blockchain.rpc_url.clone(),
            cache_ttl_secs: 300,
            refresh_interval_secs: 300,
        };
        let steward_client = Arc::new(StewardClient::new(steward_config));

        // Register the vault with the steward client so it knows how to look up assets
        // For now, use the vault_orders address as the default vault for index 10000
        if let Ok(vault_addr) = config.contracts.vault_orders.parse::<alloy_primitives::Address>() {
            steward_client.register_vault(10000, vault_addr).await;
            tracing::info!(vault = %vault_addr, "Registered vault with StewardClient");
        }

        let keeper_loop = Arc::new(KeeperLoop::with_asset_extractor(
            accumulator.clone(),
            quote_processor.clone(),
            submitter.clone(),
            steward_client,
            keeper_vendor_client,
            keeper_loop_config,
        ));

        let cancel_token = keeper_loop.cancel_token();

        // Spawn the keeper loop
        let keeper_handle = {
            let keeper_loop = keeper_loop.clone();
            tokio::spawn(async move {
                if let Err(e) = keeper_loop.run().await {
                    tracing::error!("Keeper loop error: {:?}", e);
                }
            })
        };

        let event_listener_handle = if !cli.no_event_listener {

            // Parse vault addresses from config (fallback)
            let config_vault_address: alloy_primitives::Address = config
                .contracts
                .vault_orders
                .parse()
                .unwrap_or_else(|_| {
                    tracing::warn!("Invalid vault_orders address, using default");
                    "0x4d856a5b7529edfd15ffaa7a36d2c7cfd52ac598"
                        .parse()
                        .unwrap()
                });

            // Create a separate StewardClient for vault discovery
            let discovery_steward_config = StewardClientConfig {
                steward_address: config.blockchain.castle_address.parse().unwrap_or_default(),
                rpc_url: config.blockchain.rpc_url.clone(),
                cache_ttl_secs: 300,
                refresh_interval_secs: 30, // Check for new vaults every 30s
            };
            let discovery_steward = Arc::new(StewardClient::new(discovery_steward_config));

            // Discover vaults from Stewart contract at startup
            let initial_vaults = match discovery_steward.refresh_vault_list().await {
                Ok(vaults) if !vaults.is_empty() => {
                    tracing::info!("🔍 Discovered {} vaults from Stewart", vaults.len());
                    vaults
                }
                Ok(_) => {
                    tracing::warn!("🔍 No vaults discovered from Stewart, using config fallback");
                    vec![config_vault_address]
                }
                Err(e) => {
                    tracing::warn!("🔍 Vault discovery failed: {:?}, using config fallback", e);
                    vec![config_vault_address]
                }
            };

            // Create event listener config with discovered vaults
            let listener_config = chain::EventListenerConfig {
                rpc_url: config.blockchain.rpc_url.clone(),
                ws_url: config.event_listener.ws_url.clone(),
                polling_interval_ms: config.event_listener.polling_interval_ms,
                reconnect_max_delay_ms: config.event_listener.reconnect_max_delay_ms,
                ws_failure_threshold: 3,
                vault_addresses: initial_vaults.clone(),
                pong_timeout_secs: 60,
            };

            // Create channel for events
            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<chain::VaultEvent>();

            // Create event listener
            let event_listener = chain::ChainEventListener::new(listener_config, event_tx);
            let listener_cancel = event_listener.cancel_token();

            // Backtrack historical events on startup (last 1000 blocks)
            let backtrack_blocks = std::env::var("BACKTRACK_BLOCKS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(1000);

            if backtrack_blocks > 0 {
                // Get current block to calculate from_block
                use alloy::providers::Provider;
                let backtrack_from = {
                    let provider = alloy::providers::ProviderBuilder::new()
                        .connect_http(config.blockchain.rpc_url.parse().unwrap());
                    let current = provider.get_block_number().await.unwrap_or(0);
                    current.saturating_sub(backtrack_blocks)
                };

                tracing::info!("⏪ Backtracking events from block {}", backtrack_from);
                match event_listener.backtrack_events(backtrack_from).await {
                    Ok(count) => {
                        tracing::info!("⏪ Backtrack complete: {} historical events processed", count);
                    }
                    Err(e) => {
                        tracing::warn!("⏪ Backtrack failed: {:?}", e);
                    }
                }
            }

            // Start periodic vault discovery refresh
            let event_listener_for_refresh = event_listener.clone();
            let discovery_steward_for_refresh = discovery_steward.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
                loop {
                    interval.tick().await;
                    // Refresh vaults from Stewart
                    if let Ok(vaults) = discovery_steward_for_refresh.refresh_vault_list().await {
                        let added = event_listener_for_refresh.add_vaults(vaults).await;
                        if added > 0 {
                            tracing::info!("🆕 Added {} new vaults from periodic refresh", added);
                        }
                    }
                }
            });

            // Wire event listener to accumulator
            // Story 2.8: Event processor handles both orders and lifecycle events
            let accumulator_for_events = accumulator.clone();

            // Story 2.8: Create ClaimHandler for event-driven claim processing
            let claim_handler_config = ClaimHandlerConfig::default().with_env_overrides();
            let claim_vendor_client = Arc::new(VendorClient::with_update_market_data_config(
                config.vendor.url.clone(),
                config.vendor.timeout_secs,
                config.vendor.retry_attempts,
                UpdateMarketDataConfig {
                    endpoint: config.vendor.update_market_data_endpoint.clone(),
                    timeout_secs: config.vendor.update_market_data_timeout_secs,
                    retry_attempts: config.vendor.update_market_data_retry_attempts,
                },
            ));
            let claim_handler = Arc::new(ClaimHandler::new(
                claim_handler_config,
                claim_vendor_client,
                quote_processor.index_mapper.clone(),
                submitter.clone(),
            ));

            tracing::debug!(threshold = claim_handler.threshold(), enabled = claim_handler.is_enabled(), "ClaimHandler ready");

            let event_processor_handle = tokio::spawn(async move {
                while let Some(event) = event_rx.recv().await {
                    let order = match &event {
                        chain::VaultEvent::Buy(e) => {
                            let correlation_id = format!("tx:{}", e.tx_hash);
                            tracing::debug!(correlation_id = %correlation_id, index_id = e.index_id, amount = e.collateral_amount, "Buy");
                            Some(IndexOrder {
                                index_id: e.index_id,
                                action: OrderAction::Deposit {
                                    user_address: format!("{:?}", e.trader),
                                    amount_usd: Amount::from_u128_raw(e.collateral_amount as u128),
                                },
                                timestamp: e.received_at,
                                vault_address: Some(e.vault_address),
                                trader_address: Some(e.trader), // Story 2.6: Preserve trader address
                                tx_hash: Some(e.tx_hash),
                                correlation_id: Some(correlation_id),
                            })
                        }
                        chain::VaultEvent::Sell(e) => {
                            let correlation_id = format!("tx:{}", e.tx_hash);
                            tracing::debug!(correlation_id = %correlation_id, index_id = e.index_id, amount = e.itp_amount, "Sell");
                            Some(IndexOrder {
                                index_id: e.index_id,
                                action: OrderAction::Withdraw {
                                    user_address: format!("{:?}", e.trader),
                                    amount_itp: Amount::from_u128_raw(e.itp_amount as u128),
                                },
                                timestamp: e.received_at,
                                vault_address: Some(e.vault_address),
                                trader_address: Some(e.trader), // Story 2.6: Preserve trader address
                                tx_hash: Some(e.tx_hash),
                                correlation_id: Some(correlation_id),
                            })
                        }
                        chain::VaultEvent::Acquisition(e) => {
                            tracing::debug!(index_id = e.index_id, "Acquisition");
                            None
                        }
                        chain::VaultEvent::Disposal(e) => {
                            tracing::debug!(index_id = e.index_id, "Disposal");
                            None
                        }
                        chain::VaultEvent::AcquisitionClaim(_) | chain::VaultEvent::DisposalClaim(_) => {
                            let result = claim_handler.handle_event(&event).await;
                            match result {
                                keeper::ClaimResult::Processed { index_id, .. } => {
                                    tracing::debug!(index_id, "Claim processed");
                                }
                                keeper::ClaimResult::BelowThreshold { .. } | keeper::ClaimResult::Disabled | keeper::ClaimResult::NotAClaim => {}
                                keeper::ClaimResult::Error { index_id, error } => {
                                    tracing::error!(index_id, %error, "Claim failed");
                                }
                            }
                            None
                        }
                    };

                    if let Some(order) = order {
                        if let Err(e) = accumulator_for_events.submit_order(order) {
                            tracing::error!(?e, "Submit failed");
                        }
                    }
                }
            });

            // Start event listener
            let listener_handle = tokio::spawn(async move {
                if let Err(e) = event_listener.start().await {
                    tracing::error!("Event listener error: {:?}", e);
                }
            });

            tracing::info!(mode = %config.event_listener.mode, vaults = initial_vaults.len(), "Event listener started");
            Some((listener_handle, event_processor_handle, listener_cancel))
        } else {
            tracing::warn!("Event listener disabled");
            None
        };

        tracing::info!(events = event_listener_handle.is_some(), "Keeper running");

        // Wait for shutdown signal
        tokio::signal::ctrl_c().await?;
        cancel_token.cancel();

        if let Some((listener_handle, processor_handle, listener_cancel)) = event_listener_handle {
            listener_cancel.cancel();
            let _ = listener_handle.await;
            let _ = processor_handle.await;
        }

        let _ = keeper_handle.await;
    }

    tracing::info!("Keeper stopped");
    Ok(())
}