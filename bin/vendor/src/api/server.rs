use super::routes::{health, quote_assets, AppState};
use axum::{
    routing::{get, post},
    Router,
};
use std::net::SocketAddr;
use tokio_util::sync::CancellationToken;

pub struct ApiServer {
    state: AppState,
    addr: SocketAddr,
    cancel_token: CancellationToken,
}

impl ApiServer {
    pub fn new(state: AppState, addr: SocketAddr) -> Self {
        Self {
            state,
            addr,
            cancel_token: CancellationToken::new(),
        }
    }
    
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }
    
    pub async fn start(self) -> eyre::Result<()> {
        let app = Router::new()
            .route("/health", get(health))
            .route("/quote_assets", post(quote_assets))
            .with_state(self.state);
        
        tracing::info!("API server listening on {}", self.addr);
        
        let listener = tokio::net::TcpListener::bind(&self.addr).await?;
        
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                self.cancel_token.cancelled().await;
            })
            .await?;
        
        Ok(())
    }
}