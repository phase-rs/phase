#!/usr/bin/env bash
set -euo pipefail

# Refresh the committed perf-gate baseline. NEVER a blind widen: the underlying
# gate prints the baseline-vs-current counter diff BEFORE overwriting, so the
# operator can paste it into the PR that intends the increase. Only refresh when
# a counter change is understood and intended (a real cost increase, or a
# card-data regen that legitimately shifted the decision trajectory — indicated
# by a card-data hash delta on the diff).
#
# The guarantee this baseline encodes: the per-counter MEDIAN over K independent
# cold-process trajectories for a fixed (binary, card-data, seed, action_cap, K),
# margin-validated before commit (scripts/validate-ai-perf-reproducibility.sh) —
# NOT single-run byte reproducibility, and NOT invariance across card-data
# regenerations. Individual trajectories diverge cross-process (issue #4878); the
# median + multiplicative band absorb that residual variance.
#
# Delegate to the isolated-target wrapper so the baseline rebuild never queues
# behind Tilt's shared target/debug builds (see scripts/ai-gate.sh).
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec "$ROOT/scripts/ai-perf-gate.sh" --refresh-baseline "$@"
