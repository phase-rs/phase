#!/usr/bin/env bash
set -euo pipefail

DATA_DIR="data"
OUTPUT="client/public/card-data.json"

echo "=== Card Data Generation ==="

# Verify ability JSON files exist
if [ ! -d "$DATA_DIR/abilities" ]; then
  echo "Error: $DATA_DIR/abilities not found. Typed ability JSON files are required."
  exit 1
fi

# Download MTGJSON AtomicCards if not present
MTGJSON_FILE="$DATA_DIR/mtgjson/AtomicCards.json"
if [ ! -f "$MTGJSON_FILE" ]; then
  echo "Downloading MTGJSON AtomicCards..."
  mkdir -p "$DATA_DIR/mtgjson"
  curl -L -o "$MTGJSON_FILE" "https://mtgjson.com/api/v5/AtomicCards.json"
  echo "Downloaded MTGJSON data."
fi

# Build and run the JSON-based card data exporter
echo "Exporting card data from MTGJSON + ability JSON..."
mkdir -p "$(dirname "$OUTPUT")"
cargo run --release --bin json-export -- "$DATA_DIR" > "$OUTPUT"

# Summary
FILE_SIZE=$(du -h "$OUTPUT" | cut -f1)
CARD_COUNT=$(grep -o '"name"' "$OUTPUT" | wc -l | tr -d ' ')
echo "Generated $OUTPUT ($FILE_SIZE, ~$CARD_COUNT cards)"
