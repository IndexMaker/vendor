use super::messages::{OrderbookData, PriceLevel, TickerData};
use crate::market_data::types::{MarketDataEvent, PricePointEntry, Symbol};
use chrono::{DateTime, Utc};
use common::amount::Amount;
use crc32fast::Hasher;
use eyre::{eyre, Result};
use parking_lot::RwLock as AtomicLock;
use std::collections::BTreeMap;
use std::sync::Arc;

// Store both Amount and original string for CRC32
#[derive(Debug, Clone)]
struct BookLevel {
    price: Amount,
    quantity: Amount,
    price_str: String,
    quantity_str: String,
}

pub struct Book {
    pub symbol: Symbol,
    pub bids: BTreeMap<Amount, BookLevel>, // price -> level (descending)
    pub asks: BTreeMap<Amount, BookLevel>, // price -> level (ascending)
    pub last_update_timestamp: DateTime<Utc>,
    pub checksum: Option<i32>,
    pub observer: Arc<AtomicLock<crate::market_data::types::MarketDataObserver>>,
}

impl Book {
    pub fn new(
        symbol: Symbol,
        observer: Arc<AtomicLock<crate::market_data::types::MarketDataObserver>>,
    ) -> Self {
        Self {
            symbol,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            last_update_timestamp: Utc::now(),
            checksum: None,
            observer,
        }
    }

    pub fn check_stale(&self, expiry_time: DateTime<Utc>) -> bool {
        self.last_update_timestamp < expiry_time
    }

    pub fn apply_snapshot(&mut self, data: OrderbookData) -> Result<()> {
        tracing::debug!(
            "Applying snapshot for {} (checksum: {:?})",
            self.symbol,
            data.checksum
        );

        // Clear existing data
        self.bids.clear();
        self.asks.clear();

        // Parse and insert bids
        for level in &data.bids {
            if level.len() != 2 {
                continue;
            }
            let price_str = &level[0];
            let qty_str = &level[1];
            
            let price_level = PriceLevel::from_strings(price_str, qty_str)?;
            let book_level = BookLevel {
                price: price_level.price,
                quantity: price_level.quantity,
                price_str: price_str.clone(),
                quantity_str: qty_str.clone(),
            };
            self.bids.insert(price_level.price, book_level);
        }

        // Parse and insert asks
        for level in &data.asks {
            if level.len() != 2 {
                continue;
            }
            let price_str = &level[0];
            let qty_str = &level[1];
            
            let price_level = PriceLevel::from_strings(price_str, qty_str)?;
            let book_level = BookLevel {
                price: price_level.price,
                quantity: price_level.quantity,
                price_str: price_str.clone(),
                quantity_str: qty_str.clone(),
            };
            self.asks.insert(price_level.price, book_level);
        }

        self.checksum = data.checksum;
        self.last_update_timestamp = Utc::now();

        // Validate CRC32 if provided and not zero
        if let Some(expected_checksum) = data.checksum {
            if expected_checksum != 0 {
                self.validate_checksum(expected_checksum)?;
            }
        }

        // Emit snapshot event
        self.emit_snapshot();

        Ok(())
    }

    pub fn apply_update(&mut self, data: OrderbookData) -> Result<()> {
        tracing::trace!("Applying update for {}", self.symbol);

        // Update bids
        for level in &data.bids {
            if level.len() != 2 {
                continue;
            }
            let price_str = &level[0];
            let qty_str = &level[1];
            
            let price_level = PriceLevel::from_strings(price_str, qty_str)?;
            
            if price_level.quantity.is_not() {
                self.bids.remove(&price_level.price);
            } else {
                let book_level = BookLevel {
                    price: price_level.price,
                    quantity: price_level.quantity,
                    price_str: price_str.clone(),
                    quantity_str: qty_str.clone(),
                };
                self.bids.insert(price_level.price, book_level);
            }
        }

        // Update asks
        for level in &data.asks {
            if level.len() != 2 {
                continue;
            }
            let price_str = &level[0];
            let qty_str = &level[1];
            
            let price_level = PriceLevel::from_strings(price_str, qty_str)?;
            
            if price_level.quantity.is_not() {
                self.asks.remove(&price_level.price);
            } else {
                let book_level = BookLevel {
                    price: price_level.price,
                    quantity: price_level.quantity,
                    price_str: price_str.clone(),
                    quantity_str: qty_str.clone(),
                };
                self.asks.insert(price_level.price, book_level);
            }
        }

        self.last_update_timestamp = Utc::now();

        // Validate CRC32 if provided and not zero
        if let Some(expected_checksum) = data.checksum {
            if expected_checksum != 0 {
                self.validate_checksum(expected_checksum)?;
            }
        }

        // Emit delta event
        self.emit_delta(&data)?;

        Ok(())
    }

    pub fn apply_ticker(&mut self, data: TickerData) -> Result<()> {
        tracing::trace!("Applying ticker for {}", self.symbol);

        let event = Arc::new(MarketDataEvent::TopOfBook {
            symbol: self.symbol.clone(),
            sequence_number: 0,
            best_bid_price: data.get_bid_price()?,
            best_ask_price: data.get_ask_price()?,
            best_bid_quantity: data.get_bid_size()?,
            best_ask_quantity: data.get_ask_size()?,
            timestamp: Utc::now(),
        });

        self.observer.read().publish(event);
        Ok(())
    }

    fn emit_snapshot(&self) {
        let bid_updates: Vec<PricePointEntry> = self
            .bids
            .values()
            .map(|level| PricePointEntry {
                price: level.price,
                quantity: level.quantity,
            })
            .collect();

        let ask_updates: Vec<PricePointEntry> = self
            .asks
            .values()
            .map(|level| PricePointEntry {
                price: level.price,
                quantity: level.quantity,
            })
            .collect();

        let event = Arc::new(MarketDataEvent::OrderBookSnapshot {
            symbol: self.symbol.clone(),
            sequence_number: 0,
            bid_updates,
            ask_updates,
            timestamp: self.last_update_timestamp,
        });

        self.observer.read().publish(event);
    }

    fn emit_delta(&self, data: &OrderbookData) -> Result<()> {
        let bid_updates = OrderbookData::parse_levels(&data.bids)?
            .into_iter()
            .map(|level| PricePointEntry {
                price: level.price,
                quantity: level.quantity,
            })
            .collect();

        let ask_updates = OrderbookData::parse_levels(&data.asks)?
            .into_iter()
            .map(|level| PricePointEntry {
                price: level.price,
                quantity: level.quantity,
            })
            .collect();

        let event = Arc::new(MarketDataEvent::OrderBookDelta {
            symbol: self.symbol.clone(),
            sequence_number: 0,
            bid_updates,
            ask_updates,
            timestamp: self.last_update_timestamp,
        });

        self.observer.read().publish(event);
        Ok(())
    }

    fn validate_checksum(&self, expected: i32) -> Result<()> {
        let calculated = self.calculate_checksum();

        if calculated != expected {
            return Err(eyre!(
                "CRC32 checksum mismatch for {}: expected {}, got {}",
                self.symbol,
                expected,
                calculated
            ));
        }

        tracing::trace!("CRC32 checksum validated for {}: {}", self.symbol, calculated);
        Ok(())
    }

    fn calculate_checksum(&self) -> i32 {
        let mut hasher = Hasher::new();

        // Get first 25 bids (reversed for descending order) and 25 asks
        let bids: Vec<_> = self.bids.values().rev().take(25).collect();
        let asks: Vec<_> = self.asks.values().take(25).collect();

        // Build string alternating: bid1:ask1:bid2:ask2...
        let max_len = bids.len().max(asks.len());
        let mut parts = Vec::new();

        for i in 0..max_len {
            if let Some(bid) = bids.get(i) {
                parts.push(format!("{}:{}", bid.price_str, bid.quantity_str));
            }
            if let Some(ask) = asks.get(i) {
                parts.push(format!("{}:{}", ask.price_str, ask.quantity_str));
            }
        }

        let checksum_str = parts.join(":");
        tracing::trace!("CRC32 string for {}: {}", self.symbol, checksum_str);
        
        hasher.update(checksum_str.as_bytes());
        hasher.finalize() as i32
    }
}