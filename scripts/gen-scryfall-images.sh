#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib/scryfall-fetch.sh"

DATA_DIR="data/scryfall"
ORACLE_FILE="$DATA_DIR/oracle-cards.json"
OUTPUT="client/public/scryfall-data.json"

echo "=== Scryfall Data Generation ==="

# Download oracle-cards bulk data if not present
if [ ! -f "$ORACLE_FILE" ]; then
  echo "Downloading Scryfall oracle-cards bulk data..."
  mkdir -p "$DATA_DIR"
  scryfall_fetch_bulk oracle_cards "$ORACLE_FILE"
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
# Keys (all lowercased):
#   1. The card's `oracle_id` (Scryfall's stable per-card identifier). This is
#      the *canonical* lookup path — the engine carries `printed_ref.oracle_id`
#      on every battlefield object and the frontend resolves images by it.
#      Keying by oracle_id sidesteps the name-asymmetry trap that breaks
#      MDFCs played as their Scryfall-back face (e.g. Mystic Peak, the back
#      face of "Pinnacle Monk // Mystic Peak", was unreachable when keyed by
#      `card_faces[0].name` alone).
#   2. The card's display name (`$card.name`). Retained for legacy callers
#      that only have a card name in scope (lobby, deck builder, hand UI for
#      face-down cards) and for cards loaded into the engine without a
#      printed_ref (synthesized objects, future paths).
#   3. The front-face name (`$card.card_faces[0].name`) when it differs from
#      `$card.name`. Same legacy rationale.
#
# Back-face names are NOT keys — they would collide across cards (e.g. an
# art_series "Forest // Forest" overwriting basic Forest). The oracle_id
# path supersedes the back-face-name use case anyway.
#
# Non-playable layouts (token, emblem, art_series, etc.) are excluded from the
# main card entries to prevent name collisions with real cards.
#
# Each entry value contains:
#   - oracle_id        — Scryfall's stable per-card id (mirrors the key path)
#   - face_names       — lowercased face names in Scryfall's card_faces order;
#                        single-element when the card has no `card_faces`.
#                        Used by the frontend to resolve `faceIndex` from the
#                        engine-reported `printed_ref.face_name`.
#   - faces            — array of {normal, art_crop} per face (image URLs)
#   - layout           — Scryfall layout string; the frontend uses this for
#                        presentation-only orientation such as sideways split
#                        cards, including Room cards.
#   - name, mana_cost, cmc, type_line, colors, color_identity, keywords
#
# Token entries are included separately with a "token:" prefix key to avoid
# name collisions (e.g., a card named "Saproling" vs a Saproling token).
# The frontend resolves token images via this prefix key, avoiding live
# Scryfall API calls at runtime.
NON_PLAYABLE='["token","double_faced_token","emblem","art_series","vanguard","scheme","planar","augment","host"]'

jq -c --argjson exclude "$NON_PLAYABLE" "$SCRYFALL_JQ_PRELUDE"'
  # Playable cards
  ([.[] |
    select(.layout as $l | $exclude | index($l) | not) |
    . as $card |
    {
      oracle_id: $card.oracle_id,
      face_names: (if $card.card_faces then
        [$card.card_faces[] | .name | js_downcase]
      else
        [$card.name | js_downcase]
      end),
      faces: (if $card.card_faces then
        [$card.card_faces[] | {
          normal: (.image_uris.normal // $card.image_uris.normal),
          art_crop: (.image_uris.art_crop // $card.image_uris.art_crop)
        }]
      else
        [{normal: $card.image_uris.normal, art_crop: $card.image_uris.art_crop}]
      end),
      layout: $card.layout,
      name: $card.name,
      mana_cost: ($card.mana_cost // $card.card_faces[0].mana_cost // ""),
      cmc: $card.cmc,
      type_line: $card.type_line,
      colors: ($card.colors // $card.card_faces[0].colors // []),
      color_identity: $card.color_identity,
      keywords: ($card.keywords // [])
    } as $entry |
    (
      [$card.oracle_id | ascii_downcase] +
      [$card.name | js_downcase] +
      if $card.card_faces and ($card.card_faces[0].name != $card.name)
      then [$card.card_faces[0].name | js_downcase]
      else [] end
    ) | unique[] |
    {key: ., value: $entry}
  ]) +
  # Token entries (keyed with "token:" prefix)
  ([.[] |
    select(.layout == "token") |
    select(.image_uris.normal != null) |
    . as $tok |
    {
      oracle_id: $tok.oracle_id,
      face_names: [$tok.name | js_downcase],
      faces: [{normal: $tok.image_uris.normal, art_crop: $tok.image_uris.art_crop}],
      layout: $tok.layout,
      name: $tok.name,
      mana_cost: "",
      cmc: 0,
      type_line: $tok.type_line,
      colors: ($tok.colors // []),
      color_identity: ($tok.color_identity // []),
      keywords: ($tok.keywords // []),
      power: ($tok.power // null),
      toughness: ($tok.toughness // null)
    } as $entry |
    {key: ("token:" + ($tok.name | js_downcase)), value: $entry}
  ]) | from_entries
' "$ORACLE_FILE" > "$OUTPUT"

ENTRY_COUNT=$(jq 'length' "$OUTPUT")
FILE_SIZE=$(du -h "$OUTPUT" | cut -f1)
echo "Generated $OUTPUT ($FILE_SIZE, $ENTRY_COUNT entries)"
