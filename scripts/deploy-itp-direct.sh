#!/usr/bin/env bash
# =============================================================================
# deploy-itp-direct.sh - Deploy ITP directly on Orbit (bypass bridge)
# =============================================================================
# Creates a proper ITP directly on Orbit chain with valid weights that sum to 1.0
# This bypasses the bridge for immediate testing.
#
# Usage:
#   ./vendor/scripts/deploy-itp-direct.sh
#   ./vendor/scripts/deploy-itp-direct.sh 2001 "My Index" "MYIDX" 1000
# =============================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
VAULTWORKS_DIR="$PROJECT_ROOT/vaultworks"

# Load environment
[ -f "$PROJECT_ROOT/.env" ] && source "$PROJECT_ROOT/.env"
[ -f "$PROJECT_ROOT/global.env" ] && source "$PROJECT_ROOT/global.env"

# Configuration
RPC_URL="${ORBIT_RPC_URL:-https://index.rpc.zeeve.net/}"
CASTLE="${CASTLE_ADDRESS:-0x1409a0ce0770e6e428add1ef73c6d872319557d8}"
COLLATERAL="${COLLATERAL_ADDRESS:-0x183A81F735430AAF58227aF4c0D7B35bC8e0f8B6}"
CUSTODY="${CUSTODY_ADDRESS:-$CASTLE}"

# ITP Parameters
INDEX_ID="${1:-2001}"
ITP_NAME="${2:-Top 5 Crypto Index}"
ITP_SYMBOL="${3:-TOP5}"
INITIAL_PRICE="${4:-1000}"  # In USDC, will be converted to 18 decimals
VENDOR_ID="${5:-1}"

# Private key
if [ -z "$DEPLOY_PRIVATE_KEY" ]; then
    DEPLOY_PRIVATE_KEY="${ORBIT_PRIVATE_KEY:-$TESTNET_PRIVATE_KEY}"
fi

if [ -z "$DEPLOY_PRIVATE_KEY" ]; then
    echo "ERROR: DEPLOY_PRIVATE_KEY not set"
    exit 1
fi

DEPLOYER=$(cast wallet address "$DEPLOY_PRIVATE_KEY")

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[0;33m'
RED='\033[0;31m'
NC='\033[0m'

log() { echo -e "${BLUE}[INFO]${NC} $1"; }
success() { echo -e "${GREEN}[OK]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; }

encode_values() {
    python3 "$VAULTWORKS_DIR/scripts/vector_to_bytes.py" "$1"
}

echo ""
echo "============================================"
echo "Deploy ITP Directly on Orbit"
echo "============================================"
echo ""
log "Index ID:    $INDEX_ID"
log "Name:        $ITP_NAME"
log "Symbol:      $ITP_SYMBOL"
log "Price:       $INITIAL_PRICE USDC"
log "Deployer:    $DEPLOYER"
log "Castle:      $CASTLE"
log "Vendor ID:   $VENDOR_ID"
echo ""

# Check if index already exists
EXISTING=$(cast call "$CASTLE" "getVault(uint128)(address)" "$INDEX_ID" --rpc-url "$RPC_URL" 2>/dev/null || echo "")
if [ -n "$EXISTING" ] && [ "$EXISTING" != "0x0000000000000000000000000000000000000000" ]; then
    warn "Index $INDEX_ID already exists at: $EXISTING"
    echo "Use a different INDEX_ID or continue with vendor data submission"
    read -p "Continue? (y/N) " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        exit 1
    fi
fi

echo ""

# Step 1: Submit assets
log "[1/8] Submitting assets..."
ASSETS=$(encode_values "[101, 102, 105, 114, 115]")
cast send "$CASTLE" "submitAssets(uint128,bytes)" "$VENDOR_ID" "$ASSETS" \
    --rpc-url "$RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null 2>&1
success "Assets submitted: BTC(101), ETH(102), SOL(105), LINK(114), UNI(115)"

# Step 2: Submit margin
log "[2/8] Submitting margin..."
ASSET_NAMES=$(encode_values "[101, 102, 105, 114, 115]")
MARGINS=$(encode_values "[2.0, 2.0, 2.0, 2.0, 2.0]")
cast send "$CASTLE" "submitMargin(uint128,bytes,bytes)" "$VENDOR_ID" "$ASSET_NAMES" "$MARGINS" \
    --rpc-url "$RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null 2>&1
success "Margin submitted"

# Step 3: Submit supply
log "[3/8] Submitting supply..."
SHORT=$(encode_values "[0, 0, 0, 0, 0]")
LONG=$(encode_values "[1.0, 1.0, 1.0, 1.0, 1.0]")
cast send "$CASTLE" "submitSupply(uint128,bytes,bytes,bytes)" "$VENDOR_ID" "$ASSET_NAMES" "$SHORT" "$LONG" \
    --rpc-url "$RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null 2>&1
success "Supply submitted"

# Step 4: Create index via submitIndex
log "[4/8] Creating index (submitIndex)..."

DESCRIPTION="Diversified index of top 5 crypto assets by market cap"
METHODOLOGY="Market Cap Weighted"
CUSTODY_STRING="Orbit Custody"

# Convert price to 18 decimals
PRICE_18=$(python3 -c "print(int($INITIAL_PRICE * 10**18))")
MAX_ORDER_SIZE="100000000000000000000"  # 100 * 10^18

cast send "$CASTLE" \
    "submitIndex(uint128,uint128,string,string,string,string,uint128,address,string,address[],address,address,uint128)" \
    "$VENDOR_ID" "$INDEX_ID" "$ITP_NAME" "$ITP_SYMBOL" "$DESCRIPTION" "$METHODOLOGY" \
    "$PRICE_18" "$DEPLOYER" "$CUSTODY_STRING" "[$DEPLOYER]" "$CUSTODY" "$COLLATERAL" "$MAX_ORDER_SIZE" \
    --rpc-url "$RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null 2>&1
success "Index created"

# Step 5: Submit asset weights (MUST SUM TO 1.0)
log "[5/8] Submitting asset weights (sum = 1.0)..."
WEIGHT_ASSETS=$(encode_values "[101, 102, 105, 114, 115]")
# Weights: 0.30 + 0.25 + 0.20 + 0.15 + 0.10 = 1.00
WEIGHTS=$(encode_values "[0.30, 0.25, 0.20, 0.15, 0.10]")
cast send "$CASTLE" "submitAssetWeights(uint128,bytes,bytes)" "$INDEX_ID" "$WEIGHT_ASSETS" "$WEIGHTS" \
    --rpc-url "$RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null 2>&1
success "Weights submitted: BTC=30%, ETH=25%, SOL=20%, LINK=15%, UNI=10%"

# Step 6: Submit vote
log "[6/8] Submitting vote..."
cast send "$CASTLE" "submitVote(uint128,bytes)" "$INDEX_ID" "0x" \
    --rpc-url "$RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null 2>&1
success "Vote submitted"

# Step 7: Submit market data
log "[7/8] Submitting market data..."
LIQUIDITY=$(encode_values "[0.5, 0.5, 0.5, 0.5, 0.5]")
PRICES=$(encode_values "[100000.0, 3500.0, 200.0, 25.0, 15.0]")
SLOPES=$(encode_values "[1.0, 0.5, 0.2, 0.1, 0.1]")
cast send "$CASTLE" "submitMarketData(uint128,bytes,bytes,bytes,bytes)" \
    "$VENDOR_ID" "$WEIGHT_ASSETS" "$LIQUIDITY" "$PRICES" "$SLOPES" \
    --rpc-url "$RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null 2>&1
success "Market data submitted"

# Step 8: Update index quote
log "[8/8] Updating index quote..."
cast send "$CASTLE" "updateIndexQuote(uint128,uint128)" "$VENDOR_ID" "$INDEX_ID" \
    --rpc-url "$RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null 2>&1
success "Quote updated"

# Get vault address
echo ""
log "Getting vault address..."
VAULT_ADDR=$(cast call "$CASTLE" "getVault(uint128)(address)" "$INDEX_ID" --rpc-url "$RPC_URL")
success "Vault deployed: $VAULT_ADDR"

# Verify
echo ""
log "Verifying ITP..."
NAME=$(cast call "$VAULT_ADDR" "name()(string)" --rpc-url "$RPC_URL" | tr -d '"')
SYMBOL=$(cast call "$VAULT_ADDR" "symbol()(string)" --rpc-url "$RPC_URL" | tr -d '"')

echo ""
echo "============================================"
success "ITP Deployment Complete!"
echo "============================================"
echo ""
echo "  Index ID:  $INDEX_ID"
echo "  Vault:     $VAULT_ADDR"
echo "  Name:      $NAME"
echo "  Symbol:    $SYMBOL"
echo ""

# Verify weights
log "Verifying weights sum to 1.0..."
WEIGHTS_HEX=$(cast call "$CASTLE" "getIndexWeights(uint128)(bytes)" "$INDEX_ID" --rpc-url "$RPC_URL")
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
    print(f"  Weights: {[f'{w:.2f}' for w in weights]}")
    print(f"  Sum: {sum(weights):.4f}")
    if abs(sum(weights) - 1.0) < 0.01:
        print("  ✓ Weights are valid!")
    else:
        print("  ✗ ERROR: Weights don't sum to 1.0!")
else:
    print("  No weights found")
PYEOF

echo ""
echo "Next steps:"
echo "  1. Sync ITP to database: ./vendor/scripts/sync-itps.sh"
echo "  2. View in frontend at: http://localhost:3000/discover"
echo ""
