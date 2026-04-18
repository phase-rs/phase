#!/usr/bin/env bash
set -euo pipefail

# Load .env if present (for PHASE_FORGE_PATH, etc.)
if [ -f ".env" ]; then
  set -a; source .env; set +a
fi

DATA_DIR="data"
OUTPUT_DIR="client/public"
OUTPUT="${OUTPUT_DIR}/card-data.json"
NAMES_OUTPUT="${OUTPUT_DIR}/card-names.json"
COVERAGE_OUTPUT="${OUTPUT_DIR}/coverage-data.json"
COVERAGE_SUMMARY="${OUTPUT_DIR}/coverage-summary.json"
META_OUTPUT="${OUTPUT_DIR}/card-data-meta.json"

echo "=== Card Data Generation ==="

# Download MTGJSON AtomicCards if not present
MTGJSON_FILE="$DATA_DIR/mtgjson/AtomicCards.json"
if [ ! -f "$MTGJSON_FILE" ]; then
  echo "Downloading MTGJSON AtomicCards..."
  mkdir -p "$DATA_DIR/mtgjson"
  curl -L -o "$MTGJSON_FILE" "https://mtgjson.com/api/v5/AtomicCards.json"
  echo "Downloaded MTGJSON data."
fi

# Build and run the Oracle-based card data generator
echo "Generating card data from MTGJSON via Oracle text parser..."
mkdir -p "$(dirname "$OUTPUT")"

# Enable Forge bridge when cardsfolder is available
FEATURES="cli"
if [ -n "${PHASE_FORGE_PATH:-}" ] || [ -d "$DATA_DIR/forge-cardsfolder" ]; then
  FEATURES="cli,forge"
  echo "Forge bridge enabled"
fi

# Write to a .tmp sibling first, and only promote to the final path on success.
# This prevents failures mid-generation from clobbering a good prior output
# with an empty/partial file (truncate-on-open semantics of `>`).
TMP_FILES=()
cleanup_tmp() {
  for f in "${TMP_FILES[@]}"; do
    [ -e "$f" ] && rm -f "$f"
  done
}
trap cleanup_tmp EXIT

run_tool_with_recovery() {
  local output_file="$1"
  shift

  if "$@" > "$output_file"; then
    return 0
  fi

  echo "Tool profile build failed; clearing target/tool and retrying once..." >&2
  rm -rf target/tool
  "$@" > "$output_file"
}

OUTPUT_TMP="${OUTPUT}.tmp"
NAMES_OUTPUT_TMP="${NAMES_OUTPUT}.tmp"
COVERAGE_OUTPUT_TMP="${COVERAGE_OUTPUT}.tmp"
COVERAGE_SUMMARY_TMP="${COVERAGE_SUMMARY}.tmp"
META_OUTPUT_TMP="${META_OUTPUT}.tmp"
TMP_FILES=("$OUTPUT_TMP" "$NAMES_OUTPUT_TMP" "$COVERAGE_OUTPUT_TMP" "$COVERAGE_SUMMARY_TMP" "$META_OUTPUT_TMP")

run_tool_with_recovery \
  "$OUTPUT_TMP" \
  cargo run --profile tool --bin oracle-gen --features "$FEATURES" -- "$DATA_DIR" --stats --names-out "$NAMES_OUTPUT_TMP"
# Sanity-check the generated card data is non-empty JSON before promoting.
if [ ! -s "$OUTPUT_TMP" ] || ! jq -e 'type == "object" and length > 0' "$OUTPUT_TMP" >/dev/null 2>&1; then
  echo "Generated $OUTPUT_TMP is empty or not a valid card object; aborting." >&2
  exit 1
fi
if [ ! -s "$NAMES_OUTPUT_TMP" ]; then
  echo "Generated $NAMES_OUTPUT_TMP is empty; aborting." >&2
  exit 1
fi

echo "Generating card coverage data..."
run_tool_with_recovery \
  "$COVERAGE_OUTPUT_TMP" \
  cargo run --profile tool --bin coverage-report -- "$DATA_DIR" --all
if [ ! -s "$COVERAGE_OUTPUT_TMP" ] || ! jq -e '.' "$COVERAGE_OUTPUT_TMP" >/dev/null 2>&1; then
  echo "Generated $COVERAGE_OUTPUT_TMP is empty or not valid JSON; aborting." >&2
  exit 1
fi
jq '{total_cards, supported_cards, coverage_pct, coverage_by_format}' "$COVERAGE_OUTPUT_TMP" > "$COVERAGE_SUMMARY_TMP"

# Generate metadata sidecar with generation timestamp and parser commit
GEN_TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
GEN_COMMIT=$(git rev-parse HEAD 2>/dev/null || echo "unknown")
GEN_COMMIT_SHORT=$(git rev-parse --short HEAD 2>/dev/null || echo "unknown")
cat > "$META_OUTPUT_TMP" <<METAEOF
{"generated_at":"${GEN_TIMESTAMP}","commit":"${GEN_COMMIT}","commit_short":"${GEN_COMMIT_SHORT}"}
METAEOF

# All generation succeeded — atomically promote each .tmp to its final path.
mv -f "$OUTPUT_TMP"           "$OUTPUT"
mv -f "$NAMES_OUTPUT_TMP"     "$NAMES_OUTPUT"
mv -f "$COVERAGE_OUTPUT_TMP"  "$COVERAGE_OUTPUT"
mv -f "$COVERAGE_SUMMARY_TMP" "$COVERAGE_SUMMARY"
mv -f "$META_OUTPUT_TMP"      "$META_OUTPUT"
echo "Generated $META_OUTPUT"

# Summary
FILE_SIZE=$(du -h "$OUTPUT" | cut -f1)
NAMES_SIZE=$(du -h "$NAMES_OUTPUT" | cut -f1)
CARD_COUNT=$(grep -o '"name"' "$OUTPUT" | wc -l | tr -d ' ')
echo "Generated $OUTPUT ($FILE_SIZE, ~$CARD_COUNT cards)"
echo "Generated $NAMES_OUTPUT ($NAMES_SIZE)"
