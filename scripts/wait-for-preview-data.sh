#!/usr/bin/env bash
# Exits 0 once every URL argument answers HEAD 200, and 1 if any is still
# unavailable when another poll would pass DATA_WAIT_SECONDS from this run's
# own start, never on the strength of a single probe. HEAD is answered by
# origin, so a cached 404 cannot hide an upload.
set -euo pipefail
: "${DATA_WAIT_SECONDS:?}" "${DATA_POLL_SECONDS:?}"
(( $# > 0 )) || { echo "::error::no data URLs given"; exit 2; }
deadline=$(( $(date +%s) + DATA_WAIT_SECONDS ))
pending=("$@")
probed=0
while :; do
  waiting=()
  for url in "${pending[@]}"; do
    status=$(curl -sS -I -o /dev/null -w '%{http_code}' --connect-timeout 5 --max-time 15 "$url" || true)
    if [ "$status" = 200 ]; then
      echo "Available: $url"
    else
      echo "Waiting (HTTP $status): $url"
      waiting+=("$url")
    fi
  done
  (( ${#waiting[@]} == 0 )) && exit 0
  # One HEAD can miss a served object on a 15s cap against a cold origin, so no
  # URL is declared unavailable on a single probe.
  if (( probed )) && (( $(date +%s) + DATA_POLL_SECONDS > deadline )); then
    for url in "${waiting[@]}"; do
      echo "::error::Still unavailable after ${DATA_WAIT_SECONDS}s: $url"
    done
    exit 1
  fi
  probed=1
  pending=("${waiting[@]}")
  sleep "$DATA_POLL_SECONDS"
done
