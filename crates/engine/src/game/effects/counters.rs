use std::collections::HashSet;

use crate::game::game_object::GameObject;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    AbilityTag, CounterTransferMode, Effect, EffectError, EffectKind, ResolvedAbility,
    TargetChoiceTiming, TargetFilter, TargetRef,
};
#[cfg(test)]
use crate::types::counter::parse_counter_type;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{CounterAddedRecord, GameState};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;

/// CR 306.5c + CR 310.4c: After mutating the counter map, re-derive the
/// `obj.loyalty` / `obj.defense` field so the counter count and the cached
/// characteristic stay in lockstep. This is the single site outside
/// `evaluate_layers` that writes those fields.
///
/// Other counter types (P1P1, M1M1, Stun, Lore, Generic) don't project into
/// a dedicated field — their effects flow through layer 7c (P/T) or are
/// evaluated directly from the counter map at read time.
fn sync_derived_from_counters(obj: &mut GameObject, counter_type: &CounterType) {
    match counter_type {
        // CR 306.5c: A planeswalker's loyalty equals the number of loyalty counters on it.
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
        // CR 702.24a: Age counters likewise live only in the counter map (read
        // by the cumulative-upkeep trigger to scale the cost) — no derived field.
        CounterType::Plus1Plus1
        | CounterType::Minus1Minus1
        | CounterType::PowerToughness { .. }
        | CounterType::Stun
        | CounterType::Lore
        | CounterType::Time
        | CounterType::Age
        | CounterType::Keyword(_)
        | CounterType::Generic(_) => {}
    }
}

/// Mark layers dirty if this counter type projects into a derived characteristic
/// computed by the layer system. P/T counters feed layer 7c (CR 613.4c);
/// Loyalty/Defense are cached fields mirrored from the counter map; keyword
/// counters grant abilities at layer 6 (CR 613.1f + CR 122.1b). Setting
/// `layers_dirty` for these is defensive — the layer reset/re-derive path is
/// idempotent when counters already match.
pub(crate) fn counter_type_affects_layers(counter_type: &CounterType) -> bool {
    counter_type.power_toughness_delta().is_some()
        || matches!(
            counter_type,
            CounterType::Loyalty | CounterType::Defense | CounterType::Keyword(_)
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
    actor: PlayerId,
    object_id: ObjectId,
    counter_type: CounterType,
    count: u32,
    events: &mut Vec<GameEvent>,
) {
    if count == 0 {
        return;
    }
    let proposed = ProposedEvent::AddCounter {
        actor,
        object_id,
        counter_type,
        count,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if let ProposedEvent::AddCounter {
                actor,
                object_id,
                counter_type,
                count,
                ..
            } = event
            {
                apply_counter_addition(state, actor, object_id, counter_type, count, events);
            }
        }
        ReplacementResult::Prevented => {}
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
        }
    }
}

/// CR 122.1 + CR 122.6: Apply an already-accepted counter addition and record
/// the actor/recipient snapshot for "counters you've put this turn" quantities.
pub(crate) fn apply_counter_addition(
    state: &mut GameState,
    actor: PlayerId,
    object_id: ObjectId,
    counter_type: CounterType,
    count: u32,
    events: &mut Vec<GameEvent>,
) {
    if count == 0 {
        return;
    }

    let Some(obj) = state.objects.get_mut(&object_id) else {
        return;
    };

    let entry = obj.counters.entry(counter_type.clone()).or_insert(0);
    *entry += count;

    // CR 306.5c / CR 310.4c: Keep obj.loyalty / obj.defense in
    // sync with the counter map — the field IS the counter count.
    sync_derived_from_counters(obj, &counter_type);

    if counter_type_affects_layers(&counter_type) {
        state.layers_dirty = true;
    }

    state.counter_added_this_turn.push(CounterAddedRecord {
        actor,
        object_id,
        counter_type: counter_type.clone(),
        count,
        name: obj.name.clone(),
        core_types: obj.card_types.core_types.clone(),
        subtypes: obj.card_types.subtypes.clone(),
        supertypes: obj.card_types.supertypes.clone(),
        keywords: obj.keywords.clone(),
        power: obj.power,
        toughness: obj.toughness,
        colors: obj.color.clone(),
        mana_value: obj.mana_cost.mana_value(),
        controller: obj.controller,
        owner: obj.owner,
        counters: obj
            .counters
            .iter()
            .map(|(ct, n)| (ct.clone(), *n))
            .collect(),
    });

    events.push(GameEvent::CounterAdded {
        object_id,
        counter_type,
        count,
    });
}

/// CR 122.1: Apply an already-accepted counter removal, clamping to the number
/// actually present and keeping derived counter-backed characteristics in sync.
pub(crate) fn apply_counter_removal(
    state: &mut GameState,
    object_id: ObjectId,
    counter_type: CounterType,
    count: u32,
    events: &mut Vec<GameEvent>,
) {
    let Some(obj) = state.objects.get_mut(&object_id) else {
        return;
    };

    let entry = obj.counters.entry(counter_type.clone()).or_insert(0);
    let removed = (*entry).min(count);
    *entry = entry.saturating_sub(count);

    // CR 306.5c / CR 310.4c: Keep obj.loyalty / obj.defense in
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

/// CR 601.2h: Resolve a `CounterMatch` cost intent against the counters
/// currently on `object_id`, returning the concrete `CounterType` that the
/// cost will actually remove. `OfType(t)` passes through unchanged; `Any`
/// picks the type with the largest current count from the object's counter
/// map (so the cost is satisfiable iff at least one counter is present). The
/// largest-count heuristic is rules-correct for single-type permanents (Loch
/// Mare's -1/-1 only) and deterministic-enough for multi-type fallbacks
/// pending a NeedsChoice prompt for the player paying the cost to choose
/// (CR 601.2h: the player makes the choices required to pay — follow-up work).
///
/// Returns `None` when `Any` is requested but the object has no counters.
/// Callers should treat that as "skip the removal step" — the payability
/// gate (`cost_payability::counter_on_object`) already prevents activation in
/// that case, so this is defense-in-depth.
pub fn resolve_counter_match_for_removal(
    state: &GameState,
    object_id: ObjectId,
    counter_type: &crate::types::counter::CounterMatch,
) -> Option<CounterType> {
    match counter_type {
        crate::types::counter::CounterMatch::OfType(t) => Some(t.clone()),
        crate::types::counter::CounterMatch::Any => state
            .objects
            .get(&object_id)?
            .counters
            .iter()
            .filter(|(_, &n)| n > 0)
            .max_by_key(|(_, &n)| n)
            .map(|(ty, _)| ty.clone()),
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
                apply_counter_removal(state, object_id, counter_type, count, events);
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
    let (counter_type, counter_num) = match &ability.effect {
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
        _ => (CounterType::Plus1Plus1, 1),
    };

    // CR 601.2d: If distribution was assigned at cast time, apply per-target counter counts.
    if let Some(distribution) = &ability.distribution {
        for (target, count) in distribution {
            if let crate::types::ability::TargetRef::Object(obj_id) = target {
                let event_start = events.len();
                add_counter_with_replacement(
                    state,
                    ability.controller,
                    *obj_id,
                    counter_type.clone(),
                    *count,
                    events,
                );
                emit_evolved_event_for_counter_addition(
                    ability,
                    events,
                    event_start,
                    *obj_id,
                    &counter_type,
                );
            }
        }
    } else {
        let targets = resolve_defined_or_targets(state, ability);
        for obj_id in targets {
            let event_start = events.len();
            add_counter_with_replacement(
                state,
                ability.controller,
                obj_id,
                counter_type.clone(),
                counter_num,
                events,
            );
            emit_evolved_event_for_counter_addition(
                ability,
                events,
                event_start,
                obj_id,
                &counter_type,
            );
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

fn emit_evolved_event_for_counter_addition(
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    event_start: usize,
    object_id: ObjectId,
    counter_type: &CounterType,
) {
    if ability.context.ability_tag != Some(AbilityTag::Evolve)
        || *counter_type != CounterType::Plus1Plus1
    {
        return;
    }
    let evolved = events[event_start..].iter().any(|event| {
        matches!(
            event,
            GameEvent::CounterAdded {
                object_id: added_to,
                counter_type: CounterType::Plus1Plus1,
                count
            } if *added_to == object_id && *count > 0
        )
    });
    if evolved {
        events.push(GameEvent::Evolved { object_id });
    }
}

/// CR 122.1: Place counters on all battlefield objects matching a filter (no targeting).
pub fn resolve_add_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (counter_type, counter_num, target_filter) = match &ability.effect {
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
    // CR 608.2c: Bind the `TrackedSetId(0)` sentinel emitted by the parser for
    // "put a counter on each [card] this way" continuations to the highest
    // tracked set id — the set the immediately preceding effect in this chain
    // published. Empty sets are *not* skipped here (unlike
    // `targeting::resolve_tracked_set_sentinel`): a chained counter effect
    // refers to the preceding effect's set even when it ended up empty.
    let target_filter = match crate::game::effects::resolved_object_filter(ability, &target_filter)
    {
        TargetFilter::TrackedSet {
            id: crate::types::identifiers::TrackedSetId(0),
        } => state
            .tracked_object_sets
            .iter()
            .max_by_key(|(id, _)| id.0)
            .map(|(id, _)| TargetFilter::TrackedSet { id: *id })
            .unwrap_or(TargetFilter::TrackedSet {
                id: crate::types::identifiers::TrackedSetId(0),
            }),
        filter => filter,
    };

    // Collect matching IDs first to avoid borrow conflict during mutation.
    // CR 107.3a + CR 601.2b: ability-context filter evaluation.
    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    let matching_ids: Vec<crate::types::identifiers::ObjectId> =
        if let TargetFilter::TrackedSet { id } = target_filter {
            state
                .tracked_object_sets
                .get(&id)
                .cloned()
                .unwrap_or_default()
        } else {
            state
                .battlefield
                .iter()
                .filter(|id| {
                    crate::game::filter::matches_target_filter(state, **id, &target_filter, &ctx)
                })
                .copied()
                .collect()
        };

    for obj_id in matching_ids {
        add_counter_with_replacement(
            state,
            ability.controller,
            obj_id,
            counter_type.clone(),
            counter_num,
            events,
        );
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
    let (counter_type, multiplier) = match &ability.effect {
        Effect::MultiplyCounter {
            counter_type,
            multiplier,
            ..
        } => (counter_type.clone(), *multiplier as u32),
        _ => (CounterType::Plus1Plus1, 2),
    };

    let targets = resolve_defined_or_targets(state, ability);
    for obj_id in targets {
        let current = state
            .objects
            .get(&obj_id)
            .ok_or(EffectError::ObjectNotFound(obj_id))?
            .counters
            .get(&counter_type)
            .copied()
            .unwrap_or(0);
        let to_add = current.saturating_mul(multiplier).saturating_sub(current);
        if to_add > 0 {
            // CR 701.10e: doubling counters gives the permanent that many
            // additional counters, so this must flow through the central
            // counter-addition path for replacement effects and per-turn
            // "counters you've put" history.
            add_counter_with_replacement(
                state,
                ability.controller,
                obj_id,
                counter_type.clone(),
                to_add,
                events,
            );
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
    state: &GameState,
    ability: &ResolvedAbility,
) -> Vec<crate::types::identifiers::ObjectId> {
    let target_spec = match &ability.effect {
        Effect::MultiplyCounter { target, .. }
        | Effect::AddCounter { target, .. }
        | Effect::RemoveCounter { target, .. }
        | Effect::PutCounter { target, .. } => Some(target),
        _ => None,
    };

    // CR 608.2c: SelfRef is the printed-name anaphor — always resolves to the
    // source object regardless of `ability.targets`. Mirrors the post-#323
    // short-circuit in `targeting::resolved_targets`. Without this, a chained
    // `AddCounter { target: SelfRef }` sub-ability would inherit the parent's
    // targets via chain propagation in `effects::mod.rs::resolve_ability_chain`.
    if let Some(TargetFilter::SelfRef) = target_spec {
        return vec![ability.source_id];
    }

    // CR 603.10a (tier 2 of `resolved_targets`): `None` falls back to source
    // only when no chosen targets were supplied — preserves the LTB
    // self-trigger anaphor ("put a +1/+1 counter on it") while letting chain
    // propagation populate the target slot for legitimately targeted
    // sub-abilities.
    if let Some(TargetFilter::None) = target_spec {
        if ability.targets.is_empty() {
            return vec![ability.source_id];
        }
    }

    // CR 608.2k: "the exiled card" — an untargeted reference to the object
    // referred to by this ability's cost (Jhoira of the Ghitu: "Put four time
    // counters on the exiled card"). Resolved from the recursively-stamped
    // `cost_paid_object`; mirrors the `resolved_targets` chokepoint arm.
    if let Some(TargetFilter::CostPaidObject) = target_spec {
        return ability
            .cost_paid_object
            .iter()
            .map(|snap| snap.object_id)
            .collect();
    }

    if let Some(filter) = target_spec {
        let event_targets =
            crate::game::targeting::resolve_event_context_targets(state, filter, ability.source_id);
        if !event_targets.is_empty() {
            return event_targets
                .into_iter()
                .filter_map(|target| match target {
                    TargetRef::Object(id) => Some(id),
                    TargetRef::Player(_) => None,
                })
                .collect();
        }
        if ability.target_choice_timing == TargetChoiceTiming::Resolution
            && ability.targets.is_empty()
            && filter.contains_source_attachment_host()
        {
            return crate::game::targeting::resolved_object_ids_for_filter(state, ability, filter);
        }
    }

    if let Effect::MultiplyCounter { target, .. } = &ability.effect {
        if ability.targets.is_empty() {
            let effective_filter = crate::game::effects::resolved_object_filter(ability, target);
            let ctx = crate::game::filter::FilterContext::from_ability(ability);
            return state
                .battlefield_phased_in_ids()
                .into_iter()
                .filter(|id| {
                    crate::game::filter::matches_target_filter(state, *id, &effective_filter, &ctx)
                })
                .collect();
        }
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

/// CR 122.5 / CR 122.8: Read counters from source and transfer them to target.
/// True move effects remove counters from the source. "Put its counters on"
/// effects copy matching counters from source/LKI state without removal.
pub fn resolve_move(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (source_filter, counter_type_filter, count, mode, target_filter) = match &ability.effect {
        Effect::MoveCounters {
            source,
            counter_type,
            count,
            mode,
            target,
        } => (source, counter_type.as_ref(), count.as_ref(), *mode, target),
        _ => return Ok(()),
    };

    let source_ids = resolve_counter_transfer_sources(state, ability, source_filter);
    let dest_ids =
        resolve_counter_transfer_destinations(state, ability, source_filter, target_filter);

    if source_ids.is_empty() || dest_ids.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let transfer_limit = count
        .map(|expr| crate::game::quantity::resolve_quantity_with_targets(state, expr, ability))
        .map(|value| value.max(0) as u32);

    for source_id in source_ids {
        let source_counters =
            counter_transfer_source_counters(state, source_id, mode, counter_type_filter);

        if source_counters.is_empty() {
            continue;
        }

        let mut remaining = transfer_limit;
        let destinations: &[ObjectId] = if mode == CounterTransferMode::Move {
            &dest_ids[..1]
        } else {
            &dest_ids
        };

        for dest_id in destinations.iter().copied() {
            if mode == CounterTransferMode::Move && source_id == dest_id {
                continue;
            }
            for (ct, available) in &source_counters {
                let count = remaining.map_or(*available, |limit| limit.min(*available));
                if count == 0 {
                    continue;
                }
                let transferred = if mode == CounterTransferMode::Move {
                    let before = counter_count(state, source_id, ct);
                    remove_counter_with_replacement(state, source_id, ct.clone(), count, events);
                    if matches!(
                        state.waiting_for,
                        crate::types::game_state::WaitingFor::ReplacementChoice { .. }
                    ) {
                        return Ok(());
                    }
                    before.saturating_sub(counter_count(state, source_id, ct))
                } else {
                    count
                };
                if transferred == 0 {
                    continue;
                }
                add_counter_with_replacement(
                    state,
                    ability.controller,
                    dest_id,
                    ct.clone(),
                    transferred,
                    events,
                );
                if let Some(limit) = remaining.as_mut() {
                    *limit = limit.saturating_sub(transferred);
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

fn resolve_counter_transfer_sources(
    state: &GameState,
    ability: &ResolvedAbility,
    source_filter: &TargetFilter,
) -> Vec<ObjectId> {
    if matches!(source_filter, TargetFilter::SelfRef | TargetFilter::None) {
        return vec![ability.source_id];
    }

    if let Some(TargetRef::Object(id)) = crate::game::targeting::resolve_event_context_target(
        state,
        source_filter,
        ability.source_id,
    ) {
        return vec![id];
    }

    ability
        .targets
        .iter()
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .take(1)
        .collect()
}

fn resolve_counter_transfer_destinations(
    state: &GameState,
    ability: &ResolvedAbility,
    source_filter: &TargetFilter,
    target_filter: &TargetFilter,
) -> Vec<ObjectId> {
    if matches!(target_filter, TargetFilter::SelfRef | TargetFilter::None) {
        return vec![ability.source_id];
    }

    if let Some(TargetRef::Object(id)) = crate::game::targeting::resolve_event_context_target(
        state,
        target_filter,
        ability.source_id,
    ) {
        return vec![id];
    }

    let skip_source_slot = !source_filter.is_context_ref();
    ability
        .targets
        .iter()
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .skip(usize::from(skip_source_slot))
        .collect()
}

fn counter_transfer_source_counters(
    state: &GameState,
    source_id: ObjectId,
    mode: CounterTransferMode,
    counter_type_filter: Option<&CounterType>,
) -> Vec<(CounterType, u32)> {
    let mut counters = state
        .objects
        .get(&source_id)
        .map(|obj| obj.counters.clone())
        .unwrap_or_default();

    if counters.is_empty() && mode == CounterTransferMode::Put {
        counters = state
            .lki_cache
            .get(&source_id)
            .map(|lki| lki.counters.clone())
            .unwrap_or_default();
    }

    counters
        .into_iter()
        .filter(|(ct, count)| *count > 0 && counter_type_filter.is_none_or(|filter| filter == ct))
        .collect()
}

fn counter_count(state: &GameState, object_id: ObjectId, counter_type: &CounterType) -> u32 {
    state
        .objects
        .get(&object_id)
        .and_then(|obj| obj.counters.get(counter_type).copied())
        .unwrap_or(0)
}

/// Remove counters from target objects, clamping at 0.
/// CR 122.1: When counter_type is empty, removes counters of every type (Vampire Hexmage).
pub fn resolve_remove(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (counter_type, raw_count) = match &ability.effect {
        Effect::RemoveCounter {
            counter_type,
            count,
            ..
        } => (counter_type.clone(), *count),
        _ => (Some(CounterType::Plus1Plus1), 1),
    };

    let targets = resolve_defined_or_targets(state, ability);
    for obj_id in targets {
        // Build the list of (counter_type, count) pairs to remove.
        let removals: Vec<(CounterType, u32)> = if let Some(counter_type) = &counter_type {
            // CR 122.1: count == -1 means "remove all" — resolve to the actual counter count.
            let counter_num = if raw_count < 0 {
                state
                    .objects
                    .get(&obj_id)
                    .and_then(|obj| obj.counters.get(counter_type).copied())
                    .unwrap_or(0)
            } else {
                raw_count as u32
            };
            vec![(counter_type.clone(), counter_num)]
        } else {
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
                counters
            } else {
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
    use crate::types::ability::{
        ControllerRef, FilterProp, QuantityExpr, TargetChoiceTiming, TargetFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
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

    fn mark_creature(state: &mut GameState, object_id: ObjectId) {
        state
            .objects
            .get_mut(&object_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
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
                    counter_type: CounterType::Plus1Plus1,
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
    fn parameterized_power_toughness_counter_add_and_remove_marks_layers_dirty() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let counter_type = CounterType::PowerToughness {
            power: 0,
            toughness: -1,
        };
        let mut events = Vec::new();

        state.layers_dirty = false;
        apply_counter_addition(
            &mut state,
            PlayerId(0),
            obj_id,
            counter_type.clone(),
            1,
            &mut events,
        );
        assert!(state.layers_dirty);

        state.layers_dirty = false;
        apply_counter_removal(&mut state, obj_id, counter_type, 1, &mut events);
        assert!(state.layers_dirty);
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
                    counter_type: Some(CounterType::Plus1Plus1),
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
                    counter_type: CounterType::Generic("charge".to_string()),
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
                    counter_type: CounterType::Plus1Plus1,
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

    #[test]
    fn multiply_counter_records_added_counter_history() {
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
            .insert(CounterType::Plus1Plus1, 2);
        let mut events = Vec::new();

        resolve_multiply(
            &mut state,
            &make_counter_ability(
                Effect::MultiplyCounter {
                    counter_type: CounterType::Plus1Plus1,
                    multiplier: 2,
                    target: TargetFilter::Any,
                },
                obj_id,
            ),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.objects[&obj_id].counters[&CounterType::Plus1Plus1], 4);
        assert_eq!(state.counter_added_this_turn.len(), 1);
        assert_eq!(state.counter_added_this_turn[0].actor, PlayerId(0));
        assert_eq!(state.counter_added_this_turn[0].object_id, obj_id);
        assert_eq!(
            state.counter_added_this_turn[0].counter_type,
            CounterType::Plus1Plus1
        );
        assert_eq!(state.counter_added_this_turn[0].count, 2);
    }

    #[test]
    fn multiply_counter_with_no_explicit_targets_expands_filter() {
        let mut state = GameState::new_two_player(42);
        let creature_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Hydra".to_string(),
            Zone::Battlefield,
        );
        let creature_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Elf".to_string(),
            Zone::Battlefield,
        );
        let opponent_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        for id in [creature_a, creature_b, opponent_creature] {
            mark_creature(&mut state, id);
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .counters
                .insert(CounterType::Plus1Plus1, 2);
        }
        let ability = ResolvedAbility::new(
            Effect::MultiplyCounter {
                counter_type: CounterType::Plus1Plus1,
                multiplier: 2,
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        resolve_multiply(&mut state, &ability, &mut Vec::new()).unwrap();

        assert_eq!(
            state.objects[&creature_a].counters[&CounterType::Plus1Plus1],
            4
        );
        assert_eq!(
            state.objects[&creature_b].counters[&CounterType::Plus1Plus1],
            4
        );
        assert_eq!(
            state.objects[&opponent_creature].counters[&CounterType::Plus1Plus1],
            2
        );
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
                counter_type: CounterType::Plus1Plus1,
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

    #[test]
    fn put_counter_resolution_attachment_host_applies_to_equipped_creature() {
        let mut state = GameState::new_two_player(42);
        let equipment = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Blade of the Bloodchief".to_string(),
            Zone::Battlefield,
        );
        let creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Equipped Creature".to_string(),
            Zone::Battlefield,
        );
        mark_creature(&mut state, creature);
        {
            let obj = state.objects.get_mut(&equipment).unwrap();
            obj.card_types.subtypes.push("Equipment".to_string());
            obj.attached_to = Some(creature.into());
        }
        let mut ability = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(
                    TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
                ),
            },
            vec![],
            equipment,
            PlayerId(0),
        );
        ability.target_choice_timing = TargetChoiceTiming::Resolution;

        resolve_add(&mut state, &ability, &mut Vec::new()).unwrap();

        assert_eq!(
            state.objects[&creature].counters[&CounterType::Plus1Plus1],
            1
        );
        assert!(!state.objects[&equipment]
            .counters
            .contains_key(&CounterType::Plus1Plus1));
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
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: crate::types::ability::ObjectScope::Source,
                    },
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
                base_power: Some(5),
                base_toughness: Some(4),
                mana_value: 2,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                counters: lki_counters,
            },
        );

        let ability = ResolvedAbility::new(
            Effect::MoveCounters {
                source: TargetFilter::SelfRef,
                counter_type: None,
                count: None,
                mode: CounterTransferMode::Put,
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

    #[test]
    fn move_one_counter_removes_one_from_source_and_adds_one_to_target() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Tidus".to_string(),
            Zone::Battlefield,
        );
        let dest_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Ally".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 5);
        state
            .objects
            .get_mut(&dest_id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        let ability = ResolvedAbility::new(
            Effect::MoveCounters {
                source: TargetFilter::SelfRef,
                counter_type: Some(CounterType::Plus1Plus1),
                count: Some(QuantityExpr::Fixed { value: 1 }),
                mode: CounterTransferMode::Move,
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(dest_id)],
            source_id,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve_move(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects[&source_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            4
        );
        assert_eq!(
            state.objects[&dest_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            2
        );
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::CounterRemoved {
                object_id,
                counter_type: CounterType::Plus1Plus1,
                count: 1,
            } if *object_id == source_id
        )));
    }

    #[test]
    fn move_counter_uses_selected_source_target_before_destination_target() {
        let mut state = GameState::new_two_player(42);
        let ability_source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Tidus".to_string(),
            Zone::Battlefield,
        );
        let counter_source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Counter Source".to_string(),
            Zone::Battlefield,
        );
        let dest_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Counter Destination".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&counter_source_id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 2);

        let ability = ResolvedAbility::new(
            Effect::MoveCounters {
                source: TargetFilter::Any,
                counter_type: Some(CounterType::Plus1Plus1),
                count: Some(QuantityExpr::Fixed { value: 1 }),
                mode: CounterTransferMode::Move,
                target: TargetFilter::Any,
            },
            vec![
                TargetRef::Object(counter_source_id),
                TargetRef::Object(dest_id),
            ],
            ability_source_id,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve_move(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects[&counter_source_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            1
        );
        assert_eq!(
            state.objects[&dest_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            1
        );
        assert_eq!(
            state.objects[&ability_source_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0
        );
    }

    #[test]
    fn move_counter_after_target_selection_removes_from_source_and_adds_to_destination() {
        let mut state = GameState::new_two_player(42);
        let ability_source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Tidus".to_string(),
            Zone::Battlefield,
        );
        let counter_source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Counter Source".to_string(),
            Zone::Battlefield,
        );
        let dest_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Counter Destination".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&counter_source_id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 5);
        state
            .objects
            .get_mut(&dest_id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        let mut ability = ResolvedAbility::new(
            Effect::MoveCounters {
                source: TargetFilter::Any,
                counter_type: None,
                count: Some(QuantityExpr::Fixed { value: 1 }),
                mode: CounterTransferMode::Move,
                target: TargetFilter::Any,
            },
            vec![],
            ability_source_id,
            PlayerId(0),
        );
        crate::game::ability_utils::assign_selected_slots_in_chain(
            &state,
            &mut ability,
            &[
                Some(TargetRef::Object(counter_source_id)),
                Some(TargetRef::Object(dest_id)),
            ],
        )
        .expect("target selection should preserve both move-counters targets");

        let mut events = Vec::new();
        resolve_move(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects[&counter_source_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            4
        );
        assert_eq!(
            state.objects[&dest_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            2
        );
    }

    /// CR 306.5c: Adding a Loyalty counter through the resolver must keep
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
        add_counter_with_replacement(
            &mut state,
            PlayerId(0),
            pw_id,
            CounterType::Loyalty,
            1,
            &mut events,
        );

        let obj = &state.objects[&pw_id];
        assert_eq!(
            obj.counters.get(&CounterType::Loyalty).copied(),
            Some(5),
            "counter map must reflect the increment"
        );
        assert_eq!(
            obj.loyalty,
            Some(5),
            "obj.loyalty must mirror counters[Loyalty] (CR 306.5c)"
        );
    }

    /// CR 306.5c: Removing a Loyalty counter through the resolver must keep
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
        add_counter_with_replacement(
            &mut state,
            PlayerId(0),
            battle_id,
            CounterType::Defense,
            2,
            &mut events,
        );
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

    /// CR 613.1 + CR 306.5c: After the resolver syncs `obj.loyalty`, a forced
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
        add_counter_with_replacement(
            &mut state,
            PlayerId(0),
            pw_id,
            CounterType::Loyalty,
            1,
            &mut events,
        );
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
        add_counter_with_replacement(
            &mut state,
            PlayerId(0),
            pw_id,
            CounterType::Loyalty,
            1,
            &mut events,
        );
        assert_eq!(state.objects[&pw_id].loyalty, Some(5));
        assert_eq!(
            state.objects[&pw_id]
                .counters
                .get(&CounterType::Loyalty)
                .copied(),
            Some(5)
        );

        // Trigger 2 fires.
        add_counter_with_replacement(
            &mut state,
            PlayerId(0),
            pw_id,
            CounterType::Loyalty,
            1,
            &mut events,
        );
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

        let intrinsic = vec![(CounterType::Loyalty, 3u32)];
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

    /// CR 614.6 + CR 614.7 + CR 122.1: Melira's Keepers class — a permanent
    /// carrying a self-targeted `AddCounter` replacement with
    /// `QuantityModification::Prevent` must fully suppress incoming
    /// counter-placement events. The replaced event "never happens"
    /// (CR 614.6); no counters land, no `CounterAdded` event fires.
    ///
    /// Helper for the suite of Melira's Keepers tests: installs the
    /// counter-prohibition replacement on `target_id`. Returns nothing — the
    /// caller exercises `add_counter_with_replacement` directly to drive the
    /// pipeline.
    fn install_no_counters_replacement(state: &mut GameState, target_id: ObjectId) {
        use crate::types::ability::{QuantityModification, ReplacementDefinition};
        use crate::types::replacements::ReplacementEvent;
        let mut repl = ReplacementDefinition::new(ReplacementEvent::AddCounter);
        repl.valid_card = Some(TargetFilter::SelfRef);
        repl.quantity_modification = Some(QuantityModification::Prevent);
        repl.description = Some("~ can't have counters put on it.".to_string());
        state
            .objects
            .get_mut(&target_id)
            .unwrap()
            .replacement_definitions
            .push(repl);
    }

    #[test]
    fn meliras_keepers_prevents_plus1_plus1_counter_placement() {
        // CR 122.1a + CR 614.6: A +1/+1 counter is a counter (CR 122.1) — the
        // replacement must apply to ANY counter type, including +1/+1.
        let mut state = GameState::new_two_player(42);
        let keepers_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Melira's Keepers".to_string(),
            Zone::Battlefield,
        );
        install_no_counters_replacement(&mut state, keepers_id);

        let mut events = Vec::new();
        add_counter_with_replacement(
            &mut state,
            PlayerId(0),
            keepers_id,
            CounterType::Plus1Plus1,
            3,
            &mut events,
        );

        assert!(
            state.objects[&keepers_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
                == 0,
            "no +1/+1 counters may land on Melira's Keepers"
        );
    }

    #[test]
    fn meliras_keepers_prevents_minus1_minus1_counter_placement() {
        // CR 122.1a + CR 614.6: -1/-1 counters are also counters; the
        // replacement is counter-type-agnostic, so it suppresses these too.
        let mut state = GameState::new_two_player(42);
        let keepers_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Melira's Keepers".to_string(),
            Zone::Battlefield,
        );
        install_no_counters_replacement(&mut state, keepers_id);

        let mut events = Vec::new();
        add_counter_with_replacement(
            &mut state,
            PlayerId(0),
            keepers_id,
            CounterType::Minus1Minus1,
            2,
            &mut events,
        );

        assert!(
            state.objects[&keepers_id]
                .counters
                .get(&CounterType::Minus1Minus1)
                .copied()
                .unwrap_or(0)
                == 0,
            "no -1/-1 counters may land on Melira's Keepers"
        );
    }

    #[test]
    fn meliras_keepers_prevents_arbitrary_counter_types() {
        // CR 122.1 + CR 614.6: counter-agnostic — every CounterType variant
        // routes through the same `AddCounter` proposed event, so the
        // replacement suppresses charge / poison / generic counters identically.
        let mut state = GameState::new_two_player(42);
        let keepers_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Melira's Keepers".to_string(),
            Zone::Battlefield,
        );
        install_no_counters_replacement(&mut state, keepers_id);

        let mut events = Vec::new();
        // Charge counter — generic named counter, not P/T-affecting.
        add_counter_with_replacement(
            &mut state,
            PlayerId(0),
            keepers_id,
            CounterType::Generic("charge".to_string()),
            1,
            &mut events,
        );

        assert!(
            state.objects[&keepers_id].counters.is_empty(),
            "no counters of any type may land on Melira's Keepers"
        );
    }

    #[test]
    fn meliras_keepers_does_not_affect_other_creatures() {
        // CR 614.1a + TargetFilter::SelfRef: the replacement is scoped to the
        // source object only. Other creatures the same controller controls
        // receive counters normally.
        let mut state = GameState::new_two_player(42);
        let keepers_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Melira's Keepers".to_string(),
            Zone::Battlefield,
        );
        install_no_counters_replacement(&mut state, keepers_id);

        let bystander_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bystander".to_string(),
            Zone::Battlefield,
        );

        let mut events = Vec::new();
        add_counter_with_replacement(
            &mut state,
            PlayerId(0),
            bystander_id,
            CounterType::Plus1Plus1,
            2,
            &mut events,
        );

        assert_eq!(
            state.objects[&bystander_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(2),
            "the bystander must receive +1/+1 counters normally; the replacement is self-scoped"
        );
        assert!(
            state.objects[&keepers_id].counters.is_empty(),
            "Melira's Keepers must not have any counters from a placement targeting another object"
        );
    }

    #[test]
    fn meliras_keepers_replacement_filtered_when_source_off_battlefield() {
        // CR 113.6 + CR 614.1: A replacement provided by a permanent functions
        // only while that permanent is on the battlefield (or in another
        // zone-of-function that opted in via `active_zones`). When the source
        // moves to a non-battlefield zone, `find_applicable_replacements`
        // filters it out via its `zones_to_scan` gate (currently Battlefield
        // and Command) — counter placement on that very object (now in the
        // graveyard, unreachable as a counter target in practice) is no
        // longer suppressed by the SelfRef-scoped replacement.
        //
        // We exercise this by setting the source's zone to Graveyard and then
        // proposing an AddCounter event directly against the now-off-battlefield
        // object id. The applier path must skip the replacement (zone gate)
        // and the count must land.
        //
        // CR 122.2 normally erases counters on zone change, but for the
        // purpose of verifying the replacement gate we route the event
        // through the same `add_counter_with_replacement` entrypoint.
        let mut state = GameState::new_two_player(42);
        let keepers_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Melira's Keepers".to_string(),
            Zone::Battlefield,
        );
        install_no_counters_replacement(&mut state, keepers_id);

        // Sanity: while on the battlefield, counters are prevented.
        {
            let mut events = Vec::new();
            add_counter_with_replacement(
                &mut state,
                PlayerId(0),
                keepers_id,
                CounterType::Plus1Plus1,
                1,
                &mut events,
            );
            assert!(
                state.objects[&keepers_id].counters.is_empty(),
                "battlefield-resident Keepers must suppress counters (sanity check)"
            );
        }

        // Move the source out of the battlefield — the zone gate in
        // `find_applicable_replacements` (`zones_to_scan` = Battlefield +
        // Command) must now filter the replacement out.
        state.objects.get_mut(&keepers_id).unwrap().zone = Zone::Graveyard;

        let mut events = Vec::new();
        add_counter_with_replacement(
            &mut state,
            PlayerId(0),
            keepers_id,
            CounterType::Plus1Plus1,
            1,
            &mut events,
        );

        assert_eq!(
            state.objects[&keepers_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(1),
            "off-battlefield source's SelfRef replacement must not fire"
        );
    }
}
