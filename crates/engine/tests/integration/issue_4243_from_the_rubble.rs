//! Issue #4243: From the Rubble must target creature cards in your graveyard,
//! not creatures on the battlefield.

use engine::game::scenario::{GameScenario, P0};
use engine::game::targeting::find_legal_targets;
use engine::types::ability::{
    ChosenAttribute, Effect, FilterProp, TargetFilter, TargetRef, TypeFilter,
};
use engine::types::zones::Zone;

const FROM_THE_RUBBLE: &str = "As this enchantment enters, choose a creature type.\n\
At the beginning of your end step, return target creature card of the chosen type from your graveyard to the battlefield with a finality counter on it.";

#[test]
fn from_the_rubble_end_step_trigger_targets_graveyard_creature_cards() {
    let mut scenario = GameScenario::new();
    let rubble = scenario
        .add_creature(P0, "From the Rubble", 0, 0)
        .as_enchantment()
        .from_oracle_text(FROM_THE_RUBBLE)
        .id();
    let gy_dino = scenario
        .add_creature_to_graveyard(P0, "Graveyard Dinosaur", 3, 3)
        .with_subtypes(vec!["Dinosaur"])
        .id();
    let bf_dino = scenario
        .add_creature(P0, "Battlefield Dinosaur", 2, 2)
        .with_subtypes(vec!["Dinosaur"])
        .id();
    let mut runner = scenario.build();

    runner
        .state_mut()
        .objects
        .get_mut(&rubble)
        .unwrap()
        .chosen_attributes = vec![ChosenAttribute::CreatureType("Dinosaur".to_string())];

    let trigger = &runner.state().objects[&rubble].trigger_definitions[0];
    let execute = trigger
        .definition
        .execute
        .as_ref()
        .expect("trigger execute");
    let Effect::ChangeZone { target, origin, .. } = &*execute.effect else {
        panic!("expected ChangeZone trigger, got {:?}", execute.effect);
    };
    assert_eq!(*origin, Some(Zone::Graveyard));
    assert_eq!(
        target.extract_in_zone(),
        Some(Zone::Graveyard),
        "target filter must constrain to graveyard cards, got {target:?}"
    );
    match target {
        TargetFilter::Typed(tf) => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf.properties.contains(&FilterProp::IsChosenCreatureType));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::InZone { zone } if *zone == Zone::Graveyard)));
        }
        other => panic!("expected typed graveyard target filter, got {other:?}"),
    }

    let legal = find_legal_targets(runner.state(), target, P0, rubble);
    assert!(
        legal.contains(&TargetRef::Object(gy_dino)),
        "graveyard dinosaur must be a legal target"
    );
    assert!(
        !legal.contains(&TargetRef::Object(bf_dino)),
        "battlefield dinosaur must not be a legal target, got {legal:?}"
    );
}
