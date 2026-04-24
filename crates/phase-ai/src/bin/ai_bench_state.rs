//! Load a saved GameState JSON and time AI decisions.
//!
//! Usage: `cargo run --release --bin ai-bench-state -- <path> [--difficulty medium] [--iters N]`

use std::fs;
use std::time::Instant;

use engine::types::game_state::GameState;
use phase_ai::choose_action;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
use rand::rngs::StdRng;
use rand::SeedableRng;
use serde::Deserialize;

#[derive(Deserialize)]
struct Saved {
    #[serde(rename = "gameState")]
    game_state: GameState,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/tmp/gamestate.json".to_string());

    let mut difficulty = AiDifficulty::Medium;
    let mut iters = 3usize;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--difficulty" => {
                difficulty = match args[i + 1].to_lowercase().as_str() {
                    "veryeasy" => AiDifficulty::VeryEasy,
                    "easy" => AiDifficulty::Easy,
                    "medium" => AiDifficulty::Medium,
                    "hard" => AiDifficulty::Hard,
                    "veryhard" => AiDifficulty::VeryHard,
                    _ => AiDifficulty::Medium,
                };
                i += 2;
            }
            "--iters" => {
                iters = args[i + 1].parse().unwrap_or(3);
                i += 2;
            }
            _ => i += 1,
        }
    }

    let t_load_start = Instant::now();
    let raw = fs::read_to_string(&path).expect("read file");
    let t_read = t_load_start.elapsed();

    let t_parse_start = Instant::now();
    let saved: Saved = serde_json::from_str(&raw).expect("parse json");
    let t_parse = t_parse_start.elapsed();
    let state = saved.game_state;

    let t_clone_start = Instant::now();
    for _ in 0..100 {
        let _ = state.clone();
    }
    let t_clone = t_clone_start.elapsed() / 100;

    println!("=== Load timings ===");
    println!("file size:        {} MB", raw.len() / 1_000_000);
    println!("file read:        {:?}", t_read);
    println!("json parse:       {:?}", t_parse);
    println!("GameState clone:  {:?} (avg over 100)", t_clone);
    println!("turn_number:      {}", state.turn_number);
    println!("active_player:    {:?}", state.active_player);
    println!("players:          {}", state.players.len());
    println!("objects:          {}", state.objects.len());
    println!("battlefield:      {}", state.battlefield.len());
    println!("stack:            {}", state.stack.len());
    println!("waiting_for:      {:?}", std::mem::discriminant(&state.waiting_for));
    println!();

    let ai_player = state.active_player;
    let config = create_config_for_players(difficulty, Platform::Native, state.players.len() as u8);

    println!("=== AI choose_action (difficulty={:?}, iters={}) ===", difficulty, iters);
    println!(
        "search.enabled={} max_depth={} max_nodes={} time_budget_ms={:?}",
        config.search.enabled,
        config.search.max_depth,
        config.search.max_nodes,
        config.search.time_budget_ms,
    );
    let mut rng = StdRng::seed_from_u64(42);
    let mut total = std::time::Duration::ZERO;
    for i in 0..iters {
        let t = Instant::now();
        let action = choose_action(&state, ai_player, &config, &mut rng);
        let dt = t.elapsed();
        total += dt;
        match action {
            Some(a) => println!("iter {}: {:?}  action={:?}", i, dt, std::mem::discriminant(&a)),
            None => println!("iter {}: {:?}  action=None", i, dt),
        }
    }
    println!("mean:             {:?}", total / iters as u32);
}
