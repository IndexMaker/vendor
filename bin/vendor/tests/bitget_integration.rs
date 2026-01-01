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
}