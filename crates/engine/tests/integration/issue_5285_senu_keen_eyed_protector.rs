//! Regression for issue #5285: Senu, Keen-Eyed Protector exile attack trigger must
//! return Senu from exile onto the battlefield attacking when a legendary
//! creature you control attacks unblocked.
//!
//! https://github.com/phase-rs/phase/issues/5285

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const SENU_ORACLE: &str = "Flying, vigilance\n\
{T}, Exile Senu: You gain 2 life and scry 2.\n\
When a legendary creature you control attacks and isn't blocked, if this card is exiled, put it onto the battlefield attacking.";

/// CR 508.1 + CR 509.1: Declare an unblocked attack and drain
/// `YouAttackUnblocked` triggers (CR 509.2).
fn declare_unblocked_attack(runner: &mut engine::game::scenario::GameRunner, attacker: ObjectId) {
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare attackers");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner.declare_blockers(&[]).expect("declare no blockers");
    }
    runner.advance_until_stack_empty();
}

#[test]
fn issue_5285_senu_returns_from_exile_attacking_on_legendary_unblocked_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let senu = scenario
        .add_creature_from_oracle(P0, "Senu, Keen-Eyed Protector", 2, 1, SENU_ORACLE)
        .id();
    let legendary_attacker = scenario.add_creature(P0, "Legend Attacker", 3, 3).id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&legendary_attacker)
        .unwrap()
        .card_types
        .supertypes
        .push(Supertype::Legendary);
    runner
        .state_mut()
        .objects
        .get_mut(&legendary_attacker)
        .unwrap()
        .base_card_types
        .supertypes
        .push(Supertype::Legendary);

    runner.activate(senu, 0).resolve();
    assert_eq!(
        runner.state().objects[&senu].zone,
        Zone::Exile,
        "activated ability must exile Senu"
    );

    runner.advance_to_combat();
    declare_unblocked_attack(&mut runner, legendary_attacker);

    assert_eq!(
        runner.state().objects[&senu].zone,
        Zone::Battlefield,
        "Senu must return from exile when a legendary creature attacks unblocked"
    );
    let combat = runner
        .state()
        .combat
        .as_ref()
        .expect("combat must still be active");
    assert!(
        combat.attackers.iter().any(|a| a.object_id == senu),
        "Senu must enter the battlefield attacking (CR 508.4)"
    );
    assert!(
        !runner.state().objects[&senu].tapped,
        "enters attacking alone must not tap Senu"
    );
}

#[test]
fn issue_5285_senu_does_not_return_when_still_on_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let senu = scenario
        .add_creature_from_oracle(P0, "Senu, Keen-Eyed Protector", 2, 1, SENU_ORACLE)
        .id();
    let legendary_attacker = scenario.add_creature(P0, "Legend Attacker", 3, 3).id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&legendary_attacker)
        .unwrap()
        .card_types
        .supertypes
        .push(Supertype::Legendary);
    runner
        .state_mut()
        .objects
        .get_mut(&legendary_attacker)
        .unwrap()
        .base_card_types
        .supertypes
        .push(Supertype::Legendary);

    runner.advance_to_combat();
    declare_unblocked_attack(&mut runner, legendary_attacker);

    assert_eq!(
        runner.state().objects[&senu].zone,
        Zone::Battlefield,
        "trigger condition requires Senu to be exiled"
    );
    let combat = runner.state().combat.as_ref();
    assert!(
        combat.is_none_or(|c| !c.attackers.iter().any(|a| a.object_id == senu)),
        "Senu must not enter attacking from the battlefield"
    );
}
