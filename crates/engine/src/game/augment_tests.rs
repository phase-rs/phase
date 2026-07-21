use crate::database::augment::synthesize_augment;
use crate::game::augment::resolve_combine_host;
use crate::game::effects::become_copy;
use crate::game::merge::{merge_object_onto, MergeSide};
use crate::game::printed_cards::apply_card_face_to_object;
use crate::game::zones::create_object;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, CombineSource, Effect, PtValue, QuantityExpr, ResolvedAbility,
    TargetFilter, TargetRef, TriggerDefinition,
};
use crate::types::card::CardFace;
use crate::types::card_type::{CoreType, Supertype};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::CardId;
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

fn host_face() -> CardFace {
    let mut face = CardFace {
        name: "Adorable Kitten".to_string(),
        ..CardFace::default()
    };
    face.card_type.core_types.push(CoreType::Creature);
    face.card_type.supertypes.push(Supertype::Host);
    face.card_type.subtypes.push("Cat".to_string());
    face.power = Some(PtValue::Fixed(1));
    face.toughness = Some(PtValue::Fixed(1));
    face.triggers.push(
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    player: TargetFilter::Controller,
                },
            ))
            .description("When ~ enters, you gain 2 life.".to_string())
            .valid_card(TargetFilter::SelfRef)
            .destination(Zone::Battlefield)
            .trigger_zones(vec![Zone::Battlefield]),
    );
    face
}

fn augment_face() -> CardFace {
    let mut face = CardFace {
        name: "Monkey-".to_string(),
        oracle_text: Some(
            "Whenever a nontoken creature you control dies,\nAugment {2}{G} ({2}{G}, Reveal this card from your hand: Combine it with target host. Augment only as a sorcery.)"
                .to_string(),
        ),
        ..CardFace::default()
    };
    face.card_type.core_types.push(CoreType::Creature);
    face.card_type.subtypes.push("Monkey".to_string());
    face.power = Some(PtValue::Fixed(2));
    face.toughness = Some(PtValue::Fixed(2));
    face.keywords.push(crate::types::keywords::Keyword::Augment);
    face.abilities.push(
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Unimplemented {
                name: "unknown".to_string(),
                description: Some("Augment {2}{G}".to_string()),
            },
        )
        .description("Augment {2}{G}".to_string()),
    );
    face.triggers.push(
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .description("Whenever a nontoken creature you control dies,".to_string())
            .origin(Zone::Battlefield)
            .destination(Zone::Graveyard)
            .trigger_zones(vec![Zone::Battlefield]),
    );
    synthesize_augment(&mut face);
    face
}

fn copied_host_face() -> CardFace {
    let mut face = CardFace {
        name: "Curious Puppy".to_string(),
        ..CardFace::default()
    };
    face.card_type.core_types.push(CoreType::Creature);
    face.card_type.supertypes.push(Supertype::Host);
    face.card_type.subtypes.push("Dog".to_string());
    face.power = Some(PtValue::Fixed(3));
    face.toughness = Some(PtValue::Fixed(4));
    face.color_override = Some(vec![ManaColor::Green]);
    face.triggers.push(
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 5 },
                    player: TargetFilter::Controller,
                },
            ))
            .description("When ~ enters, you gain 5 life.".to_string())
            .valid_card(TargetFilter::SelfRef)
            .destination(Zone::Battlefield)
            .trigger_zones(vec![Zone::Battlefield]),
    );
    face
}

fn rider_face() -> CardFace {
    let mut face = CardFace {
        name: "Sky Rider".to_string(),
        ..CardFace::default()
    };
    face.card_type.core_types.push(CoreType::Creature);
    face.card_type.supertypes.push(Supertype::Host);
    face.card_type.subtypes.push("Bird".to_string());
    face.power = Some(PtValue::Fixed(4));
    face.toughness = Some(PtValue::Fixed(4));
    face.color_override = Some(vec![ManaColor::Blue]);
    face
}

fn gain_life_amount(trigger: &TriggerDefinition) -> Option<i32> {
    match trigger
        .execute
        .as_deref()
        .map(|ability| ability.effect.as_ref())
    {
        Some(Effect::GainLife {
            amount: QuantityExpr::Fixed { value },
            ..
        }) => Some(*value),
        _ => None,
    }
}

#[test]
fn combine_host_merges_augmented_values_onto_host() {
    let mut state = GameState::new_two_player(1);
    let host = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Adorable Kitten".to_string(),
        Zone::Battlefield,
    );
    let augment = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Monkey-".to_string(),
        Zone::Hand,
    );
    apply_card_face_to_object(state.objects.get_mut(&host).unwrap(), &host_face());
    apply_card_face_to_object(state.objects.get_mut(&augment).unwrap(), &augment_face());

    let ability = ResolvedAbility::new(
        Effect::CombineHost {
            source: CombineSource::SpecificObject { id: augment },
            host: Box::new(TargetFilter::SpecificObject { id: host }),
        },
        Vec::new(),
        augment,
        PlayerId(0),
    );
    let mut events = Vec::<GameEvent>::new();
    resolve_combine_host(&mut state, &ability, &mut events).unwrap();

    let merged = &state.objects[&host];
    assert_eq!(merged.name, "Monkey-Kitten");
    assert_eq!(merged.power, Some(3));
    assert_eq!(merged.toughness, Some(3));
    assert_eq!(
        merged.merge_kind,
        Some(crate::game::game_object::MergeKind::Augment)
    );
    assert!(!merged.card_types.supertypes.contains(&Supertype::Host));
    assert!(merged.card_types.subtypes.contains(&"Cat".to_string()));
    assert!(merged.card_types.subtypes.contains(&"Monkey".to_string()));
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::Augmented {
                merged_id,
                augmenting_id,
                ..
            } if *merged_id == host && *augmenting_id == augment
        )),
        "augment should emit its dedicated Host/Augment event"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::Mutated { .. })),
        "augment must not emit the mutate-only event or satisfy mutate triggers"
    );
}

#[test]
fn combine_host_uses_current_copiable_values_after_copy_effect() {
    let mut state = GameState::new_two_player(1);
    let host = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Adorable Kitten".to_string(),
        Zone::Battlefield,
    );
    let copied_host = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Curious Puppy".to_string(),
        Zone::Battlefield,
    );
    let augment = create_object(
        &mut state,
        CardId(3),
        PlayerId(0),
        "Monkey-".to_string(),
        Zone::Hand,
    );
    apply_card_face_to_object(state.objects.get_mut(&host).unwrap(), &host_face());
    apply_card_face_to_object(
        state.objects.get_mut(&copied_host).unwrap(),
        &copied_host_face(),
    );
    apply_card_face_to_object(state.objects.get_mut(&augment).unwrap(), &augment_face());

    let mut events = Vec::<GameEvent>::new();
    let copy_ability = ResolvedAbility::new(
        Effect::BecomeCopy {
            recipient: TargetFilter::SelfRef,
            target: TargetFilter::Any,
            duration: None,
            mana_value_limit: None,
            additional_modifications: Vec::new(),
        },
        vec![TargetRef::Object(copied_host)],
        host,
        PlayerId(0),
    );
    become_copy::resolve(&mut state, &copy_ability, &mut events).unwrap();

    let ability = ResolvedAbility::new(
        Effect::CombineHost {
            source: CombineSource::SpecificObject { id: augment },
            host: Box::new(TargetFilter::SpecificObject { id: host }),
        },
        Vec::new(),
        augment,
        PlayerId(0),
    );
    resolve_combine_host(&mut state, &ability, &mut events).unwrap();

    let merged = &state.objects[&host];
    assert_eq!(merged.name, "Monkey-Puppy");
    assert_eq!(merged.power, Some(5));
    assert_eq!(merged.toughness, Some(6));
    assert_eq!(merged.color, vec![ManaColor::Green]);
    assert!(merged.card_types.subtypes.contains(&"Dog".to_string()));
    assert!(merged.card_types.subtypes.contains(&"Monkey".to_string()));
    assert_eq!(
        merged
            .trigger_definitions
            .iter_all()
            .find_map(|entry| gain_life_amount(&entry.definition)),
        Some(5)
    );
}

#[test]
fn combine_host_preserves_existing_merged_stack_and_current_values() {
    let mut state = GameState::new_two_player(1);
    let host = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Adorable Kitten".to_string(),
        Zone::Battlefield,
    );
    let rider = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Sky Rider".to_string(),
        Zone::Battlefield,
    );
    let augment = create_object(
        &mut state,
        CardId(3),
        PlayerId(0),
        "Monkey-".to_string(),
        Zone::Hand,
    );
    apply_card_face_to_object(state.objects.get_mut(&host).unwrap(), &host_face());
    apply_card_face_to_object(state.objects.get_mut(&rider).unwrap(), &rider_face());
    apply_card_face_to_object(state.objects.get_mut(&augment).unwrap(), &augment_face());

    let mut events = Vec::<GameEvent>::new();
    merge_object_onto(&mut state, rider, host, MergeSide::Top, &mut events);

    let ability = ResolvedAbility::new(
        Effect::CombineHost {
            source: CombineSource::SpecificObject { id: augment },
            host: Box::new(TargetFilter::SpecificObject { id: host }),
        },
        Vec::new(),
        augment,
        PlayerId(0),
    );
    resolve_combine_host(&mut state, &ability, &mut events).unwrap();

    let merged = &state.objects[&host];
    assert_eq!(merged.name, "Monkey-Rider");
    assert_eq!(merged.power, Some(6));
    assert_eq!(merged.toughness, Some(6));
    assert_eq!(merged.color, vec![ManaColor::Blue]);
    assert_eq!(
        merged.merged_components,
        vec![rider, host, augment],
        "augment should extend the existing merged stack without discarding prior components"
    );
}
