//! Profile one AI priority decision as the opponent's board scales.
//!
//! The AI holds Grizzly Bears with {G}{G} floating in its precombat main phase,
//! facing N vanilla 1/1s; each N prints the fastest of the repeated
//! `choose_action` calls and the perf counters they accrued. N = 1 and 100 run
//! the scored route (below `LARGE_BOARD_FAST_PRIORITY_BATTLEFIELD_OBJECTS`, 128,
//! in `search.rs`): compare their `min` ratio with the same bench on the parent
//! commit, and a higher ratio flags repeated board-dependent work. N = 1000 and
//! 2000 run the large-board fast route: their `min` ratio reads about 2x for
//! work linear in the board and about 4x for quadratic.
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
use engine::game::zones::create_object;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaPipId, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use phase_ai::config::{create_config, AiDifficulty, Platform};
use phase_ai::search::choose_action;
use rand::rngs::SmallRng;
use rand::SeedableRng;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const LARGE_BOARD_OBJECTS: usize = 128;
const REPS: u32 = 20;

fn board(tokens: usize) -> GameState {
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

fn bench_n(n: usize) -> Duration {
    let state = board(n);
    let objects = state.battlefield.len();
    let route = if objects >= LARGE_BOARD_OBJECTS {
        "fast"
    } else {
        "scored"
    };
    let config = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(42);

    perf_counters::reset();
    let mut action = None;
    let mut fastest = Duration::MAX;
    for _ in 0..REPS {
        let mut rng = SmallRng::seed_from_u64(42);
        let rep_start = Instant::now();
        action = choose_action(&state, P0, &config, &mut rng);
        fastest = fastest.min(rep_start.elapsed());
    }
    let c = perf_counters::snapshot();

    println!(
        "N={n:5} battlefield={objects:5} route={route:6} min={fastest:>10.3?}  \
         clones={} static_scans={} layers(full={} inc={})  -> {action:?}",
        c.state_clone_for_legality, c.static_full_scans, c.layers_full_eval, c.layers_incremental,
    );
    fastest
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
    let one = bench_n(1);
    let hundred = bench_n(100);
    print_ratio(1, 100, one, hundred);

    let thousand = bench_n(1000);
    let two_thousand = bench_n(2000);
    print_ratio(1000, 2000, thousand, two_thousand);
}
