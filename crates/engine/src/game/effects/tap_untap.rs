use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::proposed_event::ProposedEvent;

/// CR 701.26a: Tap — turn a permanent sideways. CR 701.26b: Untap — return to upright.
pub fn resolve_tap(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 701.26a: `Effect::Tap { target: SelfRef }` with no collected targets
    // means tap the source itself (the "tap ~" idiom). SelfRef is a
    // context-ref — not surfaced as a target slot and not auto-resolved by the
    // event-context resolver — so this resolver must expand it. Mirrors the
    // pump resolver's SelfRef shortcut.
    let self_targets;
    let targets: &[TargetRef] = if ability.targets.is_empty()
        && matches!(
            &ability.effect,
            Effect::Tap {
                target: TargetFilter::SelfRef
            }
        ) {
        self_targets = [TargetRef::Object(ability.source_id)];
        &self_targets
    } else {
        &ability.targets
    };
    for target in targets {
        if let TargetRef::Object(obj_id) = target {
            let proposed = ProposedEvent::Tap {
                object_id: *obj_id,
                applied: HashSet::new(),
            };

            match replacement::replace_event(state, proposed, events) {
                ReplacementResult::Execute(event) => {
                    if let ProposedEvent::Tap { object_id, .. } = event {
                        let obj = state
                            .objects
                            .get_mut(&object_id)
                            .ok_or(EffectError::ObjectNotFound(object_id))?;
                        obj.tapped = true;
                        events.push(GameEvent::PermanentTapped {
                            object_id,
                            caused_by: Some(ability.source_id),
                        });
                    }
                }
                ReplacementResult::Prevented => {}
                ReplacementResult::NeedsChoice(player) => {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    return Ok(());
                }
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 701.26b: Untap target permanents — rotate back to upright position.
pub fn resolve_untap(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 701.26b: `Effect::Untap { target: SelfRef }` with no collected targets
    // means untap the source itself (the "untap ~" idiom). SelfRef is a
    // context-ref — not surfaced as a target slot and not auto-resolved by the
    // event-context resolver — so this resolver must expand it. This is the
    // runtime path for trigger shapes like Ragost's "At the beginning of each
    // end step, if you gained life this turn, untap ~" (CR 603.4
    // intervening-if + CR 514 end step).
    let self_targets;
    let targets: &[TargetRef] = if ability.targets.is_empty()
        && matches!(
            &ability.effect,
            Effect::Untap {
                target: TargetFilter::SelfRef
            }
        ) {
        self_targets = [TargetRef::Object(ability.source_id)];
        &self_targets
    } else {
        &ability.targets
    };
    for target in targets {
        if let TargetRef::Object(obj_id) = target {
            let proposed = ProposedEvent::Untap {
                object_id: *obj_id,
                applied: HashSet::new(),
            };

            match replacement::replace_event(state, proposed, events) {
                ReplacementResult::Execute(event) => {
                    if let ProposedEvent::Untap { object_id, .. } = event {
                        let obj = state
                            .objects
                            .get_mut(&object_id)
                            .ok_or(EffectError::ObjectNotFound(object_id))?;
                        obj.tapped = false;
                        events.push(GameEvent::PermanentUntapped { object_id });
                    }
                }
                ReplacementResult::Prevented => {}
                ReplacementResult::NeedsChoice(player) => {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    return Ok(());
                }
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 701.26a: Tap all permanents matching the filter.
pub fn resolve_tap_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target_filter = match &ability.effect {
        Effect::TapAll { target } => target.clone(),
        _ => TargetFilter::Any,
    };

    let effective_filter = crate::game::effects::resolved_object_filter(ability, &target_filter);

    // CR 107.3a + CR 601.2b: ability-context filter evaluation.
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
        let proposed = ProposedEvent::Tap {
            object_id: obj_id,
            applied: HashSet::new(),
        };

        match replacement::replace_event(state, proposed, events) {
            ReplacementResult::Execute(event) => {
                if let ProposedEvent::Tap { object_id, .. } = event {
                    if let Some(obj) = state.objects.get_mut(&object_id) {
                        obj.tapped = true;
                        events.push(GameEvent::PermanentTapped {
                            object_id,
                            caused_by: Some(ability.source_id),
                        });
                    }
                }
            }
            ReplacementResult::Prevented => {}
            ReplacementResult::NeedsChoice(player) => {
                state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
                return Ok(());
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 701.26b: Untap all permanents matching the filter.
pub fn resolve_untap_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target_filter = match &ability.effect {
        Effect::UntapAll { target } => target.clone(),
        _ => TargetFilter::Any,
    };

    let effective_filter = crate::game::effects::resolved_object_filter(ability, &target_filter);

    // CR 107.3a + CR 601.2b: ability-context filter evaluation.
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
        let proposed = ProposedEvent::Untap {
            object_id: obj_id,
            applied: HashSet::new(),
        };

        match replacement::replace_event(state, proposed, events) {
            ReplacementResult::Execute(event) => {
                if let ProposedEvent::Untap { object_id, .. } = event {
                    if let Some(obj) = state.objects.get_mut(&object_id) {
                        obj.tapped = false;
                        events.push(GameEvent::PermanentUntapped { object_id });
                    }
                }
            }
            ReplacementResult::Prevented => {}
            ReplacementResult::NeedsChoice(player) => {
                state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
                return Ok(());
            }
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
    use crate::types::ability::{Effect, TargetFilter};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_tap_ability(target: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Tap {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target)],
            ObjectId(100),
            PlayerId(0),
        )
    }

    fn make_untap_ability(target: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Untap {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target)],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn tap_sets_tapped_true() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Land".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();

        resolve_tap(&mut state, &make_tap_ability(obj_id), &mut events).unwrap();

        assert!(state.objects[&obj_id].tapped);
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentTapped { .. })));
    }

    /// CR 701.26b: When a triggered ability has
    /// `Effect::Untap { target: SelfRef }` and the source is the trigger's
    /// own object (Ragost, Famished Paladin, Pristine Angel, etc.), the
    /// resolver must untap the source even when `ability.targets` is empty.
    /// SelfRef is a context-ref (no target slot is surfaced and the
    /// event-context resolver does not bind it), so the resolver itself
    /// must expand SelfRef to the source.
    #[test]
    fn untap_self_ref_with_empty_targets_untaps_source() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Ragost".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().tapped = true;

        let ability = ResolvedAbility::new(
            Effect::Untap {
                target: TargetFilter::SelfRef,
            },
            vec![], // empty — SelfRef must resolve via source_id
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_untap(&mut state, &ability, &mut events).unwrap();

        assert!(
            !state.objects[&obj_id].tapped,
            "SelfRef untap must untap the source object"
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentUntapped { .. })));
    }

    /// CR 701.26a: Same SelfRef expansion for tap (e.g. "tap ~" triggered
    /// effects).
    #[test]
    fn tap_self_ref_with_empty_targets_taps_source() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "SomeCreature".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Tap {
                target: TargetFilter::SelfRef,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_tap(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.objects[&obj_id].tapped,
            "SelfRef tap must tap the source object"
        );
    }

    #[test]
    fn untap_sets_tapped_false() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Land".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().tapped = true;
        let mut events = Vec::new();

        resolve_untap(&mut state, &make_untap_ability(obj_id), &mut events).unwrap();

        assert!(!state.objects[&obj_id].tapped);
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentUntapped { .. })));
    }

    #[test]
    fn untap_all_nonland_permanents_you_control() {
        use crate::types::ability::{ControllerRef, TypeFilter, TypedFilter};
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);

        // 3 nonland permanents (tapped, controller P0)
        let creature1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&creature1).unwrap().tapped = true;

        let creature2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Elf".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&creature2).unwrap().tapped = true;

        let artifact = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Signet".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&artifact)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        state.objects.get_mut(&artifact).unwrap().tapped = true;

        // 1 land (tapped, controller P0) — should NOT be untapped
        let land = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        state.objects.get_mut(&land).unwrap().tapped = true;

        let filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![
                TypeFilter::Permanent,
                TypeFilter::Non(Box::new(TypeFilter::Land)),
            ],
            controller: Some(ControllerRef::You),
            properties: vec![],
        });

        let ability = ResolvedAbility::new(
            Effect::UntapAll { target: filter },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_untap_all(&mut state, &ability, &mut events).unwrap();

        // All 3 nonland permanents should be untapped
        assert!(
            !state.objects[&creature1].tapped,
            "creature1 should be untapped"
        );
        assert!(
            !state.objects[&creature2].tapped,
            "creature2 should be untapped"
        );
        assert!(
            !state.objects[&artifact].tapped,
            "artifact should be untapped"
        );
        // Land should remain tapped
        assert!(state.objects[&land].tapped, "land should remain tapped");
        // Should have 3 PermanentUntapped events
        let untap_count = events
            .iter()
            .filter(|e| matches!(e, GameEvent::PermanentUntapped { .. }))
            .count();
        assert_eq!(untap_count, 3);
    }

    #[test]
    fn tap_all_creatures() {
        use crate::types::ability::{TypeFilter, TypedFilter};
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);

        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let land = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: None,
            properties: vec![],
        });

        let ability = ResolvedAbility::new(
            Effect::TapAll { target: filter },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_tap_all(&mut state, &ability, &mut events).unwrap();

        assert!(state.objects[&creature].tapped, "creature should be tapped");
        assert!(!state.objects[&land].tapped, "land should not be tapped");
    }
}
