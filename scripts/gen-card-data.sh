#!/usr/bin/env bash
set -euo pipefail

DATA_DIR="data"
OUTPUT="client/public/card-data.json"
NAMES_OUTPUT="client/public/card-names.json"

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
cargo run --release --bin oracle-gen -- "$DATA_DIR" --stats --names-out "$NAMES_OUTPUT" > "$OUTPUT"

# Summary
FILE_SIZE=$(du -h "$OUTPUT" | cut -f1)
NAMES_SIZE=$(du -h "$NAMES_OUTPUT" | cut -f1)
CARD_COUNT=$(grep -o '"name"' "$OUTPUT" | wc -l | tr -d ' ')
echo "Generated $OUTPUT ($FILE_SIZE, ~$CARD_COUNT cards)"
echo "Generated $NAMES_OUTPUT ($NAMES_SIZE)"
