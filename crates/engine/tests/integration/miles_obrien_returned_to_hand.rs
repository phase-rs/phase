//! Miles O'Brien, Transporter Ops — "At the beginning of each end step, if a
//! permanent was returned to your hand from the battlefield this turn, draw a
//! card."
//!
//! The intervening-if (CR 603.4) reads this turn's battlefield-to-hand zone
//! changes of permanents its controller OWNS — a card only ever goes to its
//! owner's hand (CR 400.3) — regardless of which player or effect returned it.
//! Drives the verbatim Oracle text through the real cast / trigger pipeline.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Miles O'Brien, Transporter Ops, verbatim (Scryfall Oracle text).
const MILES: &str = "When Miles O'Brien enters, return up to one other target creature or \
artifact you control to its owner's hand.\n\
At the beginning of each end step, if a permanent was returned to your hand from the \
battlefield this turn, draw a card.";

/// Unsummon, verbatim — an independent "return to its owner's hand" source.
const UNSUMMON: &str = "Return target creature to its owner's hand.";

const MILES_NAME: &str = "Miles O'Brien, Transporter Ops";

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(player, &["Island"; 10]);
    }
    scenario
}

fn library_len(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].library.len()
}

/// Advance to the current turn's end step and let its triggers resolve.
/// Returns how many cards P0 drew there.
fn end_step_draws(runner: &mut GameRunner) -> usize {
    let before = library_len(runner);
    runner.advance_to_end_step();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner.declare_attackers(&[]).expect("attack with nothing");
        runner.advance_to_end_step();
    }
    assert_eq!(runner.state().phase, Phase::End);
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "end step settled: {:?}",
        runner.state().waiting_for
    );
    before - library_len(runner)
}

fn cast_unsummon(runner: &mut GameRunner, unsummon: ObjectId, victim: ObjectId) {
    runner.cast(unsummon).target_objects(&[victim]).resolve();
    assert_eq!(runner.state().objects[&victim].zone, Zone::Hand, "bounced");
}

/// CR 603.4 + CR 400.7: Miles's own ETB returns another creature you control
/// to your hand, so his end-step trigger draws a card that turn.
#[test]
fn miles_etb_bounce_draws_at_that_end_step() {
    let mut scenario = scenario();
    let miles = scenario
        .add_creature_to_hand_from_oracle(P0, MILES_NAME, 2, 2, MILES)
        .id();
    let bear = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();
    let mut runner = scenario.build();

    runner.cast(miles).target_objects(&[bear]).resolve();
    assert_eq!(runner.state().objects[&miles].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&bear].zone, Zone::Hand, "ETB bounce");

    assert_eq!(end_step_draws(&mut runner), 1);
}

/// CR 603.4: with nothing returned this turn the intervening-if is false and
/// no card is drawn; Unsummon on your own creature (any source of the return)
/// then turns it on for the same kind of end step.
#[test]
fn miles_draws_only_when_a_permanent_you_own_returned_this_turn() {
    let mut scenario = scenario();
    scenario.add_creature_from_oracle(P0, MILES_NAME, 2, 2, MILES);
    let bear = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .id();
    let mut runner = scenario.build();

    // Nothing returned on this turn.
    assert_eq!(end_step_draws(&mut runner), 0, "no return, no draw");

    // Next turn (P1's): P0 bounces their own Bear at instant speed.
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    // P1 passes priority in their main phase; P0 responds with Unsummon.
    runner.act(GameAction::PassPriority).expect("P1 passes");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    cast_unsummon(&mut runner, unsummon, bear);
    assert_eq!(
        end_step_draws(&mut runner),
        1,
        "each end step: P0's permanent returned during P1's turn"
    );
}

/// CR 400.3 + CR 109.5: an opponent's creature returned to ITS owner's hand
/// is not "returned to your hand" — no draw.
#[test]
fn miles_ignores_an_opponents_permanent_returned_to_their_hand() {
    let mut scenario = scenario();
    scenario.add_creature_from_oracle(P0, MILES_NAME, 2, 2, MILES);
    let their_bear = scenario.add_creature(P1, "Grizzly Bear", 2, 2).id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .id();
    let mut runner = scenario.build();

    cast_unsummon(&mut runner, unsummon, their_bear);
    assert_eq!(runner.state().objects[&their_bear].owner, P1);
    assert_eq!(end_step_draws(&mut runner), 0);
}

/// CR 603.4: "this turn" — a return on an earlier turn does not satisfy a
/// later turn's end step.
#[test]
fn miles_ignores_a_permanent_returned_on_a_previous_turn() {
    let mut scenario = scenario();
    let miles = scenario
        .add_creature_to_hand_from_oracle(P0, MILES_NAME, 2, 2, MILES)
        .id();
    let bear = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();
    let mut runner = scenario.build();

    runner.cast(miles).target_objects(&[bear]).resolve();
    assert_eq!(runner.state().objects[&bear].zone, Zone::Hand);
    assert_eq!(end_step_draws(&mut runner), 1, "same turn: draws");

    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1, "the next turn");
    assert_eq!(end_step_draws(&mut runner), 0, "previous turn's return");
}
