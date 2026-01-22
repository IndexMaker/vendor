#!/bin/bash
# =============================================================================
# discover-itps.sh - Smart ITP Discovery via Castle ACL
# =============================================================================
# Discovers deployed ITPs using Castle's role-based access control.
# All vault contracts are granted CASTLE_VAULT_ROLE when created.
#
# Usage: ./discover-itps.sh [--sync-db] [--verbose]
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
SYNC_DB=false
VERBOSE=false

# Parse args
while [[ $# -gt 0 ]]; do
    case $1 in
        --sync-db) SYNC_DB=true; shift ;;
        --verbose) VERBOSE=true; shift ;;
        *) shift ;;
    esac
done

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m'

log() { echo -e "${BLUE}[INFO]${NC} $1"; }
success() { echo -e "${GREEN}[OK]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }

echo ""
echo "============================================"
echo "ITP Discovery via Castle ACL"
echo "============================================"
echo ""
log "RPC:    $RPC_URL"
log "Castle: $CASTLE"
echo ""

# =============================================================================
# Method 1: Query via Castle.getVault for known index IDs
# =============================================================================
echo "============================================"
echo "Method 1: Query known index IDs (1001-1020)"
echo "============================================"
echo ""

FOUND_VAULTS=()

for INDEX_ID in $(seq 1001 1020); do
    VAULT=$(cast call "$CASTLE" "getVault(uint128)(address)" "$INDEX_ID" --rpc-url "$RPC_URL" 2>/dev/null || echo "")

    if [ -n "$VAULT" ] && [ "$VAULT" != "0x0000000000000000000000000000000000000000" ]; then
        # Get vault metadata
        NAME=$(cast call "$VAULT" "name()(string)" --rpc-url "$RPC_URL" 2>/dev/null || echo "Unknown")
        SYMBOL=$(cast call "$VAULT" "symbol()(string)" --rpc-url "$RPC_URL" 2>/dev/null || echo "???")

        success "Index $INDEX_ID: $VAULT"
        echo "    Name:   $NAME"
        echo "    Symbol: $SYMBOL"

        if $VERBOSE; then
            SUPPLY=$(cast call "$VAULT" "totalSupply()(uint256)" --rpc-url "$RPC_URL" 2>/dev/null || echo "0")
            echo "    Supply: $SUPPLY"
        fi
        echo ""

        FOUND_VAULTS+=("$INDEX_ID:$VAULT:$NAME:$SYMBOL")
    fi
done

# =============================================================================
# Method 2: Query Castle ACL for VAULT_ROLE holders
# =============================================================================
echo "============================================"
echo "Method 2: Query Castle VAULT_ROLE via ACL"
echo "============================================"
echo ""

# CASTLE_VAULT_ROLE = keccak256("Castle.VAULT_ROLE")
VAULT_ROLE=$(cast keccak "Castle.VAULT_ROLE")
log "VAULT_ROLE hash: $VAULT_ROLE"

# Try to get role assignee count
ROLE_COUNT=$(cast call "$CASTLE" "getRoleAssigneeCount(bytes32)(uint256)" "$VAULT_ROLE" --rpc-url "$RPC_URL" 2>/dev/null || echo "error")

if [ "$ROLE_COUNT" != "error" ] && [ "$ROLE_COUNT" != "0" ]; then
    success "Found $ROLE_COUNT vault(s) via VAULT_ROLE"

    # Get vault addresses
    VAULT_ADDRESSES=$(cast call "$CASTLE" "getRoleAssignees(bytes32,uint256,uint256)(address[])" "$VAULT_ROLE" 0 "$ROLE_COUNT" --rpc-url "$RPC_URL" 2>/dev/null || echo "")

    if [ -n "$VAULT_ADDRESSES" ]; then
        echo "Vault addresses:"
        echo "$VAULT_ADDRESSES"
    fi
else
    warn "getRoleAssigneeCount returned: $ROLE_COUNT"
    log "Trying alternative role hashes..."

    # Try alternative role formats
    for ROLE_NAME in "Castle.VAULT_ROLE" "VAULT_ROLE" "vault" "Vault"; do
        ROLE_HASH=$(cast keccak "$ROLE_NAME" 2>/dev/null || continue)
        COUNT=$(cast call "$CASTLE" "getRoleAssigneeCount(bytes32)(uint256)" "$ROLE_HASH" --rpc-url "$RPC_URL" 2>/dev/null || echo "0")
        if [ "$COUNT" != "0" ] && [ "$COUNT" != "error" ]; then
            success "Found $COUNT assignee(s) for role '$ROLE_NAME'"
        fi
    done
fi

# =============================================================================
# Method 3: Check events for IndexCreated
# =============================================================================
echo ""
echo "============================================"
echo "Method 3: Check IndexCreated events"
echo "============================================"
echo ""

# IndexCreated event signature - need to find the correct one
# Try to get recent logs from Castle
log "Fetching Castle events..."

EVENTS=$(cast logs --from-block 0 --address "$CASTLE" --rpc-url "$RPC_URL" 2>/dev/null | head -100)

if [ -n "$EVENTS" ]; then
    EVENT_COUNT=$(echo "$EVENTS" | grep -c "transactionHash" || echo "0")
    success "Found $EVENT_COUNT event(s) on Castle"

    if $VERBOSE; then
        echo "$EVENTS" | head -50
    fi
else
    warn "No events found or query failed"
fi

# =============================================================================
# Method 4: Check global.env for TEST_ITP_ADDRESS
# =============================================================================
echo ""
echo "============================================"
echo "Method 4: Check global.env"
echo "============================================"
echo ""

if grep -q "TEST_ITP_ADDRESS=" "$PROJECT_ROOT/global.env" 2>/dev/null; then
    TEST_ITP=$(grep "TEST_ITP_ADDRESS=" "$PROJECT_ROOT/global.env" | cut -d= -f2)
    if [ -n "$TEST_ITP" ] && [ "$TEST_ITP" != "" ]; then
        success "Found TEST_ITP_ADDRESS: $TEST_ITP"

        # Verify it exists
        NAME=$(cast call "$TEST_ITP" "name()(string)" --rpc-url "$RPC_URL" 2>/dev/null || echo "")
        if [ -n "$NAME" ]; then
            SYMBOL=$(cast call "$TEST_ITP" "symbol()(string)" --rpc-url "$RPC_URL" 2>/dev/null || echo "")
            INDEX_ID=$(cast call "$TEST_ITP" "indexId()(uint128)" --rpc-url "$RPC_URL" 2>/dev/null || echo "")
            success "Verified: $NAME ($SYMBOL) - Index ID: $INDEX_ID"

            # Add to found vaults if not already there
            FOUND=false
            for v in "${FOUND_VAULTS[@]}"; do
                if [[ "$v" == *"$TEST_ITP"* ]]; then
                    FOUND=true
                    break
                fi
            done
            if ! $FOUND; then
                FOUND_VAULTS+=("$INDEX_ID:$TEST_ITP:$NAME:$SYMBOL")
            fi
        else
            warn "TEST_ITP_ADDRESS exists but contract not responding"
        fi
    fi
else
    log "No TEST_ITP_ADDRESS in global.env"
fi

# =============================================================================
# Summary
# =============================================================================
echo ""
echo "============================================"
echo "Discovery Summary"
echo "============================================"
echo ""

if [ ${#FOUND_VAULTS[@]} -eq 0 ]; then
    warn "No ITPs discovered on Orbit chain"
    echo ""
    echo "The create-test-itp.sh script may not have been run yet."
    echo "Run: ./vendor/scripts/create-test-itp.sh"
else
    success "Found ${#FOUND_VAULTS[@]} ITP(s):"
    echo ""

    for vault in "${FOUND_VAULTS[@]}"; do
        IFS=':' read -r idx addr name symbol <<< "$vault"
        echo "  Index $idx: $symbol ($name)"
        echo "    Address: $addr"
        echo ""
    done
fi

# =============================================================================
# Sync to Database (if requested)
# =============================================================================
if $SYNC_DB && [ ${#FOUND_VAULTS[@]} -gt 0 ]; then
    echo "============================================"
    echo "Syncing to Database"
    echo "============================================"
    echo ""

    for vault in "${FOUND_VAULTS[@]}"; do
        IFS=':' read -r idx addr name symbol <<< "$vault"

        log "Inserting $symbol ($addr)..."

        # Insert into database
        psql -d indexmaker_db -c "
            INSERT INTO itps (orbit_address, name, symbol, state, created_at, updated_at)
            VALUES ('$addr', '$name', '$symbol', 1, NOW(), NOW())
            ON CONFLICT (orbit_address) DO UPDATE SET
                name = EXCLUDED.name,
                symbol = EXCLUDED.symbol,
                updated_at = NOW();
        " 2>/dev/null && success "Inserted $symbol" || warn "Failed to insert $symbol"
    done

    echo ""
    log "Database sync complete"
fi

echo ""
echo "Done."
