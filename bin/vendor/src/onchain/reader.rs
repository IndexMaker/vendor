use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy_primitives::Address;
use alloy_sol_types::SolCall;
use common::amount::Amount;
use common::interfaces::factor::IFactor;
use eyre::Result;

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
        let call = IFactor::getMarketDataCall {
            vendor_id: self.vendor_id,
        };
        
        // Create transaction request
        let tx = TransactionRequest::default()
            .to(self.castle_address)
            .input(call.abi_encode().into());
        
        // Make the call (read-only, no transaction sent)
        let result_bytes = self.provider.call(tx).await?;
        
        // Decode the result
        let decoded = IFactor::getMarketDataCall::abi_decode_returns(&result_bytes)?;
        
        // Parse the three vectors: (liquidity, prices, slopes)
        let liquidity = parse_vector(&decoded._0)?;
        let prices = parse_vector(&decoded._1)?;
        let slopes = parse_vector(&decoded._2)?;
        
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