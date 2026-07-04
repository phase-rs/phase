#!/usr/bin/env bash
# Run the ai-perf-gate decision-cost regression gate without queueing behind
# Tilt's continuous builds.
#
# Same rationale as scripts/ai-gate.sh: an isolated CARGO_TARGET_DIR gives the
# perf gate its own build lock and fingerprint namespace so it never blocks on
# (or thrashes against) Tilt's shared target/debug builds.
#
# NOTE: this wrapper builds --release; its WALL-CLOCK is NOT comparable to CI,
# which runs the DEBUG profile via `cargo ai-perf-gate`. Counter VERDICTS are
# profile-independent (logical event counts), so the wrapper's PASS/FAIL is
# correct locally — only its timing must not be transferred to the CI budget.
#
# Usage: scripts/ai-perf-gate.sh [ai-perf-gate args...]
#   scripts/ai-perf-gate.sh                    # compare against the saved baseline
#   scripts/ai-perf-gate.sh --refresh-baseline # overwrite the baseline
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_TARGET_DIR="$ROOT/target/ai"

cargo build --release --bin ai-perf-gate
exec "$CARGO_TARGET_DIR/release/ai-perf-gate" "$@"
