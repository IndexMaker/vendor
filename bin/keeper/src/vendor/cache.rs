use super::types::AssetsQuote;
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Cached quote data with timestamp
#[derive(Debug, Clone)]
struct CachedQuote {
    quote: AssetsQuote,
    cached_at: DateTime<Utc>,
}

/// Cache for vendor quotes to avoid redundant requests
pub struct QuoteCache {
    cache: Arc<RwLock<HashMap<Vec<u128>, CachedQuote>>>,
    ttl_secs: i64,
}

impl QuoteCache {
    pub fn new(ttl_secs: i64) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            ttl_secs,
        }
    }

    /// Store a quote in the cache
    pub fn put(&self, asset_ids: Vec<u128>, quote: AssetsQuote) {
        let mut cache = self.cache.write();
        
        let mut sorted_ids = asset_ids;
        sorted_ids.sort_unstable();
        
        cache.insert(
            sorted_ids,
            CachedQuote {
                quote,
                cached_at: Utc::now(),
            },
        );
    }

    /// Get a quote from cache if not expired
    pub fn get(&self, asset_ids: &[u128]) -> Option<AssetsQuote> {
        let cache = self.cache.read();
        
        let mut sorted_ids = asset_ids.to_vec();
        sorted_ids.sort_unstable();
        
        cache.get(&sorted_ids).and_then(|cached| {
            let age = Utc::now().signed_duration_since(cached.cached_at);
            
            if age.num_seconds() < self.ttl_secs {
                tracing::debug!("Cache hit for {} assets (age: {}s)", asset_ids.len(), age.num_seconds());
                Some(cached.quote.clone())
            } else {
                tracing::debug!("Cache expired for {} assets (age: {}s)", asset_ids.len(), age.num_seconds());
                None
            }
        })
    }

    /// Clear all cached quotes
    pub fn clear(&self) {
        self.cache.write().clear();
        tracing::debug!("Quote cache cleared");
    }

    /// Remove expired entries
    pub fn cleanup(&self) {
        let mut cache = self.cache.write();
        let now = Utc::now();
        
        cache.retain(|_, cached| {
            let age = now.signed_duration_since(cached.cached_at);
            age.num_seconds() < self.ttl_secs
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quote_cache() {
        let cache = QuoteCache::new(5); // 5 second TTL
        
        let asset_ids = vec![101, 102, 103];
        let quote = AssetsQuote {
            assets: vec![101],
            liquidity: vec![25.5],
            prices: vec![95000.0],
            slopes: vec![3.5],
        };
        
        // Put and get
        cache.put(asset_ids.clone(), quote.clone());
        let cached = cache.get(&asset_ids);
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().assets, quote.assets);
        
        // Cache miss for different assets
        let other_ids = vec![104, 105];
        assert!(cache.get(&other_ids).is_none());
    }
}