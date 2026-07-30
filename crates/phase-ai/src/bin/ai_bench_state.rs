//! Load a saved GameState JSON and time AI decisions.
//!
//! Usage: `cargo run --release --bin ai-bench-state -- <path> [--difficulty medium] [--iters N] [--assert-under-ms N]`

use std::fs;
use std::time::Instant;

use phase_ai::choose_action;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
use phase_ai::saved_state::load_saved_game_state;
use rand::rngs::StdRng;
use rand::SeedableRng;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/tmp/gamestate.json".to_string());

    let mut difficulty = AiDifficulty::Medium;
    let mut iters = 3usize;
    let mut assert_under_ms: Option<u128> = None;
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
            "--assert-under-ms" => {
                assert_under_ms = args[i + 1].parse().ok();
                i += 2;
            }
            _ => i += 1,
        }
    }

    let t_load_start = Instant::now();
    let raw = fs::read_to_string(&path).expect("read file");
    let t_read = t_load_start.elapsed();

    let t_parse_start = Instant::now();
    let state = load_saved_game_state(&raw).expect("parse json");
    let t_parse = t_parse_start.elapsed();

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
    println!(
        "acting_player:    {:?} (from waiting_for)",
        state.waiting_for.acting_player()
    );
    println!("players:          {}", state.players.len());
    println!("objects:          {}", state.objects.len());
    println!("battlefield:      {}", state.battlefield.len());
    println!("stack:            {}", state.stack.len());
    println!(
        "waiting_for:      {:?}",
        std::mem::discriminant(&state.waiting_for)
    );
    println!();

    let ai_player = state
        .waiting_for
        .acting_player()
        .unwrap_or(state.active_player);
    let config = create_config_for_players(difficulty, Platform::Native, state.players.len() as u8);

    println!(
        "=== AI choose_action (difficulty={:?}, iters={}) ===",
        difficulty, iters
    );
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
            Some(a) => println!(
                "iter {}: {:?}  action={:?}",
                i,
                dt,
                std::mem::discriminant(&a)
            ),
            None => println!("iter {}: {:?}  action=None", i, dt),
        }
    }
    let mean = total / iters as u32;
    println!("mean:             {:?}", mean);

    // V3 mechanical gate: exit non-zero if the mean choose_action latency exceeds
    // the asserted ceiling (budget + T_apply_max + margin). Skips cleanly when the
    // flag is absent, so the saved-state check stays reproducible without pinning
    // the 23 MB state into the repo.
    if let Some(ceiling_ms) = assert_under_ms {
        let mean_ms = mean.as_millis();
        if mean_ms > ceiling_ms {
            eprintln!("FAIL: mean {mean_ms} ms exceeds --assert-under-ms {ceiling_ms} ms");
            std::process::exit(1);
        }
        println!("PASS: mean {mean_ms} ms <= --assert-under-ms {ceiling_ms} ms");
    }
}
