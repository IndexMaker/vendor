#!/bin/bash
# Diagnose ITP deployment status
# Checks both Arbitrum BridgeProxy events and Orbit Castle state

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../../global.env" 2>/dev/null || true

# Configuration with defaults
ARB_RPC="${ARB_RPC_URL:-https://arb1.arbitrum.io/rpc}"
ORBIT_RPC="${ORBIT_RPC_URL:-https://index.rpc.zeeve.net/}"
BRIDGE_PROXY="${BRIDGE_PROXY_ADDRESS:-0x511d7cbb2190Ba3261db993A81EbF158729c32ab}"
FACTORY="${ARBITRUM_FACTORY_ADDRESS:-0x6773EbDEb543C10a5D48242F5c7650f42491F651}"
CASTLE="${CASTLE_ADDRESS:-0x1409a0ce0770e6e428add1ef73c6d872319557d8}"
START_BLOCK="${CONTRACT_DEPLOY_BLOCK:-290000000}"

echo "============================================"
echo "ITP Deployment Diagnostic"
echo "============================================"
echo ""
echo "Configuration:"
echo "  Arbitrum RPC:  $ARB_RPC"
echo "  Orbit RPC:     $ORBIT_RPC"
echo "  BridgeProxy:   $BRIDGE_PROXY"
echo "  Factory:       $FACTORY"
echo "  Castle:        $CASTLE"
echo "  Start Block:   $START_BLOCK"
echo ""

# Event signatures
CREATE_REQUESTED="0xb50d4d3fc817c54ae0bc5765780ee264a61072c333de6e903d619c39e60c372c"
ITP_CREATED="0x..." # We'll compute this

echo "============================================"
echo "1. CreateItpRequested Events (Arbitrum)"
echo "============================================"
echo ""

REQUESTED_EVENTS=$(cast logs --from-block $START_BLOCK \
    --address $BRIDGE_PROXY \
    --rpc-url "$ARB_RPC" 2>/dev/null || echo "")

if [ -z "$REQUESTED_EVENTS" ]; then
    echo "No events found on BridgeProxy"
else
    echo "Raw events found. Parsing..."
    echo ""

    # Parse events and extract data
    echo "$REQUESTED_EVENTS" | grep -A 20 "topics:" | while read -r line; do
        if [[ "$line" == *"$CREATE_REQUESTED"* ]]; then
            echo "Found CreateItpRequested event"
        fi
    done

    # Count events by topic
    REQUESTED_COUNT=$(echo "$REQUESTED_EVENTS" | grep -c "$CREATE_REQUESTED" || echo "0")
    echo "CreateItpRequested events: $REQUESTED_COUNT"
fi

echo ""
echo "============================================"
echo "2. Decoding CreateItpRequested Events"
echo "============================================"
echo ""

# Get all CreateItpRequested events and decode them
cast logs --from-block $START_BLOCK \
    --address $BRIDGE_PROXY \
    --rpc-url "$ARB_RPC" 2>/dev/null | grep -B5 -A15 "$CREATE_REQUESTED" | while read -r line; do
    # Extract nonce from topics[2]
    if [[ "$line" =~ 0x0000000000000000000000000000000000000000000000000000000000000([0-9a-f]+) ]]; then
        nonce_hex="${BASH_REMATCH[1]}"
        nonce=$((16#$nonce_hex))
        if [ "$nonce" -lt 100 ]; then
            echo "  Nonce: $nonce"
        fi
    fi
    # Extract data for name/symbol
    if [[ "$line" == *"data:"* ]]; then
        data=$(echo "$line" | sed 's/.*data: //')
        echo "  Data: ${data:0:66}..."
    fi
    if [[ "$line" == *"blockNumber:"* ]]; then
        block=$(echo "$line" | sed 's/.*blockNumber: //')
        echo "  Block: $block"
        echo ""
    fi
done

echo ""
echo "============================================"
echo "3. ItpCreated Events (Completed ITPs)"
echo "============================================"
echo ""

# Check for ItpCreated events - these would have 4 topics (signature + 3 indexed params)
# ItpCreated(address indexed orbitItp, address indexed arbitrumBridgedItp, uint256 indexed nonce)
CREATED_EVENTS=$(cast logs --from-block $START_BLOCK \
    --address $BRIDGE_PROXY \
    --rpc-url "$ARB_RPC" 2>/dev/null | grep -B5 -A15 "topics:" | grep -E "^\s+0x[a-f0-9]{64}" | wc -l)

echo "Looking for events with 4 topics (ItpCreated)..."

# Get unique topic0 values to find event types
echo ""
echo "Event types found on BridgeProxy:"
cast logs --from-block $START_BLOCK \
    --address $BRIDGE_PROXY \
    --rpc-url "$ARB_RPC" 2>/dev/null | grep -A1 "topics:" | grep "0x" | head -20 | sort -u

echo ""
echo "============================================"
echo "4. Factory Deployed Tokens (Arbitrum)"
echo "============================================"
echo ""

# Check factory for any deployed tokens
echo "Checking factory events..."
cast logs --from-block $START_BLOCK \
    --address $FACTORY \
    --rpc-url "$ARB_RPC" 2>/dev/null | head -30 || echo "No factory events"

echo ""
echo "============================================"
echo "5. Orbit Castle State"
echo "============================================"
echo ""

# Try to get vault count
echo "Querying Castle.getVaultCount()..."
VAULT_COUNT=$(cast call $CASTLE "getVaultCount()(uint128)" --rpc-url "$ORBIT_RPC" 2>/dev/null || echo "error")
echo "Vault count: $VAULT_COUNT"

# Try known index IDs
echo ""
echo "Checking known vault indices..."
for INDEX_ID in 0 1 1001 1002; do
    VAULT=$(cast call $CASTLE "getVault(uint128)(address)" $INDEX_ID --rpc-url "$ORBIT_RPC" 2>/dev/null || echo "0x0")
    if [ "$VAULT" != "0x0000000000000000000000000000000000000000" ] && [ "$VAULT" != "0x0" ] && [ -n "$VAULT" ]; then
        echo "  Index $INDEX_ID: $VAULT"

        # Get vault metadata
        NAME=$(cast call "$VAULT" "name()(string)" --rpc-url "$ORBIT_RPC" 2>/dev/null || echo "N/A")
        SYMBOL=$(cast call "$VAULT" "symbol()(string)" --rpc-url "$ORBIT_RPC" 2>/dev/null || echo "N/A")
        echo "    Name: $NAME"
        echo "    Symbol: $SYMBOL"
    fi
done

echo ""
echo "============================================"
echo "6. Database State"
echo "============================================"
echo ""

# Check database
cd "$SCRIPT_DIR/../../indexmaker-backend" 2>/dev/null || cd "$SCRIPT_DIR/../.."
psql -d indexmaker_db -c "SELECT id, name, symbol, orbit_address, arbitrum_address, state, created_at FROM itps LIMIT 10;" 2>/dev/null || echo "Could not query database"

echo ""
echo "============================================"
echo "Summary"
echo "============================================"
echo ""
echo "Issues detected:"

# Check if CreateItpRequested but no ItpCreated
if [ "$REQUESTED_COUNT" -gt 0 ]; then
    echo "  - $REQUESTED_COUNT ITP creation(s) requested but may not be completed"
    echo "    -> Bridge node may not be running or processing events"
fi

if [ "$VAULT_COUNT" = "0" ] || [ "$VAULT_COUNT" = "error" ]; then
    echo "  - No vaults found on Orbit Castle"
    echo "    -> ITPs may not have been created on Orbit chain"
fi

echo ""
echo "Recommended actions:"
echo "  1. Check if bridge-node is running: ps aux | grep bridge"
echo "  2. Check bridge-node logs for errors"
echo "  3. If ITPs were created directly on Orbit, they need to be synced to DB"
echo "  4. Run: vendor/scripts/sync-itps-to-db.sh (to be created)"
