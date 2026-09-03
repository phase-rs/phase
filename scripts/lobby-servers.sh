#!/usr/bin/env bash
# Manage the official lobby directory's server allowlist (Cloudflare KV).
#
#   scripts/lobby-servers.sh list [--env preview]
#   scripts/lobby-servers.sh add    wss://play.example.com/ws [--env preview]
#   scripts/lobby-servers.sh remove wss://play.example.com/ws [--env preview]
#
# WHAT THE ALLOWLIST IS. `GET /servers` lists a server iff it is announcing
# (a fresh row in the Durable Object) AND its canonical URL is a key in this
# namespace. Membership is the entire contract the Worker reads — the VALUE is
# an operator-facing note it never parses, which is what keeps the allowlist a
# set rather than a second unvalidated schema. `add` writes the UTC timestamp
# of the add, so `list` can answer "when did this get here?".
#
# The allowlist is the ADMISSION gate; the health score only ORDERS what is
# already admitted. A server cannot promote itself into the listing by
# reporting good metrics, and a well-scoring server that is not a key here is
# not listed at all. Keep it that way: any change that lets the score affect
# admission turns a curated directory into an open one.
#
# PREREQUISITES. The KV namespace is a created Cloudflare resource. Once, per
# environment, with your own Cloudflare login:
#
#   cd lobby-worker
#   npx wrangler kv namespace create SERVER_ALLOWLIST                 # production
#   npx wrangler kv namespace create SERVER_ALLOWLIST --env preview   # preview
#
# then paste the returned ids over the `REPLACE_WITH_REAL_KV_NAMESPACE_ID`
# sentinels in lobby-worker/wrangler.toml. Until that is done every command
# here fails at wrangler, and the Worker deploys with an empty directory.
#
# Requires: wrangler (the pinned one in lobby-worker/node_modules), jq, curl.
# Tests: scripts/lib/lobby_servers_tests.sh (Tilt resource `lobby-servers`,
# label 'lint' — local-only, not CI).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BINDING="SERVER_ALLOWLIST"

usage() {
  sed -n '3,6p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  exit 2
}

# ── Constants read from the tree, never retyped ────────────────────────────
# Same idiom as the deploy workflow's protocol-version check: grep the Rust
# constant out of the source and fail loudly if the grep comes back empty,
# because an empty value would silently probe `https://host` and compare
# against nothing.
read_constants() {
  INFO_PATH="$(grep -oE 'pub const INFO_PATH: &str = "[^"]+"' \
    "$ROOT/crates/lobby-broker/src/directory.rs" | grep -oE '"[^"]+"' | tr -d '"')"
  TREE_PROTOCOL="$(grep -oE 'pub const PROTOCOL_VERSION: u32 = [0-9]+' \
    "$ROOT/crates/lobby-broker/src/protocol.rs" | grep -oE '[0-9]+$')"
  TREE_LOBBY_PROTOCOL="$(grep -oE 'pub const LOBBY_PROTOCOL_VERSION: u32 = [0-9]+' \
    "$ROOT/crates/lobby-broker/src/protocol.rs" | grep -oE '[0-9]+$')"
  if [ -z "$INFO_PATH" ] || [ -z "$TREE_PROTOCOL" ] || [ -z "$TREE_LOBBY_PROTOCOL" ]; then
    echo "error: could not read INFO_PATH / protocol constants from the tree." >&2
    exit 1
  fi
}

# ── Argument parsing ───────────────────────────────────────────────────────
COMMAND=""
URL=""
ENV_ARGS=()

while [ $# -gt 0 ]; do
  case "$1" in
    --env)
      [ $# -ge 2 ] || usage
      ENV_ARGS+=(--env "$2")
      shift 2
      ;;
    list|add|remove)
      [ -z "$COMMAND" ] || usage
      COMMAND="$1"
      shift
      ;;
    -h|--help) usage ;;
    *)
      [ -z "$URL" ] || usage
      URL="$1"
      shift
      ;;
  esac
done
[ -n "$COMMAND" ] || usage

wrangler_kv() {
  # Run from lobby-worker/ so wrangler finds its config, and always against
  # REMOTE storage: this script manages the deployed allowlist, never a local
  # simulator.
  (cd "$ROOT/lobby-worker" && wrangler kv "$@" --binding "$BINDING" --remote \
    ${ENV_ARGS[@]+"${ENV_ARGS[@]}"})
}

# ── The Worker's admission shape, approximately ────────────────────────────
# A CHEAP shape check, deliberately not a reimplementation of
# `lobby_broker::directory::normalize_announced_url`. That function is ten
# ordered rules including a public-DNS check, a non-public-TLD list, an
# IP-literal parse and a port range; a bash copy would be a second authority
# that drifts the first time a rule changes, and it would sometimes be wrong
# in the direction that matters.
#
# So this answers "would the Worker probably drop this key?", and `list`
# prints the answer as a QUESTION (`DROPPED-BY-WORKER?`). Bash can say
# "probably dropped"; only the Worker can say "dropped".
looks_dialable() {
  local url="$1" rest host
  rest="${url#wss://}"
  [ "$rest" != "$url" ] || return 1
  case "$url" in *' '*|*@*|*'?'*|*'#'*) return 1 ;; esac
  host="${rest%%/*}"
  host="${host%%:*}"
  # At least two dot-separated labels: a single label cannot be resolved from
  # the public internet.
  case "$host" in *.*) ;; *) return 1 ;; esac
  # A bare IPv4 literal is refused by the Worker (SSRF-shaped, and unlistable).
  if [[ "$host" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then return 1; fi
  return 0
}

# The authority (host[:port]) of a `wss://` URL — the same slice the Worker's
# info probe uses.
authority_of() {
  local rest="${1#wss://}"
  printf '%s' "${rest%%/*}"
}

cmd_add() {
  [ -n "$URL" ] || usage
  # Refused BEFORE wrangler is touched: a `ws://` key would sit in the
  # allowlist matching nothing, since the directory keys on `wss://` only.
  case "$URL" in
    wss://*) ;;
    *)
      echo "error: server URLs must start with wss:// (got: $URL)" >&2
      exit 1
      ;;
  esac
  if ! looks_dialable "$URL"; then
    echo "warning: $URL does not look dialable from the public internet;" >&2
    echo "         the Worker will probably drop it. Adding it anyway." >&2
  fi
  # The value is the ISO-8601 UTC time of the add. `+%FT%TZ` is POSIX and
  # gives exactly YYYY-MM-DDTHH:MM:SSZ.
  wrangler_kv key put "$URL" "$(date -u +%FT%TZ)"
  echo "added $URL"
}

cmd_remove() {
  [ -n "$URL" ] || usage
  wrangler_kv key delete "$URL"
  echo "removed $URL"
}

cmd_list() {
  local keys key added authority info remote_lobby delta marker
  keys="$(wrangler_kv key list | jq -r '.[].name')"
  if [ -z "$keys" ]; then
    echo "(the allowlist is empty — GET /servers lists nothing)"
    return 0
  fi
  printf 'tree: protocol_version=%s lobby_protocol_version=%s info_path=%s\n\n' \
    "$TREE_PROTOCOL" "$TREE_LOBBY_PROTOCOL" "$INFO_PATH"
  while IFS= read -r key; do
    [ -n "$key" ] || continue
    marker=""
    looks_dialable "$key" || marker=" DROPPED-BY-WORKER?"
    added="$(wrangler_kv key get "$key" 2>/dev/null || true)"
    authority="$(authority_of "$key")"
    info="$(curl -sS -m 5 "https://${authority}${INFO_PATH}" 2>/dev/null || true)"
    remote_lobby="$(printf '%s' "$info" | jq -r '.lobby_protocol_version // empty' 2>/dev/null || true)"
    if [ -z "$remote_lobby" ]; then
      # No parseable info document means the verification fetch the Worker
      # makes on every announce would fail too, so the server cannot be listed
      # even though its key is here.
      delta="no info document"
      marker=" DROPPED-BY-WORKER?"
    elif [ "$remote_lobby" -eq "$TREE_LOBBY_PROTOCOL" ]; then
      delta="up to date"
    else
      delta="behind by $((TREE_LOBBY_PROTOCOL - remote_lobby))"
    fi
    printf '%s  added=%s  lobby_protocol_version=%s (%s)%s\n' \
      "$key" "${added:-?}" "${remote_lobby:-?}" "$delta" "$marker"
  done <<< "$keys"
}

read_constants
case "$COMMAND" in
  add) cmd_add ;;
  remove) cmd_remove ;;
  list) cmd_list ;;
esac
