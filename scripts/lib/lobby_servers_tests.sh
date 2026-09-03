#!/usr/bin/env bash
# Tests for scripts/lobby-servers.sh — the operator's handle on the lobby
# directory allowlist.
#
# What these exist for: the script is the ONLY way a server gets admitted to
# `GET /servers`, and every one of its failure modes is silent. A key written
# with a `ws://` scheme matches no row. A probe pointed at a path that is not
# `lobby_broker::directory::INFO_PATH` reports every server as unreachable. A
# `--env preview` that is not forwarded edits PRODUCTION. None of those
# produces an error anywhere — the listing is just quietly wrong.
#
# `wrangler` and `curl` are stubbed on a narrowed PATH and record their argv,
# so the assertions are about what the script WOULD send, not about Cloudflare.
# jq's real directory is added to that PATH: the script parses `kv key list`
# output with it, and stubbing a JSON parser would test the stub.
#
# Venue: the Tilt `lobby-servers` resource (label 'lint'). NOT GitHub CI —
# enrolling a script gate there needs a `.github/workflows/**` edit, which is a
# hard stop for agent changes, the same reason `pnpm-preflight` and probe-pin
# live here. So this gate is local-only: a contributor who never runs
# `tilt up -- lint` never runs it.
#
# Run:  bash scripts/lib/lobby_servers_tests.sh

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT="$ROOT/scripts/lobby-servers.sh"
JQ_DIR="$(dirname "$(command -v jq || echo /usr/bin/jq)")"

PASS=0
FAIL=0

fail() { printf '  FAIL: %s\n' "$1" >&2; FAIL=$((FAIL + 1)); }
ok()   { printf '  ok: %s\n' "$1";     PASS=$((PASS + 1)); }

# Build a throwaway bin/ whose `wrangler` and `curl` log their argv and print
# whatever the fixture files hold.
make_fixture() {
  FIXTURE="$(mktemp -d)"
  mkdir -p "$FIXTURE/bin"
  : > "$FIXTURE/wrangler.log"
  : > "$FIXTURE/curl.log"
  : > "$FIXTURE/kv_list.json"
  : > "$FIXTURE/kv_get.txt"
  : > "$FIXTURE/info.json"

  cat > "$FIXTURE/bin/wrangler" <<STUB
#!/bin/sh
printf '%s\n' "\$*" >> "$FIXTURE/wrangler.log"
case "\$1 \$2 \$3" in
  "kv key list") cat "$FIXTURE/kv_list.json" ;;
  "kv key get")  cat "$FIXTURE/kv_get.txt" ;;
esac
exit 0
STUB

  cat > "$FIXTURE/bin/curl" <<STUB
#!/bin/sh
printf '%s\n' "\$*" >> "$FIXTURE/curl.log"
cat "$FIXTURE/info.json"
exit 0
STUB

  chmod +x "$FIXTURE/bin/wrangler" "$FIXTURE/bin/curl"
}

run_script() { # $@ = script args; stdout captured in OUT, exit in STATUS
  OUT="$(PATH="$FIXTURE/bin:$JQ_DIR:/usr/bin:/bin" bash "$SCRIPT" "$@" 2>/dev/null)"
  STATUS=$?
}

# The constants the script reads, read again here INDEPENDENTLY. Asserting the
# probe path against a value grepped from the same file is the point of V-U8d:
# a hardcoded "/info" in the script passes only while the constant happens to
# say "/info".
INFO_PATH="$(grep -oE 'pub const INFO_PATH: &str = "[^"]+"' \
  "$ROOT/crates/lobby-broker/src/directory.rs" | grep -oE '"[^"]+"' | tr -d '"')"
TREE_LOBBY_PROTOCOL="$(grep -oE 'pub const LOBBY_PROTOCOL_VERSION: u32 = [0-9]+' \
  "$ROOT/crates/lobby-broker/src/protocol.rs" | grep -oE '[0-9]+$')"

echo "lobby-servers tests"

# The greps themselves must be non-empty, or every assertion below that uses
# them is vacuously satisfiable.
if [ -n "$INFO_PATH" ] && [ -n "$TREE_LOBBY_PROTOCOL" ]; then
  ok "the tree's INFO_PATH and LOBBY_PROTOCOL_VERSION are readable ($INFO_PATH, $TREE_LOBBY_PROTOCOL)"
else
  fail "could not read INFO_PATH / LOBBY_PROTOCOL_VERSION from the tree"
fi

# ── V-U8a: add and remove reach wrangler with the right key AND value ───────
make_fixture
run_script add "wss://play.example.com/ws" --env preview
if [ "$STATUS" -eq 0 ]; then ok "add exits 0"; else fail "add exits 0 (got $STATUS)"; fi

PUT_LINE="$(grep '^kv key put' "$FIXTURE/wrangler.log" || true)"
case "$PUT_LINE" in
  *"--binding SERVER_ALLOWLIST"*) ok "add binds SERVER_ALLOWLIST" ;;
  *) fail "add binds SERVER_ALLOWLIST (got: $PUT_LINE)" ;;
esac
case "$PUT_LINE" in
  *"wss://play.example.com/ws"*) ok "add passes the URL as the key" ;;
  *) fail "add passes the URL as the key (got: $PUT_LINE)" ;;
esac
case "$PUT_LINE" in
  *"--env preview"*) ok "add forwards --env preview" ;;
  *) fail "add forwards --env preview (got: $PUT_LINE)" ;;
esac
# By REGEX, never a literal: a pinned timestamp is red one second after it is
# written.
VALUE="$(printf '%s' "$PUT_LINE" | grep -oE '[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z' || true)"
if [ -n "$VALUE" ]; then
  ok "add writes an ISO-8601 UTC timestamp as the value ($VALUE)"
else
  fail "add writes an ISO-8601 UTC timestamp as the value (got: $PUT_LINE)"
fi
rm -rf "$FIXTURE"

make_fixture
run_script remove "wss://play.example.com/ws" --env preview
DEL_LINE="$(grep '^kv key delete' "$FIXTURE/wrangler.log" || true)"
case "$DEL_LINE" in
  *"--binding SERVER_ALLOWLIST"*"wss://play.example.com/ws"*|*"wss://play.example.com/ws"*"--binding SERVER_ALLOWLIST"*)
    ok "remove deletes the key from the same binding" ;;
  *) fail "remove deletes the key from the same binding (got: $DEL_LINE)" ;;
esac
case "$DEL_LINE" in
  *"--env preview"*) ok "remove forwards --env preview" ;;
  *) fail "remove forwards --env preview (got: $DEL_LINE)" ;;
esac
rm -rf "$FIXTURE"

# ── V-U8b: ws:// is refused BEFORE wrangler is called ───────────────────────
make_fixture
run_script add "ws://play.example.com/ws"
if [ "$STATUS" -ne 0 ]; then ok "ws:// exits non-zero"; else fail "ws:// exits non-zero"; fi
if [ ! -s "$FIXTURE/wrangler.log" ]; then
  ok "ws:// never reaches wrangler"
else
  fail "ws:// never reaches wrangler (log: $(cat "$FIXTURE/wrangler.log"))"
fi
rm -rf "$FIXTURE"

# The paired positive for the assertion above: an empty log must mean
# "refused", not "the stub never runs".
make_fixture
run_script add "wss://play.example.com/ws"
if [ "$STATUS" -eq 0 ] && [ -s "$FIXTURE/wrangler.log" ]; then
  ok "wss:// does reach wrangler (the empty-log assertion is not vacuous)"
else
  fail "wss:// does reach wrangler (status $STATUS, log: $(cat "$FIXTURE/wrangler.log"))"
fi
rm -rf "$FIXTURE"

# ── V-U8c: list prints the key, its added-at value, and the version delta ───
make_fixture
printf '[{"name":"wss://play.example.com/ws"}]\n' > "$FIXTURE/kv_list.json"
printf '2026-09-02T21:14:07Z' > "$FIXTURE/kv_get.txt"
printf '{"mode":"Full","protocol_version":55,"lobby_protocol_version":%d,"server_version":"0.9.1"}\n' \
  "$((TREE_LOBBY_PROTOCOL - 2))" > "$FIXTURE/info.json"
run_script list

case "$OUT" in
  *"wss://play.example.com/ws"*) ok "list prints the key" ;;
  *) fail "list prints the key (got: $OUT)" ;;
esac
case "$OUT" in
  *"2026-09-02T21:14:07Z"*) ok "list prints the stored added-at value" ;;
  *) fail "list prints the stored added-at value (got: $OUT)" ;;
esac
case "$OUT" in
  *"behind by 2"*) ok "list reports the version delta against the tree" ;;
  *) fail "list reports the version delta against the tree (got: $OUT)" ;;
esac
# V-U8d: the probed path IS the Rust constant, and the grep above is non-empty.
case "$(cat "$FIXTURE/curl.log")" in
  *"https://play.example.com${INFO_PATH}"*) ok "list probes the tree's INFO_PATH ($INFO_PATH)" ;;
  *) fail "list probes the tree's INFO_PATH (got: $(cat "$FIXTURE/curl.log"))" ;;
esac
# V-U8g's paired positive: a well-formed key with a good info document must
# print WITHOUT the marker, so a script that marks everything fails.
case "$OUT" in
  *"DROPPED-BY-WORKER?"*) fail "a healthy key is not marked (got: $OUT)" ;;
  *) ok "a healthy key is not marked" ;;
esac
rm -rf "$FIXTURE"

# The up-to-date form, so "behind by 2" is a computed delta rather than a
# constant string.
make_fixture
printf '[{"name":"wss://play.example.com/ws"}]\n' > "$FIXTURE/kv_list.json"
printf '2026-09-02T21:14:07Z' > "$FIXTURE/kv_get.txt"
printf '{"lobby_protocol_version":%d}\n' "$TREE_LOBBY_PROTOCOL" > "$FIXTURE/info.json"
run_script list
case "$OUT" in
  *"up to date"*) ok "list reports an up-to-date server as up to date" ;;
  *) fail "list reports an up-to-date server as up to date (got: $OUT)" ;;
esac
rm -rf "$FIXTURE"

# ── V-U8g: list marks keys the Worker would drop ───────────────────────────
# A key that fails the script's own shape check.
make_fixture
printf '[{"name":"wss://localhost/ws"}]\n' > "$FIXTURE/kv_list.json"
printf '{"lobby_protocol_version":%d}\n' "$TREE_LOBBY_PROTOCOL" > "$FIXTURE/info.json"
run_script list
case "$OUT" in
  *"DROPPED-BY-WORKER?"*) ok "a single-label host is marked DROPPED-BY-WORKER?" ;;
  *) fail "a single-label host is marked DROPPED-BY-WORKER? (got: $OUT)" ;;
esac
rm -rf "$FIXTURE"

# A key whose info probe returns nothing parseable: the Worker's verification
# fetch would fail too, so the server cannot be listed even though the key is
# present.
make_fixture
printf '[{"name":"wss://play.example.com/ws"}]\n' > "$FIXTURE/kv_list.json"
: > "$FIXTURE/info.json"
run_script list
case "$OUT" in
  *"DROPPED-BY-WORKER?"*) ok "an unprobeable key is marked DROPPED-BY-WORKER?" ;;
  *) fail "an unprobeable key is marked DROPPED-BY-WORKER? (got: $OUT)" ;;
esac
rm -rf "$FIXTURE"

# A bare IPv4 literal — refused by the Worker for the same SSRF-shaped reason
# `normalize_announced_url` refuses it.
make_fixture
printf '[{"name":"wss://1.2.3.4/ws"}]\n' > "$FIXTURE/kv_list.json"
printf '{"lobby_protocol_version":%d}\n' "$TREE_LOBBY_PROTOCOL" > "$FIXTURE/info.json"
run_script list
case "$OUT" in
  *"DROPPED-BY-WORKER?"*) ok "a bare IPv4 key is marked DROPPED-BY-WORKER?" ;;
  *) fail "a bare IPv4 key is marked DROPPED-BY-WORKER? (got: $OUT)" ;;
esac
rm -rf "$FIXTURE"

# ── V-U8e: the Helm ingress still routes INFO_PATH ─────────────────────────
INGRESS="$ROOT/deploy/helm/phase-server/templates/ingress.yaml"
if [ -n "$INFO_PATH" ] && grep -qF "\"$INFO_PATH\"" "$INGRESS"; then
  ok "the Helm ingress routes lobby_broker::INFO_PATH ($INFO_PATH)"
else
  fail "the Helm ingress no longer routes lobby_broker::INFO_PATH — fix the chart or the constant, not this test"
fi

# ── V-U8f: both scripts are syntactically valid ────────────────────────────
if bash -n "$SCRIPT"; then ok "scripts/lobby-servers.sh parses"; else fail "scripts/lobby-servers.sh parses"; fi
if bash -n "${BASH_SOURCE[0]}"; then ok "this test file parses"; else fail "this test file parses"; fi

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
