use alloy_primitives::{Address, uint, Bytes};
use alloy_sol_types::SolCall;
use amount_macros::amount;
use common::interfaces::banker::IBanker;
use common::interfaces::factor::IFactor;
use common::interfaces::guildmaster::IGuildmaster;
use common::{labels::Labels, log_msg, vector::Vector};
use labels_macros::label_vec;
use vector_macros::amount_vec;

use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;

pub async fn run_scenario<P>(
    provider: P,
    castle_address: Address,
) -> eyre::Result<()>
where
    P: Provider + Clone,
{
    println!("Scenario 5.");

    let vendor_id = uint!(1u128);
    let index_id = 1001;

    {
        println!("Submit Assets #1");

        let asset_names = label_vec!(101, 102, 104, 105, 106);

        let call = IBanker::submitAssetsCall {
            vendor_id,
            market_asset_names: asset_names.to_vec(),
        };
        
        let tx = TransactionRequest::default()
            .to(castle_address)
            .input(call.abi_encode().into());
        
        let receipt = provider.send_transaction(tx).await?.get_receipt().await?;
    }

    {
        println!("Submit Assets #2");

        let asset_names = label_vec!(102, 103, 107, 108, 109);

        let call = IBanker::submitAssetsCall {
            vendor_id,
            market_asset_names: asset_names.to_vec(),
        };
        
        let tx = TransactionRequest::default()
            .to(castle_address)
            .input(call.abi_encode().into());
        
        let receipt = provider.send_transaction(tx).await?.get_receipt().await?;
    }

    {
        println!("Submit Margin");

        let asset_names = label_vec!(101, 102, 103, 104, 105, 106, 107, 108, 109);
        let asset_margin = amount_vec!(2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0);

        let call = IBanker::submitMarginCall {
            vendor_id,
            asset_names: asset_names.to_vec(),
            asset_margin: asset_margin.to_vec(),
        };
        
        let tx = TransactionRequest::default()
            .to(castle_address)
            .input(call.abi_encode().into());
        
        let receipt = provider.send_transaction(tx).await?.get_receipt().await?;
    }

    {
        println!("Submit Supply");

        let asset_names = label_vec!(101, 102, 103, 104, 105, 106, 107, 108, 109);
        let asset_short = amount_vec!(0, 0, 0, 0, 0, 0, 0, 0, 0);
        let asset_long = amount_vec!(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0);

        let call = IBanker::submitSupplyCall {
            vendor_id,
            asset_names: asset_names.to_vec(),
            asset_quantities_short: asset_short.to_vec(),
            asset_quantities_long: asset_long.to_vec(),
        };
        
        let tx = TransactionRequest::default()
            .to(castle_address)
            .input(call.abi_encode().into());
        
        let receipt = provider.send_transaction(tx).await?.get_receipt().await?;
    }

    {
        println!("Submit Index");

        let asset_names = label_vec!(102, 103, 104, 106, 107);
        let asset_weights = amount_vec!(1.0, 0.5, 0.5, 0.5, 1.5);
        let info = b"Test Index 1001".to_vec();

        let call = IGuildmaster::submitIndexCall {
            index: index_id,
            asset_names: asset_names.to_vec(),
            asset_weights: asset_weights.to_vec(),
            info,
        };
        
        let tx = TransactionRequest::default()
            .to(castle_address)
            .input(call.abi_encode().into());
        
        let receipt = provider.send_transaction(tx).await?.get_receipt().await?;
    }

    {
        println!("Submit Vote");

        let vote = vec![];

        let call = IGuildmaster::submitVoteCall {
            index: index_id,
            vote,
        };
        
        let tx = TransactionRequest::default()
            .to(castle_address)
            .input(call.abi_encode().into());
        
        let receipt = provider.send_transaction(tx).await?.get_receipt().await?;
    }

    {
        println!("Submit Market Data");

        let asset_names = label_vec!(102, 103, 104, 106, 107);
        let asset_liquidity = amount_vec!(0.5, 0.5, 0.5, 0.5, 0.5);
        let asset_prices = amount_vec!(100.0, 50.0, 20.0, 10.0, 1.0);
        let asset_slopes = amount_vec!(1.0, 0.5, 0.2, 0.1, 0.01);

        let call = IFactor::submitMarketDataCall {
            vendor_id,
            asset_names: asset_names.to_vec(),
            asset_liquidity: asset_liquidity.to_vec(),
            asset_prices: asset_prices.to_vec(),
            asset_slopes: asset_slopes.to_vec(),
        };
        
        let tx = TransactionRequest::default()
            .to(castle_address)
            .input(call.abi_encode().into());
        
        let receipt = provider.send_transaction(tx).await?.get_receipt().await?;
    }

    {
        println!("Update Index Quote");

        let call = IFactor::updateIndexQuoteCall {
            vendor_id,
            index: index_id,
        };
        
        let tx = TransactionRequest::default()
            .to(castle_address)
            .input(call.abi_encode().into());
        
        let receipt = provider.send_transaction(tx).await?.get_receipt().await?;
    }

    {
        println!("Submit Buy Order");

        let collateral_added = amount!(10.0);
        let collateral_removed = amount!(0);
        let max_order_size = amount!(1000.0);
        let acf = amount_vec!(1.0, 1.0, 1.0, 0.5, 0.5);

        let call = IFactor::submitBuyOrderCall {
            vendor_id,
            index: index_id,
            collateral_added: collateral_added.to_u128_raw(),
            collateral_removed: collateral_removed.to_u128_raw(),
            max_order_size: max_order_size.to_u128_raw(),
            asset_contribution_fractions: acf.to_vec(),
        };
        
        let tx = TransactionRequest::default()
            .to(castle_address)
            .input(call.abi_encode().into());
        
        let receipt = provider.send_transaction(tx).await?.get_receipt().await?;
        println!("Order placement result: {:?}", receipt);
    }

    Ok(())
}