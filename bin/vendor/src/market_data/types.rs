use chrono::{DateTime, Utc};
use common::amount::Amount;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub type Symbol = String;

#[derive(Debug, Clone)]
pub struct Subscription {
    pub ticker: Symbol,
    pub exchange: String,
}

#[derive(Debug, Clone)]
pub enum MarketDataEvent {
    OrderBookSnapshot {
        symbol: Symbol,
        sequence_number: u64,
        bid_updates: Vec<PricePointEntry>,
        ask_updates: Vec<PricePointEntry>,
        timestamp: DateTime<Utc>,
    },
    OrderBookDelta {
        symbol: Symbol,
        sequence_number: u64,
        bid_updates: Vec<PricePointEntry>,
        ask_updates: Vec<PricePointEntry>,
        timestamp: DateTime<Utc>,
    },
    TopOfBook {
        symbol: Symbol,
        sequence_number: u64,
        best_bid_price: Amount,
        best_ask_price: Amount,
        best_bid_quantity: Amount,
        best_ask_quantity: Amount,
        timestamp: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricePointEntry {
    pub price: Amount,
    pub quantity: Amount,
}

// Simple observer pattern for publishing market data events
pub struct MarketDataObserver {
    callbacks: Vec<Box<dyn Fn(Arc<MarketDataEvent>) + Send + Sync>>,
}

impl MarketDataObserver {
    pub fn new() -> Self {
        Self {
            callbacks: Vec::new(),
        }
    }

    pub fn subscribe<F>(&mut self, callback: F)
    where
        F: Fn(Arc<MarketDataEvent>) + Send + Sync + 'static,
    {
        self.callbacks.push(Box::new(callback));
    }

    pub fn publish(&self, event: Arc<MarketDataEvent>) {
        for callback in &self.callbacks {
            callback(event.clone());
        }
    }
}