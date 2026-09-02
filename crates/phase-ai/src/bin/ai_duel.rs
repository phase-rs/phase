// pod-lab loop-3 Q5: native-binary throughput lever, gated in Cargo.toml so
// wasm32 builds of this crate's lib (pulled in by engine-wasm/draft-wasm)
// never see it.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use engine::database::CardDatabase;
use engine::game::deck_loading::{
    load_deck_into_state, resolve_deck_list, DeckList, DeckPayload, PlayerDeckList,
    PlayerDeckPayload,
};
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::log::{GameLogEntry, LogCategory, LogSegment};
use engine::types::player::PlayerId;
use phase_ai::auto_play::{run_ai_actions, run_ai_actions_bounded};
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
use phase_ai::duel_suite::compare::{
    compare as compare_reports, emit_gate_verdict, load_report, render_error_markdown,
    CompareOptions,
};
use phase_ai::duel_suite::run::{
    resolve_matchup, run_suite, AttributionMode, ReportSink, SuiteOptions,
};
use phase_ai::duel_suite::{all_matchups, find_matchup};
use rand::rngs::StdRng;
use rand::SeedableRng;

const MAX_TOTAL_ACTIONS: usize = 10_000;
const COMMANDER_MAX_TOTAL_ACTIONS: usize = 200_000;

/// Seats in a `--commander-suite` rotation. The suite measures one candidate
/// against three baselines, so its seat count is fixed where the duel's is not.
const COMMANDER_SUITE_SEATS: u8 = 4;

/// Per-game wall budget for a 1v1 run when `--game-timeout` is not given.
const DEFAULT_GAME_TIMEOUT: Duration = Duration::from_secs(300);

/// Actions a duel game may take without the turn number changing before the
/// driver calls it wedged. A real Commander turn is tens of actions; four
/// hundred times that on one turn is a loop, not a slow turn.
const DUEL_STALL_ACTIONS: usize = 40_000;

/// Seats in a 1v1 Commander run. `FormatConfig::commander()` declares
/// `min_players: 2`, so two seats is a configuration the engine already supports.
const DUEL_SEATS: u8 = 2;

/// Actions per driver step. The budget is re-checked between steps, so this is
/// how far a game can overrun its wall budget: `run_ai_actions` on its own takes
/// up to `MAX_AI_ACTIONS_PER_SEQUENCE` (200) actions before it returns, and a
/// single deep-search action can be seconds, so an unbounded batch cannot honour
/// a timeout at all.
const DRIVER_STEP_ACTIONS: usize = 16;

/// The step must be positive (`run_ai_actions_bounded` takes no actor at 0) and
/// strictly smaller than `auto_play`'s own `MAX_AI_ACTIONS_PER_SEQUENCE` (200),
/// which is where a batch would otherwise be clamped -- at that size the budget
/// check between steps buys nothing. Enforced at compile time because the bound
/// is a property of the constant, not of any run.
const _: () = assert!(DRIVER_STEP_ACTIONS > 0 && DRIVER_STEP_ACTIONS < 200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Single,
    Suite,
    CommanderSuite,
    /// Head-to-head Commander between two named decks from a feed.
    ///
    /// `FormatConfig::commander()` declares `min_players: 2`, so a two-seat
    /// Commander game is a legal configuration the engine already supports
    /// (see the 2-player commander states in `mulligan.rs` / `visibility.rs`);
    /// only this binary's Commander path previously hardcoded four seats.
    CommanderDuel,
    /// List the registered single-matchup specs. Needs no card database, so it
    /// is answered before the database load rather than in the dispatch below.
    ListMatchups,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // `compare` subcommand: `ai-duel compare BASELINE CURRENT`
    // Does not require a card database or any of the single/suite-mode flags.
    if args.get(1).map(|s| s.as_str()) == Some("compare") {
        let exit = run_compare(&args[1..]);
        std::process::exit(exit);
    }

    let cli = match parse_cli(&args[1..]) {
        Ok(cli) => cli,
        Err(message) => {
            eprintln!("{message}");
            eprintln!();
            print_usage();
            std::process::exit(2);
        }
    };

    // Answered before the database load: it describes the binary, not a game.
    if cli.mode == Mode::ListMatchups {
        list_matchups();
        return;
    }

    let Some(path) = resolve_cards_root(
        cli.cards_root.clone(),
        std::env::var("PHASE_CARDS_PATH").ok(),
    ) else {
        print_usage();
        std::process::exit(1);
    };

    let export_path = path.join("card-data.json");
    let db = match CardDatabase::from_export(&export_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!(
                "Failed to load card database from {}: {e}",
                export_path.display()
            );
            std::process::exit(1);
        }
    };

    let base_seed = cli.seed.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    });

    match cli.mode {
        Mode::ListMatchups => {
            unreachable!("--list-matchups returns before the card database is loaded")
        }
        Mode::Suite => {
            let games = cli.games.unwrap_or(10);
            let output_path = cli
                .output
                .unwrap_or_else(|| PathBuf::from("target/duel-suite-results.json"));
            let mut options = SuiteOptions::new(cli.difficulty, games, base_seed);
            options.output = ReportSink::Create(output_path.clone());
            options.filter = cli.suite_filter;
            options.attribution = cli.attribution;
            options.harvest_output = cli.harvest_output;
            match run_suite(&db, &options) {
                Ok(_) => {
                    eprintln!("\nSuite report written to {}", output_path.display());
                }
                Err(e) => {
                    eprintln!("Suite run failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        Mode::CommanderSuite => {
            let games = cli.games.unwrap_or(4);
            run_commander_suite(
                &db,
                CommanderSuiteOptions {
                    cards_root: &path,
                    feed: &cli.feed,
                    games_per_seat: games,
                    base_seed,
                    candidate_difficulty: cli.difficulty,
                    baseline_difficulty: cli.baseline_difficulty,
                    output: cli.output,
                    // The suite keeps its historical bounds unless a wall budget
                    // is asked for explicitly: `--game-timeout` is opt-in here,
                    // where a 1v1 run always carries one.
                    budget: GameBudget {
                        max_actions: COMMANDER_MAX_TOTAL_ACTIONS,
                        wall: cli.game_timeout,
                        stall_actions: None,
                    },
                },
            );
        }
        Mode::CommanderDuel => {
            let (Some(p0), Some(p1)) = (cli.duel_p0.clone(), cli.duel_p1.clone()) else {
                eprintln!("--commander-1v1 requires --p0 NAME and --p1 NAME");
                std::process::exit(2);
            };
            run_commander_duel(
                &db,
                CommanderDuelOptions {
                    cards_root: &path,
                    feed: &cli.feed,
                    p0: &p0,
                    p1: &p1,
                    games: cli.games.unwrap_or(4),
                    base_seed,
                    difficulty: cli.difficulty,
                    baseline_difficulty: cli.baseline_difficulty,
                    output: cli.output,
                    budget: duel_budget(cli.game_timeout),
                    // Tracing is diagnostic output only. Every stop condition
                    // lives in `budget`, so `--game-timeout` holds with or
                    // without `--trace`.
                    trace: cli.trace.then(TraceOptions::default),
                },
            );
        }
        Mode::Single => {
            run_single(
                &db,
                &cli.matchup,
                cli.batch,
                base_seed,
                cli.difficulty,
                cli.verbose,
            );
        }
    }
}

/// Every `ai-duel` option outside the `compare` subcommand, parsed exactly once.
///
/// One pass is the point. The previous implementation consumed the flags and then
/// rescanned the original argv for the first token without a `--` prefix to use as
/// the data root, so `--commander-1v1 --p0 Krenko --p1 Giada` selected `Krenko`
/// and tried to load `Krenko/card-data.json`. A flag's value is now consumed by
/// the flag that owns it, and the data root is a single explicit positional.
#[derive(Debug, Clone)]
struct CliOptions {
    mode: Mode,
    /// The one positional argument: the directory holding `card-data.json`.
    /// `None` falls back to `PHASE_CARDS_PATH`.
    cards_root: Option<PathBuf>,
    verbose: bool,
    batch: Option<usize>,
    seed: Option<u64>,
    difficulty: AiDifficulty,
    baseline_difficulty: AiDifficulty,
    matchup: String,
    games: Option<usize>,
    output: Option<PathBuf>,
    suite_filter: Option<String>,
    attribution: AttributionMode,
    harvest_output: Option<PathBuf>,
    feed: String,
    /// Diagnostic output only — never a stop condition. See `GameBudget`.
    trace: bool,
    /// Per-game wall budget. `None` means "not requested": a 1v1 run still gets
    /// `DEFAULT_GAME_TIMEOUT`, and the suite stays bounded only by its action cap.
    game_timeout: Option<Duration>,
    duel_p0: Option<String>,
    duel_p1: Option<String>,
}

impl Default for CliOptions {
    fn default() -> Self {
        Self {
            mode: Mode::Single,
            cards_root: None,
            verbose: false,
            batch: None,
            seed: None,
            difficulty: AiDifficulty::Medium,
            baseline_difficulty: AiDifficulty::Medium,
            matchup: "red-vs-green".to_string(),
            games: None,
            output: None,
            suite_filter: None,
            attribution: AttributionMode::Disabled,
            harvest_output: None,
            feed: "feeds/mtggoldfish-commander.json".to_string(),
            trace: false,
            game_timeout: None,
            duel_p0: None,
            duel_p1: None,
        }
    }
}

/// Consumes the value token belonging to `flag`.
///
/// A value-taking flag at the end of argv is an error rather than a silent
/// default: `--seed` with nothing after it used to leave the seed time-based,
/// which quietly destroys the reproducibility the flag exists to provide.
fn take_value<'a>(
    args: &mut impl Iterator<Item = &'a String>,
    flag: &str,
) -> Result<&'a String, String> {
    args.next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

/// Consumes and parses the value token belonging to `flag`.
///
/// `expected` describes the accepted shape for the error message. A malformed
/// value is rejected instead of falling back, so `--batch 2o` cannot silently
/// run a single game.
fn parse_value<'a, T: std::str::FromStr>(
    args: &mut impl Iterator<Item = &'a String>,
    flag: &str,
    expected: &str,
) -> Result<T, String> {
    let raw = take_value(args, flag)?;
    raw.parse()
        .map_err(|_| format!("{flag} expects {expected}, got '{raw}'"))
}

/// Parses argv (without the program name) into typed options.
///
/// Rejects unknown flags and any positional beyond the data root: with several
/// value-taking flags in play, a tolerated stray token is indistinguishable from
/// a data root and silently changes which card database is loaded.
fn parse_cli(args: &[String]) -> Result<CliOptions, String> {
    let mut cli = CliOptions::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--verbose" => cli.verbose = true,
            "--batch" => cli.batch = Some(parse_value(&mut args, "--batch", "a game count")?),
            "--seed" => cli.seed = Some(parse_value(&mut args, "--seed", "an integer seed")?),
            "--difficulty" => {
                cli.difficulty = parse_difficulty(take_value(&mut args, "--difficulty")?)
            }
            "--baseline-difficulty" => {
                cli.baseline_difficulty =
                    parse_difficulty(take_value(&mut args, "--baseline-difficulty")?);
            }
            "--matchup" => cli.matchup = take_value(&mut args, "--matchup")?.clone(),
            "--suite" => cli.mode = Mode::Suite,
            "--commander-suite" => cli.mode = Mode::CommanderSuite,
            "--commander-1v1" => cli.mode = Mode::CommanderDuel,
            "--list-matchups" => cli.mode = Mode::ListMatchups,
            "--trace" => cli.trace = true,
            "--game-timeout" => {
                let secs: u64 = parse_value(&mut args, "--game-timeout", "seconds")?;
                if secs == 0 {
                    return Err("--game-timeout must be at least 1 second".to_string());
                }
                cli.game_timeout = Some(Duration::from_secs(secs));
            }
            "--p0" => cli.duel_p0 = Some(take_value(&mut args, "--p0")?.clone()),
            "--p1" => cli.duel_p1 = Some(take_value(&mut args, "--p1")?.clone()),
            "--games" => cli.games = Some(parse_value(&mut args, "--games", "a game count")?),
            "--output" => cli.output = Some(PathBuf::from(take_value(&mut args, "--output")?)),
            "--suite-filter" => {
                cli.suite_filter = Some(take_value(&mut args, "--suite-filter")?.clone());
            }
            "--show-attribution" => cli.attribution = AttributionMode::Enabled,
            "--harvest" => {
                cli.harvest_output = Some(PathBuf::from(take_value(&mut args, "--harvest")?));
            }
            "--feed" => cli.feed = take_value(&mut args, "--feed")?.clone(),
            unknown if unknown.starts_with("--") => {
                return Err(format!("Unknown option: {unknown}"));
            }
            positional => {
                if let Some(existing) = &cli.cards_root {
                    return Err(format!(
                        "Unexpected argument '{positional}': the data root is already '{}' and is the only positional argument",
                        existing.display()
                    ));
                }
                cli.cards_root = Some(PathBuf::from(positional));
            }
        }
    }
    Ok(cli)
}

/// The data root: the positional argument, else `PHASE_CARDS_PATH`.
///
/// An empty environment value is treated as unset — it would otherwise resolve to
/// a relative `card-data.json` and report a confusing load failure.
fn resolve_cards_root(positional: Option<PathBuf>, env: Option<String>) -> Option<PathBuf> {
    positional.or_else(|| env.filter(|value| !value.is_empty()).map(PathBuf::from))
}

/// Bounds for one 1v1 game.
///
/// The wall budget is unconditional, which is the reason it no longer lives on
/// `TraceOptions`: a requested `--game-timeout` must bound the run whether or not
/// diagnostics were also asked for.
fn duel_budget(game_timeout: Option<Duration>) -> GameBudget {
    GameBudget {
        max_actions: COMMANDER_MAX_TOTAL_ACTIONS,
        wall: Some(game_timeout.unwrap_or(DEFAULT_GAME_TIMEOUT)),
        stall_actions: Some(DUEL_STALL_ACTIONS),
    }
}

fn run_single(
    db: &CardDatabase,
    matchup: &str,
    batch: Option<usize>,
    base_seed: u64,
    difficulty: AiDifficulty,
    verbose: bool,
) {
    let Some(spec) = find_matchup(matchup) else {
        eprintln!("Unknown matchup '{matchup}'. Use --list-matchups to see options.");
        std::process::exit(1);
    };

    let (payload, p0_label, p1_label) = match resolve_matchup(db, spec) {
        Ok(v) => v,
        Err(reason) => {
            eprintln!("Failed to resolve matchup '{matchup}': {reason}");
            std::process::exit(1);
        }
    };

    validate_deck(&payload.player, 60, &p0_label);
    validate_deck(&payload.opponent, 60, &p1_label);

    let game_count = batch.unwrap_or(1);
    let is_batch = batch.is_some();

    let mut p0_wins: usize = 0;
    let mut p1_wins: usize = 0;
    let mut draws: usize = 0;
    let mut total_turns: u32 = 0;
    let mut total_duration_ms: u128 = 0;

    for game_idx in 0..game_count {
        let game_seed = base_seed + game_idx as u64;

        if !is_batch {
            eprintln!("AI Duel — seed: {game_seed}, difficulty: {difficulty:?}");
        }

        let start = Instant::now();
        let (winner, turns) = run_game(&payload, game_seed, difficulty, verbose, is_batch);
        let elapsed = start.elapsed().as_millis();

        match winner {
            Some(PlayerId(0)) => p0_wins += 1,
            Some(_) => p1_wins += 1,
            None => draws += 1,
        }
        total_turns += turns;
        total_duration_ms += elapsed;

        if !is_batch {
            match winner {
                Some(PlayerId(0)) => {
                    eprintln!("\nGame over — {p0_label} (P0) wins on turn {turns} ({elapsed}ms)")
                }
                Some(_) => {
                    eprintln!("\nGame over — {p1_label} (P1) wins on turn {turns} ({elapsed}ms)")
                }
                None => eprintln!("\nGame over — draw/aborted on turn {turns} ({elapsed}ms)"),
            }
        }
    }

    if is_batch {
        let n = game_count;
        let avg_turns = total_turns as f64 / n as f64;
        let avg_ms = total_duration_ms as f64 / n as f64;
        eprintln!("\nResults ({n} games, seed: {base_seed}, difficulty: {difficulty:?}, matchup: {matchup}):");
        eprintln!(
            "  P0 ({p0_label}) wins: {p0_wins:>4} ({:.1}%)",
            p0_wins as f64 / n as f64 * 100.0
        );
        eprintln!(
            "  P1 ({p1_label}) wins: {p1_wins:>4} ({:.1}%)",
            p1_wins as f64 / n as f64 * 100.0
        );
        eprintln!(
            "  Draws/aborted:             {draws:>4} ({:.1}%)",
            draws as f64 / n as f64 * 100.0
        );
        eprintln!("  Avg turns: {avg_turns:.1}");
        eprintln!("  Avg duration: {avg_ms:.0}ms");
    }
}

fn run_game(
    payload: &DeckPayload,
    seed: u64,
    difficulty: AiDifficulty,
    verbose: bool,
    silent: bool,
) -> (Option<PlayerId>, u32) {
    let mut state = GameState::new_two_player(seed);
    load_deck_into_state(&mut state, payload);
    engine::game::engine::start_game(&mut state);

    let ai_players: HashSet<PlayerId> = [PlayerId(0), PlayerId(1)].into_iter().collect();
    // Pin measurement mode for regression runs: search is bounded by
    // max_nodes only, so duel outcomes don't observe wall-clock variance
    // across hardware. Production code leaves this off to use time budgets.
    let config = create_config_for_players(difficulty, Platform::Native, 2).into_measurement(seed);
    let ai_configs: HashMap<PlayerId, _> = [(PlayerId(0), config.clone()), (PlayerId(1), config)]
        .into_iter()
        .collect();

    let mut total_actions: usize = 0;
    let mut last_turn: u32 = 0;
    let mut ai_rng = StdRng::seed_from_u64(seed);
    let ai_session = phase_ai::session::AiSession::arc_from_game(&state);

    loop {
        let results = run_ai_actions(
            &mut state,
            &ai_players,
            &ai_configs,
            &mut ai_rng,
            &ai_session,
        );
        if results.is_empty() {
            if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
                break;
            }
            eprintln!("Warning: no AI actions and game not over — breaking");
            break;
        }
        total_actions += results.len();

        if !silent {
            for result in &results {
                if verbose {
                    eprintln!("  ACTION: {:?}", result.action);
                }
                for entry in &result.log_entries {
                    if entry.turn != last_turn {
                        last_turn = entry.turn;
                        eprintln!("=== Turn {last_turn} ===");
                    }
                    if should_show(entry, verbose) {
                        eprintln!("  {}", render_log_entry(entry));
                    }
                }
            }
        }

        if total_actions >= MAX_TOTAL_ACTIONS {
            eprintln!("Safety: hit {MAX_TOTAL_ACTIONS} total actions — aborting game");
            break;
        }
    }

    let winner = match &state.waiting_for {
        WaitingFor::GameOver { winner } => *winner,
        _ => None,
    };
    (winner, state.turn_number)
}

struct CommanderSuiteOptions<'a> {
    cards_root: &'a std::path::Path,
    feed: &'a str,
    games_per_seat: usize,
    base_seed: u64,
    candidate_difficulty: AiDifficulty,
    baseline_difficulty: AiDifficulty,
    output: Option<PathBuf>,
    budget: GameBudget,
}

fn run_commander_suite(db: &CardDatabase, options: CommanderSuiteOptions<'_>) {
    let seats = usize::from(COMMANDER_SUITE_SEATS);
    let deck_lists = load_commander_decks(db, options.cards_root, options.feed, Some(seats));
    if deck_lists.len() < seats {
        eprintln!(
            "Commander suite needs at least {seats} resolvable decks, found {}",
            deck_lists.len()
        );
        std::process::exit(1);
    }
    let deck_list = DeckList {
        player: deck_lists[0].clone(),
        opponent: deck_lists[1].clone(),
        ai_decks: vec![deck_lists[2].clone(), deck_lists[3].clone()],
        ..Default::default()
    };
    let payload = resolve_deck_list(db, &deck_list);

    let mut seat_rows = Vec::new();
    let mut all_games = Vec::new();
    for candidate_seat in 0..COMMANDER_SUITE_SEATS {
        let candidate = PlayerId(candidate_seat);
        let mut wins = 0usize;
        let mut total_survival_turns = 0u64;
        let mut total_elimination_order = 0u64;

        for game_idx in 0..options.games_per_seat {
            let seed = options
                .base_seed
                .wrapping_add(u64::from(candidate_seat) * 10_000)
                .wrapping_add(game_idx as u64);
            let result = run_commander_game(CommanderGameOptions {
                payload: &payload,
                seed,
                candidate,
                candidate_difficulty: options.candidate_difficulty,
                baseline_difficulty: options.baseline_difficulty,
                players: COMMANDER_SUITE_SEATS,
                budget: options.budget,
                trace: None,
            });
            if result.outcome.winner() == Some(candidate) {
                wins += 1;
            }
            total_survival_turns += result.candidate_survival_turn as u64;
            total_elimination_order += result.candidate_elimination_order as u64;
            all_games.push(serde_json::json!({
                "candidate_seat": candidate.0,
                "seed": seed,
                "winner": result.outcome.winner().map(|p| p.0),
                "completed": result.outcome.completed(),
                "stop_reason": result.outcome.stop_reason().map(StopReason::label),
                "turns": result.turns,
                "candidate_survival_turn": result.candidate_survival_turn,
                "candidate_elimination_order": result.candidate_elimination_order,
            }));
        }

        let n = options.games_per_seat.max(1) as f64;
        let win_rate = wins as f64 / n;
        let avg_survival_turns = total_survival_turns as f64 / n;
        let avg_elimination_order = total_elimination_order as f64 / n;
        eprintln!(
            "Commander seat P{}: wins={}/{} ({:.1}%) survival_turns={:.1} elimination_order={:.1}",
            candidate.0,
            wins,
            options.games_per_seat,
            win_rate * 100.0,
            avg_survival_turns,
            avg_elimination_order
        );
        seat_rows.push(serde_json::json!({
            "candidate_seat": candidate.0,
            "games": options.games_per_seat,
            "wins": wins,
            "win_rate": rounded(win_rate),
            "avg_survival_turns": rounded(avg_survival_turns),
            "avg_elimination_order": rounded(avg_elimination_order),
        }));
    }

    let report = serde_json::json!({
        "schema_version": 1,
        "mode": "commander_suite",
        "feed": options.feed,
        "candidate_difficulty": format!("{:?}", options.candidate_difficulty),
        "baseline_difficulty": format!("{:?}", options.baseline_difficulty),
        "games_per_seat": options.games_per_seat,
        "base_seed": options.base_seed,
        "metrics": {
            "win_rate": "candidate wins / games",
            "survival_turns": "turn number when candidate was eliminated, or final turn if not eliminated",
            "elimination_order": format!(
                "1 = first eliminated, {COMMANDER_SUITE_SEATS} = winner or last survivor"
            ),
        },
        "seats": seat_rows,
        "games": all_games,
    });
    let output_path = options
        .output
        .unwrap_or_else(|| PathBuf::from("target/commander-suite-results.json"));
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|err| {
            eprintln!("failed to create {}: {err}", parent.display());
            std::process::exit(1);
        });
    }
    std::fs::write(
        &output_path,
        serde_json::to_string_pretty(&report).expect("commander report serializes"),
    )
    .unwrap_or_else(|err| {
        eprintln!("failed to write {}: {err}", output_path.display());
        std::process::exit(1);
    });
    eprintln!(
        "Commander suite report written to {}",
        output_path.display()
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("commander report serializes")
    );
}

/// Why the driver abandoned a game that never reached `WaitingFor::GameOver`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopReason {
    /// The whole-game action cap was reached.
    ActionCap,
    /// The wall-clock budget for the game was reached.
    WallTimeout,
    /// Actions kept being taken with no change in turn number — a driver loop.
    StalledSameTurn,
    /// The AI produced no action for a decision it was asked to make.
    NoLegalActions,
}

impl StopReason {
    /// Stable label for reports. Reports are archived and diffed across runs, so
    /// these strings are part of the output contract, not debug text.
    fn label(self) -> &'static str {
        match self {
            StopReason::ActionCap => "action_cap",
            StopReason::WallTimeout => "wall_timeout",
            StopReason::StalledSameTurn => "stalled_same_turn",
            StopReason::NoLegalActions => "no_legal_actions",
        }
    }
}

/// How one Commander game ended.
///
/// Replaces a `completed: bool` plus `stop_reason: Option<&str>` pair that could
/// disagree — `completed: false` with no reason, or a reason attached to a
/// finished game — and had to be patched back into agreement at every report
/// site. Follows the typed-outcome seam in `ai_commander.rs`
/// (`RunOutcome`/`classify_run_outcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GameOutcome {
    /// Reached `WaitingFor::GameOver` with a winner.
    Decided(PlayerId),
    /// Reached `WaitingFor::GameOver` with no winner: a genuine draw, which is a
    /// result, not a failed run.
    Draw,
    /// The driver gave up before the game ended.
    Stopped(StopReason),
}

impl GameOutcome {
    /// Whether the game reached its own end rather than being abandoned.
    fn completed(self) -> bool {
        !matches!(self, GameOutcome::Stopped(_))
    }

    fn winner(self) -> Option<PlayerId> {
        match self {
            GameOutcome::Decided(winner) => Some(winner),
            GameOutcome::Draw | GameOutcome::Stopped(_) => None,
        }
    }

    fn stop_reason(self) -> Option<StopReason> {
        match self {
            GameOutcome::Stopped(reason) => Some(reason),
            GameOutcome::Decided(_) | GameOutcome::Draw => None,
        }
    }
}

/// Classifies a finished driver loop from the reason it broke and where the state
/// machine parked.
///
/// `waiting_for` is authoritative: reaching `GameOver` means the game is decided
/// even if a budget guard fired on the same iteration. This is the opposite
/// precedence to `ai_commander::classify_run_outcome`, deliberately — there the
/// cap truncates a batch mid-flight, so a `GameOver` state can be the tail of a
/// run the cap cut short; here every guard is evaluated *before* a step is taken,
/// so a guard and a finished game on the same iteration means the game finished
/// first.
fn classify_outcome(stop: Option<StopReason>, waiting_for: &WaitingFor) -> GameOutcome {
    match waiting_for {
        WaitingFor::GameOver {
            winner: Some(winner),
        } => GameOutcome::Decided(*winner),
        WaitingFor::GameOver { winner: None } => GameOutcome::Draw,
        _ => GameOutcome::Stopped(stop.unwrap_or(StopReason::NoLegalActions)),
    }
}

/// Bounds on one Commander game, independent of diagnostics.
///
/// These are the stop conditions. `TraceOptions` decides only what gets printed,
/// so a wall budget holds with or without `--trace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GameBudget {
    /// Hard cap on actions taken across the whole game.
    max_actions: usize,
    /// Wall-clock budget. `None` leaves the game bounded only by `max_actions`.
    wall: Option<Duration>,
    /// Give up after this many actions with no change in turn number. `None`
    /// disables stall detection.
    stall_actions: Option<usize>,
}

impl GameBudget {
    /// Single authority for "must this game stop now?".
    ///
    /// Evaluated before each bounded step, so a game can overrun its budget by at
    /// most one step rather than by a whole unbounded `run_ai_actions` batch.
    /// Ordered most-authoritative first: the action cap is a hard invariant, the
    /// wall budget is what the operator asked for, and the stall cutoff is the
    /// heuristic.
    fn exceeded(
        &self,
        elapsed: Duration,
        total_actions: usize,
        actions_since_turn_change: usize,
    ) -> Option<StopReason> {
        if total_actions >= self.max_actions {
            return Some(StopReason::ActionCap);
        }
        if self.wall.is_some_and(|wall| elapsed >= wall) {
            return Some(StopReason::WallTimeout);
        }
        if self
            .stall_actions
            .is_some_and(|stall| actions_since_turn_change >= stall)
        {
            return Some(StopReason::StalledSameTurn);
        }
        None
    }
}

/// Measurement nonce for one seat's `AiConfig`.
///
/// Keyed on the seat's *role*, not its index: the candidate always anchors on
/// `seed`, and baselines take `seed + 1 ..` numbered by their position among the
/// non-candidate seats. Both callers rotate one deck across seats and compare the
/// results — the duel swaps seats within a seed pair, the suite rotates the
/// candidate through all four — so a nonce that moved with the seat would put a
/// per-seat difference inside the very comparison the rotation exists to make.
fn measurement_nonce(seed: u64, seat: PlayerId, candidate: PlayerId) -> u64 {
    if seat == candidate {
        return seed;
    }
    let baseline_index = u64::from(seat.0) - u64::from(seat.0 > candidate.0);
    seed.wrapping_add(baseline_index + 1)
}

/// Elimination bookkeeping for one game, sized from the seat count.
///
/// Commander seats 2 through 6 (`FormatConfig::commander()` declares
/// `min_players: 2`, `max_players: 6`), so neither the per-seat turn table nor
/// the survivor's reported order may be fixed at four: a fixed table panics on
/// `PlayerId(4)` in a five- or six-seat game, and a fixed fallback reports a
/// two-seat survivor as having outlasted three opponents that never existed.
struct EliminationLedger {
    seats: u8,
    /// Turn each seat was first observed eliminated, indexed by `PlayerId.0`.
    turns: Vec<Option<u32>>,
    /// Seats in the order they were first observed eliminated.
    order: Vec<PlayerId>,
}

impl EliminationLedger {
    fn new(seats: u8) -> Self {
        Self {
            seats,
            turns: vec![None; usize::from(seats)],
            order: Vec::new(),
        }
    }

    /// Records any newly eliminated seat as having gone out on `turn`.
    ///
    /// Idempotent per seat — the first observation wins — so the driver can poll
    /// `state.eliminated_players` on every iteration.
    fn observe(&mut self, eliminated: &[PlayerId], turn: u32) {
        for seat in eliminated {
            if self.order.contains(seat) {
                continue;
            }
            self.order.push(*seat);
            if let Some(slot) = self.turns.get_mut(usize::from(seat.0)) {
                *slot = Some(turn);
            }
        }
    }

    /// Turn `seat` was eliminated, or `final_turn` if it survived.
    fn survival_turn(&self, seat: PlayerId, final_turn: u32) -> u32 {
        self.turns
            .get(usize::from(seat.0))
            .copied()
            .flatten()
            .unwrap_or(final_turn)
    }

    /// 1 = first eliminated. A seat that was never eliminated reports the seat
    /// count — the winner or last survivor — which is why the fallback is
    /// configured rather than a literal 4.
    fn elimination_order(&self, seat: PlayerId) -> u8 {
        self.order
            .iter()
            .position(|player| *player == seat)
            .map_or(self.seats, |idx| idx as u8 + 1)
    }
}

struct CommanderGameResult {
    outcome: GameOutcome,
    turns: u32,
    candidate_survival_turn: u32,
    candidate_elimination_order: u8,
}

/// Everything one Commander game needs, as a struct rather than a positional
/// argument list. Matches `CommanderSuiteOptions`/`CommanderDuelOptions` in this
/// file, and keeps the two callers from transposing the seat count, the seeds and
/// the two difficulties — all of which are same-typed and adjacent.
struct CommanderGameOptions<'a> {
    payload: &'a DeckPayload,
    seed: u64,
    /// The seat being measured. It takes `candidate_difficulty`, anchors the
    /// measurement nonce, and is the seat the survival metrics describe.
    candidate: PlayerId,
    candidate_difficulty: AiDifficulty,
    baseline_difficulty: AiDifficulty,
    players: u8,
    /// Every condition that can stop the game before it ends.
    budget: GameBudget,
    /// Diagnostic output only.
    trace: Option<&'a TraceOptions>,
}

fn run_commander_game(options: CommanderGameOptions<'_>) -> CommanderGameResult {
    let CommanderGameOptions {
        payload,
        seed,
        candidate,
        candidate_difficulty,
        baseline_difficulty,
        players,
        budget,
        trace,
    } = options;

    let mut state = GameState::new(FormatConfig::commander(), players, seed);
    load_deck_into_state(&mut state, payload);
    engine::game::engine::start_game(&mut state);

    let ai_players: HashSet<PlayerId> = (0..players).map(PlayerId).collect();
    let mut ai_configs: HashMap<PlayerId, _> = HashMap::new();
    for seat in (0..players).map(PlayerId) {
        let difficulty = if seat == candidate {
            candidate_difficulty
        } else {
            baseline_difficulty
        };
        // Player count is threaded into the AI config too: threat assessment and
        // politics weighting differ between a pod and a duel, so a 1v1 run must
        // not be configured as if three opponents were present.
        ai_configs.insert(
            seat,
            create_config_for_players(difficulty, Platform::Native, players)
                .into_measurement(measurement_nonce(seed, seat, candidate)),
        );
    }

    let mut total_actions = 0usize;
    // Hang diagnosis. A full action trace is useless at this scale (the cap is 200k
    // actions), so instead: periodic progress showing whether the game is advancing at
    // all, and a ring buffer of recent actions dumped on a bad exit -- a repeating cycle
    // is visible immediately in the last few dozen actions. The cutoffs themselves are
    // `budget`'s, not this block's, so they apply whether or not tracing is on.
    let started = Instant::now();
    let mut recent: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let mut last_progress_turn = 0u32;
    let mut actions_at_last_turn_change = 0usize;
    let mut ai_rng = StdRng::seed_from_u64(seed);
    let ai_session = phase_ai::session::AiSession::arc_from_game(&state);
    let mut ledger = EliminationLedger::new(players);

    let stop = loop {
        ledger.observe(&state.eliminated_players, state.turn_number);
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            break None;
        }
        // Checked before the step, not after: a step that has already run cannot be
        // un-run, and an unbounded batch would take up to 200 actions -- each of which
        // can be a full search -- past the point the budget was spent.
        if let Some(reason) = budget.exceeded(
            started.elapsed(),
            total_actions,
            total_actions - actions_at_last_turn_change,
        ) {
            break Some(reason);
        }

        let run = run_ai_actions_bounded(
            &mut state,
            &ai_players,
            &ai_configs,
            &mut ai_rng,
            &ai_session,
            DRIVER_STEP_ACTIONS,
        );
        if run.results.is_empty() {
            break Some(StopReason::NoLegalActions);
        }
        if let Some(opts) = trace {
            for result in &run.results {
                if recent.len() == opts.ring {
                    recent.pop_front();
                }
                recent.push_back(format!(
                    "T{} P{} {:?}",
                    state.turn_number, state.active_player.0, result.action
                ));
            }
        }
        let taken = run.results.len();
        total_actions += taken;

        if state.turn_number != last_progress_turn {
            last_progress_turn = state.turn_number;
            actions_at_last_turn_change = total_actions;
        }
        if let Some(opts) = trace {
            if total_actions / opts.every != (total_actions - taken) / opts.every {
                eprintln!(
                    "{}",
                    trace_progress_line(
                        state.turn_number,
                        total_actions,
                        started.elapsed(),
                        &state.waiting_for,
                    )
                );
            }
        }
    };

    ledger.observe(&state.eliminated_players, state.turn_number);
    let outcome = classify_outcome(stop, &state.waiting_for);

    if let (Some(_), Some(reason)) = (trace, outcome.stop_reason()) {
        eprintln!(
            "    [trace] STOPPED {} at turn {} after {} actions ({:.0}s); last {} actions:",
            reason.label(),
            state.turn_number,
            total_actions,
            started.elapsed().as_secs_f64(),
            recent.len()
        );
        for line in &recent {
            eprintln!("      {line}");
        }
    }

    CommanderGameResult {
        outcome,
        turns: state.turn_number,
        candidate_survival_turn: ledger.survival_turn(candidate, state.turn_number),
        candidate_elimination_order: ledger.elimination_order(candidate),
    }
}

fn load_commander_decks(
    db: &CardDatabase,
    cards_root: &std::path::Path,
    feed: &str,
    max_decks: Option<usize>,
) -> Vec<PlayerDeckList> {
    let feed_path = cards_root.join(feed);
    let feed_file = std::fs::File::open(&feed_path).unwrap_or_else(|err| {
        eprintln!("failed to open {}: {err}", feed_path.display());
        std::process::exit(1);
    });
    let feed_json: serde_json::Value = serde_json::from_reader(feed_file).unwrap_or_else(|err| {
        eprintln!("failed to parse {}: {err}", feed_path.display());
        std::process::exit(1);
    });
    let decks_json = feed_json["decks"].as_array().unwrap_or_else(|| {
        eprintln!("{} missing decks array", feed_path.display());
        std::process::exit(1);
    });

    let mut deck_lists = Vec::new();
    for deck in decks_json {
        if max_decks.is_some_and(|max| deck_lists.len() == max) {
            break;
        }
        let deck_name = deck["name"].as_str().unwrap_or("<unnamed>");
        let commander_names: Vec<String> = match deck["commander"].as_array() {
            Some(arr) if !arr.is_empty() => arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            _ => vec![deck_name.to_string()],
        };
        let Some(primary_commander) = commander_names.first() else {
            continue;
        };
        if db.get_face_by_name(primary_commander).is_none() {
            eprintln!("Skipping {deck_name}: commander '{primary_commander}' not in card db");
            continue;
        }

        let mut main_deck = Vec::new();
        let Some(main_entries) = deck["main"].as_array() else {
            continue;
        };
        for entry in main_entries {
            let Some(name) = entry["name"].as_str() else {
                continue;
            };
            if commander_names.iter().any(|commander| commander == name) {
                continue;
            }
            let count = entry["count"].as_u64().unwrap_or(0) as usize;
            main_deck.extend(std::iter::repeat_n(name.to_string(), count));
        }

        deck_lists.push(PlayerDeckList {
            main_deck,
            sideboard: Vec::new(),
            commander: commander_names,
            ..Default::default()
        });
    }
    deck_lists
}

/// Instrumentation for diagnosing games that do not terminate.
///
/// Output settings only. Every condition that can stop a game lives on
/// `GameBudget`, so turning diagnostics off cannot turn a cutoff off with it.
struct TraceOptions {
    /// Emit a progress line every this many actions.
    every: usize,
    /// Keep this many recent actions to dump when a game exits badly.
    ring: usize,
}

impl Default for TraceOptions {
    fn default() -> Self {
        Self {
            every: 2_000,
            ring: 40,
        }
    }
}

/// One `--trace` progress line.
///
/// The wait label comes from the engine-owned `WaitingFor::variant_name()` — an
/// exhaustive mapping the compiler keeps current — rather than from formatting
/// and re-splitting `Debug` output, which allocated the full payload on every
/// line to throw all of it away.
fn trace_progress_line(
    turn: u32,
    actions: usize,
    elapsed: Duration,
    waiting_for: &WaitingFor,
) -> String {
    format!(
        "    [trace] turn {turn:>3}  actions {actions:>7}  {:.0}s  waiting_for {}",
        elapsed.as_secs_f64(),
        waiting_for.variant_name(),
    )
}

struct CommanderDuelOptions<'a> {
    cards_root: &'a std::path::Path,
    feed: &'a str,
    p0: &'a str,
    p1: &'a str,
    games: usize,
    base_seed: u64,
    difficulty: AiDifficulty,
    baseline_difficulty: AiDifficulty,
    output: Option<PathBuf>,
    budget: GameBudget,
    trace: Option<TraceOptions>,
}

/// Seed for `game_idx`, paired so that a seat swap is the *only* difference
/// inside a pair.
///
/// Games `2k` and `2k+1` replay one seed with the seats reversed. The swap exists
/// to cancel Commander's first-player advantage, and it can only do that with the
/// RNG stream held fixed across the pair; advancing the seed per game made every
/// swap an independent sample instead, leaving the play/draw effect in the noise
/// it was meant to remove. This is the paired-seed methodology the suite compare
/// gate already reports against.
fn paired_seed(base_seed: u64, game_idx: usize) -> u64 {
    base_seed.wrapping_add((game_idx / 2) as u64)
}

/// Whether deck 0 is on the play for `game_idx`. Even games seat it first.
fn deck0_on_the_play(game_idx: usize) -> bool {
    game_idx.is_multiple_of(2)
}

/// Rejects a game count that cannot give both decks the play equally.
fn validate_duel_games(games: usize) -> Result<(), String> {
    if games == 0 {
        return Err("--games must be at least 2 for a 1v1 run".to_string());
    }
    if !games.is_multiple_of(2) {
        return Err(format!(
            "--games must be even for a 1v1 run so both decks get the play equally (got {games})"
        ));
    }
    Ok(())
}

/// The winning deck's label for one game, or `None` for a draw or an abandoned run.
///
/// Keyed on the seat the deck actually occupied, not on a fixed seat: the decks
/// swap seats every game, so `PlayerId(0)` is deck 0 only on even games.
fn duel_result_label<'a>(
    outcome: GameOutcome,
    deck0_seat: PlayerId,
    p0: &'a str,
    p1: &'a str,
) -> Option<&'a str> {
    match outcome.winner() {
        Some(winner) if winner == deck0_seat => Some(p0),
        Some(_) => Some(p1),
        None => None,
    }
}

/// Per-game stderr disposition. A draw is a result and is named as one; only an
/// abandoned run reads as `INCOMPLETE`.
fn duel_game_disposition<'a>(
    outcome: GameOutcome,
    deck0_seat: PlayerId,
    p0: &'a str,
    p1: &'a str,
) -> &'a str {
    match outcome {
        GameOutcome::Decided(_) => {
            duel_result_label(outcome, deck0_seat, p0, p1).unwrap_or("decided")
        }
        GameOutcome::Draw => "draw",
        GameOutcome::Stopped(_) => "INCOMPLETE",
    }
}

/// Head-to-head Commander between two decks from a feed, identified by commander name.
///
/// Seats alternate every game. Commander's first-player advantage is large, and a short
/// unbalanced sample mostly measures who was on the play, so an even `--games` gives each deck
/// the play exactly half the time; an odd count is rejected rather than silently handing one
/// deck an extra turn one.
fn run_commander_duel(db: &CardDatabase, options: CommanderDuelOptions<'_>) {
    if let Err(message) = validate_duel_games(options.games) {
        eprintln!("{message}");
        std::process::exit(2);
    }
    let decks = load_commander_decks(db, options.cards_root, options.feed, None);
    let find = |needle: &str| {
        decks
            .iter()
            .find(|d| d.commander.first().is_some_and(|c| c == needle))
            .cloned()
    };
    let (Some(deck0), Some(deck1)) = (find(options.p0), find(options.p1)) else {
        let known: Vec<&str> = decks
            .iter()
            .filter_map(|d| d.commander.first().map(String::as_str))
            .collect();
        eprintln!(
            "Could not resolve both decks ('{}', '{}') in {}. Known commanders: {:?}",
            options.p0, options.p1, options.feed, known
        );
        std::process::exit(1);
    };

    let mut p0_wins = 0usize;
    let mut p1_wins = 0usize;
    let mut draws = 0usize;
    let mut incomplete = 0usize;
    let mut rows = Vec::new();

    for game_idx in 0..options.games {
        // Even games put deck0 on the play (seat 0); odd games swap the seats.
        let deck0_first = deck0_on_the_play(game_idx);
        let (player, opponent) = if deck0_first {
            (deck0.clone(), deck1.clone())
        } else {
            (deck1.clone(), deck0.clone())
        };
        let deck_list = DeckList {
            player,
            opponent,
            ai_decks: Vec::new(),
            ..Default::default()
        };
        let payload = resolve_deck_list(db, &deck_list);
        let deck0_seat = PlayerId(u8::from(!deck0_first));
        let seed = paired_seed(options.base_seed, game_idx);
        if options.trace.is_some() {
            eprintln!("  game {game_idx} (seed {seed}) starting...");
        }
        let result = run_commander_game(CommanderGameOptions {
            payload: &payload,
            seed,
            candidate: deck0_seat,
            candidate_difficulty: options.difficulty,
            baseline_difficulty: options.baseline_difficulty,
            players: DUEL_SEATS,
            budget: options.budget,
            trace: options.trace.as_ref(),
        });

        // One exhaustive match over the outcome: a run can no longer be counted
        // as both incomplete and drawn, or as neither.
        match result.outcome {
            GameOutcome::Decided(winner) if winner == deck0_seat => p0_wins += 1,
            GameOutcome::Decided(_) => p1_wins += 1,
            GameOutcome::Draw => draws += 1,
            GameOutcome::Stopped(_) => incomplete += 1,
        }
        let winner_label = duel_result_label(result.outcome, deck0_seat, options.p0, options.p1);
        let stop_reason = result.outcome.stop_reason();
        let on_play = if deck0_first { options.p0 } else { options.p1 };
        rows.push(serde_json::json!({
            "game": game_idx,
            "seed": seed,
            "on_the_play": on_play,
            "winner": winner_label,
            "completed": result.outcome.completed(),
            "stop_reason": stop_reason.map(StopReason::label),
            "turns": result.turns,
        }));
        eprintln!(
            "  game {game_idx}: {} (turns={}, on_play={on_play}){}",
            duel_game_disposition(result.outcome, deck0_seat, options.p0, options.p1),
            result.turns,
            stop_reason
                .map(|reason| format!(" [{}]", reason.label()))
                .unwrap_or_default(),
        );
    }

    let decided = p0_wins + p1_wins;
    eprintln!(
        "{} {}-{} {} ({} decided, {} drawn, {} incomplete of {})",
        options.p0, p0_wins, p1_wins, options.p1, decided, draws, incomplete, options.games
    );

    let report = serde_json::json!({
        "schema_version": 1,
        "mode": "commander_duel",
        "feed": options.feed,
        "p0": options.p0,
        "p1": options.p1,
        "games": options.games,
        "base_seed": options.base_seed,
        "seed_schedule": "paired: games 2k and 2k+1 share base_seed+k with the seats reversed",
        "difficulty": format!("{:?}", options.difficulty),
        "p0_wins": p0_wins,
        "p1_wins": p1_wins,
        "draws": draws,
        "incomplete": incomplete,
        "p0_win_rate": if decided > 0 {
            rounded(p0_wins as f64 / decided as f64)
        } else {
            0.0
        },
        "games_detail": rows,
    });
    if let Some(path) = options.output {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&path, serde_json::to_string_pretty(&report).unwrap()) {
            Ok(()) => eprintln!("Duel report written to {}", path.display()),
            Err(e) => eprintln!("Failed to write {}: {e}", path.display()),
        }
    } else {
        println!("{}", serde_json::to_string(&report).unwrap());
    }
}

fn rounded(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn should_show(entry: &GameLogEntry, verbose: bool) -> bool {
    if verbose {
        return true;
    }
    matches!(
        entry.category,
        LogCategory::Stack
            | LogCategory::Combat
            | LogCategory::Life
            | LogCategory::Destroy
            | LogCategory::Special
    )
}

fn render_log_entry(entry: &GameLogEntry) -> String {
    entry
        .segments
        .iter()
        .map(|seg| match seg {
            LogSegment::Text(s) => s.clone(),
            LogSegment::CardName { name, .. } => name.clone(),
            LogSegment::PlayerName { name, .. } => name.clone(),
            LogSegment::Number(n) => n.to_string(),
            LogSegment::Mana(s) => s.clone(),
            LogSegment::Zone(z) => format!("{z:?}"),
            LogSegment::Keyword(k) => k.clone(),
        })
        .collect::<Vec<_>>()
        .join("")
}

fn validate_deck(payload: &PlayerDeckPayload, expected: usize, label: &str) {
    let actual: u32 = payload.main_deck.iter().map(|e| e.count).sum();
    if actual as usize != expected {
        eprintln!("WARNING: {label} resolved {actual}/{expected} cards");
    }
}

fn parse_difficulty(s: &str) -> AiDifficulty {
    // Single authority for the label → enum mapping lives on `AiDifficulty`.
    AiDifficulty::from_label(s)
}

fn print_usage() {
    eprintln!("Usage: ai-duel <data-root> [OPTIONS]");
    eprintln!("  <data-root> is the single positional argument (the directory holding");
    eprintln!("  card-data.json) and may appear before or after the options.");
    eprintln!("       ai-duel compare BASELINE.json CURRENT.json");
    eprintln!("  Or set PHASE_CARDS_PATH environment variable");
    eprintln!();
    eprintln!("Single-matchup mode:");
    eprintln!("  --verbose          Print every action (full trace)");
    eprintln!("  --batch N          Run N games, print summary only");
    eprintln!("  --seed S           RNG seed (default: time-based)");
    eprintln!("  --difficulty LEVEL VeryEasy|Easy|Medium|Hard|VeryHard (default: Medium)");
    eprintln!(
        "  --baseline-difficulty LEVEL Baseline seats for --commander-suite (default: Medium)"
    );
    eprintln!("  --matchup NAME     Deck matchup (default: red-vs-green)");
    eprintln!("  --list-matchups    Show available matchups");
    eprintln!();
    eprintln!("Suite mode:");
    eprintln!("  --suite            Run every registered MatchupSpec");
    eprintln!("  --games N          Games per matchup in suite mode (default: 10)");
    eprintln!(
        "  --output PATH      Write JSON report to PATH (default: target/duel-suite-results.json)"
    );
    eprintln!("  --suite-filter STR Only run matchups whose id contains STR");
    eprintln!("  --show-attribution Capture per-policy decision traces and include");
    eprintln!("                     them in the JSON + markdown output.");
    eprintln!("  --harvest PATH     Harvest per-turn eval features to JSONL at PATH");
    eprintln!("                     (Texel retrain corpus; forces sequential run).");
    eprintln!();
    eprintln!("Commander 1v1 mode:");
    eprintln!("  --commander-1v1    Head-to-head Commander between two feed decks");
    eprintln!("  --p0 NAME          Commander name of the first deck");
    eprintln!("  --p1 NAME          Commander name of the second deck");
    eprintln!("  --games N          Games to play (must be even; seats alternate)");
    eprintln!("  --trace            Progress lines and a dump of the last actions when a");
    eprintln!("                     game fails to finish (diagnostic output only)");
    eprintln!("  --game-timeout S   Per-game wall budget in seconds, with or without");
    eprintln!("                     --trace (default 300 for 1v1; off for --commander-suite)");
    eprintln!("  --output PATH      Write JSON report to PATH (default: stdout)");
    eprintln!();
    eprintln!("Commander suite mode:");
    eprintln!("  --commander-suite  Run 4-player Commander candidate-seat rotations");
    eprintln!(
        "  --feed PATH        Feed under data-root (default: feeds/mtggoldfish-commander.json)"
    );
    eprintln!("  --games N          Games per candidate seat (default: 4)");
    eprintln!(
        "  --output PATH      Write JSON report to PATH (default: target/commander-suite-results.json)"
    );
    eprintln!();
    eprintln!("Compare mode (CI regression gate):");
    eprintln!("  compare BASELINE CURRENT   Diff two suite reports");
    eprintln!("  reports paired-seed flips and a binomial sign-test p-value");
    eprintln!("  Exit code 0 if no regressions; 1 if any matchup FAILs; 2 if the two");
    eprintln!("  reports cannot be compared at all (the refusal is printed to stdout).");
}

/// Parse `compare` subcommand arguments and run the comparison. Returns the
/// process exit code.
fn run_compare(args: &[String]) -> i32 {
    // args[0] == "compare"
    if args.len() < 3 {
        eprintln!("Usage: ai-duel compare BASELINE.json CURRENT.json");
        return 2;
    }
    let baseline_path = PathBuf::from(&args[1]);
    let current_path = PathBuf::from(&args[2]);

    for arg in args.iter().skip(3) {
        if arg.starts_with("--") {
            eprintln!("Unknown compare option: {arg}");
            return 2;
        }
    }

    // A report that cannot be READ is refused on the same terms as one that cannot be COMPARED.
    // Every other refusal on this path publishes a stdout body; an arm that spoke only to stderr
    // would hand a caller redirecting stdout — the only way this command is used in CI — an empty
    // file and no statement of what failed.
    //
    // The path stays on stderr because `CompareError` carries the cause but not the file, and a
    // refusal that says "I/O error" without naming which of two inputs it was reading is not
    // actionable.
    let baseline = match load_report(&baseline_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to load baseline {}: {e}", baseline_path.display());
            print!("{}", render_error_markdown(&e));
            return 2;
        }
    };
    let current = match load_report(&current_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to load current {}: {e}", current_path.display());
            print!("{}", render_error_markdown(&e));
            return 2;
        }
    };

    // Third caller of the same two statements, and it had the same defect: a refusal spoke
    // only to stderr, so anything redirecting this command's stdout got an empty file and no
    // statement of what failed. Routed through the shared emitter rather than repaired in
    // place — `tests/gate_cli.rs` drives THIS binary, because it is the only one of the three
    // that needs no card database, so the contract is bound at a real process boundary for
    // milliseconds instead of a full suite run.
    let comparison = compare_reports(&baseline, &current, &CompareOptions);
    if let Err(e) = &comparison {
        eprintln!("Compare failed: {e}");
    }
    emit_gate_verdict(&comparison)
}

fn list_matchups() {
    eprintln!("Available matchups:");
    eprintln!();
    for spec in all_matchups() {
        eprintln!("  {:30}  {} vs {}", spec.id, spec.p0_label, spec.p1_label);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    fn eliminated(seats: &[u8]) -> Vec<PlayerId> {
        seats.iter().copied().map(PlayerId).collect()
    }

    // ---------------------------------------------------------------- CLI parsing

    /// The defect this rework exists for: the parser used to rescan argv for the
    /// first token without a `--` prefix, so a flags-first invocation handed it
    /// `Krenko, Mob Boss` and it tried to load `Krenko, Mob Boss/card-data.json`.
    #[test]
    fn a_flag_value_is_never_taken_as_the_data_root() {
        let cli = parse_cli(&args(&[
            "--commander-1v1",
            "--p0",
            "Krenko, Mob Boss",
            "--p1",
            "Giada, Font of Hope",
        ]))
        .expect("flags-first invocation parses");

        assert_eq!(cli.cards_root, None, "no positional was given");
        assert_eq!(cli.duel_p0.as_deref(), Some("Krenko, Mob Boss"));
        assert_eq!(cli.duel_p1.as_deref(), Some("Giada, Font of Hope"));
        assert_eq!(
            resolve_cards_root(cli.cards_root, Some("client/public".to_string())),
            Some(PathBuf::from("client/public")),
            "with no positional the root falls back to PHASE_CARDS_PATH"
        );
    }

    #[test]
    fn the_positional_root_is_accepted_on_either_side_of_the_flags() {
        let leading = parse_cli(&args(&["client/public", "--suite", "--games", "4"]))
            .expect("root-first parses");
        let trailing = parse_cli(&args(&["--suite", "--games", "4", "client/public"]))
            .expect("root-last parses");

        assert_eq!(leading.cards_root, Some(PathBuf::from("client/public")));
        assert_eq!(trailing.cards_root, Some(PathBuf::from("client/public")));
        assert_eq!(leading.mode, Mode::Suite);
        assert_eq!(leading.games, Some(4));
    }

    #[test]
    fn an_explicit_root_wins_over_the_environment_fallback() {
        assert_eq!(
            resolve_cards_root(
                Some(PathBuf::from("explicit")),
                Some("from-env".to_string())
            ),
            Some(PathBuf::from("explicit"))
        );
        assert_eq!(resolve_cards_root(None, None), None);
        assert_eq!(
            resolve_cards_root(None, Some(String::new())),
            None,
            "an empty PHASE_CARDS_PATH is unset, not a relative root"
        );
    }

    #[test]
    fn a_second_positional_is_rejected_rather_than_silently_ignored() {
        let err = parse_cli(&args(&["client/public", "stray"])).expect_err("rejects");
        assert!(err.contains("stray"), "{err}");
        assert!(err.contains("client/public"), "{err}");
    }

    #[test]
    fn unknown_flags_are_rejected() {
        let err = parse_cli(&args(&["--commander-2v2"])).expect_err("rejects");
        assert!(err.contains("--commander-2v2"), "{err}");
    }

    #[test]
    fn a_flag_missing_its_value_is_rejected() {
        for flag in ["--p0", "--p1", "--seed", "--games", "--output", "--feed"] {
            let Err(err) = parse_cli(&args(&[flag])) else {
                panic!("{flag} at the end of argv must be refused");
            };
            assert!(err.contains(flag), "{err}");
        }
    }

    #[test]
    fn a_malformed_numeric_value_is_rejected_instead_of_defaulting() {
        // `--seed 2o` previously left the seed time-based, silently destroying the
        // reproducibility the flag exists to give.
        let err = parse_cli(&args(&["--seed", "2o"])).expect_err("rejects");
        assert!(err.contains("--seed"), "{err}");
        assert!(err.contains("2o"), "{err}");
    }

    #[test]
    fn list_matchups_is_a_mode_and_needs_no_data_root() {
        let cli = parse_cli(&args(&["--list-matchups"])).expect("parses");
        assert_eq!(cli.mode, Mode::ListMatchups);
        assert_eq!(cli.cards_root, None);
    }

    // ------------------------------------------------- timeout / trace separation

    #[test]
    fn a_game_timeout_is_carried_without_trace() {
        let cli = parse_cli(&args(&[
            "--commander-1v1",
            "--p0",
            "A",
            "--p1",
            "B",
            "--game-timeout",
            "45",
        ]))
        .expect("parses");

        assert!(!cli.trace, "no --trace was given");
        assert_eq!(cli.game_timeout, Some(Duration::from_secs(45)));

        let budget = duel_budget(cli.game_timeout);
        assert_eq!(budget.wall, Some(Duration::from_secs(45)));
        assert_eq!(
            budget.exceeded(Duration::from_secs(45), 0, 0),
            Some(StopReason::WallTimeout),
            "the wall budget bounds the game with tracing off"
        );
    }

    #[test]
    fn a_duel_always_carries_a_wall_budget_and_a_stall_cutoff() {
        let budget = duel_budget(None);
        assert_eq!(budget.wall, Some(DEFAULT_GAME_TIMEOUT));
        assert_eq!(budget.stall_actions, Some(DUEL_STALL_ACTIONS));
        assert_eq!(budget.max_actions, COMMANDER_MAX_TOTAL_ACTIONS);
    }

    #[test]
    fn a_zero_game_timeout_is_rejected() {
        let err = parse_cli(&args(&["--game-timeout", "0"])).expect_err("rejects");
        assert!(err.contains("--game-timeout"), "{err}");
    }

    #[test]
    fn an_unbounded_budget_only_stops_on_the_action_cap() {
        let budget = GameBudget {
            max_actions: 100,
            wall: None,
            stall_actions: None,
        };
        assert_eq!(budget.exceeded(Duration::from_secs(86_400), 99, 99), None);
        assert_eq!(
            budget.exceeded(Duration::ZERO, 100, 0),
            Some(StopReason::ActionCap)
        );
    }

    #[test]
    fn the_stall_cutoff_only_fires_when_it_is_configured() {
        let unwatched = GameBudget {
            max_actions: usize::MAX,
            wall: None,
            stall_actions: None,
        };
        let watched = GameBudget {
            stall_actions: Some(40),
            ..unwatched
        };
        assert_eq!(unwatched.exceeded(Duration::ZERO, 1_000, 1_000), None);
        assert_eq!(
            watched.exceeded(Duration::ZERO, 1_000, 40),
            Some(StopReason::StalledSameTurn)
        );
        assert_eq!(watched.exceeded(Duration::ZERO, 1_000, 39), None);
    }

    #[test]
    fn the_action_cap_outranks_the_wall_budget() {
        let budget = GameBudget {
            max_actions: 10,
            wall: Some(Duration::from_secs(1)),
            stall_actions: Some(1),
        };
        assert_eq!(
            budget.exceeded(Duration::from_secs(60), 10, 10),
            Some(StopReason::ActionCap)
        );
    }

    // ------------------------------------------------------ seat bookkeeping

    #[test]
    fn elimination_bookkeeping_sizes_itself_from_the_seat_count() {
        // Commander seats 2..=6. Every seat but the last goes out, one per turn.
        for seats in [2u8, 4, 6] {
            let mut ledger = EliminationLedger::new(seats);
            let survivor = PlayerId(seats - 1);
            let mut dead = Vec::new();
            for seat in 0..seats - 1 {
                dead.push(seat);
                ledger.observe(&eliminated(&dead), u32::from(seat) + 3);
            }

            assert_eq!(
                ledger.elimination_order(PlayerId(0)),
                1,
                "first out at {seats} seats"
            );
            assert_eq!(
                ledger.elimination_order(survivor),
                seats,
                "the survivor's order is the seat count, not a fixed 4"
            );
            assert_eq!(ledger.survival_turn(PlayerId(0), 99), 3);
            assert_eq!(
                ledger.survival_turn(survivor, 99),
                99,
                "a survivor's survival turn is the final turn"
            );
        }
    }

    /// The pre-rework `[None; 4]` panicked the moment `PlayerId(4)` was eliminated.
    #[test]
    fn six_seats_record_every_elimination_without_panicking() {
        let mut ledger = EliminationLedger::new(6);
        ledger.observe(&eliminated(&[5, 4]), 12);
        ledger.observe(&eliminated(&[5, 4, 3]), 15);

        assert_eq!(ledger.survival_turn(PlayerId(5), 40), 12);
        assert_eq!(ledger.survival_turn(PlayerId(3), 40), 15);
        assert_eq!(ledger.elimination_order(PlayerId(5)), 1);
        assert_eq!(ledger.elimination_order(PlayerId(4)), 2);
        assert_eq!(ledger.elimination_order(PlayerId(3)), 3);
        assert_eq!(ledger.elimination_order(PlayerId(0)), 6);
    }

    #[test]
    fn a_seat_is_recorded_once_even_though_the_driver_polls_every_iteration() {
        let mut ledger = EliminationLedger::new(4);
        ledger.observe(&eliminated(&[2]), 7);
        ledger.observe(&eliminated(&[2]), 9);
        ledger.observe(&eliminated(&[2, 0]), 9);

        assert_eq!(
            ledger.survival_turn(PlayerId(2), 50),
            7,
            "the first observation wins"
        );
        assert_eq!(ledger.elimination_order(PlayerId(2)), 1);
        assert_eq!(ledger.elimination_order(PlayerId(0)), 2);
    }

    // ----------------------------------------------------------- paired seeds

    #[test]
    fn adjacent_games_share_a_seed_and_swap_the_seats() {
        for pair in 0..4usize {
            let (even, odd) = (pair * 2, pair * 2 + 1);
            assert_eq!(
                paired_seed(1_000, even),
                paired_seed(1_000, odd),
                "pair {pair} must replay one seed"
            );
            assert!(deck0_on_the_play(even), "deck 0 leads the even game");
            assert!(!deck0_on_the_play(odd), "deck 1 leads the odd game");
        }
    }

    #[test]
    fn separate_pairs_use_separate_seeds() {
        assert_eq!(paired_seed(1_000, 0), 1_000);
        assert_eq!(paired_seed(1_000, 2), 1_001);
        assert_eq!(paired_seed(1_000, 3), 1_001);
        assert_eq!(paired_seed(1_000, 4), 1_002);
        assert_eq!(
            paired_seed(u64::MAX, 2),
            0,
            "the seed schedule wraps rather than panicking"
        );
    }

    /// A seat swap is only a controlled experiment if everything except the seat
    /// is held fixed, and the AI's measurement nonce is part of "everything".
    #[test]
    fn the_measurement_nonce_follows_the_role_not_the_seat() {
        // 1v1: the same deck draws the same nonce from either seat.
        assert_eq!(measurement_nonce(7, PlayerId(0), PlayerId(0)), 7);
        assert_eq!(measurement_nonce(7, PlayerId(1), PlayerId(1)), 7);
        assert_eq!(measurement_nonce(7, PlayerId(1), PlayerId(0)), 8);
        assert_eq!(measurement_nonce(7, PlayerId(0), PlayerId(1)), 8);
    }

    #[test]
    fn every_seat_draws_a_distinct_nonce_wherever_the_candidate_sits() {
        for candidate in 0..COMMANDER_SUITE_SEATS {
            let nonces: HashSet<u64> = (0..COMMANDER_SUITE_SEATS)
                .map(|seat| measurement_nonce(100, PlayerId(seat), PlayerId(candidate)))
                .collect();
            assert_eq!(
                nonces.len(),
                usize::from(COMMANDER_SUITE_SEATS),
                "candidate {candidate} must not share a nonce with a baseline"
            );
            assert_eq!(
                measurement_nonce(100, PlayerId(candidate), PlayerId(candidate)),
                100,
                "the candidate anchors on the game seed from every seat"
            );
        }
    }

    #[test]
    fn an_odd_or_empty_game_count_is_refused() {
        assert!(validate_duel_games(4).is_ok());
        let odd = validate_duel_games(3).expect_err("odd is refused");
        assert!(odd.contains('3'), "{odd}");
        assert!(validate_duel_games(0).is_err());
    }

    // --------------------------------------------------------- typed outcomes

    #[test]
    fn a_finished_game_is_decided_even_if_a_guard_fired_on_the_same_iteration() {
        let outcome = classify_outcome(
            Some(StopReason::WallTimeout),
            &WaitingFor::GameOver {
                winner: Some(PlayerId(1)),
            },
        );
        assert_eq!(outcome, GameOutcome::Decided(PlayerId(1)));
        assert!(outcome.completed());
        assert_eq!(outcome.stop_reason(), None);
    }

    #[test]
    fn a_draw_is_a_completed_game_and_not_an_abandoned_run() {
        let outcome = classify_outcome(None, &WaitingFor::GameOver { winner: None });
        assert_eq!(outcome, GameOutcome::Draw);
        assert!(outcome.completed());
        assert_eq!(outcome.winner(), None);
        assert_eq!(outcome.stop_reason(), None);
    }

    #[test]
    fn an_abandoned_game_keeps_its_reason_and_is_never_a_winner() {
        let parked = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let outcome = classify_outcome(Some(StopReason::ActionCap), &parked);
        assert_eq!(outcome, GameOutcome::Stopped(StopReason::ActionCap));
        assert!(!outcome.completed());
        assert_eq!(outcome.winner(), None);
        assert_eq!(outcome.stop_reason(), Some(StopReason::ActionCap));
    }

    /// A loop that exits with no recorded reason and no `GameOver` state is an AI
    /// that produced no action — the one remaining way out of the driver loop.
    #[test]
    fn a_reasonless_stop_is_reported_as_no_legal_actions_not_as_unknown() {
        let outcome = classify_outcome(
            None,
            &WaitingFor::Priority {
                player: PlayerId(1),
            },
        );
        assert_eq!(outcome, GameOutcome::Stopped(StopReason::NoLegalActions));
    }

    #[test]
    fn stop_reason_labels_are_stable_and_distinct() {
        let all = [
            StopReason::ActionCap,
            StopReason::WallTimeout,
            StopReason::StalledSameTurn,
            StopReason::NoLegalActions,
        ];
        let labels: HashSet<&str> = all.iter().map(|reason| reason.label()).collect();
        assert_eq!(
            labels.len(),
            all.len(),
            "every reason reports its own label"
        );
        assert_eq!(StopReason::ActionCap.label(), "action_cap");
        assert_eq!(StopReason::WallTimeout.label(), "wall_timeout");
        assert_eq!(StopReason::StalledSameTurn.label(), "stalled_same_turn");
        assert_eq!(StopReason::NoLegalActions.label(), "no_legal_actions");
    }

    // ------------------------------------------------------- reporting labels

    #[test]
    fn the_duel_label_follows_the_seat_the_deck_actually_sat_in() {
        // Odd game: deck 0 is in seat 1, so a seat-1 win is a deck-0 win.
        let deck0_seat = PlayerId(1);
        assert_eq!(
            duel_result_label(GameOutcome::Decided(PlayerId(1)), deck0_seat, "p0", "p1"),
            Some("p0")
        );
        assert_eq!(
            duel_result_label(GameOutcome::Decided(PlayerId(0)), deck0_seat, "p0", "p1"),
            Some("p1")
        );
        assert_eq!(
            duel_result_label(GameOutcome::Draw, deck0_seat, "p0", "p1"),
            None
        );
        assert_eq!(
            duel_result_label(
                GameOutcome::Stopped(StopReason::WallTimeout),
                deck0_seat,
                "p0",
                "p1"
            ),
            None
        );
    }

    #[test]
    fn the_disposition_separates_a_draw_from_an_abandoned_run() {
        let seat = PlayerId(0);
        assert_eq!(
            duel_game_disposition(GameOutcome::Decided(PlayerId(0)), seat, "p0", "p1"),
            "p0"
        );
        assert_eq!(
            duel_game_disposition(GameOutcome::Draw, seat, "p0", "p1"),
            "draw"
        );
        assert_eq!(
            duel_game_disposition(
                GameOutcome::Stopped(StopReason::StalledSameTurn),
                seat,
                "p0",
                "p1"
            ),
            "INCOMPLETE"
        );
    }

    #[test]
    fn the_trace_line_labels_the_wait_from_the_engine_owned_mapping() {
        let line = trace_progress_line(
            7,
            4_000,
            Duration::from_secs(12),
            &WaitingFor::Priority {
                player: PlayerId(0),
            },
        );
        assert!(
            line.contains("waiting_for Priority"),
            "the label is WaitingFor::variant_name(), not parsed Debug output: {line}"
        );
        assert!(line.contains("turn   7"), "{line}");
        assert!(line.contains("actions    4000"), "{line}");
        assert!(line.contains("12s"), "{line}");
    }
}
