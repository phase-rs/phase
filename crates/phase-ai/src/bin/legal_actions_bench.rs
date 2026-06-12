//! Time legal-action generation on a saved GameState.
//!
//! Loads a client checkpoint (`{ "gameState": ... }`) or raw `GameState`, then
//! separately times raw candidate generation, simulation validation, full
//! frontend legal-action packaging, and one AI `choose_action` call.
//!
//! Build/run with an isolated target dir:
//!   CARGO_TARGET_DIR=/tmp/forge-prof-target cargo run --profile profiling \
//!       -p phase-ai --bin legal-actions-bench -- path/to/state.json

use std::fs;
use std::time::{Duration, Instant};

use engine::ai_support;
use engine::game::perf_counters;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
use phase_ai::saved_state::load_saved_game_state;
use phase_ai::search::choose_action;
use rand::rngs::StdRng;
use rand::SeedableRng;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/tmp/gs16.json".to_string());
    let iters = args
        .windows(2)
        .find_map(|window| {
            (window[0] == "--iters")
                .then(|| window[1].parse::<u32>().ok())
                .flatten()
        })
        .unwrap_or(5);

    let raw = fs::read_to_string(&path).expect("read state file");
    let state = load_saved_game_state(&raw).expect("parse saved state");
    let actor = state
        .waiting_for
        .acting_player()
        .unwrap_or(state.active_player);

    println!("debug_assertions = {}", cfg!(debug_assertions));
    println!("path = {path}");
    println!("iters = {iters}");
    println!("objects = {}", state.objects.len());
    println!("battlefield = {}", state.battlefield.len());
    println!("stack = {}", state.stack.len());
    println!("players = {}", state.players.len());
    println!("waiting_for = {}", state.waiting_for.variant_name());
    println!("actor = {:?}", actor);
    println!();

    perf_counters::reset();

    let mut raw_total = Duration::ZERO;
    let mut valid_total = Duration::ZERO;
    let mut full_total = Duration::ZERO;
    let mut raw_count = 0usize;
    let mut valid_count = 0usize;
    let mut full_count = 0usize;
    let mut spell_cost_count = 0usize;
    let mut grouped_count = 0usize;

    for _ in 0..iters {
        let start = Instant::now();
        let raw_candidates = ai_support::candidate_actions(&state);
        raw_total += start.elapsed();
        raw_count = raw_candidates.len();

        let start = Instant::now();
        let valid_candidates = ai_support::validated_candidate_actions(&state);
        valid_total += start.elapsed();
        valid_count = valid_candidates.len();

        let start = Instant::now();
        let (actions, spell_costs, grouped) = ai_support::legal_actions_full(&state);
        full_total += start.elapsed();
        full_count = actions.len();
        spell_cost_count = spell_costs.len();
        grouped_count = grouped.len();
    }

    let raw_mean = raw_total / iters;
    let valid_mean = valid_total / iters;
    let full_mean = full_total / iters;
    let sim_filter_mean = valid_mean.saturating_sub(raw_mean);
    let display_map_mean = full_mean.saturating_sub(valid_mean);
    let per_candidate = if raw_count == 0 {
        Duration::ZERO
    } else {
        sim_filter_mean / raw_count as u32
    };

    println!("=== legal actions ===");
    println!("raw candidates:      {raw_count}");
    println!("valid candidates:    {valid_count}");
    println!("flat legal actions:  {full_count}");
    println!("spell cost entries:  {spell_cost_count}");
    println!("grouped objects:     {grouped_count}");
    println!("raw mean:            {raw_mean:?}");
    println!("validated mean:      {valid_mean:?}");
    println!("full mean:           {full_mean:?}");
    println!("simulation filter:   {sim_filter_mean:?}");
    println!("display map:         {display_map_mean:?}");
    println!("sim per raw cand:    {per_candidate:?}");
    println!();

    let config = create_config_for_players(
        AiDifficulty::Medium,
        Platform::Native,
        state.players.len() as u8,
    )
    .into_measurement(42);
    let mut rng = StdRng::seed_from_u64(42);
    let start = Instant::now();
    let action = choose_action(&state, actor, &config, &mut rng);
    let choose_dt = start.elapsed();
    println!("=== choose_action ===");
    println!("difficulty:          {:?}", config.difficulty);
    println!("search enabled:      {}", config.search.enabled);
    println!("max_depth:           {}", config.search.max_depth);
    println!("max_nodes:           {}", config.search.max_nodes);
    println!("duration:            {choose_dt:?}");
    println!(
        "action:              {:?}",
        action.as_ref().map(std::mem::discriminant)
    );
    println!();

    let counters = perf_counters::snapshot();
    println!("=== perf counters ===");
    println!(
        "state_clone_for_legality: {}",
        counters.state_clone_for_legality
    );
    println!("layers_full_eval:         {}", counters.layers_full_eval);
    println!("layers_incremental:       {}", counters.layers_incremental);
    println!("layers_escalated:         {}", counters.layers_escalated);
    println!("mana_display_sweeps:      {}", counters.mana_display_sweeps);
    println!(
        "mana_display_swept_objs:  {}",
        counters.mana_display_swept_objects
    );
}
