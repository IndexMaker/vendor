use clap::Parser;
use eyre::Result;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod client;
mod simulation;

use client::VendorClient;
use simulation::{OrderSimulator, SimulationConfig};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Vendor base URL
    #[arg(long, default_value = "http://localhost:8080")]
    vendor_url: String,

    /// Indices to trade (comma-separated)
    #[arg(long, value_delimiter = ',')]
    indices: Option<Vec<String>>,

    /// Minimum collateral in USD
    #[arg(long, default_value = "100.0")]
    min_collateral: f64,

    /// Maximum collateral in USD
    #[arg(long, default_value = "5000.0")]
    max_collateral: f64,

    /// Minimum interval between orders (seconds)
    #[arg(long, default_value = "10")]
    min_interval: u64,

    /// Maximum interval between orders (seconds)
    #[arg(long, default_value = "30")]
    max_interval: u64,

    /// Client ID prefix
    #[arg(long, default_value = "keeper-sim")]
    client_id_prefix: String,

    /// Check vendor health and exit
    #[arg(long)]
    health_check: bool,

    /// Get inventory and exit
    #[arg(long)]
    get_inventory: bool,

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
    tracing::info!("Vendor URL: {}", cli.vendor_url);

    // Create vendor client
    let vendor_client = VendorClient::new(cli.vendor_url.clone());

    // Health check mode
    if cli.health_check {
        tracing::info!("Running health check...");
        match vendor_client.health_check().await {
            Ok(health) => {
                tracing::info!("✅ Vendor is healthy");
                tracing::info!("Status: {}", health.status);
                tracing::info!("Version: {}", health.version);
                tracing::info!("Timestamp: {}", health.timestamp);
                return Ok(());
            }
            Err(e) => {
                tracing::error!("❌ Health check failed: {:?}", e);
                return Err(e);
            }
        }
    }

    // Get inventory mode
    if cli.get_inventory {
        tracing::info!("Fetching inventory...");
        match vendor_client.get_inventory().await {
            Ok(inventory) => {
                tracing::info!("✅ Inventory retrieved");
                println!("{}", serde_json::to_string_pretty(&inventory)?);
                return Ok(());
            }
            Err(e) => {
                tracing::error!("❌ Failed to fetch inventory: {:?}", e);
                return Err(e);
            }
        }
    }

    // Simulation mode (default)
    let indices = cli.indices.unwrap_or_else(|| vec!["SY100".to_string()]);

    let config = SimulationConfig {
        indices,
        min_collateral_usd: cli.min_collateral,  // Changed
        max_collateral_usd: cli.max_collateral,  // Changed
        min_interval_secs: cli.min_interval,
        max_interval_secs: cli.max_interval,
        client_id_prefix: cli.client_id_prefix,
    };

    let mut simulator = OrderSimulator::new(config, vendor_client);

    // Run simulation
    tracing::info!("Starting order simulation mode");
    tracing::info!("Press Ctrl+C to stop");

    tokio::select! {
        result = simulator.run() => {
            if let Err(e) = result {
                tracing::error!("Simulation failed: {:?}", e);
                return Err(e);
            }
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Received Ctrl+C, shutting down...");
        }
    }

    tracing::info!("Keeper stopped");
    Ok(())
}