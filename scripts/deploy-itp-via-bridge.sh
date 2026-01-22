#!/usr/bin/env bash
# =============================================================================
# deploy-itp-via-bridge.sh - Deploy ITP via Bridge (Arbitrum → Orbit)
# =============================================================================
# Creates a proper ITP via the bridge with valid weights that sum to 1.0
#
# Flow:
# 1. Call requestCreateItp on Arbitrum BridgeProxy
# 2. Wait for bridge node to create vault on Orbit
# 3. Submit vendor data (assets, weights, margin, supply, market data)
#
# Usage:
#   ./vendor/scripts/deploy-itp-via-bridge.sh
#   ./vendor/scripts/deploy-itp-via-bridge.sh "My Index" "MYIDX" 1000
# =============================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
VAULTWORKS_DIR="$PROJECT_ROOT/vaultworks"

# Load environment
[ -f "$PROJECT_ROOT/.env" ] && source "$PROJECT_ROOT/.env"
[ -f "$PROJECT_ROOT/global.env" ] && source "$PROJECT_ROOT/global.env"

# Configuration
ARB_RPC_URL="${ARB_RPC_URL:-https://arb1.arbitrum.io/rpc}"
ORBIT_RPC_URL="${ORBIT_RPC_URL:-https://index.rpc.zeeve.net/}"
BRIDGE_PROXY="${BRIDGE_PROXY_ADDRESS:-0x511d7cbb2190Ba3261db993A81EbF158729c32ab}"
CASTLE="${CASTLE_ADDRESS:-0x1409a0ce0770e6e428add1ef73c6d872319557d8}"
COLLATERAL="${COLLATERAL_ADDRESS:-0xE1AccDbE71393F582CD86a6F04872ed341B499e9}"

# ITP Parameters (can be overridden via arguments)
ITP_NAME="${1:-Top 5 Crypto Index}"
ITP_SYMBOL="${2:-TOP5}"
INITIAL_PRICE="${3:-1000}"  # USDC with 6 decimals = 1000 * 10^6 = 1000000000
VENDOR_ID="${VENDOR_ID:-1}"

# Private key for transactions
if [ -z "$DEPLOY_PRIVATE_KEY" ]; then
    DEPLOY_PRIVATE_KEY="${ARBITRUM_PRIVATE_KEY:-${ORBIT_PRIVATE_KEY:-$TESTNET_PRIVATE_KEY}}"
fi

if [ -z "$DEPLOY_PRIVATE_KEY" ]; then
    echo "ERROR: DEPLOY_PRIVATE_KEY not set"
    echo "Set one of: DEPLOY_PRIVATE_KEY, ARBITRUM_PRIVATE_KEY, ORBIT_PRIVATE_KEY, TESTNET_PRIVATE_KEY"
    exit 1
fi

DEPLOYER=$(cast wallet address "$DEPLOY_PRIVATE_KEY")

# Colors for output
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[0;33m'
RED='\033[0;31m'
NC='\033[0m'

log() { echo -e "${BLUE}[INFO]${NC} $1"; }
success() { echo -e "${GREEN}[OK]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; }

# Byte encoding helper for vector values
encode_values() {
    local values="$1"
    python3 "$VAULTWORKS_DIR/scripts/vector_to_bytes.py" "$values"
}

echo ""
echo "============================================"
echo "Deploy ITP via Bridge"
echo "============================================"
echo ""
log "Name:        $ITP_NAME"
log "Symbol:      $ITP_SYMBOL"
log "Price:       $INITIAL_PRICE USDC"
log "Deployer:    $DEPLOYER"
log "BridgeProxy: $BRIDGE_PROXY"
log "Castle:      $CASTLE"
log "Vendor ID:   $VENDOR_ID"
echo ""

# Step 1: Check current nonce
log "Getting current ITP creation nonce..."
CURRENT_NONCE=$(cast call "$BRIDGE_PROXY" "getAdminItpCreationNonce(address)(uint256)" "$DEPLOYER" --rpc-url "$ARB_RPC_URL" 2>/dev/null || echo "0")
log "Current nonce: $CURRENT_NONCE"

# Step 2: Request ITP creation on Arbitrum
log "[1/9] Requesting ITP creation on Arbitrum..."

# Convert price to USDC (6 decimals)
PRICE_WITH_DECIMALS="${INITIAL_PRICE}000000"

TX_HASH=$(cast send "$BRIDGE_PROXY" \
    "requestCreateItp(string,string,uint256)" \
    "$ITP_NAME" "$ITP_SYMBOL" "$PRICE_WITH_DECIMALS" \
    --rpc-url "$ARB_RPC_URL" \
    --private-key "$DEPLOY_PRIVATE_KEY" \
    --json 2>/dev/null | jq -r '.transactionHash')

if [ -z "$TX_HASH" ] || [ "$TX_HASH" = "null" ]; then
    error "Failed to send requestCreateItp transaction"
    exit 1
fi

success "Transaction sent: $TX_HASH"

# Wait for confirmation
log "Waiting for transaction confirmation..."
cast receipt "$TX_HASH" --rpc-url "$ARB_RPC_URL" > /dev/null 2>&1

success "requestCreateItp confirmed"

# Step 3: Wait for bridge to create vault on Orbit
log "[2/9] Waiting for bridge to create vault on Orbit..."
log "This may take 30-60 seconds..."

# Poll for ItpCreated event
MAX_WAIT=120
WAITED=0
ORBIT_VAULT=""

while [ $WAITED -lt $MAX_WAIT ]; do
    # Get logs for ItpCreated event
    # Event signature: ItpCreated(address indexed orbitItp, address indexed arbitrumBridgedItp, uint256 indexed nonce)
    LOGS=$(cast logs \
        --from-block "latest-100" \
        --to-block "latest" \
        --address "$BRIDGE_PROXY" \
        "ItpCreated(address indexed,address indexed,uint256 indexed)" \
        --rpc-url "$ARB_RPC_URL" 2>/dev/null || echo "")

    if echo "$LOGS" | grep -qi "0x"; then
        # Extract orbit ITP address from topics[1]
        ORBIT_VAULT=$(echo "$LOGS" | head -1 | grep -oE "0x[a-fA-F0-9]{40}" | head -1)
        if [ -n "$ORBIT_VAULT" ]; then
            break
        fi
    fi

    sleep 5
    WAITED=$((WAITED + 5))
    echo -n "."
done
echo ""

if [ -z "$ORBIT_VAULT" ]; then
    warn "ItpCreated event not found after ${MAX_WAIT}s"
    warn "Bridge node may not be running or may be slow"
    warn "Checking Orbit for vault directly..."

    # Try to find the vault via Castle by checking recent indices
    # Generate expected index ID from nonce (base 10000 + nonce)
    EXPECTED_INDEX_ID=$((10000 + CURRENT_NONCE))
    log "Expected index ID: $EXPECTED_INDEX_ID"

    ORBIT_VAULT=$(cast call "$CASTLE" "getVault(uint128)(address)" "$EXPECTED_INDEX_ID" --rpc-url "$ORBIT_RPC_URL" 2>/dev/null || echo "")

    if [ -z "$ORBIT_VAULT" ] || [ "$ORBIT_VAULT" = "0x0000000000000000000000000000000000000000" ]; then
        error "Could not find vault on Orbit"
        error "Either:"
        error "  1. Bridge node is not running"
        error "  2. Bridge node failed to create vault"
        error "  3. Vault creation is still in progress"
        echo ""
        error "To debug, check:"
        error "  - Bridge node logs: tail -f .pids/bridge-node.log"
        error "  - Manually check Castle events on Orbit"
        exit 1
    fi
fi

success "Vault found on Orbit: $ORBIT_VAULT"

# Get the index ID from the vault
INDEX_ID=$((10000 + CURRENT_NONCE))
log "Using index ID: $INDEX_ID"

# Step 4: Submit assets on Orbit via vendor
log "[3/9] Submitting assets..."

# Top 5 assets: BTC(101), ETH(102), SOL(105), LINK(114), UNI(115)
ASSETS=$(encode_values "[101, 102, 105, 114, 115]")
cast send "$CASTLE" "submitAssets(uint128,bytes)" "$VENDOR_ID" "$ASSETS" \
    --rpc-url "$ORBIT_RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null 2>&1
success "Assets submitted: BTC, ETH, SOL, LINK, UNI"

# Step 5: Submit margin
log "[4/9] Submitting margin..."
ASSET_NAMES=$(encode_values "[101, 102, 105, 114, 115]")
MARGINS=$(encode_values "[2.0, 2.0, 2.0, 2.0, 2.0]")
cast send "$CASTLE" "submitMargin(uint128,bytes,bytes)" "$VENDOR_ID" "$ASSET_NAMES" "$MARGINS" \
    --rpc-url "$ORBIT_RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null 2>&1
success "Margin submitted"

# Step 6: Submit supply
log "[5/9] Submitting supply..."
SHORT=$(encode_values "[0, 0, 0, 0, 0]")
LONG=$(encode_values "[1.0, 1.0, 1.0, 1.0, 1.0]")
cast send "$CASTLE" "submitSupply(uint128,bytes,bytes,bytes)" "$VENDOR_ID" "$ASSET_NAMES" "$SHORT" "$LONG" \
    --rpc-url "$ORBIT_RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null 2>&1
success "Supply submitted"

# Step 7: Submit asset weights (MUST SUM TO 1.0)
log "[6/9] Submitting asset weights (sum = 1.0)..."
# Weights: 0.30 + 0.25 + 0.20 + 0.15 + 0.10 = 1.00
WEIGHT_ASSETS=$(encode_values "[101, 102, 105, 114, 115]")
WEIGHTS=$(encode_values "[0.30, 0.25, 0.20, 0.15, 0.10]")
cast send "$CASTLE" "submitAssetWeights(uint128,bytes,bytes)" "$INDEX_ID" "$WEIGHT_ASSETS" "$WEIGHTS" \
    --rpc-url "$ORBIT_RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null 2>&1
success "Weights submitted (0.30 + 0.25 + 0.20 + 0.15 + 0.10 = 1.00)"

# Step 8: Submit market data
log "[7/9] Submitting market data..."
LIQUIDITY=$(encode_values "[0.5, 0.5, 0.5, 0.5, 0.5]")
PRICES=$(encode_values "[100000.0, 3500.0, 200.0, 25.0, 15.0]")  # Approximate prices
SLOPES=$(encode_values "[1.0, 0.5, 0.2, 0.1, 0.1]")
cast send "$CASTLE" "submitMarketData(uint128,bytes,bytes,bytes,bytes)" \
    "$VENDOR_ID" "$WEIGHT_ASSETS" "$LIQUIDITY" "$PRICES" "$SLOPES" \
    --rpc-url "$ORBIT_RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null 2>&1
success "Market data submitted"

# Step 9: Update index quote
log "[8/9] Updating index quote..."
cast send "$CASTLE" "updateIndexQuote(uint128,uint128)" "$VENDOR_ID" "$INDEX_ID" \
    --rpc-url "$ORBIT_RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null 2>&1
success "Quote updated"

# Step 10: Verify and display
log "[9/9] Verifying ITP..."

# Get vault info
NAME=$(cast call "$ORBIT_VAULT" "name()(string)" --rpc-url "$ORBIT_RPC_URL" 2>/dev/null | tr -d '"' || echo "Unknown")
SYMBOL=$(cast call "$ORBIT_VAULT" "symbol()(string)" --rpc-url "$ORBIT_RPC_URL" 2>/dev/null | tr -d '"' || echo "Unknown")

echo ""
echo "============================================"
success "ITP Deployment Complete!"
echo "============================================"
echo ""
echo "Index ID:      $INDEX_ID"
echo "Orbit Vault:   $ORBIT_VAULT"
echo "Name:          $NAME"
echo "Symbol:        $SYMBOL"
echo ""

# Verify weights sum to 1.0
log "Verifying weights..."
WEIGHTS_HEX=$(cast call "$CASTLE" "getIndexWeights(uint128)(bytes)" "$INDEX_ID" --rpc-url "$ORBIT_RPC_URL" 2>/dev/null || echo "")

if [ -n "$WEIGHTS_HEX" ]; then
python3 << PYEOF
weights_hex = "$WEIGHTS_HEX"
if weights_hex.startswith("0x"):
    weights_hex = weights_hex[2:]
if len(weights_hex) > 0:
    weights_bytes = bytes.fromhex(weights_hex)
    weights = []
    for i in range(0, len(weights_bytes), 16):
        chunk = weights_bytes[i:i+16]
        value = int.from_bytes(chunk, 'little')
        weights.append(value / 1e18)
    print(f"  Weights: {weights}")
    print(f"  Sum: {sum(weights):.4f}")
    if abs(sum(weights) - 1.0) < 0.01:
        print("  ✓ Weights are valid!")
    else:
        print("  ✗ ERROR: Weights don't sum to 1.0!")
PYEOF
fi

echo ""
echo "Next steps:"
echo "  1. Sync ITP to database: ./vendor/scripts/sync-itps.sh"
echo "  2. View in frontend at: http://localhost:3000/discover"
echo ""
