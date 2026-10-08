#!/usr/bin/env bash
set -euo pipefail

default_render=${1:?usage: assert-compression-boundary.sh DEFAULT_RENDER SCALEOUT_RENDER}
scaleout_render=${2:?usage: assert-compression-boundary.sh DEFAULT_RENDER SCALEOUT_RENDER}
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT

extract_doc() {
  awk -v kind="$1" -v name="$2" '
    BEGIN { RS = "---"; ORS = "" }
    $0 ~ "kind: " kind "\\n" && $0 ~ "metadata:\\n  name: " name "\\n" { print }
  ' "$3"
}

extract_doc Ingress phase-server "$default_render" > "$work_dir/default-health.yaml"
extract_doc Ingress phase-server-ws "$default_render" > "$work_dir/default-ws.yaml"
extract_doc Ingress phase-server-backup "$default_render" > "$work_dir/default-backup.yaml"
test -s "$work_dir/default-health.yaml"
test -s "$work_dir/default-ws.yaml"
test -s "$work_dir/default-backup.yaml"
grep -q 'path: /health' "$work_dir/default-health.yaml"
grep -q -- '-compress@kubernetescrd' "$work_dir/default-health.yaml"
grep -q 'path: /p2p-draft-backup' "$work_dir/default-backup.yaml"
grep -q -- '-compress@kubernetescrd' "$work_dir/default-backup.yaml"
grep -q 'path: /ws' "$work_dir/default-ws.yaml"
if grep -q -- '-compress@kubernetescrd' "$work_dir/default-ws.yaml"; then
  echo 'WebSocket Ingress unexpectedly carries compression'
  exit 1
fi

assert_ingressroute_pair() {
  local name=$1
  local output="$work_dir/$2"
  extract_doc IngressRoute "$name" "$scaleout_render" > "$output"
  test -s "$output"
  awk '/^    - kind: Rule/{route++} route == 1' "$output" > "$output-ws"
  awk '/^    - kind: Rule/{route++} route == 2' "$output" > "$output-http"
  grep -q 'PathPrefix(`/ws`)' "$output-ws"
  if grep -q 'name: phase-server-compress' "$output-ws"; then
    echo "$name WebSocket route unexpectedly carries compression"
    exit 1
  fi
  grep -q 'name: phase-server-compress' "$output-http"

  # Neither rule may pin an explicit `priority:`. Traefik priority is
  # entrypoint-wide (shared with every other IngressRoute/Ingress on the
  # same entrypoint, not just this chart's own pair), so an explicit low
  # value on the general route would make it globally losable to any other
  # router that keeps Traefik's default. We rely instead on Traefik's
  # documented default: routers are sorted by descending rule-string length
  # when no `priority:` is set, so the longer/more specific rule wins.
  if grep -q '^      priority:' "$output-ws" "$output-http"; then
    echo "$name IngressRoute pair unexpectedly pins an explicit priority — see comment in templates/ingressroute.yaml"
    exit 1
  fi

  # Verify the structural property the default-priority contract depends
  # on: the /ws rule's match string must be strictly longer than the
  # general rule's match string, for every host value, so Traefik's
  # rule-length ordering always ranks it first. The /ws match is
  # constructed as the general match plus a fixed ` && PathPrefix(`/ws`)`
  # suffix, so this holds regardless of the rendered host name.
  local ws_match http_match
  ws_match=$(grep '^      match:' "$output-ws" | sed 's/^      match: //')
  http_match=$(grep '^      match:' "$output-http" | sed 's/^      match: //')
  test -n "$ws_match"
  test -n "$http_match"
  if [ "${#ws_match}" -le "${#http_match}" ]; then
    echo "$name /ws rule (\"$ws_match\") is not longer than the general rule (\"$http_match\") — Traefik's default rule-length priority would no longer favor /ws"
    exit 1
  fi
  case "$ws_match" in
    "$http_match"' && PathPrefix(`/ws`)') ;;
    *)
      echo "$name /ws rule (\"$ws_match\") is not the general rule (\"$http_match\") plus the expected PathPrefix suffix"
      exit 1
      ;;
  esac
}

assert_ingressroute_pair phase-server scaleout-entry.yaml
assert_ingressroute_pair phase-server-0 scaleout-ordinal.yaml
