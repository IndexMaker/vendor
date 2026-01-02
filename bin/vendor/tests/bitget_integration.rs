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

    #[tokio::test]
    #[ignore]
    async fn test_bitget_order_sender_dry_run() {
        dotenv::dotenv().ok();
    
        let api_key = std::env::var("BITGET_API_KEY").expect("BITGET_API_KEY not set");
        let api_secret = std::env::var("BITGET_API_SECRET").expect("BITGET_API_SECRET not set");
        let passphrase = std::env::var("BITGET_PASSPHRASE").expect("BITGET_PASSPHRASE not set");
    
        // Create sender in DRY-RUN mode (trading_enabled = false)
        let mut sender = vendor::order_sender::bitget::BitgetOrderSender::new(
            api_key,
            api_secret,
            passphrase,
            5,     // 5 bps spread
            3,     // 3 retry attempts
            false, // DRY-RUN mode
        );
    
        // Start sender
        sender.start().await.expect("Failed to start");
    
        // Create test order
        let order = vendor::order_sender::AssetOrder::limit(
            "BTCUSDT".to_string(),
            vendor::order_sender::OrderSide::Buy,
            common::amount::Amount(1_000_000_000_000_000), // 0.001 BTC
            common::amount::Amount(95000 * 1_000_000_000_000_000_000), // $95000
        );
    
        // Execute (should fail in dry-run mode)
        use vendor::order_sender::OrderSender;
        let results = sender.send_orders(vec![order]).await.expect("Failed to send orders");
    
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, vendor::order_sender::OrderStatus::Failed);
        assert!(results[0].error_message.as_ref().unwrap().contains("dry-run"));
    
        println!("✅ Dry-run test passed - no real trades executed");
    
        sender.stop().await.expect("Failed to stop");
    }
    
    // WARNING: This test will execute a REAL trade!
    // Only run with --test-threads=1 and very small amounts
    #[tokio::test]
    #[ignore]
    async fn test_bitget_order_sender_real_trade() {
        dotenv::dotenv().ok();
    
        // Safety check
        let trading_enabled = std::env::var("BITGET_TRADING_ENABLED").unwrap_or_default() == "1";
        if !trading_enabled {
            println!("⚠️  Set BITGET_TRADING_ENABLED=1 to run real trade test");
            return;
        }
    
        println!("⚠️  WARNING: This will execute a REAL trade on Bitget!");
        println!("⚠️  Press Ctrl+C within 5 seconds to cancel...");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    
        let api_key = std::env::var("BITGET_API_KEY").expect("BITGET_API_KEY not set");
        let api_secret = std::env::var("BITGET_API_SECRET").expect("BITGET_API_SECRET not set");
        let passphrase = std::env::var("BITGET_PASSPHRASE").expect("BITGET_PASSPHRASE not set");
    
        let mut sender = vendor::order_sender::bitget::BitgetOrderSender::new(
            api_key,
            api_secret,
            passphrase,
            5,    // 5 bps spread
            3,    // 3 retries
            true, // LIVE TRADING
        );
    
        sender.start().await.expect("Failed to start");
    
        // VERY SMALL test order - adjust for your exchange minimums
        let order = vendor::order_sender::AssetOrder::limit(
            "BTCUSDT".to_string(),
            vendor::order_sender::OrderSide::Buy,
            common::amount::Amount::from_u128_raw(1_000_000_000_000_000), // 0.001 BTC
            common::amount::Amount::from_u128_raw(95000 * 1_000_000_000_000_000_000), // $95000 limit
        );
    
        use vendor::order_sender::OrderSender;
        let results = sender.send_orders(vec![order]).await.expect("Failed to send orders");
    
        assert_eq!(results.len(), 1);
        println!("Order result: {:?}", results[0]);
    
        sender.stop().await.expect("Failed to stop");
    }
}