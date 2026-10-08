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

# Guard: the shipped engine wasm must carry the enlarged shadow stack from
# .cargo/config.toml [target.wasm32-unknown-unknown] (-z stack-size=16MiB). The
# stack is reserved at the start of linear memory, so the module's declared
# initial memory `min` (in 64 KiB pages) is >= the stack size. If the link-arg is
# ever dropped (e.g. a rebase drops the config line) min falls to ~17 pages and
# the CR 732.2a loop replay overflows the 1 MiB default in WASM ("connection
# lost"). 200 pages (12.5 MiB) separates the 16 MiB build (>=256) from the 1 MiB
# default (~17). Parsed with node (repo dev-dep); no wasm tooling required.
assert_wasm_stack() {
  local wasm="$1" want="$2"
  local pages
  pages=$(node -e '
    const fs = require("fs");
    const b = fs.readFileSync(process.argv[1]);
    const leb = (o) => { let r = 0, s = 0, by; do { by = b[o++]; r |= (by & 0x7f) << s; s += 7; } while (by & 0x80); return [r >>> 0, o]; };
    let o = 8; // skip magic + version
    while (o < b.length) { const id = b[o++]; let [len, o2] = leb(o); o = o2; const end = o + len;
      if (id === 5) { let a = leb(o); o = a[1]; let f = leb(o); o = f[1]; let m = leb(o); console.log(m[0]); process.exit(0); }
      o = end; }
    console.error("no memory section"); process.exit(2);
  ' "$wasm") || { echo "ERROR: cannot read memory section of $wasm" >&2; exit 1; }
  if [ "$pages" -lt "$want" ]; then
    echo "ERROR: $wasm initial memory min=$pages pages (< $want). The WASM shadow-stack link-arg is missing from .cargo/config.toml [target.wasm32-unknown-unknown] -> the CR 732.2a loop replay overflows the browser's 1 MiB default stack. Restore the -z stack-size rustflags." >&2
    exit 1
  fi
  echo "  stack guard OK: $(basename "$wasm") memory min=$pages pages (>= $want, ~$((pages * 64 / 1024)) MiB)"
}

mkdir -p "$WASM_OUT"

build_wasm_crate engine-wasm engine_wasm
assert_wasm_stack "$WASM_OUT/engine_wasm_bg.wasm" 200
build_wasm_crate draft-wasm draft_wasm

echo ""
echo "WASM build complete. Output in $WASM_OUT"
echo "  engine: $(du -h "$WASM_OUT/engine_wasm_bg.wasm" | cut -f1)"
echo "  draft:  $(du -h "$WASM_OUT/draft_wasm_bg.wasm" | cut -f1)"
