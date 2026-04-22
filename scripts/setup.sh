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

# Optional: fetch the WotC Comprehensive Rules for local CR lookups.
# Gitignored — not redistributed by this repo. Non-fatal on failure.
if [ ! -f docs/MagicCompRules.txt ]; then
  echo ""
  echo "Fetching MTG Comprehensive Rules (local dev reference only)..."
  ./scripts/fetch-comp-rules.sh || echo "  (skipped — you can run ./scripts/fetch-comp-rules.sh later)"
fi

echo ""
echo "Step 2/3: Installing frontend dependencies..."
(cd client && pnpm install)

echo ""
echo "Step 3/3: Configuring git hooks..."
git config --local include.path ../.gitconfig

echo ""
echo "Done!"
echo ""
echo "Run 'cd client && pnpm dev' to start the dev server."
