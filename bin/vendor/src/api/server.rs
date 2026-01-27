use super::routes::{health, quote_assets, process_assets, update_market_data, refresh_quote, AppState};
use axum::{
    routing::{get, post},
    Router,
};
use std::net::SocketAddr;
use tokio_util::sync::CancellationToken;

pub struct ApiServer<P>
where
    P: alloy::providers::Provider + Clone,
{
    state: AppState<P>,
    addr: SocketAddr,
    cancel_token: CancellationToken,
}

impl<P> ApiServer<P>
where
    P: alloy::providers::Provider + Clone + Send + Sync + 'static,
{
    pub fn new(state: AppState<P>, addr: SocketAddr) -> Self {
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
            .route("/health", get(health::<P>))
            .route("/quote_assets", post(quote_assets::<P>))
            .route("/process-assets", post(process_assets::<P>))
            .route("/update-market-data", post(update_market_data::<P>))
            .route("/refresh-quote", post(refresh_quote::<P>))  // Story 0-1 AC6
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