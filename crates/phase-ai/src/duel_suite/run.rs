//! Suite runner — executes every registered `MatchupSpec` and emits a
//! structured JSON report.
//!
//! Deterministic-core results are a pure function of `(binary, spec, seed)`.
//! Wall-clock fields are retained in `SuiteReport` for operator visibility but
//! are excluded from [`SuiteReport::deterministic_core`].

use std::collections::{HashMap, HashSet};
use std::io::BufWriter;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use engine::database::CardDatabase;
use engine::game::deck_loading::{
    load_deck_into_state, resolve_deck_list, DeckList, DeckPayload, PlayerDeckList,
};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;
use rand::rngs::StdRng;
use rand::SeedableRng;
use serde::{Deserialize, Serialize};
use tracing_subscriber::layer::SubscriberExt;

use crate::auto_play::run_ai_actions;
use crate::config::{create_config_for_players, AiConfig, AiDifficulty, Platform};

use super::attribution::{aggregate_events, CaptureLayer, MatchupAttribution};
use super::harvest::{self, HarvestSink};
use super::{all_matchups, resolve_deck_ref, Expected, FeatureKind, MatchupSpec};

/// Safety cap on total AI actions per game — matches the constant in
/// `bin/ai_duel.rs` so suite games and single-matchup games terminate
/// identically.
const MAX_TOTAL_ACTIONS: usize = 10_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SuiteStatus {
    Pass,
    Fail,
    Open,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameResult {
    pub seed: u64,
    pub winner: Option<u8>,
    pub turns: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchupResult {
    pub matchup_id: String,
    pub exercises: Vec<FeatureKind>,
    pub p0_label: String,
    pub p1_label: String,
    pub expected: Expected,
    pub p0_wins: usize,
    pub p1_wins: usize,
    pub draws: usize,
    pub games: Vec<GameResult>,
    pub total_turns: u64,
    pub total_duration_ms: u128,
    pub avg_turns: f64,
    pub avg_duration_ms: f64,
    pub status: SuiteStatus,
    pub fail_reason: Option<String>,
    /// Per-player policy attribution, populated when
    /// `phase_ai::decision_trace` tracing is enabled during the suite run.
    /// Absent from the JSON when tracing is off (zero overhead path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<MatchupAttribution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteReport {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_data_hash: Option<String>,
    pub unix_timestamp_secs: i64,
    pub difficulty: String,
    pub games_per_matchup: usize,
    pub base_seed: u64,
    pub results: Vec<MatchupResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeterministicMatchupResult {
    pub matchup_id: String,
    pub exercises: Vec<FeatureKind>,
    pub p0_label: String,
    pub p1_label: String,
    pub expected: Expected,
    pub p0_wins: usize,
    pub p1_wins: usize,
    pub draws: usize,
    pub games: Vec<GameResult>,
    pub total_turns: u64,
    pub avg_turns: f64,
    pub status: SuiteStatus,
    pub fail_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeterministicSuiteReport {
    pub schema_version: u32,
    pub git_sha: Option<String>,
    pub card_data_hash: Option<String>,
    pub difficulty: String,
    pub games_per_matchup: usize,
    pub base_seed: u64,
    pub results: Vec<DeterministicMatchupResult>,
}

impl SuiteReport {
    pub fn deterministic_core(&self) -> DeterministicSuiteReport {
        DeterministicSuiteReport {
            schema_version: self.schema_version,
            git_sha: self.git_sha.clone(),
            card_data_hash: self.card_data_hash.clone(),
            difficulty: self.difficulty.clone(),
            games_per_matchup: self.games_per_matchup,
            base_seed: self.base_seed,
            results: self
                .results
                .iter()
                .map(|result| DeterministicMatchupResult {
                    matchup_id: result.matchup_id.clone(),
                    exercises: result.exercises.clone(),
                    p0_label: result.p0_label.clone(),
                    p1_label: result.p1_label.clone(),
                    expected: result.expected,
                    p0_wins: result.p0_wins,
                    p1_wins: result.p1_wins,
                    draws: result.draws,
                    games: result.games.clone(),
                    total_turns: result.total_turns,
                    avg_turns: result.avg_turns,
                    status: result.status,
                    fail_reason: result.fail_reason.clone(),
                })
                .collect(),
        }
    }
}

/// Controls decision-trace attribution capture during a suite run. When set
/// to `Enabled`, the runner installs a `CaptureLayer` subscriber with an env
/// filter that enables `phase_ai::decision_trace=debug`. When `Disabled`,
/// no subscriber is installed and the tactical search incurs zero overhead
/// (gated on `tracing::event_enabled!`).
#[derive(Debug, Clone, Copy)]
pub enum AttributionMode {
    Disabled,
    Enabled,
}

#[derive(Debug)]
pub struct SuiteOptions {
    pub difficulty: AiDifficulty,
    pub games_per_matchup: usize,
    pub base_seed: u64,
    pub output_path: PathBuf,
    /// Comma-separated list of id substrings; a matchup is run if its id
    /// contains *any* of them (e.g. `"red-mirror,affinity-mirror"` runs both).
    /// `None` runs every matchup. A single substring keeps the legacy behavior.
    pub filter: Option<String>,
    pub attribution: AttributionMode,
    pub git_sha: Option<String>,
    pub card_data_hash: Option<String>,
    /// When set, harvest per-turn eval features to this JSONL path (Texel retrain
    /// corpus). Like attribution, harvesting forces the sequential branch so a
    /// single `HarvestSink` owns the file.
    pub harvest_output: Option<PathBuf>,
}

impl SuiteOptions {
    pub fn new(difficulty: AiDifficulty, games_per_matchup: usize, base_seed: u64) -> Self {
        Self {
            difficulty,
            games_per_matchup,
            base_seed,
            output_path: PathBuf::from("target/duel-suite-results.json"),
            filter: None,
            attribution: AttributionMode::Disabled,
            git_sha: None,
            card_data_hash: None,
            harvest_output: None,
        }
    }
}

/// Run every registered matchup, write the report to `options.output_path`,
/// and return the in-memory report for the caller to print.
pub fn run_suite(db: &CardDatabase, options: &SuiteOptions) -> Result<SuiteReport, std::io::Error> {
    let capture = match options.attribution {
        AttributionMode::Enabled => Some(CaptureLayer::new()),
        AttributionMode::Disabled => None,
    };

    // One sink per suite run: created ONCE here, before the matchup loop, writing
    // the single file-scoped meta line at construction. `run_all_matchups` (which
    // harvesting forces onto the sequential branch) appends every game's records
    // through this same handle.
    let mut harvest_sink = match &options.harvest_output {
        Some(path) => {
            let meta = harvest::HarvestMeta {
                schema: 1,
                git_sha: options.git_sha.clone(),
                card_data_hash: options.card_data_hash.clone(),
                difficulty: format!("{:?}", options.difficulty),
            };
            Some(harvest::HarvestSink::create(path, &meta)?)
        }
        None => None,
    };

    // Install the subscriber for the duration of this call. When attribution
    // is disabled, skip subscriber installation entirely — the
    // `event_enabled!` gate inside `emit_decision_trace` short-circuits and
    // `PolicyRegistry::verdicts()` is never invoked.
    let results = if let Some(layer) = capture.as_ref() {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new("phase_ai::decision_trace=debug")
        });
        let subscriber = tracing_subscriber::registry::Registry::default()
            .with(filter)
            .with(layer.clone());
        tracing::subscriber::with_default(subscriber, || {
            run_all_matchups(db, options, capture.as_ref(), harvest_sink.as_mut())
        })
    } else {
        run_all_matchups(db, options, None, harvest_sink.as_mut())
    };

    if let Some(sink) = harvest_sink.as_mut() {
        sink.flush()?;
    }

    finalize_report(options, results)
}

/// True if a matchup `id` should run under `filter`. The filter is a
/// comma-separated list of id substrings — the matchup runs if its id contains
/// *any* of them. `None` runs every matchup; a single substring keeps the
/// legacy `contains` behavior. Empty/whitespace-only parts are ignored.
fn matchup_selected(id: &str, filter: Option<&str>) -> bool {
    filter.is_none_or(|filter| {
        filter
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .any(|part| id.contains(part))
    })
}

fn run_all_matchups(
    db: &CardDatabase,
    options: &SuiteOptions,
    capture: Option<&CaptureLayer>,
    mut harvest_sink: Option<&mut HarvestSink>,
) -> Vec<MatchupResult> {
    let matchups = all_matchups();
    let total = matchups.len();
    // Indexed selection honoring the id filter. The retained index is the
    // matchup's *original* position, which both derives its deterministic seed
    // (base_seed + idx*1000) and labels progress output.
    let selected: Vec<(usize, &MatchupSpec)> = matchups
        .iter()
        .enumerate()
        .filter(|(_, spec)| matchup_selected(spec.id, options.filter.as_deref()))
        .collect();

    // Attribution drains a process-global tracing subscriber between matchups, so
    // capture runs must stay sequential — concurrent matchups would interleave
    // their decision-trace events into the one capture layer. Harvesting joins the
    // same sequential branch: a single `HarvestSink` owns the output file and
    // parallel writers would interleave records / corrupt the append stream.
    if capture.is_some() || harvest_sink.is_some() {
        let mut results = Vec::with_capacity(selected.len());
        for (idx, spec) in &selected {
            eprintln!(
                "[{n:>2}/{total}] {id}  (games: {games})",
                n = idx + 1,
                id = spec.id,
                games = options.games_per_matchup,
            );
            // Drain any stale events captured before this matchup started.
            if let Some(layer) = capture {
                let _ = layer.drain();
            }
            let matchup_seed = options.base_seed.wrapping_add(*idx as u64 * 1_000);
            let mut result =
                run_single_matchup(db, spec, options, matchup_seed, harvest_sink.as_deref_mut());
            if let Some(layer) = capture {
                let events = layer.drain();
                result.attribution = Some(aggregate_events(&events));
            }
            print_matchup_row(&result);
            results.push(result);
        }
        return results;
    }

    run_matchups_parallel(db, options, &selected)
}

/// Run the selected matchups across all available cores via a work-stealing
/// atomic cursor (matchups vary widely in length, so a static split would leave
/// cores idle). Each matchup is a pure function of `(db, spec, options,
/// matchup_seed)` with its seed derived from the original index, so results are
/// byte-identical to a sequential run regardless of scheduling — only live
/// progress order varies. The returned Vec is restored to selection order.
fn run_matchups_parallel(
    db: &CardDatabase,
    options: &SuiteOptions,
    selected: &[(usize, &MatchupSpec)],
) -> Vec<MatchupResult> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let run_total = selected.len();
    let n_workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(run_total.max(1));
    let cursor = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);

    let mut collected: Vec<(usize, MatchupResult)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..n_workers)
            .map(|_| {
                // The plan-mandated `cargo ai-gate --difficulty hard` runs in a
                // debug + measurement build with no wall-clock bail, so the
                // determinized Hard+ search recurses deep — bounded, but deeper
                // than the default ~2MB scoped-thread stack, which overflows.
                // Give each worker a roomy 32 MiB stack. Test-harness only; zero
                // production impact.
                std::thread::Builder::new()
                    .stack_size(32 << 20)
                    .spawn_scoped(scope, || {
                        let mut local: Vec<(usize, MatchupResult)> = Vec::new();
                        loop {
                            let pos = cursor.fetch_add(1, Ordering::Relaxed);
                            if pos >= run_total {
                                break;
                            }
                            let (idx, spec) = selected[pos];
                            let matchup_seed = options.base_seed.wrapping_add(idx as u64 * 1_000);
                            // Parallel path never harvests (harvesting forces the
                            // sequential branch in `run_all_matchups`).
                            let result = run_single_matchup(db, spec, options, matchup_seed, None);
                            let completed = done.fetch_add(1, Ordering::Relaxed) + 1;
                            eprintln!(
                                "[{completed:>2}/{run_total}] {id}  done (games: {games})",
                                id = spec.id,
                                games = options.games_per_matchup,
                            );
                            print_matchup_row(&result);
                            local.push((pos, result));
                        }
                        local
                    })
                    .expect("failed to spawn suite worker thread")
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("suite worker thread panicked"))
            .collect()
    });

    // Parallel completion is unordered; restore the original selection order so
    // the report and any baseline comparison are stable across runs.
    collected.sort_by_key(|(pos, _)| *pos);
    collected.into_iter().map(|(_, result)| result).collect()
}

fn finalize_report(
    options: &SuiteOptions,
    results: Vec<MatchupResult>,
) -> Result<SuiteReport, std::io::Error> {
    let report = SuiteReport {
        schema_version: 2,
        git_sha: options.git_sha.clone(),
        card_data_hash: options.card_data_hash.clone(),
        unix_timestamp_secs: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        difficulty: format!("{:?}", options.difficulty),
        games_per_matchup: options.games_per_matchup,
        base_seed: options.base_seed,
        results,
    };

    write_report(&report, &options.output_path)?;
    print_markdown_table(&report);

    Ok(report)
}

fn run_single_matchup(
    db: &CardDatabase,
    spec: &MatchupSpec,
    options: &SuiteOptions,
    matchup_seed: u64,
    mut harvest_sink: Option<&mut HarvestSink>,
) -> MatchupResult {
    let payload = match build_payload(db, spec) {
        Ok(p) => p,
        Err(reason) => return failed_result(spec, &reason),
    };

    let mut p0_wins = 0usize;
    let mut p1_wins = 0usize;
    let mut draws = 0usize;
    let mut games = Vec::with_capacity(options.games_per_matchup);
    let mut total_turns: u64 = 0;
    let mut total_duration_ms: u128 = 0;

    for game_idx in 0..options.games_per_matchup {
        let seed = matchup_seed.wrapping_add(game_idx as u64);
        let start = Instant::now();
        let (winner, turns) = if harvest_sink.is_some() {
            // Harvester declared OUTSIDE catch_unwind. The observe closure's `&mut`
            // borrow ends when the closure returns; `finish(winner)` then runs
            // unconditionally (panic → `catch_unwind` Err → winner None → empty
            // records, partial buffer dropped with the harvester).
            let mut harvester = harvest::GameHarvester::new(seed, spec.id.to_string(), game_idx);
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
                run_game_observed(&payload, seed, options.difficulty, &mut |state, session| {
                    harvester.observe(state, session)
                })
            }));
            let (winner, turns) = match outcome {
                Ok(result) => result,
                Err(_) => {
                    eprintln!("       seed {seed} aborted: AI panic during suite game");
                    (None, 0)
                }
            };
            let records = harvester.finish(winner);
            if let Some(sink) = harvest_sink.as_deref_mut() {
                if let Err(e) = sink.write_records(&records) {
                    eprintln!("       seed {seed}: harvest write failed: {e}");
                }
            }
            (winner, turns)
        } else {
            match std::panic::catch_unwind(AssertUnwindSafe(|| {
                run_game(&payload, seed, options.difficulty)
            })) {
                Ok(result) => result,
                Err(_) => {
                    eprintln!("       seed {seed} aborted: AI panic during suite game");
                    (None, 0)
                }
            }
        };
        total_duration_ms += start.elapsed().as_millis();
        total_turns += turns as u64;
        games.push(GameResult {
            seed,
            winner: winner.map(|p| p.0),
            turns,
        });
        match winner {
            Some(PlayerId(0)) => p0_wins += 1,
            Some(_) => p1_wins += 1,
            None => draws += 1,
        }
    }

    let n = options.games_per_matchup.max(1) as f64;
    let avg_turns = total_turns as f64 / n;
    let avg_duration_ms = total_duration_ms as f64 / n;
    let (status, fail_reason) = classify(&spec.expected, p0_wins, options.games_per_matchup);

    MatchupResult {
        matchup_id: spec.id.to_string(),
        exercises: spec.exercises.to_vec(),
        p0_label: spec.p0_label.to_string(),
        p1_label: spec.p1_label.to_string(),
        expected: spec.expected,
        p0_wins,
        p1_wins,
        draws,
        games,
        total_turns,
        total_duration_ms,
        avg_turns,
        avg_duration_ms,
        status,
        fail_reason,
        attribution: None,
    }
}

fn build_payload(db: &CardDatabase, spec: &MatchupSpec) -> Result<DeckPayload, String> {
    let p0 = resolve_deck_ref(&spec.p0).map_err(|e| format!("p0 load: {e}"))?;
    let p1 = resolve_deck_ref(&spec.p1).map_err(|e| format!("p1 load: {e}"))?;
    let deck_list = DeckList {
        player: PlayerDeckList {
            main_deck: p0,
            sideboard: Vec::new(),
            commander: Vec::new(),
            ..Default::default()
        },
        opponent: PlayerDeckList {
            main_deck: p1,
            sideboard: Vec::new(),
            commander: Vec::new(),
            ..Default::default()
        },
        ..Default::default()
    };
    Ok(resolve_deck_list(db, &deck_list))
}

fn failed_result(spec: &MatchupSpec, reason: &str) -> MatchupResult {
    MatchupResult {
        matchup_id: spec.id.to_string(),
        exercises: spec.exercises.to_vec(),
        p0_label: spec.p0_label.to_string(),
        p1_label: spec.p1_label.to_string(),
        expected: spec.expected,
        p0_wins: 0,
        p1_wins: 0,
        draws: 0,
        games: Vec::new(),
        total_turns: 0,
        total_duration_ms: 0,
        avg_turns: 0.0,
        avg_duration_ms: 0.0,
        status: SuiteStatus::Fail,
        fail_reason: Some(format!("setup error: {reason}")),
        attribution: None,
    }
}

fn classify(expected: &Expected, p0_wins: usize, total: usize) -> (SuiteStatus, Option<String>) {
    if total == 0 {
        return (SuiteStatus::Open, None);
    }
    let p0_rate = p0_wins as f32 / total as f32;
    match expected {
        Expected::Open => (SuiteStatus::Open, None),
        Expected::Mirror { .. } => {
            let (low, high) = wilson_interval(p0_wins, total);
            if low <= 0.5 && 0.5 <= high {
                (SuiteStatus::Pass, None)
            } else {
                (
                    SuiteStatus::Fail,
                    Some(format!(
                        "mirror imbalance: p0={p0_rate:.2}, Wilson 95% CI [{low:.2}, {high:.2}] excludes 0.50"
                    )),
                )
            }
        }
        Expected::Triangle {
            p0_winrate_min,
            p0_winrate_max,
        } => {
            if p0_rate >= *p0_winrate_min && p0_rate <= *p0_winrate_max {
                (SuiteStatus::Pass, None)
            } else {
                (
                    SuiteStatus::Fail,
                    Some(format!(
                        "triangle out of range: p0={p0_rate:.2}, expected \
                         [{p0_winrate_min:.2}, {p0_winrate_max:.2}]"
                    )),
                )
            }
        }
    }
}

fn wilson_interval(successes: usize, total: usize) -> (f32, f32) {
    if total == 0 {
        return (0.0, 1.0);
    }

    let n = total as f32;
    let p = successes as f32 / n;
    let z = 1.959_964_f32;
    let z2 = z * z;
    let denominator = 1.0 + z2 / n;
    let center = p + z2 / (2.0 * n);
    let margin = z * ((p * (1.0 - p) + z2 / (4.0 * n)) / n).sqrt();

    (
        (center - margin) / denominator,
        (center + margin) / denominator,
    )
}

fn run_game(payload: &DeckPayload, seed: u64, difficulty: AiDifficulty) -> (Option<PlayerId>, u32) {
    drive_game(payload, seed, difficulty, MAX_TOTAL_ACTIONS)
}

/// [`run_game`]'s observing sibling — same `MAX_TOTAL_ACTIONS` cap, so harvested
/// and unharvested suite games are byte-identical in `(winner, turns)`. Keeping
/// the action cap in lockstep with `run_game` is what guarantees no drift between
/// the two paths.
fn run_game_observed(
    payload: &DeckPayload,
    seed: u64,
    difficulty: AiDifficulty,
    observe: &mut dyn FnMut(&GameState, &std::sync::Arc<crate::session::AiSession>),
) -> (Option<PlayerId>, u32) {
    drive_game_observed(payload, seed, difficulty, MAX_TOTAL_ACTIONS, observe)
}

/// Deterministic core game driver shared by the win-rate suite and the perf
/// gate. Builds the two-player state, installs measurement-mode AI configs, and
/// loops `run_ai_actions` until the action stream is empty or `action_cap` total
/// actions have been taken (checked at `run_ai_actions` batch boundaries, so the
/// realized count may overshoot the cap within a batch — identical semantics to
/// the historical `run_game` body, which capped at `MAX_TOTAL_ACTIONS`). The
/// result `(winner, turn_number)` is a pure function of
/// `(binary, payload, seed, difficulty, action_cap)`; no wall-clock or thread
/// scheduling influences it.
pub(crate) fn drive_game(
    payload: &DeckPayload,
    seed: u64,
    difficulty: AiDifficulty,
    action_cap: usize,
) -> (Option<PlayerId>, u32) {
    // Delegate with a no-op observer. The `&mut dyn FnMut` closure is called once
    // per `run_ai_actions` *batch* (a batch spans many engine applies), so at
    // measurement granularity this is perf-neutral vs the historical body — the
    // unchanged `ai-perf-gate` baseline is the witness.
    drive_game_observed(payload, seed, difficulty, action_cap, &mut |_, _| {})
}

/// [`drive_game`] with an observer seam: `observe(&state, &ai_session)` fires
/// after every `run_ai_actions` batch. The observer receives an immutable
/// `&GameState` (read-only by construction) and the per-game `AiSession` p0's
/// planner consumes. With a no-op closure the results are byte-identical to the
/// historical `drive_game` body.
pub(crate) fn drive_game_observed(
    payload: &DeckPayload,
    seed: u64,
    difficulty: AiDifficulty,
    action_cap: usize,
    observe: &mut dyn FnMut(&GameState, &std::sync::Arc<crate::session::AiSession>),
) -> (Option<PlayerId>, u32) {
    let mut state = GameState::new_two_player(seed);
    load_deck_into_state(&mut state, payload);
    engine::game::engine::start_game(&mut state);

    let ai_players: HashSet<PlayerId> = [PlayerId(0), PlayerId(1)].into_iter().collect();
    let config = create_config_for_players(difficulty, Platform::Native, 2).into_measurement(seed);
    let ai_configs: HashMap<PlayerId, AiConfig> =
        [(PlayerId(0), config.clone()), (PlayerId(1), config)]
            .into_iter()
            .collect();

    let mut total_actions: usize = 0;
    let mut ai_rng = StdRng::seed_from_u64(seed);
    let ai_session = crate::session::AiSession::arc_from_game(&state);
    loop {
        let results = run_ai_actions(
            &mut state,
            &ai_players,
            &ai_configs,
            &mut ai_rng,
            &ai_session,
        );
        observe(&state, &ai_session);
        if results.is_empty() {
            break;
        }
        total_actions += results.len();
        if total_actions >= action_cap {
            break;
        }
    }

    let winner = match &state.waiting_for {
        WaitingFor::GameOver { winner } => *winner,
        _ => None,
    };
    (winner, state.turn_number)
}

fn write_report(report: &SuiteReport, path: &Path) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(path)?;
    serde_json::to_writer_pretty(BufWriter::new(file), report).map_err(std::io::Error::other)?;
    Ok(())
}

fn print_matchup_row(r: &MatchupResult) {
    let total = r.p0_wins + r.p1_wins + r.draws;
    let p0_pct = if total > 0 {
        r.p0_wins as f64 / total as f64 * 100.0
    } else {
        0.0
    };
    let status_str = match r.status {
        SuiteStatus::Pass => "PASS",
        SuiteStatus::Fail => "FAIL",
        SuiteStatus::Open => "OPEN",
    };
    eprintln!(
        "       {status_str}  p0={:>3}/{total} ({p0_pct:.0}%)  turns={:.1}",
        r.p0_wins, r.avg_turns
    );
    if let Some(reason) = &r.fail_reason {
        eprintln!("       reason: {reason}");
    }
}

fn print_markdown_table(report: &SuiteReport) {
    let has_attribution = report.results.iter().any(|r| r.attribution.is_some());
    println!();
    if has_attribution {
        println!(
            "| matchup | exercises | p0% | avg turns | top-policy p0 | top-policy p1 | status |"
        );
        println!(
            "|---------|-----------|-----|-----------|---------------|---------------|--------|"
        );
    } else {
        println!("| matchup | exercises | p0% | avg turns | status |");
        println!("|---------|-----------|-----|-----------|--------|");
    }
    for r in &report.results {
        let total = r.p0_wins + r.p1_wins + r.draws;
        let p0_pct = if total > 0 {
            r.p0_wins as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        let exercises: Vec<String> = r.exercises.iter().map(|f| format!("{f:?}")).collect();
        let status_str = match r.status {
            SuiteStatus::Pass => "PASS",
            SuiteStatus::Fail => "FAIL",
            SuiteStatus::Open => "OPEN",
        };
        if has_attribution {
            let (p0_top, p1_top) = match &r.attribution {
                Some(a) => (format_top(&a.p0), format_top(&a.p1)),
                None => ("—".to_string(), "—".to_string()),
            };
            println!(
                "| {} | {} | {:.0}% | {:.1} | {} | {} | {} |",
                r.matchup_id,
                exercises.join(", "),
                p0_pct,
                r.avg_turns,
                p0_top,
                p1_top,
                status_str,
            );
        } else {
            println!(
                "| {} | {} | {:.0}% | {:.1} | {} |",
                r.matchup_id,
                exercises.join(", "),
                p0_pct,
                r.avg_turns,
                status_str,
            );
        }
    }
}

fn format_top(attribution: &super::attribution::PolicyAttribution) -> String {
    match attribution.top_scores.first() {
        Some(e) => format!("{}:{}={:+.2}", e.policy_id, e.kind, e.mean_delta),
        None => "—".to_string(),
    }
}

/// Utility for external callers (e.g. the binary's `--matchup` single-matchup
/// path) to resolve a `DeckRef` to a `DeckPayload`. Returns the resolved
/// payload and labels on success.
pub fn resolve_matchup(
    db: &CardDatabase,
    spec: &MatchupSpec,
) -> Result<(DeckPayload, String, String), String> {
    let payload = build_payload(db, spec)?;
    Ok((
        payload,
        spec.p0_label.to_string(),
        spec.p1_label.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duel_suite::{Expected, FeatureKind};

    #[test]
    fn filter_none_runs_every_matchup() {
        assert!(matchup_selected("red-mirror", None));
        assert!(matchup_selected("affinity-mirror", None));
    }

    #[test]
    fn filter_single_substring_is_legacy_contains() {
        assert!(matchup_selected("red-mirror", Some("red-mirror")));
        assert!(matchup_selected("red-mirror", Some("mirror")));
        assert!(!matchup_selected("affinity-mirror", Some("red-mirror")));
    }

    #[test]
    fn filter_comma_list_matches_any_part() {
        let f = Some("red-mirror,affinity-mirror");
        assert!(matchup_selected("red-mirror", f));
        assert!(matchup_selected("affinity-mirror", f));
        assert!(!matchup_selected("green-mirror", f));
        // The quick-gate set is exactly red-mirror + affinity-mirror: no other
        // mirror leaks in (guards against an accidental bare "mirror" part).
        assert!(!matchup_selected("blue-mirror", f));
    }

    #[test]
    fn filter_ignores_blank_and_whitespace_parts() {
        assert!(matchup_selected("red-mirror", Some(" red-mirror , ")));
        assert!(!matchup_selected("red-mirror", Some(",,")));
    }

    fn report_with_timing(timestamp: i64, duration_ms: u128) -> SuiteReport {
        SuiteReport {
            schema_version: 2,
            git_sha: None,
            card_data_hash: None,
            unix_timestamp_secs: timestamp,
            difficulty: "Medium".to_string(),
            games_per_matchup: 1,
            base_seed: 99,
            results: vec![MatchupResult {
                matchup_id: "red-mirror".to_string(),
                exercises: vec![FeatureKind::AggroPressure],
                p0_label: "Red Aggro".to_string(),
                p1_label: "Red Aggro".to_string(),
                expected: Expected::Mirror { tolerance: 0.15 },
                p0_wins: 1,
                p1_wins: 0,
                draws: 0,
                games: vec![GameResult {
                    seed: 99,
                    winner: Some(0),
                    turns: 7,
                }],
                total_turns: 7,
                total_duration_ms: duration_ms,
                avg_turns: 7.0,
                avg_duration_ms: duration_ms as f64,
                status: SuiteStatus::Pass,
                fail_reason: None,
                attribution: None,
            }],
        }
    }

    #[test]
    fn deterministic_core_excludes_wall_clock_fields() {
        let first = report_with_timing(1, 100);
        let second = report_with_timing(2, 200);

        assert_eq!(first.deterministic_core(), second.deterministic_core());
    }

    #[test]
    fn mirror_classification_uses_wilson_interval() {
        let (status, reason) = classify(&Expected::Mirror { tolerance: 0.15 }, 8, 10);

        assert_eq!(status, SuiteStatus::Pass);
        assert!(reason.is_none());
    }

    #[test]
    fn mirror_classification_fails_when_wilson_excludes_half() {
        let (status, reason) = classify(&Expected::Mirror { tolerance: 0.15 }, 90, 100);

        assert_eq!(status, SuiteStatus::Fail);
        assert!(reason.unwrap().contains("Wilson 95% CI"));
    }

    /// Observer seam is inert: for the same `(payload, seed)`, a no-op observer
    /// yields an identical `(winner, turns)` to the un-observed driver. Uses an
    /// empty `DeckPayload` (both libraries empty → deterministic draw-from-empty
    /// loss), so the test needs no card database and runs in CI.
    ///
    /// Scope caveat: `drive_game` IS `drive_game_observed` with a no-op closure,
    /// so both calls execute the same code — this catches nondeterminism in the
    /// shared driver, not observer-induced drift (impossible by construction:
    /// the observer receives only `&GameState`). The load-bearing inertness
    /// evidence is the unchanged duel-suite win-rate baseline with harvest off.
    #[test]
    fn observer_seam_is_inert_for_noop_observer() {
        let payload = DeckPayload::default();
        let seed = 4242;
        let baseline = drive_game(&payload, seed, AiDifficulty::Easy, 200);
        let observed = drive_game_observed(&payload, seed, AiDifficulty::Easy, 200, &mut |_, _| {});
        assert_eq!(
            baseline, observed,
            "no-op observer must not perturb (winner, turns)"
        );
    }
}
