# Pipeline 4 — Round-5 AMENDMENT plan (r2): make the `cargo ai-perf-gate` counter gate shippable under cross-process nondeterminism

**Status of the base:** the round-3 plan is fully implemented and CLEAN in worktree
`/Users/matt/dev/forge.rs/.claude/worktrees/agent-ab012b2369009baa4`
(`duel_suite/perf.rs` = 734 lines, `bin/ai_perf_gate.rs` = 177 lines, `drive_game` extraction in
`duel_suite/run.rs`, `scripts/{ai-perf-gate.sh,refresh-ai-perf-baseline.sh}`, two `ai-gate.yml` jobs, 11 unit
tests green, clippy clean). The baseline JSON (`crates/phase-ai/baselines/perf-baseline.json`) was deliberately
**not** generated because the determinism gate failed.

This amendment is surgical: it touches only the compare/aggregation layer (`perf.rs`), the binary's run
orchestration (`bin/ai_perf_gate.rs`), one reframed and one new+strengthened test, the two `ai-gate.yml` perf-gate
`run:` lines, the two perf scripts, and one new validation script. **No engine files, no `planner/mod.rs`, no
`policies/**`.**

This r2 revision is a full re-issue (not a delta) of the round-4 amendment, incorporating the five reviewer gaps:
GAP 1 (M15 quantitative margin criterion + named consts), GAP 2 (M9 asserts the real 18/19 in-process invariant),
GAP 3 (CI runs the release profile so M15 timing transfers), GAP 4 (child-process stdout hygiene), GAP 5
(`sample_count` threading enumeration + §5 wording fix). The reviewer-verified-SOUND core is preserved verbatim:
median-of-K over independent cold child processes via `current_exe()`; `WorkloadMismatch` reuse; schema `1→2`
no-migration; the M11–M14 + M-even matrix; and the identity/provenance contract.

---

## 0. The blocker, restated precisely (this is what the design must survive)

The counter payload is **not** a deterministic function of `(binary, card-data, seed, action_cap)`:

- **Cross-process (the CI-relevant axis):** baseline JSON is generated on one process, current JSON on another.
  Cold-process pairs diverge in **trajectory**: 7/19 counters differ, and `state_clone_for_legality` moves
  4622↔4564 — a whole-game divergence. Root cause: engine-wide `std::collections::HashSet`/`HashMap` default
  `RandomState` (per-process seed) leaks iteration order into AI action tie-breaking. Tracked as **issue #4878**;
  the engine fix is out of scope for this pipeline.
- **In-process:** two runs in the same process are trajectory-identical. Measured invariant: **18 of 19 counters
  byte-equal** (incl. `state_clone_for_legality`); only `layers_full_eval` jitters ±2 (695↔693), from
  HashSet-order layer-flush coalescing within the batch. `(winner, turn)` is in-process deterministic.

**Two consequences that kill the naive options:**

1. Comparing one baseline-process to one current-process compares **two different random trajectories** — flaky by
   construction; one calibration sample cannot bound the worst-case trajectory spread.
2. Looping the suite K times **in one process** gives correlated (identical-trajectory) samples → **zero** variance
   reduction. Independent samples require **independent processes** (fresh `RandomState` per child).

---

## 1. Decision: median-of-K over independent cold processes, tight band, empirical margin gate, #4878 tightening trajectory

Synthesis of options A (statistical workload) + C (multiplicative band) + D (interim ship w/ #4878 dependency),
with option B's honest core (per-counter drift is heterogeneous) folded in as informational disclosure.

| Option | Verdict | Why |
|---|---|---|
| **(A) median-of-K over independent processes** | **ADOPT (core)** | Every process is a different trajectory, so the only sound comparison is between *central-tendency estimators*, not two single trajectories. Median over K **independent processes** suppresses minority-outlier trajectories and shrinks the compared statistic's spread to ~0.3–0.6% on big counters. **Independence requires spawning K child processes** — in-process looping does nothing (fact #2). |
| **(B) typed per-counter stability classification** | **REJECT as primary** | Trajectory divergence is whole-game, so it can perturb any counter; the counters that *do* diverge (`state_clone_for_legality`, `static_full_scans`, mana sweeps) are exactly the ones the gate watches. Demoting them guts the gate. Its honest core (drift is heterogeneous per counter) survives as the **informational per-counter drift/margin table** the M15 validation prints — gating nothing on its own, but feeding the margin criterion below. |
| **(C) empirically-derived multiplicative band** | **ADOPT (retain existing structure)** | Keep the implemented `current > baseline*RATIO + FLOOR` compare unchanged. The band now absorbs the *residual* median-of-K sampling error, so it stays at the implemented `1.05` ratio + `64` floor. The M15 margin gate is the authority that confirms/sizes it. |
| **(D) interim ship + #4878 dependency** | **ADOPT (trajectory)** | Ship median-of-K now; when #4878 makes the engine cross-process deterministic, collapse `PERF_SAMPLE_COUNT` to `1` and tighten the band. One-const change + baseline regen; a linking comment references #4878. |

**Why not just widen the band (C alone)?** One two-run sample cannot bound the tail, and every cold-process pair is
a different trajectory, so a K=1 gate compares different-apples-to-different-apples on every run. A band wide enough
to cover the full trajectory spread would blind the gate to real sub-band regressions. Median-of-K measures and
suppresses the spread structurally, so C-alone is strictly dominated. Median-of-K is the minimum honest mechanism
for a per-process-random engine, not gold-plating.

### Sensitivity argument, with numbers
- Observed single-run cross-process drift: big counters 0.5–1.3% (`static_full_scans` 37613/37294 = 0.85%,
  `mana_aura_trigger_scans` 7447/7363 = 1.13%); small counters up to ~15% (`legal_actions_spell_cost_sweeps`
  269/233) but **absorbed by the floor** (233·1.05+64 = 308 ≥ 269).
- With K=5 median over independent trajectories the compared statistic's run-to-run spread drops to ~0.3–0.6% on
  the big counters (median is outlier-robust; the observed distribution clusters near a dominant trajectory, not
  50/50 bimodal — confirmed by the ~1% single-run spread).
- Band `1.05` ⇒ a real regression must exceed ~5% on a big counter to trip. The gate's target class — clone storms,
  quadratic combat scans, display sweeps in search — produces **≥1.5× (50%+)** counter blow-ups, caught with ~10×
  margin. Sub-5% micro-regressions are explicitly out of scope (documented in the module doc).

**What M15 actually proves (softened per GAP 1).** M15 does **not** prove "≈0 false positives." It empirically
bounds the *observed* residual drift: across `PERF_REPRO_VALIDATION_RUNS` independent median-of-K gate runs, every
counter's worst observed value stays at or below the **midpoint** between its baseline and its FAIL threshold
(`PERF_REPRO_MARGIN_FRACTION = 0.5` of the band headroom). That establishes a **≥2× safety factor** between measured
drift and the trip point over the sampled process pairs — it bounds the measured envelope, not the true tail, and
carries no formal false-positive-probability claim. If any counter breaches the midpoint, the band/K is insufficient
for that counter and the baseline is **not** committed (escalate: raise `PERF_SAMPLE_COUNT`, or size the band up from
the printed max-drift table with a named-const + rationale).

**Net:** not flaky (median suppresses outliers + band ≫ statistic spread + margin-validated before commit) and not
insensitive to the target class (5% ≪ typical structural regression). Shippable.

---

## 2. Analogous trace (the base plan's compare pipeline is the thing being amended)

Traced the **win-rate gate** end-to-end and the **implemented perf gate** it mirrors:
`bin/ai_gate.rs` (arg parse → provenance via `command_output` → `compare::compare` → `print_markdown` → exit code)
→ `duel_suite/compare.rs` (`CompareError`, `load_report`, schema guard) — mirrored by the implemented
`bin/ai_perf_gate.rs` → `duel_suite/perf.rs` (`PerfReport`, `PerfCounters::from_snapshot`, `compare`,
`print_markdown`, matrix tests). Full path followed for this amendment:
`crates/phase-ai/src/duel_suite/perf.rs` → `crates/phase-ai/src/bin/ai_perf_gate.rs` →
`crates/phase-ai/src/duel_suite/run.rs` (`drive_game`, `pub(crate) fn drive_game(..) -> (Option<PlayerId>, u32)`)
→ `scripts/ai-perf-gate.sh` / `scripts/refresh-ai-perf-baseline.sh` → `.github/workflows/ai-gate.yml`
(`ai-perf-gate`, `ai-perf-gate-nightly` jobs). The amendment extends exactly this seam: a new pure `median_report`
aggregator + a pure `repro_margin_report` aggregator in `perf.rs`, a subprocess sampling loop and a validation-report
mode in `bin/ai_perf_gate.rs`, and one extended workload-guard clause in `compare`.

---

## 3. Concrete changes

### 3.1 `crates/phase-ai/src/duel_suite/perf.rs`

**(a) New sample-count const (workload-pinned; consts-not-flags):**
```rust
/// Number of INDEPENDENT cold-process trajectory samples the gate aggregates by
/// per-counter median. Independence is why each sample must be its own process
/// (fresh std RandomState) — see the binary's sampling loop. Odd so the median is
/// a single observed value. K=5 keeps the whole gate ~2 min (well under the
/// 30-min CI timeout) while suppressing minority-outlier trajectories.
///
/// #4878: when the engine's HashSet/HashMap iteration order stops leaking
/// per-process RandomState into AI tie-breaking, every trajectory becomes
/// cross-process identical; set this to 1 and tighten PERF_TOLERANCE_RATIO to
/// byte-exact, then regenerate the baseline.
pub const PERF_SAMPLE_COUNT: usize = 5;
```

**(b) New reproducibility-validation consts (GAP 1 — the M15 margin criterion, in `perf.rs` on purpose; see
placement argument below):**
```rust
/// Number of independent median-of-K gate runs the pre-baseline reproducibility
/// validation performs (in addition to the baseline-generating run). 25 gives a
/// tight empirical picture of the residual cross-process drift the band absorbs.
pub const PERF_REPRO_VALIDATION_RUNS: usize = 25;

/// Fraction of each counter's FAIL headroom (`threshold - baseline`) that the
/// WORST observed drift across the validation runs may consume. At 0.5 the entire
/// validated envelope must sit at or below the midpoint between baseline and FAIL
/// threshold — a >=2x safety factor between measured drift and the trip point.
/// This is the quantitative margin criterion: the drift table IS the gate.
pub const PERF_REPRO_MARGIN_FRACTION: f64 = 0.5;
```

*Placement argument (GAP 1).* The margin ceiling is defined in terms of the FAIL threshold, i.e. it must reuse the
exact `fail_threshold(baseline) = baseline*PERF_TOLERANCE_RATIO + PERF_ABSOLUTE_FLOOR` formula. Re-deriving that
formula in bash/jq inside the validation script would create a **second authority** for the band math that silently
drifts from `perf.rs` the day anyone tweaks the ratio or floor. So both consts and the pure aggregator
(`repro_margin_report`, below) live in `perf.rs`, composed from `fail_threshold`; the script only orchestrates the
run count and pastes the printed table. This keeps the numeric policy typed, unit-testable, and single-authority.

**(c) Bump schema (PerfReport gains a field → shape change):**
```rust
pub const PERF_SCHEMA_VERSION: u32 = 2; // was 1: added `sample_count`
```

**(d) Add `sample_count` to `PerfReport`** as a **required** field (no `#[serde(default)]` — no legacy JSON exists,
the baseline was never committed, so schema `1→2` needs no migration). Adding a required field makes **every**
`PerfReport { .. }` struct literal a compile error until updated — this is the self-flagging enforcement referenced in
GAP 5's threading sweep.
```rust
pub struct PerfReport {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_data_hash: Option<String>,
    pub base_seed: u64,
    pub action_cap: usize,
    /// Number of independent cold-process samples aggregated into this report.
    /// A single-trajectory suite run is `1`; a median report is `PERF_SAMPLE_COUNT`.
    /// Part of the estimator contract — the compare workload guard rejects a
    /// baseline/current pair produced with different K.
    pub sample_count: usize,
    pub scenarios: Vec<String>,
    pub counters: PerfCounters,
    /// Human triage only — NEVER compared.
    pub wall_clock_ms: u128,
}
```

**(e) New pure aggregator — the only new "compare semantics":**
```rust
/// Element-wise per-counter median over K independent single-trajectory sample
/// reports. Median (not mean) is outlier-robust: a minority anomalous trajectory
/// cannot move the aggregate. The result's counters need not equal any single
/// real trajectory — this gate compares aggregate COST LEVELS, not a replayed game.
///
/// Panics (internal invariant, not a runtime input path) if `samples` is empty or
/// the samples disagree on schema_version / base_seed / action_cap — every sample
/// is produced by the same binary at the same const workload, so disagreement is a
/// bug. Provenance (git_sha, card_data_hash) is left None for the caller to stamp.
pub fn median_report(samples: &[PerfReport]) -> PerfReport {
    assert!(!samples.is_empty(), "median_report requires at least one sample");
    let first = &samples[0];
    for s in &samples[1..] {
        assert_eq!(s.schema_version, first.schema_version, "sample schema mismatch");
        assert_eq!(s.base_seed, first.base_seed, "sample seed mismatch");
        assert_eq!(s.action_cap, first.action_cap, "sample action_cap mismatch");
    }
    // All samples share an identical key set (from_snapshot is a total destructure).
    let mut counters = BTreeMap::new();
    for key in first.counters.0.keys() {
        let mut vals: Vec<u64> = samples
            .iter()
            .map(|s| *s.counters.0.get(key).expect("sample missing a counter key"))
            .collect();
        vals.sort_unstable();
        counters.insert(key.clone(), vals[vals.len() / 2]); // upper-middle: real observed value, deterministic for any K
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
`vals.len()/2` is the exact middle for odd K and the upper-middle for even K — deterministic, always an observed
value, no fractional counter. K is pinned odd (=5); even-K totality is still tested (M-even).

**(f) New pure margin aggregator (GAP 1) — reuses `fail_threshold`, single authority for the band math:**
```rust
/// One counter's reproducibility-margin verdict over the validation runs.
pub struct ReproMarginRow {
    pub key: String,
    pub baseline: u64,
    /// max current observed across the validation runs.
    pub worst_current: u64,
    /// FAIL threshold = fail_threshold(baseline) = baseline*RATIO + FLOOR.
    pub threshold: u64,
    /// baseline + PERF_REPRO_MARGIN_FRACTION * (threshold - baseline).
    pub margin_ceiling: u64,
    pub within_margin: bool,
}

pub struct ReproMarginReport {
    pub rows: Vec<ReproMarginRow>,
}

impl ReproMarginReport {
    /// The margin gate: every counter's worst observed drift stayed within the
    /// named fraction of its FAIL headroom.
    pub fn all_within_margin(&self) -> bool {
        self.rows.iter().all(|r| r.within_margin)
    }
}

/// Aggregate the committed baseline + the N validation-run median reports into a
/// per-counter reproducibility-margin table. `worst_current` is the element-wise
/// MAX current across `runs`; `margin_ceiling` reuses `fail_threshold` so the band
/// formula has exactly one authority.
pub fn repro_margin_report(baseline: &PerfReport, runs: &[PerfReport]) -> ReproMarginReport {
    let mut rows = Vec::with_capacity(baseline.counters.0.len());
    for (key, &base) in &baseline.counters.0 {
        let worst_current = runs
            .iter()
            .map(|r| *r.counters.0.get(key).unwrap_or_else(|| {
                panic!("validation run missing counter '{key}' present in baseline")
            }))
            .max()
            .unwrap_or(base);
        let threshold = fail_threshold(base);
        let headroom = threshold - base; // >= PERF_ABSOLUTE_FLOOR, always > 0
        let margin_ceiling = base + (PERF_REPRO_MARGIN_FRACTION * headroom as f64) as u64;
        rows.push(ReproMarginRow {
            key: key.clone(),
            baseline: base,
            worst_current,
            threshold,
            margin_ceiling,
            within_margin: worst_current <= margin_ceiling,
        });
    }
    ReproMarginReport { rows }
}

/// Render the margin table to stdout; the row status column IS the gate result.
pub fn print_repro_margin(report: &ReproMarginReport) {
    println!();
    println!("| counter | baseline | worst_current | ceiling (50% band) | threshold | status |");
    println!("|---------|---------:|--------------:|-------------------:|----------:|--------|");
    for r in &report.rows {
        println!(
            "| {} | {} | {} | {} | {} | {} |",
            r.key, r.baseline, r.worst_current, r.margin_ceiling, r.threshold,
            if r.within_margin { "OK" } else { "OVER-MARGIN" },
        );
    }
    let over = report.rows.iter().filter(|r| !r.within_margin).count();
    println!("\nrepro margin: {over} OVER-MARGIN of {} counters", report.rows.len());
}
```

**(g) Extend the workload guard in `compare()`** so a baseline and current produced with different K are rejected
(K is part of the estimator contract — a K=1 baseline vs K=5 current is unsound). Add alongside the existing
`base_seed`/`action_cap` clauses (after them, before counter classification):
```rust
if baseline.sample_count != current.sample_count {
    return Err(PerfCompareError::WorkloadMismatch {
        field: "sample_count",
        baseline: baseline.sample_count.to_string(),
        current: current.sample_count.to_string(),
    });
}
```
**No new error variant — reuse `WorkloadMismatch { field, baseline, current }`.**

**(h) Module doc "baseline honesty" rewrite** (`//!` header, replacing the current "deterministic function of
`(binary, card-data, seed, action_cap)`" claim):
> The gate compares the **per-counter median over K independent cold-process trajectories** for a fixed
> `(binary, card-data, seed, action_cap, K)`. Individual trajectories are **not** cross-process deterministic —
> engine `HashSet`/`HashMap` iteration order leaks per-process `RandomState` into AI tie-breaking (issue #4878), so
> every process follows a slightly different game. Median-of-K plus the multiplicative band absorb that residual
> trajectory variance. Before the baseline is committed, a reproducibility validation
> (`scripts/validate-ai-perf-reproducibility.sh`) runs `PERF_REPRO_VALIDATION_RUNS` further median-of-K gate runs and
> requires every counter's worst observed value to stay within `PERF_REPRO_MARGIN_FRACTION` of its FAIL headroom
> (the midpoint between baseline and threshold) — a measured ≥2× safety factor, not a formal false-positive bound.
> Within a single process the trajectory is deterministic: `(winner, turn)` and 18 of 19 counters are byte-equal
> across in-process runs; only `layers_full_eval` jitters (HashSet-order layer-flush batching, absorbed by the
> floor). When #4878 lands, K→1 and the band tightens to byte-exact.

### 3.2 `crates/phase-ai/src/bin/ai_perf_gate.rs` — subprocess sampling orchestration + validation mode

Restructure `main()` into three mutually exclusive branches (the DB load moves OUT of the top level — only the child
branch loads the DB; the parent and validation branches never do):

**Args additions** (`struct Args` + `parse_args`): `emit_sample: Option<PathBuf>`, `repro_report: bool`,
`repro_inputs: Vec<PathBuf>`. New flags in the parse loop:
- `--emit-sample PATH` → `emit_sample = Some(next_path(..))`
- `--repro-report` → `repro_report = true`
- `--repro-input PATH` → `repro_inputs.push(next_path(..))` (repeatable)

**Branch 1 — child (`--emit-sample PATH` present):**
1. Load DB (`CardDatabase::from_export(&db_path)`), fail → `exit(2)`.
2. `let report = run_perf_suite(&db, PERF_BASE_SEED, PERF_ACTION_CAP, &default_scenarios());` (returns a
   `sample_count: 1` report).
3. `write_report(&report, PATH)` (writes to the FILE only), fail → `exit(2)`.
4. `exit(0)`. **The child never stamps provenance, never loads/compares a baseline, and (GAP 4) emits NOTHING on
   stdout** — no `print_markdown`, no `println!`. Any diagnostics use `eprintln!` (stderr) only.

**Branch 2 — repro-report (`--repro-report` present):** (no DB load)
1. `let baseline = load_report(&args.baseline)?;` (fail → `exit(2)`).
2. `let runs: Vec<PerfReport> = args.repro_inputs.iter().map(|p| load_report(p)).collect::<Result<_,_>>()?;`
   (fail → `exit(2)`).
3. `let margin = repro_margin_report(&baseline, &runs); print_repro_margin(&margin);`
4. `if margin.all_within_margin() { exit(0) } else { exit(1) }` — **this exit code is the M15 margin gate.**

**Branch 3 — parent gate (default; `--refresh-baseline` still handled here):** (no DB load)
1. Spawn `PERF_SAMPLE_COUNT` children via `std::env::current_exe()`, each:
   ```rust
   Command::new(&exe)
       .args(["--emit-sample", tmp_i, "--data-root", data_root_str])
       .stdout(std::process::Stdio::null())   // GAP 4: parent's stdout stays a clean table
       .stderr(std::process::Stdio::inherit()) // child diagnostics still visible in CI logs
       .status()
   ```
   `tmp_i = std::env::temp_dir().join(format!("ai-perf-sample-{}-{i}.json", std::process::id()))`.
2. **No silent truncation** ("no silent caps" principle): if any child exits non-zero, or its report is
   missing/unparseable, print the failure to stderr and `exit(2)`. The gate aggregates exactly K valid samples or
   fails loudly — a degraded K silently weakens the statistic.
3. `load_report` each of the K JSONs → `let mut current = median_report(&samples);`
4. Stamp provenance the parent can compute without loading the DB:
   `current.git_sha = command_output("git", &["rev-parse", "--short=12", "HEAD"]);`
   `current.card_data_hash = command_output("git", &["hash-object", path_str(&db_path)]);`
   (`db_path = args.data_root.join("card-data.json")`, never opened by the parent).
5. Provenance diagnostic to stderr (GAP 5 — now prints `sample_count`):
   `eprintln!("perf suite: seed={} action_cap={} sample_count={} scenarios={:?} wall_clock={}ms", current.base_seed, current.action_cap, current.sample_count, current.scenarios, current.wall_clock_ms);`
6. `write_report(&current, &args.current_output)`.
7. Existing tail unchanged: `--refresh-baseline` → compare-then-overwrite; else `load_report(baseline)` →
   `compare(&baseline, &current)` → `print_markdown` → `exit(1)` on `any_fail()`.
8. Best-effort cleanup: `remove_file` each temp sample (ignore errors).

Reuse the existing `command_output`, `write_report`, `load_report`, `path_str`, `next_path` helpers unchanged.
`print_usage` gains lines noting the gate runs `PERF_SAMPLE_COUNT` independent sample processes and documenting the
internal `--emit-sample` / `--repro-report` / `--repro-input` flags.

### 3.3 Scripts

- `scripts/ai-perf-gate.sh` / `refresh-ai-perf-baseline.sh`: **no logic change** — the K-sampling lives in the
  binary, so `cargo ai-perf-gate` and `--refresh-baseline` still do everything. Update the header comment in
  `refresh-ai-perf-baseline.sh` (and the guarantee note) to state the guarantee is now "median-of-K over K
  independent cold-process trajectories, margin-validated before commit" rather than single-run byte reproducibility.
- **New** `scripts/validate-ai-perf-reproducibility.sh` (the strict baseline-sequencing gate — M15):
  ```bash
  set -euo pipefail
  ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  export CARGO_TARGET_DIR="$ROOT/target/ai"     # isolated: no Tilt lock contention (mirrors ai-perf-gate.sh)
  cargo build --release --bin ai-perf-gate      # BUILD ONCE
  BIN="$CARGO_TARGET_DIR/release/ai-perf-gate"

  "$BIN" --refresh-baseline                     # 1) generate the median-of-K baseline

  N=25                                          # keep in sync with PERF_REPRO_VALIDATION_RUNS
  inputs=(); band_fail=0
  for i in $(seq 1 "$N"); do                    # 2) N further median-of-K gate runs vs the baseline
    out="$ROOT/target/ai-perf-repro-$i.json"
    start=$(date +%s)
    if ! "$BIN" --current-output "$out"; then band_fail=1; fi   # existing band gate (weak Bernoulli check)
    echo "run $i wall=$(( $(date +%s) - start ))s"
    inputs+=(--repro-input "$out")
  done

  "$BIN" --repro-report "${inputs[@]}"          # 3) MARGIN GATE — exit 0 iff all counters within 50% headroom
  margin_rc=$?

  if [ "$band_fail" -ne 0 ] || [ "$margin_rc" -ne 0 ]; then
    echo "REPRO VALIDATION FAILED (band_fail=$band_fail margin_rc=$margin_rc) — DO NOT COMMIT baseline; escalate."
    exit 1
  fi
  echo "REPRO VALIDATION PASSED — safe to commit the baseline; paste the margin table + per-run wall-clock into the PR."
  ```
  The executor runs this once and pastes (a) the margin table, (b) the per-run wall-clock lines, and (c) the PASS
  summary into the PR before committing the baseline. Note `N` mirrors `PERF_REPRO_VALIDATION_RUNS`; the numeric
  policy (the fraction) lives only in `perf.rs`.

### 3.4 `.github/workflows/ai-gate.yml` — CI runs the release profile (GAP 3)

**Problem.** The `ai-perf-gate` cargo alias is `run --bin ai-perf-gate --` — the **debug** profile. Both CI jobs
invoke `cargo ai-perf-gate` (line 140 PR job, line 175 nightly), so CI currently runs **debug**, while the plan's
~25s/sample figure and `scripts/ai-perf-gate.sh` are **release**. Counter *values* are profile-independent (logical
event counts), but wall-clock is not, and `current_exe()` under debug would spawn debug children — so K=5 debug
timing must be validated on the debug profile, which the release-only M15 script cannot produce. That breaks
"local M15 timing transfers to CI."

**Fix (option b, minimal, cache-friendly).** Change the two perf-gate `run:` lines to invoke the release binary via
the **default target dir** (so the existing `cache-shared-key: rust-ai-gate` still caches the build, and
`current_exe()` resolves to `target/release/ai-perf-gate` → release children):
- PR job (line 140): `cargo ai-perf-gate` → `cargo run --release --bin ai-perf-gate --`
- Nightly (line 175): `cargo ai-perf-gate > target/ai-perf-gate-report.md`
  → `cargo run --release --bin ai-perf-gate -- > target/ai-perf-gate-report.md`
  (`cargo` build progress goes to **stderr**, so the redirect captures only the binary's clean markdown stdout;
  GAP 4 guarantees the spawned children add nothing to stdout).

Add a one-line comment above each step: `# release profile so wall-clock matches local M15 validation; runs
PERF_SAMPLE_COUNT independent sample processes and compares the per-counter median (issue #4878).`

Leave the `ai-perf-gate` alias as debug for local one-off convenience (correct verdicts, faster incremental build);
document in the module doc / script header that the **authoritative gate profile is release**, as run by CI and M15.
`timeout-minutes` stays at **30**.

**Timing budget (GAP 3), made concrete and transferable.** M15 runs the release binary (via
`scripts/validate-ai-perf-reproducibility.sh`), which is the **same profile** CI now runs, so M15's measured
per-sample RUN wall-clock `W` is the CI per-sample wall-clock. The gate's runtime is `K × W ≈ 5 × ~25s ≈ ~2 min`
(from the round-3 release measurement; M15 re-measures and prints `W` per run). Adding the release build:
incremental (cache hit on `rust-ai-gate`) ≈ 1–2 min, cold worst-case ≈ ~15 min. Even cold: `~15 min build + ~2 min
run ≈ 17 min < 30 min` with margin. **M15 acceptance includes a timing check:** the executor confirms
`K × W(observed) + observed_build_time < 30 min` with comfortable margin before committing; if `W` is materially
larger than assumed, escalate (lower K or split the suite) rather than raising the timeout.

---

## 4. Mandatory architectural sections

- **Pattern Coverage.** Covers the *class* of cross-process-nondeterministic integer-counter regression gates. Any
  counter later added to `PerfCounterSnapshot` flows through `from_snapshot` (total struct destructure) → `merge_add`
  → `median_report` → `repro_margin_report` → `compare` automatically; nothing is per-counter special-cased. Not one
  counter.
- **Building Blocks.** Reuse the implemented `PerfReport`, `PerfCounters`, `compare`, `fail_threshold`, `load_report`,
  `write_report`, `print_markdown`, `command_output`, `path_str`, `next_path`,
  `PerfCompareError::WorkloadMismatch`. Two new pure fns — `median_report` (outlier-robust aggregation is the
  mechanism the blocker requires) and `repro_margin_report` (composed from `fail_threshold` so the band formula has a
  single authority). Subprocess loop composes `std::process::Command` + `std::env::current_exe`/`temp_dir` — the same
  `Command` primitive already used for `git` in `command_output`. No new external dependency.
- **Logic Placement.** Aggregation (`median_report`), the margin criterion (`repro_margin_report` +
  `PERF_REPRO_*` consts), and the estimator-contract guard (`sample_count`) live in the engine-adjacent `phase-ai`
  crate (`perf.rs`) — median-of-reports and headroom-fraction math are logic, and the margin math must reuse
  `fail_threshold` to stay single-authority (GAP 1 placement argument). The binary owns only process orchestration
  (spawning + IO + provenance stamping) and the thin `--repro-report` CLI adapter. Scripts own only build-isolation
  and the run-N-times loop.
- **Rust Idioms.** `sample_count` reuses the existing `WorkloadMismatch { field, .. }` shape (no new variant, no
  bool). Median via `sort_unstable` + midpoint index (no float, no fractional counter). Margin via reuse of
  `fail_threshold` + a single f64 multiply then `u64` floor. Internal invariants are `assert!`/`assert_eq!`
  (programmer error, not runtime input). Child/parent/validation branching is an explicit three-way dispatch on typed
  `Args` fields, exhaustive matches unchanged.
- **Extension vs Creation.** Pure extension of the implemented compare pipeline — two new pure fns, one new guard
  clause, one new required field, one subprocess loop + one CLI adapter branch. No new pattern; no new type beyond the
  consts and the `ReproMargin*` report structs (which mirror the existing `CounterRow`/`PerfCompareReport` shape).
- **Nom Compliance.** N/A — no file under `crates/engine/src/parser/` changes; this is CLI/aggregation tooling.
- **Variant Discoverability.** N/A — no new engine enum variant. `CounterVerdict`/`PerfCompareError` unchanged.
- **Identity / Provenance Contract.** The compared **authority** is the committed **median-of-K** report
  (`crates/phase-ai/baselines/perf-baseline.json`). Source concept: central tendency of the counter payload over K
  independent cold-process trajectories. Selected authority: `sample_count = PERF_SAMPLE_COUNT` at binding time
  (baseline refresh / M15). Live vs latched: latched into the committed JSON; `compare` reads it verbatim. Storage:
  `crates/phase-ai/baselines/perf-baseline.json`. Consumer: `compare()`. Invalidation: `schema_version`,
  `base_seed`, `action_cap`, **or `sample_count`** mismatch → `WorkloadMismatch`/`SchemaMismatch` (exit 2);
  card-data regen → non-gating hash-delta diagnostic. **Multi-authority hostile fixture:** a K=5 baseline vs a K=1
  current must be rejected by the extended workload guard (proves the K binding is enforced, not assumed) — M14.

---

## 5. Verification matrix (revert-failing)

**§5 wording (GAP 5).** `compare`'s **counter-classification core is unchanged**, but a workload-guard clause **is**
added (the `sample_count` check, §3.1g). M1–M8 and M10 exercise classification and the schema/seed/cap guards, not
the new K clause, so they remain valid unchanged; the new clause is covered by M14.

| # | Claim | Seam / entry point | Test (revert-failing assertion) | Hostile / negative sibling |
|---|---|---|---|---|
| **M9 (reframed + strengthened — GAP 2)** | The within-process guarantee is trajectory identity: `(winner, turn)` **and 18/19 counters** are byte-equal in-process; only `layers_full_eval` jitters. | `run::drive_game` + `run_perf_scenario` (in-process, same thread) | `#[ignore]` DB-gated (needs `PHASE_CARDS_PATH`). Resolve `red-mirror`; run in one process TWICE: for each run `perf_counters::reset(); let wt = drive_game(&payload, PERF_BASE_SEED, Medium, PERF_ACTION_CAP); let snap = perf_counters::snapshot();`. Assert `wt_1 == wt_2` (the `(Option<PlayerId>, u32)` tuple). Then build `let m1 = PerfCounters::from_snapshot(&snap_1).0; let m2 = ...(&snap_2).0;`, remove the `"layers_full_eval"` key from both, and assert `m1 == m2` (**all 18 other counters byte-equal**). Cite #4878 for the one exclusion. **Revert-failing:** if in-process determinism regresses on any of the 18 counters (e.g. `state_clone_for_legality` starts jittering in-process), the map compare fails; this is NOT vacuous — 18 exact values must match. | Paired positive reach-guard = the equal `(winner,turn)` tuple AND the non-empty 18-key map equality (proves the assertion actually ran over real counters, not an empty map). Do **not** assert full-snapshot equality (would falsely fail on `layers_full_eval`). |
| **M11** | `median_report` is element-wise per-counter **median**, not mean/min/max. | `perf::median_report` | Three samples with counter `c` = `[10, 1000, 20]` → median `20`. Assert `== 20` and `!= 343` (mean), `!= 10` (min), `!= 1000` (max). | Sibling counter with a different distribution in the same sample set medians independently. |
| **M12** | K=1 median is the identity. | `perf::median_report` | Single-sample slice → output counters/workload equal the input; `sample_count` in output = 1. | — |
| **M13** | Median inherits/pins workload; disagreeing samples are a hard internal error. | `perf::median_report` | Output `base_seed`/`action_cap`/`schema_version`/`scenarios` inherited from samples; `sample_count == samples.len()`. `#[should_panic]` test feeding samples with differing `base_seed`. | Proves the invariant is enforced, not assumed. Reachable only via programmer error (children always use the const workload) — documented. |
| **M14** | Extended workload guard rejects K mismatch (K binding is enforced). | `perf::compare` | Baseline `sample_count=5`, current `sample_count=1`, all else equal → `Err(WorkloadMismatch { field: "sample_count", .. })`; **not** a silent PASS. Revert-failing on the new guard clause (§3.1g). | Multi-authority hostile from §4 (K=5 authority vs K=1 current). |
| **M-even** | Median totality for even K. | `perf::median_report` | 4 samples with counter `[1,2,3,4]` → deterministic upper-middle `3` (index `len/2`); no panic, no fractional value. | Guards the `vals.len()/2` index against off-by-one on even K even though K is pinned odd. |
| **M-margin (GAP 1)** | `repro_margin_report` marks a counter OVER-MARGIN iff its worst observed current exceeds the 50%-headroom midpoint, reusing `fail_threshold`. | `perf::repro_margin_report` | baseline `c=100` ⇒ `threshold = fail_threshold(100) = 169`, `headroom = 69`, `margin_ceiling = 100 + 0.5*69 = 134`. Runs with `c` max `134` ⇒ `within_margin == true` and `all_within_margin()`; runs with `c` max `135` ⇒ `within_margin == false` and `!all_within_margin()`. **Revert-failing on both `PERF_REPRO_MARGIN_FRACTION` and the reuse of `fail_threshold`** (changing the fraction to 1.0 makes 135 within-margin; hardcoding a wrong threshold breaks 134/135 boundary). | Sibling counter within margin in the same report must NOT flip `all_within_margin()` — proves the "any over-margin fails" reduction. A run missing a baseline counter key `#[should_panic]`s (loud, no silent skip). |
| **M15 (empirical — strict baseline-sequencing gate; run-once by executor, output pasted into PR)** | The median-of-K + `1.05`+`64` band keeps every counter's worst observed drift within 50% of its FAIL headroom across N independent runs. | `scripts/validate-ai-perf-reproducibility.sh` → `ai-perf-gate --repro-report` | Generate a fresh median-of-K baseline, run `PERF_REPRO_VALIDATION_RUNS=25` further median-of-K gate runs (each its own cold children), then run `--repro-report` over the 25 current reports. **PASS iff (a) all 25 band-gate runs exit 0 AND (b) `all_within_margin()` (exit 0) — the margin table is the gate.** Also record per-run wall-clock and confirm `K×W + build < 30 min`. On FAIL, **do not commit the baseline**; escalate (raise `PERF_SAMPLE_COUNT`, or size the band up from the printed OVER-MARGIN rows with a named-const + rationale). | The margin gate is the quantitative bound a single sample could not provide; the 25×exit-0 check is the weaker Bernoulli sanity layer beneath it. |

Coverage-status impact: N/A (tooling; no card-coverage surface). No Oracle text accepted-but-deferred (no parser change).

---

## 6. Implementation steps (surgical order)

1. `perf.rs`: add `PERF_SAMPLE_COUNT`, `PERF_REPRO_VALIDATION_RUNS`, `PERF_REPRO_MARGIN_FRACTION` consts; bump
   `PERF_SCHEMA_VERSION` to `2`; add the required `sample_count` field to `PerfReport`.
2. `perf.rs`: thread `sample_count` through every `PerfReport` literal (GAP 5 — the compiler enforces this; sweep
   with `rg -n "PerfReport \{" crates/phase-ai`):
   - **Constructors:** `run_perf_suite` → `sample_count: 1` (single trajectory); `median_report` (new) →
     `sample_count: samples.len()`; test `mk_report` helper → `sample_count: PERF_SAMPLE_COUNT` (so both sides of a
     compare match; M14 overrides one side to force the mismatch). Any inline `PerfReport { .. }` in the new
     median/margin tests must also set it (or build via `mk_report`).
   - **Consumers:** `compare()` workload guard (new `sample_count` clause, §3.1g); the binary's provenance
     `eprintln!` (prints `sample_count`); the serialized baseline + current JSON (automatic via the `pub` serde
     field, now required at schema 2).
3. `perf.rs`: add `median_report`, `repro_margin_report`, `ReproMarginRow`, `ReproMarginReport`, `print_repro_margin`;
   rewrite the module `//!` honesty doc (§3.1h).
4. `perf.rs` tests: reframe+strengthen M9 (GAP 2, 18/19 + `(winner,turn)`); add M11–M14, M-even, M-margin. Keep
   M1–M8, M10.
5. `bin/ai_perf_gate.rs`: add `--emit-sample`, `--repro-report`, `--repro-input` args; implement the three-way
   `main()` dispatch (child loads DB + writes sample + `Stdio::null()` stdout hygiene; repro-report loads baseline +
   inputs + prints margin + exits on `all_within_margin`; parent spawns K children, fails loud on any bad sample,
   `median_report`, stamps provenance, prints `sample_count`, temp cleanup); update `print_usage`.
6. `scripts/refresh-ai-perf-baseline.sh`: header comment → median-of-K + margin-validated guarantee.
7. **New** `scripts/validate-ai-perf-reproducibility.sh` (§3.3 — build once, refresh baseline, N runs, `--repro-report`
   margin gate, PASS/FAIL).
8. `ai-gate.yml`: change the two perf-gate `run:` lines to `cargo run --release --bin ai-perf-gate --` (nightly keeps
   the `> target/ai-perf-gate-report.md` redirect); add the one-line comment (§3.4). Leave `timeout-minutes: 30`.
9. **Baseline sequencing (strict):** `cargo fmt --all`; verify `phase-ai` compiles + unit tests green (Tilt
   `test-ai`/`clippy` if up, else targeted `cargo test -p phase-ai` / `cargo clippy -p phase-ai --all-targets -D
   warnings` with a worktree-local `CARGO_TARGET_DIR`); run M9 `--ignored` locally; then run
   `scripts/validate-ai-perf-reproducibility.sh` (M15). **Only if the margin gate passes (and all 25 band runs exit 0,
   and `K×W + build < 30 min`)** commit the generated median-of-K `perf-baseline.json` (paste the margin table +
   per-run wall-clock + PASS summary into the PR). If M15 fails, escalate per M15 before committing anything.

## 7. Verification cadence
`cargo fmt --all` direct. `phase-ai` clippy + unit tests via Tilt `test-ai`/`clippy` when up, else targeted cargo with
an isolated `CARGO_TARGET_DIR` (worktrees aren't Tilt-watched). M9 `--ignored` + M15 run once locally before baseline
commit; M15 runs the release binary (the profile CI now runs, so its per-sample wall-clock transfers to CI). No
TypeScript touched. No engine files touched.
