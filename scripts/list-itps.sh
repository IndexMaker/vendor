#!/bin/bash
# List all deployed ITPs on Orbit chain
# Usage: ./list-itps.sh [--verbose]

set -e

# Load environment
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../../global.env"

# Configuration
RPC_URL="${ORBIT_RPC_URL:-https://index.rpc.zeeve.net/}"
CASTLE="${CASTLE_ADDRESS:-0x1409a0ce0770e6e428add1ef73c6d872319557d8}"
VERBOSE="${1:-}"

echo "============================================"
echo "ITP Discovery - Orbit Chain"
echo "============================================"
echo "RPC:    $RPC_URL"
echo "Castle: $CASTLE"
echo ""

# Function to call contract
call() {
    cast call "$CASTLE" "$1" --rpc-url "$RPC_URL" 2>/dev/null || echo "0x"
}

# Function to decode uint128 from hex
decode_uint128() {
    local hex="$1"
    # Remove 0x prefix and leading zeros
    hex="${hex#0x}"
    # Convert to decimal using printf
    printf "%d" "0x${hex:-0}" 2>/dev/null || echo "0"
}

# Try to get vault count
echo "Querying vault count..."
VAULT_COUNT_HEX=$(call "getVaultCount()(uint128)")
VAULT_COUNT=$(decode_uint128 "$VAULT_COUNT_HEX")

echo "Total vaults registered: $VAULT_COUNT"
echo ""

if [ "$VAULT_COUNT" -eq 0 ]; then
    echo "No vaults found. Trying alternative discovery..."
    echo ""

    # Try known index IDs (1001 is default from create-test-itp.sh)
    for INDEX_ID in 1001 1002 1 2 3; do
        VAULT_ADDR=$(call "getVault(uint128)(address)" "$INDEX_ID")
        if [ -n "$VAULT_ADDR" ] && [ "$VAULT_ADDR" != "0x" ] && [ "$VAULT_ADDR" != "0x0000000000000000000000000000000000000000" ]; then
            echo "Found ITP at index $INDEX_ID: $VAULT_ADDR"
        fi
    done
    echo ""
fi

echo "============================================"
echo "Querying known index IDs (1001-1010)..."
echo "============================================"
echo ""

# Query individual vaults by index ID
for INDEX_ID in $(seq 1001 1010); do
    VAULT_ADDR=$(call "getVault(uint128)(address)" "$INDEX_ID")

    # Check if valid address (not zero address)
    if [ -n "$VAULT_ADDR" ] && [ "$VAULT_ADDR" != "0x" ] && [ "$VAULT_ADDR" != "0x0000000000000000000000000000000000000000" ]; then
        echo "Index ID: $INDEX_ID"
        echo "  Vault Address: $VAULT_ADDR"

        if [ "$VERBOSE" = "--verbose" ]; then
            # Get vault token metadata
            NAME=$(cast call "$VAULT_ADDR" "name()(string)" --rpc-url "$RPC_URL" 2>/dev/null || echo "N/A")
            SYMBOL=$(cast call "$VAULT_ADDR" "symbol()(string)" --rpc-url "$RPC_URL" 2>/dev/null || echo "N/A")
            SUPPLY=$(cast call "$VAULT_ADDR" "totalSupply()(uint256)" --rpc-url "$RPC_URL" 2>/dev/null || echo "0")

            echo "  Name: $NAME"
            echo "  Symbol: $SYMBOL"
            echo "  Total Supply: $SUPPLY"
        fi
        echo ""
    fi
done

echo "============================================"
echo "Checking Arbitrum Factory for bridged ITPs..."
echo "============================================"
echo ""

ARB_RPC="${ARBITRUM_RPC:-https://arb1.arbitrum.io/rpc}"
FACTORY="${ARBITRUM_FACTORY_ADDRESS:-0x6773EbDEb543C10a5D48242F5c7650f42491F651}"

# Try to get bridged ITP list from factory
echo "Factory: $FACTORY"
echo ""

# Query factory for deployed tokens
# Try getDeployedTokens or similar
DEPLOYED=$(cast call "$FACTORY" "getDeployedTokens()(address[])" --rpc-url "$ARB_RPC" 2>/dev/null || echo "")

if [ -n "$DEPLOYED" ] && [ "$DEPLOYED" != "[]" ]; then
    echo "Bridged ITPs on Arbitrum:"
    echo "$DEPLOYED"
else
    echo "No getDeployedTokens function or empty list"
    echo "Trying to enumerate via events..."

    # Check for TokenDeployed events
    cast logs --from-block 290000000 --to-block latest \
        --address "$FACTORY" \
        "TokenDeployed(uint256,address)" \
        --rpc-url "$ARB_RPC" 2>/dev/null | head -50 || echo "No events found"
fi

echo ""
echo "============================================"
echo "Done"
echo "============================================"
