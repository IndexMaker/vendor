use alloy_primitives::Address;
use clap::Parser;
use config::VendorConfig;
use eyre::Result;
use market_data::{MultiWebSocketSubscriber, MultiSubscriberConfig, MarketDataEvent, MarketDataObserver};
use onchain::{AssetMapper, PriceTracker};
use parking_lot::RwLock as AtomicLock;
use crate::api::{ApiServer, AppState};
use crate::delta_rebalancer::MarginCalculator;
use std::net::SocketAddr;
use std::{path::PathBuf, sync::Arc};
use std::time::Duration;
// unbounded_channel no longer needed with MultiWebSocketSubscriber
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

// Story 3-6: Import buffer module from lib crate
use vendor::buffer;


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
    tracing::debug!("OrderSender ready");

    // Load configuration
    tracing::info!("cli args: {:?}", cli);
    let config = if let Some(config_path) = &cli.config_path {
        let vendor_config_path = PathBuf::from(config_path).join("vendor_config.json");
        VendorConfig::from_file(vendor_config_path.to_str().unwrap()).unwrap()
        // .unwrap_or(VendorConfig::default())
    } else {
        // Load from environment variables (CASTLE_ADDRESS, ORBIT_RPC_URL, etc.)
        VendorConfig::from_env_and_args()
    };
        

    // Access margin config
    let margin_config = config.margin.clone();
    let min_order_size_usd = margin_config.min_order_size_usd;
    let total_exposure_usd = margin_config.total_exposure_usd;

    tracing::info!("Margin config: min_order=${}, total_exposure=${}", min_order_size_usd, total_exposure_usd);

    // Load asset mapper and symbols
    // Priority: 1) --config-path, 2) bitget-pairs.json (default), 3) CLI symbols, 4) hardcoded defaults
    let (symbols, asset_mapper_locked) = if let Some(config_path) = &cli.config_path {
        // Legacy: Load from config directory with assets.json
        tracing::info!("Loading configuration from: {}", config_path);
        let asset_path = PathBuf::from(config_path).join("assets.json");
        let asset_mapper_raw = AssetMapper::load_from_file(&asset_path).await?;
        let asset_mapper = Arc::new(asset_mapper_raw);
        let asset_mapper_locked = Arc::new(parking_lot::RwLock::new(asset_mapper.as_ref().clone()));
        let symbols = asset_mapper.get_all_symbols();
        tracing::info!("Loaded {} assets from config", symbols.len());
        (symbols, Some(asset_mapper_locked))
    } else {
        // Default: Try loading all Bitget pairs from generated file
        let bitget_pairs_path = std::env::var("BITGET_PAIRS_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("vendor/data/bitget-pairs.json"));

        if bitget_pairs_path.exists() {
            tracing::info!("Loading all Bitget pairs from: {:?}", bitget_pairs_path);
            match AssetMapper::load_from_bitget_pairs_file(&bitget_pairs_path).await {
                Ok(asset_mapper_raw) => {
                    let asset_mapper = Arc::new(asset_mapper_raw);
                    let asset_mapper_locked = Arc::new(parking_lot::RwLock::new(asset_mapper.as_ref().clone()));
                    let symbols = asset_mapper.get_all_symbols();
                    tracing::info!("Loaded {} assets from bitget-pairs.json", symbols.len());
                    (symbols, Some(asset_mapper_locked))
                }
                Err(e) => {
                    tracing::warn!("Failed to load bitget-pairs.json: {:?}, falling back to defaults", e);
                    if !cli.symbols.is_empty() {
                        (cli.symbols.clone(), None)
                    } else {
                        (vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()], None)
                    }
                }
            }
        } else if !cli.symbols.is_empty() {
            tracing::info!("Using symbols from CLI: {:?}", cli.symbols);
            (cli.symbols.clone(), None)
        } else {
            let default_symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
            tracing::info!("Using default symbols: {:?}", default_symbols);
            (default_symbols, None)
        }
    };

    if symbols.is_empty() {
        tracing::error!("No symbols to subscribe. Exiting.");
        return Ok(());
    }

    // Create price tracker
    let price_tracker = Arc::new(PriceTracker::new());

    // Create shared wallet provider for both VendorSubmitter and StalenessManager
    // This ensures they use the same provider type P for AppState<P>
    let shared_provider = if let Some(castle_address) = &config.blockchain.castle_address {
        if !config.blockchain.private_key.is_empty() {
            let signer: alloy::signers::local::PrivateKeySigner =
                config.blockchain.private_key.parse()?;
            let wallet = alloy::network::EthereumWallet::from(signer);

            Some(alloy::providers::ProviderBuilder::new()
                .with_gas_estimation()
                .wallet(wallet)
                .connect_http(config.blockchain.rpc_url.parse()?))
        } else {
            tracing::warn!("No private_key configured - using read-only provider");
            None
        }
    } else {
        None
    };

    // Initialize VendorSubmitter (for submitAssets, submitMargin, submitSupply)
    let vendor_submitter = if let Some(ref asset_mapper_arc) = asset_mapper_locked {
        if let Some(castle_address) = &config.blockchain.castle_address {
            // Use private key from config instead of environment variable
            if let Some(ref provider) = shared_provider {
                let castle_addr: Address = castle_address.parse()?;

                let submitter = Arc::new(onchain::VendorSubmitter::new(
                    provider.clone(),
                    castle_addr,
                    config.blockchain.vendor_id,
                ));

                // Submit assets on startup with IDEMPOTENCY check
                let asset_mapper_read = asset_mapper_arc.read();
                let all_symbols = asset_mapper_read.get_all_symbols();
                let asset_ids: Vec<u128> = all_symbols
                    .iter()
                    .filter_map(|symbol| asset_mapper_read.get_id(symbol))
                    .collect();
                drop(asset_mapper_read);

                if !asset_ids.is_empty() && cli.enable_onchain {
                    // Create a read-only provider to check on-chain state
                    let read_provider = alloy::providers::ProviderBuilder::new()
                        .connect_http(config.blockchain.rpc_url.parse()?);
                    let onchain_reader = onchain::OnchainReader::new(
                        read_provider,
                        castle_addr,
                        config.blockchain.vendor_id,
                    );

                    // Check which assets need to be submitted (idempotency)
                    let (new_asset_ids, already_count) = match onchain_reader.filter_new_assets(&asset_ids).await {
                        Ok(result) => result,
                        Err(e) => {
                            tracing::warn!(?e, "Failed to check on-chain assets, submitting all");
                            (asset_ids.clone(), 0)
                        }
                    };

                    // Only submit if there are new assets
                    let assets_submitted = if !new_asset_ids.is_empty() {
                        match submitter.submit_assets(new_asset_ids.clone()).await {
                            Ok(_) => {
                                tracing::info!(
                                    "✓ {} NEW assets submitted to Castle ({} were already on-chain)",
                                    new_asset_ids.len(),
                                    already_count
                                );
                                true
                            }
                            Err(e) => {
                                tracing::error!(?e, "Asset submit failed");
                                false
                            }
                        }
                    } else {
                        tracing::info!(
                            "✓ All {} assets already submitted on-chain - skipping submitAssets",
                            already_count
                        );
                        true // Assets are already there, proceed with market data
                    };

                    if assets_submitted {
                        // Spawn task to submit margin, supply, and market data after startup
                        let submitter_clone = submitter.clone();
                        let asset_ids_clone = asset_ids.clone();
                        let price_tracker_clone = price_tracker.clone();
                        let asset_mapper_clone = asset_mapper_arc.clone();
                        let vendor_id = config.blockchain.vendor_id;
                        let total_exposure = config.margin.total_exposure_usd;

                        tokio::spawn(async move {
                            // Wait for market data to be available from Bitget WebSocket
                            tracing::info!("⏳ Waiting 15s for Bitget market data to populate...");
                            tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;

                            let n = asset_ids_clone.len();
                            tracing::info!("");
                            tracing::info!("╔══════════════════════════════════════════════════════════════════╗");
                            tracing::info!("║  COMPUTING 3 VENDOR VECTORS FOR {} ASSETS                        ║", n);
                            tracing::info!("╚══════════════════════════════════════════════════════════════════╝");
                            tracing::info!("");

                            // ========================================
                            // VECTOR 1: PRICE (P) - Micro-Price
                            // P_i = (P_A1 * Q_B1 + P_B1 * Q_A1) / (Q_B1 + Q_A1)
                            // ========================================
                            tracing::info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                            tracing::info!("VECTOR 1: PRICE (P) - Micro-Price Formula");
                            tracing::info!("  Formula: P_i = (P_A1 × Q_B1 + P_B1 × Q_A1) / (Q_B1 + Q_A1)");
                            tracing::info!("  Source: Top-of-book bid/ask from Bitget WebSocket");
                            tracing::info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

                            let mut prices = Vec::with_capacity(n);
                            let mut computed_prices = 0usize;
                            let mut fallback_prices = 0usize;

                            // Default price for assets without market data
                            let default_price = common::amount::Amount::from_u128_raw(100_000_000_000_000_000_000u128); // $100

                            {
                                let asset_mapper_read = asset_mapper_clone.read();
                                for (i, asset_id) in asset_ids_clone.iter().enumerate() {
                                    let symbol = asset_mapper_read.get_symbol(*asset_id).cloned();

                                    let (price, source) = if let Some(ref sym) = symbol {
                                        if let Some(p) = price_tracker_clone.get_price(sym) {
                                            computed_prices += 1;
                                            (p, "live")
                                        } else {
                                            fallback_prices += 1;
                                            (default_price, "default")
                                        }
                                    } else {
                                        fallback_prices += 1;
                                        (default_price, "default")
                                    };

                                    // Log first 5 and last 2 for visibility
                                    if i < 5 || i >= n.saturating_sub(2) {
                                        let price_f64 = price.to_u128_raw() as f64 / 1e18;
                                        tracing::info!(
                                            "  [{}] {} (id={}): ${:.4} ({})",
                                            i + 1,
                                            symbol.as_deref().unwrap_or("?"),
                                            asset_id,
                                            price_f64,
                                            source
                                        );
                                    } else if i == 5 {
                                        tracing::info!("  ... ({} more assets) ...", n - 7);
                                    }

                                    prices.push(price);
                                }
                            }
                            tracing::info!("  ✓ Price vector: {} live, {} fallback", computed_prices, fallback_prices);
                            tracing::info!("");

                            // ========================================
                            // VECTOR 2: MARGIN (M) - Per-Asset Exposure
                            // M_i = (V_max / n) / P_i
                            // ========================================
                            tracing::info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                            tracing::info!("VECTOR 2: MARGIN (M) - Per-Asset Exposure");
                            tracing::info!("  Formula: M_i = (V_max / n) / P_i");
                            tracing::info!("  V_max (total exposure): ${}", total_exposure);
                            tracing::info!("  n (asset count): {}", n);
                            tracing::info!("  Per-asset volley: ${:.2}", total_exposure / n as f64);
                            tracing::info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

                            let per_asset_volley = total_exposure / n as f64;
                            let margins: Vec<common::amount::Amount> = prices
                                .iter()
                                .enumerate()
                                .map(|(i, p)| {
                                    let price_f64 = p.to_u128_raw() as f64 / 1e18;
                                    let margin = if price_f64 > 0.0 {
                                        per_asset_volley / price_f64
                                    } else {
                                        1.0 // minimum margin
                                    };

                                    // Log first 5 and last 2
                                    if i < 5 || i >= n.saturating_sub(2) {
                                        let asset_id = asset_ids_clone[i];
                                        let asset_mapper_read = asset_mapper_clone.read();
                                        let symbol = asset_mapper_read.get_symbol(asset_id).cloned();
                                        drop(asset_mapper_read);
                                        tracing::info!(
                                            "  [{}] {}: M = {:.2} / {:.4} = {:.6} units",
                                            i + 1,
                                            symbol.as_deref().unwrap_or("?"),
                                            per_asset_volley,
                                            price_f64,
                                            margin
                                        );
                                    }

                                    common::amount::Amount::from_u128_raw((margin * 1e18) as u128)
                                })
                                .collect();

                            tracing::info!("  ✓ Margin vector computed for {} assets", margins.len());
                            tracing::info!("");

                            // ========================================
                            // VECTOR 3: SUPPLY (Long/Short Positions)
                            // Initial: zeros (no positions)
                            // Future: Poisson PDE solver
                            // ========================================
                            tracing::info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                            tracing::info!("VECTOR 3: SUPPLY (Long/Short Inventory)");
                            tracing::info!("  Current: All zeros (no open positions on startup)");
                            tracing::info!("  Future: Poisson PDE solver for delta convergence:");
                            tracing::info!("    Delta_i = Delta_{{i-1}} + SupplyUpdate_i");
                            tracing::info!("    Solver: a*d2(Delta) + b*d(Delta) + c*Delta = 0");
                            tracing::info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

                            let zero = common::amount::Amount::ZERO;
                            let supply_long: Vec<common::amount::Amount> = vec![zero; n];
                            let supply_short: Vec<common::amount::Amount> = vec![zero; n];
                            tracing::info!("  ✓ Supply vectors: {} long (all 0), {} short (all 0)", n, n);
                            tracing::info!("");

                            // ========================================
                            // SUBMIT ALL VECTORS
                            // ========================================
                            tracing::info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                            tracing::info!("SUBMITTING VECTORS TO CASTLE (vendor_id={})", vendor_id);
                            tracing::info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

                            // Submit margin
                            match submitter_clone.submit_margin(asset_ids_clone.clone(), margins).await {
                                Ok(_) => tracing::info!("  ✅ MARGIN vector submitted ({} assets)", n),
                                Err(e) => tracing::error!("  ❌ Margin submit failed: {:?}", e),
                            }

                            // Submit supply
                            match submitter_clone.submit_supply(
                                asset_ids_clone.clone(),
                                supply_long,
                                supply_short,
                            ).await {
                                Ok(_) => tracing::info!("  ✅ SUPPLY vectors submitted ({} long, {} short)", n, n),
                                Err(e) => tracing::error!("  ❌ Supply submit failed: {:?}", e),
                            }

                            // For market data, we need P/S/L vectors
                            // Using prices computed above, with default slope/liquidity for now
                            // TODO: Integrate with PSLComputeService for real K=5 order book computation
                            let default_slope = common::amount::Amount::from_u128_raw(1_000_000_000_000_000u128); // 0.001
                            let default_liquidity = common::amount::Amount::from_u128_raw(500_000_000_000_000_000u128); // 0.5
                            let slopes: Vec<common::amount::Amount> = vec![default_slope; n];
                            let liquidities: Vec<common::amount::Amount> = vec![default_liquidity; n];

                            match submitter_clone.submit_market_data(
                                asset_ids_clone.clone(),
                                prices,
                                slopes,
                                liquidities,
                            ).await {
                                Ok(_) => tracing::info!("  ✅ MARKET DATA (P/S/L) submitted ({} assets)", n),
                                Err(e) => tracing::error!("  ❌ Market data submit failed: {:?}", e),
                            }

                            tracing::info!("");
                            tracing::info!("╔══════════════════════════════════════════════════════════════════╗");
                            tracing::info!("║  ✅ VENDOR INITIALIZATION COMPLETE - {} ASSETS REGISTERED        ║", n);
                            tracing::info!("╚══════════════════════════════════════════════════════════════════╝");
                            tracing::info!("");
                        });
                    }
                }

                Some(submitter)
            } else {
                tracing::warn!("VendorSubmitter disabled (no private_key in vendor_config.json)");
                None
            }
        } else {
            tracing::warn!("VendorSubmitter disabled (no castle_address in config)");
            None
        }
    } else {
        None
    };


    // Create market data observer
    let observer = Arc::new(AtomicLock::new(MarketDataObserver::new()));

    // Create OrderBookProcessor with default config (K=5 levels, 1.01 fee multiplier)
    let order_book_config = order_book::OrderBookConfig::default();
    // Clone for later use in PSLComputeService
    let order_book_config_for_psl = order_book_config.clone();

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


    // Initialize StalenessManager (only if asset_mapper exists and shared_provider is available)
    // Uses same provider type as VendorSubmitter for AppState type compatibility
    let staleness_manager = if let Some(ref asset_mapper_arc) = asset_mapper_locked {
        if let Some(castle_address) = &config.blockchain.castle_address {
            if let Some(ref provider) = shared_provider {
                let castle_addr: Address = castle_address.parse()?;

                let onchain_reader = onchain::OnchainReader::new(
                    provider.clone(),
                    castle_addr,
                    config.blockchain.vendor_id,
                );

                let mgr = onchain::StalenessManager::new(
                    config.staleness.clone(),
                    price_tracker.clone(),
                    asset_mapper_arc.clone(),
                    onchain_reader,
                );

                Some(Arc::new(parking_lot::RwLock::new(mgr)))
            } else {
                tracing::warn!("StalenessManager disabled (no private_key - required for unified provider type)");
                None
            }
        } else {
            tracing::warn!("StalenessManager disabled (no castle_address in config)");
            None
        }
    } else {
        None
    };


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


    // Subscribe to market data events
    {
        let mut obs = observer.write();
        let price_tracker_clone = price_tracker.clone();

        obs.subscribe(move |event: Arc<MarketDataEvent>| {
            // Update price tracker
            price_tracker_clone.handle_event(event.clone());

            // Log events
            match event.as_ref() {
                MarketDataEvent::OrderBookSnapshot { symbol, bid_updates, ask_updates, .. } => {
                    if !bid_updates.is_empty() && !ask_updates.is_empty() {
                        tracing::trace!(symbol, bid = %bid_updates[0].price, ask = %ask_updates[0].price, "Snapshot");
                    }
                }
                MarketDataEvent::OrderBookDelta { .. } | MarketDataEvent::TopOfBook { .. } => {}
            }
        });
    }

    // Create multi-websocket subscriber for handling 600+ assets
    // Distributes symbols across multiple WebSocket connections (50 symbols each)
    let multi_config = MultiSubscriberConfig {
        websocket_url: config.market_data.bitget.websocket_url.clone(),
        symbols_per_connection: config.market_data.bitget.symbols_per_connection.unwrap_or(50),
        stale_check_period: Duration::from_secs(config.market_data.bitget.stale_check_period_secs),
        stale_timeout: chrono::Duration::seconds(config.market_data.bitget.stale_timeout_secs),
        heartbeat_interval: Duration::from_secs(
            config.market_data.bitget.heartbeat_interval_secs,
        ),
    };

    let mut multi_subscriber = MultiWebSocketSubscriber::new(multi_config, observer.clone());

    // Start subscriber with all symbols at once (batch subscribes across multiple connections)
    multi_subscriber.start(symbols.clone()).await?;
    tracing::info!(count = symbols.len(), "Multi-websocket subscriber started with symbols");

    // Start API server
    let api_server = if let (Some(asset_mapper_arc), Some(staleness_mgr), Some(ob_processor)) = 
        (&asset_mapper_locked, &staleness_manager, &order_book_processor) 
    {
        let api_addr: SocketAddr = format!("0.0.0.0:{}", cli.api_port).parse()?;

        // Story 3-4: Create PSLComputeService for /process-assets endpoint
        // PSLComputeService::new requires Arc<AssetMapper> (unwrapped) and OrderBookConfig
        let psl_service = {
            let unwrapped_mapper = Arc::new(asset_mapper_arc.read().clone());
            match order_book::PSLComputeService::new(
                unwrapped_mapper,
                order_book_config_for_psl.clone(),
            ) {
                Ok(svc) => Some(Arc::new(svc)),
                Err(e) => {
                    tracing::warn!("Failed to create PSLComputeService: {:?}", e);
                    None
                }
            }
        };

        // Story 3-6: Initialize buffer components
        let order_buffer = Arc::new(buffer::OrderBuffer::new(
            std::env::var("BUFFER_MAX_QUEUE_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1000)
        ));
        let min_size_handler = Arc::new(buffer::MinSizeHandler::default());

        let app_state = AppState {
            vendor_id: config.blockchain.vendor_id,
            asset_mapper: asset_mapper_arc.clone(),
            staleness_manager: staleness_mgr.clone(),
            order_book_processor: ob_processor.clone(),
            // Story 3-4: Wire PSL service and MarginConfig
            // vendor_submitter is wired when shared_provider is used (same P type)
            psl_service,
            vendor_submitter: vendor_submitter.clone(),
            margin_config: Some(market_data::MarginConfig::default()),
            // Story 3-6: Async buffer components
            order_buffer: Some(order_buffer),
            min_size_handler: Some(min_size_handler),
        };

        let server = ApiServer::new(app_state, api_addr);
        let cancel_token = server.cancel_token();

        tokio::spawn(async move {
            if let Err(e) = server.start().await {
                tracing::error!("API server failed: {:?}", e);
            }
        });

        tracing::info!(port = cli.api_port, "API server started");

        Some(cancel_token)
    } else {
        tracing::warn!("API server disabled (missing dependencies)");
        None
    };

    // Initialize DeltaRebalancer
    let _delta_rebalancer = if let (
        Some(asset_mapper_arc), 
        Some(supply_mgr), 
        Some(vendor_sub)) 
        = 
        (&asset_mapper_locked, &supply_manager, &vendor_submitter)
    {
        let rebalancer_config = delta_rebalancer::RebalancerConfig {
            vendor_id: config.blockchain.vendor_id,
            min_order_size_usd: config.margin.min_order_size_usd,
            total_exposure_usd: config.margin.total_exposure_usd,
            rebalance_interval_secs: 60,
            enable_onchain_submit: cli.enable_onchain,
            // On-chain first strategy: submit to blockchain immediately, exchange later
            onchain_first_enabled: true,
            // Inventory simulation: accept orders even when can't cover min_order_size
            inventory_simulation_enabled: true,
            // USDC balance threshold for simulation mode
            usdc_balance_threshold: 10.0,
        };
        
        let rebalancer = Arc::new(delta_rebalancer::DeltaRebalancer::new(
            rebalancer_config,
            Arc::clone(vendor_sub),  // Explicit Arc::clone
            Arc::clone(supply_mgr),  // Explicit Arc::clone
            Arc::clone(&price_tracker),
            Arc::clone(asset_mapper_arc),  // Explicit Arc::clone
            order_sender,
        ));
        
        tokio::spawn({
            let rebalancer = Arc::clone(&rebalancer);
            async move {
                rebalancer.run().await;
            }
        });
        
        Some(rebalancer)
    } else {
        None
    };

    tracing::info!("Vendor running");
    tokio::signal::ctrl_c().await?;

    multi_subscriber.stop().await?;
    order_sender_config.stop().await?;
    if let Some(api_cancel) = api_server {
        api_cancel.cancel();
    }
    tracing::info!("Vendor stopped");
    Ok(())
}