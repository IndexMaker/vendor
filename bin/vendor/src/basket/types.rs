use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexAsset {
    pub id: u32,
    pub pair: String,
    pub listing: String,
    pub assetname: String,
    pub sector: String,
    pub market_cap: u64,
    pub weights: String,
    pub quantity: f64,
}

#[derive(Debug, Clone)]
pub struct Index {
    pub symbol: String,
    pub assets: Vec<IndexAsset>,
}

impl Index {
    pub fn new(symbol: String, assets: Vec<IndexAsset>) -> Self {
        Self { symbol, assets }
    }

    pub fn get_asset_symbols(&self) -> Vec<String> {
        self.assets.iter().map(|a| a.pair.clone()).collect()
    }
}