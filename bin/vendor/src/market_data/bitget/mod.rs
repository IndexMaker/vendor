mod book;
mod books;
mod messages;
mod multi_subscriber;
pub mod orderbook_service;
pub mod rest_client;
mod subscriber;
mod websocket;

pub use orderbook_service::{OrderBookService, OrderBookServiceConfig};
pub use rest_client::{BitgetRestClient, BitgetRestConfig, OrderBookLevel, OrderBookSnapshot};
pub use subscriber::{BitgetSubscriber, BitgetSubscriberConfig};
pub use multi_subscriber::{MultiWebSocketSubscriber, MultiSubscriberConfig};