#!/usr/bin/env bash
# =============================================================================
# deploy-proper-itp.sh - Deploy ITP with proper weights summing to 1.0
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

# Must have private key
if [ -z "$DEPLOY_PRIVATE_KEY" ]; then
    DEPLOY_PRIVATE_KEY="${ORBIT_PRIVATE_KEY:-$TESTNET_PRIVATE_KEY}"
fi
[ -z "$DEPLOY_PRIVATE_KEY" ] && { echo "ERROR: DEPLOY_PRIVATE_KEY not set"; exit 1; }

DEPLOYER=$(cast wallet address "$DEPLOY_PRIVATE_KEY")

# Use a unique index ID
INDEX_ID="${1:-2001}"
VENDOR_ID="${2:-1}"

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'

log() { echo -e "${BLUE}[INFO]${NC} $1"; }
success() { echo -e "${GREEN}[OK]${NC} $1"; }

# Byte encoding helper
encode_values() {
    local values="$1"
    python3 "$VAULTWORKS_DIR/scripts/vector_to_bytes.py" "$values"
}

echo ""
echo "============================================"
echo "Deploy Proper ITP (Weights Sum to 1.0)"
echo "============================================"
echo ""
log "RPC:       $RPC_URL"
log "Castle:    $CASTLE"
log "Deployer:  $DEPLOYER"
log "Index ID:  $INDEX_ID"
log "Vendor ID: $VENDOR_ID"
echo ""

# Check vault prototype
VAULT_PROTO=$(cast call "$CASTLE" "getVaultPrototype()(address)" --rpc-url "$RPC_URL" 2>/dev/null || echo "")
if [ -z "$VAULT_PROTO" ] || [ "$VAULT_PROTO" = "0x0000000000000000000000000000000000000000" ]; then
    echo "ERROR: Vault prototype not set. Run deploy.sh first."
    exit 1
fi
success "Vault prototype: $VAULT_PROTO"

echo ""
log "[1/8] Submitting assets..."
# Assets: BTC(101), ETH(102), SOL(105), LINK(114), UNI(115)
ASSETS=$(encode_values "[101, 102, 105, 114, 115]")
cast send "$CASTLE" "submitAssets(uint128,bytes)" "$VENDOR_ID" "$ASSETS" \
    --rpc-url "$RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null
success "Assets submitted"

log "[2/8] Submitting margin..."
ASSET_NAMES=$(encode_values "[101, 102, 105, 114, 115]")
MARGINS=$(encode_values "[2.0, 2.0, 2.0, 2.0, 2.0]")
cast send "$CASTLE" "submitMargin(uint128,bytes,bytes)" "$VENDOR_ID" "$ASSET_NAMES" "$MARGINS" \
    --rpc-url "$RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null
success "Margin submitted"

log "[3/8] Submitting supply..."
SHORT=$(encode_values "[0, 0, 0, 0, 0]")
LONG=$(encode_values "[1.0, 1.0, 1.0, 1.0, 1.0]")
cast send "$CASTLE" "submitSupply(uint128,bytes,bytes,bytes)" "$VENDOR_ID" "$ASSET_NAMES" "$SHORT" "$LONG" \
    --rpc-url "$RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null
success "Supply submitted"

log "[4/8] Creating index (submitIndex)..."
INDEX_NAME="Top 5 Crypto Index"
INDEX_SYMBOL="TOP5"
DESCRIPTION="Diversified index of top 5 crypto assets"
METHODOLOGY="Market Cap Weighted"
INITIAL_PRICE="1000000000000000000000"  # 1000 * 10^18
MAX_ORDER_SIZE="100000000000000000000"  # 100 * 10^18
CUSTODY_STRING="Orbit Custody"

cast send "$CASTLE" \
    "submitIndex(uint128,uint128,string,string,string,string,uint128,address,string,address[],address,address,uint128)" \
    "$VENDOR_ID" "$INDEX_ID" "$INDEX_NAME" "$INDEX_SYMBOL" "$DESCRIPTION" "$METHODOLOGY" \
    "$INITIAL_PRICE" "$DEPLOYER" "$CUSTODY_STRING" "[$DEPLOYER]" "$CUSTODY" "$COLLATERAL" "$MAX_ORDER_SIZE" \
    --rpc-url "$RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null
success "Index created"

log "[5/8] Submitting asset weights (sum = 1.0)..."
# Weights: 0.30 + 0.25 + 0.20 + 0.15 + 0.10 = 1.00
WEIGHT_ASSETS=$(encode_values "[101, 102, 105, 114, 115]")
WEIGHTS=$(encode_values "[0.30, 0.25, 0.20, 0.15, 0.10]")
cast send "$CASTLE" "submitAssetWeights(uint128,bytes,bytes)" "$INDEX_ID" "$WEIGHT_ASSETS" "$WEIGHTS" \
    --rpc-url "$RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null
success "Weights submitted (0.30 + 0.25 + 0.20 + 0.15 + 0.10 = 1.00)"

log "[6/8] Submitting vote..."
cast send "$CASTLE" "submitVote(uint128,bytes)" "$INDEX_ID" "0x" \
    --rpc-url "$RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null
success "Vote submitted"

log "[7/8] Submitting market data..."
LIQUIDITY=$(encode_values "[0.5, 0.5, 0.5, 0.5, 0.5]")
PRICES=$(encode_values "[100000.0, 3500.0, 200.0, 25.0, 15.0]")  # BTC, ETH, SOL, LINK, UNI approx prices
SLOPES=$(encode_values "[1.0, 0.5, 0.2, 0.1, 0.1]")
cast send "$CASTLE" "submitMarketData(uint128,bytes,bytes,bytes,bytes)" \
    "$VENDOR_ID" "$WEIGHT_ASSETS" "$LIQUIDITY" "$PRICES" "$SLOPES" \
    --rpc-url "$RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null
success "Market data submitted"

log "[8/8] Updating index quote..."
cast send "$CASTLE" "updateIndexQuote(uint128,uint128)" "$VENDOR_ID" "$INDEX_ID" \
    --rpc-url "$RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null
success "Quote updated"

echo ""
log "Getting vault address..."
VAULT_ADDR=$(cast call "$CASTLE" "getVault(uint128)(address)" "$INDEX_ID" --rpc-url "$RPC_URL")
success "Vault deployed: $VAULT_ADDR"

# Verify
echo ""
log "Verifying ITP..."
NAME=$(cast call "$VAULT_ADDR" "name()(string)" --rpc-url "$RPC_URL" | tr -d '"')
SYMBOL=$(cast call "$VAULT_ADDR" "symbol()(string)" --rpc-url "$RPC_URL" | tr -d '"')
echo "  Name:   $NAME"
echo "  Symbol: $SYMBOL"

# Verify weights
WEIGHTS_HEX=$(cast call "$CASTLE" "getIndexWeights(uint128)(bytes)" "$INDEX_ID" --rpc-url "$RPC_URL")
echo ""
log "Verifying weights sum to 1.0..."
python3 << PYEOF
weights_hex = "$WEIGHTS_HEX"
if weights_hex.startswith("0x"):
    weights_hex = weights_hex[2:]
weights_bytes = bytes.fromhex(weights_hex)
weights = []
for i in range(0, len(weights_bytes), 16):
    chunk = weights_bytes[i:i+16]
    value = int.from_bytes(chunk, 'little')
    weights.append(value / 1e18)
print(f"  Weights: {weights}")
print(f"  Sum: {sum(weights):.2f}")
if abs(sum(weights) - 1.0) < 0.01:
    print("  ✓ Weights are valid!")
else:
    print("  ✗ ERROR: Weights don't sum to 1.0!")
PYEOF

echo ""
success "ITP Deployment Complete!"
echo ""
echo "Index ID: $INDEX_ID"
echo "Vault:    $VAULT_ADDR"
echo ""
echo "Run sync to add to database:"
echo "  ./vendor/scripts/sync-itps.sh"
