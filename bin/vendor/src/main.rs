use clap::Parser;
use config::VendorConfig;
use eyre::Result;
use market_data::{BitgetSubscriber, BitgetSubscriberConfig, MarketDataEvent, MarketDataObserver, Subscription};
use parking_lot::RwLock as AtomicLock;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod config;
mod market_data;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Path to configuration file
    #[arg(short, long)]
    config: Option<String>,

    /// Symbols to subscribe (comma-separated)
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
    let config = if let Some(config_path) = cli.config {
        VendorConfig::from_file(&config_path)?
    } else {
        VendorConfig::default()
    };

    tracing::info!("Configuration loaded");

    // Create market data observer
    let observer = Arc::new(AtomicLock::new(MarketDataObserver::new()));

    // Subscribe to market data events
    {
        let mut obs = observer.write();
        obs.subscribe(|event: Arc<MarketDataEvent>| {
            match event.as_ref() {
                MarketDataEvent::OrderBookSnapshot { symbol, bid_updates, ask_updates, .. } => {
                    tracing::info!(
                        "📸 Snapshot: {} - Bids: {}, Asks: {}",
                        symbol,
                        bid_updates.len(),
                        ask_updates.len()
                    );
                }
                MarketDataEvent::OrderBookDelta { symbol, bid_updates, ask_updates, .. } => {
                    tracing::debug!(
                        "📊 Update: {} - Bids: {}, Asks: {}",
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
            }
        });
    }

    // Create Bitget subscriber
    let bitget_config = BitgetSubscriberConfig {
        websocket_url: config.market_data.bitget.websocket_url.clone(),
        subscription_limit_rate: config.market_data.bitget.subscription_limit_rate,
        stale_check_period: Duration::from_secs(config.market_data.bitget.stale_check_period_secs),
        stale_timeout: chrono::Duration::seconds(config.market_data.bitget.stale_timeout_secs),
        heartbeat_interval: Duration::from_secs(config.market_data.bitget.heartbeat_interval_secs),
    };

    let mut bitget_subscriber = BitgetSubscriber::new(bitget_config, observer.clone());

    // Create subscription channel
    let (subscription_tx, subscription_rx) = unbounded_channel();

    // Start subscriber
    bitget_subscriber.start(subscription_rx).await?;

    tracing::info!("Bitget subscriber started");

    // Subscribe to symbols from CLI or defaults
    let symbols = if cli.symbols.is_empty() {
        vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()]
    } else {
        cli.symbols
    };

    for symbol in symbols {
        tracing::info!("Subscribing to {}", symbol);
        subscription_tx.send(Subscription {
            ticker: symbol.clone(),
            exchange: "Bitget".to_string(),
        })?;
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