#!/usr/bin/env bash
set -euo pipefail

FORGE_REPO_URL="https://github.com/Card-Forge/forge"
CARDS_DIR="data/cardsfolder"
OUTPUT="client/public/card-data.json"

echo "=== Card Data Generation ==="

# Step 1: Download Forge cardsfolder via sparse checkout
if [ -d "$CARDS_DIR" ]; then
  echo "Card files already present, skipping download."
  echo "Delete $CARDS_DIR to re-download."
else
  echo "Downloading Forge card definitions (sparse checkout)..."
  TEMP_DIR=$(mktemp -d)
  trap 'rm -rf "$TEMP_DIR"' EXIT

  git clone --depth 1 --filter=blob:none --sparse "$FORGE_REPO_URL" "$TEMP_DIR/forge"
  (cd "$TEMP_DIR/forge" && git sparse-checkout set forge-gui/res/cardsfolder)

  mkdir -p "$(dirname "$CARDS_DIR")"
  cp -r "$TEMP_DIR/forge/forge-gui/res/cardsfolder" "$CARDS_DIR"

  rm -rf "$TEMP_DIR"
  trap - EXIT

  echo "Downloaded card files to $CARDS_DIR"
fi

# Step 2: Build and run the card data exporter
echo "Exporting card data..."
mkdir -p "$(dirname "$OUTPUT")"
cargo run --release --bin card-data-export -- "$CARDS_DIR" > "$OUTPUT"

# Step 3: Summary
FILE_SIZE=$(du -h "$OUTPUT" | cut -f1)
CARD_COUNT=$(grep -o '"name"' "$OUTPUT" | wc -l | tr -d ' ')
echo "Generated $OUTPUT ($FILE_SIZE, ~$CARD_COUNT cards)"
