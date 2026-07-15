//! Issue #5242: Mystic Reflection's delayed replacement must make the next
//! creature/planeswalker entry copy the creature chosen when Mystic resolved.

use engine::game::scenario::{GameScenario, P0};
use engine::types::phase::Phase;

const MYSTIC_REFLECTION: &str = "Choose target nonlegendary creature. The next time one or more creatures or planeswalkers enter this turn, they enter as copies of the chosen creature.";

#[test]
fn mystic_reflection_makes_next_creature_token_copy_chosen_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let chosen = scenario
        .add_creature(P0, "Colossal Dreadmaw", 6, 6)
        .with_subtypes(vec!["Dinosaur"])
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let token_spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Raise the Alarm",
            true,
            "Create a 1/1 white Soldier creature token.",
        )
        .id();

    let mut runner = scenario.build();
    runner.cast(mystic).target_object(chosen).resolve();
    runner.cast(token_spell).resolve();

    let copied_token = runner
        .state()
        .last_created_token_ids
        .first()
        .copied()
        .expect("token spell must create one token");
    let obj = runner
        .state()
        .objects
        .get(&copied_token)
        .expect("created token must exist");

    assert!(obj.is_token, "the entering object must still be a token");
    assert_eq!(obj.power, Some(6), "token must copy chosen creature power");
    assert_eq!(
        obj.toughness,
        Some(6),
        "token must copy chosen creature toughness"
    );
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Dinosaur"),
        "token must copy chosen creature subtype, got {:?}",
        obj.card_types.subtypes
    );
}
