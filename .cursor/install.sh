#!/usr/bin/env bash
# Cloud Agent environment bootstrap for phase.rs.
#
# Idempotent: safe to re-run. Installs the two tools the default image lacks
# (a wasm-bindgen-cli whose version matches Cargo.lock, and tilt-dev/tilt), then
# runs the repo's own onboarding script to fetch card data, build the WASM
# bundles, and install frontend dependencies.
#
# Rust (nightly, pinned by rust-toolchain.toml), Node 22, pnpm (via corepack,
# pinned by client/package.json), jq, and a C toolchain are already present in
# the base image, so this script does not reinstall them.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
REPO_ROOT="$(pwd)"

# cargo installs land in $CARGO_HOME/bin, which is already on PATH in the base
# image; export it explicitly so a non-login build shell still finds them.
export PATH="${CARGO_HOME:-/usr/local/cargo}/bin:$PATH"

# --- wasm-bindgen-cli: must exactly match the wasm-bindgen crate in Cargo.lock.
# A mismatch fails build-wasm.sh with a "schema version" error, so we pin to the
# locked version rather than a hardcoded one that drifts on dependency bumps.
WB_WANT="$(awk '/^name = "wasm-bindgen"$/{getline; gsub(/[";]/,""); print $3; exit}' Cargo.lock)"
if [ -z "$WB_WANT" ]; then
  echo "ERROR: could not determine wasm-bindgen version from Cargo.lock" >&2
  exit 1
fi
WB_HAVE="$(wasm-bindgen --version 2>/dev/null | awk '{print $2}' || true)"
if [ "$WB_HAVE" != "$WB_WANT" ]; then
  echo "Installing wasm-bindgen-cli $WB_WANT (have: ${WB_HAVE:-none})..."
  cargo install -f wasm-bindgen-cli --version "$WB_WANT" --locked
else
  echo "wasm-bindgen-cli $WB_WANT already installed."
fi

# --- tilt: the repo's canonical dev-loop orchestrator (CLAUDE.md assumes it is
# always running). Install a pinned release to /usr/local/bin if missing.
TILT_VERSION="0.37.7"
if ! command -v tilt >/dev/null 2>&1; then
  echo "Installing tilt v$TILT_VERSION..."
  tmp="$(mktemp -d)"
  curl -fsSL "https://github.com/tilt-dev/tilt/releases/download/v${TILT_VERSION}/tilt.${TILT_VERSION}.linux.x86_64.tar.gz" \
    | tar -xz -C "$tmp" tilt
  sudo install -m 0755 "$tmp/tilt" /usr/local/bin/tilt
  rm -rf "$tmp"
else
  echo "tilt already installed: $(tilt version 2>/dev/null || echo present)."
fi

# --- Repo onboarding: fetch card data + Scryfall image sidecars, build the WASM
# bundles, and install client + lobby-worker dependencies. --no-tilt forces the
# eager WASM + card-data build inline (even though tilt is installed) so the
# snapshot ships with those artifacts prebuilt and the app is runnable on boot.
# The Scryfall/MTGJSON fetchers skip work whose output already exists, so a
# re-run is fast.
echo "Running ./scripts/setup.sh --no-tilt from $REPO_ROOT ..."
./scripts/setup.sh --no-tilt

echo "phase.rs environment bootstrap complete."
