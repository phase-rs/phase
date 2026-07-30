#!/usr/bin/env bash
# Strict pre-baseline reproducibility + CI-budget validation (M15).
#
# Generates a fresh median-of-K perf baseline, runs N further median-of-K gate
# runs against it, then applies the MARGIN gate: every counter's worst observed
# value must stay within PERF_REPRO_MARGIN_FRACTION of its FAIL headroom. It also
# TIMES the cold build and every gate run so the executor can apply the CI-budget
# check with MEASURED numbers rather than asserted estimates.
#
# Runs the DEBUG binary — the authoritative gate profile CI runs
# (`cargo ai-perf-gate`). Under debug, the parent's current_exe() resolves to
# target/debug/ai-perf-gate, so the K spawned children are debug too
# (profile-consistent parent and children).
#
# ONLY commit the generated baseline if this script PASSES (margin + all N band
# runs exit 0) AND the executor's CI-budget arithmetic passes (see the echoed
# rule at the end). Otherwise escalate per the plan — never widen the band here.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_TARGET_DIR="$ROOT/target/ai"     # isolated: no Tilt lock contention (mirrors ai-perf-gate.sh)

# DEBUG profile — the authoritative gate profile CI runs (`cargo ai-perf-gate`).
# Time the cold isolated build: cold-isolated >= CI's warm rust-ai-gate cache
# hit, so this is a conservative T_build ceiling for the budget check.
build_start=$(date +%s)
cargo build --bin ai-perf-gate                # BUILD ONCE (debug, default profile)
echo "T_build (cold isolated debug build) = $(( $(date +%s) - build_start ))s"
BIN="$CARGO_TARGET_DIR/debug/ai-perf-gate"    # current_exe() -> debug children (profile-consistent)

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
echo "  T_run_max = max over 'run i wall' above; W_debug = T_run_max / PERF_SAMPLE_COUNT(5)."
echo "  Option (c) commit iff  T_run_max*2.5 + T_build < 25min  (see plan 3.4)."
echo "  Else fall back to option (b): release + cache-shared-key rust-ai-perf-release; re-measure."
