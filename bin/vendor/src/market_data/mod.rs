pub mod bitget;
pub mod margin;
pub mod types;

pub use bitget::{
    BitgetRestClient, BitgetRestConfig, BitgetSubscriber, BitgetSubscriberConfig,
    MultiWebSocketSubscriber, MultiSubscriberConfig,
    OrderBookLevel, OrderBookService, OrderBookServiceConfig, OrderBookSnapshot,
};
pub use margin::{compute_margin_vector, compute_supply_placeholder, MarginConfig};
pub use types::{MarketDataEvent, MarketDataObserver, Subscription};