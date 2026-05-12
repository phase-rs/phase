#!/usr/bin/env bash
# Wait for one or more Tilt resources to reach a terminal state.
#
# Usage: tilt-wait.sh [--interval SECONDS] [--timeout SECONDS] <resource>...
#
# Exit codes:
#   0    all resources reached updateStatus=ok with no in-flight build
#   1    a resource reached updateStatus=error with no in-flight build
#   2    usage error
#   124  --timeout elapsed before all resources settled
#
# A resource is only considered terminal when currentBuild.spanID == "none".
# This avoids reacting to stale buildHistory errors while a newer build is
# still compiling.

set -euo pipefail

interval=20
timeout=""
resources=()

usage() {
  sed -n '2,15p' "$0" >&2
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

while true; do
  all_done=1
  for r in "${resources[@]}"; do
    if ! json=$(tilt get uiresource "$r" -o json 2>/dev/null); then
      echo "tilt-wait: failed to read resource '$r' (is Tilt running?)" >&2
      exit 1
    fi
    st=$(jq -r '.status.updateStatus // "unknown"' <<< "$json")
    current=$(jq -r '.status.currentBuild.spanID // "none"' <<< "$json")
    last=$(jq -r '.status.buildHistory[0].finishTime // "none"' <<< "$json")
    printf '%s status=%s current=%s last=%s\n' "$r" "$st" "$current" "$last"

    if [[ "$current" != "none" ]]; then
      all_done=0
      continue
    fi

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
