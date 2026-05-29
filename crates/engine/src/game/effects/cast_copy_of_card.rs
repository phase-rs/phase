use crate::game::ability_utils::build_resolved_from_def;
use crate::game::effects::prepare::open_copy_target_selection;
use crate::game::filter::{matches_target_filter, FilterContext};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{CastingVariant, GameState, StackEntry, StackEntryKind, WaitingFor};
use crate::types::identifiers::{ObjectId, TrackedSetId};
use crate::types::zones::Zone;

/// CR 707.12: Cast a copy of a card/object. The copy is created from the
/// source object's copiable values and put onto the stack as part of casting.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (target_filter, cost) = match &ability.effect {
        Effect::CastCopyOfCard { target, cost } => (target, cost),
        _ => return Err(EffectError::MissingParam("CastCopyOfCard".to_string())),
    };

    let mut source_ids: Vec<ObjectId> = ability
        .targets
        .iter()
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();

    if source_ids.is_empty() && references_tracked_set(target_filter) {
        let ctx = FilterContext::from_ability(ability);
        // CR 608.2c: Resolve the tracked-set sentinel from the resolving effect's
        // last known context before collecting the affected objects.
        let effective_filter =
            crate::game::targeting::resolve_tracked_set_sentinel(state, target_filter.clone());
        let tracked_set_id = tracked_set_id_from_filter(&effective_filter)
            .or_else(|| crate::game::targeting::latest_tracked_set_id(state));
        source_ids = tracked_set_id
            .and_then(|id| state.tracked_object_sets.get(&id).cloned())
            .unwrap_or_default()
            .into_iter()
            .filter(|id| matches_target_filter(state, *id, &effective_filter, &ctx))
            .collect();

        if !source_ids.is_empty() {
            let count = source_ids.len();
            let mut resume = ability.clone();
            resume.effect = Effect::CastCopyOfCard {
                target: TargetFilter::None,
                cost: cost.clone(),
            };
            resume.sub_ability = None;
            super::append_to_pending_continuation(state, Some(Box::new(resume)));
            state.waiting_for = WaitingFor::ChooseFromZoneChoice {
                player: ability.controller,
                cards: source_ids,
                count,
                up_to: true,
                constraint: None,
                source_id: ability.source_id,
            };
            return Ok(());
        }
    }

    if source_ids.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::CastCopyOfCard,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    for (index, source_id) in source_ids.iter().copied().enumerate() {
        let copy_id =
            cast_one_copy(state, source_id, ability, events).map_err(EffectError::InvalidParam)?;

        if open_copy_target_selection(state, copy_id, ability.controller)
            .map_err(EffectError::InvalidParam)?
        {
            let mut resume = ability.clone();
            resume.effect = Effect::CastCopyOfCard {
                target: TargetFilter::None,
                cost: cost.clone(),
            };
            resume.sub_ability = None;
            if index + 1 < source_ids.len() {
                resume.targets = source_ids[index + 1..]
                    .iter()
                    .copied()
                    .map(TargetRef::Object)
                    .collect();
            } else {
                resume.targets.clear();
            }
            super::append_to_pending_continuation(state, Some(Box::new(resume)));
            return Ok(());
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::CastCopyOfCard,
        source_id: ability.source_id,
    });
    Ok(())
}

fn references_tracked_set(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. } => true,
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(references_tracked_set)
        }
        TargetFilter::Not { filter } => references_tracked_set(filter),
        _ => false,
    }
}

fn tracked_set_id_from_filter(filter: &TargetFilter) -> Option<TrackedSetId> {
    match filter {
        TargetFilter::TrackedSet { id } | TargetFilter::TrackedSetFiltered { id, .. } => Some(*id),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().find_map(tracked_set_id_from_filter)
        }
        TargetFilter::Not { filter } => tracked_set_id_from_filter(filter),
        _ => None,
    }
}

fn cast_one_copy(
    state: &mut GameState,
    source_id: ObjectId,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<ObjectId, String> {
    let (source, card_id, origin_zone) = {
        let Some(source) = state.objects.get(&source_id) else {
            return Err(format!("copy source {source_id:?} not found"));
        };
        (source.clone(), source.card_id, source.zone)
    };

    let copy_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;

    let ability_def = spell_ability_definition(&source.abilities);
    let mut copy = source;
    copy.id = copy_id;
    copy.controller = ability.controller;
    copy.owner = ability.controller;
    // CR 707.12 + CR 601.2a: The copy is created and cast as a spell on the stack.
    copy.zone = Zone::Stack;
    copy.is_token = false;
    copy.tapped = false;
    copy.prepared = None;
    // CR 707.12: The copy is created in the same zone as the source object before casting.
    copy.cast_from_zone = Some(origin_zone);
    copy.cost_x_paid = None;
    copy.kickers_paid.clear();
    copy.additional_cost_payment_count = 0;
    state.objects.insert(copy_id, copy);

    let mut resolved =
        ability_def.map(|def| build_resolved_from_def(&def, copy_id, ability.controller));
    if let Some(resolved) = resolved.as_mut() {
        resolved.context.cast_from_zone = Some(origin_zone);
    }

    state.stack.push_back(StackEntry {
        id: copy_id,
        source_id: copy_id,
        controller: ability.controller,
        kind: StackEntryKind::Spell {
            card_id,
            ability: resolved,
            casting_variant: CastingVariant::Normal,
            // CR 118.9 + CR 601.2f: "Without paying its mana cost" is an alternative cost.
            actual_mana_spent: 0,
        },
    });
    events.push(GameEvent::StackPushed { object_id: copy_id });
    events.push(GameEvent::SpellCast {
        card_id,
        controller: ability.controller,
        object_id: copy_id,
    });
    if let Some(obj) = state.objects.get(&copy_id).cloned() {
        crate::game::restrictions::record_spell_cast_from_zone(
            state,
            ability.controller,
            &obj,
            origin_zone,
            CastingVariant::Normal,
        );
    }

    Ok(copy_id)
}

fn spell_ability_definition(abilities: &[AbilityDefinition]) -> Option<AbilityDefinition> {
    abilities
        .iter()
        .find(|ability| ability.kind == AbilityKind::Spell)
        .or_else(|| abilities.first())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::game::zones::create_object;
    use crate::types::ability::{AbilityDefinition, AbilityKind};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, TrackedSetId};
    use crate::types::mana::ManaCost;
    use crate::types::player::PlayerId;

    fn add_exiled_spell_card(state: &mut GameState, name: &str) -> ObjectId {
        let owner = PlayerId(0);
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Exile,
        );
        let obj = state.objects.get_mut(&id).expect("created object exists");
        obj.card_types.core_types.push(CoreType::Instant);
        obj.abilities = Arc::new(vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Unimplemented {
                name: "test spell".to_string(),
                description: None,
            },
        )]);
        id
    }

    #[test]
    fn explicit_target_casts_a_stack_copy_without_moving_source_card() {
        let mut state = GameState::new_two_player(7);
        let source_id = add_exiled_spell_card(&mut state, "Lightning Bolt");
        let source_card_id = state
            .objects
            .get(&source_id)
            .expect("source exists")
            .card_id;
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CastCopyOfCard {
                target: TargetFilter::None,
                cost: ManaCost::zero(),
            },
            vec![TargetRef::Object(source_id)],
            ObjectId(99),
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).expect("cast copy resolves");

        assert_eq!(
            state.objects.get(&source_id).expect("source exists").zone,
            Zone::Exile
        );
        assert_eq!(state.stack.len(), 1);
        let copy_id = state.stack.back().expect("copy on stack").id;
        let copy = state.objects.get(&copy_id).expect("copy object exists");
        assert_eq!(copy.zone, Zone::Stack);
        assert!(!copy.is_token);
        assert_eq!(copy.owner, PlayerId(0));
        assert_eq!(copy.controller, PlayerId(0));
        assert!(matches!(
            state.stack.back().expect("copy on stack").kind,
            StackEntryKind::Spell { card_id, .. } if card_id == source_card_id
        ));
        assert!(events.iter().any(|event| {
            matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == copy_id)
        }));
    }

    #[test]
    fn tracked_set_opens_up_to_choice_for_copies_to_cast() {
        let mut state = GameState::new_two_player(7);
        let first = add_exiled_spell_card(&mut state, "Opt");
        let second = add_exiled_spell_card(&mut state, "Consider");
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![first, second]);
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CastCopyOfCard {
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                cost: ManaCost::zero(),
            },
            Vec::new(),
            ObjectId(99),
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).expect("choice opens");

        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice {
                player,
                cards,
                count,
                up_to,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 2);
                assert!(*up_to);
                assert_eq!(cards, &vec![first, second]);
            }
            other => panic!("expected ChooseFromZoneChoice, got {other:?}"),
        }
        assert!(state.pending_continuation.is_some());
        assert!(events.is_empty());
    }

    #[test]
    fn tracked_set_choice_uses_resolved_filter_id_not_latest_set() {
        let mut state = GameState::new_two_player(7);
        let first = add_exiled_spell_card(&mut state, "Opt");
        let latest = add_exiled_spell_card(&mut state, "Consider");
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![first]);
        state
            .tracked_object_sets
            .insert(TrackedSetId(2), vec![latest]);
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CastCopyOfCard {
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(1),
                },
                cost: ManaCost::zero(),
            },
            Vec::new(),
            ObjectId(99),
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).expect("choice opens");

        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                assert_eq!(cards, &vec![first]);
            }
            other => panic!("expected ChooseFromZoneChoice, got {other:?}"),
        }
        assert!(events.is_empty());
    }

    #[test]
    fn cast_copy_uses_spell_ability_when_non_spell_ability_is_first() {
        let mut state = GameState::new_two_player(7);
        let source_id = add_exiled_spell_card(&mut state, "Lightning Bolt");
        let source = state
            .objects
            .get_mut(&source_id)
            .expect("source object exists");
        source.abilities = Arc::new(vec![
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Unimplemented {
                    name: "activated ability".to_string(),
                    description: None,
                },
            ),
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "spell ability".to_string(),
                    description: None,
                },
            ),
        ]);
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CastCopyOfCard {
                target: TargetFilter::None,
                cost: ManaCost::zero(),
            },
            vec![TargetRef::Object(source_id)],
            ObjectId(99),
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).expect("cast copy resolves");

        let entry = state.stack.back().expect("copy on stack");
        match &entry.kind {
            StackEntryKind::Spell {
                ability: Some(resolved),
                ..
            } => assert!(matches!(
                resolved.effect,
                Effect::Unimplemented { ref name, .. } if name == "spell ability"
            )),
            other => panic!("expected spell with resolved ability, got {other:?}"),
        }
    }
}
