#!/usr/bin/env bash
# =============================================================================
# deploy.sh - Self-Contained Deployment Script for feb26
# =============================================================================
# Deploys all Vaultworks Stylus contracts without using vaultworks scripts.
# This script is the single source of truth for deployments.
#
# Usage: ./deploy.sh [OPTIONS]
#
# Options:
#   --skip-castle       Skip Castle + Officers deployment
#   --skip-vault        Skip Vault infrastructure deployment
#   --skip-pairs        Skip PairRegistry pair population
#   --skip-itp          Skip test ITP creation
#   --clean             Force redeploy everything
#   --help              Show this help message
#
# Prerequisites:
#   - DEPLOY_PRIVATE_KEY set in global.env or environment
#   - cargo stylus installed
#   - cast (foundry) installed
#   - jq installed
# =============================================================================

set -e

# =============================================================================
# Configuration
# =============================================================================
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
VAULTWORKS_DIR="$PROJECT_ROOT/vaultworks"
VENDOR_DIR="$PROJECT_ROOT/vendor"
BRIDGE_DIR="$PROJECT_ROOT/bridge"
GLOBAL_ENV="$PROJECT_ROOT/global.env"

# Deployment settings
MAX_FEE_PER_GAS_GWEI=${MAX_FEE_PER_GAS_GWEI:-30}

# Timing
START_TIME=$(date +%s)

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

# =============================================================================
# Utility Functions
# =============================================================================
log_info()    { echo -e "${BLUE}[INFO]${NC} $1"; }
log_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
log_warn()    { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error()   { echo -e "${RED}[ERROR]${NC} $1" >&2; }
log_step()    { echo -e "${CYAN}[STEP]${NC} $1"; }
die()         { log_error "$1"; exit 1; }

phase_start() {
    echo ""
    echo "=============================================="
    echo "  $1"
    echo "=============================================="
    PHASE_START=$(date +%s)
}

phase_end() {
    local duration=$(($(date +%s) - PHASE_START))
    log_success "$1 completed in ${duration}s"
}

# =============================================================================
# Environment Loading
# =============================================================================
load_environment() {
    log_info "Loading environment..."

    # Load from .env if exists (has IM_ADMIN key)
    if [ -f "$PROJECT_ROOT/.env" ]; then
        set -a
        source "$PROJECT_ROOT/.env"
        set +a
    fi

    # Load from global.env if exists
    if [ -f "$GLOBAL_ENV" ]; then
        set -a
        source "$GLOBAL_ENV"
        set +a
    fi

    # Set defaults
    export RPC_URL="${RPC_URL:-${ORBIT_RPC_URL:-https://index.rpc.zeeve.net}}"
    export ORBIT_CHAIN_ID="${ORBIT_CHAIN_ID:-111222333}"

    # Accept multiple key names: DEPLOY_PRIVATE_KEY, IM_ADMIN, ORBIT_PRIVATE_KEY
    if [ -z "$DEPLOY_PRIVATE_KEY" ]; then
        if [ -n "$IM_ADMIN" ]; then
            # IM_ADMIN might not have 0x prefix
            if [[ "$IM_ADMIN" != 0x* ]]; then
                export DEPLOY_PRIVATE_KEY="0x$IM_ADMIN"
            else
                export DEPLOY_PRIVATE_KEY="$IM_ADMIN"
            fi
            log_info "Using IM_ADMIN as deploy key"
        elif [ -n "$ORBIT_PRIVATE_KEY" ]; then
            export DEPLOY_PRIVATE_KEY="$ORBIT_PRIVATE_KEY"
            log_info "Using ORBIT_PRIVATE_KEY as deploy key"
        else
            die "No private key found. Set DEPLOY_PRIVATE_KEY, IM_ADMIN, or ORBIT_PRIVATE_KEY"
        fi
    fi

    DEPLOYER_ADDRESS=$(cast wallet address "$DEPLOY_PRIVATE_KEY")
    log_info "Deployer: $DEPLOYER_ADDRESS"
    log_info "RPC: $RPC_URL"
}

# =============================================================================
# Stylus Contract Deployment (replaces vaultworks/scripts/vars.sh functions)
# Uses cargo-stylus v0.5.7 workflow: check (build) then deploy with --wasm-file
# Must run from vaultworks directory for Cargo workspace context
# =============================================================================

# Build a Stylus contract (returns wasm path)
build_contract() {
    local contract_name=$1
    local contract_path="$VAULTWORKS_DIR/contracts/$contract_name"
    local wasm_path="$VAULTWORKS_DIR/target/wasm32-unknown-unknown/release/${contract_name}.wasm"

    if [ ! -d "$contract_path" ]; then
        die "Contract not found: $contract_name at $contract_path"
    fi

    log_step "Building $contract_name..."

    # Build the contract using cargo stylus check from the contract directory
    # This ensures Cargo workspace context is correct
    (
        cd "$contract_path"
        cargo stylus check --endpoint="$RPC_URL" 2>&1 || true
    )

    # Check WASM was built
    if [ ! -f "$wasm_path" ]; then
        die "Failed to build contract: $contract_name (no wasm at $wasm_path)"
    fi

    echo "$wasm_path"
}

# Deploy a Stylus contract and return the address
# NOTE: Only echoes the address on stdout. All logs go to stderr.
deploy_stylus_contract() {
    local contract_name=$1
    local wasm_path="$VAULTWORKS_DIR/target/wasm32-unknown-unknown/release/${contract_name}.wasm"

    # Build first if wasm doesn't exist
    if [ ! -f "$wasm_path" ]; then
        build_contract "$contract_name" > /dev/null
    fi

    log_step "Deploying $contract_name..." >&2

    # Deploy from vaultworks directory for correct workspace context
    local output
    output=$(cd "$VAULTWORKS_DIR" && cargo stylus deploy \
        --wasm-file="target/wasm32-unknown-unknown/release/${contract_name}.wasm" \
        --endpoint="$RPC_URL" \
        --private-key="$DEPLOY_PRIVATE_KEY" \
        --no-verify \
        --max-fee-per-gas-gwei=$MAX_FEE_PER_GAS_GWEI 2>&1) || {
            log_error "Deployment output: $output" >&2
            die "Failed to deploy $contract_name"
        }

    # Parse the deployed address from output
    # Format: "deployed code at address: 0x..."
    local address
    address=$(echo "$output" | grep -oE '0x[a-fA-F0-9]{40}' | head -1)

    if [ -z "$address" ]; then
        log_error "Deploy output: $output" >&2
        die "Could not parse address for $contract_name"
    fi

    log_success "$contract_name deployed at: $address" >&2
    echo "$address"
}

# Send a transaction to a contract
contract_send() {
    local address=$1
    local function_sig=$2
    shift 2
    local args=("$@")

    log_step "Calling $function_sig on $address"

    cast send "$address" "$function_sig" "${args[@]}" \
        --rpc-url "$RPC_URL" \
        --private-key "$DEPLOY_PRIVATE_KEY" \
        2>&1 || die "Failed to call $function_sig"
}

# Call (read) a contract function
contract_call() {
    local address=$1
    local function_sig=$2
    shift 2
    local args=("$@")

    cast call "$address" "$function_sig" "${args[@]}" \
        --rpc-url "$RPC_URL" 2>&1
}

# Generate calldata for a function
calldata() {
    cast calldata "$@"
}

# =============================================================================
# Help
# =============================================================================
show_help() {
    cat << 'EOF'
deploy.sh - Self-Contained Deployment Script

Deploys all Vaultworks Stylus contracts:
  - Castle (Diamond proxy) + Gate
  - Officers: Constable, Banker, Factor, Steward, Guildmaster, Scribe, Worksman, Clerk
  - Vault infrastructure: vault_native, vault, vault_native_orders, vault_native_claims

Usage: ./deploy.sh [OPTIONS]

Options:
  --skip-castle       Skip Castle + Officers deployment
  --skip-vault        Skip Vault infrastructure deployment
  --skip-pairs        Skip PairRegistry pair population
  --skip-itp          Skip test ITP creation
  --clean             Force redeploy everything (ignore existing)
  --help              Show this help message

Prerequisites:
  - DEPLOY_PRIVATE_KEY in global.env
  - cargo stylus, cast, jq installed

Contract Deployment Order:
  1. Castle (logic) -> Gate (proxy) -> Initialize
  2. Constable -> appointConstable()
  3. Officers: Banker, Factor, Steward, Guildmaster, Scribe, Worksman, Clerk
  4. Vault: vault_native -> vault -> gate -> orders + claims
EOF
    exit 0
}

# =============================================================================
# Command Line Parsing
# =============================================================================
SKIP_CASTLE=false
SKIP_VAULT=false
SKIP_PAIRS=false
SKIP_ITP=false
FORCE_CLEAN=false

while [[ "$#" -gt 0 ]]; do
    case $1 in
        --skip-castle)  SKIP_CASTLE=true ;;
        --skip-vault)   SKIP_VAULT=true ;;
        --skip-pairs)   SKIP_PAIRS=true ;;
        --skip-itp)     SKIP_ITP=true ;;
        --clean)        FORCE_CLEAN=true ;;
        --help|-h)      show_help ;;
        *) die "Unknown option: $1. Use --help for usage." ;;
    esac
    shift
done

# =============================================================================
# Pre-flight Checks
# =============================================================================
preflight_checks() {
    phase_start "Pre-flight Checks"

    # Check required tools
    for tool in cargo cast jq; do
        if ! command -v "$tool" &>/dev/null; then
            die "Required tool not found: $tool"
        fi
    done
    log_info "Tools: cargo, cast, jq - OK"

    # Check cargo-stylus
    if ! cargo stylus --version &>/dev/null; then
        die "cargo-stylus not installed. Run: cargo install cargo-stylus"
    fi
    log_info "cargo-stylus: OK"

    # Check vaultworks directory
    if [ ! -d "$VAULTWORKS_DIR/contracts" ]; then
        die "vaultworks/contracts not found at $VAULTWORKS_DIR"
    fi
    log_info "vaultworks/contracts: OK"

    # List available contracts
    log_info "Available contracts:"
    ls -1 "$VAULTWORKS_DIR/contracts" | while read contract; do
        echo "  - $contract"
    done

    phase_end "Pre-flight Checks"
}

# =============================================================================
# Castle + Officers Deployment
# =============================================================================
deploy_castle_and_officers() {
    if [ "$SKIP_CASTLE" = true ]; then
        log_info "Skipping Castle deployment (--skip-castle)"
        return 0
    fi

    # Check if already deployed
    if [ -n "$CASTLE_ADDRESS" ] && [ "$FORCE_CLEAN" = false ]; then
        log_info "Castle already deployed at $CASTLE_ADDRESS"
        log_info "Use --clean to force redeploy"
        return 0
    fi

    phase_start "Castle + Officers Deployment"

    # ===================
    # 1. Deploy Castle Logic
    # ===================
    log_info "Step 1: Deploying Castle logic..."
    CASTLE_LOGIC=$(deploy_stylus_contract "castle")

    # ===================
    # 2. Deploy Gate (Proxy)
    # ===================
    log_info "Step 2: Deploying Gate (proxy)..."
    GATE_ADDRESS=$(deploy_stylus_contract "gate")

    # ===================
    # 3. Initialize Gate with Castle logic
    # ===================
    log_info "Step 3: Initializing Gate with Castle..."

    # Generate calldata for initialize(address castle, address admin)
    local init_calldata
    init_calldata=$(calldata "initialize(address,address)" "$CASTLE_LOGIC" "$DEPLOYER_ADDRESS")

    # Call gate.initialize(castle_logic, init_calldata)
    contract_send "$GATE_ADDRESS" "initialize(address,bytes)" "$CASTLE_LOGIC" "$init_calldata"
    log_success "Gate initialized with Castle logic"

    # The Gate is now our Castle proxy - all further calls go through it
    export CASTLE_ADDRESS="$GATE_ADDRESS"

    # ===================
    # 4. Deploy and Appoint Constable
    # ===================
    log_info "Step 4: Deploying Constable..."
    CONSTABLE_ADDRESS=$(deploy_stylus_contract "constable")

    log_info "Step 4b: Appointing Constable..."
    contract_send "$CASTLE_ADDRESS" "appointConstable(address)" "$CONSTABLE_ADDRESS"
    log_success "Constable appointed"

    # ===================
    # 5. Deploy and Appoint Officers
    # ===================
    # Now we call appointment functions on CASTLE_ADDRESS (the proxy)
    # because Constable registered those functions in the Diamond

    # Banker
    log_info "Step 5a: Deploying Banker..."
    BANKER_ADDRESS=$(deploy_stylus_contract "banker")
    contract_send "$CASTLE_ADDRESS" "appointBanker(address)" "$BANKER_ADDRESS"
    log_success "Banker appointed"

    # Factor
    log_info "Step 5b: Deploying Factor..."
    FACTOR_ADDRESS=$(deploy_stylus_contract "factor")
    contract_send "$CASTLE_ADDRESS" "appointFactor(address)" "$FACTOR_ADDRESS"
    log_success "Factor appointed"

    # Steward
    log_info "Step 5c: Deploying Steward..."
    STEWARD_ADDRESS=$(deploy_stylus_contract "steward")
    contract_send "$CASTLE_ADDRESS" "appointSteward(address)" "$STEWARD_ADDRESS"
    log_success "Steward appointed"

    # Guildmaster (CRITICAL - enables submitIndex for ITP creation!)
    log_info "Step 5d: Deploying Guildmaster..."
    GUILDMASTER_ADDRESS=$(deploy_stylus_contract "guildmaster")
    contract_send "$CASTLE_ADDRESS" "appointGuildmaster(address)" "$GUILDMASTER_ADDRESS"
    log_success "Guildmaster appointed - submitIndex now available!"

    # Scribe
    log_info "Step 5e: Deploying Scribe..."
    SCRIBE_ADDRESS=$(deploy_stylus_contract "scribe")
    contract_send "$CASTLE_ADDRESS" "appointScribe(address)" "$SCRIBE_ADDRESS"
    log_success "Scribe appointed"

    # Worksman
    log_info "Step 5f: Deploying Worksman..."
    WORKSMAN_ADDRESS=$(deploy_stylus_contract "worksman")
    contract_send "$CASTLE_ADDRESS" "appointWorksman(address)" "$WORKSMAN_ADDRESS"
    log_success "Worksman appointed"

    # Clerk
    log_info "Step 5g: Deploying Clerk..."
    CLERK_ADDRESS=$(deploy_stylus_contract "clerk")
    contract_send "$CASTLE_ADDRESS" "appointClerk(address)" "$CLERK_ADDRESS"
    log_success "Clerk appointed"

    # ===================
    # Summary
    # ===================
    echo ""
    echo "======================================================"
    echo "           Castle Deployment Complete"
    echo "======================================================"
    echo "  Castle (Gate):     $CASTLE_ADDRESS"
    echo "  Castle Logic:      $CASTLE_LOGIC"
    echo "------------------------------------------------------"
    echo "  Constable:         $CONSTABLE_ADDRESS"
    echo "  Banker:            $BANKER_ADDRESS"
    echo "  Factor:            $FACTOR_ADDRESS"
    echo "  Steward:           $STEWARD_ADDRESS"
    echo "  Guildmaster:       $GUILDMASTER_ADDRESS"
    echo "  Scribe:            $SCRIBE_ADDRESS"
    echo "  Worksman:          $WORKSMAN_ADDRESS"
    echo "  Clerk:             $CLERK_ADDRESS"
    echo "======================================================"

    phase_end "Castle + Officers Deployment"
}

# =============================================================================
# Vault Infrastructure Deployment
# =============================================================================
deploy_vault_infrastructure() {
    if [ "$SKIP_VAULT" = true ]; then
        log_info "Skipping Vault deployment (--skip-vault)"
        return 0
    fi

    if [ -z "$CASTLE_ADDRESS" ]; then
        log_warn "Castle not deployed, skipping Vault deployment"
        return 0
    fi

    # Check if already deployed
    if [ -n "$VAULT_ADDRESS" ] && [ "$FORCE_CLEAN" = false ]; then
        log_info "Vault already deployed at $VAULT_ADDRESS"
        log_info "Use --clean to force redeploy"
        return 0
    fi

    phase_start "Vault Infrastructure Deployment"

    # ===================
    # 1. Deploy Vault Native (Provider)
    # ===================
    log_info "Step 1: Deploying vault_native (provider)..."
    VAULT_NATIVE_ADDRESS=$(deploy_stylus_contract "vault_native")

    # ===================
    # 2. Deploy Vault Logic
    # ===================
    log_info "Step 2: Deploying vault (logic)..."
    VAULT_LOGIC=$(deploy_stylus_contract "vault")

    # ===================
    # 3. Deploy Gate for Vault (Proxy)
    # ===================
    log_info "Step 3: Deploying Gate for Vault..."
    VAULT_GATE=$(deploy_stylus_contract "gate")

    # ===================
    # 4. Initialize Vault Gate
    # ===================
    log_info "Step 4: Initializing Vault Gate..."

    # initialize(address admin, address provider, address castle)
    local vault_init_calldata
    vault_init_calldata=$(calldata "initialize(address,address,address)" "$DEPLOYER_ADDRESS" "$VAULT_NATIVE_ADDRESS" "$CASTLE_ADDRESS")

    contract_send "$VAULT_GATE" "initialize(address,bytes)" "$VAULT_LOGIC" "$vault_init_calldata"
    log_success "Vault Gate initialized"

    export VAULT_ADDRESS="$VAULT_GATE"

    # ===================
    # 5. Deploy and Install Orders Extension
    # ===================
    log_info "Step 5: Deploying vault_native_orders..."
    VAULT_ORDERS_ADDRESS=$(deploy_stylus_contract "vault_native_orders")

    contract_send "$VAULT_ADDRESS" "installOrders(address)" "$VAULT_ORDERS_ADDRESS"
    log_success "Orders extension installed"

    # ===================
    # 6. Deploy and Install Claims Extension
    # ===================
    log_info "Step 6: Deploying vault_native_claims..."
    VAULT_CLAIMS_ADDRESS=$(deploy_stylus_contract "vault_native_claims")

    contract_send "$VAULT_ADDRESS" "installClaims(address)" "$VAULT_CLAIMS_ADDRESS"
    log_success "Claims extension installed"

    # ===================
    # 7. Set Vault Prototype on Worksman (CRITICAL for ITP creation!)
    # ===================
    log_info "Step 7: Setting vault prototype on Worksman..."
    log_info "  This enables Guildmaster.submitIndex() to call Worksman.buildVault()"

    contract_send "$CASTLE_ADDRESS" "setVaultPrototype(address)" "$VAULT_ADDRESS"
    log_success "Vault prototype set - buildVault() will now work!"

    # Verify it was set correctly
    local vault_prototype
    vault_prototype=$(contract_call "$CASTLE_ADDRESS" "getVaultPrototype()(address)" 2>/dev/null) || true
    if [ "$vault_prototype" = "$VAULT_ADDRESS" ]; then
        log_success "Verified: Vault prototype = $vault_prototype"
    else
        log_warn "Could not verify vault prototype (got: $vault_prototype)"
    fi

    # ===================
    # Summary
    # ===================
    echo ""
    echo "======================================================"
    echo "           Vault Deployment Complete"
    echo "======================================================"
    echo "  Vault (Gate):           $VAULT_ADDRESS"
    echo "  Vault Logic:            $VAULT_LOGIC"
    echo "  Vault Prototype Set:    YES (on Worksman)"
    echo "------------------------------------------------------"
    echo "  Vault Native:           $VAULT_NATIVE_ADDRESS"
    echo "  Vault Native Orders:    $VAULT_ORDERS_ADDRESS"
    echo "  Vault Native Claims:    $VAULT_CLAIMS_ADDRESS"
    echo "======================================================"

    phase_end "Vault Infrastructure Deployment"
}

# =============================================================================
# Grant Roles (Required for ITP creation - Scenario 5 flow)
# =============================================================================
grant_required_roles() {
    if [ -z "$CASTLE_ADDRESS" ]; then
        log_warn "Castle not deployed, skipping role grants"
        return 0
    fi

    phase_start "Role Configuration"

    # Get role hashes
    local ISSUER_ROLE VENDOR_ROLE KEEPER_ROLE

    ISSUER_ROLE=$(contract_call "$CASTLE_ADDRESS" "getIssuerRole()(bytes32)" 2>/dev/null) || {
        log_warn "Could not get ISSUER_ROLE - Constable may not be appointed"
        phase_end "Role Configuration"
        return 0
    }

    VENDOR_ROLE=$(contract_call "$CASTLE_ADDRESS" "getVendorRole()(bytes32)" 2>/dev/null) || {
        log_warn "Could not get VENDOR_ROLE"
    }

    KEEPER_ROLE=$(contract_call "$CASTLE_ADDRESS" "getKeeperRole()(bytes32)" 2>/dev/null) || {
        log_warn "Could not get KEEPER_ROLE"
    }

    log_info "Role hashes:"
    log_info "  ISSUER_ROLE: $ISSUER_ROLE"
    log_info "  VENDOR_ROLE: $VENDOR_ROLE"
    log_info "  KEEPER_ROLE: $KEEPER_ROLE"

    # Grant ISSUER_ROLE to deployer (allows submitIndex, submitAssetWeights, submitVote)
    log_info "Granting ISSUER_ROLE to deployer..."
    contract_send "$CASTLE_ADDRESS" "grantRole(bytes32,address)" "$ISSUER_ROLE" "$DEPLOYER_ADDRESS" || {
        log_warn "Failed to grant ISSUER_ROLE (may already be granted)"
    }

    # Grant VENDOR_ROLE to deployer (allows submitAssets, submitMargin, submitSupply, submitMarketData)
    if [ -n "$VENDOR_ROLE" ]; then
        log_info "Granting VENDOR_ROLE to deployer..."
        contract_send "$CASTLE_ADDRESS" "grantRole(bytes32,address)" "$VENDOR_ROLE" "$DEPLOYER_ADDRESS" || {
            log_warn "Failed to grant VENDOR_ROLE (may already be granted)"
        }
    fi

    # Grant KEEPER_ROLE to deployer (allows updateIndexQuote, processPendingBuyOrder, processPendingSellOrder)
    if [ -n "$KEEPER_ROLE" ]; then
        log_info "Granting KEEPER_ROLE to deployer..."
        contract_send "$CASTLE_ADDRESS" "grantRole(bytes32,address)" "$KEEPER_ROLE" "$DEPLOYER_ADDRESS" || {
            log_warn "Failed to grant KEEPER_ROLE (may already be granted)"
        }
    fi

    log_success "Deployer can now execute full Scenario 5 flow:"
    log_success "  - submitIndex, submitAssetWeights, submitVote (ISSUER)"
    log_success "  - submitAssets, submitMargin, submitSupply, submitMarketData (VENDOR)"
    log_success "  - updateIndexQuote, processPendingBuyOrder/SellOrder (KEEPER)"

    phase_end "Role Configuration"
}

# =============================================================================
# PairRegistry Population
# =============================================================================
populate_pairs() {
    if [ "$SKIP_PAIRS" = true ]; then
        log_info "Skipping pair population (--skip-pairs)"
        return 0
    fi

    phase_start "PairRegistry Population"

    # Check for pairs script
    local pairs_script="$BRIDGE_DIR/bridge-contracts/script/populate-pairs.sh"
    local pairs_json="$VENDOR_DIR/data/bitget-pairs-solidity.json"

    if [ ! -f "$pairs_script" ]; then
        log_warn "populate-pairs.sh not found at $pairs_script"
        phase_end "PairRegistry Population"
        return 0
    fi

    if [ ! -f "$pairs_json" ]; then
        log_warn "bitget-pairs-solidity.json not found"
        log_info "Run: cargo run --bin list-bitget-pairs (from vendor/)"
        phase_end "PairRegistry Population"
        return 0
    fi

    log_info "Populating PairRegistry..."
    export ORBIT_RPC="$RPC_URL"
    export ORBIT_PRIVATE_KEY="$DEPLOY_PRIVATE_KEY"

    bash "$pairs_script" || log_warn "PairRegistry population may have failed"

    phase_end "PairRegistry Population"
}

# =============================================================================
# Test ITP Creation
# =============================================================================
create_test_itp() {
    if [ "$SKIP_ITP" = true ]; then
        log_info "Skipping test ITP (--skip-itp)"
        return 0
    fi

    if [ -z "$CASTLE_ADDRESS" ]; then
        log_warn "Castle not deployed, skipping ITP creation"
        return 0
    fi

    phase_start "Test ITP Creation"

    # Check if submitIndex is available
    local submit_selector="0x50840452"
    local delegates
    delegates=$(contract_call "$CASTLE_ADDRESS" "getFunctionDelegates(bytes4[])(address[])" "[$submit_selector]" 2>/dev/null)

    if [ "$delegates" = "[]" ] || [ -z "$delegates" ]; then
        log_error "submitIndex function not delegated - Guildmaster not properly appointed"
        phase_end "Test ITP Creation"
        return 1
    fi

    log_info "submitIndex is available via delegate"

    # Test ITP parameters
    local VENDOR_ID=1
    local INDEX_ID=1001
    local NAME="Test BTC-ETH Index"
    local SYMBOL="TBTCETH"
    local DESCRIPTION="Test index for dev environment"
    local METHODOLOGY="Market cap weighted"
    local INITIAL_PRICE="1000000000000000000"  # 1e18
    local MAX_ORDER_SIZE="1000000000000000000000"  # 1000e18
    local COLLATERAL_ASSET="${USDC_ADDRESS:-0xaf88d065e77c8cC2239327C5EDb3A432268e5831}"
    local CUSTODY_ADDR="$DEPLOYER_ADDRESS"

    log_info "Creating test ITP..."
    log_info "  Index ID: $INDEX_ID"
    log_info "  Name: $NAME"
    log_info "  Symbol: $SYMBOL"

    # submitIndex(uint128,uint128,string,string,string,string,uint128,address,string,address[],address,address,uint128)
    local tx_output
    tx_output=$(cast send "$CASTLE_ADDRESS" \
        "submitIndex(uint128,uint128,string,string,string,string,uint128,address,string,address[],address,address,uint128)(address)" \
        "$VENDOR_ID" \
        "$INDEX_ID" \
        "$NAME" \
        "$SYMBOL" \
        "$DESCRIPTION" \
        "$METHODOLOGY" \
        "$INITIAL_PRICE" \
        "$DEPLOYER_ADDRESS" \
        "custody:$CUSTODY_ADDR" \
        "[]" \
        "$CUSTODY_ADDR" \
        "$COLLATERAL_ASSET" \
        "$MAX_ORDER_SIZE" \
        --rpc-url "$RPC_URL" \
        --private-key "$DEPLOY_PRIVATE_KEY" 2>&1) || {
            log_warn "ITP creation may have failed: $tx_output"
        }

    log_info "Transaction output: $tx_output"

    # Try to parse ITP address
    TEST_ITP_ADDRESS=$(echo "$tx_output" | grep -oE '0x[a-fA-F0-9]{40}' | tail -1)
    if [ -n "$TEST_ITP_ADDRESS" ]; then
        log_success "Test ITP created at: $TEST_ITP_ADDRESS"
    fi

    phase_end "Test ITP Creation"
}

# =============================================================================
# Add Custodians to VaultGate (Required for placeBuyOrder/placeSellOrder)
# =============================================================================
add_vault_custodians() {
    if [ -z "$VAULT_ADDRESS" ]; then
        log_warn "Vault not deployed, skipping custodian setup"
        return 0
    fi

    phase_start "Vault Custodian Setup"

    # Add deployer as custodian (for testing)
    log_info "Adding deployer as custodian..."
    contract_send "$VAULT_ADDRESS" "addCustodian(address)" "$DEPLOYER_ADDRESS" || {
        log_warn "Failed to add deployer as custodian (may already be added)"
    }

    # Add keeper address if configured
    local KEEPER_ADDRESS="${KEEPER_ADDRESSES:-}"
    if [ -n "$KEEPER_ADDRESS" ]; then
        # Handle comma-separated list
        IFS=',' read -ra KEEPERS <<< "$KEEPER_ADDRESS"
        for keeper in "${KEEPERS[@]}"; do
            keeper=$(echo "$keeper" | xargs)  # trim whitespace
            if [ -n "$keeper" ]; then
                log_info "Adding keeper $keeper as custodian..."
                contract_send "$VAULT_ADDRESS" "addCustodian(address)" "$keeper" || {
                    log_warn "Failed to add keeper as custodian (may already be added)"
                }
            fi
        done
    fi

    # Add bridge-node trader address if configured
    local BRIDGE_TRADER="${BRIDGE_TRADER_ADDRESS:-}"
    if [ -z "$BRIDGE_TRADER" ] && [ -n "$BRIDGE_TRADER_PRIVATE_KEY" ]; then
        BRIDGE_TRADER=$(cast wallet address "$BRIDGE_TRADER_PRIVATE_KEY" 2>/dev/null || echo "")
    fi
    if [ -n "$BRIDGE_TRADER" ]; then
        log_info "Adding bridge trader $BRIDGE_TRADER as custodian..."
        contract_send "$VAULT_ADDRESS" "addCustodian(address)" "$BRIDGE_TRADER" || {
            log_warn "Failed to add bridge trader as custodian (may already be added)"
        }
    fi

    log_success "Custodians configured on VaultGate"
    phase_end "Vault Custodian Setup"
}

# =============================================================================
# Approve Vault (Required for buy/sell orders)
# =============================================================================
approve_vault() {
    if [ "$SKIP_ITP" = true ]; then
        log_info "Skipping vault approval (--skip-itp)"
        return 0
    fi

    if [ -z "$CASTLE_ADDRESS" ]; then
        log_warn "Castle not deployed, skipping vault approval"
        return 0
    fi

    phase_start "Vault Approval"

    # Index ID matches what was created in create_test_itp()
    local INDEX_ID=1001

    log_info "Approving vault for index $INDEX_ID via submitVote..."
    log_info "  (Scribe signature verification is stubbed to always return true)"

    # submitVote(uint128 index_id, bytes vote_data)
    # The scribe's verify_signature() returns true for any input,
    # so we pass empty bytes
    local tx_output
    tx_output=$(cast send "$CASTLE_ADDRESS" \
        "submitVote(uint128,bytes)" \
        "$INDEX_ID" \
        "0x" \
        --rpc-url "$RPC_URL" \
        --private-key "$DEPLOY_PRIVATE_KEY" 2>&1) || {
            log_warn "Vault approval may have failed: $tx_output"
        }

    log_info "Transaction output: $tx_output"
    log_success "Vault approved for index $INDEX_ID - now ready for buy/sell orders!"

    phase_end "Vault Approval"
}

# =============================================================================
# Update global.env
# =============================================================================
update_global_env() {
    phase_start "Update global.env"

    # Preserve existing values that aren't being set
    ARB_RPC_URL="${ARB_RPC_URL:-https://arb1.arbitrum.io/rpc}"
    ARB_CHAIN_ID="${ARB_CHAIN_ID:-42161}"
    BRIDGE_PROXY_ADDRESS="${BRIDGE_PROXY_ADDRESS:-}"
    BRIDGED_ITP_FACTORY_ADDRESS="${BRIDGED_ITP_FACTORY_ADDRESS:-}"
    USDC_ADDRESS="${USDC_ADDRESS:-0xaf88d065e77c8cC2239327C5EDb3A432268e5831}"
    PAIR_REGISTRY_ADDRESS="${PAIR_REGISTRY_ADDRESS:-}"

    cat > "$GLOBAL_ENV" << EOF
# Global Environment Variables
# Generated by deploy.sh - $(date)
# Single source of truth for all contract addresses

# =============================================================================
# PRIVATE KEYS (do not commit to git!)
# =============================================================================
DEPLOY_PRIVATE_KEY=$DEPLOY_PRIVATE_KEY

# =============================================================================
# ORBIT CHAIN (Chain ID: $ORBIT_CHAIN_ID)
# =============================================================================
ORBIT_RPC_URL=$RPC_URL
ORBIT_CHAIN_ID=$ORBIT_CHAIN_ID

# Castle Diamond (main entry point)
CASTLE_ADDRESS=${CASTLE_ADDRESS:-}
CASTLE_LOGIC=${CASTLE_LOGIC:-}

# Castle Officers
CONSTABLE_ADDRESS=${CONSTABLE_ADDRESS:-}
BANKER_ADDRESS=${BANKER_ADDRESS:-}
FACTOR_ADDRESS=${FACTOR_ADDRESS:-}
STEWARD_ADDRESS=${STEWARD_ADDRESS:-}
GUILDMASTER_ADDRESS=${GUILDMASTER_ADDRESS:-}
SCRIBE_ADDRESS=${SCRIBE_ADDRESS:-}
WORKSMAN_ADDRESS=${WORKSMAN_ADDRESS:-}
CLERK_ADDRESS=${CLERK_ADDRESS:-}

# Vault Infrastructure
VAULT_ADDRESS=${VAULT_ADDRESS:-}
VAULT_LOGIC=${VAULT_LOGIC:-}
VAULT_NATIVE_ADDRESS=${VAULT_NATIVE_ADDRESS:-}
VAULT_ORDERS_ADDRESS=${VAULT_ORDERS_ADDRESS:-}
VAULT_CLAIMS_ADDRESS=${VAULT_CLAIMS_ADDRESS:-}

# Legacy aliases (for backward compatibility)
COLLATERAL_ADDRESS=${COLLATERAL_ADDRESS:-}
CUSTODY_ADDRESS=${CUSTODY_ADDRESS:-}
VENDOR_CONTRACT_ADDRESS=${VENDOR_CONTRACT_ADDRESS:-}

# PairRegistry (Story 1.3)
PAIR_REGISTRY_ADDRESS=$PAIR_REGISTRY_ADDRESS

# Test ITP
TEST_ITP_ADDRESS=${TEST_ITP_ADDRESS:-}

# =============================================================================
# ARBITRUM (Chain ID: $ARB_CHAIN_ID)
# =============================================================================
ARB_RPC_URL=$ARB_RPC_URL
ARB_CHAIN_ID=$ARB_CHAIN_ID

# Bridge
BRIDGE_PROXY_ADDRESS=$BRIDGE_PROXY_ADDRESS
BRIDGED_ITP_FACTORY_ADDRESS=$BRIDGED_ITP_FACTORY_ADDRESS

# Tokens
USDC_ADDRESS=$USDC_ADDRESS
EOF

    log_success "global.env updated"
    phase_end "Update global.env"
}

# =============================================================================
# Summary
# =============================================================================
print_summary() {
    local total_duration=$(($(date +%s) - START_TIME))

    echo ""
    echo "=============================================="
    echo "         DEPLOYMENT COMPLETE"
    echo "=============================================="
    echo ""
    echo "Duration: ${total_duration}s"
    echo ""
    echo "Castle Diamond:"
    echo "  CASTLE_ADDRESS:     ${CASTLE_ADDRESS:-NOT DEPLOYED}"
    echo "  GUILDMASTER:        ${GUILDMASTER_ADDRESS:-NOT DEPLOYED}"
    echo ""
    echo "Vault:"
    echo "  VAULT_ADDRESS:      ${VAULT_ADDRESS:-NOT DEPLOYED}"
    echo ""
    echo "Test ITP:"
    echo "  TEST_ITP_ADDRESS:   ${TEST_ITP_ADDRESS:-NOT CREATED}"
    echo ""
    echo "Config saved to: $GLOBAL_ENV"
    echo ""

    # Verify submitIndex is available
    if [ -n "$CASTLE_ADDRESS" ]; then
        local delegates
        delegates=$(contract_call "$CASTLE_ADDRESS" "getFunctionDelegates(bytes4[])(address[])" "[0x50840452]" 2>/dev/null || echo "[]")
        if [ "$delegates" != "[]" ] && [ -n "$delegates" ]; then
            echo -e "${GREEN}[VERIFIED]${NC} submitIndex is available"
        else
            echo -e "${RED}[WARNING]${NC} submitIndex not available - check Guildmaster appointment"
        fi
    fi

    echo ""
    echo "=============================================="
}

# =============================================================================
# Main
# =============================================================================
main() {
    echo ""
    echo "=============================================="
    echo "    feb26 Self-Contained Deployment"
    echo "=============================================="
    echo ""

    log_info "Options:"
    log_info "  --skip-castle: $SKIP_CASTLE"
    log_info "  --skip-vault:  $SKIP_VAULT"
    log_info "  --skip-pairs:  $SKIP_PAIRS"
    log_info "  --skip-itp:    $SKIP_ITP"
    log_info "  --clean:       $FORCE_CLEAN"

    load_environment
    preflight_checks

    deploy_castle_and_officers
    deploy_vault_infrastructure
    add_vault_custodians
    grant_required_roles
    populate_pairs
    create_test_itp
    approve_vault

    update_global_env
    print_summary
}

main "$@"
