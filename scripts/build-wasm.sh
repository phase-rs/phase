#!/usr/bin/env bash
set -euo pipefail

WASM_OUT="client/src/wasm"
PROFILE="${1:-wasm-dev}"
# Honor CARGO_TARGET_DIR so callers (e.g. Tilt's dev loop) can relocate the
# build tree off the shared target/ root. cargo build already respects the env
# var; this mirrors it for the wasm-bindgen input path below. Defaults to
# target/ for CI/deploy/setup callers that don't set it.
TARGET_DIR="${CARGO_TARGET_DIR:-target}"

# Build a single WASM crate: compile, bind, optimize.
build_wasm_crate() {
  local PACKAGE="$1"
  local OUT_NAME="$2"

  echo "Building $PACKAGE (profile: $PROFILE)..."

  if [ "$PROFILE" = "release" ]; then
    cargo build --package "$PACKAGE" --target wasm32-unknown-unknown --release
  else
    cargo build --package "$PACKAGE" --target wasm32-unknown-unknown --profile "$PROFILE"
  fi

  wasm-bindgen \
    --target web \
    --out-dir "$WASM_OUT" \
    --out-name "$OUT_NAME" \
    "$TARGET_DIR/wasm32-unknown-unknown/$PROFILE/${PACKAGE//-/_}.wasm"

  if [ "$PROFILE" = "release" ] && command -v wasm-opt &> /dev/null; then
    echo "Optimizing $OUT_NAME..."
    wasm-opt -Oz --strip-debug --enable-bulk-memory --enable-nontrapping-float-to-int \
      "$WASM_OUT/${OUT_NAME}_bg.wasm" \
      -o "$WASM_OUT/${OUT_NAME}_bg.wasm"
  fi
}

mkdir -p "$WASM_OUT"

build_wasm_crate engine-wasm engine_wasm
build_wasm_crate draft-wasm draft_wasm

echo ""
echo "WASM build complete. Output in $WASM_OUT"
echo "  engine: $(du -h "$WASM_OUT/engine_wasm_bg.wasm" | cut -f1)"
echo "  draft:  $(du -h "$WASM_OUT/draft_wasm_bg.wasm" | cut -f1)"
