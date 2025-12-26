use clap::Parser;
use config::VendorConfig;
use eyre::Result;
use market_data::{BitgetSubscriber, BitgetSubscriberConfig, MarketDataEvent, MarketDataObserver, Subscription};
use parking_lot::RwLock as AtomicLock;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod basket;
mod config;
mod market_data;

use basket::BasketManager;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Path to configuration directory (e.g., ./configs/dev)
    #[arg(long)]
    config_path: Option<String>,

    /// Symbols to subscribe (comma-separated) - used if config_path not provided
    #[arg(short, long, value_delimiter = ',')]
    symbols: Vec<String>,

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
    let (symbols, basket_manager) = if let Some(config_path) = cli.config_path {
        tracing::info!("Loading indices from config path: {}", config_path);

        let basket_manager = BasketManager::load_from_config(&config_path).await?;

        tracing::info!("{}", basket_manager.summary());

        let symbols = basket_manager.get_all_unique_symbols();

        tracing::info!("Index symbols: {:?}", basket_manager.get_index_symbols());
        tracing::info!("Asset symbols to subscribe: {:?}", symbols);

        (symbols, Some(basket_manager))
    } else if !cli.symbols.is_empty() {
        tracing::info!("Using symbols from CLI: {:?}", cli.symbols);
        (cli.symbols, None)
    } else {
        // Default symbols
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

    // Subscribe to market data events
    {
        let mut obs = observer.write();
        obs.subscribe(|event: Arc<MarketDataEvent>| match event.as_ref() {
            MarketDataEvent::OrderBookSnapshot {
                symbol,
                bid_updates,
                ask_updates,
                ..
            } => {
                if !bid_updates.is_empty() && !ask_updates.is_empty() {
                    let best_bid = &bid_updates[0];
                    let best_ask = &ask_updates[0];
                    tracing::info!(
                        "📸 Snapshot: {} - Bid: {:.8} ({:.8}) | Ask: {:.8} ({:.8}) | Levels: {}b/{}a",
                        symbol,
                        best_bid.price,
                        best_bid.quantity,
                        best_ask.price,
                        best_ask.quantity,
                        bid_updates.len(),
                        ask_updates.len()
                    );
                }
            }
            MarketDataEvent::OrderBookDelta {
                symbol,
                bid_updates,
                ask_updates,
                ..
            } => {
                tracing::debug!(
                    "📊 Update: {} - Changes: {}b/{}a",
                    symbol,
                    bid_updates.len(),
                    ask_updates.len()
                );
            }
            MarketDataEvent::TopOfBook {
                symbol,
                best_bid_price,
                best_ask_price,
                best_bid_quantity,
                best_ask_quantity,
                ..
            } => {
                tracing::info!(
                    "💹 TOB: {} - Bid: {:.8} ({:.8}) | Ask: {:.8} ({:.8})",
                    symbol,
                    best_bid_price,
                    best_bid_quantity,
                    best_ask_price,
                    best_ask_quantity
                );
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
    for symbol in symbols {
        tracing::info!("Subscribing to {}", symbol);
        subscription_tx.send(Subscription {
            ticker: symbol.clone(),
            exchange: "Bitget".to_string(),
        })?;
    }

    // Store basket manager for future use
    if let Some(manager) = basket_manager {
        tracing::info!("Basket manager available for future on-chain interactions");
        // You can store this in a global state or pass it around as needed
        // For now, we just log that it's available
        drop(manager);
    }

    // Keep running
    tracing::info!("Vendor running. Press Ctrl+C to stop.");
    tokio::signal::ctrl_c().await?;

    // Cleanup
    tracing::info!("Shutting down...");
    bitget_subscriber.stop().await?;

    tracing::info!("Vendor stopped");
    Ok(())
}