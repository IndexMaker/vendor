use common::amount::Amount;
use std::collections::HashMap;

/// Market data for a single asset
#[derive(Debug, Clone)]
pub struct AssetMarketData {
    pub asset_id: u128,
    pub liquidity: Amount,
    pub price: Amount,
    pub slope: Amount,
}

/// Complete market data from vendor
#[derive(Debug, Clone)]
pub struct MarketDataSnapshot {
    pub assets: HashMap<u128, AssetMarketData>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl MarketDataSnapshot {
    pub fn new() -> Self {
        Self {
            assets: HashMap::new(),
            timestamp: chrono::Utc::now(),
        }
    }

    pub fn add_asset(&mut self, data: AssetMarketData) {
        self.assets.insert(data.asset_id, data);
    }

    pub fn get_asset(&self, asset_id: u128) -> Option<&AssetMarketData> {
        self.assets.get(&asset_id)
    }

    pub fn has_all_assets(&self, asset_ids: &[u128]) -> bool {
        asset_ids.iter().all(|id| self.assets.contains_key(id))
    }
}

/// Asset quantity needed for an index
#[derive(Debug, Clone)]
pub struct AssetAllocation {
    pub asset_id: u128,
    pub quantity: Amount,
    pub target_value_usd: Amount,
}

/// Buy order for a single index
#[derive(Debug, Clone)]
pub struct IndexBuyOrder {
    pub index_id: u128,
    pub collateral_added: Amount,
    pub collateral_removed: Amount,
    pub max_order_size: Amount,
    pub asset_allocations: Vec<AssetAllocation>,
}

impl IndexBuyOrder {
    pub fn net_collateral_change(&self) -> Amount {
        self.collateral_added
            .checked_sub(self.collateral_removed)
            .unwrap_or(Amount::ZERO)
    }
}

/// Complete submission payload for on-chain
#[derive(Debug, Clone)]
pub struct SubmissionPayload {
    pub market_data: MarketDataSnapshot,
    pub buy_orders: Vec<IndexBuyOrder>,
    pub vendor_id: u128,
}

impl SubmissionPayload {
    pub fn new(vendor_id: u128, market_data: MarketDataSnapshot) -> Self {
        Self {
            market_data,
            buy_orders: Vec::new(),
            vendor_id,
        }
    }

    pub fn add_buy_order(&mut self, order: IndexBuyOrder) {
        self.buy_orders.push(order);
    }

    pub fn is_empty(&self) -> bool {
        self.buy_orders.is_empty()
    }
}