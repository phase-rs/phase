#!/usr/bin/env bash
set -euo pipefail

WASM_OUT="client/src/wasm"
PROFILE="${1:-wasm-dev}"

echo "Building WASM (profile: $PROFILE)..."

# Step 1: Compile Rust to WASM
if [ "$PROFILE" = "release" ]; then
  cargo build --package engine-wasm --target wasm32-unknown-unknown --release
else
  cargo build --package engine-wasm --target wasm32-unknown-unknown --profile "$PROFILE"
fi

# Step 2: Generate JS/TS bindings
mkdir -p "$WASM_OUT"
wasm-bindgen \
  --target web \
  --out-dir "$WASM_OUT" \
  --out-name engine_wasm \
  "target/wasm32-unknown-unknown/$PROFILE/engine_wasm.wasm"

# Step 3: Optimize (release only)
if [ "$PROFILE" = "release" ] && command -v wasm-opt &> /dev/null; then
  echo "Optimizing WASM binary..."
  wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int \
    "$WASM_OUT/engine_wasm_bg.wasm" \
    -o "$WASM_OUT/engine_wasm_bg.wasm"
fi

echo "WASM build complete. Output in $WASM_OUT"
echo "Binary size: $(du -h "$WASM_OUT/engine_wasm_bg.wasm" | cut -f1)"
