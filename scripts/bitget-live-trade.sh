#!/usr/bin/env bash
# =============================================================================
# bitget-live-trade.sh - Execute Live Trades on Bitget
# =============================================================================
# Executes real IOC orders on Bitget for testing purposes.
# Can buy assets or sell all back to USDC.
#
# Usage:
#   ./bitget-live-trade.sh buy                 # Buy small test amounts of BTC/ETH/SOL
#   ./bitget-live-trade.sh sell                # Sell all crypto back to USDC
#   ./bitget-live-trade.sh balances            # Show current Bitget balances
#   ./bitget-live-trade.sh test-round-trip     # Buy then immediately sell back
#
# Environment Required:
#   BITGET_API_KEY, BITGET_API_SECRET, BITGET_PASSPHRASE
# =============================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
VENDOR_DIR="$PROJECT_ROOT/vendor"
GLOBAL_ENV="$PROJECT_ROOT/global.env"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info()    { echo -e "${BLUE}[INFO]${NC} $1"; }
log_success() { echo -e "${GREEN}[PASS]${NC} $1"; }
log_warn()    { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error()   { echo -e "${RED}[FAIL]${NC} $1" >&2; }

# Test amounts in quote currency (USDC for market buy orders)
# Bitget minimum is ~1 USDC/USDT per order, using 10 for safety margin
BUY_AMOUNT="10"   # $10 worth per asset

# Trading pairs - use USDC pairs since user has USDC balance
BTC_PAIR="BTCUSDC"
ETH_PAIR="ETHUSDC"
SOL_PAIR="SOLUSDC"
QUOTE_CURRENCY="USDC"

# =============================================================================
# Load Environment
# =============================================================================
load_env() {
    if [ -f "$GLOBAL_ENV" ]; then
        set -a
        source "$GLOBAL_ENV"
        set +a
    fi

    if [ -z "$BITGET_API_KEY" ] || [ -z "$BITGET_API_SECRET" ] || [ -z "$BITGET_PASSPHRASE" ]; then
        log_error "Bitget credentials required"
        log_error "Set BITGET_API_KEY, BITGET_API_SECRET, BITGET_PASSPHRASE in global.env"
        exit 2
    fi

    log_info "Bitget credentials: configured"
}

# =============================================================================
# Bitget API Helpers
# =============================================================================
BITGET_API_URL="https://api.bitget.com"

# Get millisecond timestamp (works on macOS and Linux)
get_timestamp_ms() {
    # Use Python for reliable cross-platform millisecond timestamp
    python3 -c "import time; print(int(time.time() * 1000))"
}

# Generate Bitget signature
bitget_sign() {
    local timestamp="$1"
    local method="$2"
    local request_path="$3"
    local body="${4:-}"

    local pre_hash="${timestamp}${method}${request_path}${body}"
    echo -n "$pre_hash" | openssl dgst -sha256 -hmac "$BITGET_API_SECRET" -binary | base64
}

# Make authenticated Bitget API call
bitget_api() {
    local method="$1"
    local endpoint="$2"
    local body="${3:-}"

    local timestamp=$(get_timestamp_ms)
    local signature=$(bitget_sign "$timestamp" "$method" "$endpoint" "$body")

    local headers=(
        -H "ACCESS-KEY: $BITGET_API_KEY"
        -H "ACCESS-SIGN: $signature"
        -H "ACCESS-TIMESTAMP: $timestamp"
        -H "ACCESS-PASSPHRASE: $BITGET_PASSPHRASE"
        -H "Content-Type: application/json"
        -H "locale: en-US"
    )

    if [ "$method" = "GET" ]; then
        curl -s "${headers[@]}" "${BITGET_API_URL}${endpoint}"
    else
        curl -s -X "$method" "${headers[@]}" -d "$body" "${BITGET_API_URL}${endpoint}"
    fi
}

# =============================================================================
# Commands
# =============================================================================
cmd_balances() {
    log_info "Fetching Bitget spot balances..."

    local response
    response=$(bitget_api "GET" "/api/v2/spot/account/assets")

    echo ""
    echo "Bitget Spot Balances:"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

    # Parse and display non-zero balances
    echo "$response" | jq -r '.data[] | select(.available != "0" or .frozen != "0") | "  \(.coin): \(.available) (frozen: \(.frozen))"' 2>/dev/null || {
        log_error "Failed to parse balances"
        echo "$response"
        return 1
    }

    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
}

cmd_buy() {
    log_info "Executing BUY orders on Bitget..."
    log_warn "This will execute REAL trades!"

    echo ""
    echo "Orders to place (using $QUOTE_CURRENCY):"
    echo "  BTC: Buy \$${BUY_AMOUNT} worth via $BTC_PAIR"
    echo "  ETH: Buy \$${BUY_AMOUNT} worth via $ETH_PAIR"
    echo "  SOL: Buy \$${BUY_AMOUNT} worth via $SOL_PAIR"
    echo ""

    # Get current prices
    log_info "Fetching current prices..."

    local btc_price eth_price sol_price
    btc_price=$(curl -s "${BITGET_API_URL}/api/v2/spot/market/tickers?symbol=$BTC_PAIR" | jq -r '.data[0].lastPr')
    eth_price=$(curl -s "${BITGET_API_URL}/api/v2/spot/market/tickers?symbol=$ETH_PAIR" | jq -r '.data[0].lastPr')
    sol_price=$(curl -s "${BITGET_API_URL}/api/v2/spot/market/tickers?symbol=$SOL_PAIR" | jq -r '.data[0].lastPr')

    log_info "Current prices: BTC=$btc_price, ETH=$eth_price, SOL=$sol_price"

    # Check quote currency balance
    log_info "Checking $QUOTE_CURRENCY balance..."
    local quote_balance
    quote_balance=$(bitget_api "GET" "/api/v2/spot/account/assets" | jq -r ".data[] | select(.coin == \"$QUOTE_CURRENCY\") | .available" 2>/dev/null || echo "0")
    log_info "$QUOTE_CURRENCY available: $quote_balance"

    if [ -z "$quote_balance" ] || [ "$quote_balance" = "null" ] || [ "$quote_balance" = "0" ]; then
        log_error "No $QUOTE_CURRENCY balance available for trading"
        return 1
    fi

    # Calculate base amounts and aggressive prices for IOC limit orders
    # USDC pairs don't support market buy orders well, so we use IOC limit orders
    # with prices 0.5% above market to ensure immediate fill
    # Precision: BTCUSDC=5, ETHUSDC=4, SOLUSDC=2
    local btc_size=$(python3 -c "print(f'{$BUY_AMOUNT / $btc_price:.5f}')")
    local eth_size=$(python3 -c "print(f'{$BUY_AMOUNT / $eth_price:.4f}')")
    local sol_size=$(python3 -c "print(f'{$BUY_AMOUNT / $sol_price:.2f}')")

    # Add 0.5% premium to ensure fill
    local btc_buy_price=$(python3 -c "print(f'{$btc_price * 1.005:.2f}')")
    local eth_buy_price=$(python3 -c "print(f'{$eth_price * 1.005:.2f}')")
    local sol_buy_price=$(python3 -c "print(f'{$sol_price * 1.005:.2f}')")

    log_info "Calculated sizes: BTC=$btc_size, ETH=$eth_size, SOL=$sol_size"
    log_info "Using IOC limit prices: BTC=$btc_buy_price, ETH=$eth_buy_price, SOL=$sol_buy_price"

    # Place IOC limit buy orders (fills immediately at best price or cancels)
    local timestamp=$(get_timestamp_ms)

    # BTC Buy - use IOC limit order
    log_info "Placing BTC buy order ($btc_size BTC @ $btc_buy_price)..."
    local btc_order_body="{\"symbol\":\"$BTC_PAIR\",\"side\":\"buy\",\"orderType\":\"limit\",\"size\":\"$btc_size\",\"price\":\"$btc_buy_price\",\"force\":\"ioc\",\"clientOid\":\"test-$timestamp-btc\"}"
    local btc_response
    btc_response=$(bitget_api "POST" "/api/v2/spot/trade/place-order" "$btc_order_body")

    if echo "$btc_response" | jq -e '.code == "00000"' > /dev/null 2>&1; then
        local btc_order_id=$(echo "$btc_response" | jq -r '.data.orderId')
        log_success "BTC order placed: $btc_order_id"
    else
        log_error "BTC order failed: $(echo "$btc_response" | jq -r '.msg // .message // "Unknown error"')"
    fi

    # ETH Buy
    log_info "Placing ETH buy order ($eth_size ETH @ $eth_buy_price)..."
    local eth_order_body="{\"symbol\":\"$ETH_PAIR\",\"side\":\"buy\",\"orderType\":\"limit\",\"size\":\"$eth_size\",\"price\":\"$eth_buy_price\",\"force\":\"ioc\",\"clientOid\":\"test-$timestamp-eth\"}"
    local eth_response
    eth_response=$(bitget_api "POST" "/api/v2/spot/trade/place-order" "$eth_order_body")

    if echo "$eth_response" | jq -e '.code == "00000"' > /dev/null 2>&1; then
        local eth_order_id=$(echo "$eth_response" | jq -r '.data.orderId')
        log_success "ETH order placed: $eth_order_id"
    else
        log_error "ETH order failed: $(echo "$eth_response" | jq -r '.msg // .message // "Unknown error"')"
    fi

    # SOL Buy
    log_info "Placing SOL buy order ($sol_size SOL @ $sol_buy_price)..."
    local sol_order_body="{\"symbol\":\"$SOL_PAIR\",\"side\":\"buy\",\"orderType\":\"limit\",\"size\":\"$sol_size\",\"price\":\"$sol_buy_price\",\"force\":\"ioc\",\"clientOid\":\"test-$timestamp-sol\"}"
    local sol_response
    sol_response=$(bitget_api "POST" "/api/v2/spot/trade/place-order" "$sol_order_body")

    if echo "$sol_response" | jq -e '.code == "00000"' > /dev/null 2>&1; then
        local sol_order_id=$(echo "$sol_response" | jq -r '.data.orderId')
        log_success "SOL order placed: $sol_order_id"
    else
        log_error "SOL order failed: $(echo "$sol_response" | jq -r '.msg // .message // "Unknown error"')"
    fi

    # Wait for fills
    log_info "Waiting 3 seconds for orders to fill..."
    sleep 3

    # Show updated balances
    cmd_balances
}

cmd_sell() {
    log_info "Selling ALL crypto back to $QUOTE_CURRENCY on Bitget..."

    # Get current balances
    local response
    response=$(bitget_api "GET" "/api/v2/spot/account/assets")

    # Extract non-zero balances for tradeable assets
    local assets_to_sell=()

    # Check BTC
    local btc_balance=$(echo "$response" | jq -r '.data[] | select(.coin == "BTC") | .available' 2>/dev/null || echo "0")
    if [ -n "$btc_balance" ] && [ "$btc_balance" != "0" ] && [ "$btc_balance" != "null" ]; then
        local btc_num=$(echo "$btc_balance" | awk '{printf "%.8f", $1}')
        if (( $(echo "$btc_num > 0.00001" | bc -l 2>/dev/null || echo "0") )); then
            log_info "Found BTC balance: $btc_balance"
            assets_to_sell+=("BTC:$btc_balance:$BTC_PAIR")
        fi
    fi

    # Check ETH
    local eth_balance=$(echo "$response" | jq -r '.data[] | select(.coin == "ETH") | .available' 2>/dev/null || echo "0")
    if [ -n "$eth_balance" ] && [ "$eth_balance" != "0" ] && [ "$eth_balance" != "null" ]; then
        local eth_num=$(echo "$eth_balance" | awk '{printf "%.8f", $1}')
        if (( $(echo "$eth_num > 0.0001" | bc -l 2>/dev/null || echo "0") )); then
            log_info "Found ETH balance: $eth_balance"
            assets_to_sell+=("ETH:$eth_balance:$ETH_PAIR")
        fi
    fi

    # Check SOL
    local sol_balance=$(echo "$response" | jq -r '.data[] | select(.coin == "SOL") | .available' 2>/dev/null || echo "0")
    if [ -n "$sol_balance" ] && [ "$sol_balance" != "0" ] && [ "$sol_balance" != "null" ]; then
        local sol_num=$(echo "$sol_balance" | awk '{printf "%.8f", $1}')
        if (( $(echo "$sol_num > 0.001" | bc -l 2>/dev/null || echo "0") )); then
            log_info "Found SOL balance: $sol_balance"
            assets_to_sell+=("SOL:$sol_balance:$SOL_PAIR")
        fi
    fi

    if [ ${#assets_to_sell[@]} -eq 0 ]; then
        log_info "No crypto assets to sell - already all $QUOTE_CURRENCY"
        return 0
    fi

    echo ""
    log_warn "Will sell the following assets:"
    for asset in "${assets_to_sell[@]}"; do
        IFS=':' read -r coin balance symbol <<< "$asset"
        echo "  $coin: $balance → $QUOTE_CURRENCY ($symbol)"
    done
    echo ""

    local timestamp=$(get_timestamp_ms)

    # Sell each asset
    for asset in "${assets_to_sell[@]}"; do
        IFS=':' read -r coin balance symbol <<< "$asset"

        # Truncate balance to correct precision per coin
        # Precision: BTCUSDC=5, ETHUSDC=4, SOLUSDC=2
        local precision=5  # default
        case "$coin" in
            BTC) precision=5 ;;
            ETH) precision=4 ;;
            SOL) precision=2 ;;
        esac
        local formatted_balance=$(python3 -c "import math; print(f'{math.floor(float($balance) * (10**$precision)) / (10**$precision):.{$precision}f}')")

        log_info "Selling $formatted_balance $coin..."

        local order_body="{\"symbol\":\"$symbol\",\"side\":\"sell\",\"orderType\":\"market\",\"size\":\"$formatted_balance\",\"clientOid\":\"sellall-$timestamp-$coin\"}"
        local sell_response
        sell_response=$(bitget_api "POST" "/api/v2/spot/trade/place-order" "$order_body")

        if echo "$sell_response" | jq -e '.code == "00000"' > /dev/null 2>&1; then
            local order_id=$(echo "$sell_response" | jq -r '.data.orderId')
            log_success "$coin sell order placed: $order_id"
        else
            log_error "$coin sell failed: $(echo "$sell_response" | jq -r '.msg // .message // "Unknown error"')"
        fi

        sleep 1  # Rate limit between orders
    done

    # Wait for fills
    log_info "Waiting 3 seconds for orders to fill..."
    sleep 3

    # Show final balances
    cmd_balances
}

cmd_round_trip() {
    log_info "Executing round-trip test (buy then sell)..."
    echo ""

    cmd_buy

    echo ""
    log_info "Waiting 5 seconds before selling back..."
    sleep 5
    echo ""

    cmd_sell

    echo ""
    log_success "Round-trip test complete"
}

# =============================================================================
# Help
# =============================================================================
show_help() {
    cat << 'EOF'
bitget-live-trade.sh - Execute Live Trades on Bitget

WARNING: This script executes REAL trades with REAL money!

USAGE:
  ./bitget-live-trade.sh <command>

COMMANDS:
  balances         Show current Bitget spot balances
  buy              Buy small test amounts of BTC/ETH/SOL
  sell             Sell all crypto back to USDT
  test-round-trip  Buy then immediately sell back

ENVIRONMENT VARIABLES (from global.env):
  BITGET_API_KEY      Required
  BITGET_API_SECRET   Required
  BITGET_PASSPHRASE   Required

TEST AMOUNTS:
  BTC: 0.0001 (~$10 at $100k)
  ETH: 0.003  (~$10 at $3.3k)
  SOL: 0.05   (~$10 at $200)
EOF
    exit 0
}

# =============================================================================
# Main
# =============================================================================
main() {
    local cmd="${1:-help}"

    case "$cmd" in
        balances)
            load_env
            cmd_balances
            ;;
        buy)
            load_env
            cmd_buy
            ;;
        sell)
            load_env
            cmd_sell
            ;;
        test-round-trip|round-trip)
            load_env
            cmd_round_trip
            ;;
        help|--help|-h)
            show_help
            ;;
        *)
            log_error "Unknown command: $cmd"
            show_help
            ;;
    esac
}

main "$@"
