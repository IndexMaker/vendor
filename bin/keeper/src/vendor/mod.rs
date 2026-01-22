mod client;
mod types;

pub use client::{UpdateMarketDataConfig, VendorClient};
pub use types::AssetsQuote;
// Story 2.5: Export update market data types
pub use types::UpdateMarketDataRequest;