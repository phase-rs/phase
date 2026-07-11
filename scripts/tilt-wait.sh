#!/usr/bin/env bash
# Wait for one or more Tilt resources to reach a terminal state for the CURRENT code.
#
# Usage: tilt-wait.sh [--interval SECONDS] [--timeout SECONDS] <resource>...
#
# Exit codes:
#   0    all resources are fresh and reached updateStatus=ok with no in-flight build
#   1    a resource is fresh and reached updateStatus=error with no in-flight build
#   2    usage error
#   124  --timeout elapsed before all resources settled
#
# A resource must be both TERMINAL and FRESH before its status is believed.
#
#   Terminal: currentBuild.spanID == "none". This avoids reacting to a stale
#   buildHistory error while a newer build is still compiling.
#
#   Fresh: the last build STARTED after the newest change to the files that resource is
#   built from. A build status is only ever a statement about the code that build
#   actually compiled. If the last build started before your edit, its "ok" describes
#   the code as it was BEFORE the change -- a green that measured nothing. A stale
#   resource is simply not terminal yet: we keep waiting for the rebuild (and report the
#   usual 124 if it never arrives), so the exit-code contract above is unchanged. The
#   same applies to a stale "error": it is not reported, because it describes old code.
#
# The freshness reference is derived per resource from Tilt's own watch config, so
# callers pass nothing extra (`tilt get filewatch local:<resource> -o json`):
#
#   .spec.watchedPaths         roots this resource rebuilds from (clippy: crates/,
#                              check-frontend: client/src/, card-data: crates/engine/src/)
#   .spec.ignores              paths/patterns Tilt does not rebuild for
#   .status.fileEvents[].time  changes Tilt has already observed
#
# A resource is stale when Tilt has observed a change since its last build started (a
# rebuild is coming), or when a file under its watchedPaths is newer than that build's
# startTime (the edit landed on disk but Tilt's watcher has not caught up -- the window
# that produced false greens). Scoping the reference per resource is what keeps this from
# over-waiting: a Rust-only edit does not make `check-frontend` stale, because
# check-frontend is not built from crates/.
#
# Known limits, kept loud rather than silent:
#   * A resource with no watchedPaths (the manual `coverage`) has no freshness reference.
#     It warns on stderr and falls back to status-only.
#   * Tilt watches one checkout. Running this from a different worktree than the one Tilt
#     watches is a hard error (exit 1): no build over there can describe your edits here.

set -euo pipefail

interval=20
timeout=""
resources=()

usage() {
  sed -n '2,10p' "$0" >&2
  exit 2
}

while (($#)); do
  case "$1" in
    --interval)
      interval="${2:?--interval requires a value}"
      shift 2
      ;;
    --timeout)
      timeout="${2:?--timeout requires a value}"
      shift 2
      ;;
    -h|--help)
      usage
      ;;
    --)
      shift
      resources+=("$@")
      break
      ;;
    -*)
      echo "tilt-wait: unknown flag: $1" >&2
      usage
      ;;
    *)
      resources+=("$1")
      shift
      ;;
  esac
done

if ((${#resources[@]} == 0)); then
  echo "tilt-wait: at least one resource is required" >&2
  usage
fi

deadline=""
if [[ -n "$timeout" ]]; then
  deadline=$((SECONDS + timeout))
fi

repo_root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel 2>/dev/null || pwd)
start_ref="$(mktemp "${TMPDIR:-/tmp}/tilt-wait.XXXXXX")"
trap 'rm -f "$start_ref"' EXIT
warned=""

# freshness <resource> <last-build-start-time>
# Echoes: fresh | stale | never-built | unverifiable | foreign
# (foreign also explains itself on stderr)
freshness() {
  local r="$1" start="$2" fw p newer
  local paths=() prune=()

  [[ "$start" == "none" ]] && { echo never-built; return; }

  if ! fw=$(tilt get filewatch "local:$r" -o json 2>/dev/null); then
    echo unverifiable
    return
  fi

  while IFS= read -r p; do
    [[ -n "$p" ]] && paths+=("$p")
  done < <(jq -r '.spec.watchedPaths[]?' <<< "$fw")

  if ((${#paths[@]} == 0)); then
    echo unverifiable
    return
  fi

  # Tilt watches a checkout, not this shell's cwd. If it is watching a different tree,
  # nothing it builds can describe the edits made here.
  local in_tree=0
  for p in "${paths[@]}"; do
    case "$p" in
      "$repo_root"|"$repo_root"/*) in_tree=1 ;;
    esac
  done
  if ((in_tree == 0)); then
    echo "tilt-wait: Tilt watches ${paths[0]}, outside this checkout ($repo_root)." >&2
    echo "tilt-wait: no build there can describe changes made here -- a green would be meaningless." >&2
    echo foreign
    return
  fi

  # Has Tilt already seen a change since this build began? Then a rebuild is coming.
  # Timestamps are truncated to whole seconds; the mtime scan below is the precise check.
  if [[ $(jq -r --arg start "$start" '
        def ts: sub("\\.[0-9]+Z$"; "Z") | fromdateiso8601;
        ([.status.fileEvents[]?.time] | max) as $ev
        | if $ev != null and ($ev | ts) > ($start | ts) then "stale" else "fresh" end' <<< "$fw") == stale ]]; then
    echo stale
    return
  fi

  # Has a file changed on disk that Tilt has not reacted to yet? This is the window that
  # produced false greens: the edit is saved, the watcher has not caught up, and the last
  # completed build still reports ok for the code as it was before the edit.
  touch -d "$start" "$start_ref"
  while IFS= read -r p; do
    [[ -z "$p" ]] && continue
    ((${#prune[@]})) && prune+=(-o)
    prune+=(-path "$p")
  done < <(jq -r '.spec.ignores[]? | select((.patterns // []) | length == 0) | .basePath' <<< "$fw")
  while IFS= read -r p; do
    [[ -z "$p" ]] && continue
    ((${#prune[@]})) && prune+=(-o)
    prune+=(-name "${p##*/}")
  done < <(jq -r '.spec.ignores[]? | .patterns[]?' <<< "$fw")

  if ((${#prune[@]})); then
    newer=$(find "${paths[@]}" \( "${prune[@]}" \) -prune -o -type f -newer "$start_ref" -print -quit 2>/dev/null || true)
  else
    newer=$(find "${paths[@]}" -type f -newer "$start_ref" -print -quit 2>/dev/null || true)
  fi

  if [[ -n "$newer" ]]; then
    echo "tilt-wait: $r is stale -- ${newer} changed after its last build started" >&2
    echo stale
    return
  fi

  echo fresh
}

while true; do
  all_done=1
  for r in "${resources[@]}"; do
    if ! json=$(tilt get uiresource "$r" -o json 2>/dev/null); then
      echo "tilt-wait: failed to read resource '$r' (is Tilt running?)" >&2
      exit 1
    fi
    st=$(jq -r '.status.updateStatus // "unknown"' <<< "$json")
    current=$(jq -r '.status.currentBuild.spanID // "none"' <<< "$json")
    started=$(jq -r '.status.buildHistory[0].startTime // "none"' <<< "$json")

    if [[ "$current" != "none" ]]; then
      printf '%s status=%s current=%s started=%s\n' "$r" "$st" "$current" "$started"
      all_done=0
      continue
    fi

    fresh=$(freshness "$r" "$started")
    printf '%s status=%s current=%s started=%s freshness=%s\n' "$r" "$st" "$current" "$started" "$fresh"

    case "$fresh" in
      foreign)
        exit 1
        ;;
      stale|never-built)
        # Any ok/error here describes code from before the change (or no code at all).
        # Wait for the rebuild.
        all_done=0
        continue
        ;;
      unverifiable)
        if [[ "$warned" != *"|$r|"* ]]; then
          echo "tilt-wait: $r has no watched paths; cannot verify build freshness (status only)" >&2
          warned="$warned|$r|"
        fi
        ;;
    esac

    case "$st" in
      ok)
        ;;
      error)
        err=$(jq -r '.status.buildHistory[0].error // ""' <<< "$json")
        printf '%s error=%s\n' "$r" "$err" >&2
        exit 1
        ;;
      *)
        all_done=0
        ;;
    esac
  done

  if ((all_done == 1)); then
    exit 0
  fi

  if [[ -n "$deadline" && $SECONDS -ge $deadline ]]; then
    echo "tilt-wait: timed out after ${timeout}s" >&2
    exit 124
  fi

  sleep "$interval"
done
