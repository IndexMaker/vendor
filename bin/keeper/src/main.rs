use clap::Parser;
use eyre::Result;
use std::path::PathBuf;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod config;
mod index;
mod vendor;

use config::KeeperConfig;
use index::IndexMapper;
use vendor::{QuoteCache, VendorClient};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Path to configuration directory
    #[arg(long, default_value = "./configs/dev")]
    config_path: String,

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

    tracing::info!("Starting VaultWorks Keeper");

    // Load configuration
    let config_dir = PathBuf::from(&cli.config_path);
    let keeper_config_path = config_dir.join("keeper.json");
    let indices_config_path = config_dir.join("indices.json");

    let config = if keeper_config_path.exists() {
        KeeperConfig::load_from_file(&keeper_config_path).await?
    } else {
        tracing::warn!("keeper.json not found, using defaults");
        KeeperConfig::default()
    };

    tracing::info!("✓ Configuration loaded");
    tracing::info!("  Keeper ID: {}", config.keeper_id);
    tracing::info!("  Vendor URL: {}", config.vendor.url);
    tracing::info!("  Polling interval: {}s", config.polling.interval_secs);

    // Load index mapper
    let index_mapper = if indices_config_path.exists() {
        IndexMapper::load_from_file(&indices_config_path).await?
    } else {
        tracing::warn!("indices.json not found, using empty mapper");
        IndexMapper::new()
    };

    tracing::info!("✓ Loaded {} indices", index_mapper.len());
    for index_id in index_mapper.get_all_index_ids() {
        if let Some(index) = index_mapper.get_index(index_id) {
            tracing::info!(
                "  Index {}: {} ({} assets)",
                index_id,
                index.name,
                index.assets.len()
            );
        }
    }

    // Create vendor client
    let vendor_client = VendorClient::new(
        config.vendor.url.clone(),
        config.vendor.timeout_secs,
        config.vendor.retry_attempts,
    );

    // Create quote cache (5 second TTL)
    let quote_cache = QuoteCache::new(5);

    // Test vendor connection
    tracing::info!("Testing vendor connection...");
    match vendor_client.health_check().await {
        Ok(health) => {
            tracing::info!("✓ Vendor connection successful");
            tracing::info!("  Status: {}", health.status);
            tracing::info!("  Vendor ID: {}", health.vendor_id);
            tracing::info!("  Tracked Assets: {}", health.tracked_assets);
        }
        Err(e) => {
            tracing::error!("✗ Vendor connection failed: {:?}", e);
            tracing::warn!("Keeper will continue but may not function correctly");
        }
    }

    // Test quote request
    let all_assets = index_mapper.get_all_asset_ids();
    if !all_assets.is_empty() {
        tracing::info!("Testing quote request for {} assets...", all_assets.len());
        
        match vendor_client.quote_assets(all_assets.clone()).await {
            Ok(quote) => {
                tracing::info!("✓ Quote request successful");
                tracing::info!("  Requested: {} assets", all_assets.len());
                tracing::info!("  Stale: {} assets", quote.len());
                
                if !quote.is_empty() {
                    tracing::info!("  Sample data:");
                    for i in 0..quote.len().min(3) {
                        tracing::info!(
                            "    Asset {}: L={:.2}, P=${:.2}, S={:.6}",
                            quote.assets[i],
                            quote.liquidity[i],
                            quote.prices[i],
                            quote.slopes[i]
                        );
                    }
                    
                    // Store in cache
                    quote_cache.put(all_assets, quote);
                    tracing::info!("  ✓ Quote cached");
                }
            }
            Err(e) => {
                tracing::error!("✗ Quote request failed: {:?}", e);
            }
        }
    }

    tracing::info!("\nKeeper foundation initialized successfully!");
    tracing::info!("Next phases:");
    tracing::info!("  - Phase 4.3: Order Accumulator");
    tracing::info!("  - Phase 4.4: Quote Processor");
    tracing::info!("  - Phase 4.5: On-chain Submitter");
    tracing::info!("  - Phase 4.6: Main Event Loop");

    Ok(())
}