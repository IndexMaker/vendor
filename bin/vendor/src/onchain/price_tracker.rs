use std::collections::HashMap;
use parking_lot::RwLock;
use std::sync::Arc;
use crate::market_data::MarketDataEvent;
use crate::order_book::types::{OrderBook, Level};
use common::amount::Amount;

pub struct PriceTracker {
    /// Current prices (best mid)
    prices: Arc<RwLock<HashMap<String, f64>>>,
    
    /// Full order book snapshots (NEW)
    order_books: Arc<RwLock<HashMap<String, OrderBook>>>,
}

impl PriceTracker {
    pub fn new() -> Self {
        Self {
            prices: Arc::new(RwLock::new(HashMap::new())),
            order_books: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn handle_event(&self, event: Arc<MarketDataEvent>) {
        match event.as_ref() {
            MarketDataEvent::OrderBookSnapshot {
                symbol,
                bid_updates,
                ask_updates,
                ..
            } => {
                // Update order book - convert Amount to f64
                let mut books = self.order_books.write();
                
                let bids: Vec<Level> = bid_updates
                    .iter()
                    .map(|u| Level {
                        price: u.price.to_u128_raw() as f64 / 1e18,
                        quantity: u.quantity.to_u128_raw() as f64 / 1e18,
                    })
                    .collect();
                
                let asks: Vec<Level> = ask_updates
                    .iter()
                    .map(|u| Level {
                        price: u.price.to_u128_raw() as f64 / 1e18,
                        quantity: u.quantity.to_u128_raw() as f64 / 1e18,
                    })
                    .collect();
                
                let mut book = OrderBook::new(symbol.clone());
                book.bids = bids;
                book.asks = asks;
                book.timestamp = chrono::Utc::now();
                
                books.insert(symbol.clone(), book);
                
                // Also update price
                if !bid_updates.is_empty() && !ask_updates.is_empty() {
                    let best_bid = bid_updates[0].price.to_u128_raw() as f64 / 1e18;
                    let best_ask = ask_updates[0].price.to_u128_raw() as f64 / 1e18;
                    let mid = (best_bid + best_ask) / 2.0;
                    self.prices.write().insert(symbol.clone(), mid);
                }
            }
            
            MarketDataEvent::TopOfBook {
                symbol,
                best_bid_price,
                best_ask_price,
                ..
            } => {
                let bid = best_bid_price.to_u128_raw() as f64 / 1e18;
                let ask = best_ask_price.to_u128_raw() as f64 / 1e18;
                let mid = (bid + ask) / 2.0;
                self.prices.write().insert(symbol.clone(), mid);
            }
            
            _ => {}
        }
    }

    pub fn get_price(&self, symbol: &str) -> Option<Amount> {
        self.prices.read().get(symbol).map(|&price| {
            Amount::from_u128_raw((price * 1e18) as u128)
        })
    }

    // pub fn all_prices_available(&self, symbols: &[String]) -> bool {
    //     let prices = self.prices.read();
    //     symbols.iter().all(|s| prices.contains_key(s))
    // }
    
    /// Get full order book for a symbol (NEW)
    pub fn get_order_book(&self, symbol: &str) -> Option<OrderBook> {
        self.order_books.read().get(symbol).cloned()
    }
    
    // /// Check if order book is available with sufficient depth (NEW)
    // pub fn has_order_book(&self, symbol: &str, min_levels: usize) -> bool {
    //     self.order_books
    //         .read()
    //         .get(symbol)
    //         .map(|book| book.has_sufficient_depth(min_levels))
    //         .unwrap_or(false)
    // }
}