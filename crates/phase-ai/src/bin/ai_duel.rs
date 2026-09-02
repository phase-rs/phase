// pod-lab loop-3 Q5: native-binary throughput lever, gated in Cargo.toml so
// wasm32 builds of this crate's lib (pulled in by engine-wasm/draft-wasm)
// never see it.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use engine::database::CardDatabase;
use engine::game::deck_loading::{
    load_deck_into_state, resolve_deck_list, DeckList, DeckPayload, PlayerDeckList,
    PlayerDeckPayload,
};
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::log::{GameLogEntry, LogCategory, LogSegment};
use engine::types::player::PlayerId;
use phase_ai::auto_play::run_ai_actions;
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
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // `compare` subcommand: `ai-duel compare BASELINE CURRENT`
    // Does not require a card database or any of the single/suite-mode flags.
    if args.get(1).map(|s| s.as_str()) == Some("compare") {
        let exit = run_compare(&args[1..]);
        std::process::exit(exit);
    }

    let mut verbose = false;
    let mut batch: Option<usize> = None;
    let mut seed: Option<u64> = None;
    let mut difficulty = AiDifficulty::Medium;
    let mut baseline_difficulty = AiDifficulty::Medium;
    let mut matchup = "red-vs-green".to_string();
    let mut mode = Mode::Single;
    let mut suite_games: Option<usize> = None;
    let mut output: Option<PathBuf> = None;
    let mut suite_filter: Option<String> = None;
    let mut attribution = AttributionMode::Disabled;
    let mut harvest_output: Option<PathBuf> = None;
    let mut commander_feed = "feeds/mtggoldfish-commander.json".to_string();
    let mut duel_p0: Option<String> = None;
    let mut duel_p1: Option<String> = None;
    let mut trace = false;
    let mut game_timeout_secs: u64 = 300;

    let mut args_iter = args.iter().skip(1).peekable();
    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "--verbose" => verbose = true,
            "--batch" => batch = args_iter.next().and_then(|v| v.parse().ok()),
            "--seed" => seed = args_iter.next().and_then(|v| v.parse().ok()),
            "--difficulty" => {
                if let Some(level) = args_iter.next() {
                    difficulty = parse_difficulty(level);
                }
            }
            "--baseline-difficulty" => {
                if let Some(level) = args_iter.next() {
                    baseline_difficulty = parse_difficulty(level);
                }
            }
            "--matchup" => {
                if let Some(m) = args_iter.next() {
                    matchup = m.clone();
                }
            }
            "--suite" => mode = Mode::Suite,
            "--commander-suite" => mode = Mode::CommanderSuite,
            "--commander-1v1" => mode = Mode::CommanderDuel,
            "--trace" => trace = true,
            "--game-timeout" => {
                game_timeout_secs = args_iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(game_timeout_secs);
            }
            "--p0" => duel_p0 = args_iter.next().cloned(),
            "--p1" => duel_p1 = args_iter.next().cloned(),
            "--games" => suite_games = args_iter.next().and_then(|v| v.parse().ok()),
            "--output" => output = args_iter.next().map(PathBuf::from),
            "--suite-filter" => suite_filter = args_iter.next().cloned(),
            "--show-attribution" => attribution = AttributionMode::Enabled,
            "--harvest" => harvest_output = args_iter.next().map(PathBuf::from),
            "--feed" => {
                if let Some(feed) = args_iter.next() {
                    commander_feed = feed.clone();
                }
            }
            "--list-matchups" => {
                list_matchups();
                return;
            }
            _ => {}
        }
    }

    let path = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .cloned()
        .or_else(|| std::env::var("PHASE_CARDS_PATH").ok())
        .map(PathBuf::from);

    let Some(path) = path else {
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

    let base_seed = seed.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    });

    match mode {
        Mode::Suite => {
            let games = suite_games.unwrap_or(10);
            let output_path =
                output.unwrap_or_else(|| PathBuf::from("target/duel-suite-results.json"));
            let mut options = SuiteOptions::new(difficulty, games, base_seed);
            options.output = ReportSink::Create(output_path.clone());
            options.filter = suite_filter;
            options.attribution = attribution;
            options.harvest_output = harvest_output;
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
            let games = suite_games.unwrap_or(4);
            run_commander_suite(
                &db,
                CommanderSuiteOptions {
                    cards_root: &path,
                    feed: &commander_feed,
                    games_per_seat: games,
                    base_seed,
                    candidate_difficulty: difficulty,
                    baseline_difficulty,
                    output,
                },
            );
        }
        Mode::CommanderDuel => {
            let (Some(p0), Some(p1)) = (duel_p0, duel_p1) else {
                eprintln!("--commander-1v1 requires --p0 NAME and --p1 NAME");
                std::process::exit(2);
            };
            run_commander_duel(
                &db,
                CommanderDuelOptions {
                    cards_root: &path,
                    feed: &commander_feed,
                    p0: &p0,
                    p1: &p1,
                    games: suite_games.unwrap_or(4),
                    base_seed,
                    difficulty,
                    baseline_difficulty,
                    output,
                    trace: trace.then(|| TraceOptions {
                        every: 2_000,
                        ring: 40,
                        stall_actions: 40_000,
                        wall_budget: std::time::Duration::from_secs(game_timeout_secs),
                    }),
                },
            );
        }
        Mode::Single => {
            run_single(&db, &matchup, batch, base_seed, difficulty, verbose);
        }
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
}

fn run_commander_suite(db: &CardDatabase, options: CommanderSuiteOptions<'_>) {
    let deck_lists = load_commander_decks(db, options.cards_root, options.feed, Some(4));
    if deck_lists.len() < 4 {
        eprintln!(
            "Commander suite needs at least 4 resolvable decks, found {}",
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
    for candidate_seat in 0..4 {
        let candidate = PlayerId(candidate_seat as u8);
        let mut wins = 0usize;
        let mut total_survival_turns = 0u64;
        let mut total_elimination_order = 0u64;

        for game_idx in 0..options.games_per_seat {
            let seed = options
                .base_seed
                .wrapping_add(candidate_seat as u64 * 10_000)
                .wrapping_add(game_idx as u64);
            let result = run_commander_game(
                &payload,
                seed,
                candidate,
                options.candidate_difficulty,
                options.baseline_difficulty,
                4,
                None,
            );
            if result.winner == Some(candidate) {
                wins += 1;
            }
            total_survival_turns += result.candidate_survival_turn as u64;
            total_elimination_order += result.candidate_elimination_order as u64;
            all_games.push(serde_json::json!({
                "candidate_seat": candidate.0,
                "seed": seed,
                "winner": result.winner.map(|p| p.0),
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
            "elimination_order": "1 = first eliminated, 4 = winner or last survivor",
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

struct CommanderGameResult {
    winner: Option<PlayerId>,
    turns: u32,
    candidate_survival_turn: u32,
    candidate_elimination_order: u8,
    /// True only when the game reached `WaitingFor::GameOver`. False means the
    /// driver loop gave up — the action cap was hit, or the AI produced no legal
    /// action — which is a run that failed to complete rather than a draw, and
    /// must be reported as such rather than folded into a 0-win result.
    completed: bool,
    /// Why an incomplete game stopped. `None` when `completed`.
    stop_reason: Option<&'static str>,
}

fn run_commander_game(
    payload: &DeckPayload,
    seed: u64,
    candidate: PlayerId,
    candidate_difficulty: AiDifficulty,
    baseline_difficulty: AiDifficulty,
    players: u8,
    trace: Option<&TraceOptions>,
) -> CommanderGameResult {
    let mut state = GameState::new(FormatConfig::commander(), players, seed);
    load_deck_into_state(&mut state, payload);
    engine::game::engine::start_game(&mut state);

    let ai_players: HashSet<PlayerId> = (0..players).map(PlayerId).collect();
    let mut ai_configs: HashMap<PlayerId, _> = HashMap::new();
    for seat in 0..players {
        let player = PlayerId(seat);
        let difficulty = if player == candidate {
            candidate_difficulty
        } else {
            baseline_difficulty
        };
        // Player count is threaded into the AI config too: threat assessment and
        // politics weighting differ between a pod and a duel, so a 1v1 run must
        // not be configured as if three opponents were present.
        ai_configs.insert(
            player,
            create_config_for_players(difficulty, Platform::Native, players)
                .into_measurement(seed.wrapping_add(u64::from(seat))),
        );
    }

    let mut total_actions = 0usize;
    let mut stop_reason: Option<&'static str> = None;
    // Hang diagnosis. A full action trace is useless at this scale (the cap is 200k
    // actions), so instead: periodic progress showing whether the game is advancing at
    // all, a wall-clock budget so a stuck game reports where it stopped instead of being
    // killed from outside, and a ring buffer of recent actions dumped on a bad exit --
    // a repeating cycle is visible immediately in the last few dozen actions.
    let started = Instant::now();
    let mut recent: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let mut last_progress_turn = 0u32;
    let mut actions_at_last_turn_change = 0usize;
    let mut ai_rng = StdRng::seed_from_u64(seed);
    let ai_session = phase_ai::session::AiSession::arc_from_game(&state);
    let mut elimination_turns = [None; 4];
    let mut seen_eliminated = HashSet::new();

    loop {
        for player in &state.eliminated_players {
            if seen_eliminated.insert(*player) {
                elimination_turns[player.0 as usize] = Some(state.turn_number);
            }
        }
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            break;
        }

        let results = run_ai_actions(
            &mut state,
            &ai_players,
            &ai_configs,
            &mut ai_rng,
            &ai_session,
        );
        if results.is_empty() {
            stop_reason = Some("no_legal_actions");
            break;
        }
        if let Some(opts) = trace {
            for r in &results {
                if recent.len() == opts.ring {
                    recent.pop_front();
                }
                recent.push_back(format!(
                    "T{} P{} {:?}",
                    state.turn_number, state.active_player.0, r.action
                ));
            }
        }
        total_actions += results.len();

        if state.turn_number != last_progress_turn {
            last_progress_turn = state.turn_number;
            actions_at_last_turn_change = total_actions;
        }
        if let Some(opts) = trace {
            if total_actions / opts.every != (total_actions - results.len()) / opts.every {
                eprintln!(
                    "    [trace] turn {:>3}  actions {:>7}  {:.0}s  waiting_for {}",
                    state.turn_number,
                    total_actions,
                    started.elapsed().as_secs_f64(),
                    waiting_for_kind(&state.waiting_for),
                );
            }
            // The turn number not moving while actions pile up is the signature of a loop,
            // and it is what distinguishes a stuck game from a merely slow one.
            if total_actions - actions_at_last_turn_change >= opts.stall_actions {
                stop_reason = Some("stalled_same_turn");
                break;
            }
            if started.elapsed() >= opts.wall_budget {
                stop_reason = Some("wall_timeout");
                break;
            }
        }
        if total_actions >= COMMANDER_MAX_TOTAL_ACTIONS {
            stop_reason = Some("action_cap");
            break;
        }
    }

    if let Some(opts) = trace {
        if !matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            eprintln!(
                "    [trace] STOPPED {:?} at turn {} after {} actions ({:.0}s); last {} actions:",
                stop_reason.unwrap_or("unknown"),
                state.turn_number,
                total_actions,
                started.elapsed().as_secs_f64(),
                recent.len()
            );
            for line in &recent {
                eprintln!("      {line}");
            }
            let _ = opts;
        }
    }

    for player in &state.eliminated_players {
        if seen_eliminated.insert(*player) {
            elimination_turns[player.0 as usize] = Some(state.turn_number);
        }
    }
    let completed = matches!(state.waiting_for, WaitingFor::GameOver { .. });
    let winner = match &state.waiting_for {
        WaitingFor::GameOver { winner } => *winner,
        _ => None,
    };
    let candidate_survival_turn =
        elimination_turns[candidate.0 as usize].unwrap_or(state.turn_number);
    let candidate_elimination_order = state
        .eliminated_players
        .iter()
        .position(|player| *player == candidate)
        .map(|idx| idx as u8 + 1)
        .unwrap_or(4);

    CommanderGameResult {
        winner,
        completed,
        stop_reason: if completed {
            None
        } else {
            stop_reason.or(Some("unknown"))
        },
        turns: state.turn_number,
        candidate_survival_turn,
        candidate_elimination_order,
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
struct TraceOptions {
    /// Emit a progress line every this many actions.
    every: usize,
    /// Keep this many recent actions to dump when a game exits badly.
    ring: usize,
    /// Give up if this many actions pass without the turn number changing.
    stall_actions: usize,
    /// Give up after this much wall clock, so the game reports where it stopped.
    wall_budget: std::time::Duration,
}

/// Human-readable discriminant for the state machine's current wait.
///
/// Debug-formats and keeps the variant name only: the payloads are large and the variant is
/// the part that identifies a loop.
fn waiting_for_kind(w: &WaitingFor) -> String {
    let s = format!("{w:?}");
    s.split(['{', '(', ' ']).next().unwrap_or("?").to_string()
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
    trace: Option<TraceOptions>,
}

/// Head-to-head Commander between two decks from a feed, identified by commander name.
///
/// Seats alternate every game. Commander's first-player advantage is large, and a short
/// unbalanced sample mostly measures who was on the play, so an even `--games` gives each deck
/// the play exactly half the time; an odd count is rejected rather than silently handing one
/// deck an extra turn one.
fn run_commander_duel(db: &CardDatabase, options: CommanderDuelOptions<'_>) {
    if !options.games.is_multiple_of(2) {
        eprintln!(
            "--games must be even for a 1v1 run so both decks get the play equally (got {})",
            options.games
        );
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
    let mut incomplete = 0usize;
    let mut rows = Vec::new();

    for game_idx in 0..options.games {
        // Even games put deck0 on the play (seat 0); odd games swap the seats.
        let deck0_first = game_idx.is_multiple_of(2);
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
        let seed = options.base_seed.wrapping_add(game_idx as u64);
        if options.trace.is_some() {
            eprintln!("  game {game_idx} (seed {seed}) starting...");
        }
        let result = run_commander_game(
            &payload,
            seed,
            deck0_seat,
            options.difficulty,
            options.baseline_difficulty,
            2,
            options.trace.as_ref(),
        );

        let winner_label = if !result.completed {
            incomplete += 1;
            None
        } else if result.winner == Some(deck0_seat) {
            p0_wins += 1;
            Some(options.p0)
        } else if result.winner.is_some() {
            p1_wins += 1;
            Some(options.p1)
        } else {
            // Completed with no winner is a genuine draw, distinct from an incomplete run.
            None
        };
        let on_play = if deck0_first { options.p0 } else { options.p1 };
        rows.push(serde_json::json!({
            "game": game_idx,
            "seed": seed,
            "on_the_play": on_play,
            "winner": winner_label,
            "completed": result.completed,
            "stop_reason": result.stop_reason,
            "turns": result.turns,
        }));
        eprintln!(
            "  game {game_idx}: {} (turns={}, on_play={on_play}){}",
            winner_label.unwrap_or(if result.completed {
                "draw"
            } else {
                "INCOMPLETE"
            }),
            result.turns,
            result
                .stop_reason
                .map(|r| format!(" [{r}]"))
                .unwrap_or_default(),
        );
    }

    let decided = p0_wins + p1_wins;
    eprintln!(
        "{} {}-{} {} ({} decided, {} incomplete of {})",
        options.p0, p0_wins, p1_wins, options.p1, decided, incomplete, options.games
    );

    let report = serde_json::json!({
        "schema_version": 1,
        "mode": "commander_duel",
        "feed": options.feed,
        "p0": options.p0,
        "p1": options.p1,
        "games": options.games,
        "base_seed": options.base_seed,
        "difficulty": format!("{:?}", options.difficulty),
        "p0_wins": p0_wins,
        "p1_wins": p1_wins,
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
    eprintln!("  --trace            Progress lines, stall/wall-clock cutoffs, and a dump of");
    eprintln!("                     the last actions when a game fails to finish");
    eprintln!("  --game-timeout S   Per-game wall budget under --trace (default 300)");
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
