use super::handlers::{create_order, get_inventory_summary, health_check, AppState};
use crate::inventory::InventoryManager;
use axum::{
    routing::{get, post},
    Router,
};
use eyre::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::RwLock; // Changed to tokio::sync::RwLock
use tokio_util::sync::CancellationToken;

pub struct ApiServer {
    state: AppState,
    addr: SocketAddr,
    cancel_token: CancellationToken,
}

impl ApiServer {
    pub fn new(
        inventory: Arc<RwLock<InventoryManager>>,
        addr: SocketAddr,
    ) -> Self {
        let state = AppState { inventory };

        Self {
            state,
            addr,
            cancel_token: CancellationToken::new(),
        }
    }

    pub async fn start(self) -> Result<()> {
        let app = Router::new()
            .route("/health", get(health_check))
            .route("/api/v1/orders", post(create_order))
            .route("/api/v1/inventory", get(get_inventory_summary))
            .with_state(self.state);

        let listener = TcpListener::bind(&self.addr).await?;
        tracing::info!("API server listening on {}", self.addr);

        let cancel_token = self.cancel_token.clone();

        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                cancel_token.cancelled().await;
                tracing::info!("API server shutting down gracefully");
            })
            .await?;

        Ok(())
    }

    pub fn stop(&self) {
        tracing::info!("Stopping API server");
        self.cancel_token.cancel();
    }

    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }
}