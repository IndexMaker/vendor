use alloy_sol_types::sol;

sol! {
    /// Event emitted when a new trading pair is registered in the system
    #[allow(missing_docs)]
    event PairRegistered(uint128 indexed pair_id, string symbol, string base_asset, string quote_asset);

    interface IPairRegistry {
        /// Register a new trading pair
        function registerPair(string calldata symbol, string calldata base_asset, string calldata quote_asset) external returns (uint128);

        /// Get pair information by ID
        function getPair(uint128 pair_id) external view returns (string memory symbol, string memory base_asset, string memory quote_asset);

        /// Get pair ID by symbol
        function getPairBySymbol(string calldata symbol) external view returns (uint128);

        /// Check if a pair exists
        function pairExists(uint128 pair_id) external view returns (bool);

        /// Get total number of registered pairs
        function pairCount() external view returns (uint128);
    }
}
