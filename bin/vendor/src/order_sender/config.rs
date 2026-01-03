use super::simulated::SimulatedOrderSender;
use super::traits::OrderSender;
use derive_builder::Builder;
use eyre::Result;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub enum OrderSenderMode {
    Simulated { failure_rate: f64 },
    Bitget(BitgetCredentials), // TODO: Implement in later phase
}

#[derive(Debug, Clone)]
pub struct BitgetCredentials {
    pub account_name: String,
    pub api_key: String,
    pub api_secret: String,
    pub passphrase: String,
}

impl BitgetCredentials {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            account_name: std::env::var("BITGET_ACCOUNT_NAME")
                .unwrap_or_else(|_| "Bitget-Account-1".to_string()),
            api_key: std::env::var("BITGET_API_KEY")?,
            api_secret: std::env::var("BITGET_API_SECRET")?,
            passphrase: std::env::var("BITGET_PASSPHRASE")?,
        })
    }

    pub fn trading_enabled_from_env() -> bool {
        std::env::var("BITGET_TRADING_ENABLED")
            .unwrap_or_else(|_| "0".to_string())
            == "1"
    }
}

#[derive(Builder)]
#[builder(pattern = "owned", build_fn(skip))] // Skip auto-generated build
pub struct OrderSenderConfig {
    #[builder(setter(into))]
    pub mode: OrderSenderMode,

    #[builder(setter(into), default = "5")]
    pub price_limit_bps: u16,

    #[builder(setter(into), default = "3")]
    pub retry_attempts: u8,

    #[builder(setter(into), default = "false")]
    pub trading_enabled: bool,

    #[builder(setter(skip))]
    sender: Option<Arc<RwLock<dyn OrderSender>>>,
}

impl OrderSenderConfig {
    pub fn builder() -> OrderSenderConfigBuilder {
        OrderSenderConfigBuilder::default()
    }

    pub fn get_sender(&self) -> Option<Arc<RwLock<dyn OrderSender>>> {
        self.sender.clone()
    }

    pub async fn start(&self) -> Result<()> {
        if let Some(sender) = &self.sender {
            sender.write().await.start().await?;
        }
        Ok(())
    }

    pub async fn stop(&self) -> Result<()> {
        if let Some(sender) = &self.sender {
            sender.write().await.stop().await?;
        }
        Ok(())
    }
}

impl OrderSenderConfigBuilder {
    pub fn build(self) -> Result<OrderSenderConfig> {
        let mode = self.mode.ok_or_else(|| eyre::eyre!("mode is required"))?;
        let price_limit_bps = self.price_limit_bps.unwrap_or(5);
        let retry_attempts = self.retry_attempts.unwrap_or(3);
        let trading_enabled = self.trading_enabled.unwrap_or(false);

        let sender: Arc<RwLock<dyn OrderSender>> = match &mode {
            OrderSenderMode::Simulated { failure_rate } => {
                let sim_sender = SimulatedOrderSender::new().with_failure_rate(*failure_rate);
                Arc::new(RwLock::new(sim_sender))
            }
            OrderSenderMode::Bitget(credentials) => {
                let bitget_sender = super::bitget::BitgetOrderSender::new(
                    credentials.api_key.clone(),
                    credentials.api_secret.clone(),
                    credentials.passphrase.clone(),
                    price_limit_bps,
                    retry_attempts,
                    trading_enabled,
                );
                Arc::new(RwLock::new(bitget_sender))
            }
        };

        Ok(OrderSenderConfig {
            mode,
            price_limit_bps,
            retry_attempts,
            trading_enabled,
            sender: Some(sender),
        })
    }
}