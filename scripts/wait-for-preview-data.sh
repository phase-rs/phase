#!/usr/bin/env bash
# Exits 0 once every URL argument answers HEAD 200, and 1 if any is still
# unavailable when another poll would pass DATA_DEADLINE_EPOCH; a deadline
# already past still gets one round. HEAD is answered by origin, so a cached 404
# cannot hide an upload.
set -euo pipefail
: "${DATA_DEADLINE_EPOCH:?}" "${DATA_POLL_SECONDS:?}"
(( $# > 0 )) || { echo "::error::no data URLs given"; exit 2; }
pending=("$@")
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
  if (( $(date +%s) + DATA_POLL_SECONDS > DATA_DEADLINE_EPOCH )); then
    for url in "${waiting[@]}"; do
      echo "::error::Still unavailable at deadline epoch $DATA_DEADLINE_EPOCH: $url"
    done
    exit 1
  fi
  pending=("${waiting[@]}")
  sleep "$DATA_POLL_SECONDS"
done
