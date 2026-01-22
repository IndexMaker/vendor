#!/bin/bash
# =============================================================================
# sync-interfaces.sh - Interface Synchronization Module
# =============================================================================
# Story 1.5 Task 2: Sync interfaces from vaultworks to vendor/backend
#
# This script copies ALL interface files from the vaultworks source of truth
# to the vendor and backend repositories.
#
# CRITICAL: The following interfaces MUST be synced (previously missing):
#   - steward.rs       - Required for FR49 (asset ID extraction)
#   - vault_native_claims.rs - Required for claims operations
#   - vault_native_orders.rs - Required for orders operations
#
# Usage: ./sync-interfaces.sh [--dry-run] [--vendor-only] [--backend-only]
# =============================================================================

set -e

# =============================================================================
# Configuration
# =============================================================================
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Go up two levels from vendor/scripts to project root (feb26/)
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Source and target directories
SOURCE_DIR="$PROJECT_ROOT/vaultworks/libs/common-contracts/src/interfaces"
VENDOR_TARGET="$PROJECT_ROOT/vendor/libs/common/src/interfaces"
BACKEND_TARGET="$PROJECT_ROOT/indexmaker-backend/libs/common/src/interfaces"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# =============================================================================
# Utility Functions
# =============================================================================
log_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
log_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1" >&2; }
die() { log_error "$1"; exit 1; }

# =============================================================================
# Options
# =============================================================================
DRY_RUN=false
VENDOR_ONLY=false
BACKEND_ONLY=false

while [[ "$#" -gt 0 ]]; do
    case $1 in
        --dry-run)      DRY_RUN=true ;;
        --vendor-only)  VENDOR_ONLY=true ;;
        --backend-only) BACKEND_ONLY=true ;;
        --help|-h)
            echo "Usage: $0 [--dry-run] [--vendor-only] [--backend-only]"
            exit 0
            ;;
        *) die "Unknown option: $1" ;;
    esac
    shift
done

# =============================================================================
# Pre-flight Validation
# =============================================================================
validate_source() {
    log_info "Validating source directory..."

    if [ ! -d "$SOURCE_DIR" ]; then
        die "Source directory not found: $SOURCE_DIR"
    fi

    # List source files
    SOURCE_FILES=$(ls -1 "$SOURCE_DIR"/*.rs 2>/dev/null | wc -l | tr -d ' ')
    if [ "$SOURCE_FILES" -eq 0 ]; then
        die "No .rs files found in source directory"
    fi

    log_info "Found $SOURCE_FILES interface files in vaultworks"

    # Check critical interfaces exist
    CRITICAL_INTERFACES=("steward.rs" "vault_native_claims.rs" "vault_native_orders.rs")
    for iface in "${CRITICAL_INTERFACES[@]}"; do
        if [ ! -f "$SOURCE_DIR/$iface" ]; then
            log_warn "CRITICAL interface missing in source: $iface"
        else
            log_info "  ✓ Critical: $iface"
        fi
    done
}

# =============================================================================
# Sync Function
# =============================================================================
sync_to_target() {
    local target_dir="$1"
    local target_name="$2"

    if [ ! -d "$target_dir" ]; then
        log_warn "Target directory does not exist: $target_dir"
        if [ "$DRY_RUN" = true ]; then
            log_info "[DRY-RUN] Would create: $target_dir"
        else
            mkdir -p "$target_dir"
            log_info "Created directory: $target_dir"
        fi
    fi

    # Count files before
    local before_count=0
    if [ -d "$target_dir" ]; then
        before_count=$(ls -1 "$target_dir"/*.rs 2>/dev/null | wc -l | tr -d ' ')
    fi

    log_info "Syncing to $target_name..."
    log_info "  Before: $before_count files"

    # List files being synced
    for src_file in "$SOURCE_DIR"/*.rs; do
        local filename=$(basename "$src_file")
        local target_file="$target_dir/$filename"

        if [ "$DRY_RUN" = true ]; then
            if [ -f "$target_file" ]; then
                # Check if different
                if ! diff -q "$src_file" "$target_file" &>/dev/null; then
                    log_info "  [DRY-RUN] Would update: $filename"
                fi
            else
                log_info "  [DRY-RUN] Would add: $filename"
            fi
        else
            cp "$src_file" "$target_file"
        fi
    done

    if [ "$DRY_RUN" = false ]; then
        local after_count=$(ls -1 "$target_dir"/*.rs 2>/dev/null | wc -l | tr -d ' ')
        log_info "  After: $after_count files"
        log_success "Synced $after_count interfaces to $target_name"
    fi
}

# =============================================================================
# Main
# =============================================================================
main() {
    echo ""
    echo "=============================================="
    echo "  Interface Synchronization"
    echo "=============================================="
    echo ""

    if [ "$DRY_RUN" = true ]; then
        log_warn "DRY-RUN MODE - No files will be modified"
    fi

    validate_source

    # Sync to vendor
    if [ "$BACKEND_ONLY" = false ]; then
        sync_to_target "$VENDOR_TARGET" "vendor"
    fi

    # Sync to backend (if exists)
    if [ "$VENDOR_ONLY" = false ]; then
        if [ -d "$PROJECT_ROOT/indexmaker-backend" ]; then
            sync_to_target "$BACKEND_TARGET" "indexmaker-backend"
        else
            log_info "Backend directory not found, skipping backend sync"
        fi
    fi

    echo ""
    log_success "Interface synchronization complete"

    # Summary
    echo ""
    echo "Summary:"
    echo "  Source:      $SOURCE_DIR"
    echo "  Vendor:      $VENDOR_TARGET"
    if [ -d "$PROJECT_ROOT/indexmaker-backend" ]; then
        echo "  Backend:     $BACKEND_TARGET"
    fi
}

main "$@"
