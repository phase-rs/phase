# Pipeline 4 — Round-4 AMENDMENT plan: make the `cargo ai-perf-gate` counter gate shippable under cross-process nondeterminism

**Status of the base:** the round-3 plan is fully implemented and CLEAN in worktree
`/Users/matt/dev/forge.rs/.claude/worktrees/agent-ab012b2369009baa4` (`duel_suite/perf.rs`,
`bin/ai_perf_gate.rs`, `drive_game` extraction in `run.rs`, `scripts/{ai-perf-gate.sh,refresh-ai-perf-baseline.sh}`,
two `ai-gate.yml` jobs, 11 unit tests green, clippy clean). The baseline JSON was deliberately **not** generated
because the determinism gate failed. This amendment is surgical: it touches only the compare/aggregation layer,
the binary's run orchestration, docs, one reframed test, and the baseline-generation procedure. **No engine files,
no `planner/mod.rs`, no `policies/**`.**

---

## 0. The blocker, restated precisely (this is what the design must survive)

The counter payload is **not** a deterministic function of `(binary, card-data, seed, action_cap)`:

- **Cross-process (the CI-relevant axis):** baseline JSON is generated on one process, current JSON on another.
  7/19 counters diverge on essentially *every* cold-process pair, and `state_clone_for_legality` (4622 vs 4564)
  diverges — meaning the game **trajectory itself** differs across processes. Root cause: engine-wide
  `std::collections::HashSet`/`HashMap` default `RandomState` (per-process seed) leaks iteration order into AI
  action tie-breaking. Tracked as **issue #4878**; the engine fix is out of scope for this pipeline.
- **In-process:** two runs in the same process are trajectory-identical (18/19 byte-equal incl.
  `state_clone_for_legality=601`); only `layers_full_eval` jitters ±2 (HashSet-order layer-flush coalescing).

**Two consequences that kill the naive options:**

1. Comparing one baseline-process to one current-process is comparing **two different random trajectories** — flaky
   by construction, and one calibration sample cannot bound the worst-case trajectory spread.
2. Looping the suite K times **in one process** gives correlated (identical-trajectory) samples → **zero** variance
   reduction. Independent samples require **independent processes** (fresh `RandomState` per child).

---

## 1. Decision: A + C + D synthesis — median-of-K over independent cold processes, tight band, #4878 tightening trajectory

I evaluated all four options against the two hard requirements (nightly must not be flaky; blocking PR job's false
positives are expensive) and the sensitivity requirement (must still catch the gate's target class).

| Option | Verdict | Why |
|---|---|---|
| **(A) Statistical workload (median-of-K)** | **ADOPT (core)** | Directly attacks the actual failure mode: every process is a different trajectory, so the only sound comparison is between *central-tendency estimators*, not between two single trajectories. Median over K **independent processes** suppresses minority-outlier trajectories and shrinks the compared statistic's run-to-run spread to ~0.3–0.6% on big counters, letting the band stay tight. Cost K×wall-clock (~2 min at K=5) is cheap. **Independence requires spawning K child processes** — in-process looping does nothing (fact #2 above). |
| **(B) Typed per-counter stability classification** | **REJECT as primary** | Trajectory divergence is a *whole-game* effect, so it can perturb any counter; the "stable" set from a finite calibration is untrustworthy, and the counters that *do* diverge (`state_clone_for_legality`, `static_full_scans`, mana sweeps) are **exactly** the ones the gate exists to watch. Demoting them to informational guts the gate. Its honest core — drift is heterogeneous per counter — is folded in as an *informational* per-counter drift table in the calibration report, gating nothing. |
| **(C) Empirically-derived multiplicative band** | **ADOPT (retain existing structure)** | Keep the implemented `current > baseline*RATIO + FLOOR` byte-compare unchanged. The band now absorbs the *residual* median-of-K sampling error, not raw single-trajectory noise, so it can stay at the implemented `1.05` ratio + `64` floor. The calibration step (below) is the authority that confirms/sizes it. |
| **(D) Interim ship + #4878 dependency** | **ADOPT (trajectory)** | Ship the median-of-K gate now; when #4878 makes the engine cross-process deterministic, collapse `PERF_SAMPLE_COUNT` to `1` and tighten the band to byte-exact (or keep 5%+64 for pure codegen jitter). One-const change + baseline regen; a linking comment references #4878. |

**Why not just widen the band (C alone)?** The teammate's own framing is correct: one two-run sample does not bound
the tail, and because *every* cold-process pair is a different trajectory, a K=1 gate compares
different-apples-to-different-apples on every run. To not false-positive you would need a band wide enough to cover
the full trajectory-outcome spread — which one sample cannot measure, and which would blind the gate to real
sub-band regressions. Median-of-K measures and suppresses that spread structurally, so C-alone is strictly
dominated. Median-of-K is not gold-plating here; it is the minimum honest mechanism for a per-process-random engine.

### Sensitivity argument, with numbers (is the gate still worth shipping?)
- Observed single-run cross-process drift: big counters 0.5–1.3% (`static_full_scans` 37613/37294 = 0.85%,
  `mana_aura_trigger_scans` 7447/7363 = 1.13%); small counters up to ~15% (`legal_actions_spell_cost_sweeps`
  269/233) but **absorbed by the floor** (233·1.05+64 = 308 ≥ 269).
- With K=5 median over independent trajectories, the compared statistic's run-to-run spread drops to ~0.3–0.6% on
  the big counters (median is an outlier-robust estimator; the observed distribution is tightly clustered near a
  dominant trajectory, not 50/50 bimodal — confirmed by the ~1% spread).
- Band `1.05` ⇒ a real regression must exceed ~5% on a big counter to trip. The gate's target class — clone storms,
  quadratic combat scans, display sweeps in search — produces **≥1.5× (50%+)** counter blow-ups, caught with ~10×
  margin. Sub-5% micro-regressions are explicitly out of scope (documented in the module doc). False-positive rate:
  with statistic spread ~0.5% ≪ 5% band, ≈0 over normal PR cadence — **empirically validated** by the mandatory
  N-run reproducibility gate before the baseline is committed.

**Net:** not flaky (median suppresses outliers + band ≫ statistic spread + validated) and not insensitive to the
target class (5% ≪ typical structural regression). Shippable.

---

## 2. Analogous trace (base plan's compare pipeline is the thing being amended)

Traced the **win-rate gate** end-to-end and the **implemented perf gate** it mirrors:
`bin/ai_gate.rs` (arg parse → provenance via `command_output` → `compare::compare` → `print_markdown` → exit code)
→ `duel_suite/compare.rs` (`CompareError`, `load_report`, schema guard) — mirrored by the implemented
`bin/ai_perf_gate.rs` → `duel_suite/perf.rs` (`PerfReport`, `PerfCounters::from_snapshot`, `compare`,
`print_markdown`, matrix tests). The amendment extends exactly this seam: a new pure `median_report` aggregator in
`perf.rs`, a subprocess sampling loop in `bin/ai_perf_gate.rs`, and one extended workload-guard clause in `compare`.

---

## 3. Concrete changes

### 3.1 `crates/phase-ai/src/duel_suite/perf.rs`

**(a) New sample-count const (workload-pinned, consts-not-flags per base plan §GAP B):**
```rust
/// Number of INDEPENDENT cold-process trajectory samples the gate aggregates by
/// per-counter median. Independence is why each sample must be its own process
/// (fresh std RandomState) — see the binary's sampling loop. Odd so the median is
/// a single observed value. K=5 keeps the whole gate ~2 min (well under the 30-min
/// CI timeout) while suppressing minority-outlier trajectories.
///
/// #4878: when the engine's HashSet/HashMap iteration order stops leaking per-process
/// RandomState into AI tie-breaking, every trajectory becomes cross-process identical;
/// set this to 1 and tighten PERF_TOLERANCE_RATIO to byte-exact, then regenerate the baseline.
pub const PERF_SAMPLE_COUNT: usize = 5;
```

**(b) Bump schema (PerfReport gains a field → shape change):**
```rust
pub const PERF_SCHEMA_VERSION: u32 = 2; // was 1: added `sample_count`
```

**(c) Add `sample_count` to `PerfReport`** (required field; no legacy JSON exists since the baseline was never
committed). Update `mk_report` in tests and the binary's construction sites to set it to `PERF_SAMPLE_COUNT`.
`run_perf_suite` continues to return a *single-trajectory* report; set its `sample_count: 1` (it is one sample —
the parent aggregates K of these). The median report carries `sample_count: K`.

**(d) New pure aggregator — the only new "compare semantics":**
```rust
/// Element-wise per-counter median over K independent single-trajectory sample
/// reports. Median (not mean) is outlier-robust: a minority anomalous trajectory
/// cannot move the aggregate. The result's counters need not equal any single
/// real trajectory — this gate compares aggregate COST LEVELS, not a replayed game.
///
/// Panics (internal invariant, not a runtime input path) if samples is empty or the
/// samples disagree on schema_version / base_seed / action_cap — every sample is
/// produced by the same binary at the same const workload, so disagreement is a bug.
/// Provenance (git_sha, card_data_hash) is left None for the caller to stamp.
pub fn median_report(samples: &[PerfReport]) -> PerfReport {
    assert!(!samples.is_empty(), "median_report requires at least one sample");
    let first = &samples[0];
    for s in &samples[1..] {
        assert_eq!(s.schema_version, first.schema_version, "sample schema mismatch");
        assert_eq!(s.base_seed, first.base_seed, "sample seed mismatch");
        assert_eq!(s.action_cap, first.action_cap, "sample action_cap mismatch");
    }
    // All samples share an identical key set (from_snapshot is a total struct destructure).
    let mut counters = BTreeMap::new();
    for key in first.counters.0.keys() {
        let mut vals: Vec<u64> = samples
            .iter()
            .map(|s| *s.counters.0.get(key).expect("sample missing a counter key"))
            .collect();
        vals.sort_unstable();
        counters.insert(key.clone(), vals[vals.len() / 2]); // upper-middle: always a real observed value, deterministic for any K
    }
    let mut wall: Vec<u128> = samples.iter().map(|s| s.wall_clock_ms).collect();
    wall.sort_unstable();
    PerfReport {
        schema_version: first.schema_version,
        git_sha: None,
        card_data_hash: None,
        base_seed: first.base_seed,
        action_cap: first.action_cap,
        sample_count: samples.len(),
        scenarios: first.scenarios.clone(),
        counters: PerfCounters(counters),
        wall_clock_ms: wall[wall.len() / 2], // never compared
    }
}
```
Notes: `vals.len()/2` is the upper-middle for even K and the exact middle for odd K — deterministic, always an
observed value, no fractional counters. K is pinned odd (=5) via the const; even-K totality is still tested.

**(e) Extend the workload guard in `compare()`** so a baseline and current produced with different K are rejected
(K is part of the estimator contract — a K=1 baseline vs K=5 current is unsound):
```rust
if baseline.sample_count != current.sample_count {
    return Err(PerfCompareError::WorkloadMismatch {
        field: "sample_count",
        baseline: baseline.sample_count.to_string(),
        current: current.sample_count.to_string(),
    });
}
```
Place it alongside the existing `base_seed`/`action_cap` clauses. No new error variant — reuse `WorkloadMismatch`.

**(f) Module doc "baseline honesty" rewrite** (`//!` header) — replace the current "deterministic function of
`(binary, card-data, seed, action_cap)`" claim with the real guarantee:
> The gate compares the **per-counter median over K independent cold-process trajectories** for a fixed
> `(binary, card-data, seed, action_cap, K)`. Individual trajectories are **not** cross-process deterministic —
> engine `HashSet`/`HashMap` iteration order leaks per-process `RandomState` into AI tie-breaking (issue #4878), so
> every process follows a slightly different game. Median-of-K plus the multiplicative band absorb that residual
> trajectory variance; the band is validated empirically (all N reproducibility runs PASS) before the baseline is
> committed. Within a single process, `drive_game`'s `(winner, turn)` result is deterministic; `layers_full_eval`
> is the one counter that jitters even in-process (HashSet-order layer-flush batching, ±~0.3%, absorbed by the
> floor). When #4878 lands, K→1 and the band tightens to byte-exact.

### 3.2 `crates/phase-ai/src/bin/ai_perf_gate.rs` — subprocess sampling orchestration

Add a hidden internal flag `--emit-sample <PATH>`. Control flow:

1. **Parse args**, plus `emit_sample: Option<PathBuf>`.
2. **Child branch** (`--emit-sample PATH` present): load DB, `run_perf_suite(&db, PERF_BASE_SEED, PERF_ACTION_CAP,
   &default_scenarios())` (returns a `sample_count: 1` report), `write_report(&report, &PATH)`, `exit(0)`. The child
   does **not** stamp provenance and does **not** load/compare a baseline.
3. **Parent branch** (default): does **not** load the DB. Spawn `PERF_SAMPLE_COUNT` children via
   `std::env::current_exe()` with `["--emit-sample", tmp_i, "--data-root", <root>]`, each writing to
   `std::env::temp_dir().join(format!("ai-perf-sample-{pid}-{i}.json"))` (`std::process::id()` for the pid). Wait on
   each child.
   - **No silent truncation** (base plan's "no silent caps" principle): if any child exits non-zero, or its report
     is missing/unparseable, print the failure and `exit(2)`. The gate must aggregate exactly K valid samples or
     fail loudly — a degraded K silently weakens the statistic.
   - `load_report` each of the K JSONs → `median_report(&samples)` → stamp `git_sha` = `git rev-parse --short=12`
     and `card_data_hash` = `git hash-object <db_path>` (parent computes both without loading the DB) → this is
     `current`.
   - Then the existing tail is unchanged: `eprintln!` the provenance line (now also printing `sample_count`),
     `write_report(&current, current_output)`, refresh-or-compare-and-exit exactly as today.
   - Clean up temp sample files (best-effort `remove_file`; ignore errors).

Reuse the existing `command_output`, `write_report`, `load_report`, `path_str` helpers unchanged. `print_usage`
gains a line noting the gate runs `PERF_SAMPLE_COUNT` independent sample processes.

### 3.3 Scripts

- `scripts/ai-perf-gate.sh` / `refresh-ai-perf-baseline.sh`: **no logic change** — the K-sampling lives in the
  binary, so `cargo ai-perf-gate` and `--refresh-baseline` still do everything. Update the header comment in
  `refresh-ai-perf-baseline.sh` to state the guarantee is now "median-of-K over K independent cold-process
  trajectories" rather than single-run byte reproducibility.
- **New** `scripts/validate-ai-perf-reproducibility.sh` (the strict baseline-sequencing gate — see §5, M15):
  builds the release binary once (isolated `CARGO_TARGET_DIR=$ROOT/target/ai`, mirroring `ai-perf-gate.sh`),
  `--refresh-baseline` to generate a fresh median-of-K baseline, then runs the gate `N=25` more times (each a
  fresh median-of-K) and asserts **every** run exits 0. Prints a per-counter observed-drift table (min/median/max
  across the N validation runs — the informational disclosure that folds in option B's honest core) and the
  overall PASS/FAIL. This script is what the executor runs, and pastes output into, before committing the baseline.

### 3.4 `.github/workflows/ai-gate.yml`

No structural change — both jobs already invoke `cargo ai-perf-gate`, which now self-samples. Two small edits:
- Bump the `timeout-minutes` note is unnecessary (30 min already covers ~2 min × safety). Leave at 30.
- Add a one-line comment above each `Run decision-cost perf gate` step: `# runs PERF_SAMPLE_COUNT independent
  sample processes and compares the per-counter median (issue #4878: engine not yet cross-process deterministic)`.

---

## 4. Mandatory architectural sections

- **Pattern Coverage.** Covers the *class* of cross-process-nondeterministic integer-counter regression gates. Any
  counter later added to `PerfCounterSnapshot` flows through `from_snapshot` (total struct destructure) → `merge_add`
  → `median_report` → `compare` automatically; nothing is per-counter special-cased. Not one counter.
- **Building Blocks.** Reuse the implemented `PerfReport`, `PerfCounters`, `compare`, `load_report`,
  `write_report`, `print_markdown`, `command_output`, `PerfCompareError::WorkloadMismatch`. One new pure fn
  `median_report` (justified: outlier-robust aggregation is the mechanism the blocker requires; no existing helper
  medians reports). Subprocess loop composes `std::process::Command` + `std::env::current_exe`/`temp_dir` — the same
  `Command` primitive already used for `git`. No new external dependency.
- **Logic Placement.** Aggregation (`median_report`) and the estimator-contract guard (`sample_count`) live in the
  **engine-adjacent `phase-ai` crate** (`perf.rs`), not in shell — median-of-reports is logic. The binary owns only
  process orchestration (spawning + IO), which is inherently a binary concern. Scripts own only build-isolation and
  the run-N-times validation loop.
- **Rust Idioms.** `sample_count` reuses the existing `WorkloadMismatch { field, .. }` shape (no new variant, no
  bool). Median via `sort_unstable` + midpoint index (no float, no fractional counter). Internal invariants are
  `assert!`/`assert_eq!` (programmer error, not runtime input). Exhaustive matches unchanged.
- **Extension vs Creation.** Pure extension of the implemented compare pipeline — one new pure fn, one new guard
  clause, one new field, subprocess loop in the binary. No new pattern, no new type beyond the const.
- **Nom Compliance.** N/A — no file under `crates/engine/src/parser/` changes; this is CLI/aggregation tooling.
- **Variant Discoverability.** N/A — no new engine enum variant. `CounterVerdict`/`PerfCompareError` unchanged.
- **Identity / Provenance Contract.** The compared **authority** is the committed **median-of-K** report
  (`perf-baseline.json`). Source concept: central tendency of the counter payload over K independent cold-process
  trajectories. Selected authority: `sample_count = PERF_SAMPLE_COUNT` at binding time (baseline refresh /
  calibration). Live vs latched: latched into the committed JSON; `compare` reads it verbatim. Storage:
  `crates/phase-ai/baselines/perf-baseline.json`. Consumer: `compare()`. Invalidation: `schema_version`,
  `base_seed`, `action_cap`, **or `sample_count`** mismatch → `WorkloadMismatch`/`SchemaMismatch` (exit 2);
  card-data regen → non-gating hash-delta diagnostic. **Multi-authority hostile fixture:** a K=5 baseline vs a K=1
  current must be rejected by the extended workload guard (proves the K binding is enforced, not assumed) — M14.

---

## 5. Verification matrix (revert-failing; existing M1–M8, M10 retained unchanged — `compare` core is untouched)

| # | Claim | Seam / entry point | Test (revert-failing assertion) | Hostile / negative sibling |
|---|---|---|---|---|
| **M9 (reframed)** | The within-process guarantee is **trajectory identity**, not counter byte-identity. | `run::drive_game` (in-process) | `#[ignore]` DB-gated: call `drive_game(&payload, PERF_BASE_SEED, Medium, PERF_ACTION_CAP)` **twice in one process**; assert the two `(winner, turn)` tuples are **equal**. Revert-failing: if the trajectory were in-process-nondeterministic this fails. **Replaces** the old byte-identical-snapshot test, which is now known-false (`layers_full_eval` jitters 695/693 even in-process). | Negative reach-guard: the paired positive is the equal `(winner,turn)`; do **not** assert counter equality (would vacuously fail on `layers_full_eval`). Comment cites #4878. |
| **M11** | `median_report` is element-wise per-counter **median**, not mean/min/max. | `perf::median_report` | Three samples with counter `c` = `[10, 1000, 20]` → median `20`. Assert `== 20` and `!= 343` (mean), `!= 10` (min), `!= 1000` (max). | Sibling counter with a different distribution in the same sample set medians independently. |
| **M12** | K=1 median is the identity. | `perf::median_report` | Single-sample slice → output counters/workload equal the input; `sample_count` in output = 1. | — |
| **M13** | Median inherits/pins workload; disagreeing samples are a hard internal error. | `perf::median_report` | Output `base_seed`/`action_cap`/`schema_version`/`scenarios` inherited from samples; `sample_count == samples.len()`. `#[should_panic]` test feeding samples with differing `base_seed`. | Proves the invariant is enforced, not assumed. Reachable only via programmer error (children always use the const workload) — documented. |
| **M14** | Extended workload guard rejects K mismatch (K binding is enforced). | `perf::compare` | Baseline `sample_count=5`, current `sample_count=1`, all else equal → `Err(WorkloadMismatch { field: "sample_count", .. })`; **not** a silent PASS. Revert-failing on the new guard clause. | Multi-authority hostile from §4. |
| **M-even** | Median totality for even K. | `perf::median_report` | 4 samples `[1,2,3,4]` → deterministic upper-middle `3` (index `len/2`); no panic, no fractional value. | Guards the `vals.len()/2` index against off-by-one on even K even though K is pinned odd. |
| **M15 (empirical — the strict baseline-sequencing gate; run-once by executor, output pasted into PR)** | The median-of-K + `1.05`+`64` band is cross-process reproducible in practice. | `scripts/validate-ai-perf-reproducibility.sh` | Generate a fresh median-of-K baseline, then run `cargo ai-perf-gate` **N=25** more times (each its own median-of-K over fresh child processes); assert **all 25 exit 0** (`any_fail()==false`). If any run FAILs, the band/K is insufficient → **do not commit the baseline**; escalate (raise `PERF_SAMPLE_COUNT`, or size the band up from the printed max-drift table with a named-const + rationale). Prints the per-counter observed-drift table. | This is the empirical bound on the tail that a single sample could not provide. |

Coverage-status impact: N/A (tooling; no card-coverage surface). No Oracle text accepted-but-deferred (no parser change).

---

## 6. Implementation steps (surgical order)

1. `perf.rs`: add `PERF_SAMPLE_COUNT` const; bump `PERF_SCHEMA_VERSION` to `2`; add `sample_count` field to
   `PerfReport` + set it (`1`) in `run_perf_suite`'s returned report and in the test `mk_report` helper; add
   `median_report`; add the `sample_count` clause to `compare`'s workload guard; rewrite the module `//!` honesty doc.
2. `perf.rs` tests: reframe M9; add M11–M14 + M-even. Keep M1–M8, M10.
3. `bin/ai_perf_gate.rs`: add `--emit-sample` flag + child branch; parent subprocess-sampling loop (spawn K, wait,
   fail-loud on any bad sample, `median_report`, stamp provenance, temp cleanup); update the provenance `eprintln!`
   and `print_usage`.
4. `scripts/refresh-ai-perf-baseline.sh`: header comment → median-of-K guarantee.
5. **New** `scripts/validate-ai-perf-reproducibility.sh` (M15).
6. `ai-gate.yml`: one comment line above each perf-gate step.
7. **Baseline sequencing (strict):** run `cargo fmt --all`; verify `phase-ai` compiles + unit tests green (Tilt
   `test-ai`/`clippy` if up, else targeted `cargo test -p phase-ai` / `cargo clippy -p phase-ai --all-targets -D
   warnings` with worktree-local `CARGO_TARGET_DIR`); run M9 `--ignored` locally; then run
   `scripts/validate-ai-perf-reproducibility.sh` (M15). **Only if all 25 validation runs PASS**, commit the
   generated median-of-K `perf-baseline.json` (paste the M15 drift table + PASS summary into the PR). If M15 fails,
   escalate per M15 before committing anything.

## 7. Verification cadence
`cargo fmt --all` direct. `phase-ai` clippy + unit tests via Tilt `test-ai`/`clippy` when up, else targeted cargo
with an isolated `CARGO_TARGET_DIR` (worktrees aren't Tilt-watched). M9 `--ignored` + M15 run once locally before
baseline commit. No TypeScript touched. No engine files touched.
