//! Time `apply(PassPriority)` on a saved GameState.
//!
//! The legal-actions bench measures the *read* path (enumerating candidates).
//! This measures the *write* path a human hits when they click "pass": one
//! `engine::game::apply(state, actor, PassPriority)` call, including the
//! post-action finalizers (auto-pass loop, rules-state, display-state mana
//! sweep). Reports per-call timing and the perf counters each call accrued.
//!
//! Build/run with a debug build in an isolated target dir (keeps Tilt's own
//! target lock uncontended):
//!   CARGO_TARGET_DIR=/tmp/forge-dbg cargo run \
//!       -p phase-ai --bin pass_priority_bench -- path/to/state.json
//!
//! The perf counters this prints are profile-independent — only the absolute
//! per-call times inflate in a debug build. The `profiling` profile compiles
//! the engine very slowly and contends with Tilt, so prefer the debug build
//! above unless you specifically need realistic wall-clock numbers.

use std::fs;
use std::time::Instant;

use engine::game::engine::apply;
use engine::game::perf_counters;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;
use phase_ai::saved_state::load_saved_game_state;

fn actor_for(state: &GameState) -> PlayerId {
    state
        .waiting_for
        .acting_player()
        .unwrap_or(state.active_player)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/tmp/gs39.json".to_string());
    let steps = args
        .windows(2)
        .find_map(|w| {
            (w[0] == "--steps")
                .then(|| w[1].parse::<u32>().ok())
                .flatten()
        })
        .unwrap_or(8);

    let raw = fs::read_to_string(&path).expect("read state file");
    let base = load_saved_game_state(&raw).expect("parse saved state");

    println!("debug_assertions = {}", cfg!(debug_assertions));
    println!("path = {path}");
    println!("objects = {}", base.objects.len());
    println!("battlefield = {}", base.battlefield.len());
    println!("players = {}", base.players.len());
    println!("phase = {:?}", base.phase);
    println!("waiting_for = {}", base.waiting_for.variant_name());
    println!();

    // Walk forward applying PassPriority repeatedly, fresh clone each step so we
    // measure a single apply in isolation and can attribute counters to it.
    println!("=== per-step apply(PassPriority) ===");
    let mut state = base.clone();
    for step in 0..steps {
        let actor = actor_for(&state);
        let before_variant = state.waiting_for.variant_name();
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            println!("step {step}: game over, stopping");
            break;
        }

        perf_counters::reset();
        let start = Instant::now();
        let result = apply(&mut state, actor, GameAction::PassPriority);
        let dt = start.elapsed();
        let c = perf_counters::snapshot();

        match result {
            Ok(_) => {
                println!(
                    "step {step:2}: actor={actor:?} {before_variant} -> {:<18} {dt:>10.3?}  \
                     layers(full={} inc={} esc={}) mana_sweeps={} swept_objs={}",
                    state.waiting_for.variant_name(),
                    c.layers_full_eval,
                    c.layers_incremental,
                    c.layers_escalated,
                    c.mana_display_sweeps,
                    c.mana_display_swept_objects,
                    dt = dt,
                );
            }
            Err(e) => {
                println!("step {step:2}: actor={actor:?} {before_variant} ERR {e:?} ({dt:?})");
                break;
            }
        }
    }
}
