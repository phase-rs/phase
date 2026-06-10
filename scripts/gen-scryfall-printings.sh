#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib/scryfall-fetch.sh"

DATA_DIR="data/scryfall"
CARDS_FILE="${SCRYFALL_DEFAULT_CARDS_FILE:-$DATA_DIR/default-cards.json}"
OUTPUT="${SCRYFALL_PRINTINGS_OUTPUT:-client/public/scryfall-printings.json}"

echo "=== Scryfall Printings Generation ==="

if [ ! -f "$CARDS_FILE" ]; then
  echo "Downloading Scryfall default-cards bulk data..."
  mkdir -p "$DATA_DIR"
  scryfall_fetch_bulk default_cards "$CARDS_FILE"
  echo "Downloaded $CARDS_FILE."
fi

if [ -f "$OUTPUT" ]; then
  echo "Skipping generation — $OUTPUT already exists (delete to regenerate)."
  exit 0
fi

echo "Generating $OUTPUT..."
mkdir -p "$(dirname "$OUTPUT")"

# Build a printings index keyed by oracle_id.
#
# Only cards with >1 unique artwork are included (nothing to pick from
# otherwise). Each oracle_id maps to an array of PrintingEntry objects
# sorted by released_at descending (newest first).
#
# DFC handling: when top-level image_uris is null, per-face URLs are
# extracted from card_faces[].image_uris.
#
# Reversible cards omit root-level `oracle_id`; group by
# `card_faces[0].oracle_id` instead (issue #2031).
#
# Non-playable layouts (token, emblem, art_series, etc.) are excluded.
NON_PLAYABLE='["token","double_faced_token","emblem","art_series","vanguard","scheme","planar","augment","host"]'

jq -c --argjson exclude "$NON_PLAYABLE" '
  [.[] |
    (.oracle_id // .card_faces[0].oracle_id) as $oracle_id |
    select($oracle_id != null) |
    select(.layout as $l | $exclude | index($l) | not) |
    select(.set != "plst") |
    {
      oracle_id: $oracle_id,
      entry: {
        id: .id,
        set: .set,
        set_name: .set_name,
        collector_number: .collector_number,
        released_at: .released_at,
        border_color: .border_color,
        frame_effects: (.frame_effects // []),
        full_art: (.full_art // false),
        faces: (if .card_faces then
          [.card_faces[] | {
            normal: (.image_uris.normal // null),
            art_crop: (.image_uris.art_crop // null)
          }]
        else
          [{normal: .image_uris.normal, art_crop: .image_uris.art_crop}]
        end)
      }
    }
  ] |
  group_by(.oracle_id) |
  [.[] | select(length > 1)] |
  map({
    key: .[0].oracle_id,
    value: ([.[].entry] | sort_by(.released_at) | reverse)
  }) |
  from_entries
' "$CARDS_FILE" > "$OUTPUT"

ENTRY_COUNT=$(jq 'length' "$OUTPUT")
FILE_SIZE=$(du -h "$OUTPUT" | cut -f1)
echo "Generated $OUTPUT ($FILE_SIZE, $ENTRY_COUNT oracle_ids with multiple artworks)"
