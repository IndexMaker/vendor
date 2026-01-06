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
mod onchain;
mod keeper;  // NEW: Add keeper module

use onchain::{GasConfig, OnchainSubmitter, SubmitterConfig};
use processor::{ProcessorConfig, QuoteProcessor};
use accumulator::{AccumulatorConfig, IndexOrder, OrderAccumulator, OrderAction};
use config::KeeperConfig;
use index::IndexMapper;
use vendor::{QuoteCache, VendorClient};
use keeper::{KeeperLoop, KeeperLoopConfig};  // NEW: Import keeper types

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
    test_mode: bool,  // NEW: Add test mode flag
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

    tracing::info!("🏰 Starting VaultWorks Keeper");  // CHANGED: Added emoji

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
    
    tracing::info!("\nInitializing On-chain Submitter...");
    
    let submitter_config = SubmitterConfig {
        rpc_url: config.blockchain.rpc_url.clone(),
        castle_address: config.blockchain.castle_address.parse()?,
        vendor_id: config.blockchain.vendor_id,
        private_key: std::env::var("PRIVATE_KEY").ok(),
        dry_run: config.blockchain.dry_run,
        gas_config: GasConfig::default(),
        retry_attempts: 3,
        retry_delay_ms: 1000,
    };
    
    // Always create with provider, use dry_run flag to skip actual submission
    use alloy::providers::ProviderBuilder;
    
    let provider = ProviderBuilder::new()
        .connect_http(config.blockchain.rpc_url.parse()?);
    
    let submitter = Arc::new(OnchainSubmitter::new_with_provider(submitter_config, provider)?);
    
    if config.blockchain.dry_run {
        tracing::info!("✓ On-chain submitter initialized (DRY RUN MODE)");
    } else {
        tracing::info!("✓ On-chain submitter initialized (LIVE MODE)");
    }

    // ========================================================================
    // Full pipeline integration
    // ========================================================================
    tracing::info!("\nInitializing Full Pipeline: Accumulator → Processor → Submitter");
    
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
        tracing::info!("🔥 Batch ready for processing!");
        tracing::info!("  Indices: {}", batch.indices.len());
        tracing::info!("  Total orders: {}", batch.total_order_count());
        
        // Full pipeline: Process → Submit
        let processor = processor_clone.clone();
        let submitter = submitter_clone.clone();
        
        tokio::spawn(async move {
            // Step 1: Process batch
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
                    
                    // Step 2: Submit to blockchain
                    match submitter.submit_payload(payload).await {
                        Ok(result) => {
                            match result {
                                onchain::SubmissionResult::Success { market_data_tx, buy_order_txs } => {
                                    tracing::info!("\n✅ On-chain Submission Successful!");
                                    if let Some(tx) = market_data_tx {
                                        tracing::info!("  Market data tx: {}", tx.tx_hash);
                                    }
                                    for (index_id, tx) in buy_order_txs {
                                        tracing::info!("  Buy order {} tx: {}", index_id, tx.tx_hash);
                                    }
                                }
                                onchain::SubmissionResult::DryRun { would_submit } => {
                                    tracing::info!("\n🔍 Dry Run Complete");
                                    tracing::info!("  Would submit {} assets", would_submit.market_data_assets);
                                    tracing::info!("  Would submit {} orders", would_submit.buy_orders_count);
                                }
                                onchain::SubmissionResult::Failed { error } => {
                                    tracing::error!("\n❌ Submission Failed: {}", error);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("Submission error: {:?}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to process batch: {:?}", e);
                }
            }
        });
    }).await;
    
    tracing::info!("✓ Full pipeline started (Accumulator → Processor → Submitter)");

    // ========================================================================
    // NEW: Phase 4.6 - Main Keeper Loop or Test Mode
    // ========================================================================
    
    if cli.test_mode {
        // Test mode: Submit test orders and exit
        tracing::info!("\n🧪 Running in TEST MODE");
        
        tracing::info!("Submitting test orders...");
        let test_index_ids = quote_processor.index_mapper.get_all_index_ids();
        
        for index_id in test_index_ids.iter().take(2) {
            let order1 = IndexOrder {
                index_id: *index_id,
                action: OrderAction::Deposit {
                    user_address: "0xUser1".to_string(),
                    amount_usd: Amount::from_u128_with_scale(1000, 0),
                },
                timestamp: chrono::Utc::now(),
            };
            
            let order2 = IndexOrder {
                index_id: *index_id,
                action: OrderAction::Deposit {
                    user_address: "0xUser2".to_string(),
                    amount_usd: Amount::from_u128_with_scale(500, 0),
                },
                timestamp: chrono::Utc::now(),
            };
            
            accumulator.submit_order(order1)?;
            accumulator.submit_order(order2)?;
            
            tracing::info!("  ✓ Submitted 2 orders for index {}", index_id);
        }
        
        tracing::info!("Waiting for batch processing...");
        tokio::time::sleep(tokio::time::Duration::from_millis(config.polling.batch_window_ms + 1000)).await;
        
        tracing::info!("\n✅ Test mode complete");
    } else {
        // Production mode: Run main keeper loop
        tracing::info!("\n🚀 Running in PRODUCTION MODE");
        
        // Create main keeper loop
        let keeper_loop_config = KeeperLoopConfig {
            polling_interval_secs: config.polling.interval_secs,
            health_check_interval_secs: 30,
        };

        let keeper_loop = Arc::new(KeeperLoop::new(
            accumulator.clone(),
            quote_processor.clone(),
            submitter.clone(),
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

        tracing::info!("✅ Keeper is running!");
        tracing::info!("  - Accumulator: active");
        tracing::info!("  - Processor: active");
        tracing::info!("  - Submitter: active");
        tracing::info!("  - Main loop: active");
        tracing::info!("\nPress Ctrl+C to stop gracefully...");

        // Wait for shutdown signal
        tokio::signal::ctrl_c().await?;
        
        tracing::info!("\n🛑 Shutdown signal received");
        cancel_token.cancel();
        
        // Wait for keeper loop to finish
        let _ = keeper_handle.await;
    }

    tracing::info!("✅ Keeper stopped cleanly");
    Ok(())
}