//! Regression coverage for Raubahn, Bull of Ala Mhigo.
//!
//! The card's Ward payload is dynamic: it asks for life equal to Raubahn's
//! power when the Ward trigger resolves.  Its attack trigger also has an
//! optional Equipment target followed by a required attacking-creature target.

use engine::game::combat::{AttackerInfo, CombatState};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    ControllerRef, Effect, FilterProp, MultiTargetSpec, QuantityExpr, TargetFilter, TypeFilter,
};
use engine::types::keywords::{Keyword, WardCost};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

const RAUBAHN_ORACLE: &str = "Ward—Pay life equal to Raubahn's power.\nWhenever Raubahn attacks, attach up to one target Equipment you control to target attacking creature.";

fn assert_no_unimplemented(effect: &Effect, context: &str) {
    assert!(
        !matches!(effect, Effect::Unimplemented { .. }),
        "{context} must not be Unimplemented: {effect:?}"
    );
}

#[test]
fn raubahn_full_oracle_text_parses_ward_and_attack_attachment() {
    let parsed = parse_oracle_text(
        RAUBAHN_ORACLE,
        "Raubahn, Bull of Ala Mhigo",
        &["Ward".to_string()],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Human".to_string(), "Warrior".to_string()],
    );

    assert!(
        parsed
            .extracted_keywords
            .iter()
            .any(|keyword| matches!(keyword, Keyword::Ward(WardCost::PayLifeEqualToPower))),
        "Raubahn must retain its dynamic Ward cost: {:?}",
        parsed.extracted_keywords
    );
    let attack_trigger = parsed
        .triggers
        .iter()
        .filter(|trigger| trigger.mode == TriggerMode::Attacks)
        .find_map(|trigger| {
            trigger
                .execute
                .as_deref()
                .filter(|ability| matches!(ability.effect.as_ref(), Effect::Attach { .. }))
        })
        .expect("Raubahn's attack trigger must have an execute ability");
    assert_eq!(
        parsed
            .triggers
            .iter()
            .filter(|trigger| trigger.mode == TriggerMode::Attacks)
            .count(),
        1,
        "Raubahn must have exactly one attack trigger"
    );
    assert_no_unimplemented(&attack_trigger.effect, "Raubahn's attack trigger");
    let Effect::Attach { attachment, target } = attack_trigger.effect.as_ref() else {
        panic!(
            "Raubahn's attack trigger must attach an Equipment: {:?}",
            attack_trigger.effect
        );
    };
    assert_eq!(
        attack_trigger.multi_target,
        Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 1 })),
        "the Equipment target must be optional up to one"
    );
    let TargetFilter::Typed(equipment) = attachment else {
        panic!("Equipment target must be typed: {attachment:?}");
    };
    assert_eq!(equipment.controller, Some(ControllerRef::You));
    assert!(
        equipment
            .type_filters
            .iter()
            .any(|filter| matches!(filter, TypeFilter::Subtype(subtype) if subtype == "Equipment")),
        "attachment target must require the Equipment subtype: {equipment:?}"
    );
    let TargetFilter::Typed(attacker) = target else {
        panic!("attacking-creature target must be typed: {target:?}");
    };
    assert!(
        attacker
            .type_filters
            .iter()
            .any(|filter| matches!(filter, TypeFilter::Creature)),
        "host target must require a creature: {attacker:?}"
    );
    assert!(
        attacker
            .properties
            .iter()
            .any(|property| matches!(property, FilterProp::Attacking { defender: None })),
        "host target must require an attacking creature: {attacker:?}"
    );
    assert_eq!(attacker.controller, None);
}

#[test]
fn raubahn_attack_trigger_builds_optional_equipment_then_required_attacker_slots() {
    use engine::game::ability_utils::{build_resolved_from_def, build_target_slots};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let raubahn = scenario
        .add_creature_from_oracle(P0, "Raubahn, Bull of Ala Mhigo", 2, 2, RAUBAHN_ORACLE)
        .id();
    let equipment = scenario
        .add_creature(P0, "Test Blade", 0, 1)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .id();
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().combat = Some(CombatState {
        attackers: vec![AttackerInfo::attacking_player(attacker, P1)],
        ..Default::default()
    });

    let trigger = runner.state().objects[&raubahn]
        .trigger_definitions
        .iter_unchecked()
        .find(|trigger| trigger.definition.mode == TriggerMode::Attacks)
        .expect("Raubahn attack trigger");
    let definition = trigger
        .definition
        .execute
        .as_deref()
        .expect("trigger execute");
    let resolved = build_resolved_from_def(definition, raubahn, P0);
    let slots = build_target_slots(runner.state(), &resolved).expect("target slots");
    assert_eq!(slots.len(), 2);
    assert!(slots[0].optional, "Equipment selection must be optional");
    assert!(
        slots[0]
            .legal_targets
            .contains(&engine::types::ability::TargetRef::Object(equipment)),
        "controlled Equipment must be legal in the optional first slot"
    );
    assert!(
        !slots[1].optional,
        "attacking-creature selection is required"
    );
    assert_eq!(
        slots[1].legal_targets,
        vec![engine::types::ability::TargetRef::Object(attacker)]
    );
}
