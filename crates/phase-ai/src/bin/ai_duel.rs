use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use engine::database::CardDatabase;
use engine::game::deck_loading::{load_deck_into_state, DeckPayload, PlayerDeckPayload};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::log::{GameLogEntry, LogCategory, LogSegment};
use engine::types::player::PlayerId;
use phase_ai::auto_play::run_ai_actions;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
use phase_ai::duel_suite::compare::{
    compare as compare_reports, load_report, print_markdown as print_compare_markdown,
    CompareOptions,
};
use phase_ai::duel_suite::run::{resolve_matchup, run_suite, AttributionMode, SuiteOptions};
use phase_ai::duel_suite::{all_matchups, find_matchup};

const MAX_TOTAL_ACTIONS: usize = 10_000;

enum Mode {
    Single,
    Suite,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // `compare` subcommand: `ai-duel compare BASELINE CURRENT [--warn-pp N] [--fail-pp N]`
    // Does not require a card database or any of the single/suite-mode flags.
    if args.get(1).map(|s| s.as_str()) == Some("compare") {
        let exit = run_compare(&args[1..]);
        std::process::exit(exit);
    }

    let mut verbose = false;
    let mut batch: Option<usize> = None;
    let mut seed: Option<u64> = None;
    let mut difficulty = AiDifficulty::Medium;
    let mut matchup = "red-vs-green".to_string();
    let mut mode = Mode::Single;
    let mut suite_games: Option<usize> = None;
    let mut output: Option<PathBuf> = None;
    let mut suite_filter: Option<String> = None;
    let mut attribution = AttributionMode::Disabled;

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
            "--matchup" => {
                if let Some(m) = args_iter.next() {
                    matchup = m.clone();
                }
            }
            "--suite" => mode = Mode::Suite,
            "--games" => suite_games = args_iter.next().and_then(|v| v.parse().ok()),
            "--output" => output = args_iter.next().map(PathBuf::from),
            "--suite-filter" => suite_filter = args_iter.next().cloned(),
            "--show-attribution" => attribution = AttributionMode::Enabled,
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
            options.output_path = output_path.clone();
            options.filter = suite_filter;
            options.attribution = attribution;
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
    // Pin deterministic mode for regression runs: search is bounded by
    // max_nodes only, so duel outcomes don't observe wall-clock variance
    // across hardware. Production code leaves this off to use time budgets.
    let config = create_config_for_players(difficulty, Platform::Native, 2).into_deterministic();
    let ai_configs: HashMap<PlayerId, _> = [(PlayerId(0), config.clone()), (PlayerId(1), config)]
        .into_iter()
        .collect();

    let mut total_actions: usize = 0;
    let mut last_turn: u32 = 0;

    loop {
        let results = run_ai_actions(&mut state, &ai_players, &ai_configs);
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
    match s.to_lowercase().as_str() {
        "veryeasy" => AiDifficulty::VeryEasy,
        "easy" => AiDifficulty::Easy,
        "medium" => AiDifficulty::Medium,
        "hard" => AiDifficulty::Hard,
        "veryhard" => AiDifficulty::VeryHard,
        _ => {
            eprintln!("Unknown difficulty '{s}', using Medium");
            AiDifficulty::Medium
        }
    }
}

fn print_usage() {
    eprintln!("Usage: ai-duel <data-root> [OPTIONS]");
    eprintln!("       ai-duel compare BASELINE.json CURRENT.json [--warn-pp N] [--fail-pp N]");
    eprintln!("  Or set PHASE_CARDS_PATH environment variable");
    eprintln!();
    eprintln!("Single-matchup mode:");
    eprintln!("  --verbose          Print every action (full trace)");
    eprintln!("  --batch N          Run N games, print summary only");
    eprintln!("  --seed S           RNG seed (default: time-based)");
    eprintln!("  --difficulty LEVEL VeryEasy|Easy|Medium|Hard|VeryHard (default: Medium)");
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
    eprintln!();
    eprintln!("Compare mode (CI regression gate):");
    eprintln!("  compare BASELINE CURRENT   Diff two suite reports");
    eprintln!("  --warn-pp N                Winrate drift warn threshold in pp (default: 8.0)");
    eprintln!("  --fail-pp N                Winrate drift fail threshold in pp (default: 15.0)");
    eprintln!("  Exit code 0 if no regressions; 1 if any matchup FAILs.");
}

/// Parse `compare` subcommand arguments and run the comparison. Returns the
/// process exit code.
fn run_compare(args: &[String]) -> i32 {
    // args[0] == "compare"
    if args.len() < 3 {
        eprintln!("Usage: ai-duel compare BASELINE.json CURRENT.json [--warn-pp N] [--fail-pp N]");
        return 2;
    }
    let baseline_path = PathBuf::from(&args[1]);
    let current_path = PathBuf::from(&args[2]);

    let mut options = CompareOptions::default();
    let mut iter = args.iter().skip(3);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--warn-pp" => {
                if let Some(v) = iter.next().and_then(|v| v.parse().ok()) {
                    options.warn_pp = v;
                }
            }
            "--fail-pp" => {
                if let Some(v) = iter.next().and_then(|v| v.parse().ok()) {
                    options.fail_pp = v;
                }
            }
            _ => {}
        }
    }

    let baseline = match load_report(&baseline_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to load baseline {}: {e}", baseline_path.display());
            return 2;
        }
    };
    let current = match load_report(&current_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to load current {}: {e}", current_path.display());
            return 2;
        }
    };

    let report = match compare_reports(&baseline, &current, &options) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Compare failed: {e}");
            return 2;
        }
    };
    print_compare_markdown(&report);
    if report.any_fail() {
        1
    } else {
        0
    }
}

fn list_matchups() {
    eprintln!("Available matchups:");
    eprintln!();
    for spec in all_matchups() {
        eprintln!("  {:30}  {} vs {}", spec.id, spec.p0_label, spec.p1_label);
    }
}
