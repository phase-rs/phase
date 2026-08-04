//! Suite runner — executes every registered `MatchupSpec` and emits a
//! structured JSON report.
//!
//! Deterministic-core results are a function of `(binary, spec, seed)` **modulo
//! the run-to-run caveats catalogued at the top of [`super::perf`]** — do not read
//! this as byte-stability across repeated runs. Wall-clock fields are retained in
//! `SuiteReport` for operator visibility but are excluded from
//! [`SuiteReport::deterministic_core`].

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
                // schema 2 added the `mana_development_offset` control column as
                // a SELF-ONLY absolute count; schema 3 keeps the column name and
                // changes its semantics to a signed self-minus-opponent
                // DIFFERENTIAL (Unit 5).
                //
                // The trainer accepts only schema 3+ shards, preventing it from
                // pooling this signed differential with schema 2's absolute count.
                schema: 3,
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

    run_games_parallel(db, options, &selected)
}

/// Flat `(selection position, game index)` work list for the game-level runner.
///
/// A matchup whose decks failed to resolve contributes **no** tasks: it already
/// has a `failed_result`, and enqueuing games for it would run `drive_game` on a
/// payload that does not exist. Positions stay aligned with `payloads`/`selected`
/// so a task can recover its spec, payload and matchup seed by index alone.
fn game_tasks(
    payloads: &[Result<DeckPayload, String>],
    games_per_matchup: usize,
) -> Vec<(usize, usize)> {
    payloads
        .iter()
        .enumerate()
        .filter(|(_, payload)| payload.is_ok())
        .flat_map(|(pos, _)| (0..games_per_matchup).map(move |game| (pos, game)))
        .collect()
}

/// Regroup finished games by matchup position, restoring `game_idx` order and
/// summing per-matchup duration.
///
/// The sort is load-bearing, not cosmetic: `MatchupResult::games` is a field of
/// [`DeterministicSuiteReport`], so games arriving in completion order instead of
/// `game_idx` order would be a real baseline diff on every parallel run.
fn regroup_games(
    matchup_count: usize,
    mut collected: Vec<(usize, usize, GameResult, u128)>,
) -> Vec<(Vec<GameResult>, u128)> {
    // `(pos, game_idx)` pairs are unique, so `game_idx` alone would also be correct
    // once the bucketing below splits by `pos`. Keying on both is self-documenting:
    // it says the output order is "matchup, then game", which is what the report is.
    collected.sort_by_key(|(pos, game_idx, _, _)| (*pos, *game_idx));
    let mut per_matchup: Vec<(Vec<GameResult>, u128)> =
        (0..matchup_count).map(|_| (Vec::new(), 0)).collect();
    for (pos, _, game, elapsed_ms) in collected {
        let entry = &mut per_matchup[pos];
        entry.0.push(game);
        entry.1 += elapsed_ms;
    }
    per_matchup
}

/// Run the selected matchups across all available cores via a work-stealing
/// atomic cursor over **(matchup, game) pairs**.
///
/// The unit of work is one game, not one matchup. Capping the cursor at the
/// matchup count left every core past the third idle on the quick gate
/// (`ai_gate.rs`'s `DEFAULT_QUICK_FILTER` selects three matchups) while the
/// ten games inside each matchup ran strictly one after another. A game is a
/// function of `(payload, seed, difficulty, action_cap)` **modulo #4878** — see
/// [`drive_game`] and the run-to-run caveat at the top of `duel_suite::perf` —
/// and its seed is `base_seed + matchup_idx*1000 + game_idx`, derived from the
/// matchup's ORIGINAL index, so no game's result depends on which worker picked
/// it up or when. Only live progress order varies.
///
/// Results are regrouped by matchup and re-sorted by `game_idx` before
/// aggregation, so this runner adds no *new* scheduling dependence to
/// `SuiteReport::deterministic_core`. That core is not byte-stable across repeat
/// runs and never was: `RandomState` is seeded per thread from OS randomness and
/// that iteration order reaches AI tie-breaking (#4878). The verdict is
/// insulated from it because `compare::paired_seed_shift` keys on `seed`, not on
/// position.
///
/// Output contract (changed by this restructuring): per-GAME lines stream live
/// from [`play_reported_game`], interleaved across matchups and each tagged with
/// its matchup id; the per-matchup verdict rows are printed after the join, in
/// selection order. A killed run therefore still leaves every completed game's
/// seed, winner, turn count and duration in the log.
fn run_games_parallel(
    db: &CardDatabase,
    options: &SuiteOptions,
    selected: &[(usize, &MatchupSpec)],
) -> Vec<MatchupResult> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Payloads are resolved ONCE per matchup, on this thread, before the fan-out:
    // deck resolution is pure setup, and re-resolving it per game would add real
    // work that the sequential runner never did. A matchup whose decks fail to
    // resolve contributes no game tasks and keeps its `failed_result` — one bad
    // deck list must not abort the other matchups.
    let mut payloads: Vec<Result<DeckPayload, String>> = Vec::with_capacity(selected.len());
    for (_, spec) in selected {
        payloads.push(build_payload(db, spec));
    }

    let tasks = game_tasks(&payloads, options.games_per_matchup);
    let task_total = tasks.len();
    let n_workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(task_total.max(1));
    let cursor = AtomicUsize::new(0);

    let collected: Vec<(usize, usize, GameResult, u128)> = std::thread::scope(|scope| {
        let payloads = &payloads;
        let tasks = &tasks;
        let cursor = &cursor;
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
                    .spawn_scoped(scope, move || {
                        let mut local: Vec<(usize, usize, GameResult, u128)> = Vec::new();
                        loop {
                            let next = cursor.fetch_add(1, Ordering::Relaxed);
                            if next >= task_total {
                                break;
                            }
                            let (pos, game_idx) = tasks[next];
                            let (idx, spec) = selected[pos];
                            let payload = payloads[pos]
                                .as_ref()
                                .expect("only Ok payloads produce game tasks");
                            let matchup_seed = options.base_seed.wrapping_add(idx as u64 * 1_000);
                            // Parallel path never harvests (harvesting forces the
                            // sequential branch in `run_all_matchups`).
                            let (game, elapsed_ms) = play_reported_game(
                                spec,
                                payload,
                                options,
                                matchup_seed,
                                game_idx,
                                None,
                            );
                            local.push((pos, game_idx, game, elapsed_ms));
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

    let per_matchup = regroup_games(selected.len(), collected);

    let results: Vec<MatchupResult> = selected
        .iter()
        .zip(payloads.iter())
        .zip(per_matchup)
        .map(
            |(((_, spec), payload), (games, total_duration_ms))| match payload {
                Err(reason) => failed_result(spec, reason),
                Ok(_) => assemble_matchup_result(spec, options, games, total_duration_ms),
            },
        )
        .collect();

    let run_total = results.len();
    for (n, result) in results.iter().enumerate() {
        eprintln!(
            "[{n:>2}/{run_total}] {id}  done (games: {games})",
            n = n + 1,
            id = result.matchup_id,
            games = options.games_per_matchup,
        );
        print_matchup_row(result);
    }

    results
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

/// Wall-clock `HH:MM:SS` in UTC for progress lines.
///
/// The suite has no date dependency (`phase-ai/Cargo.toml` pulls neither `chrono`
/// nor `time`), and a bare elapsed counter is useless once a CI job is killed —
/// the operator needs to line the last emitted game up against the job's own
/// timeline. Seconds-of-day arithmetic is enough for that and cannot drift.
/// Diagnostics only: never parsed, never compared, never part of a verdict.
fn utc_hms() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Progress-line label for a finished game's winner. `None` is a draw *or* an
/// aborted game — [`play_reported_game`] prints a distinct `aborted:` line on the
/// panic path, so the two stay distinguishable in a killed run's tail.
fn winner_label(winner: Option<PlayerId>) -> &'static str {
    match winner {
        Some(PlayerId(0)) => "p0",
        Some(_) => "p1",
        None => "draw",
    }
}

/// Play one suite game and report it. **Sole authority** for what a single game
/// is: the seed derivation, the panic guard, the harvest side-channel and the
/// progress lines all live here, so the sequential runner and the parallel
/// game-level runner cannot drift in `(seed, winner, turns)`.
///
/// The returned `GameResult` is derived from `(payload, matchup_seed, game_idx,
/// difficulty)` alone: no wall clock this function reads (`Instant::now` for
/// `elapsed_ms`, `utc_hms` for the progress lines) enters it, and neither does
/// completion order or worker index. It is NOT thread-identity-free in the
/// absolute sense —
/// `RandomState` is seeded per thread and leaks into AI tie-breaking (#4878,
/// documented at the top of `duel_suite::perf`) — but that exposure is identical
/// under the sequential and the parallel runner. `elapsed_ms` is the only value
/// this function itself makes scheduling-dependent, and it is excluded from
/// [`SuiteReport::deterministic_core`].
fn play_reported_game(
    spec: &MatchupSpec,
    payload: &DeckPayload,
    options: &SuiteOptions,
    matchup_seed: u64,
    game_idx: usize,
    harvest_sink: Option<&mut HarvestSink>,
) -> (GameResult, u128) {
    let seed = matchup_seed.wrapping_add(game_idx as u64);
    let start = Instant::now();
    eprintln!(
        "{ts} [{id}] game {n}/{total} seed={seed} start",
        ts = utc_hms(),
        id = spec.id,
        n = game_idx + 1,
        total = options.games_per_matchup,
    );
    let (winner, turns) = if harvest_sink.is_some() {
        // Harvester declared OUTSIDE catch_unwind. The observe closure's `&mut`
        // borrow ends when the closure returns; `finish(winner)` then runs
        // unconditionally (panic → `catch_unwind` Err → winner None → empty
        // records, partial buffer dropped with the harvester).
        let mut harvester = harvest::GameHarvester::new(seed, spec.id.to_string(), game_idx);
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            run_game_observed(payload, seed, options.difficulty, &mut |state, session| {
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
        if let Some(sink) = harvest_sink {
            if let Err(e) = sink.write_records(&records) {
                eprintln!("       seed {seed}: harvest write failed: {e}");
            }
        }
        (winner, turns)
    } else {
        match std::panic::catch_unwind(AssertUnwindSafe(|| {
            run_game(payload, seed, options.difficulty)
        })) {
            Ok(result) => result,
            Err(_) => {
                eprintln!("       seed {seed} aborted: AI panic during suite game");
                (None, 0)
            }
        }
    };
    let elapsed_ms = start.elapsed().as_millis();
    eprintln!(
        "{ts} [{id}] game {n}/{total} seed={seed} done winner={winner} turns={turns} {elapsed_ms}ms",
        ts = utc_hms(),
        id = spec.id,
        n = game_idx + 1,
        total = options.games_per_matchup,
        winner = winner_label(winner),
    );
    (
        GameResult {
            seed,
            winner: winner.map(|p| p.0),
            turns,
        },
        elapsed_ms,
    )
}

/// Fold a matchup's finished games into its [`MatchupResult`]. **Sole authority**
/// for the win/draw tally and the [`classify`] verdict, shared by both runners.
///
/// `games` MUST already be in `game_idx` order — the parallel runner sorts them
/// back before calling. The report field `games` is part of
/// [`SuiteReport::deterministic_core`], so any other order would be a visible
/// baseline diff, not a cosmetic one.
fn assemble_matchup_result(
    spec: &MatchupSpec,
    options: &SuiteOptions,
    games: Vec<GameResult>,
    total_duration_ms: u128,
) -> MatchupResult {
    let mut p0_wins = 0usize;
    let mut p1_wins = 0usize;
    let mut draws = 0usize;
    let mut total_turns: u64 = 0;
    for game in &games {
        total_turns += game.turns as u64;
        match game.winner {
            Some(0) => p0_wins += 1,
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

    let mut games = Vec::with_capacity(options.games_per_matchup);
    let mut total_duration_ms: u128 = 0;
    for game_idx in 0..options.games_per_matchup {
        let (game, elapsed_ms) = play_reported_game(
            spec,
            &payload,
            options,
            matchup_seed,
            game_idx,
            harvest_sink.as_deref_mut(),
        );
        total_duration_ms += elapsed_ms;
        games.push(game);
    }

    assemble_matchup_result(spec, options, games, total_duration_ms)
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
/// result `(winner, turn_number)` is a function of
/// `(binary, payload, seed, difficulty, action_cap)` and nothing this function
/// itself reads — but it is NOT wall-clock-free further down. `projection.rs`'s
/// `TIME_CAP` (`projection.rs:110`, 15 ms) is not gated on measurement mode and is
/// reached at `AiDifficulty::Medium` through `EvasionRemovalPriorityPolicy`'s
/// `velocity_score`, so a faster or slower host can change which creature the AI
/// targets. See the run-to-run caveats at the top of [`super::perf`]; this is a
/// second source alongside #4878.
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

    fn game(seed: u64, winner: Option<u8>, turns: u32) -> GameResult {
        GameResult {
            seed,
            winner,
            turns,
        }
    }

    /// A matchup whose decks failed to resolve must enqueue ZERO games — it
    /// already carries a `failed_result`, and its `payloads` slot holds no
    /// `DeckPayload` for a worker to run.
    ///
    /// Discriminating: dropping the `payload.is_ok()` filter makes position 1
    /// appear in the output and the `expect("only Ok payloads produce game
    /// tasks")` in the worker becomes reachable. Verified by deleting the
    /// filter — the equality below then sees the extra `(1, 0)`/`(1, 1)` pair.
    #[test]
    fn game_tasks_skip_matchups_whose_payload_failed() {
        let payloads = vec![
            Ok(DeckPayload::default()),
            Err("p0 load: no such deck".to_string()),
            Ok(DeckPayload::default()),
        ];

        let tasks = game_tasks(&payloads, 2);

        assert_eq!(tasks, vec![(0, 0), (0, 1), (2, 0), (2, 1)]);
    }

    /// Every matchup's games must land in `game_idx` order no matter what order
    /// the workers finished them in. `MatchupResult::games` is a
    /// `deterministic_core` field, so completion order leaking into it would be a
    /// baseline diff on every parallel run.
    ///
    /// Discriminating by construction: the input is deliberately shuffled
    /// (matchup 0's games arrive 2,0,1 and are interleaved with matchup 1's), so
    /// removing `regroup_games`' `sort_by_key` yields `[2,0,1]` and the first
    /// assertion fails. Verified by deleting the sort.
    #[test]
    fn regroup_games_restores_game_index_order_from_shuffled_completion() {
        let collected = vec![
            (0, 2, game(102, Some(1), 12), 30),
            (1, 1, game(1101, None, 21), 100),
            (0, 0, game(100, Some(0), 10), 10),
            (1, 0, game(1100, Some(0), 20), 200),
            (0, 1, game(101, Some(0), 11), 20),
        ];

        let per_matchup = regroup_games(2, collected);

        assert_eq!(
            per_matchup[0].0,
            vec![
                game(100, Some(0), 10),
                game(101, Some(0), 11),
                game(102, Some(1), 12)
            ],
        );
        assert_eq!(
            per_matchup[1].0,
            vec![game(1100, Some(0), 20), game(1101, None, 21)],
        );
        // Durations are summed per matchup, not mixed between them.
        assert_eq!(per_matchup[0].1, 60);
        assert_eq!(per_matchup[1].1, 300);
    }

    /// A matchup that produced no games at all (its payload failed) must still
    /// get an empty slot rather than shifting later matchups' games onto it.
    #[test]
    fn regroup_games_keeps_an_empty_slot_for_a_matchup_with_no_games() {
        let collected = vec![(2, 0, game(2100, Some(0), 7), 5)];

        let per_matchup = regroup_games(3, collected);

        assert!(per_matchup[0].0.is_empty());
        assert!(per_matchup[1].0.is_empty());
        assert_eq!(per_matchup[2].0, vec![game(2100, Some(0), 7)]);
    }

    /// `assemble_matchup_result` is the sole authority for BOTH halves of a
    /// matchup row: the win/draw tally (read off the games vector) and the
    /// [`classify`] verdict plus the two averages (divided by
    /// `options.games_per_matchup`, NOT by `games.len()`).
    ///
    /// The 4-games-against-`games_per_matchup: 16` mismatch is deliberate — it is
    /// what makes the denominator observable. Every one of the three uses flips
    /// if it is switched to `games.len()`: `avg_turns` 1.625 → 6.5,
    /// `avg_duration_ms` 25.0 → 100.0, and the verdict PASS-vs-FAIL, because the
    /// Wilson 95% interval for 2/16 excludes 0.50 while the interval for 2/4
    /// (which is centred on 0.50) cannot. A real run always has
    /// `games.len() == games_per_matchup`, so the two are indistinguishable there.
    #[test]
    fn assemble_matchup_result_tallies_from_games_and_divides_by_games_per_matchup() {
        let spec = crate::duel_suite::find_matchup("red-mirror").expect("red-mirror must resolve");
        let options = SuiteOptions::new(AiDifficulty::Medium, 16, 7);
        let games = vec![
            game(7, Some(0), 5),
            game(8, Some(1), 6),
            game(9, None, 7),
            game(10, Some(0), 8),
        ];

        let result = assemble_matchup_result(spec, &options, games.clone(), 400);

        assert_eq!((result.p0_wins, result.p1_wins, result.draws), (2, 1, 1));
        assert_eq!(result.total_turns, 26);
        assert_eq!(result.games, games);
        assert_eq!(result.total_duration_ms, 400);
        // Denominator is `games_per_matchup` (16), not `games.len()` (4).
        assert_eq!(result.avg_turns, 1.625);
        assert_eq!(result.avg_duration_ms, 25.0);
        assert_eq!(result.status, SuiteStatus::Fail);
        assert!(
            result
                .fail_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("mirror imbalance")),
            "classify must be fed games_per_matchup; got {:?}",
            result.fail_reason,
        );
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
