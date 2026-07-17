//! Sanity-check runner: four-player commander game driven entirely by the AI.
//!
//! Loads four commander precons from `feeds/commander-precons.json`, sets up
//! a 4-player commander GameState, and drives every player with the native
//! AI until the game ends (or an action budget is hit). Reports per-turn
//! life totals and the final outcome.
//!
//! Usage:
//!   cargo run --release --bin ai-commander -- client/public
//!   cargo run --release --bin ai-commander -- client/public --seed 7 --difficulty Easy

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use engine::database::CardDatabase;
use engine::game::deck_loading::{
    load_deck_into_state, resolve_deck_list, DeckList, DeckPayload, PlayerDeckList,
};
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;
use phase_ai::auto_play::run_ai_actions;
use phase_ai::config::{create_config_for_players, AiConfig, AiDifficulty, Platform};
use rand::rngs::StdRng;
use rand::SeedableRng;

const MAX_TOTAL_ACTIONS: usize = 200_000;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let cards_path = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| "client/public".to_string());

    let mut seed: u64 = 42;
    let mut difficulty = AiDifficulty::Easy;
    let mut feed: String = "feeds/mtggoldfish-commander.json".to_string();
    let mut args_iter = args.iter().skip(1).peekable();
    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "--seed" => {
                if let Some(v) = args_iter.next() {
                    if let Ok(n) = v.parse::<u64>() {
                        seed = n;
                    }
                }
            }
            "--difficulty" => {
                if let Some(v) = args_iter.next() {
                    difficulty = parse_difficulty(v);
                }
            }
            "--feed" => {
                if let Some(v) = args_iter.next() {
                    feed = v.clone();
                }
            }
            _ => {}
        }
    }

    let export_path = PathBuf::from(&cards_path).join("card-data.json");
    let db = match CardDatabase::from_export(&export_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("failed to load {}: {e}", export_path.display());
            std::process::exit(1);
        }
    };

    let feed_path = PathBuf::from(&cards_path).join(&feed);
    let feed_file = match std::fs::File::open(&feed_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("failed to open {}: {e}", feed_path.display());
            std::process::exit(1);
        }
    };
    let feed_json: serde_json::Value =
        serde_json::from_reader(feed_file).expect("feed is not valid JSON");

    let decks_json = feed_json["decks"].as_array().expect("feed.decks missing");

    println!("=== 4-player Commander AI test ===");
    println!("Feed: {feed}");
    println!("Seed: {seed}   Difficulty: {difficulty:?}");
    println!();

    let mut deck_lists: Vec<PlayerDeckList> = Vec::new();
    // Commander names are populated in PlayerDeckList.commander and resolved
    // by the pipeline — no manual tracking needed.
    for deck in decks_json.iter() {
        if deck_lists.len() == 4 {
            break;
        }
        let deck_name = deck["name"].as_str().unwrap_or("<unnamed>");
        // Two feed conventions:
        //  • Precon-style: `commander: ["Card Name"]` is an array of commander names.
        //  • MTGGoldfish-style: `commander` is null and the deck `name` IS the
        //    commander card name (included in `main`).
        let cmd_names: Vec<String> = match deck["commander"].as_array() {
            Some(arr) if !arr.is_empty() => arr
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect(),
            _ => vec![deck_name.to_string()],
        };
        let primary_cmd = cmd_names[0].clone();

        if db.get_face_by_name(&primary_cmd).is_none() {
            println!("  SKIP {deck_name}: commander '{primary_cmd}' not in card db");
            continue;
        }

        let mut main: Vec<String> = Vec::new();
        for entry in deck["main"].as_array().unwrap() {
            let n = entry["name"].as_str().unwrap();
            let count = entry["count"].as_u64().unwrap() as usize;
            if cmd_names.iter().any(|c| c == n) {
                continue;
            }
            for _ in 0..count {
                main.push(n.to_string());
            }
        }

        println!(
            "  {deck_name}  |  commander: {primary_cmd}  |  main: {} cards",
            main.len()
        );
        deck_lists.push(PlayerDeckList {
            main_deck: main,
            sideboard: vec![],
            commander: cmd_names,
            ..Default::default()
        });
    }

    if deck_lists.len() < 4 {
        eprintln!("need at least 4 precons, found {}", deck_lists.len());
        std::process::exit(1);
    }

    let deck_list = DeckList {
        player: deck_lists[0].clone(),
        opponent: deck_lists[1].clone(),
        ai_decks: vec![deck_lists[2].clone(), deck_lists[3].clone()],
        ..Default::default()
    };
    let payload: DeckPayload = resolve_deck_list(&db, &deck_list);

    let mut state = GameState::new(FormatConfig::commander(), 4, seed);
    load_deck_into_state(&mut state, &payload);

    engine::game::engine::start_game(&mut state);
    println!();
    println!("Game started. {} players.", state.players.len());
    println!();

    let ai_players: HashSet<PlayerId> = (0..4).map(|i| PlayerId(i as u8)).collect();
    let config = create_config_for_players(difficulty, Platform::Native, 4);
    let mut ai_configs: HashMap<PlayerId, AiConfig> = HashMap::new();
    for i in 0..4 {
        ai_configs.insert(PlayerId(i as u8), config.clone());
    }

    let start = Instant::now();
    let dump_log_path = read_dump_env("PHASE_DUMP_LOG");
    let mut game_log: Vec<engine::types::log::GameLogEntry> = Vec::new();
    let dump_actions_path = read_dump_env("PHASE_DUMP_ACTIONS");
    let mut actions_log: Vec<String> = Vec::new();
    let mut total_actions: usize = 0;
    let mut last_turn_reported: u32 = 0;
    let mut aborted = false;
    let mut ai_rng = StdRng::seed_from_u64(seed);
    let ai_session = phase_ai::session::AiSession::arc_from_game(&state);

    loop {
        let mut results = run_ai_actions(
            &mut state,
            &ai_players,
            &ai_configs,
            &mut ai_rng,
            &ai_session,
        );
        if results.is_empty() {
            break;
        }
        if dump_log_path.is_some() {
            for r in &mut results {
                game_log.extend(std::mem::take(&mut r.log_entries));
            }
        }
        if dump_actions_path.is_some() {
            for r in &results {
                actions_log.push(format!("{:?}", r.action));
            }
        }
        total_actions += results.len();

        if state.turn_number != last_turn_reported {
            last_turn_reported = state.turn_number;
            let snapshot: Vec<String> = state
                .players
                .iter()
                .enumerate()
                .map(|(i, p)| format!("P{i}:{}", p.life))
                .collect();
            println!(
                "Turn {:>2} (active P{})  actions={:>6}  {}",
                state.turn_number,
                state.active_player.0,
                total_actions,
                snapshot.join(" ")
            );
            let _ = std::io::stdout().flush();
        }

        if total_actions >= MAX_TOTAL_ACTIONS {
            aborted = true;
            println!();
            println!("ABORT: hit MAX_TOTAL_ACTIONS={MAX_TOTAL_ACTIONS}");
            break;
        }
    }

    let elapsed = start.elapsed();
    println!();
    println!("=== RESULT ===");
    println!("Elapsed: {:.1}s", elapsed.as_secs_f64());
    println!("Total actions: {total_actions}");
    println!("Turns played: {}", state.turn_number);
    println!();

    match &state.waiting_for {
        WaitingFor::GameOver { winner } => {
            println!(
                "Game ended cleanly. Winner: {}",
                winner.map_or("draw".to_string(), |p| format!("P{}", p.0))
            );
        }
        other => {
            println!("Game did NOT reach GameOver. waiting_for = {other:?}");
        }
    }

    println!();
    for (i, p) in state.players.iter().enumerate() {
        let bf_count = state
            .battlefield
            .iter()
            .filter(|oid| {
                state
                    .objects
                    .get(oid)
                    .map(|o| o.owner == PlayerId(i as u8))
                    .unwrap_or(false)
            })
            .count();
        println!(
            "  P{i}  life={:>4}  hand={:>2}  library={:>3}  graveyard={:>3}  battlefield={:>3}",
            p.life,
            p.hand.len(),
            p.library.len(),
            p.graveyard.len(),
            bf_count
        );
    }

    if let Some(path) = &dump_actions_path {
        std::fs::write(path, actions_log.join("\n")).expect("write actions dump");
        println!("Dumped {} actions to {path}", actions_log.len());
    }
    if let Some(path) = &dump_log_path {
        let json = serde_json::to_string(&game_log).expect("serialize game log");
        std::fs::write(path, json).expect("write game log dump");
        println!("Dumped {} game-log entries to {path}", game_log.len());
    }

    if aborted {
        std::process::exit(2);
    }
}

/// Reads an opt-in dump-destination env var once at startup. Absence is a
/// valid "not capturing" state; any other error (e.g. invalid Unicode) is a
/// misconfiguration and must not silently disable capture.
fn read_dump_env(key: &str) -> Option<String> {
    std::env::var(key)
        .map(Some)
        .or_else(|e| match e {
            std::env::VarError::NotPresent => Ok(None),
            e => Err(e),
        })
        .expect("invalid Unicode in dump-destination env var")
}

fn parse_difficulty(s: &str) -> AiDifficulty {
    match s {
        "VeryEasy" => AiDifficulty::VeryEasy,
        "Easy" => AiDifficulty::Easy,
        "Medium" => AiDifficulty::Medium,
        "Hard" => AiDifficulty::Hard,
        "VeryHard" => AiDifficulty::VeryHard,
        _ => AiDifficulty::Easy,
    }
}
