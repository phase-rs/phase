#!/usr/bin/env bash
# Downloads per-set MTGJSON JSON files for all draftable sets.
# Files are saved to data/mtgjson/sets/ (gitignored).
#
# Usage:
#   ./scripts/fetch-draft-sets.sh                  # all draftable sets
#   ./scripts/fetch-draft-sets.sh DSK BLB OTJ      # specific sets only
#   ./scripts/fetch-draft-sets.sh --force DSK       # re-download even if exists
#
# Requires data/mtgjson/SetList.json to exist (run ./scripts/gen-card-data.sh first).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SETS_DIR="$REPO_ROOT/data/mtgjson/sets"
SET_LIST="$REPO_ROOT/data/mtgjson/SetList.json"
MTGJSON_BASE="https://mtgjson.com/api/v5"

FORCE=false
REQUESTED_SETS=()

# Parse arguments
for arg in "$@"; do
    if [ "$arg" = "--force" ]; then
        FORCE=true
    else
        REQUESTED_SETS+=("$arg")
    fi
done

mkdir -p "$SETS_DIR"

if [ ! -f "$SET_LIST" ]; then
    echo "ERROR: SetList.json not found at $SET_LIST" >&2
    echo "       Run ./scripts/gen-card-data.sh first." >&2
    exit 1
fi

# Determine which set codes to download
if [ ${#REQUESTED_SETS[@]} -gt 0 ]; then
    CODES=("${REQUESTED_SETS[@]}")
else
    # Extract draftable set codes from SetList.json
    if command -v jq &>/dev/null; then
        mapfile -t CODES < <(jq -r '.data[]
            | select(.type == "core" or .type == "expansion" or .type == "draft_innovation" or .type == "masters" or .type == "funny")
            | .code' "$SET_LIST" 2>/dev/null)
    else
        # Fallback: extract codes for known draftable types using grep/sed
        CODES=()
        while IFS= read -r code; do
            CODES+=("$code")
        done < <(grep -oE '"code":"[A-Z0-9]+"' "$SET_LIST" | sed 's/"code":"//;s/"//')
        echo "Warning: jq not found, using fallback extraction (may include non-draftable sets)" >&2
    fi
fi

if [ ${#CODES[@]} -eq 0 ]; then
    echo "No set codes found to download." >&2
    exit 1
fi

echo "Will process ${#CODES[@]} sets..."
DOWNLOADED=0
SKIPPED=0
FAILED=0

for CODE in "${CODES[@]}"; do
    DEST="$SETS_DIR/$CODE.json"

    # Skip if already downloaded (unless --force)
    if [ -f "$DEST" ] && [ "$FORCE" = false ]; then
        SKIPPED=$((SKIPPED + 1))
        continue
    fi

    # Download gzipped file, decompress
    if curl -fsSL "${MTGJSON_BASE}/${CODE}.json.gz" 2>/dev/null | gunzip > "$DEST.tmp" 2>/dev/null; then
        mv "$DEST.tmp" "$DEST"
        SIZE=$(du -h "$DEST" | cut -f1 | tr -d ' ')
        echo "Downloaded ${CODE}.json (${SIZE})"
        DOWNLOADED=$((DOWNLOADED + 1))
    else
        # Try uncompressed fallback
        if curl -fsSL "${MTGJSON_BASE}/${CODE}.json" -o "$DEST.tmp" 2>/dev/null; then
            mv "$DEST.tmp" "$DEST"
            SIZE=$(du -h "$DEST" | cut -f1 | tr -d ' ')
            echo "Downloaded ${CODE}.json (${SIZE})"
            DOWNLOADED=$((DOWNLOADED + 1))
        else
            rm -f "$DEST.tmp"
            echo "Warning: failed to download ${CODE}.json, skipping" >&2
            FAILED=$((FAILED + 1))
        fi
    fi
done

echo ""
echo "Summary: downloaded $DOWNLOADED, skipped $SKIPPED (already exist), failed $FAILED"
echo "Sets directory: $SETS_DIR"
