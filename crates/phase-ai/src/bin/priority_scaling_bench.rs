//! Profile one AI priority decision as the opponent's board scales.
//!
//! The board is a precombat main phase where the AI holds Grizzly Bears with
//! {G}{G} floating, facing N vanilla 1/1s. Per N this prints the battlefield
//! size, the route the decision takes, the fastest and mean time per
//! `choose_action`, and the engine perf counters accrued over all repetitions.
//!
//! Routes: below 128 battlefield objects (`LARGE_BOARD_FAST_PRIORITY_BATTLEFIELD_OBJECTS`
//! in `search.rs`) the decision runs scored search; at or above it the
//! large-board fast route answers. N = 1 and 100 sit on the scored route, 128
//! and 1000 on the fast route.
//!
//! Reading it: within a route the perf counters match across N, but time per
//! decision still grows with N because evaluation and layer passes walk the
//! battlefield. Compare the `min` ratio from N = 1 to 100 (and 128 to 1000)
//! against a baseline run of the parent commit on the same machine: a ratio well
//! above baseline flags repeated board-dependent work (a battlefield walk nested
//! inside evaluation, candidate enumeration, or policy scoring). Read `min`, not
//! `mean`; the mean absorbs load from other processes.
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

fn bench_n(n: usize) {
    let state = board(n);
    let objects = state.battlefield.len();
    let route = if objects >= LARGE_BOARD_OBJECTS {
        "fast"
    } else {
        "scored"
    };
    let config = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(42);

    perf_counters::reset();
    let start = Instant::now();
    let mut action = None;
    let mut fastest = Duration::MAX;
    for _ in 0..REPS {
        let mut rng = SmallRng::seed_from_u64(42);
        let rep_start = Instant::now();
        action = choose_action(&state, P0, &config, &mut rng);
        fastest = fastest.min(rep_start.elapsed());
    }
    let mean = start.elapsed() / REPS;
    let c = perf_counters::snapshot();

    println!(
        "N={n:5} battlefield={objects:5} route={route:6} min={fastest:>10.3?} mean={mean:>10.3?}  \
         clones={} static_scans={} layers(full={} inc={}) mana_sweeps={} swept={}  -> {action:?}",
        c.state_clone_for_legality,
        c.static_full_scans,
        c.layers_full_eval,
        c.layers_incremental,
        c.mana_display_sweeps,
        c.mana_display_swept_objects,
    );
}

fn main() {
    println!(
        "debug_assertions = {}  reps = {REPS}",
        cfg!(debug_assertions)
    );
    println!();
    for n in [1usize, 100, 128, 1000] {
        bench_n(n);
    }
}
