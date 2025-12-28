use clap::Parser;
use config::VendorConfig;
use eyre::Result;
use market_data::{BitgetSubscriber, BitgetSubscriberConfig, MarketDataEvent, MarketDataObserver, Subscription};
use onchain::{AssetMapper, IndexMapper, OnchainSubmitter, OnchainSubmitterConfig, PriceTracker};
use parking_lot::RwLock as AtomicLock;
use parking_lot::lock_api::RwLock;
use std::{path::PathBuf, sync::Arc};
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use alloy::{
    network::EthereumWallet,
    providers::ProviderBuilder,
    signers::local::PrivateKeySigner,
};

mod basket;
mod config;
mod market_data;
mod onchain;
mod inventory;

use basket::BasketManager;
use inventory::InventoryManager;

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

    /// Private key for transaction signing
    #[arg(long)]
    private_key: Option<String>,

    /// Castle contract address
    #[arg(long)]
    castle_address: Option<String>,

    /// Enable onchain submissions
    #[arg(long)]
    enable_onchain: bool,

    /// Log level (trace, debug, info, warn, error)
    #[arg(short, long, default_value = "info")]
    log_level: String,
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

    // Load configuration
    let config = VendorConfig::default();

    // Load basket manager if config path provided
    let (symbols, basket_manager, asset_mapper, index_mapper) = if let Some(config_path) = &cli.config_path {
        tracing::info!("Loading configuration from: {}", config_path);

        // Load basket manager
        let basket_manager = BasketManager::load_from_config(config_path).await?;
        tracing::info!("{}", basket_manager.summary());

        // Load asset mapper
        let asset_path = PathBuf::from(config_path).join("assets.json");
        let asset_mapper = AssetMapper::load_from_file(&asset_path).await?;

        // Load index mapper
        let index_path = PathBuf::from(config_path).join("index_ids.json");
        let index_mapper = IndexMapper::load_from_file(&index_path).await?;

        // Get symbols
        let symbols = basket_manager.get_all_unique_symbols();

        // Validate all assets have IDs
        asset_mapper.validate_all_mapped(&symbols)?;
        tracing::info!("✓ All assets have valid ID mappings");

        // Validate all indices have IDs
        let index_symbols = basket_manager.get_index_symbols();
        index_mapper.validate_all_indices(&index_symbols)?;
        tracing::info!("✓ All indices have valid ID mappings");

        tracing::info!("Index symbols: {:?}", index_symbols);
        tracing::info!("Asset symbols to subscribe: {:?}", symbols);

        // NOW wrap in Arc<RwLock> after validation
        let basket_manager = Arc::new(RwLock::new(basket_manager));

        (symbols, Some(basket_manager), Some(asset_mapper), Some(index_mapper))
    } else if !cli.symbols.is_empty() {
        tracing::info!("Using symbols from CLI: {:?}", cli.symbols);
        (cli.symbols, None, None, None)
    } else {
        let default_symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        tracing::info!("Using default symbols: {:?}", default_symbols);
        (default_symbols, None, None, None)
    };

    if symbols.is_empty() {
        tracing::error!("No symbols to subscribe. Exiting.");
        return Ok(());
    }

    // Create market data observer
    let observer = Arc::new(AtomicLock::new(MarketDataObserver::new()));

    // Create price tracker
    let price_tracker = Arc::new(PriceTracker::new());

    // Create inventory manager (only if basket_manager exists)
    let inventory = if let Some(ref bm) = basket_manager {
        let inventory_path = PathBuf::from("./data/inventory_manager.json");
        let inv = InventoryManager::load_from_storage(
            bm.clone(),
            price_tracker.clone(),
            inventory_path,
        )
        .await?;

        tracing::info!("{}", inv.summary());

        Some(Arc::new(AtomicLock::new(inv)))
    } else {
        tracing::info!("No basket manager available, inventory manager disabled");
        None
    };

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

    // Start onchain submitter if enabled
    let onchain_submitter = if cli.enable_onchain {
        if let (Some(basket_manager), Some(asset_mapper), Some(index_mapper)) =
            (basket_manager, asset_mapper, index_mapper)
        {
            tracing::info!("Onchain submission enabled");

            // Setup blockchain connection
            let rpc_url = cli
                .rpc_url
                .unwrap_or_else(|| config.blockchain.rpc_url.clone());
            let private_key = cli
                .private_key
                .ok_or_else(|| eyre::eyre!("Private key required for onchain submissions"))?;
            let castle_address = cli
                .castle_address
                .ok_or_else(|| eyre::eyre!("Castle address required for onchain submissions"))?
                .parse()?;

            // Create signer and provider
            let signer: PrivateKeySigner = private_key.parse()?;
            let wallet = EthereumWallet::from(signer);
            let provider = ProviderBuilder::new()
                .with_gas_estimation()
                .with_simple_nonce_management()
                .wallet(wallet)
                .connect_http(rpc_url.parse()?);

            // Create submitter config
            let submitter_config = OnchainSubmitterConfig {
                vendor_id: 1,
                castle_address,
                submission_interval: Duration::from_secs(20),      // Market data every 20s
                sync_check_interval: Duration::from_secs(300),     // Sync check every 5 min
                default_liquidity: 0.5,
                default_slope: 1.0,
            };

            // Create submitter
            let submitter = OnchainSubmitter::new(
                submitter_config,
                provider,
                Arc::new(asset_mapper),
                Arc::new(index_mapper),
                basket_manager,
                price_tracker.clone(),
            );

            // Wrap in Arc for sharing between tasks
            let submitter = Arc::new(submitter);

            // Initialize (one-time setup)
            tracing::info!("Initializing on-chain state...");
            submitter.initialize().await?;

            // Start periodic market data submissions
            submitter.start().await?;  // No need for clone, just & reference
            tracing::info!("Onchain submitter started (market data: 20s)");

            // Spawn periodic sync task for new assets/indices
            {
                let submitter_for_sync = submitter.clone();
                tokio::spawn(async move {
                    let mut sync_interval = tokio::time::interval(Duration::from_secs(300)); // 5 minutes

                    loop {
                        sync_interval.tick().await;

                        tracing::debug!("Running periodic sync check for new assets/indices");
                        if let Err(e) = submitter_for_sync.sync_new_additions().await {
                            tracing::error!("Periodic sync failed: {:?}", e);
                        }
                    }
                });
            }
            tracing::info!("Periodic sync task started (every 5 minutes)");

            Some(submitter)
        } else {
            tracing::warn!("Onchain submission enabled but no config path provided");
            None
        }
    } else {
        None
    };

    // Keep running
    tracing::info!("Vendor running. Press Ctrl+C to stop.");
    tokio::signal::ctrl_c().await?;

    // Cleanup
    tracing::info!("Shutting down...");
    bitget_subscriber.stop().await?;
    if let Some(submitter) = onchain_submitter {
        submitter.stop().await;
    }

    tracing::info!("Vendor stopped");
    Ok(())
}