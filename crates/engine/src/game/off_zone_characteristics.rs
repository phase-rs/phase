use crate::game::filter::matches_target_filter_controlled;
use crate::game::layers::{
    active_continuous_effects_from_base_static_source, collect_shared_active_continuous_effects,
    order_active_continuous_effects,
};
use crate::game::quantity::resolve_quantity;
use crate::types::ability::ContinuousModification;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::layers::{ActiveContinuousEffect, Layer};
use crate::types::zones::Zone;

pub fn effective_off_zone_keywords(state: &GameState, object_id: ObjectId) -> Vec<Keyword> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };
    if obj.zone == Zone::Battlefield {
        return obj.keywords.clone();
    }

    let mut keywords = obj.base_keywords.clone();
    let effects = collect_applicable_off_zone_keyword_effects(state, object_id);
    let ordered = order_active_continuous_effects(Layer::Ability, &effects, state);

    for effect in ordered {
        apply_keyword_modification(state, &mut keywords, &effect);
    }

    keywords
}

pub fn effective_off_zone_keyword(
    state: &GameState,
    object_id: ObjectId,
    kind: crate::types::keywords::KeywordKind,
) -> Option<Keyword> {
    effective_off_zone_keywords(state, object_id)
        .into_iter()
        .find(|keyword| keyword.kind() == kind)
}

pub fn off_zone_has_keyword_kind(
    state: &GameState,
    object_id: ObjectId,
    kind: crate::types::keywords::KeywordKind,
) -> bool {
    effective_off_zone_keyword(state, object_id, kind).is_some()
}

fn collect_applicable_off_zone_keyword_effects(
    state: &GameState,
    object_id: ObjectId,
) -> Vec<ActiveContinuousEffect> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };

    let mut effects = collect_shared_active_continuous_effects(state);
    if obj.zone != Zone::Battlefield && !(obj.zone == Zone::Command && obj.is_emblem) {
        effects.extend(active_continuous_effects_from_base_static_source(
            state, obj,
        ));
    }

    effects
        .into_iter()
        .filter(|effect| {
            effect.layer == Layer::Ability
                && supports_off_zone_keyword_query(&effect.modification)
                && matches_target_filter_controlled(
                    state,
                    object_id,
                    &effect.affected_filter,
                    effect.source_id,
                    effect.controller,
                )
        })
        .collect()
}

fn supports_off_zone_keyword_query(modification: &ContinuousModification) -> bool {
    matches!(
        modification,
        ContinuousModification::AddKeyword { .. }
            | ContinuousModification::RemoveKeyword { .. }
            | ContinuousModification::AddDynamicKeyword { .. }
            | ContinuousModification::RemoveAllAbilities
    )
}

fn apply_keyword_modification(
    state: &GameState,
    keywords: &mut Vec<Keyword>,
    effect: &ActiveContinuousEffect,
) {
    match &effect.modification {
        ContinuousModification::AddKeyword { keyword } => upsert_keyword(keywords, keyword.clone()),
        ContinuousModification::RemoveKeyword { keyword } => {
            keywords.retain(|existing| {
                std::mem::discriminant(existing) != std::mem::discriminant(keyword)
            });
        }
        ContinuousModification::AddDynamicKeyword { kind, value } => {
            let dynamic_value = resolve_quantity(state, value, effect.controller, effect.source_id);
            let keyword = kind.with_value(dynamic_value.max(0) as u32);
            upsert_keyword(keywords, keyword);
        }
        ContinuousModification::RemoveAllAbilities => keywords.clear(),
        _ => {}
    }
}

fn upsert_keyword(keywords: &mut Vec<Keyword>, keyword: Keyword) {
    if let Some(existing) = keywords
        .iter_mut()
        .find(|existing| existing.kind() == keyword.kind())
    {
        *existing = keyword;
        return;
    }

    keywords.push(keyword);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ContinuousModification, Duration, QuantityExpr, StaticDefinition, TargetFilter,
    };
    use crate::types::identifiers::CardId;
    use crate::types::keywords::{DynamicKeywordKind, Keyword, KeywordKind};
    use crate::types::mana::{ManaCost, ManaCostShard};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn create_card(state: &mut GameState, owner: PlayerId, name: &str, zone: Zone) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let timestamp = state.next_timestamp();
        let object_id = create_object(state, card_id, owner, name.to_string(), zone);
        if let Some(obj) = state.objects.get_mut(&object_id) {
            obj.timestamp = timestamp;
        }
        object_id
    }

    #[test]
    fn printed_graveyard_keyword_is_returned_unchanged() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_card(
            &mut state,
            PlayerId(0),
            "Faithless Looting",
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&card_id)
            .unwrap()
            .base_keywords
            .push(Keyword::Flashback(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Red],
            }));
        let base_keywords = state.objects.get(&card_id).unwrap().base_keywords.clone();
        state.objects.get_mut(&card_id).unwrap().keywords = base_keywords;

        let keywords = effective_off_zone_keywords(&state, card_id);
        assert_eq!(keywords.len(), 1);
        assert_eq!(
            keywords[0],
            Keyword::Flashback(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Red],
            })
        );
    }

    #[test]
    fn transient_add_keyword_applies_to_graveyard_card() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_card(
            &mut state,
            PlayerId(0),
            "Snapcaster Mage",
            Zone::Battlefield,
        );
        let target_id = create_card(&mut state, PlayerId(0), "Opt", Zone::Graveyard);

        state.add_transient_continuous_effect(
            source_id,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: target_id },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flashback(ManaCost::SelfManaCost),
            }],
            None,
        );

        assert_eq!(
            effective_off_zone_keyword(&state, target_id, KeywordKind::Flashback),
            Some(Keyword::Flashback(ManaCost::SelfManaCost))
        );
    }

    #[test]
    fn battlefield_static_grants_keyword_to_graveyard_card() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_card(&mut state, PlayerId(0), "Lier", Zone::Battlefield);
        let target_id = create_card(&mut state, PlayerId(0), "Consider", Zone::Graveyard);

        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Flashback(ManaCost::SelfManaCost),
                    }]),
            );

        assert!(off_zone_has_keyword_kind(
            &state,
            target_id,
            KeywordKind::Flashback
        ));
    }

    #[test]
    fn self_static_in_graveyard_grants_keyword_to_self() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_card(&mut state, PlayerId(0), "Viral Spawning", Zone::Graveyard);

        state
            .objects
            .get_mut(&card_id)
            .unwrap()
            .base_static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Flashback(ManaCost::Cost {
                            generic: 2,
                            shards: vec![ManaCostShard::Green],
                        }),
                    }]),
            );
        let base_static_definitions = state
            .objects
            .get(&card_id)
            .unwrap()
            .base_static_definitions
            .clone();
        state.objects.get_mut(&card_id).unwrap().static_definitions = base_static_definitions;

        assert_eq!(
            effective_off_zone_keyword(&state, card_id, KeywordKind::Flashback),
            Some(Keyword::Flashback(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Green],
            }))
        );
    }

    #[test]
    fn command_zone_emblem_grants_keyword_to_non_battlefield_card() {
        let mut state = GameState::new_two_player(42);
        let emblem_id = create_card(&mut state, PlayerId(0), "Emblem", Zone::Command);
        let target_id = create_card(&mut state, PlayerId(0), "Think Twice", Zone::Exile);

        {
            let emblem = state.objects.get_mut(&emblem_id).unwrap();
            emblem.is_emblem = true;
            emblem.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Flashback(ManaCost::SelfManaCost),
                    }]),
            );
        }

        assert!(off_zone_has_keyword_kind(
            &state,
            target_id,
            KeywordKind::Flashback
        ));
    }

    #[test]
    fn remove_keyword_suppresses_matching_keyword_kind() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_card(
            &mut state,
            PlayerId(0),
            "Faithless Looting",
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&target_id)
            .unwrap()
            .base_keywords
            .push(Keyword::Flashback(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Red],
            }));
        let base_keywords = state.objects.get(&target_id).unwrap().base_keywords.clone();
        state.objects.get_mut(&target_id).unwrap().keywords = base_keywords;

        let source_id = create_card(&mut state, PlayerId(0), "Source", Zone::Battlefield);
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::RemoveKeyword {
                        keyword: Keyword::Flashback(ManaCost::Cost {
                            generic: 2,
                            shards: vec![ManaCostShard::Red],
                        }),
                    }]),
            );

        assert!(!off_zone_has_keyword_kind(
            &state,
            target_id,
            KeywordKind::Flashback
        ));
    }

    #[test]
    fn off_zone_queries_ignore_non_base_keyword_residue() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_card(&mut state, PlayerId(0), "Creature", Zone::Graveyard);
        state
            .objects
            .get_mut(&card_id)
            .unwrap()
            .keywords
            .push(Keyword::Flying);

        assert!(effective_off_zone_keywords(&state, card_id).is_empty());
    }

    #[test]
    fn off_zone_self_statics_use_base_static_definitions() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_card(&mut state, PlayerId(0), "Viral Spawning", Zone::Graveyard);
        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flashback(ManaCost::Cost {
                    generic: 2,
                    shards: vec![ManaCostShard::Green],
                }),
            }]);
        state
            .objects
            .get_mut(&card_id)
            .unwrap()
            .base_static_definitions = vec![static_def];
        state
            .objects
            .get_mut(&card_id)
            .unwrap()
            .static_definitions
            .clear();

        assert_eq!(
            effective_off_zone_keyword(&state, card_id, KeywordKind::Flashback),
            Some(Keyword::Flashback(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Green],
            }))
        );
    }

    #[test]
    fn remove_all_abilities_clears_keywords_for_query() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_card(
            &mut state,
            PlayerId(0),
            "Faithless Looting",
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&target_id)
            .unwrap()
            .base_keywords
            .push(Keyword::Flashback(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Red],
            }));
        let base_keywords = state.objects.get(&target_id).unwrap().base_keywords.clone();
        state.objects.get_mut(&target_id).unwrap().keywords = base_keywords;

        let source_id = create_card(&mut state, PlayerId(0), "Source", Zone::Battlefield);
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::RemoveAllAbilities]),
            );

        assert!(effective_off_zone_keywords(&state, target_id).is_empty());
    }

    #[test]
    fn add_dynamic_keyword_uses_quantity_resolution() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_card(&mut state, PlayerId(0), "Source", Zone::Battlefield);
        let target_id = create_card(&mut state, PlayerId(0), "Arcbound Ravager", Zone::Graveyard);

        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddDynamicKeyword {
                        kind: DynamicKeywordKind::Modular,
                        value: QuantityExpr::Fixed { value: 3 },
                    }]),
            );

        assert_eq!(
            effective_off_zone_keyword(&state, target_id, KeywordKind::Modular),
            Some(Keyword::Modular(3))
        );
    }

    #[test]
    fn later_effect_replaces_same_keyword_kind_payload() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_card(&mut state, PlayerId(0), "Think Twice", Zone::Graveyard);
        let earlier_id = create_card(&mut state, PlayerId(0), "Earlier", Zone::Battlefield);
        let later_id = create_card(&mut state, PlayerId(0), "Later", Zone::Battlefield);

        state
            .objects
            .get_mut(&earlier_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Flashback(ManaCost::Cost {
                            generic: 1,
                            shards: vec![ManaCostShard::Blue],
                        }),
                    }]),
            );
        state
            .objects
            .get_mut(&later_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Flashback(ManaCost::Cost {
                            generic: 2,
                            shards: vec![ManaCostShard::Blue],
                        }),
                    }]),
            );

        assert_eq!(
            effective_off_zone_keyword(&state, target_id, KeywordKind::Flashback),
            Some(Keyword::Flashback(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Blue],
            }))
        );
    }
}
