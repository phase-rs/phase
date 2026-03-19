use std::collections::HashSet;

use crate::game::game_object::{parse_counter_type, CounterType};
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::proposed_event::ProposedEvent;

/// CR 614.1: Add a counter to an object through the replacement pipeline.
///
/// Handles Vorinclex-class doubling, prevention, and replacement effects.
/// Used by both effect resolution (resolve_add) and turn-based actions
/// (Saga lore counters at precombat main phase).
pub fn add_counter_with_replacement(
    state: &mut GameState,
    object_id: crate::types::identifiers::ObjectId,
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

                    if matches!(
                        counter_type,
                        CounterType::Plus1Plus1 | CounterType::Minus1Minus1
                    ) {
                        state.layers_dirty = true;
                    }

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
        } => (counter_type.clone(), *count as u32),
        _ => ("P1P1".to_string(), 1),
    };
    let ct = parse_counter_type(&counter_type_str);

    let targets = resolve_defined_or_targets(ability);
    for obj_id in targets {
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

/// CR 121.5: Read counters from source and put equivalent counters on target.
/// Does NOT remove counters from source — per official rulings, "put its counters on"
/// creates new counters matching the source's counter state.
pub fn resolve_move(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Read source counters (clone — we never remove from source)
    let source_counters = state
        .objects
        .get(&ability.source_id)
        .map(|obj| obj.counters.clone())
        .unwrap_or_default();

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
pub fn resolve_remove(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (counter_type_str, counter_num) = match &ability.effect {
        Effect::RemoveCounter {
            counter_type,
            count,
            ..
        } => (counter_type.clone(), *count as u32),
        _ => ("P1P1".to_string(), 1),
    };
    let ct = parse_counter_type(&counter_type_str);

    let targets = resolve_defined_or_targets(ability);
    for obj_id in targets {
        let proposed = ProposedEvent::RemoveCounter {
            object_id: obj_id,
            counter_type: ct.clone(),
            count: counter_num,
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
                    let obj = state
                        .objects
                        .get_mut(&object_id)
                        .ok_or(EffectError::ObjectNotFound(object_id))?;
                    let entry = obj.counters.entry(counter_type.clone()).or_insert(0);
                    *entry = entry.saturating_sub(count);

                    if matches!(
                        counter_type,
                        CounterType::Plus1Plus1 | CounterType::Minus1Minus1
                    ) {
                        state.layers_dirty = true;
                    }

                    events.push(GameEvent::CounterRemoved {
                        object_id,
                        counter_type,
                        count,
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
    use crate::types::ability::TargetFilter;
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
                    count: 2,
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
                    count: 3,
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
                    count: 1,
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
                count: 1,
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
}
