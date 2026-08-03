#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib/scryfall-fetch.sh"

DATA_DIR="data/scryfall"
SETS_FILE="${SCRYFALL_SETS_FILE:-$DATA_DIR/sets.json}"
OUTPUT="${SCRYFALL_SETS_OUTPUT:-client/public/scryfall-sets.json}"
SETS_URL="${SCRYFALL_SETS_URL:-https://api.scryfall.com/sets}"

echo "=== Scryfall Sets Data Generation ==="

# scryfall_sets_validate_data FILE — true iff FILE has a top-level `.data`
# non-empty ARRAY, the shape the jq transform below requires (CodeRabbit PR
# #6775 review: a structurally-valid-but-empty/malformed JSON body must not
# be treated as a usable sets cache). `type == "array"` matters: jq -e on a
# bare `.data` is truthy for any non-null value, so {"data":"oops"} would
# pass and then crash the map() transform below; length > 0 keeps an empty
# body from being cached as a permanently-empty scryfall-sets.json.
scryfall_sets_validate_data() {
  jq -e '.data | type == "array" and length > 0' "$1" >/dev/null 2>&1
}

# Nothing else reads SETS_FILE, so an already-generated OUTPUT means there
# is no work left at all -- exit before the cache check to avoid a needless
# refetch of a discarded cache in an already-complete build.
if [ -f "$OUTPUT" ]; then
  echo "Skipping generation — $OUTPUT already exists (delete to regenerate)."
  exit 0
fi

# A cached SETS_FILE from a previous run may be stale/malformed (partial
# write, incompatible schema); validate it unconditionally before deciding
# whether a download is needed. This is a local file check only -- it never
# touches the network by itself, only the refetch below does.
if [ -f "$SETS_FILE" ] && ! scryfall_sets_validate_data "$SETS_FILE"; then
  echo "scryfall: cached $SETS_FILE failed validation, discarding..." >&2
  rm -f "$SETS_FILE"
fi

# Download sets data if not present
if [ ! -f "$SETS_FILE" ]; then
  echo "Downloading Scryfall sets data..."
  mkdir -p "$(dirname "$SETS_FILE")"
  scryfall_download "$SETS_URL" "$SETS_FILE" scryfall_sets_validate_data
  echo "Downloaded $SETS_FILE."
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
