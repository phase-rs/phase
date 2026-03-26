#!/usr/bin/env bash
set -euo pipefail

echo "=== phase.rs Setup ==="
echo ""

echo "Step 1/3: Generating card data + building WASM (parallel)..."
./scripts/gen-card-data.sh &
PID_CARDS=$!
./scripts/build-wasm.sh &
PID_WASM=$!
./scripts/gen-scryfall-images.sh &
PID_IMAGES=$!

FAIL=0
wait $PID_CARDS || FAIL=1
wait $PID_WASM || FAIL=1
wait $PID_IMAGES || FAIL=1
if [ $FAIL -ne 0 ]; then
  echo "ERROR: Card data generation or WASM build failed."
  exit 1
fi

echo ""
echo "Step 2/3: Installing frontend dependencies..."
(cd client && pnpm install)

echo ""
echo "Step 3/3: Configuring git hooks..."
git config core.hooksPath .githooks

echo ""
echo "Done!"
echo ""
echo "Run 'cd client && pnpm dev' to start the dev server."
