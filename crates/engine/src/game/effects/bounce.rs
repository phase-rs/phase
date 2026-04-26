use crate::game::zones;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef, TypedFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::zones::Zone;

/// CR 400.6: Zone change — permanent moves from battlefield to its owner's hand.
///
/// Also handles LTB self-return triggers (CR 603.10) such as Rancor: when the
/// trigger resolves, the source is already in its owner's graveyard, so the
/// resolver must accept graveyard as a valid from-zone in addition to the
/// battlefield.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Determine targets using typed Effect::Bounce target field.
    // CR 608.2c + 603.10: An anaphoric "it" in a top-level trigger effect (e.g.,
    // Rancor's "return it to its owner's hand") has no parent target to inherit
    // from — it refers to the source object itself. `SelfRef` collapses to the
    // same thing. `TriggeringSource` is deliberately excluded: it resolves via
    // `state.current_trigger_event`, and conflating it with the ability source
    // would be wrong for "Whenever a creature dies, return it ..." patterns
    // where the source is the ability's host, not the triggering object.
    let use_self = match &ability.effect {
        Effect::Bounce { target, .. } => {
            matches!(
                target,
                TargetFilter::None | TargetFilter::SelfRef | TargetFilter::ParentTarget
            ) && ability.targets.is_empty()
        }
        _ => false,
    };

    let targets: Vec<_> = if use_self {
        vec![ability.source_id]
    } else {
        ability
            .targets
            .iter()
            .filter_map(|t| {
                if let TargetRef::Object(id) = t {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect()
    };

    for obj_id in targets {
        // CR 114.5: Emblems cannot be bounced
        if state.objects.get(&obj_id).is_some_and(|o| o.is_emblem) {
            continue;
        }

        // CR 400.3 + CR 603.10: Bounce moves the object from its current zone to
        // its owner's hand. Battlefield is the usual case; graveyard covers LTB
        // self-return triggers (Rancor class) where the source has already moved
        // to the graveyard by the time the trigger resolves.
        let current_zone = state.objects.get(&obj_id).map(|o| o.zone);
        if matches!(current_zone, Some(Zone::Battlefield | Zone::Graveyard)) {
            zones::move_to_zone(state, obj_id, Zone::Hand, events);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 400.7 + CR 611.2c: Mass-bounce — return every battlefield permanent
/// matching the filter to its owner's hand (default) or to the destination
/// zone if `Effect::BounceAll.destination` is set.
///
/// Mirrors `destroy::resolve_all` in shape: collect matching object IDs from
/// the battlefield via `crate::game::filter::matches_target_filter`, then
/// move each to the destination zone with `zones::move_to_zone`.
///
/// CR 114.5: Emblems are not on the battlefield (they live in the command
/// zone), so the battlefield scan naturally excludes them — no extra guard
/// needed beyond the existing filter pipeline.
pub fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (target_filter, destination) = match &ability.effect {
        Effect::BounceAll {
            target,
            destination,
        } => (target.clone(), destination.unwrap_or(Zone::Hand)),
        _ => (TargetFilter::None, Zone::Hand),
    };

    // CR 701.3 + CR 611.2c: A `TargetFilter::None` lands here when the parser
    // didn't supply a class filter. Default to "all creatures" — the
    // historically dominant mass-bounce shape — to match `destroy::resolve_all`.
    let effective_filter = if matches!(target_filter, TargetFilter::None) {
        TargetFilter::Typed(TypedFilter {
            type_filters: vec![crate::types::ability::TypeFilter::Creature],
            controller: None,
            properties: vec![],
        })
    } else {
        crate::game::effects::resolved_object_filter(ability, &target_filter)
    };

    // CR 107.3a + CR 601.2b: Filter evaluation runs in the ability's
    // resolution context (controller, target slots already filled).
    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    let matching: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| {
            crate::game::filter::matches_target_filter(state, **id, &effective_filter, &ctx)
        })
        .copied()
        .collect();

    for obj_id in matching {
        // CR 400.3 + CR 400.7: Move each matching permanent to the
        // destination zone. The single-bounce resolver runs the same
        // `zones::move_to_zone` primitive — no replacement-pipeline detour
        // is needed because mass-bounce events are not destruction events
        // (CR 614.6 doesn't apply here).
        let current_zone = state.objects.get(&obj_id).map(|o| o.zone);
        if current_zone == Some(Zone::Battlefield) {
            zones::move_to_zone(state, obj_id, destination, events);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    #[test]
    fn test_bounce_moves_permanent_to_hand() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[1].hand.contains(&obj_id));
    }

    #[test]
    fn test_bounce_self() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Ninja".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::None,
                destination: None,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[0].hand.contains(&obj_id));
    }

    #[test]
    fn test_bounce_emits_zone_changed() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::ZoneChanged {
                from: Some(Zone::Battlefield),
                to: Zone::Hand,
                ..
            }
        )));
    }

    /// CR 603.10 / Rancor class: LTB self-return triggers fire after the source
    /// has moved to the graveyard. The parsed effect is
    /// `Bounce { target: ParentTarget }` with empty `ability.targets`; the
    /// resolver must treat that as "return the source object from the graveyard
    /// to its owner's hand."
    #[test]
    fn test_bounce_ltb_self_return_from_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Rancor".to_string(),
            Zone::Graveyard,
        );

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::ParentTarget,
                destination: None,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.players[0].graveyard.contains(&obj_id));
        assert!(state.players[0].hand.contains(&obj_id));
    }

    #[test]
    fn test_bounce_ltb_self_ref_from_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Spirit Loop".to_string(),
            Zone::Graveyard,
        );

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::SelfRef,
                destination: None,
            },
            vec![],
            obj_id,
            PlayerId(1),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.players[1].graveyard.contains(&obj_id));
        assert!(state.players[1].hand.contains(&obj_id));
    }

    /// End-to-end Rancor-class pipeline test: battlefield → graveyard emits
    /// `ZoneChanged`, `process_triggers` picks up the graveyard-zone trigger,
    /// the triggered ability resolves, and the Aura ends up in its owner's hand.
    #[test]
    fn test_rancor_ltb_pipeline_returns_to_owner_hand() {
        use crate::game::stack::resolve_top;
        use crate::game::triggers::process_triggers;
        use crate::types::ability::{AbilityDefinition, AbilityKind, TriggerDefinition};
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        let rancor_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Rancor".to_string(),
            Zone::Battlefield,
        );

        // Mirror the shape emitted by the parser for Rancor's LTB trigger.
        let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
        trigger.origin = Some(Zone::Battlefield);
        trigger.destination = Some(Zone::Graveyard);
        trigger.valid_card = Some(TargetFilter::SelfRef);
        trigger.trigger_zones = vec![Zone::Graveyard];
        trigger.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Bounce {
                target: TargetFilter::ParentTarget,
                destination: None,
            },
        )));
        state
            .objects
            .get_mut(&rancor_id)
            .unwrap()
            .trigger_definitions
            .push(trigger);

        // Destroy Rancor (move battlefield → graveyard), then run the trigger pipeline.
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, rancor_id, Zone::Graveyard, &mut events);
        assert!(state.players[0].graveyard.contains(&rancor_id));

        process_triggers(&mut state, &events);
        assert_eq!(
            state.stack.len(),
            1,
            "Rancor LTB trigger did not reach stack"
        );

        // Resolve the triggered ability and confirm Rancor landed in its owner's hand.
        let mut resolve_events = Vec::new();
        resolve_top(&mut state, &mut resolve_events);
        assert!(
            state.players[0].hand.contains(&rancor_id),
            "Rancor should return to owner's hand; actual zones: hand={:?} graveyard={:?}",
            state.players[0].hand,
            state.players[0].graveyard
        );
        assert!(!state.players[0].graveyard.contains(&rancor_id));
    }

    /// CR 400.7 + CR 611.2c: Mass-bounce iterates every battlefield permanent
    /// matching the filter. Mixed match/no-match population: only matching
    /// permanents move to their owners' hands; non-matching permanents stay.
    #[test]
    fn test_bounce_all_creatures_filters_non_creatures() {
        use crate::types::ability::TypeFilter;

        let mut state = GameState::new_two_player(42);
        // Three creatures (P0 owns 2, P1 owns 1) and one artifact owned by P0.
        let bear = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bear".to_string(),
            Zone::Battlefield,
        );
        let dragon = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Shivan Dragon".to_string(),
            Zone::Battlefield,
        );
        let elf = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        let totem = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Pithing Needle".to_string(),
            Zone::Battlefield,
        );
        // Stamp creature/artifact card_types onto each object so the filter
        // evaluator can classify them.
        for (id, core_types) in [
            (bear, vec![CoreType::Creature]),
            (dragon, vec![CoreType::Creature]),
            (elf, vec![CoreType::Creature]),
            (totem, vec![CoreType::Artifact]),
        ] {
            let obj = state.objects.get_mut(&id).unwrap();
            let card_type = crate::types::card_type::CardType {
                core_types,
                ..Default::default()
            };
            obj.card_types = card_type.clone();
            obj.base_card_types = card_type;
        }

        let creature_filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: None,
            properties: vec![],
        });
        let ability = ResolvedAbility::new(
            Effect::BounceAll {
                target: creature_filter,
                destination: None,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        // All three creatures move to their respective owners' hands.
        assert!(!state.battlefield.contains(&bear), "bear left battlefield");
        assert!(
            !state.battlefield.contains(&dragon),
            "dragon left battlefield"
        );
        assert!(!state.battlefield.contains(&elf), "elf left battlefield");
        assert!(state.players[0].hand.contains(&bear));
        assert!(state.players[0].hand.contains(&dragon));
        assert!(state.players[1].hand.contains(&elf));

        // The artifact stays on the battlefield (filter mismatch).
        assert!(
            state.battlefield.contains(&totem),
            "non-creature totem stays on battlefield"
        );
        assert!(!state.players[0].hand.contains(&totem));
    }

    /// CR 400.7: Destination override threads through `Effect::BounceAll`.
    /// `Some(Zone::Library)` covers hypothetical top-of-library mass-return
    /// patterns (no current corpus card, but the type-system shape mirrors
    /// `Effect::Bounce.destination`).
    #[test]
    fn test_bounce_all_destination_library() {
        use crate::types::ability::TypeFilter;

        let mut state = GameState::new_two_player(42);
        let bear = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&bear).unwrap();
            let card_type = crate::types::card_type::CardType {
                core_types: vec![CoreType::Creature],
                ..Default::default()
            };
            obj.card_types = card_type.clone();
            obj.base_card_types = card_type;
        }

        let ability = ResolvedAbility::new(
            Effect::BounceAll {
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: None,
                    properties: vec![],
                }),
                destination: Some(Zone::Library),
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&bear));
        assert!(
            state.players[0].library.contains(&bear),
            "bear moved to library when destination override is set"
        );
        assert!(!state.players[0].hand.contains(&bear));
    }
}
