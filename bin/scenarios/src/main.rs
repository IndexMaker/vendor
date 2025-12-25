use alloy_primitives::Address;
use alloy_sol_types::{sol};

use clap::Parser;

use alloy::{
    network::EthereumWallet,
    providers::ProviderBuilder,
    signers::local::PrivateKeySigner,
};
use eyre::{eyre, OptionExt};

mod scenario_5;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(short, long)]
    rpc_url: Option<String>,

    #[arg(short, long)]
    private_key: String,

    #[arg(long)]
    castle_address: Option<String>,

    #[arg(long)]
    clerk_address: Option<String>,

    #[arg(short, long, value_delimiter = ',')]
    scenario: Vec<String>,
}

// Define a Solidity-style struct and function selector
sol! {
    struct Order {
        address maker;
        uint256 amount;
        bytes32 salt;
    }

    function submitOrder(Order order) external;
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let cli = Cli::parse();
    let rpc_url = cli.rpc_url.unwrap_or("http://localhost:8547".to_owned());
    let private_key = cli.private_key.clone();

    let castle_address: Option<Address> = if let Some(a) = cli.castle_address {
        Some(a.parse()?)
    } else {
        None
    };

    let scenario = cli.scenario;

    // Create signer from private key
    let signer: PrivateKeySigner = private_key.parse()?;
    let wallet = EthereumWallet::from(signer);

    // Create provider with wallet - manual fillers for compatibility
    let provider = ProviderBuilder::new()
        .with_gas_estimation()
        // .with_nonce_management()
        .wallet(wallet)
        .connect_http(rpc_url.parse()?);

    for s in scenario {
        match s.as_str() {
            "scenario5" => {
                scenario_5::run_scenario(
                    provider.clone(),
                    castle_address.ok_or_eyre("Castle address is required")?,
                )
                .await?;
            }
            x => {
                Err(eyre!("No such scenario: {}", x))?;
            }
        }
    }

    println!("Done.");

    Ok(())
}