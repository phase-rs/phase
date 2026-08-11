//! Regression for Nadu's granted MaxTimesPerTurn trigger through Lavaspur Boots.
//!
//! Nadu grants the targeting trigger to each creature separately, and each
//! recipient's ability owns its own "twice each turn" limit. Each Equip
//! activation below uses the production targeting and trigger-collection pipeline.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{ContinuousModification, TriggerConstraint};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use std::sync::Arc;

const NADU_ORACLE: &str = "Flying\nCreatures you control have \"Whenever this creature becomes the target of a spell or ability, reveal the top card of your library. If it's a land card, put it onto the battlefield. Otherwise, put it into your hand. This ability triggers only twice each turn.\"";
const LAVASPUR_BOOTS_ORACLE: &str =
    "Equipped creature gets +1/+0 and has haste and ward {1}.\nEquip {1}";

#[test]
fn nadu_granted_trigger_has_independent_max_times_caps_per_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..9)
            .map(|_| {
                ManaUnit::new(
                    ManaType::Colorless,
                    engine::types::identifiers::ObjectId(0),
                    false,
                    vec![],
                )
            })
            .collect(),
    );
    scenario.with_library_top(
        P0,
        &[
            "Forest", "Island", "Mountain", "Plains", "Swamp", "Forest", "Island", "Mountain",
            "Plains",
        ],
    );

    let nadu = scenario
        .add_creature_from_oracle(P0, "Nadu, Winged Wisdom", 3, 4, NADU_ORACLE)
        .id();
    let first_target = scenario.add_vanilla(P0, 1, 1);
    let second_target = scenario.add_vanilla(P0, 1, 1);
    let third_target = scenario.add_vanilla(P0, 1, 1);
    let boots = scenario
        .add_creature(P0, "Lavaspur Boots", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(LAVASPUR_BOOTS_ORACLE)
        .id();

    let mut runner = scenario.build();
    {
        let nadu_object = runner.state_mut().objects.get_mut(&nadu).unwrap();
        for static_definition in Arc::make_mut(&mut nadu_object.base_static_definitions) {
            for modification in &mut static_definition.modifications {
                if let ContinuousModification::GrantTrigger { trigger } = modification {
                    trigger.constraint = Some(TriggerConstraint::MaxTimesPerTurn { max: 2 });
                }
            }
        }
        nadu_object.static_definitions = (*nadu_object.base_static_definitions).clone().into();
    }
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&boots].abilities.len(),
        1,
        "Lavaspur Boots must expose its Equip ability"
    );
    for target in [first_target, second_target, third_target] {
        assert!(
            runner.state().objects[&target]
                .trigger_definitions
                .as_slice()
                .iter()
                .any(|entry| matches!(
                    entry.definition.constraint,
                    Some(TriggerConstraint::MaxTimesPerTurn { max: 2 })
                )),
            "Nadu must grant its targeting trigger with MaxTimesPerTurn=2"
        );
    }
    let first_generation = runner.state().objects[&third_target]
        .trigger_occurrence_state
        .active_grants()
        .map(|(_, instance)| instance)
        .collect::<Vec<_>>();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        third_target,
        Zone::Exile,
        &mut Vec::new(),
    );
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        third_target,
        Zone::Battlefield,
        &mut Vec::new(),
    );
    evaluate_layers(runner.state_mut());
    let second_generation = runner.state().objects[&third_target]
        .trigger_occurrence_state
        .active_grants()
        .map(|(_, instance)| instance)
        .collect::<Vec<_>>();
    assert_ne!(
        first_generation, second_generation,
        "a blinked recipient must receive a fresh grant generation"
    );
    for target in [
        first_target,
        second_target,
        third_target,
        first_target,
        second_target,
        third_target,
        first_target,
        second_target,
        third_target,
    ] {
        runner.activate(boots, 0).target_object(target).resolve();
    }

    let counts = &runner.state().trigger_fire_counts_this_turn;
    assert_eq!(
        counts.values().sum::<u32>(),
        6,
        "Nadu's granted trigger must fire twice for each creature targeted by Equip"
    );
    assert_eq!(
        counts.len(),
        3,
        "each recipient must own an independent MaxTimesPerTurn ledger entry"
    );
    assert!(
        counts.values().all(|count| *count == 2),
        "each recipient's granted trigger must retain two uses"
    );
}
