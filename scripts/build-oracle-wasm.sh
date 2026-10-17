#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
ORACLE_OUT="${1:-dist/oracle-checker}"
ORACLE_TARGET="${CARGO_TARGET_DIR:-target}"
CARGO_PROFILE_RELEASE_OPT_LEVEL=z CARGO_PROFILE_RELEASE_LTO=true \
CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 CARGO_PROFILE_RELEASE_PANIC=abort \
CARGO_PROFILE_RELEASE_STRIP=symbols cargo build --release --locked --target wasm32-unknown-unknown --manifest-path crates/oracle-wasm/Cargo.toml
wasm-bindgen --target web --out-name checker --out-dir "$ORACLE_OUT" \
  "$ORACLE_TARGET/wasm32-unknown-unknown/release/oracle_wasm.wasm"
if command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt -Oz --enable-bulk-memory --enable-sign-ext --enable-reference-types --enable-multivalue --enable-nontrapping-float-to-int "$ORACLE_OUT/checker_bg.wasm" -o "$ORACLE_OUT/checker_bg.wasm"
fi
printf '{"repository":"https://github.com/phase-rs/phase","revision":"%s","license":"MIT OR Apache-2.0"}\n' "$(git rev-parse HEAD)" > "$ORACLE_OUT/source.json"
