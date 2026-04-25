use std::collections::HashSet;

use crate::game::game_object::GameObject;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::counter::{parse_counter_type, CounterType};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::proposed_event::ProposedEvent;

/// CR 306.5b + CR 310.4c: After mutating the counter map, re-derive the
/// `obj.loyalty` / `obj.defense` field so the counter count and the cached
/// characteristic stay in lockstep. This is the single site outside
/// `evaluate_layers` that writes those fields.
///
/// Other counter types (P1P1, M1M1, Stun, Lore, Generic) don't project into
/// a dedicated field — their effects flow through layer 7c (P/T) or are
/// evaluated directly from the counter map at read time.
fn sync_derived_from_counters(obj: &mut GameObject, counter_type: &CounterType) {
    match counter_type {
        // CR 306.5b: A planeswalker's loyalty equals the number of loyalty counters on it.
        CounterType::Loyalty => {
            obj.loyalty = Some(
                obj.counters
                    .get(&CounterType::Loyalty)
                    .copied()
                    .unwrap_or(0),
            );
        }
        // CR 310.4c: A battle's defense equals the number of defense counters on it.
        CounterType::Defense => {
            obj.defense = Some(
                obj.counters
                    .get(&CounterType::Defense)
                    .copied()
                    .unwrap_or(0),
            );
        }
        // CR 702.62a + CR 702.63a: Time counters live only in the counter map
        // (read by the suspend upkeep / vanishing triggers) — no derived field.
        CounterType::Plus1Plus1
        | CounterType::Minus1Minus1
        | CounterType::Stun
        | CounterType::Lore
        | CounterType::Time
        | CounterType::Keyword(_)
        | CounterType::Generic(_) => {}
    }
}

/// CR 613.1d: Mark layers dirty if this counter type projects into a derived
/// characteristic computed by the layer system. P1P1/M1M1 feed layer 7c;
/// Loyalty/Defense are cached fields mirrored from the counter map; keyword
/// counters grant abilities at layer 6 (CR 122.1b). Setting `layers_dirty`
/// for these is defensive — the layer reset/re-derive path is idempotent
/// when counters already match.
fn counter_type_affects_layers(counter_type: &CounterType) -> bool {
    matches!(
        counter_type,
        CounterType::Plus1Plus1
            | CounterType::Minus1Minus1
            | CounterType::Loyalty
            | CounterType::Defense
            | CounterType::Keyword(_)
    )
}

/// CR 614.1: Add a counter to an object through the replacement pipeline.
///
/// Single authority for counter additions. Handles Vorinclex/Doubling-Season
/// class doubling (CR 614.1a), prevention, and replacement effects. Used by:
/// - effect resolution (resolve_add)
/// - turn-based actions (Saga lore counters at precombat main phase)
/// - CR 614.1c ETB counters (routed through `apply_etb_counters`)
/// - loyalty-ability cost payment (CR 606.4) for positive loyalty amounts
/// - damage redirection to battles (CR 120.3h) — reversed via the remove path
pub fn add_counter_with_replacement(
    state: &mut GameState,
    object_id: ObjectId,
    counter_type: CounterType,
    count: u32,
    events: &mut Vec<GameEvent>,
) {
    let proposed = ProposedEvent::AddCounter {
        object_id,
        counter_type,
        count,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if let ProposedEvent::AddCounter {
                object_id,
                counter_type,
                count,
                ..
            } = event
            {
                if let Some(obj) = state.objects.get_mut(&object_id) {
                    let entry = obj.counters.entry(counter_type.clone()).or_insert(0);
                    *entry += count;

                    // CR 306.5b / CR 310.4c: Keep obj.loyalty / obj.defense in
                    // sync with the counter map — the field IS the counter count.
                    sync_derived_from_counters(obj, &counter_type);

                    if counter_type_affects_layers(&counter_type) {
                        state.layers_dirty = true;
                    }

                    // CR 122.1: Track that this player added a counter this turn
                    state
                        .players_who_added_counter_this_turn
                        .insert(obj.controller);

                    events.push(GameEvent::CounterAdded {
                        object_id,
                        counter_type,
                        count,
                    });
                }
            }
        }
        ReplacementResult::Prevented => {}
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
        }
    }
}

/// CR 614.1: Remove counters from an object through the replacement pipeline.
///
/// Single authority for counter removal, mirroring `add_counter_with_replacement`.
/// Used by:
/// - effect resolution (resolve_remove)
/// - combat / effect damage to planeswalkers (CR 120.3c, CR 306.8) and battles (CR 120.3h, CR 310.6)
/// - loyalty-ability cost payment (CR 606.4) for negative loyalty amounts
///
/// The count is clamped to the number of counters actually present, so callers
/// can pass the raw damage/cost amount without pre-clamping.
pub fn remove_counter_with_replacement(
    state: &mut GameState,
    object_id: ObjectId,
    counter_type: CounterType,
    count: u32,
    events: &mut Vec<GameEvent>,
) {
    let proposed = ProposedEvent::RemoveCounter {
        object_id,
        counter_type,
        count,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if let ProposedEvent::RemoveCounter {
                object_id,
                counter_type,
                count,
                ..
            } = event
            {
                if let Some(obj) = state.objects.get_mut(&object_id) {
                    let entry = obj.counters.entry(counter_type.clone()).or_insert(0);
                    let removed = (*entry).min(count);
                    *entry = entry.saturating_sub(count);

                    // CR 306.5b / CR 310.4c: Keep obj.loyalty / obj.defense in
                    // sync with the counter map — the field IS the counter count.
                    sync_derived_from_counters(obj, &counter_type);

                    if counter_type_affects_layers(&counter_type) {
                        state.layers_dirty = true;
                    }

                    // CR 122.1: Only emit when counters were actually removed,
                    // matching the semantics of the legacy in-line path.
                    if removed > 0 {
                        events.push(GameEvent::CounterRemoved {
                            object_id,
                            counter_type,
                            count: removed,
                        });
                    }
                }
            }
        }
        ReplacementResult::Prevented => {}
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
        }
    }
}

/// Add counters to target objects.
pub fn resolve_add(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (counter_type_str, counter_num) = match &ability.effect {
        Effect::AddCounter {
            counter_type,
            count,
            ..
        }
        | Effect::PutCounter {
            counter_type,
            count,
            ..
        } => {
            // CR 107.1b: Ability-context resolve so X-counter effects (e.g. "put X +1/+1 counters")
            // pick up the caster-chosen X.
            let resolved_count =
                crate::game::quantity::resolve_quantity_with_targets(state, count, ability).max(0)
                    as u32;
            (counter_type.clone(), resolved_count)
        }
        _ => ("P1P1".to_string(), 1),
    };
    let ct = parse_counter_type(&counter_type_str);

    // CR 601.2d: If distribution was assigned at cast time, apply per-target counter counts.
    if let Some(distribution) = &ability.distribution {
        for (target, count) in distribution {
            if let crate::types::ability::TargetRef::Object(obj_id) = target {
                add_counter_with_replacement(state, *obj_id, ct.clone(), *count, events);
            }
        }
    } else {
        let targets = resolve_defined_or_targets(ability);
        for obj_id in targets {
            add_counter_with_replacement(state, obj_id, ct.clone(), counter_num, events);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 122.1: Place counters on all battlefield objects matching a filter (no targeting).
pub fn resolve_add_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (counter_type_str, counter_num, target_filter) = match &ability.effect {
        Effect::PutCounterAll {
            counter_type,
            count,
            target,
        } => {
            let resolved =
                crate::game::quantity::resolve_quantity_with_targets(state, count, ability).max(0)
                    as u32;
            (counter_type.clone(), resolved, target.clone())
        }
        _ => return Ok(()),
    };
    let ct = parse_counter_type(&counter_type_str);
    let target_filter = crate::game::effects::resolved_object_filter(ability, &target_filter);

    // Collect matching IDs first to avoid borrow conflict during mutation.
    // CR 107.3a + CR 601.2b: ability-context filter evaluation.
    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    let matching_ids: Vec<crate::types::identifiers::ObjectId> = state
        .battlefield
        .iter()
        .filter(|id| crate::game::filter::matches_target_filter(state, **id, &target_filter, &ctx))
        .copied()
        .collect();

    for obj_id in matching_ids {
        add_counter_with_replacement(state, obj_id, ct.clone(), counter_num, events);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Multiply counters on target objects (default: double).
pub fn resolve_multiply(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (counter_type_str, multiplier) = match &ability.effect {
        Effect::MultiplyCounter {
            counter_type,
            multiplier,
            ..
        } => (counter_type.clone(), *multiplier as u32),
        _ => ("P1P1".to_string(), 2),
    };

    let targets = resolve_defined_or_targets(ability);
    for obj_id in targets {
        let ct = parse_counter_type(&counter_type_str);
        let obj = state
            .objects
            .get_mut(&obj_id)
            .ok_or(EffectError::ObjectNotFound(obj_id))?;
        let current = obj.counters.get(&ct).copied().unwrap_or(0);
        let to_add = current.saturating_mul(multiplier).saturating_sub(current);
        if to_add > 0 {
            let entry = obj.counters.entry(ct.clone()).or_insert(0);
            *entry += to_add;

            if matches!(ct, CounterType::Plus1Plus1 | CounterType::Minus1Minus1) {
                state.layers_dirty = true;
            }

            events.push(GameEvent::CounterAdded {
                object_id: obj_id,
                counter_type: ct.clone(),
                count: to_add,
            });
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Resolve targeting to object IDs using the typed TargetFilter.
fn resolve_defined_or_targets(
    ability: &ResolvedAbility,
) -> Vec<crate::types::identifiers::ObjectId> {
    let target_spec = match &ability.effect {
        Effect::MultiplyCounter { target, .. }
        | Effect::AddCounter { target, .. }
        | Effect::RemoveCounter { target, .. }
        | Effect::PutCounter { target, .. } => Some(target),
        _ => None,
    };

    if let Some(TargetFilter::None) = target_spec {
        return vec![ability.source_id];
    }

    // If the filter is SelfRef, target the source
    if let Some(TargetFilter::SelfRef) = target_spec {
        return vec![ability.source_id];
    }

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
}

/// CR 122.8: Read counters from source and put equivalent counters on target.
/// Does NOT remove counters from source — per official rulings, "put its counters on"
/// creates new counters matching the source's counter state.
pub fn resolve_move(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 400.7: Read source counters, falling back to LKI cache for objects that
    // have changed zones (e.g., dies triggers — counters cease to exist on zone
    // change per CR 122.2, but the LKI snapshot preserves them).
    let source_counters = {
        let current = state
            .objects
            .get(&ability.source_id)
            .map(|obj| obj.counters.clone())
            .unwrap_or_default();
        if current.is_empty() {
            // Object may have lost counters during zone change — check LKI
            state
                .lki_cache
                .get(&ability.source_id)
                .map(|lki| lki.counters.clone())
                .unwrap_or_default()
        } else {
            current
        }
    };

    if source_counters.is_empty() {
        // No counters to copy — no-op
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // Filter by counter_type if specified
    let counter_type_filter = match &ability.effect {
        Effect::MoveCounters { counter_type, .. } => counter_type.as_deref(),
        _ => None,
    };

    // Resolve destination target
    let dest_ids: Vec<_> = ability
        .targets
        .iter()
        .filter_map(|t| {
            if let TargetRef::Object(id) = t {
                Some(*id)
            } else {
                None
            }
        })
        .collect();

    for dest_id in dest_ids {
        for (ct, &count) in &source_counters {
            if count == 0 {
                continue;
            }
            // Filter by type if specified
            if let Some(type_filter) = counter_type_filter {
                let ct_name = format!("{ct:?}");
                if !ct_name.eq_ignore_ascii_case(type_filter) {
                    continue;
                }
            }
            add_counter_with_replacement(state, dest_id, ct.clone(), count, events);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Remove counters from target objects, clamping at 0.
/// CR 122.1: When counter_type is empty, removes counters of every type (Vampire Hexmage).
pub fn resolve_remove(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (counter_type_str, raw_count) = match &ability.effect {
        Effect::RemoveCounter {
            counter_type,
            count,
            ..
        } => (counter_type.clone(), *count),
        _ => ("P1P1".to_string(), 1),
    };

    // CR 122.1: Empty counter_type means "all types" — collect each type on the object.
    let all_types = counter_type_str.is_empty();

    let targets = resolve_defined_or_targets(ability);
    for obj_id in targets {
        // Build the list of (counter_type, count) pairs to remove.
        let removals: Vec<(CounterType, u32)> = if all_types {
            // Remove all counter types. count == -1 means remove all of each type;
            // positive count means remove up to that many total (player's choice — for now, remove
            // proportionally starting from the first type).
            let counters: Vec<(CounterType, u32)> = state
                .objects
                .get(&obj_id)
                .map(|obj| {
                    obj.counters
                        .iter()
                        .filter(|(_, &v)| v > 0)
                        .map(|(ct, &v)| (ct.clone(), v))
                        .collect()
                })
                .unwrap_or_default();
            if raw_count < 0 {
                // Remove all of every type.
                counters
            } else {
                // Remove up to N total counters across all types.
                let mut budget = raw_count as u32;
                counters
                    .into_iter()
                    .filter_map(|(ct, available)| {
                        if budget == 0 {
                            return None;
                        }
                        let to_remove = available.min(budget);
                        budget -= to_remove;
                        Some((ct, to_remove))
                    })
                    .collect()
            }
        } else {
            let ct = parse_counter_type(&counter_type_str);
            // CR 122.1: count == -1 means "remove all" — resolve to the actual counter count.
            let counter_num = if raw_count < 0 {
                state
                    .objects
                    .get(&obj_id)
                    .and_then(|obj| obj.counters.get(&ct).copied())
                    .unwrap_or(0)
            } else {
                raw_count as u32
            };
            vec![(ct, counter_num)]
        };

        for (ct, counter_num) in removals {
            // CR 614.1: Delegate to the single-authority remove pipeline so
            // prevention/modification replacements apply and derived fields
            // (obj.loyalty / obj.defense) stay in lockstep with the counter map.
            remove_counter_with_replacement(state, obj_id, ct, counter_num, events);
            // If a replacement requires player choice, suspend and bail — the
            // continuation re-enters the remove pipeline after the choice resolves.
            if matches!(
                state.waiting_for,
                crate::types::game_state::WaitingFor::ReplacementChoice { .. }
            ) {
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
    use crate::types::ability::{QuantityExpr, TargetFilter};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_counter_ability(effect: Effect, target: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            effect,
            vec![TargetRef::Object(target)],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn add_counter_increments() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();

        resolve_add(
            &mut state,
            &make_counter_ability(
                Effect::AddCounter {
                    counter_type: "P1P1".to_string(),
                    count: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::Any,
                },
                obj_id,
            ),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.objects[&obj_id].counters[&CounterType::Plus1Plus1], 2);
    }

    #[test]
    fn remove_counter_decrements_clamped() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        let mut events = Vec::new();

        resolve_remove(
            &mut state,
            &make_counter_ability(
                Effect::RemoveCounter {
                    counter_type: "P1P1".to_string(),
                    count: 3,
                    target: TargetFilter::Any,
                },
                obj_id,
            ),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.objects[&obj_id].counters[&CounterType::Plus1Plus1], 0);
    }

    #[test]
    fn add_generic_counter() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Artifact".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();

        resolve_add(
            &mut state,
            &make_counter_ability(
                Effect::AddCounter {
                    counter_type: "charge".to_string(),
                    count: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::Any,
                },
                obj_id,
            ),
            &mut events,
        )
        .unwrap();

        assert_eq!(
            state.objects[&obj_id].counters[&CounterType::Generic("charge".to_string())],
            3
        );
    }

    #[test]
    fn add_counter_emits_counter_added_event() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();

        resolve_add(
            &mut state,
            &make_counter_ability(
                Effect::AddCounter {
                    counter_type: "P1P1".to_string(),
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Any,
                },
                obj_id,
            ),
            &mut events,
        )
        .unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::CounterAdded {
                counter_type: CounterType::Plus1Plus1,
                count: 1,
                ..
            }
        )));
    }

    /// Regression test: SelfRef PutCounter (Ajani's Pridemate trigger) must apply the counter
    /// to the source object even when ability.targets is empty.
    #[test]
    fn put_counter_self_ref_applies_to_source() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();

        let ability = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: "P1P1".to_string(),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            },
            vec![], // empty targets — must resolve via SelfRef → source_id
            source_id,
            PlayerId(0),
        );

        resolve_add(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects[&source_id].counters[&CounterType::Plus1Plus1],
            1,
            "SelfRef counter must land on the source object"
        );
        assert!(state.layers_dirty, "layers must be dirtied for P/T counter");
    }

    /// Regression test: "+1/+1" oracle-text counter type must map to Plus1Plus1.
    #[test]
    fn parse_counter_type_oracle_text_forms() {
        assert_eq!(parse_counter_type("+1/+1"), CounterType::Plus1Plus1);
        assert_eq!(parse_counter_type("-1/-1"), CounterType::Minus1Minus1);
        assert_eq!(parse_counter_type("P1P1"), CounterType::Plus1Plus1);
        assert_eq!(parse_counter_type("M1M1"), CounterType::Minus1Minus1);
    }

    /// End-to-end Gruff Triplets pipeline test. CR 603.10a + CR 208.3 + CR 122.1:
    /// when a Gruff Triplets dies, each other Gruff Triplets on the battlefield
    /// you control gets +1/+1 counters equal to the dying copy's power (LKI).
    ///
    /// Mirrors the shape of `test_rancor_ltb_pipeline_returns_to_owner_hand` in
    /// bounce.rs: build the parsed trigger AST explicitly, destroy the source,
    /// run `process_triggers` + `resolve_top`, and verify counter placement.
    #[test]
    fn gruff_triplets_dies_trigger_uses_lki_power_for_counter_count() {
        use crate::game::stack::resolve_top;
        use crate::game::triggers::process_triggers;
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, ControllerRef, FilterProp, QuantityExpr, QuantityRef,
            TriggerDefinition, TypeFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);

        // Two Gruff Triplets on the battlefield owned by the same player.
        let dying_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gruff Triplets".to_string(),
            Zone::Battlefield,
        );
        let sibling_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Gruff Triplets".to_string(),
            Zone::Battlefield,
        );
        for &id in &[dying_id, sibling_id] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.power = Some(3);
            obj.toughness = Some(3);
            obj.card_types.core_types.push(CoreType::Creature);
        }

        // Wire the dies-trigger AST as the parser would emit it.
        let target = TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Creature)
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::Named {
                    name: "Gruff Triplets".to_string(),
                }]),
        );
        let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
        trigger.origin = Some(Zone::Battlefield);
        trigger.destination = Some(Zone::Graveyard);
        trigger.valid_card = Some(TargetFilter::SelfRef);
        trigger.trigger_zones = vec![Zone::Graveyard];
        trigger.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounterAll {
                counter_type: "P1P1".to_string(),
                count: QuantityExpr::Ref {
                    qty: QuantityRef::SelfPower,
                },
                target,
            },
        )));
        state
            .objects
            .get_mut(&dying_id)
            .unwrap()
            .trigger_definitions
            .push(trigger);

        // Move the dying copy to the graveyard, run the trigger pipeline,
        // resolve the resulting ability.
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, dying_id, Zone::Graveyard, &mut events);
        assert!(state.players[0].graveyard.contains(&dying_id));

        process_triggers(&mut state, &events);
        assert_eq!(state.stack.len(), 1, "dies trigger did not reach stack");

        let mut resolve_events = Vec::new();
        resolve_top(&mut state, &mut resolve_events);

        // Sibling should have 3 +1/+1 counters (the dying copy's LKI power).
        // The dying copy itself is in the graveyard and must not receive counters
        // (it no longer matches the battlefield-filtered target set).
        assert_eq!(
            state.objects[&sibling_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            3,
            "sibling should get +1/+1 counters equal to LKI power of dying Triplets"
        );
        assert!(
            !state.objects[&dying_id]
                .counters
                .contains_key(&CounterType::Plus1Plus1),
            "dying copy in graveyard should not receive counters"
        );
    }

    /// Regression test: MoveCounters must use LKI when the source has changed zones.
    /// Simulates Essence Channeler's "When this creature dies, put its counters on
    /// target creature you control" — the source is in the graveyard with no counters,
    /// but the LKI cache preserves the counters it had on the battlefield.
    #[test]
    fn move_counters_uses_lki_when_source_changed_zones() {
        use crate::types::game_state::LKISnapshot;

        let mut state = GameState::new_two_player(42);

        // Source creature (Essence Channeler) — already in graveyard, no counters
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Essence Channeler".to_string(),
            Zone::Graveyard,
        );

        // Destination creature on battlefield
        let dest_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        // Populate LKI cache as if the source died with 3 +1/+1 counters
        let mut lki_counters = std::collections::HashMap::new();
        lki_counters.insert(CounterType::Plus1Plus1, 3);
        state.lki_cache.insert(
            source_id,
            LKISnapshot {
                name: "Essence Channeler".to_string(),
                power: Some(5),
                toughness: Some(4),
                mana_value: 2,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![],
                counters: lki_counters,
            },
        );

        let ability = ResolvedAbility::new(
            Effect::MoveCounters {
                source: TargetFilter::SelfRef,
                counter_type: None,
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(dest_id)],
            source_id,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve_move(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects[&dest_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            3,
            "destination should receive counters from LKI cache"
        );
    }

    /// CR 306.5b: Adding a Loyalty counter through the resolver must keep
    /// `obj.loyalty` in lockstep with `counters[Loyalty]`. This is the
    /// invariant that prevents the Tezzeret-class display bug where the
    /// loyalty trigger fires but the visible loyalty doesn't update.
    #[test]
    fn add_loyalty_counter_syncs_loyalty_field() {
        let mut state = GameState::new_two_player(42);
        let pw_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Tezzeret".to_string(),
            Zone::Battlefield,
        );
        // Seed pre-existing 4 loyalty counters (planeswalker on battlefield).
        let obj = state.objects.get_mut(&pw_id).unwrap();
        obj.loyalty = Some(4);
        obj.counters.insert(CounterType::Loyalty, 4);

        let mut events = Vec::new();
        add_counter_with_replacement(&mut state, pw_id, CounterType::Loyalty, 1, &mut events);

        let obj = &state.objects[&pw_id];
        assert_eq!(
            obj.counters.get(&CounterType::Loyalty).copied(),
            Some(5),
            "counter map must reflect the increment"
        );
        assert_eq!(
            obj.loyalty,
            Some(5),
            "obj.loyalty must mirror counters[Loyalty] (CR 306.5b)"
        );
    }

    /// CR 306.5b: Removing a Loyalty counter through the resolver must keep
    /// `obj.loyalty` in lockstep, including the saturating clamp at zero.
    #[test]
    fn remove_loyalty_counter_syncs_loyalty_field_with_clamp() {
        let mut state = GameState::new_two_player(42);
        let pw_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test PW".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&pw_id).unwrap();
        obj.loyalty = Some(3);
        obj.counters.insert(CounterType::Loyalty, 3);

        let mut events = Vec::new();
        // Damage exceeds loyalty — must clamp to 0, not underflow.
        remove_counter_with_replacement(&mut state, pw_id, CounterType::Loyalty, 5, &mut events);

        let obj = &state.objects[&pw_id];
        assert_eq!(obj.counters.get(&CounterType::Loyalty).copied(), Some(0));
        assert_eq!(obj.loyalty, Some(0));
    }

    /// CR 310.4c: Defense counters drive `obj.defense` for battles. The same
    /// resolver-sync invariant applies to battles.
    #[test]
    fn add_remove_defense_counter_syncs_defense_field() {
        let mut state = GameState::new_two_player(42);
        let battle_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Siege".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&battle_id).unwrap();
        obj.defense = Some(4);
        obj.counters.insert(CounterType::Defense, 4);

        let mut events = Vec::new();
        add_counter_with_replacement(&mut state, battle_id, CounterType::Defense, 2, &mut events);
        assert_eq!(state.objects[&battle_id].defense, Some(6));
        assert_eq!(
            state.objects[&battle_id]
                .counters
                .get(&CounterType::Defense)
                .copied(),
            Some(6)
        );

        remove_counter_with_replacement(
            &mut state,
            battle_id,
            CounterType::Defense,
            3,
            &mut events,
        );
        assert_eq!(state.objects[&battle_id].defense, Some(3));
        assert_eq!(
            state.objects[&battle_id]
                .counters
                .get(&CounterType::Defense)
                .copied(),
            Some(3)
        );
    }

    /// CR 613.1 + CR 306.5b: After the resolver syncs `obj.loyalty`, a forced
    /// `evaluate_layers` call must leave the value unchanged — the layer
    /// reset/re-derive path is idempotent when counters and field already match.
    #[test]
    fn loyalty_field_survives_layer_re_evaluation() {
        use crate::game::layers::evaluate_layers;
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        let pw_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test PW".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&pw_id).unwrap();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        // Base printed loyalty 4; counter map starts in sync.
        obj.base_loyalty = Some(4);
        obj.loyalty = Some(4);
        obj.counters.insert(CounterType::Loyalty, 4);

        let mut events = Vec::new();
        add_counter_with_replacement(&mut state, pw_id, CounterType::Loyalty, 1, &mut events);
        assert_eq!(state.objects[&pw_id].loyalty, Some(5));

        // Force layer re-evaluation: should re-derive obj.loyalty from the
        // counter map and land on the same value.
        evaluate_layers(&mut state);
        assert_eq!(
            state.objects[&pw_id].loyalty,
            Some(5),
            "obj.loyalty must remain 5 after layer reset+re-derive"
        );
        assert_eq!(
            state.objects[&pw_id]
                .counters
                .get(&CounterType::Loyalty)
                .copied(),
            Some(5),
            "counters[Loyalty] must remain 5 after layer evaluation"
        );
    }

    /// Tezzeret, Cruel Captain regression: after a planeswalker enters with
    /// printed loyalty 4 and a "put a loyalty counter on this" trigger fires
    /// twice (e.g., because two artifacts entered), `obj.loyalty` must show
    /// 4 → 5 → 6 in lockstep with the counter map. Pre-fix, the field stayed
    /// stale at 4 (or jumped to 1 after the next layer re-evaluation).
    #[test]
    fn tezzeret_class_loyalty_trigger_synced_each_increment() {
        let mut state = GameState::new_two_player(42);
        let pw_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Tezzeret, Cruel Captain".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&pw_id).unwrap();
        obj.base_loyalty = Some(4);
        obj.loyalty = Some(4);
        obj.counters.insert(CounterType::Loyalty, 4);

        let mut events = Vec::new();
        // Trigger 1 fires.
        add_counter_with_replacement(&mut state, pw_id, CounterType::Loyalty, 1, &mut events);
        assert_eq!(state.objects[&pw_id].loyalty, Some(5));
        assert_eq!(
            state.objects[&pw_id]
                .counters
                .get(&CounterType::Loyalty)
                .copied(),
            Some(5)
        );

        // Trigger 2 fires.
        add_counter_with_replacement(&mut state, pw_id, CounterType::Loyalty, 1, &mut events);
        assert_eq!(
            state.objects[&pw_id].loyalty,
            Some(6),
            "second trigger must take loyalty 5 → 6, not regress to 1"
        );
        assert_eq!(
            state.objects[&pw_id]
                .counters
                .get(&CounterType::Loyalty)
                .copied(),
            Some(6)
        );
    }

    /// CR 614.1a + CR 614.1c: A Doubling-Season-class AddCounter replacement
    /// must apply when a planeswalker enters with intrinsic loyalty counters,
    /// because the intrinsic CR 306.5b replacement is now routed through
    /// `add_counter_with_replacement` (which dispatches each counter through
    /// the AddCounter replacement pipeline).
    ///
    /// Uses a hand-crafted replacement that doubles AddCounter quantities to
    /// avoid depending on Doubling Season specifically being implemented.
    #[test]
    fn intrinsic_etb_loyalty_counters_apply_doubling_replacement() {
        use crate::game::engine_replacement::apply_etb_counters;
        use crate::types::ability::{QuantityModification, ReplacementDefinition, TargetFilter};
        use crate::types::card_type::CoreType;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);

        // Doubling-Season fixture: a permanent on the battlefield carrying an
        // AddCounter replacement that doubles the count.
        let doubler_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Counter Doubler".to_string(),
            Zone::Battlefield,
        );
        let mut doubler_repl = ReplacementDefinition::new(ReplacementEvent::AddCounter);
        doubler_repl.valid_card = Some(TargetFilter::Any);
        doubler_repl.quantity_modification = Some(QuantityModification::Double);
        state
            .objects
            .get_mut(&doubler_id)
            .unwrap()
            .replacement_definitions
            .push(doubler_repl);

        // Planeswalker entering the battlefield with printed loyalty 3.
        // We simulate the post-ZoneChange entry path: the object is on the
        // battlefield with empty counter map and obj.loyalty seeded from the
        // printed value, then `apply_etb_counters` dispatches the intrinsic
        // CR 306.5b counter through the AddCounter replacement pipeline.
        let pw_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test PW".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&pw_id).unwrap();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        obj.loyalty = Some(3);
        obj.base_loyalty = Some(3);

        let intrinsic = vec![(CounterType::Loyalty.as_str().to_string(), 3u32)];
        let mut events = Vec::new();
        apply_etb_counters(&mut state, pw_id, &intrinsic, &mut events);

        let obj = &state.objects[&pw_id];
        assert_eq!(
            obj.counters.get(&CounterType::Loyalty).copied(),
            Some(6),
            "Doubling-class replacement must double the intrinsic 3 → 6"
        );
        assert_eq!(
            obj.loyalty,
            Some(6),
            "obj.loyalty must mirror the doubled counter count"
        );
    }
}
