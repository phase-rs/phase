//! PR-7 "One-Ring" downstream lethal: with N burden on P1's The One Ring, P1's
//! own beginning-of-upkeep trigger (.triggers[1]) loses P1 N life; N >= P1 life =>
//! CR 704.5a loss => P0 is the sole survivor and wins (CR 104.2a). Real mechanics
//! end to end: burden placed via the counter authority `add_counter_with_replacement`
//! (CR 122.1, NOT a raw field poke), phase progression via `auto_advance_to_main_phase`,
//! the printed trigger fires + resolves, and the SBA/elimination authority declares
//! the winner — NOT the loop detector.
//!
//! CR (grep-verified against docs/MagicCompRules.txt):
//!   CR 122.1  — a counter is a marker placed on an object (burden is a Generic counter).
//!   CR 500.6  — "at the beginning of" abilities trigger when the step begins.
//!   CR 503.1a — beginning-of-upkeep triggers go on the stack before priority.
//!   CR 608.2  — the LoseLife amount reads burden on the source at resolution.
//!   CR 704.5a — a player with 0 or less life loses the game.
//!   CR 104.2a — a player wins when all opponents have left the game.
//!   CR 603.6a — enters-the-battlefield trigger reindex on battlefield entry.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const P1_LIFE: i32 = 5; // arbitrary scenario config; the discriminator is N-vs-life.

/// Real-mechanics materialization: place `burden` burden counters on `ring` through
/// the single-authority counter primitive (CR 122.1 / CR 614.1), then let the caller
/// reach-guard the resulting count.
fn materialize_burden(runner: &mut GameRunner, ring: ObjectId, burden: u32) {
    let mut events = Vec::new();
    engine::game::effects::counters::add_counter_with_replacement(
        runner.state_mut(),
        P1,
        ring,
        CounterType::Generic("burden".to_string()),
        burden,
        &mut events,
    );
}

fn setup(db: &engine::database::CardDatabase, burden: u32) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.with_life(P0, 20);
    scenario.with_life(P1, P1_LIFE);
    // Real DB The One Ring on P1 (ETB "if you cast it" does NOT fire on direct install
    // => P1 gains NO protection; upkeep trigger .triggers[1] is reindexed, CR 603.6a).
    let ring = scenario.add_real_card(P1, "The One Ring", Zone::Battlefield, db);
    // Non-empty P1 library so a survived control does not deck out on the draw step.
    scenario.add_real_card(P1, "Island", Zone::Library, db);
    scenario.add_real_card(P1, "Island", Zone::Library, db);
    let mut runner = scenario.build();
    // Start at the top of P1's turn so auto-advance crosses INTO P1's upkeep.
    runner.state_mut().turn_number = 3;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    materialize_burden(&mut runner, ring, burden);
    // Reach-guard: the counter authority actually placed exactly `burden`.
    assert_eq!(
        runner.state().objects[&ring]
            .counters
            .get(&CounterType::Generic("burden".to_string()))
            .copied()
            .unwrap_or(0),
        burden,
        "reach-guard: add_counter_with_replacement must place exactly {burden} burden"
    );
    (runner, ring)
}

/// LETHAL: N = P1 life => P1 -> 0 (CR 704.5a) => GameOver winner P0 (CR 104.2a).
#[test]
fn one_ring_burden_upkeep_kills_owner_p0_wins() {
    let Some(db) = load_db() else {
        return;
    };
    let (mut runner, _ring) = setup(db, P1_LIFE as u32); // N >= life
    assert_eq!(runner.life(P1), P1_LIFE, "precondition: P1 at {P1_LIFE}");

    // Cross into P1's upkeep (CR 500.6/503.1a: the trigger fires + goes on the stack),
    // then resolve it (CR 608.2: LoseLife = burden = N).
    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    assert!(
        runner.life(P1) <= 0,
        "P1 must lose N burden = {P1_LIFE} life: {P1_LIFE} - {P1_LIFE} <= 0 (got {})",
        runner.life(P1)
    );
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "CR 704.5a loss of P1 => P0 sole survivor wins (CR 104.2a); got {:?}",
        runner.state().waiting_for
    );
}

/// CONTROL (non-vacuity flip): N < P1 life => P1 survives => NOT GameOver.
#[test]
fn one_ring_sublethal_burden_owner_survives_no_gameover() {
    let Some(db) = load_db() else {
        return;
    };
    let n = (P1_LIFE - 1) as u32; // N < life
    let (mut runner, _ring) = setup(db, n);

    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        P1_LIFE - n as i32,
        "sub-lethal: P1 loses only N={n} => life {}; got {}",
        P1_LIFE - n as i32,
        runner.life(P1)
    );
    assert!(runner.life(P1) > 0, "P1 must survive a sub-lethal drain");
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "sub-lethal drain must NOT end the game; got {:?}",
        runner.state().waiting_for
    );
}
