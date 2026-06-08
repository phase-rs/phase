#!/usr/bin/env bash
#
# coverage-history.sh — reconstruct card-support coverage over time from CI logs.
#
# The absolute supported-card count and its per-build delta are printed by
# scripts/coverage-regression-check.sh during CI as:
#
#     Current  supported: 29236 (net -1175)
#
# That line is the only durable record of the number — coverage-data.json is a
# gitignored build artifact, CI uploads no coverage artifact, and the R2 copy is
# overwritten on every push (no per-commit history). GitHub's GraphQL API does
# not expose Actions log text either, so the data is REST-log-only.
#
# This script walks push-to-main CI runs since a date, fetches *only* the
# "Card data" job's log (≈57 KB) — not the whole-run zip (≈1.2 MB) — greps for
# the line, and appends to a JSON data file. It is INCREMENTAL: runs already in
# the data file are skipped, so the one-time backfill is amortized and each
# subsequent invocation only touches new commits. Finally it renders an image
# via plot-coverage-history.py.
#
# Usage:
#   scripts/coverage-history.sh --since 2026-05-01 [options]
#   scripts/coverage-history.sh 2026-05-01                       # date as positional
#
# Options:
#   --since DATE      Only scan runs created on/after DATE (YYYY-MM-DD). Required
#                     on first run; optional once a data file exists (the file's
#                     newest record is used as the floor).
#   --workflows LIST  Comma-separated workflow names to scan. Default: "CI".
#                     (Deploy Staging regenerates coverage but never prints the
#                     line, so it is intentionally excluded.)
#   --job-prefix STR  Job-name prefix carrying the coverage line. Default: "Card data".
#   --out FILE        Data file path (read for incremental, then rewritten).
#                     Default: data/coverage-history.json
#   --image FILE      Image path. Default: data/coverage-history.png
#   --sleep SECONDS   Delay between runs (rate-limiting). Default: 0.4
#   --limit N         Max runs to list per workflow. Default: 500
#   --full            Ignore the existing data file; re-scan from --since.
#   --no-plot         Generate the data file only; skip the image.
#
# Requires: gh (authenticated), jq. PNG output needs python3 + one of
# rsvg-convert / magick / inkscape (SVG output is dependency-free).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

SINCE=""
WORKFLOWS="CI"
JOB_PREFIX="Card data"
OUT="$REPO_ROOT/data/coverage-history.json"
IMAGE="$REPO_ROOT/data/coverage-history.png"
SLEEP="0.4"
LIMIT="500"
FULL=0
PLOT=1

while [[ $# -gt 0 ]]; do
    case "$1" in
        --since)      SINCE="$2"; shift 2 ;;
        --workflows)  WORKFLOWS="$2"; shift 2 ;;
        --job-prefix) JOB_PREFIX="$2"; shift 2 ;;
        --out)        OUT="$2"; shift 2 ;;
        --image)      IMAGE="$2"; shift 2 ;;
        --sleep)      SLEEP="$2"; shift 2 ;;
        --limit)      LIMIT="$2"; shift 2 ;;
        --full)       FULL=1; shift ;;
        --no-plot)    PLOT=0; shift ;;
        -h|--help)    sed -n '2,/^set -euo/p' "$0" | sed 's/^# \{0,1\}//; /^set -euo/d'; exit 0 ;;
        *)
            if [[ -z "$SINCE" && "$1" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
                SINCE="$1"; shift
            else
                echo "Unknown argument: $1" >&2; exit 2
            fi
            ;;
    esac
done

command -v gh >/dev/null || { echo "Error: gh CLI not found." >&2; exit 1; }
command -v jq >/dev/null || { echo "Error: jq not found." >&2; exit 1; }

REPO="$(gh repo view --json nameWithOwner -q .nameWithOwner)"
mkdir -p "$(dirname "$OUT")"

# ── Incremental state ────────────────────────────────────────────────────────
# Load already-recorded run_ids (to skip) and derive a date floor from the
# newest existing record when --since is omitted.
declare -A SEEN=()
EXISTING="[]"
if [[ "$FULL" -eq 0 && -s "$OUT" ]]; then
    EXISTING="$(cat "$OUT")"
    while IFS= read -r rid; do SEEN["$rid"]=1; done < <(jq -r '.[].run_id' "$OUT")
    if [[ -z "$SINCE" ]]; then
        SINCE="$(jq -r 'map(.created) | max // "" | .[0:10]' "$OUT")"
    fi
    echo "Incremental: ${#SEEN[@]} run(s) already recorded; floor = ${SINCE:-<none>}." >&2
fi

if [[ -z "$SINCE" ]]; then
    echo "Error: --since DATE (YYYY-MM-DD) is required on first run." >&2
    exit 2
fi
if ! [[ "$SINCE" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
    echo "Error: --since must be YYYY-MM-DD, got '$SINCE'." >&2
    exit 2
fi

tmp_ndjson="$(mktemp)"
trap 'rm -f "$tmp_ndjson"' EXIT

# ── Collect candidate push-to-main runs ──────────────────────────────────────
# --event push + --branch main yields exactly the commits that produce a
# baseline-relative number (PR / merge_group runs are excluded). Cancelled and
# skipped runs never reach the coverage step, so we drop them up front.
declare -a RUN_ROWS=()
IFS=',' read -ra WF_LIST <<< "$WORKFLOWS"
for wf in "${WF_LIST[@]}"; do
    wf="${wf#"${wf%%[![:space:]]*}"}"; wf="${wf%"${wf##*[![:space:]]}"}"  # trim
    echo "Listing push-to-main runs for: $wf (since $SINCE)" >&2
    rows="$(gh run list \
        --workflow "$wf" --branch main --event push --limit "$LIMIT" \
        --json databaseId,headSha,createdAt,conclusion,displayTitle,url \
        2>/dev/null \
        | jq -r --arg since "$SINCE" --arg wf "$wf" '
            .[]
            | select(.createdAt[0:10] >= $since)
            | select((.conclusion // "") as $c | $c != "cancelled" and $c != "skipped")
            | [.databaseId, .headSha, .createdAt, (.conclusion // ""), $wf, .url, .displayTitle]
            | @tsv' || true)"
    [[ -n "$rows" ]] && while IFS= read -r r; do RUN_ROWS+=("$r"); done <<< "$rows"
done

# Filter out runs already in the data file.
declare -a TODO=()
for r in "${RUN_ROWS[@]}"; do
    id="${r%%$'\t'*}"
    [[ -n "${SEEN[$id]:-}" ]] || TODO+=("$r")
done

total="${#TODO[@]}"
echo "New run(s) to fetch: $total (skipped ${#SEEN[@]} already recorded)." >&2

# ── Extract the coverage line from each NEW run's card-data job only ──────────
COVERAGE_RE='Current  supported: [0-9]+ \(net [+-][0-9]+\)'
matched=0
i=0
for r in "${TODO[@]}"; do
    IFS=$'\t' read -r id sha created conclusion wf url title <<< "$r"
    i=$((i + 1))
    printf '[%d/%d] %s %s %s ... ' "$i" "$total" "$wf" "${sha:0:9}" "${created:0:10}" >&2

    job_id="$(gh api "repos/$REPO/actions/runs/$id/jobs" --paginate \
        --jq "[.jobs[] | select(.name|startswith(\"$JOB_PREFIX\")) | .id] | first // empty" \
        2>/dev/null || true)"
    if [[ -z "$job_id" ]]; then
        echo "no '$JOB_PREFIX' job (skipped)" >&2
        sleep "$SLEEP"; continue
    fi

    line="$(gh api "repos/$REPO/actions/jobs/$job_id/logs" 2>/dev/null \
        | grep -oE "$COVERAGE_RE" | head -1 || true)"
    if [[ -n "$line" ]]; then
        supported="$(echo "$line" | sed -E 's/.*supported: ([0-9]+).*/\1/')"
        delta="$(echo "$line" | sed -E 's/.*net ([+-][0-9]+).*/\1/')"
        jq -n \
            --argjson id "$id" --arg sha "$sha" --arg created "$created" \
            --arg conclusion "$conclusion" --arg wf "$wf" --arg url "$url" \
            --arg title "$title" --argjson supported "$supported" --argjson delta "$delta" \
            '{run_id:$id, sha:$sha, created:$created, conclusion:$conclusion,
              workflow:$wf, url:$url, title:$title, supported:$supported, delta:$delta}' \
            >> "$tmp_ndjson"
        matched=$((matched + 1))
        echo "supported=$supported (net $delta)" >&2
    else
        echo "no coverage line (skipped)" >&2
    fi
    sleep "$SLEEP"
done

# ── Merge with existing, dedup, sort, write ──────────────────────────────────
new_arr="$(jq -s '.' "$tmp_ndjson" 2>/dev/null || echo '[]')"
jq -n --argjson old "$EXISTING" --argjson new "$new_arr" \
    '$old + $new | sort_by(.created) | unique_by([.sha, .supported])' > "$OUT"

kept="$(jq 'length' "$OUT")"
echo "Added $matched new point(s); $kept total -> $OUT" >&2

# ── Render the image ─────────────────────────────────────────────────────────
if [[ "$PLOT" -eq 1 ]]; then
    if [[ "$kept" -eq 0 ]]; then
        echo "No data points; skipping image." >&2
    else
        python3 "$REPO_ROOT/scripts/plot-coverage-history.py" "$OUT" "$IMAGE"
    fi
fi
