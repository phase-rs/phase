//! Fractalize: numeric animation base power/toughness expressions.

use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::{add_to_zone, remove_from_zone};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{ContinuousModification, Effect, QuantityExpr, QuantityRef};
use engine::types::ability_visit::visit_ability_def;
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use std::ops::ControlFlow;

const FRACTALIZE: &str = "Until end of turn, target creature becomes a green and blue Fractal with base power and toughness each equal to X plus 1. (It loses all other colors and creature types.)";

fn fractalize_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::X, ManaCostShard::Blue],
        generic: 0,
    }
}

fn floating_blue(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
        .collect()
}

fn add_fractalize(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Fractalize", true, FRACTALIZE)
        .with_mana_cost(fractalize_cost())
        .id()
}

fn make_artifact_creature(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) {
    let object = runner.state_mut().objects.get_mut(&id).unwrap();
    object.card_types.core_types.push(CoreType::Artifact);
    object.base_card_types.core_types.push(CoreType::Artifact);
}

/// Fractalize's actual Oracle text must carry its announced X through the cast
/// pipeline to the existing dynamic base-P/T layer modification.
#[test]
fn fractalize_x_plus_one_changes_target_characteristics() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario
        .add_creature(P1, "Victim", 2, 5)
        .with_subtypes(vec!["Human"])
        .id();
    let unrelated = scenario.add_creature(P1, "Unrelated", 7, 8).id();
    let spell = add_fractalize(&mut scenario);
    scenario.with_mana_pool(P0, floating_blue(4));

    let mut runner = scenario.build();
    make_artifact_creature(&mut runner, victim);
    {
        let object = runner.state_mut().objects.get_mut(&victim).unwrap();
        object.color = vec![ManaColor::Red];
        object.base_color = vec![ManaColor::Red];
    }

    let outcome = runner.cast(spell).x(3).target_object(victim).resolve();
    let state = outcome.state();
    let transformed = &state.objects[&victim];

    assert_eq!(state.objects[&spell].zone, Zone::Graveyard);
    assert!(transformed.color.contains(&ManaColor::Green));
    assert!(transformed.color.contains(&ManaColor::Blue));
    assert!(transformed
        .card_types
        .subtypes
        .iter()
        .any(|s| s == "Fractal"));
    assert!(transformed
        .card_types
        .core_types
        .contains(&CoreType::Artifact));
    assert_eq!(state.objects[&unrelated].power, Some(7));
    assert_eq!(state.objects[&unrelated].toughness, Some(8));

    assert_eq!(
        transformed.power,
        Some(4),
        "CR 107.3a: announced X=3 plus one must set base power to 4"
    );
    assert_eq!(
        transformed.toughness,
        Some(4),
        "CR 107.3a: announced X=3 plus one must set base toughness to 4"
    );
}

#[test]
fn fractalize_zero_x_sets_one_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Zero Target", 6, 2).id();
    let spell = add_fractalize(&mut scenario);
    scenario.with_mana_pool(P0, floating_blue(1));

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).x(0).target_object(victim).resolve();
    let transformed = &outcome.state().objects[&victim];

    assert!(transformed.color.contains(&ManaColor::Green));
    assert!(transformed
        .card_types
        .subtypes
        .iter()
        .any(|s| s == "Fractal"));
    assert_eq!(outcome.state().objects[&spell].zone, Zone::Graveyard);
    assert_eq!(transformed.power, Some(1));
    assert_eq!(transformed.toughness, Some(1));
}

const X_MINUS_ONE_ANIMATION: &str = "Until end of turn, target creature becomes a green Fractal with base power and toughness each equal to X minus 1.";

fn add_x_minus_one_animation(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "X Minus One", true, X_MINUS_ONE_ANIMATION)
        .with_mana_cost(fractalize_cost())
        .id()
}

/// CR 107.1b, 613.4b-c: a negative value used to set base P/T remains negative
/// in layer 7b, so two +1/+1 counters applied in layer 7c produce 1/1.
#[test]
fn x_minus_one_animation_preserves_negative_base_pt_before_counters() {
    let mut zero_scenario = GameScenario::new();
    zero_scenario.at_phase(Phase::PreCombatMain);
    let zero_target = zero_scenario
        .add_creature(P1, "Counter Target", 6, 6)
        .with_plus_counters(2)
        .id();
    let zero_spell = add_x_minus_one_animation(&mut zero_scenario);
    zero_scenario.with_mana_pool(P0, floating_blue(1));
    let mut zero_runner = zero_scenario.build();
    let zero_outcome = zero_runner
        .cast(zero_spell)
        .x(0)
        .target_object(zero_target)
        .resolve();
    let zero_result = &zero_outcome.state().objects[&zero_target];
    assert!(zero_result.color.contains(&ManaColor::Green));
    assert_eq!(
        zero_outcome.state().objects[&zero_spell].zone,
        Zone::Graveyard
    );
    assert_eq!(zero_result.power, Some(1));
    assert_eq!(zero_result.toughness, Some(1));
}

#[test]
fn x_minus_one_animation_positive_x_still_sets_pt() {
    let mut positive_scenario = GameScenario::new();
    positive_scenario.at_phase(Phase::PreCombatMain);
    let positive_target = positive_scenario
        .add_creature(P1, "Positive Target", 8, 8)
        .id();
    let positive_spell = add_x_minus_one_animation(&mut positive_scenario);
    positive_scenario.with_mana_pool(P0, floating_blue(5));
    let mut positive_runner = positive_scenario.build();
    let positive_outcome = positive_runner
        .cast(positive_spell)
        .x(4)
        .target_object(positive_target)
        .resolve();
    let positive_result = &positive_outcome.state().objects[&positive_target];
    assert!(positive_result.color.contains(&ManaColor::Green));
    assert_eq!(
        positive_outcome.state().objects[&positive_spell].zone,
        Zone::Graveyard
    );
    assert_eq!(positive_result.power, Some(3));
    assert_eq!(positive_result.toughness, Some(3));
}

#[test]
fn fractalize_snapshots_each_spell_announced_x_independently() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first_target = scenario.add_creature(P1, "First Target", 9, 1).id();
    let second_target = scenario.add_creature(P1, "Second Target", 1, 9).id();
    let first_spell = add_fractalize(&mut scenario);
    let second_spell = add_fractalize(&mut scenario);
    scenario.with_mana_pool(P0, floating_blue(9));

    let mut runner = scenario.build();
    runner
        .cast(first_spell)
        .x(2)
        .target_object(first_target)
        .resolve();
    assert_eq!(runner.state().objects[&first_target].power, Some(3));
    assert_eq!(runner.state().objects[&first_target].toughness, Some(3));

    runner
        .cast(second_spell)
        .x(5)
        .target_object(second_target)
        .resolve();
    assert_eq!(runner.state().objects[&first_target].power, Some(3));
    assert_eq!(runner.state().objects[&first_target].toughness, Some(3));
    assert_eq!(runner.state().objects[&second_target].power, Some(6));
    assert_eq!(runner.state().objects[&second_target].toughness, Some(6));

    // CR 514.2: the continuous animation expires in the cleanup step.
    runner.advance_to_phase(Phase::End);
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(runner.state().objects[&first_target].power, Some(9));
    assert_eq!(runner.state().objects[&first_target].toughness, Some(1));
    assert_eq!(runner.state().objects[&second_target].power, Some(1));
    assert_eq!(runner.state().objects[&second_target].toughness, Some(9));
}

const KARNS_TOUCH: &str = "Target noncreature artifact becomes an artifact creature with power and toughness each equal to its mana value until end of turn. (It retains its abilities.)";

#[test]
fn karns_touch_keeps_recipient_mana_value_animation_path() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artifact = scenario
        .add_artifact_from_oracle(P1, "Five Relic", "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 5,
        })
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Karn's Touch", true, KARNS_TOUCH)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(P0, floating_blue(1));

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(artifact).resolve();
    let animated = &outcome.state().objects[&artifact];
    assert_eq!(outcome.state().objects[&spell].zone, Zone::Graveyard);
    assert!(animated.card_types.core_types.contains(&CoreType::Artifact));
    assert!(animated.card_types.core_types.contains(&CoreType::Creature));
    assert_eq!(animated.power, Some(5));
    assert_eq!(animated.toughness, Some(5));
}

#[test]
fn fractalize_rejects_no_target_and_skips_an_illegal_target_at_resolution() {
    let mut empty_scenario = GameScenario::new();
    empty_scenario.at_phase(Phase::PreCombatMain);
    let unavailable_spell = add_fractalize(&mut empty_scenario);
    empty_scenario.with_mana_pool(P0, floating_blue(4));
    let empty_runner = empty_scenario.build();
    assert!(
        !can_cast_object_now(empty_runner.state(), P0, unavailable_spell),
        "Fractalize must be unavailable without a legal creature target"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Leaving Target", 2, 5).id();
    let untouched = scenario.add_creature(P1, "Untouched", 7, 8).id();
    let spell = add_fractalize(&mut scenario);
    scenario.with_mana_pool(P0, floating_blue(4));
    let mut runner = scenario.build();
    let mut committed = runner.cast(spell).x(3).target_object(victim).commit();
    remove_from_zone(committed.state_mut(), victim, Zone::Battlefield, P1);
    add_to_zone(committed.state_mut(), victim, Zone::Exile, P1);
    committed.state_mut().objects.get_mut(&victim).unwrap().zone = Zone::Exile;

    let outcome = committed.resolve();
    assert_eq!(outcome.state().objects[&victim].zone, Zone::Exile);
    assert_eq!(outcome.state().objects[&spell].zone, Zone::Graveyard);
    assert_eq!(outcome.state().objects[&untouched].power, Some(7));
    assert_eq!(outcome.state().objects[&untouched].toughness, Some(8));
}

#[test]
fn fractalize_full_document_is_clean_while_unsupported_expression_stays_honest() {
    let types = vec!["Instant".to_owned()];
    let parsed = parse_oracle_text(FRACTALIZE, "Fractalize", &[], &types, &[]);
    assert_eq!(
        parsed.abilities.len(),
        1,
        "Fractalize must produce its one complete spell ability: {parsed:#?}"
    );
    assert!(
        parsed.triggers.is_empty() && parsed.statics.is_empty() && parsed.replacements.is_empty(),
        "Fractalize's complete document must not hide unsupported effects in another output tree: {parsed:#?}"
    );

    let ability = &parsed.abilities[0];
    let mut has_unimplemented = false;
    let mut visit = |effect: &Effect| {
        has_unimplemented |= matches!(effect, Effect::Unimplemented { .. });
        ControlFlow::Continue(())
    };
    let _ = visit_ability_def(ability, &mut visit);
    assert!(
        !has_unimplemented,
        "Fractalize must contain zero unsupported effect nodes before its warning-free diagnostic is trusted: {ability:#?}"
    );

    let expected = QuantityExpr::Offset {
        inner: Box::new(QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_owned(),
            },
        }),
        offset: 1,
    };
    let Effect::GenericEffect {
        static_abilities, ..
    } = &*ability.effect
    else {
        panic!(
            "Fractalize must produce a complete dynamic P/T GenericEffect, got {:#?}",
            ability.effect
        );
    };
    assert!(
        static_abilities.iter().any(|static_def| {
            static_def
                .modifications
                .contains(&ContinuousModification::SetPowerDynamic {
                    value: expected.clone(),
                })
                && static_def.modifications.contains(
                    &ContinuousModification::SetToughnessDynamic {
                        value: expected.clone(),
                    })
        }),
        "Fractalize must produce complete dynamic X plus 1 power and toughness effects: {static_abilities:#?}"
    );
    assert!(
        parsed.parse_warnings.is_empty(),
        "complete Fractalize text must not retain a parser warning: {:#?}",
        parsed.parse_warnings
    );

    let unsupported = parse_oracle_text(
        "Until end of turn, target creature becomes a green Fractal with base power and toughness each equal to X plus X.",
        "Unsupported Animation",
        &[],
        &types,
        &[],
    );
    assert!(
        !unsupported.parse_warnings.is_empty(),
        "unsupported X plus X must remain visibly unsupported rather than cleanly swallowed"
    );
}
