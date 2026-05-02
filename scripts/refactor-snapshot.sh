#!/usr/bin/env bash
# Oracle parser refactor — snapshot harness.
#
# This is the byte-equality guard rail for the AST refactor (see PLAN.md §6).
# Every phase of the refactor must produce card-data.json output that is
# structurally identical to the pre-refactor baseline.
#
# Usage:
#   ./scripts/refactor-snapshot.sh capture       # capture a fresh baseline (rare)
#   ./scripts/refactor-snapshot.sh check         # regen current + diff against baseline (default)
#   ./scripts/refactor-snapshot.sh check-fast    # diff existing client/public/card-data.json (no regen)
#   ./scripts/refactor-snapshot.sh               # alias for `check`
#
# Baseline lives at data/refactor-snapshots/baseline.json (gitignored under
# data/*). Capture once at the start of the refactor, never re-capture except
# via an explicit "rebaseline" PR with reviewer sign-off after a known-good
# slice. The whole point is byte-identical output.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BASELINE="$REPO_ROOT/data/refactor-snapshots/baseline.json"
SNAPSHOT_DIR="$REPO_ROOT/data/refactor-snapshots"
CURRENT_TMP="$SNAPSHOT_DIR/current.json"
CARD_DATA_OUT="$REPO_ROOT/client/public/card-data.json"
DATA_DIR="$REPO_ROOT/data"

mode="${1:-check}"

regen_card_data() {
  echo "Regenerating card-data.json via oracle-gen..."
  local features="cli"
  if [ -n "${PHASE_FORGE_PATH:-}" ] || [ -d "$DATA_DIR/forge-cardsfolder" ]; then
    features="cli,forge"
  fi
  cargo run --quiet --profile tool --bin oracle-gen --features "$features" -- "$DATA_DIR" \
    > "$CARD_DATA_OUT.tmp"
  if ! jq -e 'type == "object" and length > 0' "$CARD_DATA_OUT.tmp" >/dev/null 2>&1; then
    echo "ERROR: regenerated card-data.json is empty or not a JSON object" >&2
    rm -f "$CARD_DATA_OUT.tmp"
    exit 1
  fi
  mv -f "$CARD_DATA_OUT.tmp" "$CARD_DATA_OUT"
}

normalize() {
  local src="$1"
  local dst="$2"
  jq -S . "$src" > "$dst"
}

case "$mode" in
  capture)
    if [ -f "$BASELINE" ]; then
      echo "Refusing to overwrite existing baseline at $BASELINE."
      echo "Delete it first if a re-capture is genuinely intended."
      exit 1
    fi
    if [ ! -f "$CARD_DATA_OUT" ]; then
      regen_card_data
    fi
    mkdir -p "$SNAPSHOT_DIR"
    normalize "$CARD_DATA_OUT" "$BASELINE"
    bytes=$(wc -c < "$BASELINE" | tr -d ' ')
    cards=$(jq 'length' "$BASELINE")
    echo "Captured baseline: $BASELINE ($bytes bytes, $cards cards)"
    ;;

  check|check-fast)
    if [ ! -f "$BASELINE" ]; then
      echo "ERROR: no baseline at $BASELINE." >&2
      echo "Capture one first: ./scripts/refactor-snapshot.sh capture" >&2
      exit 1
    fi
    if [ "$mode" = "check" ]; then
      regen_card_data
    else
      if [ ! -f "$CARD_DATA_OUT" ]; then
        echo "ERROR: no card-data.json at $CARD_DATA_OUT." >&2
        echo "Run ./scripts/gen-card-data.sh first, or use 'check' instead of 'check-fast'." >&2
        exit 1
      fi
    fi
    mkdir -p "$SNAPSHOT_DIR"
    normalize "$CARD_DATA_OUT" "$CURRENT_TMP"

    # Fast path: compare hashes. If identical, we're done with no diff cost.
    base_hash=$(shasum -a 256 "$BASELINE"   | awk '{print $1}')
    curr_hash=$(shasum -a 256 "$CURRENT_TMP" | awk '{print $1}')
    if [ "$base_hash" = "$curr_hash" ]; then
      rm -f "$CURRENT_TMP"
      echo "PASS: card-data.json is byte-identical to baseline."
      exit 0
    fi

    echo "FAIL: card-data.json differs from baseline." >&2
    echo "Baseline: $BASELINE ($base_hash)" >&2
    echo "Current : $CURRENT_TMP ($curr_hash)" >&2
    echo "" >&2
    echo "First 200 lines of diff:" >&2
    diff "$BASELINE" "$CURRENT_TMP" | head -200 >&2 || true
    echo "" >&2
    echo "Full diff: diff $BASELINE $CURRENT_TMP" >&2
    exit 1
    ;;

  *)
    echo "Usage: $0 [capture|check]" >&2
    exit 2
    ;;
esac
