use super::traits::OrderSender;
use super::types::{AssetOrder, ExecutionResult, OrderStatus};
use chrono::Utc;
use common::amount::Amount;
use eyre::Result;
use rand::Rng;
use std::collections::HashMap;

pub struct SimulatedOrderSender {
    balances: HashMap<String, Amount>,
    active: bool,
    mock_failure_rate: f64, // 0.0 = never fail, 1.0 = always fail
}

impl SimulatedOrderSender {
    pub fn new() -> Self {
        Self {
            balances: HashMap::new(),
            active: false,
            mock_failure_rate: 0.0,
        }
    }

    pub fn with_failure_rate(mut self, rate: f64) -> Self {
        self.mock_failure_rate = rate.clamp(0.0, 1.0);
        self
    }

    /// Generate mock price for an asset (between $1 and $1000)
    fn generate_mock_price(&self) -> Amount {
        let mut rng = rand::thread_rng();
        let price_f64 = rng.gen_range(1.0..=1000.0);
        Amount::from_u128_raw((price_f64 * 1e18) as u128)
    }

    /// Simulate order execution with random price
    fn simulate_fill(&self, order: &AssetOrder) -> ExecutionResult {
        let mut rng = rand::thread_rng();

        // Randomly fail based on failure rate
        if rng.gen_bool(self.mock_failure_rate) {
            return ExecutionResult::failed(
                order.symbol.clone(),
                format!("SIM-{}", Utc::now().timestamp_millis()),
                "Simulated random failure".to_string(),
            );
        }

        let order_id = format!("SIM-{}", Utc::now().timestamp_millis());
        let avg_price = self.generate_mock_price();

        // Calculate fees: 0.1% of notional value
        let notional = order
            .quantity
            .checked_mul(avg_price)
            .unwrap_or(Amount::ZERO);
        let fee_rate = Amount::from_u128_raw((0.001 * 1e18) as u128); // 0.1%
        let fees = notional
            .checked_mul(fee_rate)
            .unwrap_or(Amount::ZERO);

        // Create fee detail (simulate as maker 70% of the time)
        let fee_type = if rng.gen_bool(0.7) {
            super::types::FeeType::Maker
        } else {
            super::types::FeeType::Taker
        };

        let fee_detail = Some(super::types::FeeDetail::new(
            fees,
            "USDT".to_string(),
            fee_type,
        ));

        tracing::debug!(
            "Simulated fill: {} {} {} @ ${} (fee: ${} - {})",
            order.side,
            order.quantity,
            order.symbol,
            avg_price,
            fees,
            fee_type
        );

        ExecutionResult::success(
            order_id,
            order.symbol.clone(),
            order.quantity,
            avg_price,
            fees,
            fee_detail,
        )
    }
}

impl Default for SimulatedOrderSender {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl OrderSender for SimulatedOrderSender {
    async fn send_orders(&mut self, orders: Vec<AssetOrder>) -> Result<Vec<ExecutionResult>> {
        if !self.active {
            return Err(eyre::eyre!("Simulated order sender not started"));
        }

        tracing::info!("📤 Simulating {} order(s)", orders.len());

        let mut results = Vec::new();

        for order in orders {
            tracing::info!(
                "  → {} {} {} (type: {:?})",
                order.side,
                order.quantity,
                order.symbol,
                order.order_type
            );

            // Simulate execution
            let result = self.simulate_fill(&order);

            // Update mock balances if successful
            if result.is_success() {
                let balance = self.balances.entry(order.symbol.clone()).or_insert(Amount::ZERO);

                match order.side {
                    super::types::OrderSide::Buy => {
                        *balance = balance
                            .checked_add(result.filled_quantity)
                            .unwrap_or(*balance);
                    }
                    super::types::OrderSide::Sell => {
                        *balance = balance
                            .checked_sub(result.filled_quantity)
                            .unwrap_or(*balance);
                    }
                }

                tracing::info!(
                    "  ✓ Filled: {} (new balance: {})",
                    result.order_id,
                    balance
                );
            } else {
                tracing::warn!(
                    "  ✗ Failed: {}",
                    result.error_message.as_ref().unwrap_or(&"Unknown error".to_string())
                );
            }

            results.push(result);
        }

        Ok(results)
    }

    async fn get_balances(&self) -> Result<HashMap<String, Amount>> {
        if !self.active {
            return Err(eyre::eyre!("Simulated order sender not started"));
        }

        Ok(self.balances.clone())
    }

    async fn start(&mut self) -> Result<()> {
        if self.active {
            return Err(eyre::eyre!("Simulated order sender already started"));
        }

        self.active = true;
        tracing::info!("🎮 Simulated order sender started (mock mode)");
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        if !self.active {
            return Err(eyre::eyre!("Simulated order sender not active"));
        }

        self.active = false;
        tracing::info!("🎮 Simulated order sender stopped");
        Ok(())
    }

    fn is_active(&self) -> bool {
        self.active
    }
}