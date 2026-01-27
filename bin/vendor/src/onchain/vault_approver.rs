//! Vault Auto-Approver
//!
//! Listens for IndexCreated events from Guildmaster and automatically
//! approves new vaults to draw USDC from the custody buffer.
//!
//! Flow:
//! 1. Guildmaster emits IndexCreated(index_id, name, symbol, vault)
//! 2. VaultApprover detects the event
//! 3. Calls USDC.approve(vault, MAX_U256) from custody buffer wallet

use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::{Filter, TransactionRequest};
use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;

// Define the IndexCreated event signature
sol! {
    event IndexCreated(uint128 index_id, string name, string symbol, address vault);

    // ERC20 approve function
    interface IERC20 {
        function approve(address spender, uint256 amount) external returns (bool);
    }
}

/// Configuration for the vault auto-approver
#[derive(Debug, Clone)]
pub struct VaultApproverConfig {
    /// RPC URL for the chain
    pub rpc_url: String,
    /// Guildmaster/Castle address to watch for IndexCreated events
    pub guildmaster_address: Address,
    /// USDC token address
    pub usdc_address: Address,
    /// Custody buffer private key (for signing approve transactions)
    pub custody_buffer_private_key: String,
    /// Polling interval in milliseconds
    pub polling_interval_ms: u64,
}

impl Default for VaultApproverConfig {
    fn default() -> Self {
        Self {
            rpc_url: "http://localhost:8547".to_string(),
            guildmaster_address: Address::ZERO,
            usdc_address: Address::ZERO,
            custody_buffer_private_key: String::new(),
            polling_interval_ms: 5000,
        }
    }
}

impl VaultApproverConfig {
    /// Load from environment variables
    pub fn from_env() -> Option<Self> {
        let rpc_url = std::env::var("ORBIT_RPC_URL")
            .or_else(|_| std::env::var("TESTNET_RPC"))
            .ok()?;

        let guildmaster_address: Address = std::env::var("CASTLE_ADDRESS")
            .ok()?
            .parse()
            .ok()?;

        let usdc_address: Address = std::env::var("WUSDC_ADDRESS")
            .or_else(|_| std::env::var("TEST_COLLATERAL_ADDRESS"))
            .ok()?
            .parse()
            .ok()?;

        let custody_buffer_private_key = std::env::var("CUSTODY_BUFFER_PRIVATE_KEY").ok()?;

        Some(Self {
            rpc_url,
            guildmaster_address,
            usdc_address,
            custody_buffer_private_key,
            polling_interval_ms: std::env::var("VAULT_APPROVER_POLL_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5000),
        })
    }

    /// Check if config is valid (all required fields set)
    pub fn is_valid(&self) -> bool {
        !self.rpc_url.is_empty()
            && !self.guildmaster_address.is_zero()
            && !self.usdc_address.is_zero()
            && !self.custody_buffer_private_key.is_empty()
    }
}

/// Event emitted when a vault is auto-approved
#[derive(Debug, Clone)]
pub struct VaultApproved {
    pub vault_address: Address,
    pub index_id: u128,
    pub tx_hash: B256,
}

/// Vault Auto-Approver service
pub struct VaultApprover {
    config: VaultApproverConfig,
    cancel_token: CancellationToken,
    last_processed_block: tokio::sync::RwLock<u64>,
    approved_vaults: tokio::sync::RwLock<std::collections::HashSet<Address>>,
}

impl VaultApprover {
    pub fn new(config: VaultApproverConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            cancel_token: CancellationToken::new(),
            last_processed_block: tokio::sync::RwLock::new(0),
            approved_vaults: tokio::sync::RwLock::new(std::collections::HashSet::new()),
        })
    }

    /// Get the cancellation token for graceful shutdown
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Start the vault approver service
    pub async fn start(
        self: Arc<Self>,
        event_tx: Option<mpsc::UnboundedSender<VaultApproved>>,
    ) -> eyre::Result<()> {
        if !self.config.is_valid() {
            tracing::warn!("VaultApprover config invalid - service disabled");
            return Ok(());
        }

        tracing::info!("🔐 Starting VaultApprover service");
        tracing::info!("  Guildmaster: {}", self.config.guildmaster_address);
        tracing::info!("  USDC: {}", self.config.usdc_address);

        // Create read-only provider for polling
        let read_provider = ProviderBuilder::new()
            .connect_http(self.config.rpc_url.parse()?);

        // Get current block
        let current_block = read_provider.get_block_number().await?;
        *self.last_processed_block.write().await = current_block;

        tracing::info!("  Starting from block: {}", current_block);

        // Create write provider for transactions (with wallet)
        let signer: alloy::signers::local::PrivateKeySigner =
            self.config.custody_buffer_private_key.parse()?;
        let custody_address = signer.address();
        tracing::info!("  Custody buffer address: {}", custody_address);

        let wallet = alloy::network::EthereumWallet::from(signer);
        let write_provider = ProviderBuilder::new()
            .with_gas_estimation()
            .wallet(wallet)
            .connect_http(self.config.rpc_url.parse()?);

        // Run polling loop
        let mut poll_interval = interval(Duration::from_millis(self.config.polling_interval_ms));

        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    tracing::info!("🛑 VaultApprover shutdown signal received");
                    break;
                }
                _ = poll_interval.tick() => {
                    if let Err(e) = self.poll_and_approve(&read_provider, &write_provider, &event_tx).await {
                        tracing::error!("VaultApprover poll error: {:?}", e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Poll for IndexCreated events and approve new vaults
    async fn poll_and_approve<RP, WP>(
        &self,
        read_provider: &RP,
        write_provider: &WP,
        event_tx: &Option<mpsc::UnboundedSender<VaultApproved>>,
    ) -> eyre::Result<()>
    where
        RP: Provider,
        WP: Provider + Clone + Send + Sync,
    {
        let from_block = *self.last_processed_block.read().await;
        let to_block = read_provider.get_block_number().await?;

        if to_block <= from_block {
            return Ok(()); // No new blocks
        }

        tracing::trace!(
            "VaultApprover polling blocks {} to {}",
            from_block + 1,
            to_block
        );

        // Build filter for IndexCreated events
        let filter = Filter::new()
            .address(self.config.guildmaster_address)
            .event_signature(IndexCreated::SIGNATURE_HASH)
            .from_block(from_block + 1)
            .to_block(to_block);

        let logs = read_provider.get_logs(&filter).await?;

        for log in logs {
            // Parse the IndexCreated event
            if let Ok(decoded) = IndexCreated::decode_log(&log.inner) {
                let vault_address = decoded.vault;
                let index_id = decoded.index_id;

                // Check if already approved
                {
                    let approved = self.approved_vaults.read().await;
                    if approved.contains(&vault_address) {
                        tracing::debug!("Vault {} already approved, skipping", vault_address);
                        continue;
                    }
                }

                tracing::info!(
                    "🆕 New vault detected: {} (index_id: {}, name: {})",
                    vault_address,
                    index_id,
                    decoded.name
                );

                // Approve the vault
                match self.approve_vault(write_provider, vault_address).await {
                    Ok(tx_hash) => {
                        tracing::info!(
                            "✅ Auto-approved vault {} (tx: {:?})",
                            vault_address,
                            tx_hash
                        );

                        // Track as approved
                        self.approved_vaults.write().await.insert(vault_address);

                        // Emit event
                        if let Some(tx) = event_tx {
                            let _ = tx.send(VaultApproved {
                                vault_address,
                                index_id,
                                tx_hash,
                            });
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            "❌ Failed to approve vault {}: {:?}",
                            vault_address,
                            e
                        );
                    }
                }
            }
        }

        // Update last processed block
        *self.last_processed_block.write().await = to_block;

        Ok(())
    }

    /// Approve a vault to draw from custody buffer
    async fn approve_vault<P>(&self, provider: &P, vault_address: Address) -> eyre::Result<B256>
    where
        P: Provider + Clone + Send + Sync,
    {
        // Build approve call with max uint256
        let call = IERC20::approveCall {
            spender: vault_address,
            amount: U256::MAX,
        };

        let tx = TransactionRequest::default()
            .to(self.config.usdc_address)
            .input(call.abi_encode().into());

        let pending = provider.send_transaction(tx).await?;
        let receipt = pending.get_receipt().await?;

        Ok(receipt.transaction_hash)
    }

    /// Get count of approved vaults
    pub async fn approved_count(&self) -> usize {
        self.approved_vaults.read().await.len()
    }

    /// Check if a vault is approved
    pub async fn is_approved(&self, vault: &Address) -> bool {
        self.approved_vaults.read().await.contains(vault)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = VaultApproverConfig::default();
        assert!(!config.is_valid());
    }

    #[test]
    fn test_config_valid() {
        let config = VaultApproverConfig {
            rpc_url: "http://localhost:8547".to_string(),
            guildmaster_address: Address::repeat_byte(1),
            usdc_address: Address::repeat_byte(2),
            custody_buffer_private_key: "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef".to_string(),
            polling_interval_ms: 5000,
        };
        assert!(config.is_valid());
    }

    #[test]
    fn test_config_invalid_no_key() {
        let config = VaultApproverConfig {
            rpc_url: "http://localhost:8547".to_string(),
            guildmaster_address: Address::repeat_byte(1),
            usdc_address: Address::repeat_byte(2),
            custody_buffer_private_key: String::new(), // Missing
            polling_interval_ms: 5000,
        };
        assert!(!config.is_valid());
    }
}
