#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib/scryfall-fetch.sh"

DATA_DIR="data/scryfall"
CARDS_FILE="$DATA_DIR/default-cards.json"
OUTPUT="client/public/scryfall-token-images.json"

# Printings the token `layout` values do not carry — everything the type-line
# clause of `is_token_printing` below contributes. Zero of them means the table
# is empty or has lost the class; see the block comment on the pre-install gate.
count_off_layout_tokens() {
  jq '[.[]
       | select((.layout == "token" or .layout == "double_faced_token") | not)
       | .scryfall_id] | unique | length' "$1"
}

# The single authority for "is this table healthy", used by all three gates in
# this script: the skip-guard, the pre-install check, and the recovery branch
# inside scryfall_finalize_download (where the generic "is this JSON" would
# otherwise accept an empty table that beat us into place).
#
# `|| echo 0` would NOT normalise the failure, and neither does a string compare
# against "0": jq exits 0 with EMPTY stdout on a 0-byte or whitespace-only
# input, so the fallback never fires and `[ "" = 0 ]` reads as healthy. Both
# checks in this file got that wrong once. Keep the arithmetic compare.
validate_token_images() {
  local count
  count=$(count_off_layout_tokens "$1" 2>/dev/null) || count=0
  [ "${count:-0}" -gt 0 ]
}

echo "=== Scryfall Token Image Generation ==="

if [ ! -f "$CARDS_FILE" ]; then
  echo "Downloading Scryfall default-cards bulk data..."
  mkdir -p "$DATA_DIR"
  scryfall_fetch_bulk default_cards "$CARDS_FILE"
  echo "Downloaded $CARDS_FILE."
fi

# The sibling generators skip on existence alone. That is what let this table
# stay wrong: a checkout, cache or dev machine already holding a Role-less file
# never regenerates it, and this fix would reach nobody who had run the script
# before. Skip only a table that still passes the health check — a stale or
# truncated one costs 4s to rebuild, and a genuinely empty class fails loudly
# at the gate below instead of being certified here.
if [ -f "$OUTPUT" ] && validate_token_images "$OUTPUT"; then
  echo "Skipping generation — $OUTPUT already exists (delete to regenerate)."
  exit 0
fi

echo "Generating $OUTPUT..."
mkdir -p "$(dirname "$OUTPUT")"

# `layout` alone does not enumerate the token printings.
#
# Role tokens (Wilds of Eldraine) are printed two-to-a-card with the second
# Role upside down, so Scryfall classifies all 6 printings as "flip" rather
# than "token". They fell through the layout filter, so every Role preset in
# known-tokens.toml carried a `token_image_ref` that resolved to nothing and
# the token rendered artless in play.
#
# `is_token_printing` therefore keeps the two layout values as a fast path and
# adds a layout-agnostic third clause: a printing whose type line marks it as a
# token is a token, whatever Scryfall calls its layout. Three details are
# load-bearing:
#
#   * The clause is deliberately NOT conjoined with `layout == "flip"`. Roles
#     are not alone out here — the Secret Lair Mechtitan is
#     `layout: "reversible_card"` with a null top-level type_line and
#     token-typed faces, and two presets point at it. Gating on "flip" would
#     fix the Role instance and leave the class, which is the shape of bug
#     this branch exists to end. Today it admits exactly 7 printings that the
#     two layout values do not (the type-line test itself is true for 2797).
#   * The type line is what keeps the genuine flip *cards* (21 of them,
#     39 printings — Budoka Gardener, Erayo, Akki Lavarunner, ...) out of a
#     table of tokens. `set_type` cannot replace it: two of the twelve Role
#     presets point at the Wicked // Cursed printing in The List, whose
#     `set_type` is "masters", not "token".
#   * Both the top-level and the per-face type lines are tested, because
#     Scryfall does not reliably put the token marker on the top-level one:
#     81 of 120 `double_faced_token` entries lack a "Token "-prefixed
#     `type_line` (62 read "Card // Card"), and the Mechtitan printing above
#     has none at all. Those survive on `layout` today, but a top-level-only
#     test would silently drop the next multi-face token that has to come in
#     through this clause.
#
# Build into a temp and install it atomically, the same temp+rename the bulk
# downloads in lib/scryfall-fetch.sh use. Writing straight to $OUTPUT truncates
# it before jq runs, so an interrupt or an OOM on the 600 MB+ input leaves a
# 0-byte file that the skip-guard at the top of this script then certifies as
# done on every later run — the silent-empty-table failure this script has
# already had once, reached through the other door.
#
# The random run has to be the LAST component of the template. Accepting a
# suffix after it is a GNU extension, documented as such in GNU mktemp(1) under
# `--suffix` ("implied if TEMPLATE does not end in X"); the underlying
# mkstemp(3) contract is EINVAL unless the final six characters are XXXXXX
# (checked directly against libc: `$OUTPUT.XXXXXX.tmp` gives errno 22,
# `$OUTPUT.tmp.XXXXXX` succeeds). `$OUTPUT.XXXXXX.tmp` was reported failing on
# macOS in review of #7335, a platform this repo ships desktop builds for
# (shell-release.yml:164). Trailing X is portable everywhere and is the house
# form here: gen-card-data.sh:248 and lib/mtgjson-fetch.sh:70 both stage so.
#
# `.gitignore` carries `client/public/*.tmp.*` for the name that produces. The
# existing `client/public/*.tmp` covers the fixed-name staging files in
# gen-card-data.sh and does not match a randomised tail; with no rule at all, an
# interrupted run leaves an untracked 2 MB file in a tracked tree.
#
# `mktemp` creates at 0600. The mode is pinned to 644 because this file is
# served as a static asset alongside the other sidecars in client/public, and
# that must not depend on the umask of whoever ran the build.
TMP=$(mktemp "$OUTPUT.tmp.XXXXXX")
trap 'rm -f "$TMP"' EXIT INT TERM
chmod 644 "$TMP"

# NOTE: the jq program below is a single-quoted shell string. An apostrophe
# anywhere inside it, comments included, terminates the string.

jq -c "$SCRYFALL_JQ_PRELUDE"'
  def is_token_printing:
    .layout == "token"
    or .layout == "double_faced_token"
    or ([.type_line?, (.card_faces[]?.type_line?)]
        | any(.[] | strings; startswith("Token ")));

  [.[] |
    select(is_token_printing) |
    select(.id != null) |
    . as $card |
    {
      scryfall_id: $card.id,
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
' "$CARDS_FILE" > "$TMP"

# Everything the type-line clause contributes over the two layout values — the
# entries that carry no token `layout` at all. The Roles went unindexed for the
# life of this file because nothing ever noticed the class was empty: the table
# stayed plausible and the art simply never appeared. Fail generation instead,
# in the shape gen-scryfall-locale-images.sh uses for its own empty classes —
# an empty class means an upstream shape change to look at, not that the class
# stopped existing.
#
# This is a presence check, not a completeness one. Asserting that every
# `token_image_ref` in known-tokens.toml resolves is the stronger invariant and
# does hold as of this commit — but it does not belong in a deploy-blocking
# build step. known-tokens.toml is generated by tokens-gen from MTGJSON, while
# this table comes from a day-cached Scryfall snapshot; a new set whose tokens
# reach MTGJSON first would fail the assertion on the new refs with nothing
# actually wrong. That check belongs in a reporting job. A partial upstream loss
# therefore still gets through here.
if ! validate_token_images "$TMP"; then
  echo "ERROR: the type-line clause of is_token_printing selected nothing." >&2
  echo "  Either Scryfall reclassified these printings into a token 'layout', in" >&2
  echo "  which case confirm they still arrive and then drop the clause; or the" >&2
  echo "  'Token ' type-line prefix changed, in which case the Role tokens (Wilds" >&2
  echo "  of Eldraine) and the Secret Lair Mechtitan are about to render artless" >&2
  echo "  again. Do not just delete this check." >&2
  exit 1
fi
TYPE_LINE_COUNT=$(count_off_layout_tokens "$TMP")

scryfall_finalize_download "$TMP" "$OUTPUT" validate_token_images
trap - EXIT INT TERM

ENTRY_COUNT=$(jq 'length' "$OUTPUT")
FILE_SIZE=$(du -h "$OUTPUT" | cut -f1)
echo "Generated $OUTPUT ($FILE_SIZE, $ENTRY_COUNT entries, $TYPE_LINE_COUNT via type line)"
