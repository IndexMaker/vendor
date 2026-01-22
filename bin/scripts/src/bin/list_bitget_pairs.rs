//! Bitget Pair Listing Script
//!
//! Fetches all Bitget spot trading pairs, prioritizes USDC over USDT,
//! assigns sequential IDs, and outputs JSON for the PairRegistry contract.

use clap::Parser;
use eyre::{eyre, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tracing::{debug, error, info, warn};

const BITGET_BASE_URL: &str = "https://api.bitget.com";
const PRODUCTS_ENDPOINT: &str = "/api/v2/spot/public/symbols";
const MAX_RETRIES: u32 = 3;
const BASE_RETRY_DELAY_MS: u64 = 500; // Base delay for exponential backoff

/// CLI arguments for the script
#[derive(Parser, Debug)]
#[command(name = "list-bitget-pairs")]
#[command(about = "Fetch Bitget trading pairs and output JSON with USDC preference")]
struct Args {
    /// Output directory for JSON files
    #[arg(short, long, default_value = "vendor/data")]
    output_dir: PathBuf,

    /// Force overwrite even if no changes detected
    #[arg(short, long)]
    force: bool,

    /// Dry run - don't write files
    #[arg(short, long)]
    dry_run: bool,
}

/// Response from Bitget API
#[derive(Debug, Deserialize)]
struct BitgetResponse {
    code: String,
    msg: String,
    data: Option<Vec<BitgetProduct>>,
}

/// Product/pair data from Bitget API v2
/// API docs: https://www.bitget.com/api-doc/spot/public/Get-Symbols
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BitgetProduct {
    symbol: String,
    base_coin: String,
    quote_coin: String,
    min_trade_amount: String,
    #[allow(dead_code)]
    max_trade_amount: String,
    #[allow(dead_code)]
    #[serde(default)]
    taker_fee_rate: String,
    #[allow(dead_code)]
    #[serde(default)]
    maker_fee_rate: String,
    price_precision: String,
    quantity_precision: String,
    status: String,
    #[allow(dead_code)]
    #[serde(default)]
    min_trade_usdt: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    buy_limit_price_ratio: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    sell_limit_price_ratio: Option<String>,
}

/// Output pair format for Vendor service
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct OutputPair {
    id: u32,
    symbol: String,
    base_coin: String,
    quote_coin: String,
    min_trade_amount: String,
    price_scale: u8,
    quantity_scale: u8,
}

/// Solidity-compatible output format for PairRegistry contract
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SolidityPair {
    id: u32,
    symbol: String,
    base_coin: String,
    quote_coin: String,
}

/// Statistics for logging
#[derive(Debug, Default)]
struct Stats {
    total_fetched: usize,
    online_pairs: usize,
    usdc_selected: usize,
    usdt_selected: usize,
    usdt_with_usdc_available: usize,
    excluded_offline: usize,
    excluded_other_quote: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("list_bitget_pairs=info".parse().unwrap()),
        )
        .init();

    let args = Args::parse();

    info!("🚀 Starting Bitget pair listing script");
    info!("Output directory: {:?}", args.output_dir);

    // Task 2: Fetch all spot pairs
    let products = fetch_bitget_products().await?;

    let mut stats = Stats {
        total_fetched: products.len(),
        ..Default::default()
    };
    info!("📥 Fetched {} total pairs from Bitget", stats.total_fetched);

    // Filter for online pairs only
    let online_products: Vec<_> = products
        .into_iter()
        .filter(|p| {
            if p.status == "online" {
                true
            } else {
                stats.excluded_offline += 1;
                debug!("Excluded {} (status: {})", p.symbol, p.status);
                false
            }
        })
        .collect();

    stats.online_pairs = online_products.len();
    info!("✅ {} pairs are online", stats.online_pairs);

    // Task 3: USDC Priority Filtering
    let selected_pairs = select_pairs_with_usdc_priority(&online_products, &mut stats);
    info!(
        "📊 Selected {} pairs ({} USDC, {} USDT fallback)",
        selected_pairs.len(),
        stats.usdc_selected,
        stats.usdt_selected
    );

    // Task 4: Sequential ID Assignment (alphabetical by baseCoin for determinism)
    let mut sorted_pairs = selected_pairs;
    sorted_pairs.sort_by(|a, b| a.base_coin.cmp(&b.base_coin));

    let output_pairs: Vec<OutputPair> = sorted_pairs
        .iter()
        .enumerate()
        .map(|(idx, product)| OutputPair {
            id: (idx + 1) as u32,
            symbol: format!("{}/{}", product.base_coin, product.quote_coin),
            base_coin: product.base_coin.clone(),
            quote_coin: product.quote_coin.clone(),
            min_trade_amount: product.min_trade_amount.clone(),
            price_scale: product.price_precision.parse().unwrap_or(2),
            quantity_scale: product.quantity_precision.parse().unwrap_or(4),
        })
        .collect();

    info!("🔢 Assigned sequential IDs 1-{}", output_pairs.len());

    // Task 6: Comprehensive Logging
    log_stats(&stats);

    // Prepare output paths
    let output_dir = &args.output_dir;
    let pairs_file = output_dir.join("bitget-pairs.json");
    let solidity_file = output_dir.join("bitget-pairs-solidity.json");

    // Task 7: Idempotent Execution - compare with existing
    let changes = check_for_changes(&pairs_file, &output_pairs)?;

    if changes.is_empty() && !args.force {
        info!("✨ No changes detected - output file is already up to date");
        return Ok(());
    }

    if !changes.is_empty() {
        info!("📝 Changes detected:");
        for change in &changes {
            info!("  {}", change);
        }
    }

    if args.dry_run {
        info!("🔍 Dry run - no files written");
        return Ok(());
    }

    // Task 5: JSON Output Generation
    // Create output directory if needed
    std::fs::create_dir_all(output_dir)?;

    // Write primary output file
    let json_output = serde_json::to_string_pretty(&output_pairs)?;
    std::fs::write(&pairs_file, &json_output)?;
    info!("💾 Wrote {} pairs to {:?}", output_pairs.len(), pairs_file);

    // Write Solidity-compatible format
    let solidity_pairs: Vec<SolidityPair> = output_pairs
        .iter()
        .map(|p| SolidityPair {
            id: p.id,
            symbol: p.symbol.clone(),
            base_coin: p.base_coin.clone(),
            quote_coin: p.quote_coin.clone(),
        })
        .collect();
    let solidity_output = serde_json::to_string_pretty(&solidity_pairs)?;
    std::fs::write(&solidity_file, &solidity_output)?;
    info!("💾 Wrote Solidity format to {:?}", solidity_file);

    info!("✅ Script completed successfully");
    Ok(())
}

/// Calculate exponential backoff delay: base * 2^(attempt-1)
/// Uses saturating arithmetic to prevent overflow if MAX_RETRIES is increased
fn exponential_backoff(attempt: u32) -> u64 {
    let shift = attempt.saturating_sub(1);
    let multiplier = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    BASE_RETRY_DELAY_MS.saturating_mul(multiplier)
}

/// Fetch products from Bitget API with retry logic and exponential backoff
async fn fetch_bitget_products() -> Result<Vec<BitgetProduct>> {
    let client = reqwest::Client::builder()
        .https_only(true)
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()?;
    let url = format!("{}{}", BITGET_BASE_URL, PRODUCTS_ENDPOINT);

    for attempt in 1..=MAX_RETRIES {
        info!("📡 Fetching Bitget products (attempt {}/{})", attempt, MAX_RETRIES);

        match client.get(&url).send().await {
            Ok(response) => {
                if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    let delay = exponential_backoff(attempt) * 2; // Extra delay for rate limits
                    warn!("Rate limited (429) - waiting {}ms before retry", delay);
                    tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                    continue;
                }

                if !response.status().is_success() {
                    error!("API error: {}", response.status());
                    if attempt < MAX_RETRIES {
                        let delay = exponential_backoff(attempt);
                        info!("Retrying after {}ms...", delay);
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                        continue;
                    }
                    return Err(eyre!("API returned error status: {}", response.status()));
                }

                let bitget_response: BitgetResponse = response.json().await?;

                if bitget_response.code != "00000" {
                    return Err(eyre!(
                        "Bitget API error: {} - {}",
                        bitget_response.code,
                        bitget_response.msg
                    ));
                }

                let products = bitget_response.data.ok_or_else(|| eyre!("Empty response data"))?;

                if products.is_empty() {
                    return Err(eyre!("Bitget returned empty products list - this is unexpected"));
                }

                return Ok(products);
            }
            Err(e) => {
                error!("Network error: {}", e);
                if attempt < MAX_RETRIES {
                    let delay = exponential_backoff(attempt);
                    info!("Retrying after {}ms...", delay);
                    tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                    continue;
                }
                return Err(e.into());
            }
        }
    }

    Err(eyre!("Failed to fetch products after {} attempts", MAX_RETRIES))
}

/// Select pairs with USDC preference over USDT
fn select_pairs_with_usdc_priority(products: &[BitgetProduct], stats: &mut Stats) -> Vec<BitgetProduct> {
    // Group by base coin
    let mut by_base: HashMap<&str, Vec<&BitgetProduct>> = HashMap::new();

    for product in products {
        // Only consider USDC and USDT quote coins
        if product.quote_coin != "USDC" && product.quote_coin != "USDT" {
            stats.excluded_other_quote += 1;
            debug!(
                "Excluded {}/{} (quote coin not USDC/USDT)",
                product.base_coin, product.quote_coin
            );
            continue;
        }

        by_base
            .entry(&product.base_coin)
            .or_default()
            .push(product);
    }

    let mut selected: Vec<BitgetProduct> = Vec::new();

    for (base_coin, pairs) in &by_base {
        let usdc_pair = pairs.iter().find(|p| p.quote_coin == "USDC");
        let usdt_pair = pairs.iter().find(|p| p.quote_coin == "USDT");

        match (usdc_pair, usdt_pair) {
            (Some(usdc), Some(_usdt)) => {
                // Prefer USDC when both available
                selected.push((*usdc).clone());
                stats.usdc_selected += 1;
                stats.usdt_with_usdc_available += 1;
                info!(
                    "Selected {}/USDC (USDT also available)",
                    base_coin
                );
            }
            (Some(usdc), None) => {
                // Only USDC available
                selected.push((*usdc).clone());
                stats.usdc_selected += 1;
                debug!("Selected {}/USDC (only option)", base_coin);
            }
            (None, Some(usdt)) => {
                // Only USDT available - use as fallback
                selected.push((*usdt).clone());
                stats.usdt_selected += 1;
                info!(
                    "Selected {}/USDT (no USDC pair available)",
                    base_coin
                );
            }
            (None, None) => {
                // Should not happen given our filter, but handle gracefully
                warn!("No USDC or USDT pair for {}", base_coin);
            }
        }
    }

    selected
}

/// Check for changes compared to existing file
fn check_for_changes(existing_path: &PathBuf, new_pairs: &[OutputPair]) -> Result<Vec<String>> {
    let mut changes = Vec::new();

    if !existing_path.exists() {
        changes.push(format!("New file will be created: {:?}", existing_path));
        return Ok(changes);
    }

    let existing_content = std::fs::read_to_string(existing_path)?;
    let existing_pairs: Vec<OutputPair> = serde_json::from_str(&existing_content)?;

    // Build lookup maps
    let existing_by_base: HashMap<_, _> = existing_pairs
        .iter()
        .map(|p| (p.base_coin.as_str(), p))
        .collect();
    let new_by_base: HashMap<_, _> = new_pairs
        .iter()
        .map(|p| (p.base_coin.as_str(), p))
        .collect();

    // Find added pairs
    for (base, pair) in &new_by_base {
        if !existing_by_base.contains_key(base) {
            changes.push(format!("+ Added: {} (ID {})", pair.symbol, pair.id));
        }
    }

    // Find removed pairs
    for (base, pair) in &existing_by_base {
        if !new_by_base.contains_key(base) {
            changes.push(format!("- Removed: {} (was ID {})", pair.symbol, pair.id));
        }
    }

    // Find modified pairs (quote coin changed, etc.)
    for (base, new_pair) in &new_by_base {
        if let Some(existing_pair) = existing_by_base.get(base) {
            if existing_pair.quote_coin != new_pair.quote_coin {
                changes.push(format!(
                    "~ Changed quote: {} → {} for {}",
                    existing_pair.quote_coin, new_pair.quote_coin, base
                ));
            }
        }
    }

    Ok(changes)
}

/// Log comprehensive statistics
fn log_stats(stats: &Stats) {
    info!("📈 Statistics:");
    info!("  Total pairs fetched: {}", stats.total_fetched);
    info!("  Online pairs: {}", stats.online_pairs);
    info!("  Excluded (offline): {}", stats.excluded_offline);
    info!("  Excluded (other quote): {}", stats.excluded_other_quote);
    info!("  USDC pairs selected: {}", stats.usdc_selected);
    info!("  USDT pairs selected (fallback): {}", stats.usdt_selected);
    info!(
        "  USDT pairs skipped (USDC available): {}",
        stats.usdt_with_usdc_available
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exponential_backoff() {
        // Base delay is 500ms
        assert_eq!(exponential_backoff(1), 500);   // 500 * 2^0 = 500ms
        assert_eq!(exponential_backoff(2), 1000);  // 500 * 2^1 = 1000ms
        assert_eq!(exponential_backoff(3), 2000);  // 500 * 2^2 = 2000ms
    }

    fn make_test_product(base: &str, quote: &str) -> BitgetProduct {
        BitgetProduct {
            symbol: format!("{}{}", base, quote),
            base_coin: base.to_string(),
            quote_coin: quote.to_string(),
            min_trade_amount: "0.0001".to_string(),
            max_trade_amount: "10000".to_string(),
            taker_fee_rate: "0.001".to_string(),
            maker_fee_rate: "0.001".to_string(),
            price_precision: "2".to_string(),
            quantity_precision: "4".to_string(),
            status: "online".to_string(),
            min_trade_usdt: Some("5".to_string()),
            buy_limit_price_ratio: None,
            sell_limit_price_ratio: None,
        }
    }

    #[test]
    fn test_usdc_priority_both_available() {
        let products = vec![
            make_test_product("BTC", "USDC"),
            make_test_product("BTC", "USDT"),
        ];

        let mut stats = Stats::default();
        let selected = select_pairs_with_usdc_priority(&products, &mut stats);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].quote_coin, "USDC");
        assert_eq!(stats.usdc_selected, 1);
        assert_eq!(stats.usdt_selected, 0);
    }

    #[test]
    fn test_usdt_fallback() {
        let products = vec![make_test_product("XYZ", "USDT")];

        let mut stats = Stats::default();
        let selected = select_pairs_with_usdc_priority(&products, &mut stats);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].quote_coin, "USDT");
        assert_eq!(stats.usdc_selected, 0);
        assert_eq!(stats.usdt_selected, 1);
    }

    #[test]
    fn test_sequential_ids_alphabetical() {
        let pairs = vec![
            OutputPair {
                id: 0,
                symbol: "BTC/USDC".to_string(),
                base_coin: "BTC".to_string(),
                quote_coin: "USDC".to_string(),
                min_trade_amount: "0.0001".to_string(),
                price_scale: 2,
                quantity_scale: 4,
            },
            OutputPair {
                id: 0,
                symbol: "ADA/USDC".to_string(),
                base_coin: "ADA".to_string(),
                quote_coin: "USDC".to_string(),
                min_trade_amount: "1".to_string(),
                price_scale: 4,
                quantity_scale: 0,
            },
            OutputPair {
                id: 0,
                symbol: "ETH/USDC".to_string(),
                base_coin: "ETH".to_string(),
                quote_coin: "USDC".to_string(),
                min_trade_amount: "0.001".to_string(),
                price_scale: 2,
                quantity_scale: 4,
            },
        ];

        let mut sorted = pairs.clone();
        sorted.sort_by(|a, b| a.base_coin.cmp(&b.base_coin));
        let assigned: Vec<_> = sorted
            .iter()
            .enumerate()
            .map(|(i, p)| (p.base_coin.clone(), i + 1))
            .collect();

        assert_eq!(assigned[0], ("ADA".to_string(), 1));
        assert_eq!(assigned[1], ("BTC".to_string(), 2));
        assert_eq!(assigned[2], ("ETH".to_string(), 3));
    }

    #[test]
    fn test_output_pair_serialization() {
        let pair = OutputPair {
            id: 1,
            symbol: "BTC/USDC".to_string(),
            base_coin: "BTC".to_string(),
            quote_coin: "USDC".to_string(),
            min_trade_amount: "0.0001".to_string(),
            price_scale: 2,
            quantity_scale: 4,
        };

        let json = serde_json::to_string(&pair).unwrap();
        assert!(json.contains("\"id\":1"));
        assert!(json.contains("\"symbol\":\"BTC/USDC\""));
        assert!(json.contains("\"baseCoin\":\"BTC\""));
        assert!(json.contains("\"quoteCoin\":\"USDC\""));
    }

    #[test]
    fn test_excludes_other_quote_coins() {
        // EUR pair should be excluded (not USDC/USDT)
        let products = vec![
            make_test_product("BTC", "USDC"),
            make_test_product("BTC", "EUR"),  // Should be excluded
            make_test_product("ETH", "BTC"),  // Should be excluded
        ];

        let mut stats = Stats::default();
        let selected = select_pairs_with_usdc_priority(&products, &mut stats);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].base_coin, "BTC");
        assert_eq!(selected[0].quote_coin, "USDC");
        assert_eq!(stats.excluded_other_quote, 2);
    }

    #[test]
    fn test_offline_pair_filtering() {
        // Offline pairs should be excluded before USDC priority selection
        let mut online_product = make_test_product("BTC", "USDC");
        online_product.status = "online".to_string();

        let mut offline_product = make_test_product("ETH", "USDC");
        offline_product.status = "offline".to_string();

        // The main function filters offline pairs before calling select_pairs_with_usdc_priority
        // So we test that the filter logic would work correctly
        let all_products = vec![online_product.clone(), offline_product.clone()];
        let online_only: Vec<_> = all_products
            .into_iter()
            .filter(|p| p.status == "online")
            .collect();

        assert_eq!(online_only.len(), 1);
        assert_eq!(online_only[0].base_coin, "BTC");
    }
}
