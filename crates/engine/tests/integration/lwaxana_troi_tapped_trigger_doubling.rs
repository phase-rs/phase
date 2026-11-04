//! Lwaxana Troi, Charming Guest — an object-scoped trigger doubler that lasts
//! for as long as its source remains tapped.
//!
//! Oracle text:
//! "You may choose not to untap Lwaxana Troi during your untap step.
//! {2}, {T}: Choose another target creature you control. For as long as
//! Lwaxana Troi remains tapped, if a triggered ability of that creature
//! triggers, that ability triggers an additional time."
//!
//! CR 603.2d: the chosen creature's triggered abilities trigger an additional
//! time. CR 611.2b: the effect ends as soon as Lwaxana is untapped.
//!
//! The doubled creature and the control creature carry a synthetic attack
//! trigger ("Whenever this creature attacks, you gain 1 life."), the nearest
//! printed analogue being Ajani's Pridemate-style life triggers; only the life
//! total is observed.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const LWAXANA_ORACLE: &str = "You may choose not to untap Lwaxana Troi during your untap step.\n{2}, {T}: Choose another target creature you control. For as long as Lwaxana Troi remains tapped, if a triggered ability of that creature triggers, that ability triggers an additional time.";
const ATTACK_LIFE_ORACLE: &str = "Whenever this creature attacks, you gain 1 life.";
const VITALIZE_ORACLE: &str = "Untap all creatures you control.";

struct Board {
    runner: GameRunner,
    lwaxana: ObjectId,
    chosen: ObjectId,
    other: ObjectId,
    vitalize: ObjectId,
}

fn board() -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lwaxana = scenario
        .add_creature_from_oracle(P0, "Lwaxana Troi, Charming Guest", 2, 2, LWAXANA_ORACLE)
        .id();
    let chosen = scenario
        .add_creature_from_oracle(P0, "Chosen Herald", 2, 2, ATTACK_LIFE_ORACLE)
        .id();
    let other = scenario
        .add_creature_from_oracle(P0, "Other Herald", 2, 2, ATTACK_LIFE_ORACLE)
        .id();
    let vitalize = scenario
        .add_spell_to_hand_from_oracle(P0, "Vitalize", true, VITALIZE_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
    scenario.with_library_top(P0, &["Plains"; 5]);
    scenario.with_library_top(P1, &["Plains"; 5]);
    Board {
        runner: scenario.build(),
        lwaxana,
        chosen,
        other,
        vitalize,
    }
}

fn life(runner: &GameRunner) -> i32 {
    runner.state().players[0].life
}

fn activate_on(board: &mut Board) {
    let (lwaxana, chosen) = (board.lwaxana, board.chosen);
    board
        .runner
        .activate(lwaxana, 0)
        .target_objects(&[chosen])
        .resolve();
    let state = board.runner.state();
    assert!(state.objects[&lwaxana].tapped, "Lwaxana taps to activate");
    assert!(state.stack.is_empty(), "the activation resolves");
}

/// Attack with both heralds and resolve every attack trigger; returns the life
/// gained.
fn attack_with_both(board: &mut Board) -> i32 {
    let before = life(&board.runner);
    board.runner.advance_to_combat();
    board
        .runner
        .declare_attackers(&[
            (board.chosen, AttackTarget::Player(P1)),
            (board.other, AttackTarget::Player(P1)),
        ])
        .expect("both heralds attack");
    for _ in 0..32 {
        match board.runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(board.runner.state_mut());
            }
            WaitingFor::Priority { .. } if !board.runner.state().stack.is_empty() => {
                board.runner.act(GameAction::PassPriority).expect("pass");
            }
            _ => break,
        }
    }
    assert!(board.runner.state().stack.is_empty());
    life(&board.runner) - before
}

#[test]
fn only_the_chosen_creatures_triggers_are_doubled_while_tapped() {
    let mut board = board();
    activate_on(&mut board);
    // CR 603.2d: the chosen herald's trigger fires twice (+2); the other
    // herald's trigger is not doubled (+1).
    assert_eq!(attack_with_both(&mut board), 3);
}

#[test]
fn untapping_lwaxana_ends_the_doubling() {
    let mut board = board();
    activate_on(&mut board);
    let vitalize = board.vitalize;
    board.runner.cast(vitalize).resolve();
    assert!(
        !board.runner.state().objects[&board.lwaxana].tapped,
        "Vitalize untaps Lwaxana"
    );
    // CR 611.2b: once Lwaxana is untapped the effect is over — each herald's
    // trigger fires once.
    assert_eq!(attack_with_both(&mut board), 2);
}

#[test]
fn without_the_activation_nothing_is_doubled() {
    let mut board = board();
    assert_eq!(attack_with_both(&mut board), 2);
}
