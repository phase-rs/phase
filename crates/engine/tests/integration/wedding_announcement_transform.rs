//! Regression for Wedding Announcement's end-step Human branch.
//!
//! CR 608.2c: the final transform instruction follows the if/otherwise choice,
//! so it must run after creating a Human as well as after drawing a card.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::game_object::BackFaceData;
use engine::game::layers::flush_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, ContinuousModification, ControllerRef, Effect, StaticDefinition, TargetFilter,
    TypeFilter, TypedFilter,
};
use engine::types::card_type::{CardType, CoreType};
use engine::types::counter::CounterType;
use engine::types::identifiers::CardId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::zones::Zone;

#[test]
fn wedding_announcement_human_branch_transforms_and_applies_festivity_anthem() {
    let execute = parse_effect_chain(
        "Put an invitation counter on this enchantment. If you attacked with two or more creatures this turn, draw a card. Otherwise, create a 1/1 white Human creature token. Then if this enchantment has three or more invitation counters on it, transform it.",
        AbilityKind::Spell,
    );
    let conditional = execute
        .sub_ability
        .as_deref()
        .expect("draw-or-Human conditional must follow the invitation counter");
    let human = conditional
        .else_ability
        .as_deref()
        .expect("conditional must retain its Human otherwise branch");
    assert!(matches!(*human.effect, Effect::Token { .. }));
    assert!(
        matches!(
            human.sub_ability.as_deref().map(|tail| &*tail.effect),
            Some(Effect::Transform { .. })
        ),
        "the independent transform tail must be present on the Human branch"
    );

    let mut scenario = GameScenario::new();
    let existing_creature = scenario.add_vanilla(P0, 2, 2);
    let mut runner = scenario.build();
    let wedding = {
        let state = runner.state_mut();
        let wedding = create_object(
            state,
            CardId(8100),
            P0,
            "Wedding Announcement".to_string(),
            Zone::Battlefield,
        );
        let object = state.objects.get_mut(&wedding).unwrap();
        object.card_types.core_types.push(CoreType::Enchantment);
        object.base_card_types = object.card_types.clone();
        object
            .counters
            .insert(CounterType::Generic("invitation".to_string()), 2);
        object.back_face = Some(BackFaceData {
            name: "Wedding Festivity".to_string(),
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            card_types: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Enchantment],
                subtypes: vec![],
            },
            mana_cost: ManaCost::default(),
            keywords: vec![],
            abilities: vec![],
            trigger_definitions: Default::default(),
            replacement_definitions: Default::default(),
            static_definitions: vec![StaticDefinition::continuous()
                .affected(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: Some(ControllerRef::You),
                    properties: vec![],
                }))
                .modifications(vec![
                    ContinuousModification::AddPower { value: 1 },
                    ContinuousModification::AddToughness { value: 1 },
                ])]
            .into(),
            color: vec![ManaColor::White],
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            layout_kind: None,
        });
        wedding
    };

    let resolved = build_resolved_from_def(&execute, wedding, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("Wedding Announcement end-step trigger must resolve");
    flush_layers(runner.state_mut());

    let state = runner.state();
    let festivity = &state.objects[&wedding];
    assert!(
        festivity.transformed,
        "the third invitation counter must transform the enchantment"
    );
    assert_eq!(festivity.name, "Wedding Festivity");
    assert_eq!(
        festivity
            .counters
            .get(&CounterType::Generic("invitation".to_string()))
            .copied(),
        Some(3),
        "Wedding Festivity keeps its invitation counters"
    );

    let human = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .find(|object| object.name == "Human" && object.controller == P0)
        .expect("the otherwise branch must create a Human token");
    assert_eq!((human.power, human.toughness), (Some(2), Some(2)));
    let existing = &state.objects[&existing_creature];
    assert_eq!((existing.power, existing.toughness), (Some(3), Some(3)));
}
