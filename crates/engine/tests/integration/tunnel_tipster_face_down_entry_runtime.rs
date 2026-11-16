//! Regression for issue #9622 — **Tunnel Tipster**.
//!
//! Oracle: "At the beginning of your end step, if a face-down creature entered
//! the battlefield under your control this turn, put a +1/+1 counter on this
//! creature."
//!
//! The intervening-if reads the CR 608.2i battlefield-entry snapshot, whose
//! `face_state` is captured at entry time. This test enters a printed morph
//! creature through the public face-down cast path, then advances through the
//! end step; the trigger is checked when the event occurs and again as it
//! resolves (CR 603.4).
//!
//! REVERT-PROBE: remove `FilterProp::FaceDown` from
//! `ledger_filter_is_evaluable` or delete the matcher's `FaceDown` arm — the
//! face-down row below loses its counter while the controls keep passing.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::AlternativeCastDecision;
use engine::types::counter::CounterType;
use engine::types::game_state::{FaceState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

/// Tunnel Tipster's printed trigger (CR 603.4 + CR 708.2).
const TUNNEL_TIPSTER: &str = "At the beginning of your end step, if a face-down creature entered the battlefield under your control this turn, put a +1/+1 counter on this creature.\n{T}: Add {G}.";

/// Sagu Archer — a printed morph creature, so the real face-down cast path
/// (CR 708.2 + CR 708.4) produces the entry snapshot this test reads.
/// Printed: {4}{G} 2/5 "Morph {4}{G}"
const SAGU_ARCHER: &str = "Morph {4}{G} (You may cast this card face down as a 2/2 creature for {3}. Turn it face up any time for its morph cost.)";

fn green_pool(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
        .collect()
}

/// Tunnel Tipster plus a Sagu Archer in hand. Six green mana covers both the
/// face-down {3} and the printed {4}{G}; three library cards fuel the draw
/// step of the turn-reset control.
fn board() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tipster = scenario
        .add_creature(P0, "Tunnel Tipster", 1, 1)
        .from_oracle_text(TUNNEL_TIPSTER)
        .id();
    let archer = scenario
        .add_creature_to_hand_from_oracle(P0, "Sagu Archer", 2, 5, SAGU_ARCHER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 4,
        })
        .id();
    scenario.with_mana_pool(P0, green_pool(6));
    scenario.with_library_top(P0, &["Filler A", "Filler B", "Filler C"]);
    scenario.with_library_top(P1, &["Filler D", "Filler E", "Filler F"]);
    (scenario.build(), tipster, archer)
}

fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn entry_face_state(runner: &GameRunner, id: ObjectId) -> FaceState {
    runner
        .state()
        .battlefield_entries_this_turn
        .iter()
        .rev()
        .find(|entry| entry.object_id == id)
        .unwrap_or_else(|| panic!("no battlefield-entry record for {id:?}"))
        .face_state
}

/// Cross combat with an empty attack declaration, then reach the end step.
fn advance_to_end_step(runner: &mut GameRunner) {
    runner.advance_to_combat();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner
            .declare_attackers(&[])
            .expect("declare no attackers to cross combat");
    }
    runner.advance_to_end_step();
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "must reach the end step, got {:?} waiting on {:?}",
        runner.state().phase,
        runner.state().waiting_for
    );
    runner.advance_until_stack_empty();
}

/// The defect: a face-down creature entered this turn must satisfy the
/// intervening-if and grow Tunnel Tipster at the end step.
#[test]
fn face_down_entry_fires_tunnel_tipster() {
    let (mut runner, tipster, archer) = board();

    runner
        .cast(archer)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .resolve();
    assert!(
        runner.state().objects[&archer].face_down,
        "reach-guard: the morph must enter face down"
    );
    assert_eq!(
        entry_face_state(&runner, archer),
        FaceState::FaceDown,
        "the entry snapshot must capture the face-down status"
    );

    advance_to_end_step(&mut runner);

    assert_eq!(
        plus_one_counters(&runner, tipster),
        1,
        "Tunnel Tipster must get a +1/+1 counter from the face-down entry"
    );
    assert_eq!(runner.state().objects[&tipster].power, Some(2));
    assert_eq!(runner.state().objects[&tipster].toughness, Some(2));
}

/// Control: an ordinary face-up entry must not satisfy the condition.
#[test]
fn face_up_entry_does_not_fire_tunnel_tipster() {
    let (mut runner, tipster, archer) = board();

    runner
        .cast(archer)
        .alternative_cast(AlternativeCastDecision::Normal)
        .resolve();
    assert!(
        !runner.state().objects[&archer].face_down,
        "reach-guard: the archer must enter face up"
    );
    assert_eq!(
        entry_face_state(&runner, archer),
        FaceState::FaceUp,
        "a face-up entry must snapshot as FaceUp"
    );

    advance_to_end_step(&mut runner);

    assert_eq!(
        plus_one_counters(&runner, tipster),
        0,
        "a face-up entry must not satisfy the face-down condition"
    );
}

/// Cross into the next turn without re-entering the End step's beginning-of-step
/// event (`advance_to_phase` stops at the phase boundary and would replay it).
fn pass_to_next_turn_precombat(runner: &mut GameRunner, from_turn: u32) {
    for _ in 0..24 {
        if runner.state().turn_number > from_turn && runner.state().phase == Phase::PreCombatMain {
            return;
        }
        runner.pass_both_players();
    }
    panic!(
        "could not cross into the next turn's precombat main (turn {}, phase {:?})",
        runner.state().turn_number,
        runner.state().phase
    );
}

/// Control: the entry ledger resets each turn, so last turn's face-down entry
/// must not fire the trigger again (the first end step already paid out once).
#[test]
fn face_down_entry_from_a_previous_turn_does_not_fire() {
    let (mut runner, tipster, archer) = board();

    runner
        .cast(archer)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .resolve();
    advance_to_end_step(&mut runner);
    assert_eq!(
        plus_one_counters(&runner, tipster),
        1,
        "reach-guard: this turn's face-down entry must fire"
    );

    let first_turn = runner.state().turn_number;
    pass_to_next_turn_precombat(&mut runner, first_turn);
    assert!(
        runner.state().battlefield_entries_this_turn.is_empty(),
        "reach-guard: the entry ledger must reset at the turn boundary"
    );

    advance_to_end_step(&mut runner);
    assert_eq!(
        plus_one_counters(&runner, tipster),
        1,
        "a face-down entry from a previous turn must not fire the trigger again"
    );
}
