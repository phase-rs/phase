//! The card **Regenerate** ("Regenerate target creature.") is named after its
//! own keyword-action verb (CR 701.19a). Card-name normalization must not
//! collapse the leading "Regenerate" into the self-reference `~`, or the effect
//! parses as a verbless "~ target creature" that resolves to nothing.
//!
//! This drives the full cast -> target -> resolve pipeline: it casts the card
//! Regenerate on a creature and asserts the one-shot destruction-replacement
//! shield (CR 701.19a) lands on that TARGET creature (not on the spell itself).
//! Reverting the name-mask fix makes the card parse to a no-op, so no shield is
//! installed and the assertion fails — i.e. this is a behavioral regression
//! test, not a parsed-AST-shape assertion.

use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::ShieldKind;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::CastPaymentMode;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

#[test]
fn regenerate_card_installs_shield_on_target_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let regen = scenario
        .add_spell_to_hand_from_oracle(P0, "Regenerate", false, "Regenerate target creature.")
        .id();

    let mut runner = scenario.build();

    // A vanilla creature starts with no replacement effects.
    assert!(
        runner.state().objects[&bear]
            .replacement_definitions
            .is_empty(),
        "precondition: the target creature has no replacement effects yet"
    );

    // Cast Regenerate targeting the creature (target chosen at cast time).
    let card_id = runner.state().objects[&regen].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: regen,
            card_id,
            targets: vec![bear],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Regenerate targeting the creature");
    runner.advance_until_stack_empty();

    // CR 701.19a: the card resolves to Regenerate on the TARGET creature,
    // installing a single one-shot destruction-replacement shield on it. The
    // shield is on the bear (the chosen target), not on the spell (self).
    assert_eq!(
        runner.state().objects[&bear].replacement_definitions.len(),
        1,
        "Regenerate must install exactly one destruction-replacement shield on the target creature",
    );
    assert_eq!(
        runner.state().objects[&bear].replacement_definitions[0].shield_kind,
        ShieldKind::Regeneration,
        "Regenerate must install a regeneration shield, not a generic replacement",
    );

    // The spell itself is a sorcery and resolves to the graveyard — the shield
    // was created on the bear, not on the resolving spell.
    assert_eq!(
        runner.state().objects[&regen].zone,
        Zone::Graveyard,
        "the Regenerate sorcery resolves to its owner's graveyard",
    );

    // CR 701.19a + CR 704.5g: the shield replaces the next destruction from
    // lethal damage, clears marked damage, taps the permanent, and is consumed.
    runner
        .state_mut()
        .objects
        .get_mut(&bear)
        .unwrap()
        .damage_marked = 4;
    let mut events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut events);

    let bear_obj = &runner.state().objects[&bear];
    assert_eq!(
        bear_obj.zone,
        Zone::Battlefield,
        "the target creature must survive the first lethal damage",
    );
    assert_eq!(bear_obj.damage_marked, 0, "regeneration clears damage");
    assert!(bear_obj.tapped, "regeneration taps the permanent");
    assert!(
        bear_obj.replacement_definitions[0].is_consumed,
        "the one-shot regeneration shield is consumed after firing",
    );
    assert!(
        events.iter().any(
            |event| matches!(event, GameEvent::Regenerated { object_id } if *object_id == bear)
        ),
        "regeneration must emit a Regenerated event for the target creature",
    );

    // CR 701.19a: the shield only protects the next destruction this turn; a
    // second lethal damage event destroys the creature.
    runner
        .state_mut()
        .objects
        .get_mut(&bear)
        .unwrap()
        .damage_marked = 4;
    let mut second_events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut second_events);

    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Graveyard,
        "the consumed shield must not protect the creature a second time",
    );
}
