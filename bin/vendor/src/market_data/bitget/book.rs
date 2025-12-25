use super::messages::{OrderbookData, PriceLevel, TickerData};
use crate::market_data::types::{MarketDataEvent, PricePointEntry, Symbol};
use chrono::{DateTime, Utc};
use common::amount::Amount;
use crc32fast::Hasher;
use eyre::{eyre, Result};
use parking_lot::RwLock as AtomicLock;
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct Book {
    pub symbol: Symbol,
    pub bids: BTreeMap<Amount, Amount>, // price -> quantity (descending)
    pub asks: BTreeMap<Amount, Amount>, // price -> quantity (ascending)
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
        let bids = data.get_bids()?;
        for PriceLevel { price, quantity } in bids.iter() {
            self.bids.insert(*price, *quantity);
        }

        // Parse and insert asks
        let asks = data.get_asks()?;
        for PriceLevel { price, quantity } in asks.iter() {
            self.asks.insert(*price, *quantity);
        }

        self.checksum = data.checksum;
        self.last_update_timestamp = Utc::now();

        // Validate CRC32 if provided
        if let Some(expected_checksum) = data.checksum {
            self.validate_checksum(expected_checksum)?;
        }

        // Emit snapshot event
        self.emit_snapshot(&bids, &asks);

        Ok(())
    }

    pub fn apply_update(&mut self, data: OrderbookData) -> Result<()> {
        tracing::trace!("Applying update for {}", self.symbol);

        let bids = data.get_bids()?;
        let asks = data.get_asks()?;

        // Update bids
        for PriceLevel { price, quantity } in &bids {
            if quantity.is_not() {
                self.bids.remove(price);
            } else {
                self.bids.insert(*price, *quantity);
            }
        }

        // Update asks
        for PriceLevel { price, quantity } in &asks {
            if quantity.is_not() {
                self.asks.remove(price);
            } else {
                self.asks.insert(*price, *quantity);
            }
        }

        self.last_update_timestamp = Utc::now();

        // Validate CRC32 if provided
        if let Some(expected_checksum) = data.checksum {
            self.validate_checksum(expected_checksum)?;
        }

        // Emit delta event
        self.emit_delta(&bids, &asks);

        Ok(())
    }

    pub fn apply_ticker(&mut self, data: TickerData) -> Result<()> {
        tracing::trace!("Applying ticker for {}", self.symbol);

        let event = Arc::new(MarketDataEvent::TopOfBook {
            symbol: self.symbol.clone(),
            sequence_number: 0, // Bitget doesn't provide sequence numbers
            best_bid_price: data.get_bid_price()?,
            best_ask_price: data.get_ask_price()?,
            best_bid_quantity: data.get_bid_size()?,
            best_ask_quantity: data.get_ask_size()?,
            timestamp: Utc::now(),
        });

        self.observer.read().publish(event);
        Ok(())
    }

    fn emit_snapshot(&self, bids: &[PriceLevel], asks: &[PriceLevel]) {
        let event = Arc::new(MarketDataEvent::OrderBookSnapshot {
            symbol: self.symbol.clone(),
            sequence_number: 0,
            bid_updates: bids
                .iter()
                .map(|level| PricePointEntry {
                    price: level.price,
                    quantity: level.quantity,
                })
                .collect(),
            ask_updates: asks
                .iter()
                .map(|level| PricePointEntry {
                    price: level.price,
                    quantity: level.quantity,
                })
                .collect(),
            timestamp: self.last_update_timestamp,
        });

        self.observer.read().publish(event);
    }

    fn emit_delta(&self, bids: &[PriceLevel], asks: &[PriceLevel]) {
        let event = Arc::new(MarketDataEvent::OrderBookDelta {
            symbol: self.symbol.clone(),
            sequence_number: 0,
            bid_updates: bids
                .iter()
                .map(|level| PricePointEntry {
                    price: level.price,
                    quantity: level.quantity,
                })
                .collect(),
            ask_updates: asks
                .iter()
                .map(|level| PricePointEntry {
                    price: level.price,
                    quantity: level.quantity,
                })
                .collect(),
            timestamp: self.last_update_timestamp,
        });

        self.observer.read().publish(event);
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

        tracing::trace!("CRC32 checksum validated for {}", self.symbol);
        Ok(())
    }

    fn calculate_checksum(&self) -> i32 {
        let mut hasher = Hasher::new();

        // Get first 25 bids (reversed for descending order) and 25 asks
        let bids: Vec<_> = self.bids.iter().rev().take(25).collect();
        let asks: Vec<_> = self.asks.iter().take(25).collect();

        // Build string alternating: bid1:ask1:bid2:ask2...
        let max_len = bids.len().max(asks.len());
        let mut parts = Vec::new();

        for i in 0..max_len {
            if let Some((price, qty)) = bids.get(i) {
                parts.push(format!("{}:{}", price, qty));
            }
            if let Some((price, qty)) = asks.get(i) {
                parts.push(format!("{}:{}", price, qty));
            }
        }

        let checksum_str = parts.join(":");
        hasher.update(checksum_str.as_bytes());
        hasher.finalize() as i32
    }
}