//! Regression for issue #2418: Timber Paladin must not enter as 10/10 with
//! trample and vigilance when enchanted by zero Auras.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::ability::{
    Comparator, ContinuousModification, FilterProp, QuantityExpr, QuantityRef, StaticCondition,
    StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::card_type::{CardType, CoreType};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::KeywordKind;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn aura_count_condition(comparator: Comparator, count: i32) -> StaticCondition {
    StaticCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![
                        TypeFilter::Enchantment,
                        TypeFilter::Subtype("Aura".to_string()),
                    ],
                    controller: None,
                    properties: vec![FilterProp::AttachedToSource],
                }),
            },
        },
        comparator,
        rhs: QuantityExpr::Fixed { value: count },
    }
}

fn timber_paladin_statics() -> Vec<StaticDefinition> {
    vec![
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .condition(aura_count_condition(Comparator::EQ, 1))
            .modifications(vec![
                ContinuousModification::SetPower { value: 3 },
                ContinuousModification::SetToughness { value: 3 },
            ]),
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .condition(aura_count_condition(Comparator::EQ, 2))
            .modifications(vec![
                ContinuousModification::SetPower { value: 5 },
                ContinuousModification::SetToughness { value: 5 },
            ]),
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .condition(aura_count_condition(Comparator::GE, 3))
            .modifications(vec![
                ContinuousModification::SetPower { value: 10 },
                ContinuousModification::SetToughness { value: 10 },
            ]),
    ]
}

#[test]
fn timber_paladin_without_auras_keeps_base_power_toughness() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    let paladin = create_object(
        runner.state_mut(),
        CardId(1),
        P0,
        "Timber Paladin".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&paladin).unwrap();
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Artifact, CoreType::Creature],
            subtypes: vec!["Knight".to_string()],
        };
        obj.power = Some(0);
        obj.toughness = Some(0);
        obj.base_power = Some(0);
        obj.base_toughness = Some(0);
        obj.static_definitions = timber_paladin_statics().into();
    }

    evaluate_layers(runner.state_mut());

    let obj = runner.state().objects.get(&paladin).unwrap();
    assert_eq!(obj.power, Some(0), "0 Auras must not apply any tier");
    assert_eq!(obj.toughness, Some(0));
    assert!(
        !obj.keywords
            .iter()
            .any(|k| k.kind() == KeywordKind::Vigilance),
        "vigilance requires exactly two Auras"
    );
}

#[test]
fn timber_paladin_scales_with_attached_aura_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    let paladin = create_object(
        runner.state_mut(),
        CardId(10),
        P0,
        "Timber Paladin".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&paladin).unwrap();
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Artifact, CoreType::Creature],
            subtypes: vec!["Knight".to_string()],
        };
        obj.power = Some(0);
        obj.toughness = Some(0);
        obj.base_power = Some(0);
        obj.base_toughness = Some(0);
        obj.static_definitions = timber_paladin_statics().into();
    }

    let attach_aura = |state: &mut engine::types::game_state::GameState, host: ObjectId| {
        let aura = create_object(
            state,
            CardId(9000 + host.0),
            P0,
            "Aura".to_string(),
            Zone::Battlefield,
        );
        {
            let aura_obj = state.objects.get_mut(&aura).unwrap();
            aura_obj.card_types.core_types.push(CoreType::Enchantment);
            aura_obj.card_types.subtypes.push("Aura".to_string());
            aura_obj.attached_to = Some(engine::game::game_object::AttachTarget::Object(host));
        }
        {
            let host_obj = state.objects.get_mut(&host).unwrap();
            host_obj.attachments.push(aura);
        }
        aura
    };

    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&paladin].power,
        Some(0),
        "fresh Timber Paladin with 0 Auras"
    );

    attach_aura(runner.state_mut(), paladin);
    evaluate_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&paladin].power, Some(3));

    attach_aura(runner.state_mut(), paladin);
    evaluate_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&paladin].power, Some(5));

    attach_aura(runner.state_mut(), paladin);
    evaluate_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&paladin].power, Some(10));
}
