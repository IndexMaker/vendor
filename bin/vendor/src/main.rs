use alloy_primitives::Address;
use clap::Parser;
use config::VendorConfig;
use eyre::Result;
use market_data::{BitgetSubscriber, BitgetSubscriberConfig, MarketDataEvent, MarketDataObserver, Subscription};
use onchain::{AssetMapper, PriceTracker};
use parking_lot::RwLock as AtomicLock;
use crate::api::{ApiServer, AppState};
use std::net::SocketAddr;
use std::{path::PathBuf, sync::Arc};
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use alloy::providers::ProviderBuilder;

mod api;
mod config;
mod market_data;
mod onchain;
mod order_sender;
mod margin;
mod supply;
mod rebalance;
mod order_book;
mod delta_rebalancer;


#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Path to configuration directory (e.g., ./configs/dev)
    #[arg(long)]
    config_path: Option<String>,

    /// Symbols to subscribe (comma-separated) - used if config_path not provided
    #[arg(short, long, value_delimiter = ',')]
    symbols: Vec<String>,

    /// RPC URL for blockchain connection
    #[arg(long)]
    rpc_url: Option<String>,

    // /// Private key for transaction signing
    // #[arg(long)]
    // private_key: Option<String>,

    /// Castle contract address
    #[arg(long)]
    castle_address: Option<String>,

    /// Enable onchain submissions
    #[arg(long)]
    enable_onchain: bool,

    /// Log level (trace, debug, info, warn, error)
    #[arg(short, long, default_value = "info")]
    log_level: String,

    /// API server port
    #[arg(long, default_value = "8080")]
    api_port: u16,

    /// Price limit spread in basis points (default: 5 = 0.05%)
    #[arg(long, default_value = "5")]
    price_limit_bps: u16,
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

    tracing::info!("Starting VaultWorks Vendor");

    // Initialize OrderSender (before inventory)
    tracing::info!("Initializing OrderSender...");

    let order_sender_mode = if let Ok(credentials) = order_sender::BitgetCredentials::from_env() {
        let trading_enabled = order_sender::BitgetCredentials::trading_enabled_from_env();
        tracing::info!("Using Bitget order sender (trading: {})", trading_enabled);
        order_sender::OrderSenderMode::Bitget(credentials)
    } else {
        tracing::warn!("No Bitget credentials - using simulated order sender");
        order_sender::OrderSenderMode::Simulated { failure_rate: 0.0 }
    };

    let order_sender_config = order_sender::OrderSenderConfig::builder()
        .mode(order_sender_mode)
        .price_limit_bps(cli.price_limit_bps)
        .retry_attempts(
            std::env::var("RETRY_ATTEMPTS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3),
        )
        .trading_enabled(order_sender::BitgetCredentials::trading_enabled_from_env())
        .build()?;

    order_sender_config.start().await?;
    let order_sender = order_sender_config.get_sender();
    tracing::info!("✓ OrderSender initialized");

    // Load configuration
    let config = VendorConfig::default();

    // Access margin config
    let margin_config = config.margin.clone();
    let min_order = margin_config.min_order_size_usd;
    let total_exp = margin_config.total_exposure_usd;

    tracing::info!("Margin config: min_order=${}, total_exposure=${}", min_order, total_exp);

    // Load asset mapper and symbols
    let (symbols, asset_mapper_locked) = if let Some(config_path) = &cli.config_path {
        tracing::info!("Loading configuration from: {}", config_path);

        // Load asset mapper
        let asset_path = PathBuf::from(config_path).join("assets.json");
        let asset_mapper_raw = AssetMapper::load_from_file(&asset_path).await?;
        let asset_mapper = Arc::new(asset_mapper_raw);
        let asset_mapper_locked = Arc::new(parking_lot::RwLock::new(asset_mapper.as_ref().clone()));

        // Get symbols from asset mapper
        let symbols = asset_mapper.get_all_symbols();

        tracing::info!("Loaded {} assets from config", symbols.len());
        tracing::info!("Asset symbols: {:?}", symbols);

        (symbols, Some(asset_mapper_locked))
    } else if !cli.symbols.is_empty() {
        tracing::info!("Using symbols from CLI: {:?}", cli.symbols);
        (cli.symbols, None)
    } else {
        let default_symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        tracing::info!("Using default symbols: {:?}", default_symbols);
        (default_symbols, None)
    };

    if symbols.is_empty() {
        tracing::error!("No symbols to subscribe. Exiting.");
        return Ok(());
    }

    // Create market data observer
    let observer = Arc::new(AtomicLock::new(MarketDataObserver::new()));

    // Create price tracker
    let price_tracker = Arc::new(PriceTracker::new());

    // Create OrderBookProcessor
    let order_book_config = order_book::OrderBookConfig {
        depth_levels: 5,  // K = 5 levels
    };

    let order_book_processor = if let Some(ref asset_mapper_arc) = asset_mapper_locked {
        let processor = order_book::OrderBookProcessor::new(
            price_tracker.clone(),
            asset_mapper_arc.clone(),
            order_book_config,
        );

        Some(Arc::new(processor))
    } else {
        None
    };

    if order_book_processor.is_some() {
        tracing::info!("✓ OrderBookProcessor initialized (K=5 levels)");
    }

    // Initialize StalenessManager (only if asset_mapper exists and onchain enabled)
    let staleness_manager = if let Some(ref asset_mapper_arc) = asset_mapper_locked {
        // Check if we have RPC configuration for on-chain reading
        if let Some(castle_address) = &config.blockchain.castle_address {
            let castle_addr: Address = castle_address.parse()?;

            // Create provider for reading on-chain data
            let provider = ProviderBuilder::new()
                .connect_http(config.blockchain.rpc_url.parse()?);

            // Create OnchainReader
            let onchain_reader = onchain::OnchainReader::new(
                provider,
                castle_addr,
                config.blockchain.vendor_id,
            );

            // Create StalenessManager with reader
            let mgr = onchain::StalenessManager::new(
                config.staleness.clone(),
                price_tracker.clone(),
                asset_mapper_arc.clone(),
                onchain_reader,
            );

            tracing::info!("✓ StalenessManager initialized with on-chain reading");
            Some(Arc::new(parking_lot::RwLock::new(mgr)))
        } else {
            tracing::warn!("StalenessManager disabled (no castle_address in config)");
            None
        }
    } else {
        None
    };

    if staleness_manager.is_some() {
        tracing::info!("✓ StalenessManager initialized");
    }

    // Initialize SupplyManager (only if asset_mapper_locked exists)
    let supply_manager = if let Some(ref asset_mapper_arc) = asset_mapper_locked {
        let mut mgr = supply::SupplyManager::new(asset_mapper_arc.clone());

        if !symbols.is_empty() {
            if let Err(e) = mgr.initialize(&symbols) {
                tracing::warn!("Failed to initialize supply tracking: {:?}", e);
            }
        }

        Some(Arc::new(parking_lot::RwLock::new(mgr)))
    } else {
        None
    };

    if supply_manager.is_some() {
        tracing::info!("✓ SupplyManager initialized");
    }

    // Inventory manager removed - Vendor is now asset-only
    // Order execution will be handled by Keeper
    tracing::info!("Vendor running in asset-only mode (no index tracking)");

    // API server will be added later with /quote_assets endpoint
    // For now, Vendor only processes market data
    tracing::info!("API server disabled (will add /quote_assets endpoint next)");

    // Subscribe to market data events
    {
        let mut obs = observer.write();
        let price_tracker_clone = price_tracker.clone();

        obs.subscribe(move |event: Arc<MarketDataEvent>| {
            // Update price tracker
            price_tracker_clone.handle_event(event.clone());

            // Log events
            match event.as_ref() {
                MarketDataEvent::OrderBookSnapshot {
                    symbol,
                    bid_updates,
                    ask_updates,
                    ..
                } => {
                    if !bid_updates.is_empty() && !ask_updates.is_empty() {
                        let best_bid = &bid_updates[0];
                        let best_ask = &ask_updates[0];
                        tracing::debug!(
                            "📸 Snapshot: {} - Bid: {:.8} | Ask: {:.8} | Levels: {}b/{}a",
                            symbol,
                            best_bid.price,
                            best_ask.price,
                            bid_updates.len(),
                            ask_updates.len()
                        );
                    }
                }
                MarketDataEvent::OrderBookDelta { symbol, .. } => {
                    tracing::trace!("📊 Update: {}", symbol);
                }
                MarketDataEvent::TopOfBook {
                    symbol,
                    best_bid_price,
                    best_ask_price,
                    ..
                } => {
                    tracing::info!(
                        "💹 TOB: {} - Bid: {:.8} | Ask: {:.8}",
                        symbol,
                        best_bid_price,
                        best_ask_price
                    );
                }
            }
        });
    }

    // Create Bitget subscriber
    let bitget_config = BitgetSubscriberConfig {
        websocket_url: config.market_data.bitget.websocket_url.clone(),
        subscription_limit_rate: config.market_data.bitget.subscription_limit_rate,
        stale_check_period: Duration::from_secs(config.market_data.bitget.stale_check_period_secs),
        stale_timeout: chrono::Duration::seconds(config.market_data.bitget.stale_timeout_secs),
        heartbeat_interval: Duration::from_secs(
            config.market_data.bitget.heartbeat_interval_secs,
        ),
    };

    let mut bitget_subscriber = BitgetSubscriber::new(bitget_config, observer.clone());

    // Create subscription channel
    let (subscription_tx, subscription_rx) = unbounded_channel();

    // Start subscriber
    bitget_subscriber.start(subscription_rx).await?;
    tracing::info!("Bitget subscriber started");

    // Subscribe to all symbols
    for symbol in &symbols {
        tracing::info!("Subscribing to {}", symbol);
        subscription_tx.send(Subscription {
            ticker: symbol.clone(),
            exchange: "Bitget".to_string(),
        })?;
    }

    // Onchain submitter removed - Vendor only provides quotes
    // Keeper will handle on-chain submissions
    tracing::info!("On-chain submissions disabled (Keeper's responsibility)");

    // Start API server
    let api_server = if let (Some(asset_mapper_arc), Some(staleness_mgr), Some(ob_processor)) = 
        (&asset_mapper_locked, &staleness_manager, &order_book_processor) 
    {
        let api_addr: SocketAddr = format!("0.0.0.0:{}", cli.api_port).parse()?;

        let app_state = AppState {
            vendor_id: config.blockchain.vendor_id,
            asset_mapper: asset_mapper_arc.clone(),
            staleness_manager: staleness_mgr.clone(),
            order_book_processor: ob_processor.clone(),
        };

        let server = ApiServer::new(app_state, api_addr);
        let cancel_token = server.cancel_token();

        tokio::spawn(async move {
            if let Err(e) = server.start().await {
                tracing::error!("API server failed: {:?}", e);
            }
        });

        tracing::info!("✓ API server started on http://0.0.0.0:{}", cli.api_port);

        Some(cancel_token)
    } else {
        tracing::warn!("API server disabled (missing dependencies)");
        None
    };

    let _delta_rebalancer = if let (Some(asset_mapper_arc), Some(supply_mgr)) = 
        (&asset_mapper_locked, &supply_manager) 
    {
        if let Some(castle_address) = &config.blockchain.castle_address {
            let castle_addr: Address = castle_address.parse()?;
            
            let provider = ProviderBuilder::new()
                .connect_http(config.blockchain.rpc_url.parse()?);
            
            let onchain_reader = Arc::new(onchain::OnchainReader::new(
                provider,
                castle_addr,
                config.blockchain.vendor_id,
            ));
            
            let rebalancer_config = delta_rebalancer::RebalancerConfig {
                vendor_id: config.blockchain.vendor_id,
                min_order_size_usd: config.margin.min_order_size_usd,
                total_exposure_usd: config.margin.total_exposure_usd,
                rebalance_interval_secs: 60,
                enable_onchain_submit: false,
            };
            
            let rebalancer = Arc::new(delta_rebalancer::DeltaRebalancer::new(
                rebalancer_config,
                onchain_reader,
                supply_mgr.clone(),
                price_tracker.clone(),
                asset_mapper_arc.clone(),
                order_sender,
            ));
            
            tokio::spawn({
                let rebalancer = rebalancer.clone();
                async move {
                    rebalancer.run().await;
                }
            });
            
            tracing::info!("✓ DeltaRebalancer started (60s interval)");
            Some(rebalancer)
        } else {
            tracing::warn!("DeltaRebalancer disabled (no castle_address)");
            None
        }
    } else {
        tracing::warn!("DeltaRebalancer disabled (missing dependencies)");
        None
    };

    // Keep running
    tracing::info!("Vendor running. Press Ctrl+C to stop.");
    tokio::signal::ctrl_c().await?;

    // Cleanup
    tracing::info!("Shutting down...");
    bitget_subscriber.stop().await?;
    

    order_sender_config.stop().await?;
    tracing::info!("✓ OrderSender stopped");

    if let Some(api_cancel) = api_server {
        api_cancel.cancel();
    }

    tracing::info!("Vendor stopped");
    Ok(())
}