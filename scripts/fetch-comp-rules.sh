#!/usr/bin/env bash
# Downloads the Magic: The Gathering Comprehensive Rules text from Wizards of
# the Coast into docs/MagicCompRules.txt — the edition pinned in
# docs/MagicCompRules.edition, which is the edition this repository's CR
# citations are written against.
#
# The CR is © Wizards of the Coast LLC. The text file is git-ignored — it is
# downloaded locally for development reference and is not redistributed by
# this repository.
#
# Usage:
#   ./scripts/fetch-comp-rules.sh            fetch the pinned edition (no-op if already present)
#   ./scripts/fetch-comp-rules.sh --force    re-download the pinned edition
#   ./scripts/fetch-comp-rules.sh --latest [--output PATH]
#       report the edition Wizards currently publishes and whether it differs
#       from the pin; with --output, also save it to PATH (not a symlink, in
#       an existing directory). Never touches
#       docs/MagicCompRules.txt — moving to a new edition means migrating the
#       tree's citations and the pin together.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DEST="$REPO_ROOT/docs/MagicCompRules.txt"
PIN="$REPO_ROOT/docs/MagicCompRules.edition"
RULES_PAGE="https://magic.wizards.com/en/rules"

# shellcheck source=lib/comp-rules-edition.sh
source "$SCRIPT_DIR/lib/comp-rules-edition.sh"

MODE=pinned
FORCE=0
OUTPUT=""
# The EXIT trap deletes this; clear any value inherited from the
# environment so it can only ever remove what this run created.
CR_CONVERT_TMP=""
while [ $# -gt 0 ]; do
  case "$1" in
    --force) FORCE=1 ;;
    --latest) MODE=latest ;;
    --output)
      [ $# -ge 2 ] || { echo "ERROR: --output needs a path" >&2; exit 2; }
      OUTPUT="$2"
      shift
      ;;
    -h | --help)
      sed -n '2,18p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "ERROR: unknown argument '$1' (see --help)" >&2
      exit 2
      ;;
  esac
  shift
done

PIN_EFFECTIVE="$(cr_pin_field "$PIN" effective)"
PIN_URL="$(cr_pin_field "$PIN" url)"

if [ -n "$OUTPUT" ]; then
  if [ "$MODE" != latest ]; then
    echo "ERROR: --output applies only to --latest" >&2
    exit 2
  fi
  # The pinned copy must only ever hold the pinned edition. Keep the output
  # path simple enough to compare exactly: not a symlink (cp would follow it,
  # and a dangling link defeats -ef), in a directory that already exists.
  # Then -ef catches an existing DEST under any name, hard links included, and
  # a not-yet-existing one is caught by directory identity (-ef on the two
  # directories, which sees through `..`, directory symlinks, mounts and UNC
  # aliases) plus the file name compared without case, as macOS and Windows
  # filesystems usually do.
  OUT_DIR="$(dirname "$OUTPUT")"
  if [ -L "$OUTPUT" ]; then
    echo "ERROR: --output must not be a symlink: $OUTPUT" >&2
    exit 2
  fi
  if [ ! -d "$OUT_DIR" ]; then
    echo "ERROR: --output directory does not exist: $OUT_DIR" >&2
    exit 2
  fi
  if [ "$OUTPUT" -ef "$DEST" ] || { [ "$OUT_DIR" -ef "$(dirname "$DEST")" ] \
    && [ "$(basename "$OUTPUT" | tr '[:upper:]' '[:lower:]')" \
      = "$(basename "$DEST" | tr '[:upper:]' '[:lower:]')" ]; }; then
    echo "ERROR: --output must not be $DEST, which holds the pinned edition" >&2
    exit 2
  fi
fi

if [ "$MODE" = pinned ]; then
  if [ "$FORCE" -eq 0 ] && [ -f "$DEST" ] \
    && [ "$(cr_file_effective "$DEST")" = "$PIN_EFFECTIVE" ]; then
    echo "$DEST is already the pinned edition ($PIN_EFFECTIVE)."
    exit 0
  fi
fi
# Stage every download (and its UTF-16 conversion file) beside DEST, never in
# a shared temp directory: the checkout belongs to this user, so no one else
# can replace a staged file, and the pinned mv is a same-filesystem rename
# that can never leave a partial file at DEST.
mkdir -p "$(dirname "$DEST")"
TMP="$(mktemp "$DEST.tmp.XXXXXX")"
trap 'rm -f "$TMP" "${CR_CONVERT_TMP:-}"' EXIT
# mktemp creates the file 0600; curl -o, mv and cp keep that mode, which would
# leave the rules text unreadable to any other uid sharing the checkout.
chmod 644 "$TMP"

if [ "$MODE" = latest ]; then
  echo "Looking up current CR URL from $RULES_PAGE..."
  LATEST_URL="$(curl -fsSL "$RULES_PAGE" \
    | grep -oE 'https://media\.wizards\.com/[^"]+MagicCompRules[^"]+\.txt' \
    | head -n 1)"
  if [ -z "${LATEST_URL:-}" ]; then
    echo "ERROR: Could not find Comprehensive Rules .txt link on $RULES_PAGE" >&2
    echo "       The page structure may have changed. Download manually from:" >&2
    echo "       $RULES_PAGE" >&2
    exit 1
  fi
  cr_download "$LATEST_URL" "$TMP"
  LATEST_EFFECTIVE="$(cr_file_effective "$TMP")"
  echo "Latest published: ${LATEST_EFFECTIVE:-<no effective date found>}"
  echo "  ${LATEST_URL// /%20}"
  echo "Pinned:           $PIN_EFFECTIVE"
  echo "  $PIN_URL"
  if [ -n "$OUTPUT" ]; then
    cp "$TMP" "$OUTPUT"
    echo "Saved latest edition to $OUTPUT"
  fi
  if [ "$LATEST_EFFECTIVE" = "$PIN_EFFECTIVE" ]; then
    echo "The pin is current."
  else
    echo "A newer edition is published. Moving to it means remapping the tree's"
    echo "CR citations and updating docs/MagicCompRules.edition in one change."
  fi
  exit 0
fi

echo "Downloading pinned edition ($PIN_EFFECTIVE): $PIN_URL"
cr_download "$PIN_URL" "$TMP"

GOT_EFFECTIVE="$(cr_file_effective "$TMP")"
if [ "$GOT_EFFECTIVE" != "$PIN_EFFECTIVE" ]; then
  echo "ERROR: $PIN_URL declares effective date '${GOT_EFFECTIVE:-<none>}'," >&2
  echo "       but docs/MagicCompRules.edition pins '$PIN_EFFECTIVE'." >&2
  echo "       $DEST was left unchanged." >&2
  exit 1
fi

mv "$TMP" "$DEST"
LINES="$(wc -l < "$DEST" | tr -d ' ')"
echo "Wrote $DEST ($LINES lines)"
