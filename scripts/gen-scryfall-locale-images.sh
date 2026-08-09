#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib/mtgjson-fetch.sh"

DATA_DIR="data/mtgjson"
SETS_TAR="${MTGJSON_ALL_SET_FILES:-$DATA_DIR/AllSetFiles.tar}"
SETS_DIR="${MTGJSON_ALL_SETS_DIR:-$DATA_DIR/allsets}"
OUTPUT_DIR="${SCRYFALL_LOCALE_IMAGES_OUTPUT_DIR:-client/public}"

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

# Skip only when every locale output already exists — a partial set must
# regenerate, or a locale added to LOCALE_MAP would never be built.
ALL_PRESENT=1
for code in $CODES; do
  [ -f "$OUTPUT_DIR/scryfall-images.$code.json" ] || ALL_PRESENT=0
done
if [ "$ALL_PRESENT" = 1 ]; then
  echo "Skipping generation — all locale maps already exist in $OUTPUT_DIR (delete to regenerate)."
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

# Split by locale code, then build each map. Keeping the split in awk means the
# large intermediate is never held in a jq value.
awk -F'\t' -v dir="$WORK_DIR" '{ print $2 "\t" $3 > (dir "/pairs-" $1 ".tsv") }' "$PAIRS"

mkdir -p "$OUTPUT_DIR"
for code in $CODES; do
  locale_pairs="$WORK_DIR/pairs-$code.tsv"
  if [ ! -f "$locale_pairs" ]; then
    echo "ERROR: no localized printings found for '$code' — is LOCALE_MAP's language name still correct for this MTGJSON version?" >&2
    exit 1
  fi
  out="$OUTPUT_DIR/scryfall-images.$code.json"
  jq -R -s -c '
    split("\n")
    | map(select(length > 0) | split("\t") | {key: .[0], value: .[1]})
    | from_entries
  ' "$locale_pairs" > "$out"
  printf "  %-8s %7d entries  %s\n" "$code" "$(jq 'length' "$out")" "$(du -h "$out" | cut -f1)"
done

# The runtime builds image URLs by substituting the localized id into the same
# CDN path shape every stored Scryfall URL already uses. That shape is not a
# documented API contract, so verify a sample here: if Scryfall reorganizes its
# CDN, this fails at generation instead of blanking every localized card image
# in production.
echo "Validating constructed image URLs..."
SAMPLE_CODE=$(echo "$CODES" | head -1)
SAMPLE_IDS=$(jq -r '[.[]] | .[0:5][]' "$OUTPUT_DIR/scryfall-images.$SAMPLE_CODE.json")
for id in $SAMPLE_IDS; do
  url="https://cards.scryfall.io/normal/front/${id:0:1}/${id:1:1}/${id}.jpg"
  status=$(curl -s -o /dev/null -w '%{http_code}' --connect-timeout 30 --retry 3 \
    -H 'User-Agent: phase-rs-card-data/1.0 (+https://github.com/phase-rs/phase)' "$url")
  if [ "$status" != "200" ]; then
    echo "ERROR: constructed image URL returned HTTP $status — $url" >&2
    echo "  The Scryfall CDN path shape may have changed. Localized art would 404 for every card." >&2
    exit 1
  fi
done
echo "  ${SAMPLE_CODE}: $(echo "$SAMPLE_IDS" | wc -l | tr -d ' ') sampled URLs OK"

echo "Generated locale image maps in $OUTPUT_DIR"
