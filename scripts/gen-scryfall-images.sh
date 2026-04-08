#!/usr/bin/env bash
set -euo pipefail

DATA_DIR="data/scryfall"
ORACLE_FILE="$DATA_DIR/oracle-cards.json"
OUTPUT="client/public/scryfall-data.json"

echo "=== Scryfall Data Generation ==="

# Download oracle-cards bulk data if not present
if [ ! -f "$ORACLE_FILE" ]; then
  echo "Downloading Scryfall oracle-cards bulk data..."
  mkdir -p "$DATA_DIR"
  DOWNLOAD_URI=$(curl -s "https://api.scryfall.com/bulk-data" \
    | jq -r '.data[] | select(.type == "oracle_cards") | .download_uri')
  curl -L -o "$ORACLE_FILE" "$DOWNLOAD_URI"
  echo "Downloaded $ORACLE_FILE."
fi

if [ -f "$OUTPUT" ]; then
  echo "Skipping generation — $OUTPUT already exists (delete to regenerate)."
  exit 0
fi

echo "Generating $OUTPUT..."
mkdir -p "$(dirname "$OUTPUT")"

# Build a combined image + card metadata map from oracle-cards bulk data.
#
# Keys: lowercased card name + front face name when it differs (e.g. so the
# engine can look up "Delver of Secrets" and get the correct DFC entry).
# Only the FRONT face is indexed — back faces are never passed as lookup keys
# by the engine (it always uses the front-face name + faceIndex for the back).
# Indexing back faces causes collisions with standalone card names (e.g. an
# art_series "Forest // Forest" would overwrite the basic Forest entry).
#
# Non-playable layouts (token, emblem, art_series, etc.) are excluded entirely
# to prevent name collisions with real cards (e.g. a token named "Llanowar Elves"
# overwriting the actual Llanowar Elves).
#
# Each entry contains:
#   - faces: array of {normal, art_crop} per face (image URLs)
#   - name, mana_cost, cmc, type_line, colors, color_identity, keywords (card metadata)
NON_PLAYABLE='["token","double_faced_token","emblem","art_series","vanguard","scheme","planar","augment","host"]'

jq -c --argjson exclude "$NON_PLAYABLE" '[.[] |
  select(.layout as $l | $exclude | index($l) | not) |
  . as $card |
  {
    faces: (if $card.card_faces then
      [$card.card_faces[] | {
        normal: (.image_uris.normal // $card.image_uris.normal),
        art_crop: (.image_uris.art_crop // $card.image_uris.art_crop)
      }]
    else
      [{normal: $card.image_uris.normal, art_crop: $card.image_uris.art_crop}]
    end),
    name: $card.name,
    mana_cost: ($card.mana_cost // $card.card_faces[0].mana_cost // ""),
    cmc: $card.cmc,
    type_line: $card.type_line,
    colors: ($card.colors // $card.card_faces[0].colors // []),
    color_identity: $card.color_identity,
    keywords: ($card.keywords // [])
  } as $entry |
  (
    [$card.name | ascii_downcase] +
    if $card.card_faces and ($card.card_faces[0].name != $card.name)
    then [$card.card_faces[0].name | ascii_downcase]
    else [] end
  ) | unique[] |
  {key: ., value: $entry}
] | from_entries' "$ORACLE_FILE" > "$OUTPUT"

ENTRY_COUNT=$(jq 'length' "$OUTPUT")
FILE_SIZE=$(du -h "$OUTPUT" | cut -f1)
echo "Generated $OUTPUT ($FILE_SIZE, $ENTRY_COUNT entries)"
