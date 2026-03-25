use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use engine::database::CardDatabase;
use engine::game::deck_loading::{
    load_deck_into_state, resolve_deck_list, DeckList, PlayerDeckList, PlayerDeckPayload,
};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::log::{GameLogEntry, LogCategory, LogSegment};
use engine::types::player::PlayerId;
use phase_ai::auto_play::run_ai_actions;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};

const MAX_TOTAL_ACTIONS: usize = 10_000;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut verbose = false;
    let mut batch: Option<usize> = None;
    let mut seed: Option<u64> = None;
    let mut difficulty = AiDifficulty::Medium;

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
        eprintln!("Usage: ai-duel <data-root> [OPTIONS]");
        eprintln!("  Or set PHASE_CARDS_PATH environment variable");
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --verbose          Print every action (full trace)");
        eprintln!("  --batch N          Run N games, print summary only");
        eprintln!("  --seed S           RNG seed (default: time-based)");
        eprintln!("  --difficulty LEVEL VeryEasy|Easy|Medium|Hard|VeryHard (default: Medium)");
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

    let deck_list = build_starter_decks();
    let payload = resolve_deck_list(&db, &deck_list);

    validate_deck(&payload.player, 60, "Red Aggro (P0)");
    validate_deck(&payload.opponent, 60, "Green Midrange (P1)");

    let base_seed = seed.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    });

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
                Some(p) => eprintln!(
                    "\nGame over — Player {} wins on turn {turns} ({elapsed}ms)",
                    p.0
                ),
                None => eprintln!("\nGame over — draw/aborted on turn {turns} ({elapsed}ms)"),
            }
        }
    }

    if is_batch {
        let n = game_count;
        let avg_turns = total_turns as f64 / n as f64;
        let avg_ms = total_duration_ms as f64 / n as f64;
        eprintln!("\nResults ({n} games, seed: {base_seed}, difficulty: {difficulty:?}):");
        eprintln!(
            "  Player 0 (Red Aggro) wins: {p0_wins:>4} ({:.1}%)",
            p0_wins as f64 / n as f64 * 100.0
        );
        eprintln!(
            "  Player 1 (Green Mid) wins: {p1_wins:>4} ({:.1}%)",
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
    payload: &engine::game::deck_loading::DeckPayload,
    seed: u64,
    difficulty: AiDifficulty,
    verbose: bool,
    silent: bool,
) -> (Option<PlayerId>, u32) {
    let mut state = GameState::new_two_player(seed);
    load_deck_into_state(&mut state, payload);
    engine::game::engine::start_game(&mut state);

    let ai_players: HashSet<PlayerId> = [PlayerId(0), PlayerId(1)].into_iter().collect();
    let config = create_config_for_players(difficulty, Platform::Native, 2);
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
                    // Print turn header when log entries reference a new turn
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

fn repeat(name: &str, count: usize) -> Vec<String> {
    vec![name.to_string(); count]
}

fn build_starter_decks() -> DeckList {
    // Red Aggro: 20 lands, 20 creatures, 20 spells = 60
    let mut red = Vec::with_capacity(60);
    red.extend(repeat("Mountain", 20));
    red.extend(repeat("Goblin Guide", 4));
    red.extend(repeat("Monastery Swiftspear", 4));
    red.extend(repeat("Raging Goblin", 4));
    red.extend(repeat("Jackal Pup", 4));
    red.extend(repeat("Mogg Fanatic", 4));
    red.extend(repeat("Lightning Bolt", 4));
    red.extend(repeat("Shock", 4));
    red.extend(repeat("Lava Spike", 4));
    red.extend(repeat("Searing Spear", 4));
    red.extend(repeat("Skullcrack", 4));

    // Green Midrange: 22 lands, 22 creatures, 16 spells = 60
    let mut green = Vec::with_capacity(60);
    green.extend(repeat("Forest", 22));
    green.extend(repeat("Llanowar Elves", 4));
    green.extend(repeat("Elvish Mystic", 4));
    green.extend(repeat("Grizzly Bears", 4));
    green.extend(repeat("Kalonian Tusker", 4));
    green.extend(repeat("Centaur Courser", 4));
    green.extend(repeat("Leatherback Baloth", 2));
    green.extend(repeat("Giant Growth", 4));
    green.extend(repeat("Rancor", 4));
    green.extend(repeat("Titanic Growth", 4));
    green.extend(repeat("Rabid Bite", 4));

    DeckList {
        player: PlayerDeckList {
            main_deck: red,
            sideboard: Vec::new(),
            commander: Vec::new(),
        },
        opponent: PlayerDeckList {
            main_deck: green,
            sideboard: Vec::new(),
            commander: Vec::new(),
        },
        ai_decks: Vec::new(),
    }
}
