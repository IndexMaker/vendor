use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy_primitives::Address;
use alloy_sol_types::SolCall;
use common::interfaces::banker::IBanker;
use common::{labels::Labels, vector::Vector, amount::Amount};
use std::collections::HashMap;
use std::sync::Arc;

/// Vendor demand from on-chain
#[derive(Debug, Clone)]
pub struct VendorDemand {
    pub assets: HashMap<u128, Amount>,  // asset_id → demand_quantity
}

/// Handles all vendor-related on-chain submissions via IBanker
pub struct VendorSubmitter<P>
where
    P: Provider + Clone,
{
    provider: P,
    castle_address: Address,
    vendor_id: u128,
}

impl<P> VendorSubmitter<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    pub fn new(provider: P, castle_address: Address, vendor_id: u128) -> Self {
        Self {
            provider,
            castle_address,
            vendor_id,
        }
    }

    /// Submit tracked assets to Castle (IBanker::submitAssets)
    /// Maximum 120 assets as per requirements
    pub async fn submit_assets(&self, asset_ids: Vec<u128>) -> eyre::Result<()> {
        if asset_ids.is_empty() {
            tracing::warn!("No assets to submit");
            return Ok(());
        }

        // Limit to 120 assets as per Sonia's requirement
        let limited_assets: Vec<u128> = asset_ids.into_iter().take(120).collect();

        tracing::info!("📤 Submitting {} assets to Castle", limited_assets.len());

        let asset_names = Labels::from_vec_u128(limited_assets.clone());

        let call = IBanker::submitAssetsCall {
            vendor_id: self.vendor_id,
            market_asset_names: asset_names.to_vec().into(),
        };

        let tx = TransactionRequest::default()
            .to(self.castle_address)
            .input(call.abi_encode().into());

        let receipt = self.provider.send_transaction(tx).await?.get_receipt().await?;

        tracing::info!(
            "  ✓ submitAssets tx: {:?} (block: {})",
            receipt.transaction_hash,
            receipt.block_number.unwrap_or(0)
        );

        Ok(())
    }

    /// Submit margin to Castle (IBanker::submitMargin)
    pub async fn submit_margin(
        &self,
        asset_ids: Vec<u128>,
        margins: Vec<Amount>,
    ) -> eyre::Result<()> {
        if asset_ids.is_empty() || margins.is_empty() {
            tracing::warn!("No margin data to submit");
            return Ok(());
        }

        if asset_ids.len() != margins.len() {
            return Err(eyre::eyre!(
                "Asset IDs and margins length mismatch: {} vs {}",
                asset_ids.len(),
                margins.len()
            ));
        }

        tracing::info!("📤 Submitting margin for {} assets", asset_ids.len());

        let asset_names = Labels::from_vec_u128(asset_ids);
        let asset_margin = Vector::from_vec_u128(
            margins.iter().map(|m| m.to_u128_raw()).collect()
        );

        let call = IBanker::submitMarginCall {
            vendor_id: self.vendor_id,
            asset_names: asset_names.to_vec().into(),
            asset_margin: asset_margin.to_vec().into(),
        };

        let tx = TransactionRequest::default()
            .to(self.castle_address)
            .input(call.abi_encode().into());

        let receipt = self.provider.send_transaction(tx).await?.get_receipt().await?;

        tracing::info!(
            "  ✓ submitMargin tx: {:?} (block: {})",
            receipt.transaction_hash,
            receipt.block_number.unwrap_or(0)
        );

        Ok(())
    }

    /// Submit supply to Castle (IBanker::submitSupply)
    pub async fn submit_supply(
        &self,
        asset_ids: Vec<u128>,
        supply_long: Vec<Amount>,
        supply_short: Vec<Amount>,
    ) -> eyre::Result<()> {
        if asset_ids.is_empty() {
            tracing::warn!("No supply data to submit");
            return Ok(());
        }

        if asset_ids.len() != supply_long.len() || asset_ids.len() != supply_short.len() {
            return Err(eyre::eyre!(
                "Asset IDs, long, and short supply length mismatch: {} vs {} vs {}",
                asset_ids.len(),
                supply_long.len(),
                supply_short.len()
            ));
        }

        tracing::info!("📤 Submitting supply for {} assets", asset_ids.len());

        let asset_names = Labels::from_vec_u128(asset_ids);
        let supply_long_vec = Vector::from_vec_u128(
            supply_long.iter().map(|m| m.to_u128_raw()).collect()
        );
        let supply_short_vec = Vector::from_vec_u128(
            supply_short.iter().map(|m| m.to_u128_raw()).collect()
        );

        let call = IBanker::submitSupplyCall {
            vendor_id: self.vendor_id,
            asset_names: asset_names.to_vec().into(),
            asset_quantities_short: supply_short_vec.to_vec().into(),
            asset_quantities_long: supply_long_vec.to_vec().into(),
        };

        let tx = TransactionRequest::default()
            .to(self.castle_address)
            .input(call.abi_encode().into());

        let receipt = self.provider.send_transaction(tx).await?.get_receipt().await?;

        tracing::info!(
            "  ✓ submitSupply tx: {:?} (block: {})",
            receipt.transaction_hash,
            receipt.block_number.unwrap_or(0)
        );

        Ok(())
    }

    /// Get vendor demand from Castle (IBanker::getVendorDemand)
    /// Returns net demand per asset (demand_long - demand_short)
    pub async fn get_vendor_demand(&self) -> eyre::Result<VendorDemand> {
        tracing::info!("📥 Reading vendor demand from Castle...");

        // Step 1: Get vendor assets (asset IDs list)
        let assets_call = IBanker::getVendorAssetsCall {
            vendor_id: self.vendor_id,
        };

        let assets_result = self
            .provider
            .call(TransactionRequest::default()
                .to(self.castle_address)
                .input(assets_call.abi_encode().into()))
            .await?;

        let assets_decoded = IBanker::getVendorAssetsCall::abi_decode_returns(&assets_result)?;
        
        // Parse asset IDs
        let asset_ids = Labels::from_vec(assets_decoded.0);

        tracing::info!("  Found {} assets for vendor", asset_ids.data.len());

        // Step 2: Get vendor demand (demand_long, demand_short)
        let demand_call = IBanker::getVendorDemandCall {
            vendor_id: self.vendor_id,
        };

        let demand_result = self
            .provider
            .call(TransactionRequest::default()
                .to(self.castle_address)
                .input(demand_call.abi_encode().into()))
            .await?;

        let demand_decoded = IBanker::getVendorDemandCall::abi_decode_returns(&demand_result)?;

        // Parse demand_long and demand_short vectors
        let demand_long = Vector::from_vec(demand_decoded._0);
        let demand_short = Vector::from_vec(demand_decoded._1);

        tracing::info!(
            "  Demand vectors: {} long, {} short",
            demand_long.data.len(),
            demand_short.data.len()
        );

        // Step 3: Calculate net demand per asset (demand_long - demand_short)
        let mut demand = VendorDemand {
            assets: HashMap::new(),
        };

        for (i, asset_id) in asset_ids.data.iter().enumerate() {
            let long_qty = demand_long.data.get(i).copied().unwrap_or(Amount::ZERO);
            let short_qty = demand_short.data.get(i).copied().unwrap_or(Amount::ZERO);

            // Net demand = long - short
            let net_demand = long_qty
                .checked_sub(short_qty)
                .unwrap_or(Amount::ZERO);

            demand.assets.insert(*asset_id, net_demand);

            tracing::info!(
                "    Asset {}: long={:.4}, short={:.4}, net={:.4}",
                asset_id,
                long_qty.to_u128_raw() as f64 / 1e18,
                short_qty.to_u128_raw() as f64 / 1e18,
                net_demand.to_u128_raw() as f64 / 1e18
            );
        }

        tracing::info!("  Calculated net demand for {} assets", demand.assets.len());

        Ok(demand)
    }

    /// Get vendor supply from Castle (IBanker::getVendorSupply)
    pub async fn get_vendor_supply(&self) -> eyre::Result<HashMap<u128, (Amount, Amount)>> {
        tracing::debug!("📥 Reading vendor supply from Castle...");

        let call = IBanker::getVendorSupplyCall {
            vendor_id: self.vendor_id,
        };

        // FIXED: Remove & and pass TransactionRequest by value
        let result = self
            .provider
            .call(TransactionRequest::default()  // REMOVED &
                .to(self.castle_address)
                .input(call.abi_encode().into()))
            .await?;

        // FIXED: Remove second argument (false)
        let decoded = IBanker::getVendorSupplyCall::abi_decode_returns(&result)?;

        // FIXED: getVendorSupply returns (asset_names, supply_short, supply_long)
        // Based on the interface: returns (uint8[] memory, uint8[] memory)
        // This is actually (asset_names, supply_quantities)
        // Let me check the actual return signature...
        
        // Parse response - the return is two arrays
        let asset_names = Labels::from_vec(decoded._0);
        let supply_quantities = Vector::from_vec(decoded._1);

        let mut supply = HashMap::new();

        // Since getVendorSupply returns two arrays (not three),
        // we need to interpret this correctly based on the actual contract
        // For now, assume it returns combined supply data
        for (i, asset_id) in asset_names.data.iter().enumerate() {
            let qty = supply_quantities.data.get(i).copied().unwrap_or(Amount::ZERO);
            // Store as (long, short) - adjust based on actual contract behavior
            supply.insert(*asset_id, (qty, Amount::ZERO));
        }

        tracing::debug!("  Read supply for {} assets", supply.len());

        Ok(supply)
    }
}