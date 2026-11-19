//! Septic Rats (issue #10950) — "Whenever this creature attacks, if defending
//! player is poisoned, it gets +1/+1 until end of turn."
//!
//! CR 122.1f DEFINES "poisoned" as having one or more poison counters, and
//! CR 508.5 makes "defending player" on an attack trigger the player the
//! attacking creature is attacking. Before the fix the guard stayed in the
//! effect text, so the trigger's +1/+1 was never granted.
//!
//! These tests drive the real combat pipeline: declare attackers → attack
//! trigger on the stack → resolution → layer system, asserting the post-layer
//! P/T of the attacker. The non-matching row keeps a DIFFERENT opponent
//! poisoned while the defending player is clean, so a controller-scoped or
//! any-opponent-scoped misread flips the assertion.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);

/// Verbatim Scryfall Oracle text, reminder text included.
const SEPTIC_RATS: &str = "Infect (This creature deals damage to creatures in the form of -1/-1 counters and to players in the form of poison counters.)\nWhenever this creature attacks, if defending player is poisoned, it gets +1/+1 until end of turn.";

/// Three-player game with Septic Rats on P0's battlefield and the given poison
/// counters, so "defending player" is distinguishable from "an opponent".
fn setup(poison: [u32; 3]) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, P2] {
        for _ in 0..10 {
            scenario.add_card_to_library_top(player, "Plains");
        }
    }
    let rats = {
        let mut builder = scenario.add_creature(P0, "Septic Rats", 2, 2);
        builder.from_oracle_text_with_keywords(&["Infect"], SEPTIC_RATS);
        builder.id()
    };
    let mut runner = scenario.build();
    for (seat, counters) in poison.iter().enumerate() {
        runner.state_mut().players[seat].poison_counters = *counters;
    }
    (runner, rats)
}

/// Set the active player and pass priority until the declare-attackers step.
fn hand_turn_to(runner: &mut GameRunner, attacker: PlayerId) {
    runner.state_mut().active_player = attacker;
    runner.state_mut().priority_player = attacker;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: attacker };

    for _ in 0..16 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass should advance toward declare attackers");
    }
    panic!("expected DeclareAttackers");
}

/// Declare `attacker` against `defender` through the public action, returning
/// whether the attack trigger reached the stack (a reach guard for the
/// absence assertion in the non-matching row).
fn declare_attack(runner: &mut GameRunner, attacker: ObjectId, defender: PlayerId) -> bool {
    hand_turn_to(runner, P0);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(defender))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    runner.state().stack.iter().any(|entry| {
        matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == attacker
        )
    })
}

fn power_toughness(runner: &GameRunner, object: ObjectId) -> (i32, i32) {
    let obj = &runner.state().objects[&object];
    (obj.power.unwrap_or(0), obj.toughness.unwrap_or(0))
}

/// CR 122.1f + CR 508.5: the defending player is poisoned, so the attacker
/// becomes a 3/3 until end of turn.
#[test]
fn septic_rats_pumps_when_the_defending_player_is_poisoned() {
    let (mut runner, rats) = setup([0, 1, 0]);
    assert!(
        declare_attack(&mut runner, rats, P1),
        "reach guard: the attack trigger must reach the stack"
    );
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "the trigger must have resolved before the P/T assertion"
    );
    assert_eq!(
        power_toughness(&runner, rats),
        (3, 3),
        "a poisoned defending player must grant +1/+1"
    );
}

/// Non-matching control: a different opponent is poisoned while the defending
/// player is not. A controller-scoped or opponents-scoped misread would pump
/// the attacker, so 2/2 is the discriminating result.
#[test]
fn septic_rats_does_not_pump_when_only_another_opponent_is_poisoned() {
    let (mut reach_runner, reach_rats) = setup([0, 1, 0]);
    assert!(
        declare_attack(&mut reach_runner, reach_rats, P1),
        "reach guard: the matching fixture's attack trigger must reach the stack"
    );
    let (mut runner, rats) = setup([0, 0, 1]);
    assert!(
        !declare_attack(&mut runner, rats, P1),
        "the intervening-if is false, so no attack trigger may be put on the stack"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        power_toughness(&runner, rats),
        (2, 2),
        "another opponent's poison must not satisfy 'defending player is poisoned'"
    );
}
