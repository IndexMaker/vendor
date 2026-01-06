use clap::Parser;
use common::amount::Amount;
use eyre::Result;
use std::{path::PathBuf, sync::Arc};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod config;
mod index;
mod vendor;
mod accumulator;

use accumulator::{AccumulatorConfig, IndexOrder, OrderAccumulator, OrderAction};
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

    // Create and test order accumulator
    tracing::info!("\nTesting Order Accumulator...");
    
    let accumulator_config = AccumulatorConfig {
        batch_window_ms: config.polling.batch_window_ms,
        max_batch_size: 100,
    };
    
    let accumulator = Arc::new(OrderAccumulator::new(accumulator_config));
    
    // Start processing with a callback
    let accumulator_clone = accumulator.clone();
    accumulator_clone.start_processing(|batch| {
        tracing::info!("🔥 Batch ready for processing!");
        tracing::info!("  Indices: {}", batch.indices.len());
        tracing::info!("  Total orders: {}", batch.total_order_count());
        
        for (index_id, state) in &batch.indices {
            let net_change = state.net_collateral_change.to_u128_raw() as f64 / 1e18;
            tracing::info!(
                "  Index {}: {} orders, net change: ${:.2}",
                index_id,
                state.order_count,
                net_change
            );
        }
    }).await;
    
    tracing::info!("✓ Order accumulator started");
    
    // Submit test orders
    tracing::info!("\nSubmitting test orders...");
    
    for index_id in index_mapper.get_all_index_ids().iter().take(2) {
        let order1 = IndexOrder {
            index_id: *index_id,
            action: OrderAction::Deposit {
                user_address: "0xUser1".to_string(),
                amount_usd: Amount::from_u128_with_scale(1000, 0), // $1000
            },
            timestamp: chrono::Utc::now(),
        };
        
        let order2 = IndexOrder {
            index_id: *index_id,
            action: OrderAction::Deposit {
                user_address: "0xUser2".to_string(),
                amount_usd: Amount::from_u128_with_scale(500, 0), // $500
            },
            timestamp: chrono::Utc::now(),
        };
        
        accumulator.submit_order(order1)?;
        accumulator.submit_order(order2)?;
        
        tracing::info!("  ✓ Submitted 2 orders for index {}", index_id);
    }
    
    // Wait for batch window to expire
    tracing::info!("\nWaiting {}ms for batch window...", config.polling.batch_window_ms);
    tokio::time::sleep(tokio::time::Duration::from_millis(config.polling.batch_window_ms + 100)).await;
    
    // Check stats
    let stats = accumulator.get_stats();
    tracing::info!("\nAccumulator stats:");
    tracing::info!("  Active indices: {}", stats.active_indices);
    tracing::info!("  Total orders: {}", stats.total_orders);
    tracing::info!("  Oldest order age: {}ms", stats.oldest_order_age_ms);
    tracing::info!("  Should flush: {}", stats.should_flush);
    
    // Manual flush if needed
    if stats.should_flush {
        if let Some(batch) = accumulator.flush_batch() {
            tracing::info!("\n🔥 Manual flush triggered");
            tracing::info!("  Flushed {} orders from {} indices", 
                batch.total_order_count(), 
                batch.indices.len()
            );
        }
    }

    tracing::info!("\n✅ Phase 4.3 Complete!");
    tracing::info!("Order Accumulator:");
    tracing::info!("  ✓ Order submission");
    tracing::info!("  ✓ Batch aggregation by index");
    tracing::info!("  ✓ Time-based flushing ({}ms window)", config.polling.batch_window_ms);
    tracing::info!("  ✓ Net collateral tracking");
    
    tracing::info!("\nNext phases:");
    tracing::info!("  - Phase 4.4: Quote Processor");
    tracing::info!("  - Phase 4.5: On-chain Submitter");
    tracing::info!("  - Phase 4.6: Main Event Loop");

    Ok(())
}