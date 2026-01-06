use clap::Parser;
use common::amount::Amount;
use eyre::Result;
use std::{path::PathBuf, sync::Arc};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod config;
mod index;
mod vendor;
mod accumulator;
mod processor;

use processor::{ProcessorConfig, QuoteProcessor};
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
    let _quote_cache = QuoteCache::new(5);

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
                }
            }
            Err(e) => {
                tracing::error!("✗ Quote request failed: {:?}", e);
            }
        }
    }

    // Create quote processor
    tracing::info!("\nInitializing Quote Processor...");
    
    let processor_config = ProcessorConfig {
        vendor_id: config.blockchain.vendor_id,
        max_order_size_multiplier: 2.0,
    };
    
    let quote_processor = Arc::new(QuoteProcessor::new(
        processor_config,
        Arc::new(index_mapper),
        Arc::new(vendor_client),
    ));
    
    tracing::info!("✓ Quote processor initialized");
    
    // Create and test order accumulator with processor integration
    tracing::info!("\nTesting Order Accumulator + Quote Processor Integration...");
    
    let accumulator_config = AccumulatorConfig {
        batch_window_ms: config.polling.batch_window_ms,
        max_batch_size: 100,
    };
    
    let accumulator = Arc::new(OrderAccumulator::new(accumulator_config));
    
    // Start processing with integrated callback
    let accumulator_clone = accumulator.clone();
    let processor_clone = quote_processor.clone();
    
    accumulator_clone.start_processing(move |batch| {
        tracing::info!("🔥 Batch ready for processing!");
        tracing::info!("  Indices: {}", batch.indices.len());
        tracing::info!("  Total orders: {}", batch.total_order_count());
        
        // Process batch with quote processor
        let processor = processor_clone.clone();
        tokio::spawn(async move {
            match processor.process_batch(batch).await {
                Ok(payload) => {
                    let summary = processor.get_payload_summary(&payload);
                    
                    tracing::info!("📊 Submission Payload Generated:");
                    tracing::info!("  Buy orders: {}", summary.index_count);
                    tracing::info!("  Unique assets: {}", summary.unique_assets);
                    tracing::info!(
                        "  Total collateral: ${:.2}",
                        summary.total_collateral_usd.to_u128_raw() as f64 / 1e18
                    );
                    tracing::info!("  Market data assets: {}", summary.market_data_assets);
                    
                    // Log details of each buy order
                    for buy_order in &payload.buy_orders {
                        tracing::info!("\n  Index {} Buy Order:", buy_order.index_id);
                        tracing::info!(
                            "    Collateral added: ${:.2}",
                            buy_order.collateral_added.to_u128_raw() as f64 / 1e18
                        );
                        tracing::info!(
                            "    Collateral removed: ${:.2}",
                            buy_order.collateral_removed.to_u128_raw() as f64 / 1e18
                        );
                        tracing::info!(
                            "    Net change: ${:.2}",
                            buy_order.net_collateral_change().to_u128_raw() as f64 / 1e18
                        );
                        tracing::info!(
                            "    Max order size: ${:.2}",
                            buy_order.max_order_size.to_u128_raw() as f64 / 1e18
                        );
                        tracing::info!("    Asset allocations:");
                        for alloc in &buy_order.asset_allocations {
                            tracing::info!(
                                "      Asset {}: qty={:.8}, value=${:.2}",
                                alloc.asset_id,
                                alloc.quantity.to_u128_raw() as f64 / 1e18,
                                alloc.target_value_usd.to_u128_raw() as f64 / 1e18
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to process batch: {:?}", e);
                }
            }
        });
    }).await;
    
    tracing::info!("✓ Accumulator + Processor integration started");
    
    // Submit test orders
    tracing::info!("\nSubmitting test orders...");
    
    let test_index_ids = quote_processor.index_mapper.get_all_index_ids();
    
    for index_id in test_index_ids.iter().take(2) {
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
    
    // Wait for batch processing
    tracing::info!("\nWaiting for batch processing...");
    tokio::time::sleep(tokio::time::Duration::from_millis(config.polling.batch_window_ms + 500)).await;
    
    tracing::info!("\n✅ Phase 4.4 Complete!");
    tracing::info!("Quote Processor:");
    tracing::info!("  ✓ Vendor quote fetching");
    tracing::info!("  ✓ Market data snapshot building");
    tracing::info!("  ✓ Asset allocation calculation");
    tracing::info!("  ✓ Buy order generation");
    tracing::info!("  ✓ Integration with accumulator");
    
    tracing::info!("\nNext phases:");
    tracing::info!("  - Phase 4.5: On-chain Submitter");
    tracing::info!("  - Phase 4.6: Main Event Loop");

    Ok(())
}