#!/usr/bin/env bash
# Strict pre-baseline reproducibility + CI-budget validation (M15).
#
# Generates a fresh median-of-K perf baseline, runs N further median-of-K gate
# runs against it, then applies the MARGIN gate: every counter's worst observed
# value must stay within PERF_REPRO_MARGIN_FRACTION of its FAIL headroom. It also
# TIMES the cold build and every gate run so the executor can apply the CI-budget
# check with MEASURED numbers rather than asserted estimates.
#
# Runs the SERVER-RELEASE binary — the authoritative gate profile CI runs
# (`cargo ai-perf-gate`). This script isolates CARGO_TARGET_DIR to target/ai, so
# the parent's current_exe() resolves to target/ai/server-release/ai-perf-gate
# and the K spawned children are server-release too (profile-consistent parent
# and children).
#
# The profile here is load-bearing and must track the `cargo ai-perf-gate` alias:
# this script hardcodes a target/<profile>/ path while the binary re-spawns ITSELF
# by current_exe(). If the alias and this path ever name different profiles, the
# script silently measures the wrong binary (or a stale one) instead of failing.
#
# ONLY commit the generated baseline if this script PASSES (margin + all N band
# runs exit 0) AND the executor's CI-budget arithmetic passes (see the echoed
# rule at the end). Otherwise escalate per the plan — never widen the band here.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_TARGET_DIR="$ROOT/target/ai"     # isolated: no Tilt lock contention (mirrors ai-perf-gate.sh)

# SERVER-RELEASE profile — the authoritative gate profile CI runs (`cargo ai-perf-gate`).
# Time the cold isolated build: cold-isolated >= CI's warm rust-ai-gate cache
# hit, so this is a conservative T_build ceiling for the budget check.
build_start=$(date +%s)
cargo build --profile server-release --bin ai-perf-gate
echo "T_build (cold isolated server-release build) = $(( $(date +%s) - build_start ))s"
BIN="$CARGO_TARGET_DIR/server-release/ai-perf-gate"   # current_exe() -> server-release children

"$BIN" --refresh-baseline                     # 1) generate the median-of-K baseline

N=25                                          # keep in sync with PERF_REPRO_VALIDATION_RUNS
inputs=(); band_fail=0
for i in $(seq 1 "$N"); do                    # 2) N further median-of-K gate runs vs the baseline
  out="$ROOT/target/ai-perf-repro-$i.json"
  start=$(date +%s)
  if ! "$BIN" --current-output "$out"; then band_fail=1; fi   # existing band gate (weak Bernoulli check)
  echo "run $i wall=$(( $(date +%s) - start ))s"              # T_run sample (= PERF_SAMPLE_COUNT children)
  inputs+=(--repro-input "$out")
done

# 3) MARGIN GATE — exit 0 iff all counters within 50% headroom. Capture the code
# without letting `set -e` abort before the summary (the margin table itself is
# printed by the binary regardless).
if "$BIN" --repro-report "${inputs[@]}"; then margin_rc=0; else margin_rc=$?; fi

if [ "$band_fail" -ne 0 ] || [ "$margin_rc" -ne 0 ]; then
  echo "REPRO VALIDATION FAILED (band_fail=$band_fail margin_rc=$margin_rc) — DO NOT COMMIT baseline; escalate."
  exit 1
fi
echo "REPRO VALIDATION PASSED (margin+band) — now apply the CI-budget check before committing:"
echo "  T_run_max = max over 'run i wall' above; W_run = T_run_max / PERF_SAMPLE_COUNT(5)."
echo "  Commit iff  T_run_max*2.5 + T_build < 25min."
echo "  (The former 'fall back to --release' option is gone: all four gate"
echo "   authorities now build server-release, and --release is the WASM-size profile.)"
