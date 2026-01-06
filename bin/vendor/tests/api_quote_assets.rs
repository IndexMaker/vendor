use common::amount::Amount;
use parking_lot::RwLock;
use reqwest;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::test]
#[ignore] // Remove this to run the test
async fn test_quote_assets_integration() {
    // This test requires a running Vendor instance
    // Run with: cargo test test_quote_assets_integration --ignored -- --nocapture
    
    let client = reqwest::Client::new();
    let base_url = "http://localhost:8080";
    
    println!("=== Testing /quote_assets Endpoint ===\n");
    
    // Test 1: Health check
    println!("1. Health Check");
    let health_response = client
        .get(format!("{}/health", base_url))
        .send()
        .await
        .expect("Failed to connect to Vendor");
    
    assert_eq!(health_response.status(), 200);
    let health: serde_json::Value = health_response.json().await.unwrap();
    println!("   Status: {}", health["status"]);
    println!("   Vendor ID: {}", health["vendor_id"]);
    println!("   Tracked Assets: {}", health["tracked_assets"]);
    println!("   ✓ Health check passed\n");
    
    // Test 2: First quote request (all should be stale)
    println!("2. First Quote Request (expect all stale)");
    let request_body = json!({
        "assets": [101, 102, 103]
    });
    
    let quote_response = client
        .post(format!("{}/quote_assets", base_url))
        .json(&request_body)
        .send()
        .await
        .expect("Failed to send quote request");
    
    assert_eq!(quote_response.status(), 200);
    let first_quote: serde_json::Value = quote_response.json().await.unwrap();
    
    println!("   Requested: {:?}", request_body["assets"]);
    println!("   Stale: {:?}", first_quote["assets"]);
    println!("   Liquidity: {:?}", first_quote["liquidity"]);
    println!("   Prices: {:?}", first_quote["prices"]);
    println!("   Slopes: {:?}", first_quote["slopes"]);
    
    assert!(
        first_quote["assets"].as_array().unwrap().len() > 0,
        "First request should return stale assets"
    );
    println!("   ✓ First quote returned {} stale assets\n", 
        first_quote["assets"].as_array().unwrap().len());
    
    // Test 3: Immediate second request (nothing should be stale)
    println!("3. Immediate Second Request (expect nothing stale)");
    let second_response = client
        .post(format!("{}/quote_assets", base_url))
        .json(&request_body)
        .send()
        .await
        .expect("Failed to send second quote request");
    
    assert_eq!(second_response.status(), 200);
    let second_quote: serde_json::Value = second_response.json().await.unwrap();
    
    println!("   Stale: {:?}", second_quote["assets"]);
    
    assert_eq!(
        second_quote["assets"].as_array().unwrap().len(),
        0,
        "Second immediate request should return no stale assets"
    );
    println!("   ✓ No stale assets (as expected)\n");
    
    // Test 4: Empty request
    println!("4. Empty Asset List");
    let empty_request = json!({
        "assets": []
    });
    
    let empty_response = client
        .post(format!("{}/quote_assets", base_url))
        .json(&empty_request)
        .send()
        .await
        .expect("Failed to send empty request");
    
    assert_eq!(empty_response.status(), 200);
    let empty_quote: serde_json::Value = empty_response.json().await.unwrap();
    
    assert_eq!(
        empty_quote["assets"].as_array().unwrap().len(),
        0,
        "Empty request should return empty response"
    );
    println!("   ✓ Empty request handled correctly\n");
    
    // Test 5: Invalid asset ID
    println!("5. Invalid Asset ID");
    let invalid_request = json!({
        "assets": [999]
    });
    
    let invalid_response = client
        .post(format!("{}/quote_assets", base_url))
        .json(&invalid_request)
        .send()
        .await
        .expect("Failed to send invalid request");
    
    assert_eq!(invalid_response.status(), 200);
    let invalid_quote: serde_json::Value = invalid_response.json().await.unwrap();
    
    println!("   Stale: {:?}", invalid_quote["assets"]);
    println!("   ✓ Invalid asset handled gracefully\n");
    
    // Test 6: Large asset list
    println!("6. Large Asset List");
    let large_request = json!({
        "assets": [101, 102, 103, 104, 105, 106, 107, 108, 109, 110]
    });
    
    let large_response = client
        .post(format!("{}/quote_assets", base_url))
        .json(&large_request)
        .send()
        .await
        .expect("Failed to send large request");
    
    assert_eq!(large_response.status(), 200);
    let large_quote: serde_json::Value = large_response.json().await.unwrap();
    
    println!("   Requested: {} assets", large_request["assets"].as_array().unwrap().len());
    println!("   Stale: {} assets", large_quote["assets"].as_array().unwrap().len());
    println!("   ✓ Large request handled\n");
    
    println!("=== All Tests Passed ===");
}

#[tokio::test]
#[ignore]
async fn test_staleness_timeout() {
    // Test that assets become stale after timeout period
    // Requires staleness config with time_threshold_secs = 5 for testing
    
    let client = reqwest::Client::new();
    let base_url = "http://localhost:8080";
    
    println!("=== Testing Staleness Timeout ===\n");
    
    let request_body = json!({
        "assets": [101]
    });
    
    // First request
    println!("1. Initial request");
    let response1 = client
        .post(format!("{}/quote_assets", base_url))
        .json(&request_body)
        .send()
        .await
        .unwrap();
    
    let quote1: serde_json::Value = response1.json().await.unwrap();
    println!("   Stale assets: {}", quote1["assets"].as_array().unwrap().len());
    
    // Immediate second request (should be non-stale)
    println!("2. Immediate second request");
    let response2 = client
        .post(format!("{}/quote_assets", base_url))
        .json(&request_body)
        .send()
        .await
        .unwrap();
    
    let quote2: serde_json::Value = response2.json().await.unwrap();
    println!("   Stale assets: {}", quote2["assets"].as_array().unwrap().len());
    assert_eq!(quote2["assets"].as_array().unwrap().len(), 0);
    
    // Wait for staleness timeout (e.g., 6 seconds if timeout is 5s)
    println!("3. Waiting 6 seconds for staleness timeout...");
    sleep(Duration::from_secs(6)).await;
    
    // Third request (should be stale again due to timeout)
    println!("4. Request after timeout");
    let response3 = client
        .post(format!("{}/quote_assets", base_url))
        .json(&request_body)
        .send()
        .await
        .unwrap();
    
    let quote3: serde_json::Value = response3.json().await.unwrap();
    println!("   Stale assets: {}", quote3["assets"].as_array().unwrap().len());
    
    assert!(
        quote3["assets"].as_array().unwrap().len() > 0,
        "Asset should be stale again after timeout"
    );
    
    println!("   ✓ Staleness timeout working correctly\n");
}

#[tokio::test]
#[ignore]
async fn test_response_format() {
    // Test that response format matches expected schema
    
    let client = reqwest::Client::new();
    let base_url = "http://localhost:8080";
    
    println!("=== Testing Response Format ===\n");
    
    let request_body = json!({
        "assets": [101, 102]
    });
    
    let response = client
        .post(format!("{}/quote_assets", base_url))
        .json(&request_body)
        .send()
        .await
        .unwrap();
    
    assert_eq!(response.status(), 200);
    
    let quote: serde_json::Value = response.json().await.unwrap();
    
    // Check schema
    assert!(quote.get("assets").is_some(), "Missing 'assets' field");
    assert!(quote.get("liquidity").is_some(), "Missing 'liquidity' field");
    assert!(quote.get("prices").is_some(), "Missing 'prices' field");
    assert!(quote.get("slopes").is_some(), "Missing 'slopes' field");
    
    let assets = quote["assets"].as_array().unwrap();
    let liquidity = quote["liquidity"].as_array().unwrap();
    let prices = quote["prices"].as_array().unwrap();
    let slopes = quote["slopes"].as_array().unwrap();
    
    // Check array lengths match
    assert_eq!(assets.len(), liquidity.len(), "Array length mismatch");
    assert_eq!(assets.len(), prices.len(), "Array length mismatch");
    assert_eq!(assets.len(), slopes.len(), "Array length mismatch");
    
    println!("   ✓ Response schema valid");
    println!("   ✓ Array lengths consistent");
    
    // Check data types
    if !assets.is_empty() {
        assert!(assets[0].is_number(), "Asset ID should be number");
        assert!(liquidity[0].is_number(), "Liquidity should be number");
        assert!(prices[0].is_number(), "Price should be number");
        assert!(slopes[0].is_number(), "Slope should be number");
        
        // Check reasonable value ranges
        let price = prices[0].as_f64().unwrap();
        let slope = slopes[0].as_f64().unwrap();
        
        assert!(price > 0.0, "Price should be positive");
        assert!(slope > 0.0, "Slope should be positive");
        
        println!("   ✓ Data types correct");
        println!("   ✓ Value ranges reasonable");
        println!("   Sample data:");
        println!("     Asset: {}", assets[0]);
        println!("     Liquidity: {}", liquidity[0]);
        println!("     Price: ${:.2}", price);
        println!("     Slope: {:.6}", slope);
    }
    
    println!("\n=== Response Format Test Passed ===");
}