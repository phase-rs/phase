#!/usr/bin/env bash
set -euo pipefail

echo "=== Forge.rs Setup ==="
echo ""

echo "Step 1/4: Generating card data..."
./scripts/gen-card-data.sh

echo ""
echo "Step 2/4: Building WASM..."
./scripts/build-wasm.sh

echo ""
echo "Step 3/4: Installing frontend dependencies..."
(cd client && pnpm install)

echo ""
echo "Step 4/4: Done!"
echo ""
echo "Run 'cd client && pnpm dev' to start the dev server."
