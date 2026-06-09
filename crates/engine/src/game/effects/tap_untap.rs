use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetChoiceTiming, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::{ObjectId, TrackedSetId};
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// CR 603.7e + CR 608.2c: Resolve the objects a `Tap`/`Untap` effect acts on.
///
/// - `SelfRef` → the source object — the printed-name "tap ~"/"untap ~"
///   anaphor that always refers to the source regardless of `ability.targets`.
/// - `TrackedSet` → the chain's tracked object set published by a preceding
///   effect (e.g. `ChooseObjectsIntoTrackedSet`'s "untap those creatures"
///   tail). The `TrackedSetId(0)` sentinel binds to the highest tracked-set
///   id — the set the most recent effect in this chain published — exactly
///   as `grant_permission::resolve` binds it. Empty sets are not skipped: an
///   empty current set means the preceding effect affected nothing.
/// - Any other filter → the ability's chosen targets (object refs only).
fn tap_untap_target_ids(
    state: &GameState,
    ability: &ResolvedAbility,
    effect_target: &TargetFilter,
) -> Vec<ObjectId> {
    match effect_target {
        TargetFilter::SelfRef => vec![ability.source_id],
        TargetFilter::TrackedSet {
            id: TrackedSetId(0),
        } => state
            .tracked_object_sets
            .iter()
            .max_by_key(|(id, _)| id.0)
            .map(|(_, objects)| objects.clone())
            .unwrap_or_default(),
        TargetFilter::TrackedSet { id } => state
            .tracked_object_sets
            .get(id)
            .cloned()
            .unwrap_or_default(),
        _ => ability
            .targets
            .iter()
            .filter_map(|t| match t {
                TargetRef::Object(id) => Some(*id),
                TargetRef::Player(_) => None,
            })
            .collect(),
    }
}

/// CR 701.26a: Tap — turn a permanent sideways. CR 701.26b: Untap — return to upright.
pub fn resolve_tap(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 701.26a + CR 608.2c: `Effect::Tap`'s subject is resolved from its own
    // `target` filter — `SelfRef` (the printed-name "tap ~" anaphor) and
    // `TrackedSet` ("tap those creatures") resolve regardless of
    // `ability.targets`, so chained `Tap` sub-abilities don't inherit the
    // parent's targets via chain propagation in
    // `effects::mod.rs::resolve_ability_chain` (issue #323 class).
    let Effect::Tap { target } = &ability.effect else {
        return Err(EffectError::MissingParam("Tap".to_string()));
    };
    if prompt_resolution_tap_untap_choice(state, ability, target, EffectKind::Tap, events) {
        return Ok(());
    }
    let target_ids = tap_untap_target_ids(state, ability, target);
    for obj_id in target_ids {
        if let TapUntapOutcome::NeedsChoice(player) =
            process_one_tap(state, obj_id, ability.source_id, events)?
        {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            return Ok(());
        };
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
    // CR 701.26b + CR 608.2c: `Effect::Untap`'s subject is resolved from its
    // own `target` filter. `SelfRef` is the printed-name "untap ~" anaphor
    // (runtime path for trigger shapes like Ragost's "At the beginning of
    // each end step, if you gained life this turn, untap ~" — CR 603.4
    // intervening-if + CR 514 end step). `TrackedSet` is the chain-unified
    // "untap those creatures" tail of a `ChooseObjectsIntoTrackedSet` chain
    // (CR 603.7e — Magnetic Mountain / Dream Tides / Thelon's Curse). Both
    // resolve from the filter regardless of `ability.targets`, so chained
    // `Untap` sub-abilities don't inherit the parent's targets via chain
    // propagation in `effects::mod.rs::resolve_ability_chain` (issue #323
    // class).
    let Effect::Untap { target } = &ability.effect else {
        return Err(EffectError::MissingParam("Untap".to_string()));
    };
    if prompt_resolution_tap_untap_choice(state, ability, target, EffectKind::Untap, events) {
        return Ok(());
    }
    let target_ids = tap_untap_target_ids(state, ability, target);
    for obj_id in target_ids {
        if let TapUntapOutcome::NeedsChoice(player) = process_one_untap(state, obj_id, events)? {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            return Ok(());
        };
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

pub(crate) enum TapUntapOutcome {
    Complete,
    NeedsChoice(crate::types::player::PlayerId),
}

pub(crate) fn process_one_tap(
    state: &mut GameState,
    object_id: ObjectId,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<TapUntapOutcome, EffectError> {
    let proposed = ProposedEvent::Tap {
        object_id,
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
                    caused_by: Some(source_id),
                });
            }
            Ok(TapUntapOutcome::Complete)
        }
        ReplacementResult::Prevented => Ok(TapUntapOutcome::Complete),
        ReplacementResult::NeedsChoice(player) => Ok(TapUntapOutcome::NeedsChoice(player)),
    }
}

pub(crate) fn process_one_untap(
    state: &mut GameState,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<TapUntapOutcome, EffectError> {
    let proposed = ProposedEvent::Untap {
        object_id,
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
            Ok(TapUntapOutcome::Complete)
        }
        ReplacementResult::Prevented => Ok(TapUntapOutcome::Complete),
        ReplacementResult::NeedsChoice(player) => Ok(TapUntapOutcome::NeedsChoice(player)),
    }
}

fn prompt_resolution_tap_untap_choice(
    state: &mut GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
    effect_kind: EffectKind,
    events: &mut Vec<GameEvent>,
) -> bool {
    if ability.target_choice_timing != TargetChoiceTiming::Resolution || !ability.targets.is_empty()
    {
        return false;
    }
    let Some(spec) = ability.multi_target.as_ref() else {
        return false;
    };

    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    let eligible: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| crate::game::filter::matches_target_filter(state, *id, target, &ctx))
        .collect();
    let Ok(bounds) = crate::game::ability_utils::resolve_multi_target_bounds(
        state,
        ability,
        spec,
        eligible.len(),
    ) else {
        return false;
    };

    if bounds.max == 0 && bounds.min == 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return true;
    }

    state.waiting_for = WaitingFor::EffectZoneChoice {
        player: ability.controller,
        cards: eligible,
        count: bounds.max,
        min_count: bounds.min,
        up_to: bounds.min != bounds.max,
        source_id: ability.source_id,
        effect_kind,
        zone: Zone::Battlefield,
        destination: None,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enter_transformed: false,
        enters_under_player: None,
        enters_attacking: false,
        owner_library: false,
        track_exiled_by_source: false,
        count_param: 0,
    };
    true
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
    use crate::types::ability::{
        Effect, MultiTargetSpec, QuantityExpr, TargetChoiceTiming, TargetFilter, TypeFilter,
        TypedFilter,
    };
    use crate::types::card_type::CoreType;
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
    fn resolution_timed_multi_untap_prompts_for_battlefield_lands() {
        let mut state = GameState::new_two_player(42);
        let land_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Island".to_string(),
            Zone::Battlefield,
        );
        let land_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        let creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land_a)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        state
            .objects
            .get_mut(&land_b)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut ability = ResolvedAbility::new(
            Effect::Untap {
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Land],
                    controller: None,
                    properties: vec![],
                }),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.multi_target = Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 3 }));
        ability.target_choice_timing = TargetChoiceTiming::Resolution;
        let mut events = Vec::new();

        resolve_untap(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                min_count,
                up_to,
                effect_kind,
                zone,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 2);
                assert_eq!(*min_count, 0);
                assert!(*up_to);
                assert_eq!(*effect_kind, EffectKind::Untap);
                assert_eq!(*zone, Zone::Battlefield);
                assert!(cards.contains(&land_a));
                assert!(cards.contains(&land_b));
                assert!(!cards.contains(&creature));
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }
        assert!(events.is_empty());
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
