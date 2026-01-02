#[cfg(test)]
mod bitget_tests {
    use vendor::order_sender::BitgetClient;

    #[tokio::test]
    #[ignore] // Run with: cargo test --test bitget_integration -- --ignored
    async fn test_bitget_get_balances() {
        dotenv::dotenv().ok();

        let api_key = std::env::var("BITGET_API_KEY").expect("BITGET_API_KEY not set");
        let api_secret = std::env::var("BITGET_API_SECRET").expect("BITGET_API_SECRET not set");
        let passphrase = std::env::var("BITGET_PASSPHRASE").expect("BITGET_PASSPHRASE not set");

        let client = BitgetClient::new(api_key, api_secret, passphrase);

        let balances = client.get_balances().await.expect("Failed to get balances");

        println!("Balances:");
        for balance in balances {
            if balance.available.parse::<f64>().unwrap_or(0.0) > 0.0 {
                println!("  {}: {} (frozen: {})", balance.coin, balance.available, balance.frozen);
            }
        }
    }

    #[tokio::test]
    #[ignore]
    async fn test_bitget_get_ticker() {
        dotenv::dotenv().ok();

        let api_key = std::env::var("BITGET_API_KEY").expect("BITGET_API_KEY not set");
        let api_secret = std::env::var("BITGET_API_SECRET").expect("BITGET_API_SECRET not set");
        let passphrase = std::env::var("BITGET_PASSPHRASE").expect("BITGET_PASSPHRASE not set");

        let client = BitgetClient::new(api_key, api_secret, passphrase);

        let ticker = client
            .get_orderbook_ticker("BTCUSDT")
            .await
            .expect("Failed to get ticker");

        println!("BTCUSDT Ticker:");
        println!("  Best Bid: {} ({})", ticker.bid_price, ticker.bid_size);
        println!("  Best Ask: {} ({})", ticker.ask_price, ticker.ask_size);
    }

    #[tokio::test]
    #[ignore]
    async fn test_pricing_strategy_with_real_orderbook() {
        dotenv::dotenv().ok();

        let api_key = std::env::var("BITGET_API_KEY").expect("BITGET_API_KEY not set");
        let api_secret = std::env::var("BITGET_API_SECRET").expect("BITGET_API_SECRET not set");
        let passphrase = std::env::var("BITGET_PASSPHRASE").expect("BITGET_PASSPHRASE not set");

        let client = vendor::order_sender::bitget::BitgetClient::new(api_key, api_secret, passphrase);
        let pricing = vendor::order_sender::bitget::PricingStrategy::new(5); // 5 bps

        // Get best ask for BTC
        let best_ask = client.get_best_ask("BTCUSDT").await.expect("Failed to get best ask");
        println!("Best Ask: ${:.2}", best_ask.to_u128_raw() as f64 / 1e18);

        // Calculate limit buy price with spread
        let limit_price = pricing
            .calculate_limit_price(best_ask, vendor::order_sender::OrderSide::Buy)
            .expect("Failed to calculate limit price");

        println!("Limit Buy Price (with {} spread): ${:.2}", 
            pricing.spread_as_percentage(),
            limit_price.to_u128_raw() as f64 / 1e18
        );

        // Verify spread is applied correctly
        let spread_amount = limit_price.checked_sub(best_ask).unwrap();
        let spread_percentage = (spread_amount.to_u128_raw() as f64 / best_ask.to_u128_raw() as f64) * 100.0;

        println!("Actual spread: {:.3}%", spread_percentage);

        // Should be approximately 0.05%
        assert!(spread_percentage > 0.04 && spread_percentage < 0.06);
    }
}