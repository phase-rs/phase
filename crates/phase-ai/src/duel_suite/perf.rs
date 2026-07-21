//! Deterministic decision-cost regression gate for phase-ai.
//!
//! The win-rate gate (`cargo ai-gate`) is structurally blind to *cost-per-decision*
//! regressions: a change that doubles the number of `GameState` clones, static
//! sweeps, or mana display sweeps per AI decision produces the identical game
//! outcome and therefore passes the paired-seed comparison. This module closes
//! that gap by running a fixed, seeded, action-capped prefix of the three
//! quick-gate mirror matchups, field-wise summing the engine's
//! [`PerfCounterSnapshot`] across the three scenarios, and comparing the integer
//! counter payload against a committed baseline.
//!
//! **Guarantee.** The gate compares the **per-counter median over K independent
//! cold-process trajectories** for a fixed `(binary, card-data, seed, action_cap,
//! K)`.
//!
//! *Nothing* is guaranteed byte-stable across repeated runs — neither
//! cross-process **nor in-process**. `std::collections::hash_map::RandomState`
//! seeds each `HashMap`/`HashSet` from a thread-local key pair that is bumped once
//! **per allocation** (not per process), so even two sequential in-process games
//! allocate their maps at different offsets and see different iteration orders in
//! AI tie-breaking (issue #4878). The game's macro trajectory (`(winner, turn)`)
//! was observed equal in the one measured in-process pair, but is **not**
//! guaranteed and must not be relied upon. What jitters is **any**
//! iteration-order-dependent scan or clone count, in- and cross-process — observed
//! in-process: `layers_full_eval`, `state_clone_for_legality`, `static_full_scans`,
//! `crew_eligibility_scans`, `legal_actions_spell_cost_sweeps`,
//! `mana_aura_trigger_scans`; cross-process the divergence is larger and reaches
//! trajectory-coupled counters.
//!
//! The gate never depends on any single-run or in-process determinism. It compares
//! the per-counter median over K independent cold-process samples against the
//! committed baseline under the `1.05×+64` band; median-of-K suppresses
//! minority-outlier trajectories and the band absorbs residual drift. Before the
//! baseline is committed, a reproducibility validation
//! (`scripts/validate-ai-perf-reproducibility.sh`) runs `PERF_REPRO_VALIDATION_RUNS`
//! further median-of-K gate runs and requires every counter's worst observed value
//! to stay within `PERF_REPRO_MARGIN_FRACTION` of its FAIL headroom (the midpoint
//! between baseline and threshold) — a measured ≥2× safety factor, not a formal
//! false-positive bound.
//!
//! Counter *values* are profile-independent (logical event counts); the
//! **authoritative gate profile is debug** (`cargo ai-perf-gate`), which CI and
//! the M15 validation both run. When #4878 lands, K→1 and the band tightens to
//! byte-exact.
//!
//! It is NOT invariant across card-data regenerations: a new card set that shifts
//! the AI's decision trajectory will move the counters legitimately. The gate
//! therefore records a `card_data_hash` for provenance and prints a hash-delta
//! diagnostic on any FAIL so an operator can distinguish a genuine cost-per-node
//! regression (identical hashes) from a card-data-driven trajectory shift
//! (differing hashes). Intended increases are adopted through the refresh protocol
//! (`--refresh-baseline`), which prints the baseline-vs-current diff before
//! overwriting — never a blind widen.
//!
//! Wall-clock is recorded (`wall_clock_ms`) for human triage only; it is never
//! compared.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use engine::database::CardDatabase;
use engine::game::deck_loading::DeckPayload;
use engine::game::perf_counters::{self, PerfCounterSnapshot};
use serde::{Deserialize, Serialize};

use crate::config::AiDifficulty;

use super::find_matchup;
use super::run::{drive_game, resolve_matchup};

/// Schema version of the serialized [`PerfReport`] / baseline JSON. Bump when
/// the report shape or the counter field set changes (a changed field set is
/// self-flagged by [`PerfCounters::from_snapshot`]'s struct destructure — the
/// `Removed`/`New` classifications also warn to bump this).
pub const PERF_SCHEMA_VERSION: u32 = 3; // was 2: added SBA snapshot counters

/// Number of INDEPENDENT cold-process trajectory samples the gate aggregates by
/// per-counter median. Independence is why each sample must be its own process
/// (fresh std RandomState) — see the binary's sampling loop. Odd so the median is
/// a single observed value. K=5 keeps one gate run to a few minutes (M15 measured
/// ≈4.4 min per run at the initial measurement; the committed budget is validated
/// by `scripts/validate-ai-perf-reproducibility.sh`, see plan §3.4), well under the
/// 30-min CI timeout, while suppressing minority-outlier trajectories.
///
/// #4878: when the engine's HashSet/HashMap iteration order stops leaking
/// per-process RandomState into AI tie-breaking, every trajectory becomes
/// cross-process identical; set this to 1 and tighten PERF_TOLERANCE_RATIO to
/// byte-exact, then regenerate the baseline.
pub const PERF_SAMPLE_COUNT: usize = 5;

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

/// Fixed base seed for every perf scenario. A compile-time constant (not a CLI
/// flag) so the gate can never be run with a workload that mismatches the
/// baseline; the [`compare`] workload guard rejects a baseline generated under a
/// different seed.
pub const PERF_BASE_SEED: u64 = 0x9E37_79B9;

/// Fixed per-scenario action-cap prefix. Chosen to reach steady-state
/// combat/casting for all three mirror decks while keeping the whole suite well
/// under a few minutes. The cap is checked at `run_ai_actions` batch boundaries,
/// so the realized action count may overshoot slightly — identical semantics for
/// baseline and current runs.
pub const PERF_ACTION_CAP: usize = 3000;

/// Multiplicative regression tolerance: a counter fails only when it exceeds
/// `baseline * PERF_TOLERANCE_RATIO + PERF_ABSOLUTE_FLOOR`.
const PERF_TOLERANCE_RATIO: f64 = 1.05;

/// Additive floor so small counters (single- and double-digit) are not gated on
/// pure noise; negligible against the large clone/sweep counters.
const PERF_ABSOLUTE_FLOOR: u64 = 64;

/// The three quick-gate mirror matchups. Duplicated from
/// `bin/ai_gate.rs::DEFAULT_QUICK_FILTER` by necessity — no shared const exists,
/// and each id is verified to resolve via [`find_matchup`] at suite run time.
pub fn default_scenarios() -> Vec<&'static str> {
    vec!["red-mirror", "affinity-mirror", "enchantress-mirror"]
}

/// Field-wise integer counter payload, keyed by counter name. A `BTreeMap` for
/// deterministic (sorted) iteration in the report and stable JSON key order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PerfCounters(pub BTreeMap<String, u64>);

impl PerfCounters {
    /// Map **every** field of the working-tree [`PerfCounterSnapshot`] into the
    /// keyed payload via an exhaustive struct destructure. Adding or removing a
    /// counter field in `perf_counters.rs` makes this a compile error — the
    /// adapter is self-flagging by construction.
    pub fn from_snapshot(snapshot: &PerfCounterSnapshot) -> Self {
        let PerfCounterSnapshot {
            state_clone_for_legality,
            static_full_scans,
            spell_keyword_grant_scans,
            layers_full_eval,
            layers_incremental,
            layers_escalated,
            mana_display_sweeps,
            mana_display_swept_objects,
            stack_batch_candidates,
            stack_batch_plans,
            stack_batch_observer_refusals,
            stack_batched_entries,
            stack_inert_noop_batches,
            stack_inert_noop_entries,
            legal_actions_spell_cost_sweeps,
            mana_aura_trigger_scans,
            crew_eligibility_scans,
            attackable_player_sweeps,
            combat_shadow_block_scans,
            granted_ability_provider_scans,
            priority_cast_probe_builds,
            auto_tap_source_cache_builds,
            cached_auto_tap_source_reuses,
            cached_auto_tap_source_rejects,
            restriction_static_exact_scans,
            restriction_static_mode_gate_scans,
            legend_rule_mode_gate_scans,
            sba_battlefield_snapshot_builds,
            sba_empty_battlefield_short_circuits,
        } = *snapshot;

        let mut map = BTreeMap::new();
        map.insert(
            "state_clone_for_legality".to_string(),
            state_clone_for_legality,
        );
        map.insert("static_full_scans".to_string(), static_full_scans);
        map.insert(
            "spell_keyword_grant_scans".to_string(),
            spell_keyword_grant_scans,
        );
        map.insert("layers_full_eval".to_string(), layers_full_eval);
        map.insert("layers_incremental".to_string(), layers_incremental);
        map.insert("layers_escalated".to_string(), layers_escalated);
        map.insert("mana_display_sweeps".to_string(), mana_display_sweeps);
        map.insert(
            "mana_display_swept_objects".to_string(),
            mana_display_swept_objects,
        );
        map.insert("stack_batch_candidates".to_string(), stack_batch_candidates);
        map.insert("stack_batch_plans".to_string(), stack_batch_plans);
        map.insert(
            "stack_batch_observer_refusals".to_string(),
            stack_batch_observer_refusals,
        );
        map.insert("stack_batched_entries".to_string(), stack_batched_entries);
        map.insert(
            "stack_inert_noop_batches".to_string(),
            stack_inert_noop_batches,
        );
        map.insert(
            "stack_inert_noop_entries".to_string(),
            stack_inert_noop_entries,
        );
        map.insert(
            "legal_actions_spell_cost_sweeps".to_string(),
            legal_actions_spell_cost_sweeps,
        );
        map.insert(
            "mana_aura_trigger_scans".to_string(),
            mana_aura_trigger_scans,
        );
        map.insert("crew_eligibility_scans".to_string(), crew_eligibility_scans);
        map.insert(
            "attackable_player_sweeps".to_string(),
            attackable_player_sweeps,
        );
        map.insert(
            "combat_shadow_block_scans".to_string(),
            combat_shadow_block_scans,
        );
        map.insert(
            "granted_ability_provider_scans".to_string(),
            granted_ability_provider_scans,
        );
        map.insert(
            "priority_cast_probe_builds".to_string(),
            priority_cast_probe_builds,
        );
        map.insert(
            "auto_tap_source_cache_builds".to_string(),
            auto_tap_source_cache_builds,
        );
        map.insert(
            "cached_auto_tap_source_reuses".to_string(),
            cached_auto_tap_source_reuses,
        );
        map.insert(
            "cached_auto_tap_source_rejects".to_string(),
            cached_auto_tap_source_rejects,
        );
        map.insert(
            "restriction_static_exact_scans".to_string(),
            restriction_static_exact_scans,
        );
        map.insert(
            "restriction_static_mode_gate_scans".to_string(),
            restriction_static_mode_gate_scans,
        );
        map.insert(
            "legend_rule_mode_gate_scans".to_string(),
            legend_rule_mode_gate_scans,
        );
        map.insert(
            "sba_battlefield_snapshot_builds".to_string(),
            sba_battlefield_snapshot_builds,
        );
        map.insert(
            "sba_empty_battlefield_short_circuits".to_string(),
            sba_empty_battlefield_short_circuits,
        );
        Self(map)
    }

    /// Field-wise add `other` into `self` (union of keys, summed values). Used to
    /// aggregate the per-scenario snapshots into a single suite payload.
    fn merge_add(&mut self, other: &PerfCounters) {
        for (key, value) in &other.0 {
            *self.0.entry(key.clone()).or_insert(0) += value;
        }
    }
}

/// A single perf-suite run: the aggregated counter payload plus workload
/// provenance. Serialized as the committed baseline and the current-run report.
/// The baseline JSON and this struct ARE a serialized surface — see
/// `schema_version` discipline in the module doc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Run a single perf scenario: reset the thread-local counters, drive a capped
/// game on the *same thread*, and snapshot the counters. The reset/snapshot pair
/// is only meaningful because the counted paths never leave the calling thread.
pub fn run_perf_scenario(
    payload: &DeckPayload,
    seed: u64,
    action_cap: usize,
) -> PerfCounterSnapshot {
    perf_counters::reset();
    let _ = drive_game(payload, seed, AiDifficulty::Medium, action_cap);
    perf_counters::snapshot()
}

/// Run every scenario in `scenarios` and field-wise sum their counter snapshots
/// into a single [`PerfReport`]. Provenance fields (`git_sha`, `card_data_hash`)
/// are left `None` for the binary to populate. Never invokes any parallel
/// runner — the entire suite executes on the calling thread so the thread-local
/// counters see exactly the work this suite performs.
pub fn run_perf_suite(
    db: &CardDatabase,
    seed: u64,
    action_cap: usize,
    scenarios: &[&str],
) -> PerfReport {
    let start = Instant::now();
    let mut counters = PerfCounters::default();
    for id in scenarios {
        let spec = find_matchup(id)
            .unwrap_or_else(|| panic!("perf scenario id '{id}' does not resolve via find_matchup"));
        let (payload, _p0, _p1) = resolve_matchup(db, spec)
            .unwrap_or_else(|err| panic!("perf scenario '{id}' failed to resolve decks: {err}"));
        let snapshot = run_perf_scenario(&payload, seed, action_cap);
        counters.merge_add(&PerfCounters::from_snapshot(&snapshot));
    }
    let wall_clock_ms = start.elapsed().as_millis();

    PerfReport {
        schema_version: PERF_SCHEMA_VERSION,
        git_sha: None,
        card_data_hash: None,
        base_seed: seed,
        action_cap,
        sample_count: 1,
        scenarios: scenarios.iter().map(|s| s.to_string()).collect(),
        counters,
        wall_clock_ms,
    }
}

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
    assert!(
        !samples.is_empty(),
        "median_report requires at least one sample"
    );
    let first = &samples[0];
    for s in &samples[1..] {
        assert_eq!(
            s.schema_version, first.schema_version,
            "sample schema mismatch"
        );
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
        // upper-middle: real observed value, deterministic for any K.
        counters.insert(key.clone(), vals[vals.len() / 2]);
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

/// Per-counter comparison verdict. Exhaustive (no wildcard) so a new variant is
/// a compile error at every match site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CounterVerdict {
    /// Within tolerance (or a decrease — reported as an improvement).
    Pass,
    /// Exceeds `baseline * ratio + floor`.
    Fail,
    /// Present in current, absent from baseline. Informational.
    New,
    /// Present in baseline, absent from current. Informational + warning.
    Removed,
}

/// One row of the counter comparison.
#[derive(Debug, Clone)]
pub struct CounterRow {
    pub key: String,
    pub baseline: Option<u64>,
    pub current: Option<u64>,
    /// The FAIL threshold `baseline * ratio + floor`, when both values exist.
    pub threshold: Option<u64>,
    pub verdict: CounterVerdict,
}

/// Full comparison result. Carries the two card-data hashes so the printer can
/// emit the hash-delta diagnostic on a FAIL without re-threading the reports.
#[derive(Debug, Clone)]
pub struct PerfCompareReport {
    pub rows: Vec<CounterRow>,
    pub baseline_card_data_hash: Option<String>,
    pub current_card_data_hash: Option<String>,
}

impl PerfCompareReport {
    /// True iff any counter regressed beyond tolerance. Drives the exit code.
    pub fn any_fail(&self) -> bool {
        self.rows
            .iter()
            .any(|r| matches!(r.verdict, CounterVerdict::Fail))
    }

    /// True iff the field set changed (a `New` or `Removed` row is present),
    /// which should prompt a `schema_version` bump + refresh.
    pub fn field_set_changed(&self) -> bool {
        self.rows
            .iter()
            .any(|r| matches!(r.verdict, CounterVerdict::New | CounterVerdict::Removed))
    }

    /// True iff card-data hashes are both present and differ.
    pub fn card_data_changed(&self) -> bool {
        match (&self.baseline_card_data_hash, &self.current_card_data_hash) {
            (Some(b), Some(c)) => b != c,
            _ => false,
        }
    }
}

/// Comparison error. Parallels the win-rate gate's `compare::CompareError` but
/// is defined locally (that type lives in the out-of-bounds `compare.rs` and
/// lacks the workload-mismatch variant this gate needs).
#[derive(Debug)]
pub enum PerfCompareError {
    Io(std::io::Error),
    Parse(serde_json::Error),
    /// Report schema versions differ — comparison is meaningless (exit 2).
    SchemaMismatch {
        baseline: u32,
        current: u32,
    },
    /// The workload (seed or action_cap) differs — the counter payloads describe
    /// different runs, so any comparison would be a false PASS/FAIL (exit 2).
    WorkloadMismatch {
        field: &'static str,
        baseline: String,
        current: String,
    },
}

impl std::fmt::Display for PerfCompareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PerfCompareError::Io(e) => write!(f, "perf compare I/O error: {e}"),
            PerfCompareError::Parse(e) => write!(f, "perf compare parse error: {e}"),
            PerfCompareError::SchemaMismatch { baseline, current } => write!(
                f,
                "schema_version mismatch: baseline={baseline} current={current} — bump schema_version and refresh the baseline"
            ),
            PerfCompareError::WorkloadMismatch {
                field,
                baseline,
                current,
            } => write!(
                f,
                "workload changed ({field}: baseline={baseline} current={current}) — comparison invalid, refresh baseline"
            ),
        }
    }
}

impl std::error::Error for PerfCompareError {}

impl From<std::io::Error> for PerfCompareError {
    fn from(e: std::io::Error) -> Self {
        PerfCompareError::Io(e)
    }
}

impl From<serde_json::Error> for PerfCompareError {
    fn from(e: serde_json::Error) -> Self {
        PerfCompareError::Parse(e)
    }
}

/// Read a [`PerfReport`] from a JSON file.
pub fn load_report(path: &Path) -> Result<PerfReport, PerfCompareError> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let report: PerfReport = serde_json::from_reader(reader)?;
    Ok(report)
}

/// FAIL threshold for a counter: `baseline * ratio + floor`, rounded via f64
/// (the counters are far below f64's 2^53 exact-integer ceiling).
fn fail_threshold(baseline: u64) -> u64 {
    (baseline as f64 * PERF_TOLERANCE_RATIO + PERF_ABSOLUTE_FLOOR as f64) as u64
}

/// A current counter fails iff it strictly exceeds `baseline * ratio + floor`.
/// Equality is a PASS; decreases always pass.
fn counter_fails(baseline: u64, current: u64) -> bool {
    (current as f64) > (baseline as f64) * PERF_TOLERANCE_RATIO + PERF_ABSOLUTE_FLOOR as f64
}

/// Compare a current report against a baseline. Guards run in order: (1) schema
/// version, (2) workload (seed/action_cap). Only then are counters classified
/// per key across the union of baseline and current keys.
pub fn compare(
    baseline: &PerfReport,
    current: &PerfReport,
) -> Result<PerfCompareReport, PerfCompareError> {
    if baseline.schema_version != current.schema_version {
        return Err(PerfCompareError::SchemaMismatch {
            baseline: baseline.schema_version,
            current: current.schema_version,
        });
    }
    if baseline.base_seed != current.base_seed {
        return Err(PerfCompareError::WorkloadMismatch {
            field: "base_seed",
            baseline: baseline.base_seed.to_string(),
            current: current.base_seed.to_string(),
        });
    }
    if baseline.action_cap != current.action_cap {
        return Err(PerfCompareError::WorkloadMismatch {
            field: "action_cap",
            baseline: baseline.action_cap.to_string(),
            current: current.action_cap.to_string(),
        });
    }
    // K is part of the estimator contract — a K=1 baseline vs a K=5 current
    // compares a single trajectory against a median of five, which is unsound.
    if baseline.sample_count != current.sample_count {
        return Err(PerfCompareError::WorkloadMismatch {
            field: "sample_count",
            baseline: baseline.sample_count.to_string(),
            current: current.sample_count.to_string(),
        });
    }

    let mut keys: BTreeMap<&str, ()> = BTreeMap::new();
    keys.extend(baseline.counters.0.keys().map(|k| (k.as_str(), ())));
    keys.extend(current.counters.0.keys().map(|k| (k.as_str(), ())));

    let mut rows = Vec::with_capacity(keys.len());
    for key in keys.into_keys() {
        let baseline_value = baseline.counters.0.get(key).copied();
        let current_value = current.counters.0.get(key).copied();
        let (threshold, verdict) = match (baseline_value, current_value) {
            (Some(b), Some(c)) => {
                let verdict = if counter_fails(b, c) {
                    CounterVerdict::Fail
                } else {
                    CounterVerdict::Pass
                };
                (Some(fail_threshold(b)), verdict)
            }
            (None, Some(_)) => (None, CounterVerdict::New),
            (Some(_), None) => (None, CounterVerdict::Removed),
            (None, None) => unreachable!("key came from the union of both maps"),
        };
        rows.push(CounterRow {
            key: key.to_string(),
            baseline: baseline_value,
            current: current_value,
            threshold,
            verdict,
        });
    }

    Ok(PerfCompareReport {
        rows,
        baseline_card_data_hash: baseline.card_data_hash.clone(),
        current_card_data_hash: current.card_data_hash.clone(),
    })
}

fn verdict_str(v: CounterVerdict) -> &'static str {
    match v {
        CounterVerdict::Pass => "PASS",
        CounterVerdict::Fail => "FAIL",
        CounterVerdict::New => "NEW",
        CounterVerdict::Removed => "REMOVED",
    }
}

/// Render the comparison as a markdown table to stdout; diagnostics (hash-delta
/// annotation, removed-field warning) go to stderr so a redirected stdout report
/// stays a clean table.
pub fn print_markdown(report: &PerfCompareReport) {
    println!();
    println!("| counter | baseline | current | delta | threshold | status |");
    println!("|---------|---------:|--------:|------:|----------:|--------|");
    for row in &report.rows {
        let baseline_cell = row.baseline.map_or("—".to_string(), |v| v.to_string());
        let current_cell = row.current.map_or("—".to_string(), |v| v.to_string());
        let delta_cell = match (row.baseline, row.current) {
            (Some(b), Some(c)) => format!("{:+}", c as i128 - b as i128),
            _ => "—".to_string(),
        };
        let threshold_cell = row.threshold.map_or("—".to_string(), |v| v.to_string());
        println!(
            "| {} | {} | {} | {} | {} | {} |",
            row.key,
            baseline_cell,
            current_cell,
            delta_cell,
            threshold_cell,
            verdict_str(row.verdict),
        );
    }

    let mut pass = 0usize;
    let mut fail = 0usize;
    let mut new = 0usize;
    let mut removed = 0usize;
    for row in &report.rows {
        match row.verdict {
            CounterVerdict::Pass => pass += 1,
            CounterVerdict::Fail => fail += 1,
            CounterVerdict::New => new += 1,
            CounterVerdict::Removed => removed += 1,
        }
    }
    println!("\nperf compare: {fail} FAIL, {pass} PASS, {new} NEW, {removed} REMOVED");

    if report.field_set_changed() {
        eprintln!(
            "warning: counter field set changed (NEW/REMOVED rows present) — bump PERF_SCHEMA_VERSION and refresh the baseline"
        );
    }
    if report.any_fail() && report.card_data_changed() {
        eprintln!(
            "note: card-data hash changed ({}→{}) — likely a card-data-driven trajectory shift, not a cost-per-node regression; review and refresh if intended",
            report.baseline_card_data_hash.as_deref().unwrap_or("?"),
            report.current_card_data_hash.as_deref().unwrap_or("?"),
        );
    }
}

/// One counter's reproducibility-margin verdict over the validation runs.
#[derive(Debug, Clone)]
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

/// Per-counter reproducibility-margin table over the validation runs.
#[derive(Debug, Clone)]
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
    // Zero runs would make every `worst_current` default to `base` and the
    // whole table vacuously OK — a silent pass when validation runs failed to
    // generate or were misconfigured.
    assert!(
        !runs.is_empty(),
        "repro_margin_report requires at least one validation run"
    );
    let mut rows = Vec::with_capacity(baseline.counters.0.len());
    for (key, &base) in &baseline.counters.0 {
        let worst_current = runs
            .iter()
            .map(|r| {
                *r.counters.0.get(key).unwrap_or_else(|| {
                    panic!("validation run missing counter '{key}' present in baseline")
                })
            })
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
            r.key,
            r.baseline,
            r.worst_current,
            r.margin_ceiling,
            r.threshold,
            if r.within_margin { "OK" } else { "OVER-MARGIN" },
        );
    }
    let over = report.rows.iter().filter(|r| !r.within_margin).count();
    println!(
        "\nrepro margin: {over} OVER-MARGIN of {} counters",
        report.rows.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn mk_report(counters: &[(&str, u64)]) -> PerfReport {
        PerfReport {
            schema_version: PERF_SCHEMA_VERSION,
            git_sha: None,
            card_data_hash: None,
            base_seed: PERF_BASE_SEED,
            action_cap: PERF_ACTION_CAP,
            sample_count: PERF_SAMPLE_COUNT,
            scenarios: default_scenarios().iter().map(|s| s.to_string()).collect(),
            counters: PerfCounters(counters.iter().map(|(k, v)| (k.to_string(), *v)).collect()),
            wall_clock_ms: 0,
        }
    }

    fn verdict_for(report: &PerfCompareReport, key: &str) -> CounterVerdict {
        report
            .rows
            .iter()
            .find(|r| r.key == key)
            .unwrap_or_else(|| panic!("no row for key {key}"))
            .verdict
    }

    // Matrix 1: boundary is revert-failing on both the ratio and floor constants.
    // baseline=100 → threshold = 100 * 1.05 + 64 = 169. current=169 ⇒ PASS,
    // current=170 ⇒ FAIL. Widening ratio (e.g. 1.10 → 174) or floor (e.g. 128 →
    // 233) makes 170 a PASS, flipping this assertion.
    #[test]
    fn boundary_pass_at_threshold_fail_just_above() {
        let baseline = mk_report(&[("c", 100)]);

        let at_threshold = mk_report(&[("c", 169)]);
        let report = compare(&baseline, &at_threshold).unwrap();
        assert_eq!(verdict_for(&report, "c"), CounterVerdict::Pass);
        assert!(!report.any_fail());

        let just_above = mk_report(&[("c", 170)]);
        let report = compare(&baseline, &just_above).unwrap();
        assert_eq!(verdict_for(&report, "c"), CounterVerdict::Fail);
        assert!(report.any_fail());
    }

    // Matrix 2.
    #[test]
    fn identity_all_pass() {
        let report = mk_report(&[("a", 10), ("b", 5000), ("c", 0)]);
        let compared = compare(&report, &report).unwrap();
        assert!(!compared.any_fail());
        assert!(compared
            .rows
            .iter()
            .all(|r| matches!(r.verdict, CounterVerdict::Pass)));
    }

    // Matrix 3.
    #[test]
    fn decrease_is_pass() {
        let baseline = mk_report(&[("c", 10_000)]);
        let current = mk_report(&[("c", 1)]);
        let report = compare(&baseline, &current).unwrap();
        assert_eq!(verdict_for(&report, "c"), CounterVerdict::Pass);
        assert!(!report.any_fail());
    }

    // Matrix 4: a NEW key is informational, but a DIFFERENT key over threshold in
    // the same report still FAILs (guards against New short-circuiting any_fail).
    #[test]
    fn new_key_informational_but_sibling_regression_still_fails() {
        let baseline = mk_report(&[("shared", 100)]);
        let current = mk_report(&[("shared", 10_000), ("brand_new", 42)]);
        let report = compare(&baseline, &current).unwrap();
        assert_eq!(verdict_for(&report, "brand_new"), CounterVerdict::New);
        assert_eq!(verdict_for(&report, "shared"), CounterVerdict::Fail);
        assert!(report.any_fail());
    }

    // Matrix 5.
    #[test]
    fn removed_key_informational_with_warning_flag() {
        let baseline = mk_report(&[("gone", 100), ("kept", 5)]);
        let current = mk_report(&[("kept", 5)]);
        let report = compare(&baseline, &current).unwrap();
        assert_eq!(verdict_for(&report, "gone"), CounterVerdict::Removed);
        assert!(!report.any_fail());
        assert!(report.field_set_changed());
    }

    // Matrix 6.
    #[test]
    fn schema_mismatch_returns_error() {
        let mut baseline = mk_report(&[("c", 1)]);
        baseline.schema_version = PERF_SCHEMA_VERSION + 1;
        let current = mk_report(&[("c", 1)]);
        let err = compare(&baseline, &current).unwrap_err();
        assert!(matches!(err, PerfCompareError::SchemaMismatch { .. }));
    }

    // Matrix 6b: differing workload is a hard error, never a silent PASS.
    #[test]
    fn workload_guard_rejects_differing_action_cap_and_seed() {
        let baseline = mk_report(&[("c", 1)]);

        let mut cap_changed = mk_report(&[("c", 1)]);
        cap_changed.action_cap = PERF_ACTION_CAP + 1;
        let err = compare(&baseline, &cap_changed).unwrap_err();
        assert!(matches!(
            err,
            PerfCompareError::WorkloadMismatch {
                field: "action_cap",
                ..
            }
        ));

        let mut seed_changed = mk_report(&[("c", 1)]);
        seed_changed.base_seed = PERF_BASE_SEED + 1;
        let err = compare(&baseline, &seed_changed).unwrap_err();
        assert!(matches!(
            err,
            PerfCompareError::WorkloadMismatch {
                field: "base_seed",
                ..
            }
        ));
    }

    // Matrix 7: adapter totality — a distinct non-zero value per field yields one
    // map entry per field, values round-trip, WITHOUT hardcoding the field count.
    // Assigning 1..=N in the struct literal is self-flagging: adding/removing a
    // field breaks compilation here and in `from_snapshot`.
    #[test]
    fn from_snapshot_maps_every_field_distinctly() {
        let snapshot = PerfCounterSnapshot {
            state_clone_for_legality: 1,
            static_full_scans: 2,
            spell_keyword_grant_scans: 29,
            layers_full_eval: 3,
            layers_incremental: 4,
            layers_escalated: 5,
            mana_display_sweeps: 6,
            mana_display_swept_objects: 7,
            stack_batch_candidates: 8,
            stack_batch_plans: 9,
            stack_batch_observer_refusals: 10,
            stack_batched_entries: 11,
            stack_inert_noop_batches: 12,
            stack_inert_noop_entries: 13,
            legal_actions_spell_cost_sweeps: 14,
            mana_aura_trigger_scans: 15,
            crew_eligibility_scans: 16,
            attackable_player_sweeps: 17,
            combat_shadow_block_scans: 18,
            granted_ability_provider_scans: 19,
            priority_cast_probe_builds: 20,
            auto_tap_source_cache_builds: 21,
            cached_auto_tap_source_reuses: 22,
            cached_auto_tap_source_rejects: 23,
            restriction_static_exact_scans: 24,
            restriction_static_mode_gate_scans: 25,
            legend_rule_mode_gate_scans: 26,
            sba_battlefield_snapshot_builds: 27,
            sba_empty_battlefield_short_circuits: 28,
        };
        let counters = PerfCounters::from_snapshot(&snapshot);

        let values: Vec<u64> = counters.0.values().copied().collect();
        let unique: BTreeSet<u64> = values.iter().copied().collect();
        // One entry per field: no field collapsed onto another's key.
        assert_eq!(values.len(), unique.len());
        // Values 1..=N round-trip contiguously: min=1, max=entry count.
        assert_eq!(*unique.iter().min().unwrap(), 1);
        assert_eq!(*unique.iter().max().unwrap(), counters.0.len() as u64);
    }

    // Matrix 7 (aggregation): merge_add is field-wise additive.
    #[test]
    fn merge_add_sums_field_wise() {
        let mut a = PerfCounters(BTreeMap::from([("x".to_string(), 3), ("y".to_string(), 1)]));
        let b = PerfCounters(BTreeMap::from([("x".to_string(), 4), ("z".to_string(), 9)]));
        a.merge_add(&b);
        assert_eq!(a.0.get("x"), Some(&7));
        assert_eq!(a.0.get("y"), Some(&1));
        assert_eq!(a.0.get("z"), Some(&9));
    }

    // Matrix 8: hash-delta annotation predicate is true iff a FAIL coincides with
    // differing card-data hashes.
    #[test]
    fn card_data_delta_flag_only_on_fail_with_differing_hash() {
        let mut baseline = mk_report(&[("c", 100)]);
        let mut current = mk_report(&[("c", 10_000)]);
        baseline.card_data_hash = Some("aaaa".to_string());
        current.card_data_hash = Some("bbbb".to_string());
        let report = compare(&baseline, &current).unwrap();
        assert!(report.any_fail());
        assert!(report.card_data_changed());

        // Same hash on a FAIL ⇒ genuine same-workload regression, no annotation.
        let mut current_same = mk_report(&[("c", 10_000)]);
        current_same.card_data_hash = Some("aaaa".to_string());
        let mut baseline_same = mk_report(&[("c", 100)]);
        baseline_same.card_data_hash = Some("aaaa".to_string());
        let report = compare(&baseline_same, &current_same).unwrap();
        assert!(report.any_fail());
        assert!(!report.card_data_changed());
    }

    // Matrix 10: each hardcoded scenario id resolves via find_matchup (fails
    // loudly on a matchup-id rename).
    #[test]
    fn default_scenarios_all_resolve() {
        for id in default_scenarios() {
            assert!(
                find_matchup(id).is_some(),
                "perf scenario id '{id}' no longer resolves via find_matchup"
            );
        }
    }

    // Matrix 9 (reframed — addendum r1 Decision 1): a NON-asserting in-process
    // jitter DIAGNOSTIC. In-process repeat identity is NOT a std guarantee:
    // `std::collections::hash_map::RandomState::default()` seeds each HashMap/
    // HashSet from a thread-local `(k0, k1)` pair whose `k0` is bumped by one on
    // every allocation, so two sequential in-process games allocate their maps at
    // different offsets → different SipHash keys → different iteration orders in
    // AI tie-breaking (issue #4878). Both `(winner, turn)` and every
    // iteration-order-dependent counter may therefore differ between two in-process
    // runs. This test drives two in-process games, asserts only NON-stochastic
    // structural liveness (so it can never flake), and PRINTS the observed
    // in-process pair-diff (pasted into issue #4878). It asserts no counter value
    // and no `(winner, turn)` equality. Requires PHASE_CARDS_PATH; run with
    // `--ignored` before generating the baseline.
    #[test]
    #[ignore = "requires card database via PHASE_CARDS_PATH; run before baseline refresh"]
    fn perf_in_process_jitter_diagnostic() {
        let data_root = std::env::var("PHASE_CARDS_PATH")
            .expect("set PHASE_CARDS_PATH to the data directory for the determinism test");
        let db_path = std::path::Path::new(&data_root).join("card-data.json");
        let db = CardDatabase::from_export(&db_path)
            .unwrap_or_else(|e| panic!("failed to load card db from {}: {e}", db_path.display()));

        let spec = find_matchup("red-mirror").expect("red-mirror resolves");
        let (payload, _p0, _p1) = resolve_matchup(&db, spec).expect("red-mirror decks resolve");

        // Run TWICE in this same process; capture both the (winner, turn) outcome
        // and the counter snapshot for each run.
        perf_counters::reset();
        let wt_1 = drive_game(
            &payload,
            PERF_BASE_SEED,
            AiDifficulty::Medium,
            PERF_ACTION_CAP,
        );
        let snap_1 = perf_counters::snapshot();

        perf_counters::reset();
        let wt_2 = drive_game(
            &payload,
            PERF_BASE_SEED,
            AiDifficulty::Medium,
            PERF_ACTION_CAP,
        );
        let snap_2 = perf_counters::snapshot();

        let m1 = PerfCounters::from_snapshot(&snap_1).0;
        let m2 = PerfCounters::from_snapshot(&snap_2).0;

        // Structural reach-guards (NON-stochastic — cannot flake under #4878):
        //  1. real counters were recorded;
        //  2. a core trajectory counter is non-zero, proving a real game ran.
        // (No key-set assert: `from_snapshot` is a total struct destructure with no
        // `..`, so both maps always carry the identical schema-total key set — such
        // an assert is vacuous. Schema-total coverage is owned by the compiler and
        // by `from_snapshot_maps_every_field_distinctly`.)
        assert!(
            !m1.is_empty(),
            "counter map must be non-empty (real counters recorded)"
        );
        assert!(
            m1.get("state_clone_for_legality").copied().unwrap_or(0) > 0,
            "a real game must clone for legality at least once"
        );

        // DIAGNOSTIC (no assertion): print the in-process pair-diff. Under
        // per-allocation std RandomState (#4878) both (winner, turn) and every
        // HashSet-order-dependent counter may differ between two in-process runs;
        // this table documents the observed in-process jitter footprint and is
        // pasted into issue #4878. Equality is deliberately NOT asserted.
        println!(
            "in-process (winner, turn): run1={wt_1:?} run2={wt_2:?} equal={}",
            wt_1 == wt_2
        );
        let mut jittered = 0usize;
        for (key, v1) in &m1 {
            let v2 = m2.get(key).copied().unwrap_or(0);
            if *v1 != v2 {
                jittered += 1;
                println!(
                    "  JITTER {key}: {v1} -> {v2} (delta {})",
                    v2 as i64 - *v1 as i64
                );
            }
        }
        println!(
            "in-process jitter: {jittered} of {} counters differ (issue #4878)",
            m1.len()
        );
    }

    // Matrix 11: median_report is element-wise per-counter MEDIAN, not mean/min/max.
    #[test]
    fn median_report_is_per_counter_median() {
        let samples = [
            mk_report(&[("c", 10), ("sib", 1)]),
            mk_report(&[("c", 1000), ("sib", 100)]),
            mk_report(&[("c", 20), ("sib", 50)]),
        ];
        let median = median_report(&samples);
        let c = *median.counters.0.get("c").unwrap();
        assert_eq!(c, 20); // median of [10, 20, 1000]
        assert_ne!(c, 343); // mean
        assert_ne!(c, 10); // min
        assert_ne!(c, 1000); // max
                             // Sibling counter medians independently: median of [1, 50, 100] = 50.
        assert_eq!(*median.counters.0.get("sib").unwrap(), 50);
    }

    // Matrix 12: K=1 median is the identity.
    #[test]
    fn median_report_k1_is_identity() {
        let sample = mk_report(&[("a", 42), ("b", 7)]);
        let median = median_report(std::slice::from_ref(&sample));
        assert_eq!(median.counters, sample.counters);
        assert_eq!(median.base_seed, sample.base_seed);
        assert_eq!(median.action_cap, sample.action_cap);
        assert_eq!(median.sample_count, 1);
    }

    // Matrix 13: median inherits/pins the workload; sample_count == samples.len().
    #[test]
    fn median_report_inherits_workload() {
        let samples = [
            mk_report(&[("c", 1)]),
            mk_report(&[("c", 2)]),
            mk_report(&[("c", 3)]),
        ];
        let median = median_report(&samples);
        assert_eq!(median.base_seed, PERF_BASE_SEED);
        assert_eq!(median.action_cap, PERF_ACTION_CAP);
        assert_eq!(median.schema_version, PERF_SCHEMA_VERSION);
        assert_eq!(
            median.scenarios,
            default_scenarios()
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(median.sample_count, 3);
    }

    // Matrix 13 (hostile): disagreeing samples are a hard internal error.
    #[test]
    #[should_panic(expected = "sample seed mismatch")]
    fn median_report_panics_on_disagreeing_seed() {
        let mut odd = mk_report(&[("c", 1)]);
        odd.base_seed = PERF_BASE_SEED + 1;
        let samples = [mk_report(&[("c", 1)]), odd];
        let _ = median_report(&samples);
    }

    // Matrix 14: the extended workload guard rejects a K mismatch (K binding is
    // ENFORCED, not assumed). Revert-failing on the new sample_count clause (§3.1g).
    #[test]
    fn workload_guard_rejects_sample_count_mismatch() {
        let baseline = mk_report(&[("c", 1)]); // sample_count = PERF_SAMPLE_COUNT (5)
        let mut current = mk_report(&[("c", 1)]);
        current.sample_count = 1; // K=1 current vs K=5 baseline
        let err = compare(&baseline, &current).unwrap_err();
        assert!(matches!(
            err,
            PerfCompareError::WorkloadMismatch {
                field: "sample_count",
                ..
            }
        ));
    }

    // Matrix M-even: median totality for even K — deterministic upper-middle at
    // index len/2, no panic, no fractional value. Guards the index against
    // off-by-one even though K is pinned odd.
    #[test]
    fn median_report_even_k_upper_middle() {
        let samples = [
            mk_report(&[("c", 1)]),
            mk_report(&[("c", 2)]),
            mk_report(&[("c", 3)]),
            mk_report(&[("c", 4)]),
        ];
        let median = median_report(&samples);
        assert_eq!(*median.counters.0.get("c").unwrap(), 3); // index 4/2 = 2 → sorted[2] = 3
        assert_eq!(median.sample_count, 4);
    }

    // Matrix M-margin (GAP 1): repro_margin_report marks a counter OVER-MARGIN iff
    // its worst observed current exceeds the 50%-headroom midpoint, reusing
    // fail_threshold. baseline c=100 ⇒ threshold = 100*1.05+64 = 169, headroom = 69,
    // margin_ceiling = 100 + 0.5*69 = 134 (0.5*69 = 34.5, floored to 34). Worst 134
    // ⇒ within; worst 135 ⇒ over. Revert-failing on both PERF_REPRO_MARGIN_FRACTION
    // (fraction 1.0 makes 135 within) and the reuse of fail_threshold.
    #[test]
    fn repro_margin_boundary_134_within_135_over() {
        let baseline = mk_report(&[("c", 100)]);

        let within = [mk_report(&[("c", 134)])];
        let report = repro_margin_report(&baseline, &within);
        let row = &report.rows[0];
        assert_eq!(row.threshold, 169);
        assert_eq!(row.margin_ceiling, 134);
        assert!(row.within_margin);
        assert!(report.all_within_margin());

        let over = [mk_report(&[("c", 135)])];
        let report = repro_margin_report(&baseline, &over);
        assert!(!report.rows[0].within_margin);
        assert!(!report.all_within_margin());
    }

    // Matrix M-margin (sibling): a within-margin sibling must NOT flip
    // all_within_margin() — proves the "any over-margin fails" reduction; and
    // worst_current is the element-wise MAX across runs.
    #[test]
    fn repro_margin_worst_is_max_and_any_over_fails() {
        let baseline = mk_report(&[("hot", 100), ("cold", 100)]);
        // `hot` peaks at 135 (over) in run 2; `cold` stays at 110 (within) always.
        let runs = [
            mk_report(&[("hot", 101), ("cold", 110)]),
            mk_report(&[("hot", 135), ("cold", 105)]),
        ];
        let report = repro_margin_report(&baseline, &runs);
        let hot = report.rows.iter().find(|r| r.key == "hot").unwrap();
        let cold = report.rows.iter().find(|r| r.key == "cold").unwrap();
        assert_eq!(hot.worst_current, 135); // element-wise MAX
        assert!(!hot.within_margin);
        assert_eq!(cold.worst_current, 110);
        assert!(cold.within_margin);
        // A single over-margin counter fails the whole report.
        assert!(!report.all_within_margin());
    }

    // Matrix M-margin (hostile): a run missing a baseline counter key panics
    // loudly — no silent skip that would hide a dropped counter.
    #[test]
    #[should_panic(expected = "missing counter")]
    fn repro_margin_panics_on_missing_counter() {
        let baseline = mk_report(&[("present", 100), ("dropped", 100)]);
        let runs = [mk_report(&[("present", 101)])];
        let _ = repro_margin_report(&baseline, &runs);
    }

    // Matrix M-margin (hostile): zero validation runs panics loudly — otherwise
    // every `worst_current` defaults to `base` and the table vacuously passes.
    #[test]
    #[should_panic(expected = "at least one validation run")]
    fn repro_margin_panics_on_empty_runs() {
        let baseline = mk_report(&[("counter", 100)]);
        let _ = repro_margin_report(&baseline, &[]);
    }
}
