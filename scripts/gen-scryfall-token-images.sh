#!/usr/bin/env bash
set -euo pipefail

DATA_DIR="data/scryfall"
CARDS_FILE="$DATA_DIR/default-cards.json"
OUTPUT="client/public/scryfall-token-images.json"

echo "=== Scryfall Token Image Generation ==="

if [ ! -f "$CARDS_FILE" ]; then
  echo "Downloading Scryfall default-cards bulk data..."
  mkdir -p "$DATA_DIR"
  DOWNLOAD_URI=$(curl -s "https://api.scryfall.com/bulk-data" \
    | jq -r '.data[] | select(.type == "default_cards") | .download_uri')
  curl -L -o "$CARDS_FILE" "$DOWNLOAD_URI"
  echo "Downloaded $CARDS_FILE."
fi

if [ -f "$OUTPUT" ]; then
  echo "Skipping generation — $OUTPUT already exists (delete to regenerate)."
  exit 0
fi

echo "Generating $OUTPUT..."
mkdir -p "$(dirname "$OUTPUT")"

jq -c '
  [.[] |
    select(.layout == "token" or .layout == "double_faced_token") |
    select(.id != null) |
    . as $card |
    {
      scryfall_id: $card.id,
      oracle_id: $card.oracle_id,
      face_names: (if $card.card_faces then
        [$card.card_faces[] | .name | ascii_downcase]
      else
        [$card.name | ascii_downcase]
      end),
      faces: (if $card.card_faces then
        [$card.card_faces[] | {
          normal: (.image_uris.normal // $card.image_uris.normal),
          art_crop: (.image_uris.art_crop // $card.image_uris.art_crop)
        }]
      else
        [{normal: $card.image_uris.normal, art_crop: $card.image_uris.art_crop}]
      end),
      name: $card.name,
      layout: $card.layout
    } as $entry |
    (
      [{key: ("scryfall:" + ($card.id | ascii_downcase)), value: $entry}] +
      if $card.oracle_id != null then
        ($entry.face_names | map({
          key: ("oracle:" + ($card.oracle_id | ascii_downcase) + ":" + .),
          value: $entry
        }))
      else [] end
    )[]
  ] | from_entries
' "$CARDS_FILE" > "$OUTPUT"

ENTRY_COUNT=$(jq 'length' "$OUTPUT")
FILE_SIZE=$(du -h "$OUTPUT" | cut -f1)
echo "Generated $OUTPUT ($FILE_SIZE, $ENTRY_COUNT entries)"
