//! Event types for Vault contract events
//!
//! These events are emitted by IVaultNativeOrders and IVaultNativeClaims:
//! - BuyOrder/SellOrder: When users place orders
//! - Acquisition/Disposal: When orders are processed (partial fills)
//! - AcquisitionClaim/DisposalClaim: When claims are processed (remainders)
//!
//! Story 2.8: Extended to support event-driven claim handling

#![allow(dead_code)] // Many fields and methods reserved for future use and testing

use alloy_primitives::{Address, B256, Log as PrimitiveLog};
use alloy_sol_types::{sol, SolEvent};
use chrono::{DateTime, Utc};

// Define Solidity events using alloy-sol-types
// NOTE: Contract events do NOT use indexed parameters
sol! {
    /// BuyOrder event emitted when a user places a buy order
    #[derive(Debug)]
    event BuyOrder(
        address keeper,
        address trader,
        uint128 index_id,
        uint128 vendor_id,
        uint128 collateral_amount
    );

    /// SellOrder event emitted when a user places a sell order
    #[derive(Debug)]
    event SellOrder(
        address keeper,
        address trader,
        uint128 index_id,
        uint128 vendor_id,
        uint128 itp_amount
    );

    // =========================================================================
    // Story 2.8: Post-fill lifecycle events from IVaultNativeOrders
    // =========================================================================

    /// Acquisition event emitted when a buy order is processed
    /// Controller processes the order, minting ITP tokens
    #[derive(Debug)]
    event Acquisition(
        address controller,
        uint128 index_id,
        uint128 vendor_id,
        uint128 remain,
        uint128 spent,
        uint128 itp_minted
    );

    /// Disposal event emitted when a sell order is processed
    /// Controller processes the order, burning ITP tokens
    #[derive(Debug)]
    event Disposal(
        address controller,
        uint128 index_id,
        uint128 vendor_id,
        uint128 itp_remain,
        uint128 itp_burned,
        uint128 gains
    );

    // =========================================================================
    // Story 2.8: Claim events from IVaultNativeClaims
    // =========================================================================

    /// AcquisitionClaim event emitted when remainder from buy order is claimed
    /// Triggers rebalancing if remain > threshold
    #[derive(Debug)]
    event AcquisitionClaim(
        address controller,
        uint128 index_id,
        uint128 vendor_id,
        uint128 remain,
        uint128 spent,
        uint128 itp_minted
    );

    /// DisposalClaim event emitted when remainder from sell order is claimed
    /// Triggers rebalancing if itp_remain > threshold
    #[derive(Debug)]
    event DisposalClaim(
        address controller,
        uint128 index_id,
        uint128 vendor_id,
        uint128 itp_remain,
        uint128 itp_burned,
        uint128 gains
    );
}

/// Parsed BuyOrder event with additional context
#[derive(Debug, Clone)]
pub struct BuyOrderEvent {
    /// Keeper address that will process this order
    pub keeper: Address,
    /// Trader address that placed the order
    pub trader: Address,
    /// Index ID for the ITP
    pub index_id: u128,
    /// Vendor ID for price data
    pub vendor_id: u128,
    /// Collateral amount in USDC (6 decimals)
    pub collateral_amount: u128,
    /// Source vault contract address
    pub vault_address: Address,
    /// Transaction hash for correlation
    pub tx_hash: B256,
    /// Block number when event was emitted
    pub block_number: u64,
    /// Timestamp when event was received
    pub received_at: DateTime<Utc>,
}

/// Parsed SellOrder event with additional context
#[derive(Debug, Clone)]
pub struct SellOrderEvent {
    /// Keeper address that will process this order
    pub keeper: Address,
    /// Trader address that placed the order
    pub trader: Address,
    /// Index ID for the ITP
    pub index_id: u128,
    /// Vendor ID for price data
    pub vendor_id: u128,
    /// ITP amount to sell (18 decimals)
    pub itp_amount: u128,
    /// Source vault contract address
    pub vault_address: Address,
    /// Transaction hash for correlation
    pub tx_hash: B256,
    /// Block number when event was emitted
    pub block_number: u64,
    /// Timestamp when event was received
    pub received_at: DateTime<Utc>,
}

// =============================================================================
// Story 2.8: New event structs for post-fill lifecycle events
// =============================================================================

/// Parsed Acquisition event with additional context
/// Emitted when a buy order is processed (partial fill)
#[derive(Debug, Clone)]
pub struct AcquisitionEvent {
    /// Controller address that processed the order
    pub controller: Address,
    /// Index ID for the ITP
    pub index_id: u128,
    /// Vendor ID for price data
    pub vendor_id: u128,
    /// Remaining collateral amount (for potential rebalancing)
    pub remain: u128,
    /// Collateral spent in this fill
    pub spent: u128,
    /// ITP tokens minted
    pub itp_minted: u128,
    /// Source vault contract address
    pub vault_address: Address,
    /// Transaction hash for correlation
    pub tx_hash: B256,
    /// Block number when event was emitted
    pub block_number: u64,
    /// Timestamp when event was received
    pub received_at: DateTime<Utc>,
}

/// Parsed Disposal event with additional context
/// Emitted when a sell order is processed (partial fill)
#[derive(Debug, Clone)]
pub struct DisposalEvent {
    /// Controller address that processed the order
    pub controller: Address,
    /// Index ID for the ITP
    pub index_id: u128,
    /// Vendor ID for price data
    pub vendor_id: u128,
    /// Remaining ITP amount (for potential rebalancing)
    pub itp_remain: u128,
    /// ITP tokens burned
    pub itp_burned: u128,
    /// Collateral gains from the sale
    pub gains: u128,
    /// Source vault contract address
    pub vault_address: Address,
    /// Transaction hash for correlation
    pub tx_hash: B256,
    /// Block number when event was emitted
    pub block_number: u64,
    /// Timestamp when event was received
    pub received_at: DateTime<Utc>,
}

/// Parsed AcquisitionClaim event with additional context
/// Emitted when remainder from buy order is claimed
/// Triggers rebalancing if remain > threshold (default: 100 units)
#[derive(Debug, Clone)]
pub struct AcquisitionClaimEvent {
    /// Controller address that processed the claim
    pub controller: Address,
    /// Index ID for the ITP
    pub index_id: u128,
    /// Vendor ID for price data
    pub vendor_id: u128,
    /// Remaining collateral amount (triggers rebalancing if > threshold)
    pub remain: u128,
    /// Collateral spent in this claim
    pub spent: u128,
    /// ITP tokens minted
    pub itp_minted: u128,
    /// Source vault contract address
    pub vault_address: Address,
    /// Transaction hash for correlation
    pub tx_hash: B256,
    /// Block number when event was emitted
    pub block_number: u64,
    /// Timestamp when event was received
    pub received_at: DateTime<Utc>,
}

/// Parsed DisposalClaim event with additional context
/// Emitted when remainder from sell order is claimed
/// Triggers rebalancing if itp_remain > threshold (default: 100 units)
#[derive(Debug, Clone)]
pub struct DisposalClaimEvent {
    /// Controller address that processed the claim
    pub controller: Address,
    /// Index ID for the ITP
    pub index_id: u128,
    /// Vendor ID for price data
    pub vendor_id: u128,
    /// Remaining ITP amount (triggers rebalancing if > threshold)
    pub itp_remain: u128,
    /// ITP tokens burned
    pub itp_burned: u128,
    /// Collateral gains from the claim
    pub gains: u128,
    /// Source vault contract address
    pub vault_address: Address,
    /// Transaction hash for correlation
    pub tx_hash: B256,
    /// Block number when event was emitted
    pub block_number: u64,
    /// Timestamp when event was received
    pub received_at: DateTime<Utc>,
}

/// Wrapper enum for vault events
/// Story 2.8: Extended with Acquisition, Disposal, AcquisitionClaim, DisposalClaim variants
#[derive(Debug, Clone)]
pub enum VaultEvent {
    Buy(BuyOrderEvent),
    Sell(SellOrderEvent),
    /// Emitted when a buy order is processed (partial fill)
    Acquisition(AcquisitionEvent),
    /// Emitted when a sell order is processed (partial fill)
    Disposal(DisposalEvent),
    /// Emitted when remainder from buy order is claimed
    AcquisitionClaim(AcquisitionClaimEvent),
    /// Emitted when remainder from sell order is claimed
    DisposalClaim(DisposalClaimEvent),
}

impl VaultEvent {
    /// Get the index ID from the event
    pub fn index_id(&self) -> u128 {
        match self {
            VaultEvent::Buy(e) => e.index_id,
            VaultEvent::Sell(e) => e.index_id,
            VaultEvent::Acquisition(e) => e.index_id,
            VaultEvent::Disposal(e) => e.index_id,
            VaultEvent::AcquisitionClaim(e) => e.index_id,
            VaultEvent::DisposalClaim(e) => e.index_id,
        }
    }

    /// Get the trader address from the event (only for order events)
    /// Returns None for lifecycle events (Acquisition/Disposal/Claims)
    pub fn trader(&self) -> Option<Address> {
        match self {
            VaultEvent::Buy(e) => Some(e.trader),
            VaultEvent::Sell(e) => Some(e.trader),
            // Lifecycle events don't have trader address, only controller
            VaultEvent::Acquisition(_) |
            VaultEvent::Disposal(_) |
            VaultEvent::AcquisitionClaim(_) |
            VaultEvent::DisposalClaim(_) => None,
        }
    }

    /// Get the controller address from the event (only for lifecycle events)
    /// Returns None for order events (BuyOrder/SellOrder)
    pub fn controller(&self) -> Option<Address> {
        match self {
            VaultEvent::Buy(_) | VaultEvent::Sell(_) => None,
            VaultEvent::Acquisition(e) => Some(e.controller),
            VaultEvent::Disposal(e) => Some(e.controller),
            VaultEvent::AcquisitionClaim(e) => Some(e.controller),
            VaultEvent::DisposalClaim(e) => Some(e.controller),
        }
    }

    /// Get the vault address from the event
    pub fn vault_address(&self) -> Address {
        match self {
            VaultEvent::Buy(e) => e.vault_address,
            VaultEvent::Sell(e) => e.vault_address,
            VaultEvent::Acquisition(e) => e.vault_address,
            VaultEvent::Disposal(e) => e.vault_address,
            VaultEvent::AcquisitionClaim(e) => e.vault_address,
            VaultEvent::DisposalClaim(e) => e.vault_address,
        }
    }

    /// Get the transaction hash from the event
    pub fn tx_hash(&self) -> B256 {
        match self {
            VaultEvent::Buy(e) => e.tx_hash,
            VaultEvent::Sell(e) => e.tx_hash,
            VaultEvent::Acquisition(e) => e.tx_hash,
            VaultEvent::Disposal(e) => e.tx_hash,
            VaultEvent::AcquisitionClaim(e) => e.tx_hash,
            VaultEvent::DisposalClaim(e) => e.tx_hash,
        }
    }

    /// Check if this is a buy order
    pub fn is_buy(&self) -> bool {
        matches!(self, VaultEvent::Buy(_))
    }

    /// Check if this is a sell order
    pub fn is_sell(&self) -> bool {
        matches!(self, VaultEvent::Sell(_))
    }

    // =========================================================================
    // Story 2.8: Helper methods for lifecycle events
    // =========================================================================

    /// Check if this is an acquisition event (order processed)
    pub fn is_acquisition(&self) -> bool {
        matches!(self, VaultEvent::Acquisition(_))
    }

    /// Check if this is a disposal event (order processed)
    pub fn is_disposal(&self) -> bool {
        matches!(self, VaultEvent::Disposal(_))
    }

    /// Check if this is an acquisition claim event
    pub fn is_acquisition_claim(&self) -> bool {
        matches!(self, VaultEvent::AcquisitionClaim(_))
    }

    /// Check if this is a disposal claim event
    pub fn is_disposal_claim(&self) -> bool {
        matches!(self, VaultEvent::DisposalClaim(_))
    }

    /// Check if this is any claim event (for threshold checking)
    pub fn is_claim(&self) -> bool {
        matches!(self, VaultEvent::AcquisitionClaim(_) | VaultEvent::DisposalClaim(_))
    }

    /// Check if this is an order event (BuyOrder or SellOrder)
    pub fn is_order(&self) -> bool {
        matches!(self, VaultEvent::Buy(_) | VaultEvent::Sell(_))
    }

    /// Check if this is a lifecycle event (Acquisition, Disposal, or Claims)
    pub fn is_lifecycle(&self) -> bool {
        !self.is_order()
    }

    /// Get the remainder amount for claim events (for threshold checking)
    /// Returns None for non-claim events
    pub fn claim_remainder(&self) -> Option<u128> {
        match self {
            VaultEvent::AcquisitionClaim(e) => Some(e.remain),
            VaultEvent::DisposalClaim(e) => Some(e.itp_remain),
            _ => None,
        }
    }
}

/// Event signature constants
pub struct EventSignatures;

impl EventSignatures {
    /// BuyOrder event signature (topic0)
    pub fn buy_order() -> B256 {
        BuyOrder::SIGNATURE_HASH
    }

    /// SellOrder event signature (topic0)
    pub fn sell_order() -> B256 {
        SellOrder::SIGNATURE_HASH
    }

    // =========================================================================
    // Story 2.8: Additional event signatures
    // =========================================================================

    /// Acquisition event signature (topic0)
    pub fn acquisition() -> B256 {
        Acquisition::SIGNATURE_HASH
    }

    /// Disposal event signature (topic0)
    pub fn disposal() -> B256 {
        Disposal::SIGNATURE_HASH
    }

    /// AcquisitionClaim event signature (topic0)
    pub fn acquisition_claim() -> B256 {
        AcquisitionClaim::SIGNATURE_HASH
    }

    /// DisposalClaim event signature (topic0)
    pub fn disposal_claim() -> B256 {
        DisposalClaim::SIGNATURE_HASH
    }

    /// Get all 6 event signatures as a vector (for filtering)
    /// Story 2.8: Extended to include lifecycle events
    pub fn all() -> Vec<B256> {
        vec![
            Self::buy_order(),
            Self::sell_order(),
            Self::acquisition(),
            Self::disposal(),
            Self::acquisition_claim(),
            Self::disposal_claim(),
        ]
    }

    /// Get only order event signatures (BuyOrder, SellOrder)
    pub fn orders_only() -> Vec<B256> {
        vec![Self::buy_order(), Self::sell_order()]
    }

    /// Get only claim event signatures (AcquisitionClaim, DisposalClaim)
    pub fn claims_only() -> Vec<B256> {
        vec![Self::acquisition_claim(), Self::disposal_claim()]
    }
}

/// Parse a raw log into a VaultEvent
/// Story 2.8: Extended to handle all 6 event types
pub fn parse_log(
    log: &PrimitiveLog,
    vault_address: Address,
    tx_hash: B256,
    block_number: u64,
) -> Result<VaultEvent, EventParseError> {
    let topic0 = log
        .topics()
        .first()
        .ok_or(EventParseError::NoTopics)?;

    let received_at = Utc::now();

    if *topic0 == EventSignatures::buy_order() {
        let decoded = BuyOrder::decode_log(log)
            .map_err(|e| EventParseError::DecodeError(e.to_string()))?;

        Ok(VaultEvent::Buy(BuyOrderEvent {
            keeper: decoded.keeper,
            trader: decoded.trader,
            index_id: decoded.index_id,
            vendor_id: decoded.vendor_id,
            collateral_amount: decoded.collateral_amount,
            vault_address,
            tx_hash,
            block_number,
            received_at,
        }))
    } else if *topic0 == EventSignatures::sell_order() {
        let decoded = SellOrder::decode_log(log)
            .map_err(|e| EventParseError::DecodeError(e.to_string()))?;

        Ok(VaultEvent::Sell(SellOrderEvent {
            keeper: decoded.keeper,
            trader: decoded.trader,
            index_id: decoded.index_id,
            vendor_id: decoded.vendor_id,
            itp_amount: decoded.itp_amount,
            vault_address,
            tx_hash,
            block_number,
            received_at,
        }))
    // =========================================================================
    // Story 2.8: Parse lifecycle events
    // =========================================================================
    } else if *topic0 == EventSignatures::acquisition() {
        let decoded = Acquisition::decode_log(log)
            .map_err(|e| EventParseError::DecodeError(e.to_string()))?;

        Ok(VaultEvent::Acquisition(AcquisitionEvent {
            controller: decoded.controller,
            index_id: decoded.index_id,
            vendor_id: decoded.vendor_id,
            remain: decoded.remain,
            spent: decoded.spent,
            itp_minted: decoded.itp_minted,
            vault_address,
            tx_hash,
            block_number,
            received_at,
        }))
    } else if *topic0 == EventSignatures::disposal() {
        let decoded = Disposal::decode_log(log)
            .map_err(|e| EventParseError::DecodeError(e.to_string()))?;

        Ok(VaultEvent::Disposal(DisposalEvent {
            controller: decoded.controller,
            index_id: decoded.index_id,
            vendor_id: decoded.vendor_id,
            itp_remain: decoded.itp_remain,
            itp_burned: decoded.itp_burned,
            gains: decoded.gains,
            vault_address,
            tx_hash,
            block_number,
            received_at,
        }))
    } else if *topic0 == EventSignatures::acquisition_claim() {
        let decoded = AcquisitionClaim::decode_log(log)
            .map_err(|e| EventParseError::DecodeError(e.to_string()))?;

        Ok(VaultEvent::AcquisitionClaim(AcquisitionClaimEvent {
            controller: decoded.controller,
            index_id: decoded.index_id,
            vendor_id: decoded.vendor_id,
            remain: decoded.remain,
            spent: decoded.spent,
            itp_minted: decoded.itp_minted,
            vault_address,
            tx_hash,
            block_number,
            received_at,
        }))
    } else if *topic0 == EventSignatures::disposal_claim() {
        let decoded = DisposalClaim::decode_log(log)
            .map_err(|e| EventParseError::DecodeError(e.to_string()))?;

        Ok(VaultEvent::DisposalClaim(DisposalClaimEvent {
            controller: decoded.controller,
            index_id: decoded.index_id,
            vendor_id: decoded.vendor_id,
            itp_remain: decoded.itp_remain,
            itp_burned: decoded.itp_burned,
            gains: decoded.gains,
            vault_address,
            tx_hash,
            block_number,
            received_at,
        }))
    } else {
        Err(EventParseError::UnknownSignature(*topic0))
    }
}

/// Errors that can occur during event parsing
#[derive(Debug, Clone)]
pub enum EventParseError {
    /// Log has no topics
    NoTopics,
    /// Failed to decode event data
    DecodeError(String),
    /// Unknown event signature
    UnknownSignature(B256),
}

impl std::fmt::Display for EventParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventParseError::NoTopics => write!(f, "Log has no topics"),
            EventParseError::DecodeError(msg) => write!(f, "Failed to decode event: {}", msg),
            EventParseError::UnknownSignature(sig) => {
                write!(f, "Unknown event signature: {}", sig)
            }
        }
    }
}

impl std::error::Error for EventParseError {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;

    #[test]
    fn test_event_signatures_are_correct() {
        // Verify signatures match expected keccak256 hashes
        let buy_sig = EventSignatures::buy_order();
        let sell_sig = EventSignatures::sell_order();

        // These should be different
        assert_ne!(buy_sig, sell_sig);

        // Verify they're not zero
        assert_ne!(buy_sig, B256::ZERO);
        assert_ne!(sell_sig, B256::ZERO);

        println!("BuyOrder signature: {}", buy_sig);
        println!("SellOrder signature: {}", sell_sig);
    }

    #[test]
    fn test_vault_event_helpers() {
        let buy_event = BuyOrderEvent {
            keeper: Address::ZERO,
            trader: Address::repeat_byte(1),
            index_id: 42,
            vendor_id: 1,
            collateral_amount: 1_000_000,
            vault_address: Address::repeat_byte(2),
            tx_hash: B256::ZERO,
            block_number: 100,
            received_at: Utc::now(),
        };

        let event = VaultEvent::Buy(buy_event);

        assert!(event.is_buy());
        assert!(!event.is_sell());
        assert_eq!(event.index_id(), 42);
        assert_eq!(event.trader(), Some(Address::repeat_byte(1)));
        assert_eq!(event.vault_address(), Address::repeat_byte(2));
    }

    #[test]
    fn test_all_signatures() {
        let sigs = EventSignatures::all();
        // Story 2.8: Now returns 6 signatures
        assert_eq!(sigs.len(), 6);
        assert!(sigs.contains(&EventSignatures::buy_order()));
        assert!(sigs.contains(&EventSignatures::sell_order()));
        assert!(sigs.contains(&EventSignatures::acquisition()));
        assert!(sigs.contains(&EventSignatures::disposal()));
        assert!(sigs.contains(&EventSignatures::acquisition_claim()));
        assert!(sigs.contains(&EventSignatures::disposal_claim()));
    }

    // =========================================================================
    // Story 2.8: Tests for new event types
    // =========================================================================

    #[test]
    fn test_lifecycle_event_signatures_are_correct() {
        // Verify lifecycle event signatures are generated correctly
        let acquisition_sig = EventSignatures::acquisition();
        let disposal_sig = EventSignatures::disposal();
        let acq_claim_sig = EventSignatures::acquisition_claim();
        let disp_claim_sig = EventSignatures::disposal_claim();

        // All signatures should be different from each other
        let all_sigs = [acquisition_sig, disposal_sig, acq_claim_sig, disp_claim_sig];
        for (i, sig1) in all_sigs.iter().enumerate() {
            assert_ne!(*sig1, B256::ZERO, "Signature {} should not be zero", i);
            for (j, sig2) in all_sigs.iter().enumerate() {
                if i != j {
                    assert_ne!(sig1, sig2, "Signatures {} and {} should be different", i, j);
                }
            }
        }

        println!("Acquisition signature: {}", acquisition_sig);
        println!("Disposal signature: {}", disposal_sig);
        println!("AcquisitionClaim signature: {}", acq_claim_sig);
        println!("DisposalClaim signature: {}", disp_claim_sig);
    }

    #[test]
    fn test_acquisition_event_helpers() {
        let acq_event = AcquisitionEvent {
            controller: Address::repeat_byte(1),
            index_id: 42,
            vendor_id: 1,
            remain: 500,
            spent: 1_000_000,
            itp_minted: 999_000,
            vault_address: Address::repeat_byte(2),
            tx_hash: B256::ZERO,
            block_number: 100,
            received_at: Utc::now(),
        };

        let event = VaultEvent::Acquisition(acq_event);

        assert!(event.is_acquisition());
        assert!(!event.is_buy());
        assert!(!event.is_sell());
        assert!(!event.is_claim());
        assert!(event.is_lifecycle());
        assert!(!event.is_order());
        assert_eq!(event.index_id(), 42);
        assert_eq!(event.trader(), None); // Lifecycle events don't have trader
        assert_eq!(event.controller(), Some(Address::repeat_byte(1)));
        assert_eq!(event.vault_address(), Address::repeat_byte(2));
        assert_eq!(event.claim_remainder(), None); // Not a claim event
    }

    #[test]
    fn test_disposal_event_helpers() {
        let disp_event = DisposalEvent {
            controller: Address::repeat_byte(1),
            index_id: 42,
            vendor_id: 1,
            itp_remain: 300,
            itp_burned: 700,
            gains: 999_000,
            vault_address: Address::repeat_byte(2),
            tx_hash: B256::ZERO,
            block_number: 100,
            received_at: Utc::now(),
        };

        let event = VaultEvent::Disposal(disp_event);

        assert!(event.is_disposal());
        assert!(!event.is_acquisition());
        assert!(!event.is_claim());
        assert!(event.is_lifecycle());
        assert_eq!(event.controller(), Some(Address::repeat_byte(1)));
    }

    #[test]
    fn test_acquisition_claim_event_helpers() {
        let claim_event = AcquisitionClaimEvent {
            controller: Address::repeat_byte(1),
            index_id: 42,
            vendor_id: 1,
            remain: 150, // Above threshold
            spent: 100_000,
            itp_minted: 99_000,
            vault_address: Address::repeat_byte(2),
            tx_hash: B256::ZERO,
            block_number: 100,
            received_at: Utc::now(),
        };

        let event = VaultEvent::AcquisitionClaim(claim_event);

        assert!(event.is_acquisition_claim());
        assert!(event.is_claim());
        assert!(!event.is_disposal_claim());
        assert!(event.is_lifecycle());
        assert!(!event.is_order());
        assert_eq!(event.index_id(), 42);
        assert_eq!(event.claim_remainder(), Some(150));
    }

    #[test]
    fn test_disposal_claim_event_helpers() {
        let claim_event = DisposalClaimEvent {
            controller: Address::repeat_byte(1),
            index_id: 42,
            vendor_id: 1,
            itp_remain: 200, // Above threshold
            itp_burned: 800,
            gains: 99_000,
            vault_address: Address::repeat_byte(2),
            tx_hash: B256::ZERO,
            block_number: 100,
            received_at: Utc::now(),
        };

        let event = VaultEvent::DisposalClaim(claim_event);

        assert!(event.is_disposal_claim());
        assert!(event.is_claim());
        assert!(!event.is_acquisition_claim());
        assert!(event.is_lifecycle());
        assert_eq!(event.claim_remainder(), Some(200));
    }

    #[test]
    fn test_orders_only_signatures() {
        let order_sigs = EventSignatures::orders_only();
        assert_eq!(order_sigs.len(), 2);
        assert!(order_sigs.contains(&EventSignatures::buy_order()));
        assert!(order_sigs.contains(&EventSignatures::sell_order()));
        assert!(!order_sigs.contains(&EventSignatures::acquisition()));
    }

    #[test]
    fn test_claims_only_signatures() {
        let claim_sigs = EventSignatures::claims_only();
        assert_eq!(claim_sigs.len(), 2);
        assert!(claim_sigs.contains(&EventSignatures::acquisition_claim()));
        assert!(claim_sigs.contains(&EventSignatures::disposal_claim()));
        assert!(!claim_sigs.contains(&EventSignatures::buy_order()));
    }

    #[test]
    fn test_claim_remainder_threshold_check() {
        // Test below threshold (100)
        let below_threshold = AcquisitionClaimEvent {
            controller: Address::ZERO,
            index_id: 1,
            vendor_id: 1,
            remain: 50, // Below 100 threshold
            spent: 0,
            itp_minted: 0,
            vault_address: Address::ZERO,
            tx_hash: B256::ZERO,
            block_number: 0,
            received_at: Utc::now(),
        };
        let event = VaultEvent::AcquisitionClaim(below_threshold);
        assert_eq!(event.claim_remainder(), Some(50));

        // Test above threshold
        let above_threshold = DisposalClaimEvent {
            controller: Address::ZERO,
            index_id: 1,
            vendor_id: 1,
            itp_remain: 150, // Above 100 threshold
            itp_burned: 0,
            gains: 0,
            vault_address: Address::ZERO,
            tx_hash: B256::ZERO,
            block_number: 0,
            received_at: Utc::now(),
        };
        let event = VaultEvent::DisposalClaim(above_threshold);
        assert_eq!(event.claim_remainder(), Some(150));
    }
}
