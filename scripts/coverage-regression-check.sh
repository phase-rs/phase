#!/usr/bin/env bash
#
# Compare a current coverage-data.json against a baseline and partition
# per-card support changes into three buckets:
#
#   REGRESSED (engine) — card flipped supported:true -> false AND gained
#                        at least one non-ParseWarning gap handler
#                        (Effect:*, Trigger:*, Static:*, Keyword:*, ...).
#                        These are the only flips that fail CI by default:
#                        the engine no longer handles something it used to.
#
#   REGRESSED (coverage honesty)
#                      — card flipped true -> false, but no previously parsed
#                        supported handler was lost. This covers ParseWarning:*
#                        and explicit Effect:* gaps for Oracle text the baseline
#                        parse tree had already swallowed into a broader
#                        supported node. Listed but non-fatal.
#
#   GAINED             — card flipped false -> true. Informational.
#
#   ORACLE CHANGED     — card flipped true -> false AND its oracle_text
#                        changed vs baseline. Treated as informational: the
#                        card wording itself was errata'd/reprinted in an
#                        MTGJSON refresh, so new gaps don't indicate an
#                        engine regression. Surfaced so reviewers can spot
#                        unexpected wording changes.
#
# Usage:
#   scripts/coverage-regression-check.sh <baseline> <current> [--fail-on-engine]
#
#   <baseline>  path OR https URL to main-branch coverage-data.json
#   <current>   path to the newly produced coverage-data.json
#   --fail-on-engine  exit 1 if REGRESSED (engine) bucket is non-empty
#
# The coverage-data.json layout comes from `coverage-report` (see
# crates/engine/src/bin/coverage_report.rs): .cards[] with .card_name,
# .supported, and .gap_details[].handler.

set -euo pipefail

if [[ $# -lt 2 ]]; then
    sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//' >&2
    exit 2
fi

BASELINE="$1"
CURRENT="$2"
FAIL_ON_ENGINE=0
if [[ "${3:-}" == "--fail-on-engine" ]]; then
    FAIL_ON_ENGINE=1
fi

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

if [[ "$BASELINE" == http*://* ]]; then
    echo "Fetching baseline: $BASELINE" >&2
    if ! curl -sSL --fail --retry 3 --max-time 120 "$BASELINE" -o "$tmpdir/baseline.json"; then
        echo "WARNING: baseline unavailable — skipping regression check." >&2
        echo "         (Expected on the first main build; otherwise check R2 upload.)" >&2
        exit 0
    fi
    BASELINE="$tmpdir/baseline.json"
fi

if [[ ! -s "$BASELINE" ]]; then
    echo "WARNING: baseline file missing or empty: $BASELINE — skipping." >&2
    exit 0
fi
if [[ ! -s "$CURRENT" ]]; then
    echo "Current file missing or empty: $CURRENT" >&2
    exit 2
fi

# Emit one JSON object per card that flipped, with categorized new gaps.
# Cards absent from the baseline are skipped (new cards don't count as regressions).
jq -n --slurpfile base "$BASELINE" --slurpfile curr "$CURRENT" '
  def flatten_items(items):
    (items // [])
    | map(
        . as $item
        | [$item] + flatten_items($item.children)
      )
    | add // [];

  def item_handler:
    if .category == "keyword" then "Keyword:\(.label)"
    elif .category == "ability" then "Effect:\(.label)"
    elif .category == "trigger" then "Trigger:\(.label)"
    elif .category == "static" then "Static:\(.label)"
    elif .category == "cost" then "Cost:\(.label)"
    elif .category == "replacement" then empty
    else empty end;

  ($base[0].cards // []) as $bcards |
  ($curr[0].cards // []) as $ccards |
  ($bcards | map({key: (.card_name | ascii_downcase), value: .}) | from_entries) as $bmap |
  $ccards
  | map(
      . as $c
      | ($bmap[$c.card_name | ascii_downcase]) as $b
      | select($b != null and $b.supported != $c.supported)
      | (flatten_items($b.parse_details)
          | map(select(.supported == true) | item_handler | ascii_downcase)
          | unique) as $baseline_supported_handlers
      | {
          name: $c.card_name,
          was: $b.supported,
          now: $c.supported,
          oracle_changed: (($b.oracle_text // "") != ($c.oracle_text // "")),
          new_handlers: (
            ([$c.gap_details[]?.handler] | unique)
            - ([$b.gap_details[]?.handler] | unique)
          ),
        }
      | .new_parser = [.new_handlers[] | select(startswith("ParseWarning:"))]
      | .new_engine = [.new_handlers[] | select(startswith("ParseWarning:") | not)]
      | .new_engine_lost = [
          .new_engine[]
          | select(. as $handler | $baseline_supported_handlers | index($handler | ascii_downcase))
        ]
      | .new_honesty = (.new_parser + (.new_engine - .new_engine_lost))
      | .bucket = (
          if .was and (.now | not) then
            if .oracle_changed then "oracle_changed"
            elif (.new_engine_lost | length) > 0 then "engine_regress"
            else "parser_regress" end
          elif (.was | not) and .now then "gained"
          else "other" end
        )
    )
' > "$tmpdir/flips.json"

engine_count=$(jq '[.[] | select(.bucket=="engine_regress")] | length' "$tmpdir/flips.json")
parser_count=$(jq '[.[] | select(.bucket=="parser_regress")] | length' "$tmpdir/flips.json")
oracle_count=$(jq '[.[] | select(.bucket=="oracle_changed")] | length' "$tmpdir/flips.json")
gained_count=$(jq '[.[] | select(.bucket=="gained")] | length' "$tmpdir/flips.json")

cur_total=$(jq '.total_cards' "$CURRENT")
cur_supported=$(jq '.supported_cards' "$CURRENT")
base_supported=$(jq '.supported_cards' "$BASELINE")
net=$((cur_supported - base_supported))

echo "== Card support delta vs baseline =="
printf "  Baseline supported: %d\n" "$base_supported"
printf "  Current  supported: %d (net %+d)\n" "$cur_supported" "$net"
printf "  Total cards:        %d\n" "$cur_total"
echo

# Cap line counts inside `jq` (via array slice) rather than piping through
# `head`. With `set -o pipefail` a truncating `head` causes SIGPIPE on the
# upstream `jq`, which `pipefail` then surfaces as a script failure even
# when every bucket was within expected bounds.
printf "REGRESSED (engine) — %d cards — engine handler lost for a previously-supported card:\n" "$engine_count"
jq -r '[.[] | select(.bucket=="engine_regress")][:30][] | "  \(.name)  [\(.new_engine_lost | join(", "))]"' \
    "$tmpdir/flips.json"
if [[ "$engine_count" -gt 30 ]]; then
    echo "  ... $((engine_count - 30)) more"
fi
echo

printf "REGRESSED (coverage honesty) — %d cards — newly surfaced parser gaps without a lost baseline handler:\n" "$parser_count"
jq -r '[.[] | select(.bucket=="parser_regress")][:10][] | "  \(.name)  [\(.new_honesty | join(", "))]"' \
    "$tmpdir/flips.json"
if [[ "$parser_count" -gt 10 ]]; then
    echo "  ... $((parser_count - 10)) more"
fi
echo

printf "ORACLE CHANGED — %d cards flipped true->false with edited oracle_text (MTGJSON rewording, not an engine regression):\n" "$oracle_count"
jq -r '[.[] | select(.bucket=="oracle_changed")][:10][] | "  \(.name)  [\(.new_handlers | join(", "))]"' \
    "$tmpdir/flips.json"
if [[ "$oracle_count" -gt 10 ]]; then
    echo "  ... $((oracle_count - 10)) more"
fi
echo

printf "GAINED — %d cards newly supported:\n" "$gained_count"
jq -r '[.[] | select(.bucket=="gained")][:10][] | "  \(.name)"' "$tmpdir/flips.json"
if [[ "$gained_count" -gt 10 ]]; then
    echo "  ... $((gained_count - 10)) more"
fi
echo

if [[ "$FAIL_ON_ENGINE" -eq 1 && "$engine_count" -gt 0 ]]; then
    echo "FAIL: $engine_count cards regressed with new engine-level gaps." >&2
    echo "      Either restore the handler or update the baseline if intentional." >&2
    exit 1
fi

exit 0
