use super::book::Book;
use crate::market_data::types::{MarketDataObserver, Symbol};
use chrono::{DateTime, Utc};
use eyre::{eyre, Result};
use parking_lot::RwLock as AtomicLock;
use std::collections::HashMap;
use std::sync::Arc;

pub struct Books {
    books: HashMap<Symbol, Book>,
    observer: Arc<AtomicLock<MarketDataObserver>>,
}

impl Books {
    pub fn new_with_observer(observer: Arc<AtomicLock<MarketDataObserver>>) -> Self {
        Self {
            books: HashMap::new(),
            observer,
        }
    }

    pub fn add_book(&mut self, symbol: &Symbol) -> Result<()> {
        if self.books.contains_key(symbol) {
            return Err(eyre!("Book already exists for {}", symbol));
        }

        let book = Book::new(symbol.clone(), self.observer.clone());
        self.books.insert(symbol.clone(), book);

        tracing::info!("Added book for {}", symbol);
        Ok(())
    }

    pub fn get_book_mut(&mut self, symbol: &Symbol) -> Result<&mut Book> {
        self.books
            .get_mut(symbol)
            .ok_or_else(|| eyre!("Book not found for {}", symbol))
    }

    pub fn check_stale(&self, expiry_time: DateTime<Utc>) -> Result<(usize, usize)> {
        let total = self.books.len();
        let stale_count = self
            .books
            .values()
            .filter(|book| book.check_stale(expiry_time))
            .count();

        Ok((stale_count, total))
    }
}