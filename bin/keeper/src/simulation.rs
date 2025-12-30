use crate::client::{CreateOrderRequest, OrderSide, VendorClient};
use eyre::Result;
use rand::Rng;
use std::time::Duration;

pub struct SimulationConfig {
    pub indices: Vec<String>,
    pub min_quantity: f64,
    pub max_quantity: f64,
    pub min_interval_secs: u64,
    pub max_interval_secs: u64,
    pub client_id_prefix: String,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            indices: vec!["SY100".to_string()],
            min_quantity: 1.0,
            max_quantity: 50.0,
            min_interval_secs: 10,
            max_interval_secs: 30,
            client_id_prefix: "keeper-sim".to_string(),
        }
    }
}

pub struct OrderSimulator {
    config: SimulationConfig,
    vendor_client: VendorClient,
    order_count: u64,
}

impl OrderSimulator {
    pub fn new(config: SimulationConfig, vendor_client: VendorClient) -> Self {
        Self {
            config,
            vendor_client,
            order_count: 0,
        }
    }

    /// Generate a random buy order
    fn generate_random_order(&mut self) -> CreateOrderRequest {
        let mut rng = rand::thread_rng();

        // Random index
        let index_symbol = self.config.indices[rng.gen_range(0..self.config.indices.len())].clone();

        // Random quantity
        let quantity = rng.gen_range(self.config.min_quantity..=self.config.max_quantity);
        let quantity_str = format!("{:.2}", quantity);

        // Client ID
        self.order_count += 1;
        let client_id = format!("{}-{:06}", self.config.client_id_prefix, self.order_count);

        CreateOrderRequest {
            side: OrderSide::Buy,
            index_symbol,
            quantity: quantity_str,
            client_id,
        }
    }

    /// Calculate random interval
    fn random_interval(&self) -> Duration {
        let mut rng = rand::thread_rng();
        let secs = rng.gen_range(self.config.min_interval_secs..=self.config.max_interval_secs);
        Duration::from_secs(secs)
    }

    /// Run simulation loop
    pub async fn run(&mut self) -> Result<()> {
        tracing::info!("Starting order simulation");
        tracing::info!("Indices: {:?}", self.config.indices);
        tracing::info!(
            "Quantity range: {:.2} - {:.2}",
            self.config.min_quantity,
            self.config.max_quantity
        );
        tracing::info!(
            "Interval range: {}s - {}s",
            self.config.min_interval_secs,
            self.config.max_interval_secs
        );

        loop {
            // Generate random order
            let order = self.generate_random_order();

            tracing::info!(
                "🎲 Generating order #{}: {} {} units of {}",
                self.order_count,
                order.side,
                order.quantity,
                order.index_symbol
            );

            // Submit to vendor
            match self.vendor_client.submit_order(order.clone()).await {
                Ok(response) => {
                    if response.success {
                        tracing::info!(
                            "✅ Order {} filled successfully: {}",
                            response.order_id,
                            response.message
                        );
                    } else {
                        tracing::warn!(
                            "⚠️  Order {} failed: {} (status: {})",
                            response.order_id,
                            response.message,
                            response.status
                        );
                    }
                }
                Err(e) => {
                    tracing::error!("❌ Failed to submit order: {:?}", e);
                }
            }

            // Wait random interval
            let interval = self.random_interval();
            tracing::debug!("Waiting {:?} before next order", interval);
            tokio::time::sleep(interval).await;
        }
    }
}