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
TREE_PROTOCOL="$(grep -oE 'pub const PROTOCOL_VERSION: u32 = [0-9]+' \
  "$ROOT/crates/lobby-broker/src/protocol.rs" | grep -oE '[0-9]+$')"

echo "lobby-servers tests"

# The greps themselves must be non-empty, or every assertion below that uses
# them is vacuously satisfiable.
if [ -n "$INFO_PATH" ] && [ -n "$TREE_LOBBY_PROTOCOL" ] && [ -n "$TREE_PROTOCOL" ]; then
  ok "the tree's INFO_PATH and both protocol constants are readable ($INFO_PATH, $TREE_LOBBY_PROTOCOL, $TREE_PROTOCOL)"
else
  fail "could not read INFO_PATH / the protocol constants from the tree"
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
# Without --remote, wrangler 4 can write the LOCAL simulator instead of the
# deployed namespace: the command succeeds, the operator sees "added", and the
# directory never lists the server.
case "$PUT_LINE" in
  *"--remote"*) ok "add targets remote storage" ;;
  *) fail "add targets remote storage (got: $PUT_LINE)" ;;
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
case "$DEL_LINE" in
  *"--remote"*) ok "remove targets remote storage" ;;
  *) fail "remove targets remote storage (got: $DEL_LINE)" ;;
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
# The protocol half matches the tree and only the LOBBY half is behind, so the
# two numbers are reported independently rather than one standing in for both.
printf '{"mode":"Full","protocol_version":%d,"lobby_protocol_version":%d,"server_version":"0.9.1"}\n' \
  "$TREE_PROTOCOL" "$((TREE_LOBBY_PROTOCOL - 2))" > "$FIXTURE/info.json"
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
  *"lobby behind by 2"*) ok "list reports the lobby version delta against the tree" ;;
  *) fail "list reports the lobby version delta against the tree (got: $OUT)" ;;
esac
# LOW-5's core: BOTH constants are compared per row. This fixture is behind on
# one and level on the other, so a script reporting only one number cannot
# satisfy both halves.
case "$OUT" in
  *"protocol up to date"*) ok "list reports the full-game version separately (level here)" ;;
  *) fail "list reports the full-game version separately (got: $OUT)" ;;
esac
case "$OUT" in
  *"protocol_version=$TREE_PROTOCOL"*) ok "list prints the remote's full-game version" ;;
  *) fail "list prints the remote's full-game version (got: $OUT)" ;;
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
printf '{"mode":"Full","server_version":"0.9.1","protocol_version":%d,"lobby_protocol_version":%d}\n' \
  "$TREE_PROTOCOL" "$TREE_LOBBY_PROTOCOL" > "$FIXTURE/info.json"
run_script list
case "$OUT" in
  *"lobby up to date"*) ok "list reports an up-to-date server as up to date" ;;
  *) fail "list reports an up-to-date server as up to date (got: $OUT)" ;;
esac
rm -rf "$FIXTURE"

# A remote AHEAD of the tree: this checkout is stale, not the server. The old
# single-branch arithmetic printed "behind by -1" here, which reads as a bug in
# the script rather than a fact about the deployment.
make_fixture
printf '[{"name":"wss://play.example.com/ws"}]\n' > "$FIXTURE/kv_list.json"
printf '{"mode":"Full","server_version":"0.9.1","protocol_version":%d,"lobby_protocol_version":%d}\n' \
  "$((TREE_PROTOCOL + 3))" "$((TREE_LOBBY_PROTOCOL + 1))" > "$FIXTURE/info.json"
run_script list
case "$OUT" in
  *"lobby ahead by 1"*) ok "a remote ahead of the tree reads as ahead, not as a negative delta" ;;
  *) fail "a remote ahead of the tree reads as ahead (got: $OUT)" ;;
esac
case "$OUT" in
  *"protocol ahead by 3"*) ok "the full-game version reports its own ahead-by count" ;;
  *) fail "the full-game version reports its own ahead-by count (got: $OUT)" ;;
esac
case "$OUT" in
  *"by -"*) fail "no delta is printed as a negative number (got: $OUT)" ;;
  *) ok "no delta is printed as a negative number" ;;
esac
rm -rf "$FIXTURE"

# The skew the SPLIT constant exists to catch: the full-game number moved and
# the lobby's did not. A row reporting one number for both would print the same
# word twice here.
make_fixture
printf '[{"name":"wss://play.example.com/ws"}]\n' > "$FIXTURE/kv_list.json"
printf '{"mode":"Full","server_version":"0.9.1","protocol_version":%d,"lobby_protocol_version":%d}\n' \
  "$((TREE_PROTOCOL - 4))" "$TREE_LOBBY_PROTOCOL" > "$FIXTURE/info.json"
run_script list
case "$OUT" in
  *"lobby up to date, protocol behind by 4"*)
    ok "a full-game-only skew is reported on the full-game number alone" ;;
  *) fail "a full-game-only skew is reported on the full-game number alone (got: $OUT)" ;;
esac
rm -rf "$FIXTURE"

# A remote version that is not a number at all. The value comes from a
# document a third party serves, so it can be anything; before the guard,
# bash's arithmetic aborted the whole run under `set -e` and every key AFTER
# the offending one went unprinted. Two keys, so the abort is what is being
# tested rather than just the bad row's wording.
make_fixture
printf '[{"name":"wss://bad.example.com/ws"},{"name":"wss://good.example.com/ws"}]\n' \
  > "$FIXTURE/kv_list.json"
printf '{"mode":"Full","server_version":"0.9.1","protocol_version":"1.2.3","lobby_protocol_version":%d}\n' \
  "$TREE_LOBBY_PROTOCOL" > "$FIXTURE/info.json"
run_script list
if [ "$STATUS" -eq 0 ]; then
  ok "a non-numeric remote version does not abort the listing"
else
  fail "a non-numeric remote version does not abort the listing (exit $STATUS, got: $OUT)"
fi
case "$OUT" in
  *"protocol unknown"*) ok "a non-numeric version reports its own value as unknown" ;;
  *) fail "a non-numeric version reports its own value as unknown (got: $OUT)" ;;
esac
# The point of the pair: the SECOND key must still be printed. Before the
# guard, the loop died on the first row.
case "$OUT" in
  *"wss://good.example.com/ws"*) ok "a later key is still printed after a bad one" ;;
  *) fail "a later key is still printed after a bad one (got: $OUT)" ;;
esac
# The other half of the row is unaffected — one bad number costs its own
# label, not the row.
case "$OUT" in
  *"lobby up to date"*) ok "the readable half of a partly-bad document still reports" ;;
  *) fail "the readable half of a partly-bad document still reports (got: $OUT)" ;;
esac
rm -rf "$FIXTURE"

# A leading-zero version string. Bash reads `010` as OCTAL 8 unless the
# arithmetic forces base 10, so before the `10#` prefix this printed a skew of
# 47 against a tree of 55 where the truth is 45 — a wrong number, silently,
# from a third-party document.
make_fixture
printf '[{"name":"wss://play.example.com/ws"}]\n' > "$FIXTURE/kv_list.json"
printf '{"mode":"Full","server_version":"0.9.1","protocol_version":"010","lobby_protocol_version":%d}\n' \
  "$TREE_LOBBY_PROTOCOL" > "$FIXTURE/info.json"
run_script list
case "$OUT" in
  *"protocol behind by $((TREE_PROTOCOL - 10))"*)
    ok "a leading-zero version is read as decimal, not octal" ;;
  *) fail "a leading-zero version is read as decimal, not octal (got: $OUT)" ;;
esac
rm -rf "$FIXTURE"

# ── V-U8g: list marks keys the Worker would drop ───────────────────────────
# A document carrying BOTH version numbers and neither of the other two
# required fields. Its versions look perfectly healthy, and it is exactly as
# dead as an empty document: `ServerInfoDocument` requires `mode` and
# `server_version` too, so it does not deserialize and the announce is
# refused.
make_fixture
printf '[{"name":"wss://play.example.com/ws"}]\n' > "$FIXTURE/kv_list.json"
printf '{"protocol_version":%d,"lobby_protocol_version":%d}\n' \
  "$TREE_PROTOCOL" "$TREE_LOBBY_PROTOCOL" > "$FIXTURE/info.json"
run_script list
case "$OUT" in
  *"DROPPED-BY-WORKER?"*) ok "a versions-only document is marked DROPPED-BY-WORKER?" ;;
  *) fail "a versions-only document is marked DROPPED-BY-WORKER? (got: $OUT)" ;;
esac
rm -rf "$FIXTURE"

# A PARTIAL info document. `ServerInfoDocument` requires all four fields, so a
# document missing `lobby_protocol_version` does not deserialize, the Worker
# refuses the announce, and the server is never listed — indistinguishable in
# outcome from no document at all, and the marker must say so.
make_fixture
printf '[{"name":"wss://play.example.com/ws"}]\n' > "$FIXTURE/kv_list.json"
printf '{"mode":"Full","protocol_version":%d,"server_version":"0.9.1"}\n' \
  "$TREE_PROTOCOL" > "$FIXTURE/info.json"
run_script list
case "$OUT" in
  *"DROPPED-BY-WORKER?"*) ok "a partial info document is marked DROPPED-BY-WORKER?" ;;
  *) fail "a partial info document is marked DROPPED-BY-WORKER? (got: $OUT)" ;;
esac
case "$OUT" in
  *"partial info document"*) ok "a partial document is named as partial, not as absent" ;;
  *) fail "a partial document is named as partial, not as absent (got: $OUT)" ;;
esac
rm -rf "$FIXTURE"

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
