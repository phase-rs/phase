//! Profile one AI priority decision as the opponent's board scales.
//!
//! The AI holds Grizzly Bears with two green mana in its pool in its precombat
//! main phase, facing N vanilla 1/1s; each N prints the fastest of the repeated
//! `choose_action` calls and the perf counters they accrued. One untimed call
//! first asserts the cast and the route: ordinary scoring, which clones the
//! state for legality, at N = 1 and 100 (below
//! `LARGE_BOARD_FAST_PRIORITY_BATTLEFIELD_OBJECTS`, 128, in `search.rs`), and the
//! large-board fast route, which takes no legality clone, at N = 1000 and 2000.
//!
//! The empty-hand rows rerun N = 1000 and 2000 with the AI holding nothing and
//! controlling five Forests. Each times `score_candidates`, asserting the
//! no-development shortcut's lone pass
//! (`large_board_main_phase_has_no_development_sources`) with no static scan,
//! and `choose_action`, asserting it passes.
//!
//! Comparing: check out the PR's merge base (`git merge-base HEAD upstream/main`),
//! which has no bench, copy this file onto it, add this target's `[[bin]]` entry to
//! `crates/phase-ai/Cargo.toml`, and run it on both revisions on one machine. Per route, a `min` ratio above the
//! comparison's flags repeated board-dependent work, and one moving toward 4x
//! rather than 2x points at quadratic work. For the empty-hand rows compare both
//! `min`s directly; a battlefield walk nested inside the shortcut multiplies them.
//!
//! Build/run (measurement mode: node-bounded, no wall-clock cap, so the time
//! reflects the work rather than the search deadline):
//!   cargo run -p phase-ai --features scenario-benches --bin priority-scaling-bench

// pod-lab loop-3 Q5: native-binary throughput lever, gated in Cargo.toml so
// wasm32 builds of this crate's lib (pulled in by engine-wasm/draft-wasm)
// never see it.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::time::{Duration, Instant};

use engine::game::perf_counters;
use engine::game::scenario::GameScenario;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use phase_ai::config::{create_config, AiDifficulty, Platform};
use phase_ai::search::{choose_action, score_candidates};
use rand::rngs::SmallRng;
use rand::SeedableRng;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const LARGE_BOARD_OBJECTS: usize = 128;
const REPS: u32 = 20;

#[derive(Clone, Copy)]
enum Hand {
    Bears,
    Empty,
}

fn board(tokens: usize, hand: Hand) -> GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..tokens {
        scenario.add_creature(P1, "Creature", 1, 1);
    }
    match hand {
        Hand::Empty => {
            for _ in 0..5 {
                scenario.add_basic_land(P0, ManaColor::Green);
            }
        }
        Hand::Bears => {
            scenario
                .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
                .with_subtypes(vec!["Bear"])
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 1,
                });
            let green = ManaUnit::new(ManaType::Green, ObjectId(0), false, Vec::new());
            scenario.with_mana_pool(P0, vec![green; 2]);
        }
    }
    scenario.build().state().clone()
}

fn bench_n(n: usize, hand: Hand) -> Duration {
    let state = board(n, hand);
    let objects = state.battlefield.len();
    let config = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(42);
    let choose = || choose_action(&state, P0, &config, &mut SmallRng::seed_from_u64(42));
    let (fastest, outcome, route) = match hand {
        Hand::Bears => {
            // Only ordinary scoring clones the state to check candidate legality.
            perf_counters::reset();
            let action = choose();
            let route = if perf_counters::snapshot().state_clone_for_legality > 0 {
                "scored"
            } else {
                "fast"
            };
            assert!(
                matches!(action, Some(GameAction::CastSpell { .. })),
                "{action:?}"
            );
            let expected = if objects >= LARGE_BOARD_OBJECTS {
                "fast"
            } else {
                "scored"
            };
            assert_eq!(route, expected, "N={n} took the wrong priority route");
            let (fastest, action) = fastest_of(choose);
            (fastest, format!("{action:?}"), route)
        }
        Hand::Empty => {
            let (choose_min, action) = fastest_of(choose);
            assert_eq!(action, Some(GameAction::PassPriority));
            let config =
                create_config(AiDifficulty::VeryEasy, Platform::Native).into_measurement(42);
            let (fastest, scored) = fastest_of(|| score_candidates(&state, P0, &config));
            // Only the shortcut answers with a lone pass before any static scan.
            assert!(
                scored == vec![(GameAction::PassPriority, 1.0)]
                    && perf_counters::snapshot().static_full_scans == 0,
                "no-development shortcut not taken: {scored:?}"
            );
            let outcome = format!("{scored:?}  choose_action min={choose_min:.3?} -> {action:?}");
            (fastest, outcome, "no-dev")
        }
    };
    let c = perf_counters::snapshot();

    println!(
        "N={n:5} battlefield={objects:5} route={route:6} min={fastest:>10.3?}  \
         clones={} static_scans={} layers(full={} inc={})  -> {outcome}",
        c.state_clone_for_legality, c.static_full_scans, c.layers_full_eval, c.layers_incremental,
    );
    fastest
}

fn fastest_of<T>(mut call: impl FnMut() -> T) -> (Duration, T) {
    perf_counters::reset();
    let mut fastest = Duration::MAX;
    let mut last = None;
    for _ in 0..REPS {
        let start = Instant::now();
        last = Some(call());
        fastest = fastest.min(start.elapsed());
    }
    (fastest, last.expect("REPS is nonzero"))
}

fn print_ratio(from: usize, to: usize, from_min: Duration, to_min: Duration) {
    println!(
        "  min ratio N={to}/N={from}: {:.2}x",
        to_min.as_secs_f64() / from_min.as_secs_f64()
    );
}

fn main() {
    println!(
        "debug_assertions = {}  reps = {REPS}",
        cfg!(debug_assertions)
    );
    println!();
    let one = bench_n(1, Hand::Bears);
    let hundred = bench_n(100, Hand::Bears);
    print_ratio(1, 100, one, hundred);

    let thousand = bench_n(1000, Hand::Bears);
    let two_thousand = bench_n(2000, Hand::Bears);
    print_ratio(1000, 2000, thousand, two_thousand);

    let thousand = bench_n(1000, Hand::Empty);
    let two_thousand = bench_n(2000, Hand::Empty);
    print_ratio(1000, 2000, thousand, two_thousand);
}
