#!/usr/bin/env bash
# Gate legacy dynamic quantity parser calls while call sites migrate to
# oracle_nom::quantity::parse_quantity_ref_complete.

set -euo pipefail

BASE="${1:-$(git merge-base origin/main HEAD 2>/dev/null || echo HEAD~1)}"
SCOPE='crates/engine/src/parser'

python3 - <<'PY'
import re
import sys
from pathlib import Path

legacy_call = re.compile(
    r'(?<![A-Za-z0-9_])(?:crate::parser::oracle_quantity::|super::super::oracle_quantity::|oracle_quantity::)?parse_quantity_ref\s*\('
)

seams = {
    "crates/engine/src/parser/oracle_effect/imperative.rs": [
        "try_parse_roll_die_with_modifier",
    ],
    "crates/engine/src/parser/oracle_effect/lower.rs": [
        "parse_dynamic_counter_suffix_body",
    ],
    "crates/engine/src/parser/oracle_effect/mod.rs": [
        "try_parse_gain_energy",
        "parse_dynamic_energy_unless_cost",
        "parse_where_x_is",
    ],
    "crates/engine/src/parser/oracle_effect/search.rs": [
        "parse_highest_mana_value_library_suffix",
    ],
    "crates/engine/src/parser/oracle_replacement.rs": [
        "parse_enters_with_where_x_suffix",
    ],
}


def function_body(source: str, name: str) -> str | None:
    match = re.search(rf'\bfn\s+{re.escape(name)}\s*\(', source)
    if not match:
        return None
    open_brace = source.find("{", match.end())
    if open_brace == -1:
        return None
    depth = 0
    for index in range(open_brace, len(source)):
        char = source[index]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return source[match.start() : index + 1]
    return None


failures: list[str] = []
for file_name, names in seams.items():
    path = Path(file_name)
    text = path.read_text()
    for name in names:
        body = function_body(text, name)
        if body is None:
            failures.append(f"{file_name}: missing seam function {name}")
            continue
        if legacy_call.search(body):
            failures.append(f"{file_name}: {name} still calls legacy parse_quantity_ref")

if failures:
    print("Legacy quantity parser calls remain in migrated seams:", file=sys.stderr)
    for failure in failures:
        print(f"  {failure}", file=sys.stderr)
    sys.exit(1)
PY

LEGACY_PATTERN='(^|[^[:alnum:]_])(parse_quantity_ref|parse_cda_quantity|parse_event_context_quantity|parse_for_each_clause|parse_for_each_clause_expr)[[:space:]]*\('
files=$(git diff --name-only "$BASE" -- "$SCOPE" \
    ':(exclude)crates/engine/src/parser/oracle_quantity.rs' \
    ':(exclude)crates/engine/src/parser/oracle_nom/quantity.rs' \
    2>/dev/null || true)

diff_fail=0
diff_report=""
while IFS= read -r file; do
    [ -n "$file" ] || continue
    [ -f "$file" ] || continue
    added=$(git diff --unified=0 "$BASE" -- "$file" | grep -E '^\+[^+]' || true)
    hits=$(printf '%s\n' "$added" | grep -E "$LEGACY_PATTERN" || true)
    if [ -n "$hits" ]; then
        diff_fail=1
        diff_report="${diff_report}
  ${file}:"
        while IFS= read -r hit; do
            diff_report="${diff_report}
    ${hit}"
        done <<< "$hits"
    fi
done <<< "$files"

if [ "$diff_fail" -ne 0 ]; then
    printf 'New parser diff lines call legacy quantity parsers:%s\n' "$diff_report" >&2
    exit 1
fi
