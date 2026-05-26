#!/usr/bin/env bash
# Build the Rust `lobby-broker` core to WASM for the Cloudflare Durable Object
# shell, emitting wasm-bindgen "web"-target glue to
# lobby-worker/src/broker-wasm-pkg/.
#
# Invoked by wrangler's [build] command (lobby-worker/wrangler.toml) on
# `wrangler deploy` / `wrangler dev`, and directly in CI (release workflow).
#
# Requires: rustup wasm32-unknown-unknown target, wasm-bindgen-cli (pinned 0.2.121
# to match the crate), and optionally wasm-opt (binaryen) for release size.
set -euo pipefail

PROFILE="${1:-release}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CRATE_DIR="$ROOT/lobby-worker/broker-wasm"
OUT_DIR="$ROOT/lobby-worker/src/broker-wasm-pkg"

# Build from the crate dir so its .cargo/config.toml — which sets
# `getrandom_backend="wasm_js"` for wasm32 — is on cargo's config search path.
cd "$CRATE_DIR"
if [ "$PROFILE" = "release" ]; then
  cargo build --target wasm32-unknown-unknown --release
  WASM="$CRATE_DIR/target/wasm32-unknown-unknown/release/lobby_broker_wasm.wasm"
else
  cargo build --target wasm32-unknown-unknown
  WASM="$CRATE_DIR/target/wasm32-unknown-unknown/debug/lobby_broker_wasm.wasm"
fi

wasm-bindgen --target web --out-dir "$OUT_DIR" --out-name broker "$WASM"

# The web-target glue receives the WebAssembly.Module via initSync and never
# imports broker_bg.wasm as a typed module, so this generated declaration is
# dead — and it would shadow the ambient `*.wasm` default-import type the DO
# shell relies on. Remove it.
rm -f "$OUT_DIR/broker_bg.wasm.d.ts"

if [ "$PROFILE" = "release" ] && command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt -Oz --strip-debug --enable-bulk-memory --enable-nontrapping-float-to-int \
    "$OUT_DIR/broker_bg.wasm" -o "$OUT_DIR/broker_bg.wasm"
fi

echo "broker-wasm built ($PROFILE): $(du -h "$OUT_DIR/broker_bg.wasm" | cut -f1)"
