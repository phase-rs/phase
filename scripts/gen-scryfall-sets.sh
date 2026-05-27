#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib/scryfall-fetch.sh"

DATA_DIR="data/scryfall"
SETS_FILE="$DATA_DIR/sets.json"
OUTPUT="client/public/scryfall-sets.json"

echo "=== Scryfall Sets Data Generation ==="

# Download sets data if not present
if [ ! -f "$SETS_FILE" ]; then
  echo "Downloading Scryfall sets data..."
  mkdir -p "$DATA_DIR"
  scryfall_download "https://api.scryfall.com/sets" "$SETS_FILE"
  echo "Downloaded $SETS_FILE."
fi

if [ -f "$OUTPUT" ]; then
  echo "Skipping generation — $OUTPUT already exists (delete to regenerate)."
  exit 0
fi

echo "Generating $OUTPUT..."
mkdir -p "$(dirname "$OUTPUT")"

# Build a map of set_code → { name, icon_svg_uri, released_at, set_type }.
jq -c '.data | map({key: .code, value: {
  name: .name,
  icon_svg_uri: .icon_svg_uri,
  released_at: .released_at
}}) | from_entries' "$SETS_FILE" > "$OUTPUT"

ENTRY_COUNT=$(jq 'length' "$OUTPUT")
FILE_SIZE=$(du -h "$OUTPUT" | cut -f1)
echo "Generated $OUTPUT ($FILE_SIZE, $ENTRY_COUNT entries)"
