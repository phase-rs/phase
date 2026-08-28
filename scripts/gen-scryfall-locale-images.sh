#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib/mtgjson-fetch.sh"
source "$SCRIPT_DIR/lib/scryfall-fetch.sh"

DATA_DIR="data/mtgjson"
SETS_TAR="${MTGJSON_ALL_SET_FILES:-$DATA_DIR/AllSetFiles.tar}"
SETS_DIR="${MTGJSON_ALL_SETS_DIR:-$DATA_DIR/allsets}"
SCRYFALL_DATA_DIR="${SCRYFALL_DATA_DIR:-data/scryfall}"
ALL_CARDS_FILE="${SCRYFALL_ALL_CARDS_FILE:-$SCRYFALL_DATA_DIR/all-cards.json}"
OUTPUT_DIR="${SCRYFALL_LOCALE_IMAGES_OUTPUT_DIR:-client/public}"
SCHEMA_VERSION="v2"

# MTGJSON `foreignData.language` (full English language name) -> UI locale code.
# MUST stay in lockstep with `locale_code` in crates/engine/src/bin/oracle_gen.rs,
# which keys the sibling text sidecars (card-data.<lng>.json). A code present
# here but not there (or vice versa) ships localized art with English text or
# the reverse.
#
# Polish is deliberately absent: MTGJSON has zero Polish foreignData records and
# Scryfall rejects `lang:pl` outright ("Unknown language `pl`"), so `pl` — which
# IS in the frontend's SUPPORTED_LNGS — can never have localized card data from
# either source. Its chrome is translated; its cards stay English.
LOCALE_MAP='{
  "German": "de",
  "Spanish": "es",
  "French": "fr",
  "Italian": "it",
  "Portuguese (Brazil)": "pt"
}'

echo "=== Scryfall Locale Image Map Generation ==="

CODES=$(jq -r '.[]' <<< "$LOCALE_MAP" | sort)

validate_locale_map() {
  jq -e '
    type == "object"
    and all(
      .[];
      type == "object"
      and (.id | type == "string")
      and (.faces | type == "array")
      and (.faces | length > 0)
      and all(.faces[]; type == "object"
        and (.small | type == "string")
        and (.normal | type == "string")
        and (.art_crop | type == "string"))
    )
  ' "$1" > /dev/null
}

# Skip only when every locale output already exists — a partial set must
# regenerate, or a locale added to LOCALE_MAP would never be built. A cached
# output must also match this schema version's contract before we reuse it.
ALL_PRESENT=1
for code in $CODES; do
  out="$OUTPUT_DIR/scryfall-images.$SCHEMA_VERSION.$code.json"
  [ -f "$out" ] && validate_locale_map "$out" || ALL_PRESENT=0
done
if [ "$ALL_PRESENT" = 1 ]; then
  echo "Skipping generation — all $SCHEMA_VERSION locale maps already exist in $OUTPUT_DIR (delete to regenerate)."
  exit 0
fi

# Per-set files, not AllPrintings.json. Both artifacts are the same ~169 MB
# download, but a whole-file `jq` parse of AllPrintings peaks at ~3.5 GB RSS
# (measured, 623 MB of JSON) while parsing one set at a time peaks at ~95 MB —
# a 36x reduction that keeps this runnable on a standard CI runner.
if [ ! -d "$SETS_DIR" ]; then
  if [ ! -f "$SETS_TAR" ]; then
    echo "Downloading MTGJSON AllSetFiles..."
    mkdir -p "$DATA_DIR"
    # mtgjson_download appends `.gz`, so "AllSetFiles.tar" resolves to the
    # published AllSetFiles.tar.gz and is decompressed to the bare tar.
    mtgjson_download "AllSetFiles.tar" "$SETS_TAR"
    echo "Downloaded $SETS_TAR."
  fi
  echo "Extracting set files..."
  # Own a private directory: data/mtgjson/sets/ belongs to fetch-draft-sets.sh
  # and fetch-token-sets.sh, whose skip-if-exists logic would treat our files
  # as their own cache hits.
  mkdir -p "$SETS_DIR"
  tar -xf "$SETS_TAR" -C "$SETS_DIR" --strip-components=1
fi

SET_COUNT=$(find "$SETS_DIR" -name '*.json' | wc -l | tr -d ' ')
if [ "$SET_COUNT" = 0 ]; then
  echo "ERROR: no set files found in $SETS_DIR" >&2
  exit 1
fi
echo "Scanning $SET_COUNT set files..."

WORK_DIR=$(mktemp -d)
trap 'rm -rf "$WORK_DIR"' EXIT
PAIRS="$WORK_DIR/pairs.tsv"
UNIQUE_PAIRS="$WORK_DIR/pairs.unique.tsv"
LOCALE_FACES="$WORK_DIR/locale-faces.tsv"

# One pass per set file, streaming `<code>\t<english id>\t<localized id>`.
#
# `identifiers.scryfallId` on a foreignData record is the Scryfall id of the
# LOCALIZED printing (verified: MID Brutal Cathar's German id resolves to
# lang "de", printed_name "Brutaler Katharer"). The card's own
# `identifiers.scryfallId` is the English printing the frontend already
# resolved, which is what the runtime looks up.
: > "$PAIRS"
for set_file in "$SETS_DIR"/*.json; do
  jq -r --argjson locales "$LOCALE_MAP" '
    .data.cards[]?
    | select(.identifiers.scryfallId != null)
    | . as $card
    | .foreignData[]?
    | select(.identifiers.scryfallId != null)
    | ($locales[.language] // empty) as $code
    | "\($code)\t\($card.identifiers.scryfallId)\t\(.identifiers.scryfallId)"
  ' "$set_file" >> "$PAIRS"
done

# Different MTGJSON set files can describe the same printing, but they must
# not create duplicate rows in the generated maps or inflate the enrichment
# audit below. Sort before handing the IDs to jq so the all_cards scan joins a
# compact, stable set of requested localized printings.
LC_ALL=C sort -u "$PAIRS" > "$UNIQUE_PAIRS"

# MTGJSON gives the English -> localized-printing relationship, but not the
# CDN URL. The old generator reconstructed URLs from an undocumented path
# shape, which loses DFC face identity and assumes every Scryfall rendition
# uses the same filename. all_cards is Scryfall's source of truth for the
# exact small, normal, and art_crop URLs for each localized face.
if [ ! -f "$ALL_CARDS_FILE" ]; then
  echo "Downloading Scryfall all-cards bulk data..."
  mkdir -p "$SCRYFALL_DATA_DIR"
  scryfall_fetch_bulk all_cards "$ALL_CARDS_FILE"
  echo "Downloaded $ALL_CARDS_FILE."
fi

# Scan all_cards once without materializing its multi-gigabyte JSON array.
# `jq --stream` emits scalar leaves in document order; awk holds only the
# current card's three URLs per face and the compact MTGJSON mapping table.
# This preserves the bounded-memory reason this generator uses per-set files.
jq --stream -r '
  select(length == 2 and (.[1] | type == "string"))
  | .[0] as $path
  | select($path[0] | type == "number")
  | if ($path | length) == 2 and $path[1] == "id" then
      [$path[0], "id", "", .[1]]
    elif ($path | length) == 3
      and $path[1] == "image_uris"
      and ($path[2] == "small" or $path[2] == "normal" or $path[2] == "art_crop") then
      [$path[0], "root", $path[2], .[1]]
    elif ($path | length) == 5
      and $path[1] == "card_faces"
      and $path[3] == "image_uris"
      and ($path[4] == "small" or $path[4] == "normal" or $path[4] == "art_crop") then
      [$path[0], "face", ($path[2] | tostring) + ":" + $path[4], .[1]]
    else empty end
  | @tsv
' "$ALL_CARDS_FILE" \
  | awk -F'\t' -v targets="$UNIQUE_PAIRS" '
      function flush(    f, mapping, mappings, i, fields, face_count) {
        if (!(id in requested)) return
        face_count = max_face >= 0 ? max_face + 1 : 1
        for (f = 0; f < face_count; f++) {
          if (small[f] == "") small[f] = root_small
          if (normal[f] == "") normal[f] = root_normal
          if (crop[f] == "") crop[f] = root_crop
          if (small[f] == "" || normal[f] == "" || crop[f] == "") {
            printf "ERROR: localized Scryfall printing %s face %d lacks a required exact image URI.\n", id, f > "/dev/stderr"
            failed = 1
            continue
          }
          split(requested[id], mappings, "\034")
          for (i in mappings) {
            split(mappings[i], fields, "\037")
            print fields[1] "\t" fields[2] "\t" id "\t" f "\t" small[f] "\t" normal[f] "\t" crop[f]
          }
        }
      }
      BEGIN {
        while ((getline line < targets) > 0) {
          split(line, fields, "\t")
          requested[fields[3]] = requested[fields[3]] == "" ? fields[1] "\037" fields[2] : requested[fields[3]] "\034" fields[1] "\037" fields[2]
        }
        close(targets)
        current = -1
        max_face = -1
      }
      $1 != current {
        if (current >= 0) flush()
        delete small; delete normal; delete crop
        id = root_small = root_normal = root_crop = ""
        max_face = -1
        current = $1
      }
      $2 == "id" { id = $4; next }
      $2 == "root" {
        if ($3 == "small") root_small = $4
        else if ($3 == "normal") root_normal = $4
        else root_crop = $4
        next
      }
      $2 == "face" {
        split($3, fields, ":")
        f = fields[1] + 0
        if (f > max_face) max_face = f
        if (fields[2] == "small") small[f] = $4
        else if (fields[2] == "normal") normal[f] = $4
        else crop[f] = $4
      }
      END {
        if (current >= 0) flush()
        exit failed
      }
    ' > "$LOCALE_FACES"

# A map with an invented URL is worse than no map: it certifies a localized
# image that cannot be downloaded later. Require every MTGJSON-linked printing
# to have a matching Scryfall record before publishing any locale sidecar.
EXPECTED_ENTRIES="$WORK_DIR/expected-entries.tsv"
RESOLVED_ENTRIES="$WORK_DIR/resolved-entries.tsv"
awk -F'\t' '{ print $1 "\t" $2 "\t" $3 }' "$UNIQUE_PAIRS" | LC_ALL=C sort -u > "$EXPECTED_ENTRIES"
awk -F'\t' '{ print $1 "\t" $2 "\t" $3 }' "$LOCALE_FACES" | LC_ALL=C sort -u > "$RESOLVED_ENTRIES"
if ! cmp -s "$EXPECTED_ENTRIES" "$RESOLVED_ENTRIES"; then
  echo "ERROR: some MTGJSON localized Scryfall IDs are absent from all_cards." >&2
  echo "  First missing mapping(s):" >&2
  comm -23 "$EXPECTED_ENTRIES" "$RESOLVED_ENTRIES" | head -5 >&2
  echo "  Refresh both caches together; do not construct CDN URLs as a fallback." >&2
  exit 1
fi

mkdir -p "$OUTPUT_DIR"
for code in $CODES; do
  if ! awk -F'\t' -v code="$code" '$1 == code { found = 1; exit } END { exit !found }' "$LOCALE_FACES"; then
    echo "ERROR: no localized printings found for '$code' — is LOCALE_MAP's language name still correct for this MTGJSON version?" >&2
    exit 1
  fi
  out="$OUTPUT_DIR/scryfall-images.$SCHEMA_VERSION.$code.json"
  jq -R -s -c --arg code "$code" '
    split("\n")
    | map(select(length > 0) | split("\t") | select(.[0] == $code))
    | group_by(.[1])
    | map({
        key: .[0][1],
        value: {
          id: .[0][2],
          faces: (sort_by(.[3] | tonumber) | map({small: .[4], normal: .[5], art_crop: .[6]}))
        }
      })
    | from_entries
  ' "$LOCALE_FACES" > "$out"
  if ! validate_locale_map "$out"; then
    echo "ERROR: generated $SCHEMA_VERSION locale map '$out' does not match the required face-URL schema." >&2
    exit 1
  fi
  printf "  %-8s %7d entries  %s\n" "$code" "$(jq 'length' "$out")" "$(du -h "$out" | cut -f1)"
done

# Validate the exact URLs stored in the output, not a reconstructed CDN path.
# A sample keeps generation bounded while still catching a stale all_cards
# cache, CDN regression, or an accidental return to URL synthesis.
echo "Validating sampled localized image URLs..."
SAMPLE_CODE=$(echo "$CODES" | head -1)
SAMPLE_URLS=$(jq -r '
  [.[] | .faces[]? | (.small, .normal, .art_crop) | select(type == "string")]
  | unique
  | .[0:5][]
' "$OUTPUT_DIR/scryfall-images.$SCHEMA_VERSION.$SAMPLE_CODE.json")
if [ -z "$SAMPLE_URLS" ]; then
  echo "ERROR: no localized image URLs found for '$SAMPLE_CODE'." >&2
  exit 1
fi
for url in $SAMPLE_URLS; do
  status=$(curl -s -o /dev/null -w '%{http_code}' --connect-timeout 30 --retry 3 \
    -H 'User-Agent: phase-rs-card-data/1.0 (+https://github.com/phase-rs/phase)' "$url")
  if [ "$status" != "200" ]; then
    echo "ERROR: Scryfall localized image URL returned HTTP $status — $url" >&2
    echo "  Refresh all_cards; do not reconstruct a CDN path as a fallback." >&2
    exit 1
  fi
done
echo "  ${SAMPLE_CODE}: $(echo "$SAMPLE_URLS" | wc -l | tr -d ' ') sampled URLs OK"

echo "Generated $SCHEMA_VERSION locale image maps in $OUTPUT_DIR"
