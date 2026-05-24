#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SETS_DIR="$REPO_ROOT/data/mtgjson/sets"
SET_LIST="$REPO_ROOT/data/mtgjson/SetList.json"
MTGJSON_BASE="https://mtgjson.com/api/v5"

mkdir -p "$SETS_DIR"

if [ ! -f "$SET_LIST" ]; then
  echo "ERROR: SetList.json not found at $SET_LIST" >&2
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "ERROR: jq is required to extract token set codes from SetList.json" >&2
  exit 1
fi

mapfile -t CODES < <(
  jq -r '.data[]
    | select(.tokenSetCode != null and .tokenSetCode != "")
    | .code, .tokenSetCode' "$SET_LIST" | sort -u
)

if [ "${#CODES[@]}" -eq 0 ]; then
  echo "No token-bearing set codes found in SetList.json."
  exit 0
fi

FORCE="${PHASE_REFRESH_MTGJSON:-0}"
DOWNLOADED=0
SKIPPED=0
FAILED=0

for CODE in "${CODES[@]}"; do
  DEST="$SETS_DIR/$CODE.json"
  MISSING="$DEST.missing"
  if [ -f "$DEST" ] && [ "$FORCE" != "1" ]; then
    SKIPPED=$((SKIPPED + 1))
    continue
  fi
  if [ -f "$MISSING" ] && [ "$FORCE" != "1" ]; then
    SKIPPED=$((SKIPPED + 1))
    continue
  fi
  rm -f "$MISSING"

  if curl -fsSL "$MTGJSON_BASE/$CODE.json.gz" 2>/dev/null | gunzip > "$DEST.tmp" 2>/dev/null; then
    mv "$DEST.tmp" "$DEST"
    DOWNLOADED=$((DOWNLOADED + 1))
  elif curl -fsSL "$MTGJSON_BASE/$CODE.json" -o "$DEST.tmp" 2>/dev/null; then
    mv "$DEST.tmp" "$DEST"
    DOWNLOADED=$((DOWNLOADED + 1))
  else
    rm -f "$DEST.tmp"
    touch "$MISSING"
    echo "Warning: failed to download $CODE.json" >&2
    FAILED=$((FAILED + 1))
  fi
done

echo "Token sets: downloaded $DOWNLOADED, skipped $SKIPPED, failed $FAILED"
