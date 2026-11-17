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
use engine::types::zones::Zone;

/// Tunnel Tipster's printed trigger (CR 603.4 + CR 708.2).
const TUNNEL_TIPSTER: &str = "At the beginning of your end step, if a face-down creature entered the battlefield under your control this turn, put a +1/+1 counter on this creature.\n{T}: Add {G}.";

/// Sagu Archer — a printed morph creature, so the real face-down cast path
/// (CR 708.2 + CR 708.4) produces the entry snapshot this test reads.
/// Printed: {4}{G} 2/5 "Reach" + "Morph {4}{G}".
const SAGU_ARCHER: &str = "Reach (This creature can block creatures with flying.)\nMorph {4}{G} (You may cast this card face down as a 2/2 creature for {3}. Turn it face up any time for its morph cost.)";

/// Cloudshift — printed Oracle text. Used to force a SECOND, face-up entry
/// record for the same `ObjectId` (CR 400.7), so the test can prove the earlier
/// face-down snapshot survives a later refresh.
const CLOUDSHIFT: &str =
    "Exile target creature you control, then return that card to the battlefield under your control.";

fn mana_pool(color: ManaType, count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

/// Tunnel Tipster on the battlefield plus a Sagu Archer in hand. Six green mana
/// cover the face-down {3} and the printed {4}{G}; three library cards fuel the
/// draw step of the turn-reset control.
fn scenario_board() -> (GameScenario, ObjectId, ObjectId) {
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
    scenario.with_mana_pool(P0, mana_pool(ManaType::Green, 6));
    scenario.with_library_top(P0, &["Filler A", "Filler B", "Filler C"]);
    scenario.with_library_top(P1, &["Filler D", "Filler E", "Filler F"]);
    (scenario, tipster, archer)
}

fn board() -> (GameRunner, ObjectId, ObjectId) {
    let (scenario, tipster, archer) = scenario_board();
    (scenario.build(), tipster, archer)
}

/// The same board plus Cloudshift in hand and the {W} to cast it (CR 601.2f).
/// The blink appends a second entry record for the archer's `ObjectId`.
fn blink_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let (mut scenario, tipster, archer) = scenario_board();
    let cloudshift = scenario
        .add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(P0, mana_pool(ManaType::White, 1));
    (scenario.build(), tipster, archer, cloudshift)
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

/// CR 608.2i: every entry keeps its own entry-time snapshot. Casting Sagu
/// Archer face down and then blinking it with Cloudshift appends a second
/// (face-up) record for the same `ObjectId`; refreshing that new record must
/// not rewrite the earlier face-down one, which is what the intervening-if
/// looks back at.
#[test]
fn later_face_up_entry_preserves_the_face_down_snapshot() {
    let (mut runner, tipster, archer, cloudshift) = blink_board();

    runner
        .cast(archer)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .resolve();
    let first_incarnation = runner.state().objects[&archer].incarnation;

    runner.cast(cloudshift).target_object(archer).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&archer].zone,
        Zone::Battlefield,
        "reach-guard: Cloudshift must return the archer"
    );
    assert_ne!(
        runner.state().objects[&archer].incarnation,
        first_incarnation,
        "CR 400.7: the returned permanent is a new object"
    );
    assert!(
        !runner.state().objects[&archer].face_down,
        "reach-guard: Cloudshift returns the card face up"
    );

    let snapshots: Vec<FaceState> = runner
        .state()
        .battlefield_entries_this_turn
        .iter()
        .filter(|entry| entry.object_id == archer)
        .map(|entry| entry.face_state)
        .collect();
    assert_eq!(
        snapshots,
        vec![FaceState::FaceDown, FaceState::FaceUp],
        "each entry must keep its own entry-time face state"
    );

    advance_to_end_step(&mut runner);

    assert_eq!(
        plus_one_counters(&runner, tipster),
        1,
        "the earlier face-down entry must still satisfy the condition"
    );
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
