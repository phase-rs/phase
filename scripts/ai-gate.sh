#!/usr/bin/env bash
# Run the ai-gate regression gate without queueing behind Tilt's continuous builds.
#
# Same rationale as scripts/ai-duel.sh: an isolated CARGO_TARGET_DIR gives ai-gate
# its own build lock and fingerprint namespace so it never blocks on (or thrashes
# against) Tilt's shared target/debug builds.
#
# Usage: scripts/ai-gate.sh [ai-gate args...]
#   scripts/ai-gate.sh                 # compare against the saved baseline
#   scripts/ai-gate.sh --full-suite    # broader matchup coverage
#   scripts/ai-gate.sh --refresh-baseline   # overwrite the baseline
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_TARGET_DIR="$ROOT/target/ai"

cargo build --release --bin ai-gate
exec "$CARGO_TARGET_DIR/release/ai-gate" "$@"
