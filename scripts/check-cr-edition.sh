#!/usr/bin/env bash
# Reports whether the local docs/MagicCompRules.txt is the Comprehensive Rules
# edition pinned in docs/MagicCompRules.edition.
#
# A CR citation is only meaningful against the edition it was written for:
# when Wizards renumbers a section, every old number still exists and now names
# a different rule, so grepping a citation against the wrong edition "verifies"
# it against the wrong rule. Run this before verifying or writing citations.
#
# Usage: scripts/check-cr-edition.sh [path-to-rules-text]
#   default path: docs/MagicCompRules.txt
#
# Exit status: 0 the local copy is the pinned edition
#              1 it is missing or a different edition (the message says how to fix it)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PIN="$REPO_ROOT/docs/MagicCompRules.edition"
RULES="${1:-$REPO_ROOT/docs/MagicCompRules.txt}"

# shellcheck source=lib/comp-rules-edition.sh
source "$SCRIPT_DIR/lib/comp-rules-edition.sh"

PIN_EFFECTIVE="$(cr_pin_field "$PIN" effective)"

if [ ! -f "$RULES" ]; then
  echo "MISSING: $RULES does not exist."
  echo "  Run ./scripts/fetch-comp-rules.sh to download the pinned edition ($PIN_EFFECTIVE)."
  exit 1
fi

LOCAL_EFFECTIVE="$(cr_file_effective "$RULES")"
if [ "$LOCAL_EFFECTIVE" = "$PIN_EFFECTIVE" ]; then
  echo "OK: $RULES is the pinned edition ($PIN_EFFECTIVE)."
  exit 0
fi

echo "MISMATCH: $RULES is effective ${LOCAL_EFFECTIVE:-<no effective date found>},"
echo "  but this repository's CR citations are written against $PIN_EFFECTIVE."
echo "  Rule numbers can name different rules across editions, so do not verify"
echo "  or write citations against this copy. Run:"
echo "    ./scripts/fetch-comp-rules.sh --force"
exit 1
