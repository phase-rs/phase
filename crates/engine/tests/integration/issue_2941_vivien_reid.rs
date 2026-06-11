//! Issue #2941: Vivien Reid's -3 loyalty ("Destroy target artifact, enchantment,
//! or creature with flying") must be activatable whenever any artifact,
//! enchantment, or flying creature is a legal target. A prior parser bug spread
//! `WithKeyword(Flying)` onto every Or disjunct, so a lone enchantment did not
//! satisfy the activation gate.

use std::sync::Arc;

use engine::game::casting::can_activate_ability_now;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, Effect, TargetFilter,
    TypeFilter,
};
use engine::types::card_type::CoreType;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::CounterType;

const MINUS_THREE_ORACLE: &str = "Destroy target artifact, enchantment, or creature with flying.";

fn vivien_minus_three_destroy_effect() -> Effect {
    match parse_effect(MINUS_THREE_ORACLE) {
        Effect::Destroy { target, .. } => Effect::Destroy {
            target,
            cant_regenerate: false,
        },
        other => panic!("expected Destroy effect, got {other:?}"),
    }
}

fn add_battlefield_permanent(
    state: &mut engine::types::game_state::GameState,
    card_id: u64,
    player: engine::types::player::PlayerId,
    name: &str,
    core_type: CoreType,
) -> ObjectId {
    let oid = create_object(
        state,
        CardId(card_id),
        player,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&oid).expect("just created");
    obj.card_types.core_types.push(core_type);
    obj.base_card_types = obj.card_types.clone();
    oid
}

fn setup_vivien_with_minus_three() -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let vivien_id = scenario.add_creature(P0, "Vivien Reid", 0, 0).id();
    let mut runner = scenario.build();

    {
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&vivien_id).unwrap();
        obj.card_types.core_types.clear();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;
        obj.loyalty = Some(5);
        obj.counters.insert(CounterType::Loyalty, 5);

        let minus_three =
            AbilityDefinition::new(AbilityKind::Activated, vivien_minus_three_destroy_effect())
                .cost(AbilityCost::Loyalty { amount: -3 })
                .activation_restrictions(vec![ActivationRestriction::AsSorcery]);

        Arc::make_mut(&mut obj.abilities).push(minus_three.clone());
        Arc::make_mut(&mut obj.base_abilities).push(minus_three);
    }

    (runner, vivien_id)
}

fn minus_three_ability_index(
    state: &engine::types::game_state::GameState,
    vivien: ObjectId,
) -> usize {
    state
        .objects
        .get(&vivien)
        .unwrap()
        .abilities
        .iter()
        .position(|ability| matches!(ability.cost, Some(AbilityCost::Loyalty { amount: -3 })))
        .expect("Vivien -3 loyalty ability")
}

#[test]
fn vivien_minus_three_parser_binds_flying_only_to_creature_leg() {
    let Effect::Destroy { target, .. } = vivien_minus_three_destroy_effect() else {
        panic!("expected Destroy");
    };
    let TargetFilter::Or { filters } = target else {
        panic!("expected Or destroy target, got {target:?}");
    };
    assert_eq!(filters.len(), 3);

    let artifact_leg = &filters[0];
    let enchantment_leg = &filters[1];
    let creature_leg = &filters[2];

    let TargetFilter::Typed(artifact) = artifact_leg else {
        panic!("artifact leg should be Typed");
    };
    assert!(artifact.type_filters.contains(&TypeFilter::Artifact));
    assert!(!artifact
        .properties
        .iter()
        .any(|p| matches!(p, engine::types::ability::FilterProp::WithKeyword { .. })));

    let TargetFilter::Typed(enchantment) = enchantment_leg else {
        panic!("enchantment leg should be Typed");
    };
    assert!(enchantment.type_filters.contains(&TypeFilter::Enchantment));
    assert!(!enchantment
        .properties
        .iter()
        .any(|p| matches!(p, engine::types::ability::FilterProp::WithKeyword { .. })));

    let TargetFilter::Typed(creature) = creature_leg else {
        panic!("creature leg should be Typed");
    };
    assert!(creature.type_filters.contains(&TypeFilter::Creature));
    assert!(creature.properties.iter().any(|p| matches!(
        p,
        engine::types::ability::FilterProp::WithKeyword {
            value: Keyword::Flying
        }
    )));
}

#[test]
fn vivien_minus_three_activates_with_only_enchantment_on_battlefield() {
    let (mut runner, vivien) = setup_vivien_with_minus_three();
    let ability_index = minus_three_ability_index(runner.state(), vivien);

    add_battlefield_permanent(
        runner.state_mut(),
        201,
        P1,
        "Opp Enchantment",
        CoreType::Enchantment,
    );

    assert!(
        can_activate_ability_now(runner.state(), P0, vivien, ability_index),
        "lone opponent enchantment must make -3 activatable"
    );
}

#[test]
fn vivien_minus_three_activates_with_only_artifact_on_battlefield() {
    let (mut runner, vivien) = setup_vivien_with_minus_three();
    let ability_index = minus_three_ability_index(runner.state(), vivien);

    add_battlefield_permanent(
        runner.state_mut(),
        202,
        P1,
        "Opp Artifact",
        CoreType::Artifact,
    );

    assert!(
        can_activate_ability_now(runner.state(), P0, vivien, ability_index),
        "lone opponent artifact must make -3 activatable"
    );
}

#[test]
fn vivien_minus_three_activates_with_only_flying_creature_on_battlefield() {
    let (mut runner, vivien) = setup_vivien_with_minus_three();
    let ability_index = minus_three_ability_index(runner.state(), vivien);

    let flyer = add_battlefield_permanent(
        runner.state_mut(),
        203,
        P1,
        "Flying Creature",
        CoreType::Creature,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&flyer).unwrap();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.keywords.push(Keyword::Flying);
        obj.base_keywords.push(Keyword::Flying);
    }

    assert!(
        can_activate_ability_now(runner.state(), P0, vivien, ability_index),
        "flying creature must make -3 activatable"
    );
}

#[test]
fn vivien_minus_three_not_activatable_with_only_nonflying_creature() {
    let (mut runner, vivien) = setup_vivien_with_minus_three();
    let ability_index = minus_three_ability_index(runner.state(), vivien);

    let walker = add_battlefield_permanent(
        runner.state_mut(),
        204,
        P1,
        "Ground Creature",
        CoreType::Creature,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&walker).unwrap();
        obj.power = Some(3);
        obj.toughness = Some(3);
        obj.base_power = Some(3);
        obj.base_toughness = Some(3);
    }

    assert!(
        !can_activate_ability_now(runner.state(), P0, vivien, ability_index),
        "non-flying creature alone must not enable -3"
    );
}
