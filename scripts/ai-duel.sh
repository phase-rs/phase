#!/usr/bin/env bash
# Run the ai-duel binary without queueing behind Tilt's continuous builds.
#
# Tilt's test/server resources compile into the shared target/debug, holding the
# cargo build lock and mutually invalidating fingerprints. Building ai-duel into
# its own CARGO_TARGET_DIR (mirroring the clippy/wasm resources) gives it a
# private lock and fingerprint namespace, so a build here never blocks or thrashes
# against Tilt. Release is the default — game-tree simulation runs far faster than
# the one-time compile costs.
#
# Usage: scripts/ai-duel.sh [ai-duel args...]
#   scripts/ai-duel.sh client/public --batch 20 --seed 42
#   scripts/ai-duel.sh client/public --suite --games 10 --seed 42
#
# Tip: the build step is a fast no-op when nothing changed. To skip even the
# up-to-date check, run the prebuilt binary directly: target/ai/release/ai-duel
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_TARGET_DIR="$ROOT/target/ai"

cargo build --release --bin ai-duel
exec "$CARGO_TARGET_DIR/release/ai-duel" "$@"
