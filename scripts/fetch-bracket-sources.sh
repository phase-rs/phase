#!/usr/bin/env bash
# Downloads the official Wizards of the Coast Commander bracket sources into
# docs/bracket-sources/ as HTML and greppable plain text.
#
# The source content is © Wizards of the Coast LLC. These files are git-ignored —
# they are downloaded locally for development reference and are not redistributed
# by this repository.
#
# Usage:
#   ./scripts/fetch-bracket-sources.sh
#   ./scripts/fetch-bracket-sources.sh --verify-only

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DEST_DIR="$REPO_ROOT/docs/bracket-sources"
MANIFEST="$REPO_ROOT/data/bracket_sources.json"
VERIFY_ONLY=false

if [ "${1:-}" = "--verify-only" ]; then
  VERIFY_ONLY=true
  shift
fi

if [ "$#" -ne 0 ]; then
  echo "Usage: $0 [--verify-only]" >&2
  exit 2
fi

mkdir -p "$DEST_DIR"

SOURCE_ROWS="$(python3 - "$MANIFEST" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as manifest_file:
    manifest = json.load(manifest_file)

for source in manifest["sources"]:
    print("\x1f".join((source["slug"], source["url"], source["sha256"], source["anchor"])))
PY
)"

declare -a SUMMARIES=()
FAILED=0

while IFS=$'\x1f' read -r SLUG URL EXPECTED_SHA256 ANCHOR; do
  [ -n "$SLUG" ] || continue

  HTML_PATH="$DEST_DIR/$SLUG.html"
  TEXT_PATH="$DEST_DIR/$SLUG.txt"

  if [ "$VERIFY_ONLY" = false ]; then
    echo "Downloading $URL"
    HTML_TMP="$HTML_PATH.tmp.$$"
    TEXT_TMP="$TEXT_PATH.tmp.$$"

    if ! curl -fsSL "$URL" -o "$HTML_TMP"; then
      rm -f "$HTML_TMP" "$TEXT_TMP"
      SUMMARIES+=("$SLUG: ERROR download failed")
      FAILED=1
      continue
    fi

    if ! sed -E \
      -e 's/<[^>]+>/ /g' \
      -e 's/&nbsp;/ /g' \
      -e 's/&amp;/\&/g' \
      -e 's/&quot;/"/g' \
      -e "s/&#39;/'/g" \
      "$HTML_TMP" \
      | awk '{$1=$1; if (length) { if (seen) printf " "; printf "%s", $0; seen=1 }} END { if (seen) print "" }' \
      > "$TEXT_TMP"; then
      rm -f "$HTML_TMP" "$TEXT_TMP"
      SUMMARIES+=("$SLUG: ERROR text conversion failed")
      FAILED=1
      continue
    fi

    mv "$HTML_TMP" "$HTML_PATH"
    mv "$TEXT_TMP" "$TEXT_PATH"
  elif [ ! -f "$HTML_PATH" ] || [ ! -f "$TEXT_PATH" ]; then
    SUMMARIES+=("$SLUG: ERROR downloaded file missing; run $0 without --verify-only first")
    FAILED=1
    continue
  fi

  if ! grep -Fq -- "$ANCHOR" "$TEXT_PATH"; then
    echo "ERROR: Anchor missing for $SLUG" >&2
    echo "       Anchor: $ANCHOR" >&2
    SUMMARIES+=("$SLUG: ERROR anchor missing: $ANCHOR")
    FAILED=1
    continue
  fi

  ACTUAL_SHA256="$(sha256sum "$TEXT_PATH" | awk '{print $1}')"
  if [ "$ACTUAL_SHA256" != "$EXPECTED_SHA256" ]; then
    echo "WARNING: SHA-256 DRIFT for $SLUG" >&2
    echo "         Expected: ${EXPECTED_SHA256:-<not set>}" >&2
    echo "         Actual:   $ACTUAL_SHA256" >&2
    SUMMARIES+=("$SLUG: WARNING digest drift (anchor OK); sha256=$ACTUAL_SHA256")
  else
    SUMMARIES+=("$SLUG: OK")
  fi
done <<< "$SOURCE_ROWS"

echo
echo "Commander bracket source summary:"
for SUMMARY in "${SUMMARIES[@]}"; do
  echo "  $SUMMARY"
done

if [ "$FAILED" -ne 0 ]; then
  exit 1
fi
