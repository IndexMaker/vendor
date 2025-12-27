use crate::market_data::types::MarketDataEvent;
use chrono::{DateTime, Utc};
use common::amount::Amount;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct PriceInfo {
    pub price: Amount,
    pub last_update: DateTime<Utc>,
}

pub struct PriceTracker {
    prices: Arc<RwLock<HashMap<String, PriceInfo>>>,
}

impl PriceTracker {
    pub fn new() -> Self {
        Self {
            prices: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn handle_event(&self, event: Arc<MarketDataEvent>) {
        match event.as_ref() {
            MarketDataEvent::TopOfBook {
                symbol,
                best_bid_price,
                best_ask_price,
                timestamp,
                ..
            } => {
                // Use mid price: (bid + ask) / 2
                let mid_price = if let (Some(bid), Some(ask)) = (
                    best_bid_price.checked_add(*best_ask_price),
                    Amount::TWO.checked_div(Amount::ONE),
                ) {
                    bid.checked_div(Amount::TWO).unwrap_or(*best_bid_price)
                } else {
                    *best_bid_price
                };

                let price_info = PriceInfo {
                    price: mid_price,
                    last_update: *timestamp,
                };

                self.prices.write().insert(symbol.clone(), price_info);

                tracing::trace!("Updated price for {}: {}", symbol, mid_price);
            }
            _ => {
                // Ignore snapshot and delta events
            }
        }
    }

    pub fn get_price(&self, symbol: &str) -> Option<Amount> {
        self.prices
            .read()
            .get(symbol)
            .map(|info| info.price)
    }

    pub fn get_prices(&self, symbols: &[String]) -> HashMap<String, Amount> {
        let prices = self.prices.read();
        symbols
            .iter()
            .filter_map(|symbol| {
                prices
                    .get(symbol)
                    .map(|info| (symbol.clone(), info.price))
            })
            .collect()
    }

    pub fn all_prices_available(&self, symbols: &[String]) -> bool {
        let prices = self.prices.read();
        symbols.iter().all(|symbol| prices.contains_key(symbol))
    }

    pub fn get_price_age(&self, symbol: &str) -> Option<chrono::Duration> {
        self.prices.read().get(symbol).map(|info| {
            let now = Utc::now();
            now - info.last_update
        })
    }

    pub fn summary(&self) -> String {
        let prices = self.prices.read();
        format!("PriceTracker: {} prices tracked", prices.len())
    }
}