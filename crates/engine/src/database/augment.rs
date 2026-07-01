//! Augment / Host synthesis for Unstable.

use crate::database::mtgjson::parse_mtgjson_mana_cost;
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, AbilityTag, CombineSource,
    ControllerRef, Effect, FilterProp, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::card::CardFace;
use crate::types::card_type::Supertype;
use crate::types::keywords::{Keyword, KeywordKind};
use crate::types::mana::ManaCost;
use crate::types::triggers::TriggerMode;
use crate::types::zones::EtbTapState;
use crate::types::zones::Zone;

pub fn synthesize_augment(face: &mut CardFace) {
    let oracle = face.oracle_text.clone().unwrap_or_default();
    let has_augment = face
        .keywords
        .iter()
        .any(|keyword| matches!(keyword, Keyword::Augment));
    let references_host_combine = oracle.contains("combine it with")
        || oracle.contains("combine this card")
        || oracle.contains("card with augment");
    if !has_augment && !references_host_combine {
        return;
    }

    if has_augment {
        synthesize_augment_half(face, &oracle);
    }

    match face.name.as_str() {
        "Teacher's Pet" => rewrite_teachers_pet(face),
        "Dr. Julius Jumblemorph" => rewrite_dr_julius(face),
        "Strutting Turkey" => rewrite_strutting_turkey(face),
        _ => {}
    }
}

fn synthesize_augment_half(face: &mut CardFace, oracle: &str) {
    let Some(cost) = augment_cost(face, oracle) else {
        return;
    };
    let printed_augment_description = face
        .abilities
        .iter()
        .filter_map(|ability| ability.description.clone())
        .find(|description| description.starts_with("Augment "))
        .unwrap_or_else(|| "Augment".to_string());

    face.abilities.retain(|ability| {
        let Some(description) = ability.description.as_deref() else {
            return true;
        };
        if description.starts_with("Augment ") {
            return false;
        }
        if description
            == "You may activate this card's augment ability any time you could cast an instant."
        {
            return false;
        }
        true
    });

    for ability in &mut face.abilities {
        let is_prefix_placeholder = ability.kind == AbilityKind::Activated
            && matches!(ability.effect.as_ref(), Effect::Unimplemented { name, .. } if name == "empty")
            && ability
                .description
                .as_deref()
                .is_some_and(|description| description.trim_end().ends_with(':'));
        if is_prefix_placeholder {
            *ability.effect = Effect::NoOp;
        }
    }

    let mut augment_ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::CombineHost {
            source: CombineSource::Source,
            host: Box::new(host_filter()),
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana { cost },
            AbilityCost::Reveal {
                count: 1,
                filter: Some(TargetFilter::SelfRef),
            },
        ],
    })
    .description(printed_augment_description);
    augment_ability.activation_zone = Some(Zone::Hand);
    augment_ability.ability_tag = Some(AbilityTag::Augment);
    if !oracle.contains("any time you could cast an instant") {
        augment_ability = augment_ability.sorcery_speed();
    }
    face.abilities.push(augment_ability);

    if face.name == "Zombified" {
        for ability in &mut face.abilities {
            if ability.description.as_deref()
                == Some("{4}{B}: Combine this card from your graveyard with target host.")
            {
                *ability.effect = Effect::CombineHost {
                    source: CombineSource::Source,
                    host: Box::new(host_filter()),
                };
                ability.activation_zone = Some(Zone::Graveyard);
            }
        }
    }
}

fn rewrite_teachers_pet(face: &mut CardFace) {
    let Some(cost) = face
        .abilities
        .first()
        .and_then(|ability| ability.cost.clone())
    else {
        return;
    };
    face.abilities.clear();
    face.abilities.push(
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::ChooseAugmentAndCombineWithHost {
                zones: vec![Zone::Library],
                filter: Box::new(augment_card_filter()),
                host: Box::new(controlled_host_filter()),
            },
        )
        .cost(cost)
        .description(
            "{2}{W}, Sacrifice ~: Search your library for a card with augment, combine it with target host you control, then shuffle."
                .to_string(),
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Shuffle {
                target: TargetFilter::Controller,
            },
        )),
    );
}

fn rewrite_dr_julius(face: &mut CardFace) {
    for trigger in &mut face.triggers {
        if !trigger.description.as_deref().is_some_and(|description| {
            description.contains("search your library and/or graveyard for a card with augment")
        }) {
            continue;
        }
        trigger.valid_card = Some(controlled_host_filter());
        trigger.mode = TriggerMode::ChangesZone;
        trigger.origin = None;
        trigger.destination = Some(Zone::Battlefield);
        trigger.execute = Some(Box::new(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChooseAugmentAndCombineWithHost {
                    zones: vec![Zone::Graveyard, Zone::Library],
                    filter: Box::new(augment_card_filter()),
                    host: Box::new(TargetFilter::TriggeringSource),
                },
            )
            .optional()
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Shuffle {
                    target: TargetFilter::Controller,
                },
            )),
        ));
    }
}

fn rewrite_strutting_turkey(face: &mut CardFace) {
    face.abilities.clear();
    for trigger in &mut face.triggers {
        if trigger.mode != TriggerMode::ChangesZone
            || trigger.destination != Some(Zone::Battlefield)
            || trigger.valid_card != Some(TargetFilter::SelfRef)
        {
            continue;
        }
        trigger.execute = Some(Box::new(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Exile,
                    target: TypedFilter::new(TypeFilter::Creature)
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::InZone {
                            zone: Zone::Graveyard,
                        }])
                        .into(),
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: Vec::new(),
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                },
            )
            .sub_ability(
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::CombineHost {
                        source: CombineSource::ParentTarget,
                        host: Box::new(controlled_host_filter()),
                    },
                )
                .condition(AbilityCondition::TargetMatchesFilter {
                    filter: augment_card_filter(),
                    use_lki: false,
                })
                .with_else_ability(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        origin: Some(Zone::Exile),
                        destination: Zone::Battlefield,
                        target: TargetFilter::ParentTarget,
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: EtbTapState::Unspecified,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: Vec::new(),
                        conditional_enter_with_counters: vec![],
                        face_down_profile: None,
                    },
                )),
            ),
        ));
        break;
    }
}

fn augment_cost(face: &CardFace, oracle: &str) -> Option<ManaCost> {
    face.abilities
        .iter()
        .filter_map(|ability| ability.description.as_deref())
        .find_map(|description| {
            description
                .strip_prefix("Augment ")
                .map(|rest| parse_mtgjson_mana_cost(rest.trim()))
        })
        .or_else(|| {
            oracle.lines().find_map(|line| {
                let cost_text = line.trim().strip_prefix("Augment ")?;
                let cost_text = cost_text.split_whitespace().next()?;
                Some(parse_mtgjson_mana_cost(cost_text))
            })
        })
}

fn augment_card_filter() -> TargetFilter {
    TypedFilter::card()
        .properties(vec![FilterProp::HasKeywordKind {
            value: KeywordKind::Augment,
        }])
        .into()
}

fn host_filter() -> TargetFilter {
    TypedFilter::creature()
        .properties(vec![FilterProp::HasSupertype {
            value: Supertype::Host,
        }])
        .into()
}

fn controlled_host_filter() -> TargetFilter {
    TypedFilter::creature()
        .controller(ControllerRef::You)
        .properties(vec![FilterProp::HasSupertype {
            value: Supertype::Host,
        }])
        .into()
}
