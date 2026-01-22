#!/usr/bin/env bash
# =============================================================================
# deploy-new-vendor.sh - Deploy a new vendor with all 627 Bitget pairs
# =============================================================================
# This script:
# 1. Grants VENDOR_ROLE to the new vendor address
# 2. Registers all 627 assets via submitAssets (from NEW_VENDOR wallet)
#
# Prerequisites:
#   - NEW_VENDOR private key in global.env
#   - CASTLE_ADDRESS in global.env
#   - cast (foundry) installed
#   - Python3 installed (for vector_to_bytes.py)
# =============================================================================

set -e

# =============================================================================
# Configuration
# =============================================================================
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
VAULTWORKS_DIR="$PROJECT_ROOT/vaultworks"
VENDOR_DIR="$PROJECT_ROOT/vendor"
GLOBAL_ENV="$PROJECT_ROOT/global.env"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

log_info()    { echo -e "${BLUE}[INFO]${NC} $1"; }
log_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
log_warn()    { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error()   { echo -e "${RED}[ERROR]${NC} $1" >&2; }
log_step()    { echo -e "${CYAN}[STEP]${NC} $1"; }
die()         { log_error "$1"; exit 1; }

# =============================================================================
# Environment Loading
# =============================================================================
load_environment() {
    log_info "Loading environment..."

    # Load from global.env
    if [ -f "$GLOBAL_ENV" ]; then
        set -a
        source "$GLOBAL_ENV"
        set +a
    else
        die "global.env not found at $GLOBAL_ENV"
    fi

    # Set RPC URL
    export RPC_URL="${RPC_URL:-${ORBIT_RPC_URL:-https://index.rpc.zeeve.net}}"

    # Validate required variables
    if [ -z "$NEW_VENDOR" ]; then
        die "NEW_VENDOR private key not found in global.env"
    fi

    if [ -z "$CASTLE_ADDRESS" ]; then
        die "CASTLE_ADDRESS not found in global.env"
    fi

    # Get admin key for granting role
    if [ -n "$DEPLOY_PRIVATE_KEY" ]; then
        ADMIN_KEY="$DEPLOY_PRIVATE_KEY"
    elif [ -n "$TESTNET_PRIVATE_KEY" ]; then
        ADMIN_KEY="$TESTNET_PRIVATE_KEY"
    else
        die "No admin private key found (DEPLOY_PRIVATE_KEY or TESTNET_PRIVATE_KEY)"
    fi

    # Normalize NEW_VENDOR key (add 0x if missing)
    if [[ "$NEW_VENDOR" != 0x* ]]; then
        NEW_VENDOR_KEY="0x$NEW_VENDOR"
    else
        NEW_VENDOR_KEY="$NEW_VENDOR"
    fi

    # Get addresses
    ADMIN_ADDRESS=$(cast wallet address "$ADMIN_KEY")
    NEW_VENDOR_ADDRESS=$(cast wallet address "$NEW_VENDOR_KEY")

    log_info "RPC: $RPC_URL"
    log_info "Castle: $CASTLE_ADDRESS"
    log_info "Admin: $ADMIN_ADDRESS"
    log_info "New Vendor: $NEW_VENDOR_ADDRESS"
}

# =============================================================================
# Helper Functions
# =============================================================================

# Send transaction from admin wallet
admin_send() {
    local address=$1
    local function_sig=$2
    shift 2
    local args=("$@")

    log_step "Admin sending: $function_sig"
    cast send --rpc-url "$RPC_URL" \
        --private-key "$ADMIN_KEY" \
        "$address" "$function_sig" "${args[@]}"
}

# Send transaction from new vendor wallet
vendor_send() {
    local address=$1
    local function_sig=$2
    shift 2
    local args=("$@")

    log_step "Vendor sending: $function_sig"
    cast send --rpc-url "$RPC_URL" \
        --private-key "$NEW_VENDOR_KEY" \
        "$address" "$function_sig" "${args[@]}"
}

# Call contract (read-only)
contract_call() {
    local address=$1
    local function_sig=$2
    shift 2
    local args=("$@")

    cast call --rpc-url "$RPC_URL" "$address" "$function_sig" "${args[@]}"
}

# =============================================================================
# Step 1: Grant VENDOR_ROLE to new vendor
# =============================================================================
grant_vendor_role() {
    log_info "Granting VENDOR_ROLE to new vendor..."

    # Calculate VENDOR_ROLE hash: keccak256("Castle.VENDOR_ROLE")
    VENDOR_ROLE=$(cast keccak "Castle.VENDOR_ROLE")
    log_info "VENDOR_ROLE hash: $VENDOR_ROLE"

    # Check if already has role
    local has_role
    has_role=$(contract_call "$CASTLE_ADDRESS" "hasRole(bytes32,address)(bool)" "$VENDOR_ROLE" "$NEW_VENDOR_ADDRESS")

    if [ "$has_role" = "true" ]; then
        log_success "New vendor already has VENDOR_ROLE"
        return 0
    fi

    # Grant role
    admin_send "$CASTLE_ADDRESS" "grantRole(bytes32,address)" "$VENDOR_ROLE" "$NEW_VENDOR_ADDRESS"

    # Verify
    has_role=$(contract_call "$CASTLE_ADDRESS" "hasRole(bytes32,address)(bool)" "$VENDOR_ROLE" "$NEW_VENDOR_ADDRESS")
    if [ "$has_role" = "true" ]; then
        log_success "VENDOR_ROLE granted successfully"
    else
        die "Failed to grant VENDOR_ROLE"
    fi
}

# =============================================================================
# Step 2: Register all 627 assets via submitAssets
# =============================================================================
register_assets() {
    log_info "Registering all 627 Bitget assets..."

    # Generate asset IDs 1-627 as JSON array
    local asset_ids="["
    for i in $(seq 1 627); do
        if [ $i -gt 1 ]; then
            asset_ids+=","
        fi
        asset_ids+="$i"
    done
    asset_ids+="]"

    log_info "Encoding 627 asset IDs to bytes..."

    # Use vector_to_bytes.py to encode asset IDs (scales by 10^18)
    local encoded_assets
    encoded_assets=$(python3 "$VAULTWORKS_DIR/scripts/vector_to_bytes.py" "$asset_ids")

    if [ -z "$encoded_assets" ] || [ "$encoded_assets" = "0x" ]; then
        die "Failed to encode asset IDs"
    fi

    log_info "Encoded bytes length: ${#encoded_assets} chars"

    # Determine vendor ID
    # Vendor ID 1 is already used by the old vendor, so use ID 2 for the new vendor
    # Each vendor address can only control their own vendor ID (set on first submitAssets call)
    NEW_VENDOR_ID=2
    log_info "Using vendor ID: $NEW_VENDOR_ID (vendor ID 1 is already in use)"
    log_info "New vendor will be ID: $NEW_VENDOR_ID"

    # Call submitAssets from new vendor wallet
    log_info "Calling submitAssets with 627 assets for vendor ID $NEW_VENDOR_ID..."
    vendor_send "$CASTLE_ADDRESS" "submitAssets(uint128,bytes)" "$NEW_VENDOR_ID" "$encoded_assets"

    log_success "All 627 assets registered for vendor ID $NEW_VENDOR_ID"
}

# =============================================================================
# Step 3: Verify registration
# =============================================================================
verify_registration() {
    log_info "Verifying asset registration..."

    # Get vendor assets
    local vendor_assets
    vendor_assets=$(contract_call "$CASTLE_ADDRESS" "getVendorAssets(uint128)(bytes)" "$NEW_VENDOR_ID" 2>/dev/null)

    if [ -n "$vendor_assets" ] && [ "$vendor_assets" != "0x" ]; then
        # Count assets (each asset is 16 bytes = 32 hex chars)
        local hex_len=${#vendor_assets}
        local asset_count=$(( (hex_len - 2) / 32 ))  # -2 for 0x prefix
        log_success "Vendor $NEW_VENDOR_ID has $asset_count assets registered"
    else
        log_warn "Could not verify asset count (getVendorAssets returned empty)"
    fi
}

# =============================================================================
# Step 4: Update global.env with new vendor ID
# =============================================================================
update_config() {
    log_info "Updating configuration..."

    # Update VENDOR_ID in global.env
    if grep -q "^VENDOR_ID=" "$GLOBAL_ENV"; then
        sed -i.bak "s/^VENDOR_ID=.*/VENDOR_ID=$NEW_VENDOR_ID/" "$GLOBAL_ENV"
        rm -f "$GLOBAL_ENV.bak"
        log_success "Updated VENDOR_ID to $NEW_VENDOR_ID in global.env"
    else
        echo "VENDOR_ID=$NEW_VENDOR_ID" >> "$GLOBAL_ENV"
        log_success "Added VENDOR_ID=$NEW_VENDOR_ID to global.env"
    fi

    # Add NEW_VENDOR_ADDRESS if not present
    if ! grep -q "^NEW_VENDOR_ADDRESS=" "$GLOBAL_ENV"; then
        echo "NEW_VENDOR_ADDRESS=$NEW_VENDOR_ADDRESS" >> "$GLOBAL_ENV"
        log_success "Added NEW_VENDOR_ADDRESS to global.env"
    fi
}

# =============================================================================
# Main
# =============================================================================
main() {
    echo "=============================================="
    echo "  Deploy New Vendor with 627 Bitget Pairs"
    echo "=============================================="

    load_environment
    grant_vendor_role
    register_assets
    verify_registration
    update_config

    echo ""
    echo "=============================================="
    echo "  Deployment Complete!"
    echo "=============================================="
    echo "  New Vendor Address: $NEW_VENDOR_ADDRESS"
    echo "  New Vendor ID: $NEW_VENDOR_ID"
    echo "  Assets Registered: 627"
    echo ""
    echo "  Next steps:"
    echo "  1. Update keeper.json with vendor_id: $NEW_VENDOR_ID"
    echo "  2. Restart vendor service"
    echo "  3. Test order processing"
    echo "=============================================="
}

main "$@"
