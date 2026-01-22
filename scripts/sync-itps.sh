#!/usr/bin/env bash
# =============================================================================
# sync-itps.sh - Comprehensive ITP Sync from Orbit Chain to Database
# =============================================================================
# Discovers all ITPs via Castle ACL, fetches full metadata including assets,
# and syncs to the PostgreSQL database.
#
# Usage: ./sync-itps.sh [--verbose]
# =============================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Load environment
[ -f "$PROJECT_ROOT/.env" ] && source "$PROJECT_ROOT/.env"
[ -f "$PROJECT_ROOT/global.env" ] && source "$PROJECT_ROOT/global.env"

# Configuration
RPC_URL="${ORBIT_RPC_URL:-https://index.rpc.zeeve.net/}"
CASTLE="${CASTLE_ADDRESS:-0x1409a0ce0770e6e428add1ef73c6d872319557d8}"
DB_NAME="${DB_NAME:-indexmaker_db}"
VERBOSE=false

# Parse args
[[ "$1" == "--verbose" ]] && VERBOSE=true

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m'

log() { echo -e "${BLUE}[INFO]${NC} $1"; }
success() { echo -e "${GREEN}[OK]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }

# Asset label mapping is done in Python (labels_to_symbols function)

# =============================================================================
# Decode packed bytes to array of values
# =============================================================================
decode_packed_bytes() {
    local hex="$1"
    local scale="${2:-1}"  # 1 for labels, 1e18 for amounts

    # Remove 0x prefix
    hex="${hex#0x}"

    python3 << PYEOF
import sys
hex_data = "$hex"
scale = $scale

if not hex_data:
    print("[]")
    sys.exit(0)

try:
    data = bytes.fromhex(hex_data)
    values = []
    for i in range(0, len(data), 16):
        chunk = data[i:i+16]
        value = int.from_bytes(chunk, 'little')
        if scale > 1:
            value = value / scale
        else:
            value = int(value / 1e18)  # Labels are scaled by 1e18
        values.append(value)
    print(values)
except Exception as e:
    print("[]")
PYEOF
}

# =============================================================================
# Map label IDs to token symbols
# =============================================================================
labels_to_symbols() {
    local labels_json="$1"

    python3 << PYEOF
import json

labels = $labels_json
label_map = {
    101: "BTC", 102: "ETH", 103: "USDT", 104: "BNB", 105: "SOL",
    106: "XRP", 107: "USDC", 108: "ADA", 109: "AVAX", 110: "DOGE",
    111: "TRX", 112: "DOT", 113: "MATIC", 114: "LINK", 115: "UNI"
}

symbols = [label_map.get(int(l), f"ASSET_{int(l)}") for l in labels]
print(json.dumps(symbols))
PYEOF
}

# =============================================================================
# Fetch ITP metadata from chain
# =============================================================================
fetch_itp_metadata() {
    local vault_addr="$1"
    local index_id="$2"

    log "Fetching metadata for vault $vault_addr (index $index_id)..."

    # Basic metadata
    local name=$(cast call "$vault_addr" "name()(string)" --rpc-url "$RPC_URL" 2>/dev/null | tr -d '"' || echo "Unknown")
    local symbol=$(cast call "$vault_addr" "symbol()(string)" --rpc-url "$RPC_URL" 2>/dev/null | tr -d '"' || echo "???")
    local total_supply=$(cast call "$vault_addr" "totalSupply()(uint256)" --rpc-url "$RPC_URL" 2>/dev/null || echo "0")

    # Extended metadata
    local methodology=$(cast call "$vault_addr" "methodology()(string)" --rpc-url "$RPC_URL" 2>/dev/null | tr -d '"' || echo "")
    local description=$(cast call "$vault_addr" "description()(string)" --rpc-url "$RPC_URL" 2>/dev/null | tr -d '"' || echo "")
    local initial_price=$(cast call "$vault_addr" "initialPrice()(uint128)" --rpc-url "$RPC_URL" 2>/dev/null || echo "0")

    # Clean up initial_price (remove scientific notation)
    initial_price=$(echo "$initial_price" | sed 's/\[.*\]//' | tr -d ' ')

    # Fetch assets and weights from Castle
    local assets_hex=$(cast call "$CASTLE" "getIndexAssets(uint128)(bytes)" "$index_id" --rpc-url "$RPC_URL" 2>/dev/null || echo "")
    local weights_hex=$(cast call "$CASTLE" "getIndexWeights(uint128)(bytes)" "$index_id" --rpc-url "$RPC_URL" 2>/dev/null || echo "")

    # Decode assets and weights
    local asset_labels=$(decode_packed_bytes "$assets_hex" 1)
    local asset_symbols=$(labels_to_symbols "$asset_labels")
    local weights=$(decode_packed_bytes "$weights_hex" 1000000000000000000)

    if $VERBOSE; then
        echo "  Name: $name"
        echo "  Symbol: $symbol"
        echo "  Methodology: $methodology"
        echo "  Description: $description"
        echo "  Initial Price: $initial_price"
        echo "  Assets: $asset_symbols"
        echo "  Weights: $weights"
    fi

    # Insert/update in database
    psql -d "$DB_NAME" -q << SQLEOF
INSERT INTO itps (
    orbit_address, index_id, name, symbol,
    methodology, description, initial_price, total_supply,
    assets, weights, state, created_at, updated_at
) VALUES (
    '$vault_addr', $index_id, '$name', '$symbol',
    '$methodology', '$description', $initial_price, $total_supply,
    '$asset_symbols'::jsonb, '$weights'::jsonb, 1, NOW(), NOW()
)
ON CONFLICT (orbit_address) DO UPDATE SET
    name = EXCLUDED.name,
    symbol = EXCLUDED.symbol,
    methodology = EXCLUDED.methodology,
    description = EXCLUDED.description,
    initial_price = EXCLUDED.initial_price,
    total_supply = EXCLUDED.total_supply,
    assets = EXCLUDED.assets,
    weights = EXCLUDED.weights,
    index_id = EXCLUDED.index_id,
    updated_at = NOW();
SQLEOF

    success "Synced $symbol ($vault_addr)"
}

# =============================================================================
# Main
# =============================================================================
echo ""
echo "============================================"
echo "ITP Sync - Orbit Chain to Database"
echo "============================================"
echo ""
log "RPC:    $RPC_URL"
log "Castle: $CASTLE"
log "DB:     $DB_NAME"
echo ""

# Get vault count via VAULT_ROLE
VAULT_ROLE=$(cast keccak "Castle.VAULT_ROLE")
log "Querying vaults with VAULT_ROLE..."

VAULT_COUNT=$(cast call "$CASTLE" "getRoleAssigneeCount(bytes32)(uint256)" "$VAULT_ROLE" --rpc-url "$RPC_URL" 2>/dev/null || echo "0")

if [ "$VAULT_COUNT" == "0" ] || [ "$VAULT_COUNT" == "error" ]; then
    warn "No vaults found via VAULT_ROLE, trying known index IDs..."

    # Fallback: scan known index IDs
    for INDEX_ID in $(seq 1001 1020) 37330; do
        VAULT=$(cast call "$CASTLE" "getVault(uint128)(address)" "$INDEX_ID" --rpc-url "$RPC_URL" 2>/dev/null || echo "")

        if [ -n "$VAULT" ] && [ "$VAULT" != "0x0000000000000000000000000000000000000000" ]; then
            fetch_itp_metadata "$VAULT" "$INDEX_ID"
        fi
    done
else
    success "Found $VAULT_COUNT vault(s) via VAULT_ROLE"

    # Get vault addresses
    VAULT_ADDRESSES=$(cast call "$CASTLE" "getRoleAssignees(bytes32,uint256,uint256)(address[])" "$VAULT_ROLE" 0 "$VAULT_COUNT" --rpc-url "$RPC_URL" 2>/dev/null || echo "")

    # Parse addresses and fetch metadata
    echo "$VAULT_ADDRESSES" | tr -d '[]' | tr ',' '\n' | while read -r VAULT_ADDR; do
        VAULT_ADDR=$(echo "$VAULT_ADDR" | tr -d ' ')
        [ -z "$VAULT_ADDR" ] && continue

        # Get index ID for this vault
        INDEX_ID=$(cast call "$VAULT_ADDR" "indexId()(uint128)" --rpc-url "$RPC_URL" 2>/dev/null || echo "0")
        INDEX_ID=$(echo "$INDEX_ID" | sed 's/\[.*\]//' | tr -d ' ')

        fetch_itp_metadata "$VAULT_ADDR" "$INDEX_ID"
    done
fi

echo ""
log "Sync complete. Database contents:"
psql -d "$DB_NAME" -c "SELECT id, symbol, name, methodology, assets, weights FROM itps ORDER BY id;"

echo ""
success "Done!"
