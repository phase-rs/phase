//! Root-cause probe for issue #4798 (AI-vs-AI game "won't advance" after combat).
//!
//! The reporter's freeze was a wedged WASM worker inside the un-timed
//! `resolveAll` batch loop, which repeatedly calls the AI callback. Each AI
//! priority decision runs `flat_priority_actions` -> `validated_candidate_actions`
//! (per-candidate `SimulationFilter` state clones) BEFORE the 1.5s search
//! deadline even arms. This bench scales a realistic 3-player deep board and
//! times that pre-search enumeration + one full `choose_action`, printing the
//! perf counters each accrues, so any superlinear (O(n^2)) curve in the
//! enumeration is visible directly (doubling N should ~2x a linear cost; ~4x
//! flags quadratic).
//!
//! Build/run with a debug build in an isolated target dir (keeps Tilt's own
//! target lock uncontended):
//!   CARGO_TARGET_DIR=/tmp/forge-dbg cargo run \
//!       -p phase-ai --bin combat_priority_bench

use std::time::{Duration, Instant};

use engine::ai_support;
use engine::game::perf_counters;
use engine::game::scenario::GameScenario;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
use phase_ai::search::choose_action;
use rand::rngs::StdRng;
use rand::SeedableRng;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

/// Build a 3-player board where every seat controls `n` mana-producing lands
/// plus a handful of creatures, the active player holds `spells` castable
/// instants, and we sit at an empty-stack priority window in `phase`.
fn bench(phase: Phase, n: usize, spells: usize) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    for &p in &[P0, P1, P2] {
        for i in 0..n {
            let color = match i % 5 {
                0 => ManaColor::White,
                1 => ManaColor::Blue,
                2 => ManaColor::Black,
                3 => ManaColor::Red,
                _ => ManaColor::Green,
            };
            scenario.add_basic_land(p, color);
        }
        // A few creatures per seat so combat/static/target scans have bodies.
        for _ in 0..6 {
            scenario.add_vanilla(p, 2, 2);
        }
    }
    // Castable instants in the active player's hand => real, non-mana candidates
    // so the low-value fast-pass cannot short-circuit.
    for _ in 0..spells {
        scenario.add_bolt_to_hand(P0);
    }

    let mut runner = scenario.build();
    {
        let s = runner.state_mut();
        s.phase = phase;
        s.active_player = P0;
        s.priority_player = P0;
        s.waiting_for = WaitingFor::Priority { player: P0 };
    }
    let state = runner.state();

    // --- Read path only: per-candidate SimulationFilter enumeration (un-timed). ---
    perf_counters::reset();
    let iters = 3u32;
    let mut raw_n = 0;
    let mut valid_n = 0;
    let mut valid_total = Duration::ZERO;
    for _ in 0..iters {
        let raw = ai_support::candidate_actions(state);
        raw_n = raw.len();
        let start = Instant::now();
        let valid = ai_support::validated_candidate_actions(state);
        valid_total += start.elapsed();
        valid_n = valid.len();
    }
    let valid_mean = valid_total / iters;
    let ec = perf_counters::snapshot();

    // --- Full AI decision (fast-path + search) as the worker actually calls it. ---
    let config =
        create_config_for_players(AiDifficulty::Medium, Platform::Wasm, 3).into_measurement(42);
    let mut rng = StdRng::seed_from_u64(42);
    perf_counters::reset();
    let start = Instant::now();
    let _ = choose_action(state, P0, &config, &mut rng);
    let choose_dt = start.elapsed();
    let cc = perf_counters::snapshot();

    println!(
        "{phase:>16?} objs={objs:5} bf={bf:4} raw={raw_n:4} valid={valid_n:4} \
         enum={valid_mean:>9.3?} [clones={ec_cl} sweeps={ec_sw} swept={ec_swept}]  \
         choose={choose_dt:>9.3?} [clones={cc_cl} sweeps={cc_sw} swept={cc_swept}]",
        objs = state.objects.len(),
        bf = state.battlefield.len(),
        // `ec` accumulates across the `iters`-iteration read-path loop, so
        // report per-decision averages to match `valid_mean` above. `cc` below
        // is a single `choose_action` call and is already per-decision.
        ec_cl = ec.state_clone_for_legality / iters as u64,
        ec_sw = ec.mana_display_sweeps / iters as u64,
        ec_swept = ec.mana_display_swept_objects / iters as u64,
        cc_cl = cc.state_clone_for_legality,
        cc_sw = cc.mana_display_sweeps,
        cc_swept = cc.mana_display_swept_objects,
    );
}

fn main() {
    println!("debug_assertions = {}", cfg!(debug_assertions));
    println!("(enum = validated_candidate_actions mean; sim = enum minus raw candidate gen)\n");
    for phase in [Phase::PostCombatMain, Phase::CombatDamage] {
        for n in [10usize, 20, 30, 40, 60, 80] {
            bench(phase, n, 4);
        }
        println!();
    }
}
