use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy_primitives::Address;
use alloy_sol_types::SolCall;
use common::amount::Amount;
use common::interfaces::steward::ISteward;
use common::labels::Labels;
use eyre::Result;
use std::collections::HashSet;

/// Minimal on-chain reader for Vendor
/// Only reads market data, never writes
pub struct OnchainReader<P> {
    provider: P,
    castle_address: Address,
    vendor_id: u128,
}

impl<P> OnchainReader<P>
where
    P: Provider + Clone,
{
    pub fn new(provider: P, castle_address: Address, vendor_id: u128) -> Self {
        Self {
            provider,
            castle_address,
            vendor_id,
        }
    }
    
    /// Read market data from on-chain
    /// 
    /// Returns (liquidity, prices, slopes) as vectors
    /// The order matches the order of assets submitted via submitAssets()
    /// 
    /// NOTE: Asset IDs are NOT returned - the order is implicit!
    /// Vendor must track which assets it submitted and in what order.
    pub async fn get_market_data(&self) -> Result<(Vec<Amount>, Vec<Amount>, Vec<Amount>)> {
        tracing::debug!("Reading market data from on-chain for vendor {}", self.vendor_id);
        
        // Encode the call
        let call = ISteward::getMarketDataCall {
            vendor_id: self.vendor_id,
        };
        
        // Create transaction request
        let tx = TransactionRequest::default()
            .to(self.castle_address)
            .input(call.abi_encode().into());
        
        // Make the call (read-only, no transaction sent)
        let result_bytes = self.provider.call(tx).await?;
        
        // Decode the result - returns Vec<Bytes> (bytes[] memory)
        let decoded = ISteward::getMarketDataCall::abi_decode_returns(&result_bytes)?;

        // Parse the three vectors: (liquidity, prices, slopes)
        // decoded is directly Vec<Bytes>
        let liquidity = if decoded.len() > 0 { parse_vector(&decoded[0])? } else { vec![] };
        let prices = if decoded.len() > 1 { parse_vector(&decoded[1])? } else { vec![] };
        let slopes = if decoded.len() > 2 { parse_vector(&decoded[2])? } else { vec![] };
        
        tracing::debug!(
            "Read market data from on-chain: {} assets (L={:?}, P={:?}, S={:?})",
            liquidity.len(),
            liquidity,
            prices,
            slopes
        );
        
        Ok((liquidity, prices, slopes))
    }
}

fn parse_vector(data: &[u8]) -> Result<Vec<Amount>> {
    use common::vector::Vector;
    let vector = Vector::from_vec(data.to_vec());
    Ok(vector.data)
}

impl<P> OnchainReader<P>
where
    P: Provider + Clone,
{
    /// Get the list of assets already submitted for this vendor
    ///
    /// Uses ISteward::getVendorAssets to retrieve the current on-chain state.
    /// Returns a HashSet for O(1) lookup.
    pub async fn get_submitted_assets(&self) -> Result<HashSet<u128>> {
        tracing::debug!("Reading submitted assets for vendor {}", self.vendor_id);

        let call = ISteward::getVendorAssetsCall {
            vendor_id: self.vendor_id,
        };

        let tx = TransactionRequest::default()
            .to(self.castle_address)
            .input(call.abi_encode().into());

        let result_bytes = self.provider.call(tx).await?;

        // getVendorAssets returns bytes memory (Labels)
        let decoded = ISteward::getVendorAssetsCall::abi_decode_returns(&result_bytes)?;

        // Parse as Labels (asset IDs)
        let labels = Labels::from_vec(decoded.to_vec());
        let asset_ids: HashSet<u128> = labels.data.into_iter().collect();

        tracing::info!(
            "📋 Vendor {} has {} assets already submitted on-chain",
            self.vendor_id,
            asset_ids.len()
        );

        Ok(asset_ids)
    }

    /// Check which assets need to be submitted (not already on-chain)
    ///
    /// Returns (assets_to_submit, already_submitted_count)
    pub async fn filter_new_assets(&self, all_asset_ids: &[u128]) -> Result<(Vec<u128>, usize)> {
        let submitted = self.get_submitted_assets().await?;
        let already_count = submitted.len();

        let new_assets: Vec<u128> = all_asset_ids
            .iter()
            .filter(|id| !submitted.contains(id))
            .copied()
            .collect();

        if new_assets.is_empty() {
            tracing::info!(
                "✓ All {} assets already submitted - skipping submitAssets call",
                all_asset_ids.len()
            );
        } else {
            tracing::info!(
                "📤 {} new assets to submit ({} already on-chain)",
                new_assets.len(),
                already_count
            );
        }

        Ok((new_assets, already_count))
    }
}