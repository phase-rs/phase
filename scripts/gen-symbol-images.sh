#!/usr/bin/env bash
set -euo pipefail

# Downloads MTG symbol SVGs (mana, tap, untap, hybrids, phyrexian, etc.) from
# Scryfall's /symbology endpoint into client/public/mana-symbols/, so the app
# can render them locally instead of hotlinking on every use.
#
# Mirrors gen-scryfall-images.sh in spirit: a one-shot fetch that populates a
# cache under client/public/ which the runtime consumes.
OUTPUT_DIR="client/public/mana-symbols"
SYMBOLOGY_JSON="data/scryfall/symbology.json"

echo "=== Scryfall Symbol Download ==="

mkdir -p "$(dirname "$SYMBOLOGY_JSON")"
mkdir -p "$OUTPUT_DIR"

if [ ! -f "$SYMBOLOGY_JSON" ]; then
  echo "Fetching Scryfall symbology catalog..."
  curl -L -s -o "$SYMBOLOGY_JSON" "https://api.scryfall.com/symbology"
fi

# Each symbol entry has { symbol: "{T}", svg_uri: "https://..." }.
# We strip the braces and slashes to produce a filename matching the code
# used by client/src/components/mana/ManaSymbol.tsx (e.g. "W/U" -> "WU.svg").
MISSING=$(jq -r '
  .data[]
  | [(.symbol | gsub("[{}/]"; "")), .svg_uri]
  | @tsv
' "$SYMBOLOGY_JSON" | while IFS=$'\t' read -r code uri; do
  [ -z "$code" ] && continue
  dest="$OUTPUT_DIR/$code.svg"
  if [ ! -f "$dest" ]; then
    echo "$code"$'\t'"$uri"
  fi
done)

if [ -z "$MISSING" ]; then
  COUNT=$(find "$OUTPUT_DIR" -name '*.svg' | wc -l | tr -d ' ')
  echo "Symbols already cached ($COUNT in $OUTPUT_DIR)."
  exit 0
fi

echo "$MISSING" | while IFS=$'\t' read -r code uri; do
  [ -z "$code" ] && continue
  dest="$OUTPUT_DIR/$code.svg"
  curl -L -s -o "$dest" "$uri"
  # Scryfall asks for 50-100ms between requests; stay polite.
  sleep 0.1
done

COUNT=$(find "$OUTPUT_DIR" -name '*.svg' | wc -l | tr -d ' ')
echo "Downloaded symbols ($COUNT total in $OUTPUT_DIR)."
