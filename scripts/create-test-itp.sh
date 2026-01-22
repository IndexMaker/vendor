#!/bin/bash
# =============================================================================
# create-test-itp.sh - Test ITP Creation Script
# =============================================================================
# Creates a test ITP index by replicating vaultworks Scenario 5 in bash.
# Handles all prerequisites: assets, margin, supply, weights, vote,
# market data, and quote update.
#
# Usage: ./create-test-itp.sh [--keeper ADDRESS] [--vendor-id ID] [--index-id ID]
#
# Prerequisites:
#   - DEPLOY_PRIVATE_KEY set
#   - CASTLE_ADDRESS set
#   - RPC_URL or ORBIT_RPC set
#   - FAKE_USDC_ORBIT_ADDRESS or COLLATERAL_ADDRESS set
#   - Vault prototype must be set (deploy.sh should have done this)
#   - Required roles granted (ISSUER, KEEPER, VENDOR, VAULT)
# =============================================================================

set -e

# =============================================================================
# Configuration
# =============================================================================
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
VAULTWORKS_DIR="$PROJECT_ROOT/vaultworks"
GLOBAL_ENV="$PROJECT_ROOT/global.env"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
log_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1" >&2; }
die() { log_error "$1"; exit 1; }

# =============================================================================
# Byte Encoding Helpers
# =============================================================================
# Encode a list of label IDs (integers) to packed bytes (128-bit little-endian)
encode_labels() {
    local labels="$1"
    python3 "$VAULTWORKS_DIR/scripts/vector_to_bytes.py" "$labels"
}

# Encode a list of amounts (decimals) to packed bytes (128-bit little-endian, 18 decimals)
encode_amounts() {
    local amounts="$1"
    python3 "$VAULTWORKS_DIR/scripts/vector_to_bytes.py" "$amounts"
}

# =============================================================================
# Environment Loading
# =============================================================================
load_env() {
    if [ -f "$PROJECT_ROOT/.env" ]; then
        set -a
        source "$PROJECT_ROOT/.env"
        set +a
    fi

    if [ -f "$GLOBAL_ENV" ]; then
        set -a
        source "$GLOBAL_ENV"
        set +a
    fi

    if [ -f "$PROJECT_ROOT/bridge/.env" ]; then
        set -a
        source "$PROJECT_ROOT/bridge/.env"
        set +a
    fi

    # RPC URL
    export RPC_URL="${RPC_URL:-${ORBIT_RPC:-${ORBIT_RPC_URL:-https://index.rpc.zeeve.net}}}"

    # Private key
    if [ -z "$DEPLOY_PRIVATE_KEY" ]; then
        DEPLOY_PRIVATE_KEY="${ORBIT_PRIVATE_KEY:-$TESTNET_PRIVATE_KEY}"
    fi
    if [ -z "$DEPLOY_PRIVATE_KEY" ]; then
        die "DEPLOY_PRIVATE_KEY (or ORBIT_PRIVATE_KEY) not set"
    fi

    # Castle address
    if [ -z "$CASTLE_ADDRESS" ]; then
        die "CASTLE_ADDRESS not set. Run deploy.sh first"
    fi

    # Collateral asset
    COLLATERAL_ASSET="${FAKE_USDC_ORBIT_ADDRESS:-${COLLATERAL_ADDRESS:-}}"
    if [ -z "$COLLATERAL_ASSET" ]; then
        die "FAKE_USDC_ORBIT_ADDRESS or COLLATERAL_ADDRESS not set"
    fi

    # Custody address (use CUSTODY_ADDRESS if set, else Castle)
    CUSTODY_ADDRESS="${CUSTODY_ADDRESS:-$CASTLE_ADDRESS}"

    # Deployer address
    DEPLOYER_ADDRESS=$(cast wallet address "$DEPLOY_PRIVATE_KEY")

    # Configurable parameters (can be overridden via CLI)
    VENDOR_ID="${VENDOR_ID:-1}"
    INDEX_ID="${INDEX_ID:-1001}"
    KEEPER_ADDRESS="${KEEPER_ADDRESS:-$DEPLOYER_ADDRESS}"

    log_info "Configuration:"
    log_info "  RPC URL:     $RPC_URL"
    log_info "  Castle:      $CASTLE_ADDRESS"
    log_info "  Collateral:  $COLLATERAL_ASSET"
    log_info "  Custody:     $CUSTODY_ADDRESS"
    log_info "  Deployer:    $DEPLOYER_ADDRESS"
    log_info "  Keeper:      $KEEPER_ADDRESS"
    log_info "  Vendor ID:   $VENDOR_ID"
    log_info "  Index ID:    $INDEX_ID"
}

# =============================================================================
# Check Prerequisites
# =============================================================================
check_prerequisites() {
    log_info "Checking prerequisites..."

    # Check Python3
    if ! command -v python3 &> /dev/null; then
        die "python3 not found. Required for byte encoding."
    fi

    # Check vector_to_bytes.py exists
    if [ ! -f "$VAULTWORKS_DIR/scripts/vector_to_bytes.py" ]; then
        die "vaultworks/scripts/vector_to_bytes.py not found"
    fi

    # Check if vault prototype is set
    log_info "Checking vault prototype..."
    VAULT_PROTOTYPE=$(cast call "$CASTLE_ADDRESS" "getVaultPrototype()(address)" --rpc-url "$RPC_URL" 2>/dev/null) || true

    if [ -z "$VAULT_PROTOTYPE" ] || [ "$VAULT_PROTOTYPE" = "0x0000000000000000000000000000000000000000" ]; then
        log_warn "Vault prototype not set!"

        # Try to set it automatically if VAULT_ADDRESS is available
        if [ -n "$VAULT_ADDRESS" ] && [ "$VAULT_ADDRESS" != "0x0000000000000000000000000000000000000000" ]; then
            log_info "Attempting to set vault prototype to $VAULT_ADDRESS..."

            if cast send "$CASTLE_ADDRESS" "setVaultPrototype(address)" "$VAULT_ADDRESS" \
                --rpc-url "$RPC_URL" --private-key "$DEPLOY_PRIVATE_KEY" > /dev/null 2>&1; then
                log_success "Vault prototype set to $VAULT_ADDRESS"
                VAULT_PROTOTYPE="$VAULT_ADDRESS"
            else
                log_error "Failed to set vault prototype. You may need ADMIN_ROLE."
                log_error "Manual fix: cast send \$CASTLE_ADDRESS \"setVaultPrototype(address)\" \$VAULT_ADDRESS"
                die "Vault prototype required"
            fi
        else
            log_error "VAULT_ADDRESS not set in environment. Run deploy.sh first."
            die "Vault prototype required"
        fi
    else
        log_success "Vault prototype: $VAULT_PROTOTYPE"
    fi

    # Check required roles
    log_info "Checking required roles..."

    # Get role hashes
    ISSUER_ROLE=$(cast call "$CASTLE_ADDRESS" "getIssuerRole()(bytes32)" --rpc-url "$RPC_URL" 2>/dev/null) || true
    VENDOR_ROLE=$(cast call "$CASTLE_ADDRESS" "getVendorRole()(bytes32)" --rpc-url "$RPC_URL" 2>/dev/null) || true
    KEEPER_ROLE=$(cast call "$CASTLE_ADDRESS" "getKeeperRole()(bytes32)" --rpc-url "$RPC_URL" 2>/dev/null) || true

    # Check if deployer has required roles
    local has_issuer has_vendor has_keeper
    has_issuer=$(cast call "$CASTLE_ADDRESS" "hasRole(bytes32,address)(bool)" "$ISSUER_ROLE" "$DEPLOYER_ADDRESS" --rpc-url "$RPC_URL" 2>/dev/null) || true
    has_vendor=$(cast call "$CASTLE_ADDRESS" "hasRole(bytes32,address)(bool)" "$VENDOR_ROLE" "$DEPLOYER_ADDRESS" --rpc-url "$RPC_URL" 2>/dev/null) || true
    has_keeper=$(cast call "$CASTLE_ADDRESS" "hasRole(bytes32,address)(bool)" "$KEEPER_ROLE" "$DEPLOYER_ADDRESS" --rpc-url "$RPC_URL" 2>/dev/null) || true

    log_info "  ISSUER_ROLE: $has_issuer"
    log_info "  VENDOR_ROLE: $has_vendor"
    log_info "  KEEPER_ROLE: $has_keeper"

    # Warn but don't fail if roles are missing (they might be granted via other means)
    if [ "$has_issuer" != "true" ]; then
        log_warn "Deployer missing ISSUER_ROLE - submitIndex may fail"
    fi
    if [ "$has_vendor" != "true" ]; then
        log_warn "Deployer missing VENDOR_ROLE - submitAssets/Margin/Supply may fail"
    fi
    if [ "$has_keeper" != "true" ]; then
        log_warn "Deployer missing KEEPER_ROLE - updateIndexQuote may fail"
    fi

    log_success "Prerequisites OK"
}

# =============================================================================
# Step 1: Submit Assets
# =============================================================================
submit_assets() {
    log_info "[1/8] Submitting vendor assets..."

    # Asset set 1: labels 101, 102, 104, 105, 106
    local ASSETS_1=$(encode_labels "[101, 102, 104, 105, 106]")
    log_info "  Submitting asset set 1: [101, 102, 104, 105, 106]"
    cast send "$CASTLE_ADDRESS" \
        "submitAssets(uint128,bytes)" \
        "$VENDOR_ID" "$ASSETS_1" \
        --rpc-url "$RPC_URL" \
        --private-key "$DEPLOY_PRIVATE_KEY" \
        > /dev/null

    # Asset set 2: labels 102, 103, 107, 108, 109
    local ASSETS_2=$(encode_labels "[102, 103, 107, 108, 109]")
    log_info "  Submitting asset set 2: [102, 103, 107, 108, 109]"
    cast send "$CASTLE_ADDRESS" \
        "submitAssets(uint128,bytes)" \
        "$VENDOR_ID" "$ASSETS_2" \
        --rpc-url "$RPC_URL" \
        --private-key "$DEPLOY_PRIVATE_KEY" \
        > /dev/null

    log_success "[1/8] Assets submitted"
}

# =============================================================================
# Step 2: Submit Margin
# =============================================================================
submit_margin() {
    log_info "[2/8] Submitting trading margin..."

    # All 9 assets with margin 2.0 each
    local ASSET_NAMES=$(encode_labels "[101, 102, 103, 104, 105, 106, 107, 108, 109]")
    local ASSET_MARGIN=$(encode_amounts "[2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0]")

    cast send "$CASTLE_ADDRESS" \
        "submitMargin(uint128,bytes,bytes)" \
        "$VENDOR_ID" "$ASSET_NAMES" "$ASSET_MARGIN" \
        --rpc-url "$RPC_URL" \
        --private-key "$DEPLOY_PRIVATE_KEY" \
        > /dev/null

    log_success "[2/8] Margin submitted"
}

# =============================================================================
# Step 3: Submit Supply
# =============================================================================
submit_supply() {
    log_info "[3/8] Submitting vendor supply..."

    local ASSET_NAMES=$(encode_labels "[101, 102, 103, 104, 105, 106, 107, 108, 109]")
    local ASSET_SHORT=$(encode_amounts "[0, 0, 0, 0, 0, 0, 0, 0, 0]")
    local ASSET_LONG=$(encode_amounts "[1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]")

    cast send "$CASTLE_ADDRESS" \
        "submitSupply(uint128,bytes,bytes,bytes)" \
        "$VENDOR_ID" "$ASSET_NAMES" "$ASSET_SHORT" "$ASSET_LONG" \
        --rpc-url "$RPC_URL" \
        --private-key "$DEPLOY_PRIVATE_KEY" \
        > /dev/null

    log_success "[3/8] Supply submitted"
}

# =============================================================================
# Step 4: Submit Index (Create ITP)
# =============================================================================
submit_index() {
    log_info "[4/8] Creating index (submitIndex)..."

    local INDEX_NAME="Test 5 Assets"
    local INDEX_SYMBOL="T5D"
    local DESCRIPTION="Test Index containing five assets"
    local METHODOLOGY="Testing"
    local INITIAL_PRICE="1000000000000000000000"  # 1000 * 10^18
    local MAX_ORDER_SIZE="100000000000000000000"  # 100 * 10^18
    local CUSTODY_STRING="Test Custody"

    log_info "  Name: $INDEX_NAME"
    log_info "  Symbol: $INDEX_SYMBOL"
    log_info "  Index ID: $INDEX_ID"

    TX_OUTPUT=$(cast send "$CASTLE_ADDRESS" \
        "submitIndex(uint128,uint128,string,string,string,string,uint128,address,string,address[],address,address,uint128)" \
        "$VENDOR_ID" \
        "$INDEX_ID" \
        "$INDEX_NAME" \
        "$INDEX_SYMBOL" \
        "$DESCRIPTION" \
        "$METHODOLOGY" \
        "$INITIAL_PRICE" \
        "$DEPLOYER_ADDRESS" \
        "$CUSTODY_STRING" \
        "[$KEEPER_ADDRESS]" \
        "$CUSTODY_ADDRESS" \
        "$COLLATERAL_ASSET" \
        "$MAX_ORDER_SIZE" \
        --rpc-url "$RPC_URL" \
        --private-key "$DEPLOY_PRIVATE_KEY" \
        2>&1) || {
            log_error "submitIndex failed!"
            echo "$TX_OUTPUT"
            die "Index creation failed"
        }

    log_success "[4/8] Index created"
}

# =============================================================================
# Step 5: Submit Asset Weights
# =============================================================================
submit_asset_weights() {
    log_info "[5/8] Submitting asset weights..."

    # 5 assets with weights: 1.0, 0.5, 0.5, 0.5, 1.5
    local ASSET_NAMES=$(encode_labels "[102, 103, 104, 106, 107]")
    local ASSET_WEIGHTS=$(encode_amounts "[1.0, 0.5, 0.5, 0.5, 1.5]")

    cast send "$CASTLE_ADDRESS" \
        "submitAssetWeights(uint128,bytes,bytes)" \
        "$INDEX_ID" "$ASSET_NAMES" "$ASSET_WEIGHTS" \
        --rpc-url "$RPC_URL" \
        --private-key "$DEPLOY_PRIVATE_KEY" \
        > /dev/null

    log_success "[5/8] Asset weights submitted"
}

# =============================================================================
# Step 6: Submit Vote
# =============================================================================
submit_vote() {
    log_info "[6/8] Submitting vote..."

    # Empty vote bytes
    local VOTE="0x"

    cast send "$CASTLE_ADDRESS" \
        "submitVote(uint128,bytes)" \
        "$INDEX_ID" "$VOTE" \
        --rpc-url "$RPC_URL" \
        --private-key "$DEPLOY_PRIVATE_KEY" \
        > /dev/null

    log_success "[6/8] Vote submitted"
}

# =============================================================================
# Step 7: Submit Market Data
# =============================================================================
submit_market_data() {
    log_info "[7/8] Submitting market data..."

    local ASSET_NAMES=$(encode_labels "[102, 103, 104, 106, 107]")
    local ASSET_LIQUIDITY=$(encode_amounts "[0.5, 0.5, 0.5, 0.5, 0.5]")
    local ASSET_PRICES=$(encode_amounts "[100.0, 50.0, 20.0, 10.0, 1.0]")
    local ASSET_SLOPES=$(encode_amounts "[1.0, 0.5, 0.2, 0.1, 0.01]")

    cast send "$CASTLE_ADDRESS" \
        "submitMarketData(uint128,bytes,bytes,bytes,bytes)" \
        "$VENDOR_ID" "$ASSET_NAMES" "$ASSET_LIQUIDITY" "$ASSET_PRICES" "$ASSET_SLOPES" \
        --rpc-url "$RPC_URL" \
        --private-key "$DEPLOY_PRIVATE_KEY" \
        > /dev/null

    log_success "[7/8] Market data submitted"
}

# =============================================================================
# Step 8: Update Index Quote
# =============================================================================
update_index_quote() {
    log_info "[8/8] Updating index quote..."

    cast send "$CASTLE_ADDRESS" \
        "updateIndexQuote(uint128,uint128)" \
        "$VENDOR_ID" "$INDEX_ID" \
        --rpc-url "$RPC_URL" \
        --private-key "$DEPLOY_PRIVATE_KEY" \
        > /dev/null

    log_success "[8/8] Index quote updated"
}

# =============================================================================
# Get Vault Address
# =============================================================================
get_vault_address() {
    log_info "Getting vault address for index $INDEX_ID..."

    VAULT_ADDRESS=$(cast call "$CASTLE_ADDRESS" \
        "getVault(uint128)(address)" \
        "$INDEX_ID" \
        --rpc-url "$RPC_URL" 2>/dev/null) || {
            log_warn "Could not get vault address via Castle"
            return 1
        }

    if [ -z "$VAULT_ADDRESS" ] || [ "$VAULT_ADDRESS" = "0x0000000000000000000000000000000000000000" ]; then
        log_warn "Vault address is zero"
        return 1
    fi

    log_success "Vault address: $VAULT_ADDRESS"
    export TEST_ITP_ADDRESS="$VAULT_ADDRESS"

    # Save to global.env
    if ! grep -q "TEST_ITP_ADDRESS=" "$GLOBAL_ENV" 2>/dev/null; then
        echo "" >> "$GLOBAL_ENV"
        echo "# Test ITP created by create-test-itp.sh" >> "$GLOBAL_ENV"
        echo "TEST_ITP_ADDRESS=$VAULT_ADDRESS" >> "$GLOBAL_ENV"
        log_info "TEST_ITP_ADDRESS added to global.env"
    else
        sed -i.bak "s|TEST_ITP_ADDRESS=.*|TEST_ITP_ADDRESS=$VAULT_ADDRESS|" "$GLOBAL_ENV"
        rm -f "$GLOBAL_ENV.bak"
        log_info "TEST_ITP_ADDRESS updated in global.env"
    fi
}

# =============================================================================
# Verify ITP
# =============================================================================
verify_itp() {
    log_info "Verifying ITP..."

    if [ -z "$TEST_ITP_ADDRESS" ]; then
        log_warn "No TEST_ITP_ADDRESS to verify"
        return 0
    fi

    SYMBOL=$(cast call "$TEST_ITP_ADDRESS" "symbol()(string)" --rpc-url "$RPC_URL" 2>/dev/null) || true
    NAME=$(cast call "$TEST_ITP_ADDRESS" "name()(string)" --rpc-url "$RPC_URL" 2>/dev/null) || true
    VAULT_INDEX_ID=$(cast call "$TEST_ITP_ADDRESS" "indexId()(uint128)" --rpc-url "$RPC_URL" 2>/dev/null) || true

    log_success "ITP Verified:"
    log_success "  Address:  $TEST_ITP_ADDRESS"
    log_success "  Symbol:   $SYMBOL"
    log_success "  Name:     $NAME"
    log_success "  Index ID: $VAULT_INDEX_ID"
}

# =============================================================================
# Parse CLI Arguments
# =============================================================================
parse_args() {
    while [[ $# -gt 0 ]]; do
        case $1 in
            --keeper)
                KEEPER_ADDRESS="$2"
                shift 2
                ;;
            --vendor-id)
                VENDOR_ID="$2"
                shift 2
                ;;
            --index-id)
                INDEX_ID="$2"
                shift 2
                ;;
            --help|-h)
                echo "Usage: $0 [--keeper ADDRESS] [--vendor-id ID] [--index-id ID]"
                echo ""
                echo "Creates a test ITP by executing all Scenario 5 steps."
                echo ""
                echo "Options:"
                echo "  --keeper ADDRESS    Keeper/operator address (default: deployer)"
                echo "  --vendor-id ID      Vendor ID (default: 1)"
                echo "  --index-id ID       Index ID (default: 1001)"
                exit 0
                ;;
            *)
                shift
                ;;
        esac
    done
}

# =============================================================================
# Main
# =============================================================================
main() {
    echo ""
    echo "=============================================="
    echo "  Test ITP Creation (Scenario 5 in Bash)"
    echo "=============================================="
    echo ""

    parse_args "$@"
    load_env
    check_prerequisites

    echo ""
    log_info "Running Scenario 5 steps..."
    echo ""

    submit_assets
    submit_margin
    submit_supply
    submit_index
    submit_asset_weights
    submit_vote
    submit_market_data
    update_index_quote

    echo ""
    get_vault_address
    verify_itp

    echo ""
    log_success "Test ITP creation complete!"
    echo ""
    echo "Index $INDEX_ID: Vault deployed at: $TEST_ITP_ADDRESS"
    echo ""
    echo "Next steps:"
    echo "  - Run full-flow-test.sh --skip-deploy to verify buy/sell flow"
    echo "  - TEST_ITP_ADDRESS is saved in global.env"
}

main "$@"
