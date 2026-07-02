#!/usr/bin/env bash
#
# Gittensor contributor impact report — one command, no flags to remember.
#
#   scripts/gittensor/report.sh                 # monthly (last 30 days)
#   scripts/gittensor/report.sh weekly          # last 7 days
#   scripts/gittensor/report.sh monthly         # last 30 days
#   scripts/gittensor/report.sh all             # all-time
#   scripts/gittensor/report.sh 2026-06-01      # since a specific date
#
# It maintains a full (non-shallow) analysis clone, refreshes it, runs the
# stats + infographic, writes dated + "latest" artifacts, and opens the PNG.
#
# Overridable via env: GITTENSOR_REPO, ANALYSIS_DIR, REPORT_DIR.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_SLUG="${GITTENSOR_REPO:-phase-rs/phase}"
ANALYSIS_DIR="${ANALYSIS_DIR:-$HOME/phase-analysis}"
REPORT_DIR="${REPORT_DIR:-$ANALYSIS_DIR/gittensor-report}"

# ── window ───────────────────────────────────────────────────────────────────
case "${1:-monthly}" in
  -h|--help) sed -n '2,14p' "${BASH_SOURCE[0]}" | sed 's/^#//;s/^ //'; exit 0 ;;
  weekly)    SINCE=(--since "7 days ago");  LABEL="weekly" ;;
  monthly)   SINCE=(--since "30 days ago"); LABEL="monthly" ;;
  all)       SINCE=();                      LABEL="all-time" ;;
  *)         SINCE=(--since "$1");          LABEL="since-${1// /_}" ;;
esac

# ── analysis clone: create on first run, otherwise refresh ───────────────────
if [[ ! -d "$ANALYSIS_DIR/.git" ]]; then
  echo "→ First run: cloning full history of $REPO_SLUG into $ANALYSIS_DIR ..."
  if command -v gh >/dev/null 2>&1; then
    gh repo clone "$REPO_SLUG" "$ANALYSIS_DIR"
  else
    git clone "https://github.com/$REPO_SLUG" "$ANALYSIS_DIR"
  fi
else
  echo "→ Refreshing $ANALYSIS_DIR ..."
  git -C "$ANALYSIS_DIR" fetch --quiet origin
fi

# ── render ───────────────────────────────────────────────────────────────────
mkdir -p "$REPORT_DIR"
STAMP="$(date +%Y-%m-%d)"
STATS="$REPORT_DIR/gittensor-stats-$LABEL-$STAMP.json"
IMG="$REPORT_DIR/gittensor-impact-$LABEL-$STAMP.png"

python3 "$SCRIPT_DIR/contrib_stats.py" --repo-dir "$ANALYSIS_DIR" "${SINCE[@]}" -o "$STATS"
python3 "$SCRIPT_DIR/contrib_infographic.py" "$STATS" "$IMG"

# Stable "latest" copies for quick access / sharing.
cp "$STATS" "$REPORT_DIR/gittensor-stats-latest.json"
cp "$IMG" "$REPORT_DIR/gittensor-impact-latest.png"

echo "→ Report ($LABEL): $IMG"
[[ "$(uname)" == "Darwin" ]] && command -v open >/dev/null 2>&1 && open "$IMG" || true
