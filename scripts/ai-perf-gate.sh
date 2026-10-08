#!/usr/bin/env bash
# Run the ai-perf-gate decision-cost regression gate without queueing behind
# Tilt's continuous builds.
#
# Same rationale as scripts/ai-gate.sh: an isolated CARGO_TARGET_DIR gives the
# perf gate its own build lock and fingerprint namespace so it never blocks on
# (or thrashes against) Tilt's shared target/debug builds.
#
# This wrapper and CI (`cargo ai-perf-gate`) now build the SAME profile,
# server-release, so wall-clock here is comparable to CI's. Counter VERDICTS were
# already profile-independent (logical event counts); unifying the profile makes
# the TIMING transferable too.
#
# Previously this built `--release` while CI built dev — two different profiles,
# neither of them the native speed one, and `--release` in this workspace is the
# WASM-size profile (opt-level 'z', panic = 'abort').
#
# Usage: scripts/ai-perf-gate.sh [ai-perf-gate args...]
#   scripts/ai-perf-gate.sh                    # compare against the saved baseline
#   scripts/ai-perf-gate.sh --refresh-baseline # overwrite the baseline
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_TARGET_DIR="$ROOT/target/ai"

cargo build --profile server-release --bin ai-perf-gate
exec "$CARGO_TARGET_DIR/server-release/ai-perf-gate" "$@"
