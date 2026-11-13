//! Profile one AI priority decision as the opponent's board scales.
//!
//! The AI holds Grizzly Bears with {G}{G} floating in its precombat main phase,
//! facing N vanilla 1/1s; each N prints the fastest of the repeated
//! `choose_action` calls and the perf counters they accrued. N = 1 and 100 run
//! the scored route (below `LARGE_BOARD_FAST_PRIORITY_BATTLEFIELD_OBJECTS`, 128,
//! in `search.rs`): compare their `min` ratio with the same bench on the parent
//! commit, and a higher ratio flags repeated board-dependent work. N = 1000 and
//! 2000 run the large-board fast route: compare their `min` ratio with the
//! parent commit's the same way; a ratio moving toward 4x rather than 2x points
//! at quadratic work.
//!
//! The empty-hand rows rerun N = 1000 and 2000 with the AI holding nothing and
//! controlling five Forests. Each row times `score_candidates`, asserting it
//! answers with the no-development shortcut's lone pass
//! (`large_board_main_phase_has_no_development_sources`) and no static scan, so
//! the shortcut is the whole measured cost; it also times `choose_action` on the
//! same board and asserts it passes. Compare both `min`s with the parent
//! commit's; a battlefield walk nested inside the shortcut multiplies them.
//!
//! Build/run (measurement mode: node-bounded, no wall-clock cap, so the time
//! reflects the work rather than the search deadline):
//!   cargo run -p phase-ai --features scenario-benches --bin priority-scaling-bench

// pod-lab loop-3 Q5: native-binary throughput lever, gated in Cargo.toml so
// wasm32 builds of this crate's lib (pulled in by engine-wasm/draft-wasm)
// never see it.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::game::perf_counters;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaPipId, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
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
    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    for _ in 0..tokens {
        let card_id = CardId(state.next_object_id);
        let id = create_object(
            &mut state,
            card_id,
            P1,
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.base_card_types = obj.card_types.clone();
        obj.base_power = obj.power;
        obj.base_toughness = obj.toughness;
        obj.entered_battlefield_turn = Some(1);
    }

    if let Hand::Empty = hand {
        for _ in 0..5 {
            let card_id = CardId(state.next_object_id);
            let id = create_object(
                &mut state,
                card_id,
                P0,
                "Forest".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.supertypes.push(Supertype::Basic);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.base_card_types = obj.card_types.clone();
            obj.entered_battlefield_turn = Some(1);
            obj.summoning_sick = false;
            let tap_for_green = AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap);
            Arc::make_mut(&mut obj.abilities).push(tap_for_green.clone());
            Arc::make_mut(&mut obj.base_abilities).push(tap_for_green);
        }
        return state;
    }

    let card_id = CardId(state.next_object_id);
    let bears = create_object(
        &mut state,
        card_id,
        P0,
        "Grizzly Bears".to_string(),
        Zone::Hand,
    );
    let obj = state.objects.get_mut(&bears).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.card_types.subtypes.push("Bear".to_string());
    obj.power = Some(2);
    obj.toughness = Some(2);
    obj.base_card_types = obj.card_types.clone();
    obj.base_power = obj.power;
    obj.base_toughness = obj.toughness;
    obj.mana_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 1,
    };

    let pool = &mut state.players[P0.0 as usize].mana_pool;
    for _ in 0..2 {
        pool.add(ManaUnit {
            color: ManaType::Green,
            source_id: ObjectId(0),
            pip_id: ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: Vec::new(),
            expiry: None,
        });
    }
    state
}

fn bench_n(n: usize, hand: Hand) -> Duration {
    let state = board(n, hand);
    let objects = state.battlefield.len();
    let config = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(42);
    let choose = || choose_action(&state, P0, &config, &mut SmallRng::seed_from_u64(42));
    let (fastest, outcome, route) = match hand {
        Hand::Bears => {
            let (fastest, action) = fastest_of(choose);
            let route = if objects >= LARGE_BOARD_OBJECTS {
                "fast"
            } else {
                "scored"
            };
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
