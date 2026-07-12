use std::collections::HashSet;

use crate::game::game_object::GameObject;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    AbilityTag, CounterMoveSelection, CounterTransferMode, DelayedTriggerCondition, Duration,
    Effect, EffectError, EffectKind, QuantityExpr, ResolvedAbility, TargetChoiceTiming,
    TargetFilter, TargetRef,
};
#[cfg(test)]
use crate::types::counter::parse_counter_type;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CounterAddedRecord, CounterMoveChoice, CounterRemoveChoice, DelayedTrigger, GameState,
    PendingCounterAddition, PendingCounterAdditionQueue, PendingCounterMove,
    PendingCounterMoveQueue, PendingCounterPostAction, PendingCounterRemovalQueue,
    PendingEffectResolutionEvent, PendingEffectResolved, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{CounterMoveStage, CounterPlacement, ProposedEvent};

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
        // CR 702.32a: Fade counters likewise live only in the counter map (read
        // by the Fading upkeep removal / sacrifice triggers) — no derived field.
        // CR 702.24a: Age counters likewise live only in the counter map (read
        // by the cumulative-upkeep trigger to scale the cost) — no derived field.
        CounterType::Plus1Plus1
        | CounterType::Minus1Minus1
        | CounterType::PowerToughness { .. }
        | CounterType::Stun
        | CounterType::Lore
        | CounterType::Time
        | CounterType::Fade
        | CounterType::Age
        | CounterType::Shield
        | CounterType::Keyword(_)
        | CounterType::Generic(_) => {}
    }
}

/// Mark layers dirty if this counter type projects into a derived characteristic
/// computed by the layer system. P/T counters feed layer 7c (CR 613.4c);
/// Loyalty/Defense are cached fields mirrored from the counter map; keyword
/// counters grant abilities at layer 6 (CR 613.1f + CR 122.1b); generic
/// counters can gate static/trigger conditions (e.g. Spacecraft Station
/// thresholds) whose effects are realized by layer recomputation. Setting
/// `layers_dirty` for these is defensive — the layer reset/re-derive path is
/// idempotent when counters already match.
pub(crate) fn counter_type_affects_layers(counter_type: &CounterType) -> bool {
    // CR 613.1: Recompute the continuous-effect layer system whenever a
    // counter change can alter condition-gated effects.
    counter_type.power_toughness_delta().is_some()
        || matches!(
            counter_type,
            CounterType::Loyalty
                | CounterType::Defense
                | CounterType::Keyword(_)
                | CounterType::Generic(_)
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
) -> bool {
    if count == 0 {
        return true;
    }
    let proposed = ProposedEvent::AddCounter {
        placement: CounterPlacement::Object {
            actor,
            object_id,
            counter_type,
        },
        count,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if let ProposedEvent::AddCounter {
                placement:
                    CounterPlacement::Object {
                        actor,
                        object_id,
                        counter_type,
                    },
                count,
                ..
            } = event
            {
                apply_counter_addition(state, actor, object_id, counter_type, count, events);
            }
            true
        }
        ReplacementResult::Prevented => true,
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            false
        }
    }
}

pub(crate) fn stash_pending_counter_additions(
    state: &mut GameState,
    remaining: Vec<PendingCounterAddition>,
    completion: PendingEffectResolved,
) {
    state.pending_counter_additions = Some(PendingCounterAdditionQueue {
        remaining,
        completion: Some(completion),
    });
}

pub(crate) fn stash_pending_counter_completion(
    state: &mut GameState,
    kind: EffectKind,
    source_id: ObjectId,
) {
    stash_pending_counter_additions(
        state,
        Vec::new(),
        PendingEffectResolved::new(kind, source_id),
    );
}

pub(crate) fn stash_pending_counter_completion_with_actions(
    state: &mut GameState,
    kind: EffectKind,
    source_id: ObjectId,
    post_actions: Vec<PendingCounterPostAction>,
) {
    stash_pending_counter_additions(
        state,
        Vec::new(),
        PendingEffectResolved::with_post_actions(kind, source_id, post_actions),
    );
}

pub(crate) fn stash_pending_counter_post_actions(
    state: &mut GameState,
    kind: EffectKind,
    source_id: ObjectId,
    post_actions: Vec<PendingCounterPostAction>,
) {
    stash_pending_counter_additions(
        state,
        Vec::new(),
        PendingEffectResolved::with_post_actions_without_effect(kind, source_id, post_actions),
    );
}

pub(crate) fn append_pending_counter_post_actions(
    state: &mut GameState,
    post_actions: Vec<PendingCounterPostAction>,
) {
    if post_actions.is_empty() {
        return;
    }
    if let Some(completion) = state
        .pending_counter_additions
        .as_mut()
        .and_then(|queue| queue.completion.as_mut())
    {
        completion.post_actions.extend(post_actions);
    }
}

fn object_counter_addition(
    actor: PlayerId,
    object_id: ObjectId,
    counter_type: CounterType,
    count: u32,
) -> PendingCounterAddition {
    PendingCounterAddition::Object {
        actor,
        object_id,
        counter_type,
        count,
    }
}

fn apply_object_counter_addition(
    state: &mut GameState,
    addition: PendingCounterAddition,
    events: &mut Vec<GameEvent>,
) -> bool {
    let PendingCounterAddition::Object {
        actor,
        object_id,
        counter_type,
        count,
    } = addition
    else {
        return true;
    };
    add_counter_with_replacement(state, actor, object_id, counter_type, count, events)
}

fn merge_pending_counter_completion_after_nested_pause(
    state: &mut GameState,
    completion: PendingEffectResolved,
) {
    let Some(queue) = state.pending_counter_additions.as_mut() else {
        stash_pending_counter_additions(state, Vec::new(), completion);
        return;
    };

    let Some(nested_completion) = queue.completion.as_mut() else {
        queue.completion = Some(completion);
        return;
    };

    nested_completion
        .post_actions
        .extend(completion.post_actions);
    match completion.resolution_event {
        PendingEffectResolutionEvent::Emit => {
            nested_completion
                .post_actions
                .push(PendingCounterPostAction::EmitEffectResolved {
                    kind: completion.kind,
                    source_id: completion.source_id,
                });
        }
        PendingEffectResolutionEvent::Suppress => {}
    }
    if let Some(action) = completion.player_action {
        nested_completion
            .post_actions
            .push(PendingCounterPostAction::RecordPlayerAction {
                player_id: action.player_id,
                action: action.action,
            });
    }
}

pub(crate) fn drain_pending_counter_additions(state: &mut GameState, events: &mut Vec<GameEvent>) {
    while let Some(mut queue) = state.pending_counter_additions.take() {
        let Some(next) = queue.remaining.first().cloned() else {
            if let Some(PendingEffectResolved {
                kind,
                source_id,
                resolution_event,
                mut post_actions,
                player_action,
            }) = queue.completion.take()
            {
                while let Some(action) = post_actions.first().cloned() {
                    post_actions.remove(0);
                    if !apply_pending_counter_post_action(state, action, events) {
                        merge_pending_counter_completion_after_nested_pause(
                            state,
                            PendingEffectResolved {
                                kind,
                                source_id,
                                resolution_event,
                                post_actions,
                                player_action,
                            },
                        );
                        return;
                    }
                }
                match resolution_event {
                    PendingEffectResolutionEvent::Emit => {
                        events.push(GameEvent::EffectResolved { kind, source_id });
                    }
                    PendingEffectResolutionEvent::Suppress => {}
                }
                if let Some(action) = player_action {
                    events.push(GameEvent::PlayerPerformedAction {
                        player_id: action.player_id,
                        action: action.action,
                    });
                }
            }
            continue;
        };
        queue.remaining.remove(0);
        state.pending_counter_additions = Some(queue);
        let completed = match next {
            PendingCounterAddition::Object {
                actor,
                object_id,
                counter_type,
                count,
            } => add_counter_with_replacement(state, actor, object_id, counter_type, count, events),
            PendingCounterAddition::Player {
                actor,
                player_id,
                counter_kind,
                count,
            } => super::player_counter::add_player_counter_with_replacement(
                state,
                actor,
                player_id,
                counter_kind,
                count,
                events,
            ),
            PendingCounterAddition::Energy {
                actor,
                player_id,
                count,
            } => super::energy::add_energy_with_replacement(state, actor, player_id, count, events),
        };
        if !completed {
            return;
        }
    }
}

fn apply_pending_counter_post_action(
    state: &mut GameState,
    action: PendingCounterPostAction,
    events: &mut Vec<GameEvent>,
) -> bool {
    match action {
        PendingCounterPostAction::EmitEffectResolved { kind, source_id } => {
            events.push(GameEvent::EffectResolved { kind, source_id });
            true
        }
        PendingCounterPostAction::RecordPlayerAction { player_id, action } => {
            events.push(GameEvent::PlayerPerformedAction { player_id, action });
            true
        }
        PendingCounterPostAction::AddSubtype { object_id, subtype } => {
            if let Some(obj) = state.objects.get_mut(&object_id) {
                if !obj
                    .card_types
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(&subtype))
                {
                    obj.card_types.subtypes.push(subtype.clone());
                    obj.base_card_types.subtypes.push(subtype);
                }
            }
            true
        }
        PendingCounterPostAction::ContinueAmassAfterTokenCreation {
            controller,
            subtype,
            count,
            ability,
        } => super::amass::continue_amass_after_token_creation(
            state, controller, &subtype, count, &ability, events,
        ),
        PendingCounterPostAction::FinalizeAmass {
            object_id,
            subtype,
            ability,
        } => {
            super::amass::finalize_amass(state, object_id, &subtype, &ability, events);
            true
        }
        PendingCounterPostAction::InjectPredefinedTokenAbilities { object_id } => {
            // CR 111.10 + CR 400.7: Incubator tokens get predefined
            // subtype abilities and battlefield-entry bookkeeping after their
            // replacement-processed counters finish.
            super::token::inject_predefined_token_abilities(state, object_id);
            crate::game::layers::mark_layers_entered(state, object_id);
            crate::game::restrictions::record_battlefield_entry(state, object_id);
            crate::game::restrictions::record_token_created(state, object_id);
            // CR 603.6a: finalize the deferred ZoneChanged here, once the
            // token's counters have actually settled, so ETB trigger
            // observers (Altar of the Brood, Soul Warden, etc.) see the
            // Incubator's final counter count rather than firing early on a
            // pre-replacement-choice snapshot (issue #4238).
            if let Some(zone_change_record) = state.objects.get(&object_id).map(|obj| {
                obj.snapshot_for_zone_change(
                    object_id,
                    None,
                    crate::types::zones::Zone::Battlefield,
                )
            }) {
                state
                    .zone_changes_this_turn
                    .push(zone_change_record.clone());
                events.push(GameEvent::ZoneChanged {
                    object_id,
                    from: None,
                    to: crate::types::zones::Zone::Battlefield,
                    record: Box::new(zone_change_record),
                });
            }
            true
        }
        PendingCounterPostAction::FinalizeTokenEntry {
            object_id,
            name,
            attach_to,
            sacrifice_at,
            source_id,
            controller,
        } => {
            // CR 111.1 + CR 111.10 + CR 603.6a: once ETB counters finish,
            // complete token entry exactly as the uninterrupted token path
            // does: abilities/bookkeeping, attachment, ETB events, and any
            // delayed sacrifice trigger.
            super::token::inject_resolved_token_abilities(state, object_id);
            crate::game::layers::mark_layers_entered(state, object_id);
            crate::game::restrictions::record_battlefield_entry(state, object_id);
            crate::game::restrictions::record_token_created(state, object_id);
            if let Some(host) = attach_to {
                match host {
                    crate::game::game_object::AttachTarget::Object(id) => {
                        super::attach::attach_to(state, object_id, id);
                    }
                    crate::game::game_object::AttachTarget::Player(pid) => {
                        super::attach::attach_to_player(state, object_id, pid);
                    }
                }
            }
            push_token_entry_events(state, events, object_id, name, source_id);
            if matches!(sacrifice_at, Some(Duration::UntilEndOfCombat)) {
                state.delayed_triggers.push(DelayedTrigger {
                    condition: DelayedTriggerCondition::AtNextPhase {
                        phase: crate::types::phase::Phase::EndCombat,
                    },
                    ability: ResolvedAbility::new(
                        Effect::Sacrifice {
                            target: TargetFilter::Any,
                            count: QuantityExpr::Fixed { value: 1 },
                            min_count: 0,
                        },
                        vec![TargetRef::Object(object_id)],
                        source_id,
                        controller,
                    ),
                    controller,
                    source_id,
                    one_shot: true,
                });
            }
            state.last_created_token_ids.push(object_id);
            true
        }
        PendingCounterPostAction::ContinueTokenCreation {
            owner,
            spec,
            enter_tapped,
            remaining_count,
        } => {
            if remaining_count == 0 {
                return true;
            }
            let event = ProposedEvent::CreateToken {
                owner,
                spec,
                copy: None,
                enter_tapped,
                count: remaining_count,
                applied: HashSet::new(),
            };
            let created_ids = state.last_created_token_ids.clone();
            super::token::apply_create_token_after_replacement_with_created_ids(
                state,
                event,
                created_ids,
                PendingEffectResolutionEvent::Suppress,
                events,
            )
        }
        PendingCounterPostAction::FinalizeCopyTokenEntry {
            object_id,
            name,
            enters_attacking,
            source_id,
            controller,
        } => {
            // CR 508.4 + CR 111.1 + CR 603.6a: complete copy-token entry after
            // replacement-processed counters finish, preserving attacking
            // placement and the normal token ETB events.
            if enters_attacking {
                crate::game::combat::enter_attacking(state, object_id, source_id, controller);
            }
            super::token::inject_predefined_token_abilities(state, object_id);
            crate::game::layers::mark_layers_entered(state, object_id);
            crate::game::restrictions::record_battlefield_entry(state, object_id);
            crate::game::restrictions::record_token_created(state, object_id);
            push_token_entry_events(state, events, object_id, name, source_id);
            state.last_created_token_ids.push(object_id);
            if let Some(pending) = state.pending_copy_token_resolution.as_mut() {
                pending.created_ids.push(object_id);
            }
            true
        }
        PendingCounterPostAction::ContinueCopyTokenCreation {
            owner,
            copy,
            enter_tapped,
            enter_with_counters,
            remaining_count,
        } => {
            if remaining_count == 0 {
                return true;
            }
            let status = super::token_copy::apply_copy_token_after_replacement(
                state,
                owner,
                *copy,
                enter_tapped,
                enter_with_counters,
                remaining_count,
                events,
            );
            let completion = status.completion;
            if let Some(pending) = state.pending_copy_token_resolution.as_mut() {
                pending.created_ids.extend(status.created_ids);
            } else {
                state.last_created_token_ids.extend(status.created_ids);
            }
            match completion {
                super::token_copy::CopyTokenApplyCompletion::Completed => true,
                super::token_copy::CopyTokenApplyCompletion::Paused => false,
            }
        }
        PendingCounterPostAction::ApplyCopyTokenModificationsAndFinalize {
            object_id,
            name,
            enters_attacking,
            source_id,
            controller,
            remaining_modifications,
        } => super::token_copy::apply_remaining_token_modifications_after_counter_pause(
            state,
            object_id,
            name,
            enters_attacking,
            source_id,
            controller,
            remaining_modifications,
            events,
        ),
        action @ PendingCounterPostAction::FinalizeCommittedLiminalTokenEntry { .. } => {
            // CR 111.1 + CR 603.6a + CR 614.12a: a liminal token may have
            // been committed to the battlefield before an ETB-counter
            // replacement paused. Finish the same token-entry tail after that
            // replacement resolves so the battlefield object is not stranded
            // without abilities, entry events, or delayed cleanup.
            super::token::finalize_committed_liminal_token_entry_from_action(state, action, events)
        }
        PendingCounterPostAction::ContinueLiminalCopyTokenBatch {
            owner,
            copy,
            enter_tapped,
            enter_with_counters,
            remaining_count,
        } => super::token::continue_liminal_copy_token_batch_after_counter_pause(
            state,
            owner,
            copy,
            enter_tapped,
            enter_with_counters,
            remaining_count,
            events,
        ),
        PendingCounterPostAction::EmitCommittedCopyTokenEntry {
            object_id,
            name,
            source_id,
        } => {
            super::token::push_committed_token_entry_events(
                state, object_id, name, source_id, events,
            );
            if !state.last_created_token_ids.contains(&object_id) {
                state.last_created_token_ids.push(object_id);
            }
            if let Some(pending) = state.pending_copy_token_resolution.as_mut() {
                pending.created_ids = state.last_created_token_ids.clone();
            }
            true
        }
        PendingCounterPostAction::ClearPendingEtbCounters { object_id } => {
            state
                .pending_etb_counters
                .retain(|(pending_id, _, _)| *pending_id != object_id);
            true
        }
        PendingCounterPostAction::ContinueZoneDeliveryTail {
            object_id,
            from,
            to,
            cause,
            source_id,
            duration,
            exile_tracking,
            drain,
        } => {
            // CR 614.12a: the delivery tail may surface a Devour as-enters
            // sacrifice `EffectZoneChoice`. On that pause, return `false` so the
            // drain stashes the remaining post-actions and pauses; the tail's
            // post-effect already fired (it surfaced the choice), so the resume
            // path continues from the EffectZoneChoice resolution.
            match super::change_zone::apply_zone_delivery_tail(
                state,
                object_id,
                from,
                to,
                cause,
                source_id,
                duration.as_ref(),
                exile_tracking,
                drain,
                // CR 701.24a: the counter-pause continuation never carries a
                // library placement — library placements bear no enters-with
                // counters and never enter the battlefield, so they never reach
                // the counter-replacement pause that re-enters this tail. (A
                // placement is not a shuffle; the tail's auto-shuffle gate is moot
                // here because this path never delivers to the library.)
                None,
                events,
            ) {
                super::change_zone::ZoneDeliveryResult::Done => true,
                super::change_zone::ZoneDeliveryResult::NeedsChoice(_) => false,
            }
        }
        PendingCounterPostAction::RecordStationed {
            spacecraft_id,
            creature_id,
            counters_added,
        } => {
            // CR 702.184a: Station records the completed keyword action after
            // its replacement-processed charge counters finish.
            events.push(GameEvent::Stationed {
                spacecraft_id,
                creature_id,
                counters_added,
            });
            true
        }
        PendingCounterPostAction::MarkMonstrous { object_id } => {
            // CR 701.37a: a creature becomes monstrous after the monstrosity
            // instruction resolves, even if counter placement was modified or
            // prevented.
            if let Some(obj) = state.objects.get_mut(&object_id) {
                obj.monstrous = true;
            }
            true
        }
        PendingCounterPostAction::MarkRenowned { object_id } => {
            // CR 702.112a: a creature becomes renowned after the renown
            // instruction resolves, even if counter placement was modified or
            // prevented.
            if let Some(obj) = state.objects.get_mut(&object_id) {
                obj.is_renowned = true;
            }
            true
        }
    }
}

fn push_token_entry_events(
    state: &GameState,
    events: &mut Vec<GameEvent>,
    object_id: ObjectId,
    name: String,
    source_id: ObjectId,
) {
    let Some(obj) = state.objects.get(&object_id) else {
        return;
    };
    let zone_change_record =
        obj.snapshot_for_zone_change(object_id, None, crate::types::zones::Zone::Battlefield);
    events.push(GameEvent::ZoneChanged {
        object_id,
        from: None,
        to: crate::types::zones::Zone::Battlefield,
        record: Box::new(zone_change_record),
    });
    events.push(GameEvent::TokenCreated {
        object_id,
        name,
        source_id,
    });
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

    // CR 122.1: Drop stale zero-count keys left over from prior removals before
    // recording the object snapshot so counter history never exposes absent
    // markers as present entries.
    crate::types::counter::prune_zero_counters(&mut obj.counters);

    if counter_type_affects_layers(&counter_type) {
        state.layers_dirty.mark_full();
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
        // CR 709.4b + CR 202.3d: combined colors / mana value for a split card off
        // the stack (no-op for single-face and battlefield Rooms, which gate out).
        colors: obj.effective_colors(),
        mana_value: obj.effective_mana_value(),
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

    let was_present = obj.counters.contains_key(&counter_type);
    let entry = obj.counters.entry(counter_type.clone()).or_insert(0);
    let removed = (*entry).min(count);
    *entry = entry.saturating_sub(count);
    let is_zero = *entry == 0;

    // CR 306.5c / CR 310.4c: Keep obj.loyalty / obj.defense in
    // sync with the counter map — the field IS the counter count.
    sync_derived_from_counters(obj, &counter_type);

    // CR 122.1: Zero-count entries are normally absent — prune so proliferate
    // and other "has a counter" checks cannot resurrect removed counter types.
    //
    // EXCEPTION (CR 306.5c): loyalty is a characteristic-defining counter whose
    // field IS the counter count, and the layer system RESETS obj.loyalty to
    // base each evaluation then re-derives it from the counter map. Once the
    // last loyalty counter is pruned, that re-derive can no longer tell "drained
    // to 0" (must die, CR 704.5i) from "not counter-tracked, use the field"
    // (a clone whose loyalty comes from the Copy layer). So a genuinely-tracked
    // planeswalker drained to exactly 0 must KEEP its 0 entry — the present 0 is
    // the signal the layer re-derive needs. A phantom 0 created by `or_insert`
    // on a counter that was never present is still pruned, so un-counter-tracked
    // objects correctly fall back to their field value. (Defense needs no such
    // exception: the layer system never resets obj.defense, so a battle drained
    // to 0 keeps defense 0 without help and the CR 704.5v SBA fires normally.)
    let keep_zero = was_present && counter_type == CounterType::Loyalty && is_zero;
    crate::types::counter::prune_zero_counters(&mut obj.counters);
    if keep_zero {
        obj.counters.insert(counter_type.clone(), 0);
    }

    if counter_type_affects_layers(&counter_type) {
        state.layers_dirty.mark_full();
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

pub(crate) fn apply_counter_move_commit(
    state: &mut GameState,
    counter_move: PendingCounterMove,
    events: &mut Vec<GameEvent>,
) {
    if !counter_move_commit_is_valid(state, &counter_move) {
        return;
    }
    apply_counter_removal(
        state,
        counter_move.source_id,
        counter_move.counter_type.clone(),
        counter_move.remove_count,
        events,
    );
    apply_counter_addition(
        state,
        counter_move.actor,
        counter_move.destination_id,
        counter_move.counter_type,
        counter_move.add_count,
        events,
    );
}

fn counter_move_commit_is_valid(state: &GameState, counter_move: &PendingCounterMove) -> bool {
    counter_move.remove_count > 0
        && counter_move.add_count > 0
        && counter_move.source_id != counter_move.destination_id
        && state.objects.contains_key(&counter_move.source_id)
        && state.objects.contains_key(&counter_move.destination_id)
        && counter_count(state, counter_move.source_id, &counter_move.counter_type)
            >= counter_move.remove_count
}

pub(crate) fn apply_move_counter_after_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> bool {
    let ProposedEvent::MoveCounter {
        actor,
        source_id,
        destination_id,
        counter_type,
        remove_count,
        add_count,
        stage,
        applied: _,
    } = event
    else {
        return true;
    };

    let counter_move = PendingCounterMove {
        actor,
        source_id,
        destination_id,
        counter_type,
        remove_count,
        add_count,
    };

    match stage {
        CounterMoveStage::Remove => {
            if !counter_move_commit_is_valid(state, &counter_move) {
                return true;
            }
            let proposed = ProposedEvent::MoveCounter {
                actor: counter_move.actor,
                source_id: counter_move.source_id,
                destination_id: counter_move.destination_id,
                counter_type: counter_move.counter_type,
                remove_count: counter_move.remove_count,
                add_count: counter_move.add_count,
                stage: CounterMoveStage::Add,
                applied: HashSet::new(),
            };
            match replacement::replace_event(state, proposed, events) {
                ReplacementResult::Execute(event) => {
                    apply_move_counter_after_replacement(state, event, events)
                }
                ReplacementResult::Prevented => true,
                ReplacementResult::NeedsChoice(player) => {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    false
                }
            }
        }
        CounterMoveStage::Add => {
            apply_counter_move_commit(state, counter_move, events);
            true
        }
    }
}

pub(crate) fn move_counter_with_replacement(
    state: &mut GameState,
    actor: PlayerId,
    source_id: ObjectId,
    destination_id: ObjectId,
    counter_type: CounterType,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> bool {
    move_counter_with_replacement_entry(
        state,
        PendingCounterMove {
            actor,
            source_id,
            destination_id,
            counter_type,
            remove_count: count,
            add_count: count,
        },
        events,
    )
}

fn move_counter_with_replacement_entry(
    state: &mut GameState,
    counter_move: PendingCounterMove,
    events: &mut Vec<GameEvent>,
) -> bool {
    if counter_move.remove_count == 0
        || counter_move.add_count == 0
        || counter_move.source_id == counter_move.destination_id
    {
        return true;
    }
    if !counter_move_commit_is_valid(state, &counter_move) {
        return true;
    }
    let proposed = ProposedEvent::MoveCounter {
        actor: counter_move.actor,
        source_id: counter_move.source_id,
        destination_id: counter_move.destination_id,
        counter_type: counter_move.counter_type,
        remove_count: counter_move.remove_count,
        add_count: counter_move.add_count,
        stage: CounterMoveStage::Remove,
        applied: HashSet::new(),
    };
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            apply_move_counter_after_replacement(state, event, events)
        }
        ReplacementResult::Prevented => true,
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            false
        }
    }
}

pub(crate) fn drain_pending_counter_moves(state: &mut GameState, events: &mut Vec<GameEvent>) {
    while let Some(mut queue) = state.pending_counter_moves.take() {
        let Some(next) = queue.remaining.first().cloned() else {
            events.push(GameEvent::EffectResolved {
                kind: queue.effect_kind,
                source_id: queue.source_id,
            });
            continue;
        };
        queue.remaining.remove(0);
        state.pending_counter_moves = Some(queue);
        if !move_counter_with_replacement_entry(state, next, events) {
            return;
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
        Effect::PutCounter {
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
    let additions: Vec<PendingCounterAddition> = if let Some(distribution) = &ability.distribution {
        distribution
            .iter()
            .filter_map(|(target, count)| {
                if let crate::types::ability::TargetRef::Object(obj_id) = target {
                    Some(object_counter_addition(
                        ability.controller,
                        *obj_id,
                        counter_type.clone(),
                        *count,
                    ))
                } else {
                    None
                }
            })
            .collect()
    } else {
        let targets = resolve_defined_or_targets(state, ability);
        targets
            .into_iter()
            .map(|obj_id| {
                object_counter_addition(
                    ability.controller,
                    obj_id,
                    counter_type.clone(),
                    counter_num,
                )
            })
            .collect()
    };

    let completion =
        PendingEffectResolved::new(EffectKind::from(&ability.effect), ability.source_id);
    for (index, addition) in additions.iter().cloned().enumerate() {
        let PendingCounterAddition::Object {
            object_id, count, ..
        } = addition
        else {
            continue;
        };
        let event_start = events.len();
        if !apply_object_counter_addition(state, addition, events) {
            stash_pending_counter_additions(state, additions[index + 1..].to_vec(), completion);
            return Ok(());
        }
        if count > 0 {
            emit_evolved_event_for_counter_addition(
                ability,
                events,
                event_start,
                object_id,
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
    let (counter_type, count, counter_num_shared, target_filter) = match &ability.effect {
        Effect::PutCounterAll {
            counter_type,
            count,
            target,
        } => {
            let resolved =
                crate::game::quantity::resolve_quantity_with_targets(state, count, ability).max(0)
                    as u32;
            (
                counter_type.clone(),
                count.clone(),
                resolved,
                target.clone(),
            )
        }
        _ => return Ok(()),
    };
    // CR 608.2c: Bind the `TrackedSetId(0)` sentinel emitted by the parser for
    // "put a counter on each [card] this way" continuations to the active
    // chain tracked set. Empty sets are *not* skipped here: a chained counter
    // effect refers to the preceding effect's set even when it affected no
    // objects. Preserve that counter-specific fallback while supporting the
    // filtered "each of those <type>" intersection.
    let target_filter = match crate::game::effects::resolved_object_filter(ability, &target_filter)
    {
        TargetFilter::TrackedSet {
            id: crate::types::identifiers::TrackedSetId(0),
        } => state
            .chain_tracked_set_id
            .map(|id| TargetFilter::TrackedSet { id })
            .or_else(|| crate::game::targeting::current_combat_damage_source_filter(state))
            .or_else(|| {
                state
                    .tracked_object_sets
                    .iter()
                    .max_by_key(|(id, _)| id.0)
                    .map(|(id, _)| TargetFilter::TrackedSet { id: *id })
            })
            .unwrap_or(TargetFilter::TrackedSet {
                id: crate::types::identifiers::TrackedSetId(0),
            }),
        TargetFilter::TrackedSetFiltered {
            id: crate::types::identifiers::TrackedSetId(0),
            filter,
            caused_by,
        } => {
            if let Some(id) = state.chain_tracked_set_id {
                TargetFilter::TrackedSetFiltered {
                    id,
                    filter,
                    caused_by,
                }
            } else if let Some(source_filter) =
                crate::game::targeting::current_combat_damage_source_filter(state)
            {
                TargetFilter::And {
                    filters: vec![source_filter, *filter],
                }
            } else if let Some((&id, _)) =
                state.tracked_object_sets.iter().max_by_key(|(id, _)| id.0)
            {
                TargetFilter::TrackedSetFiltered {
                    id,
                    filter,
                    caused_by,
                }
            } else {
                TargetFilter::TrackedSetFiltered {
                    id: crate::types::identifiers::TrackedSetId(0),
                    filter,
                    caused_by,
                }
            }
        }
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

    // CR 122.1 + CR 608.2c: A per-recipient count ("each other creature you
    // control equal to THAT CREATURE's toughness" — Canopy Gargantuan) is
    // re-evaluated against each object; a uniform count (the source's power —
    // Ouroboroid) is resolved once and shared. Detected via the recipient-
    // binding scope the parser stamps on per-recipient counts.
    let count_uses_recipient = crate::game::quantity::quantity_expr_uses_recipient(&count);

    let additions: Vec<PendingCounterAddition> = matching_ids
        .into_iter()
        .map(|obj_id| {
            let counter_num = if count_uses_recipient {
                crate::game::quantity::resolve_quantity_with_recipient(
                    state,
                    &count,
                    ability.controller,
                    ability.source_id,
                    obj_id,
                )
                .max(0) as u32
            } else {
                counter_num_shared
            };
            object_counter_addition(
                ability.controller,
                obj_id,
                counter_type.clone(),
                counter_num,
            )
        })
        .collect();

    let completion =
        PendingEffectResolved::new(EffectKind::from(&ability.effect), ability.source_id);
    for (index, addition) in additions.iter().cloned().enumerate() {
        if !apply_object_counter_addition(state, addition, events) {
            stash_pending_counter_additions(state, additions[index + 1..].to_vec(), completion);
            return Ok(());
        }
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

    let mut additions = Vec::new();
    for obj_id in resolve_defined_or_targets(state, ability) {
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
            additions.push(object_counter_addition(
                ability.controller,
                obj_id,
                counter_type.clone(),
                to_add,
            ));
        }
    }

    let completion =
        PendingEffectResolved::new(EffectKind::from(&ability.effect), ability.source_id);
    for (index, addition) in additions.iter().cloned().enumerate() {
        if !apply_object_counter_addition(state, addition, events) {
            stash_pending_counter_additions(state, additions[index + 1..].to_vec(), completion);
            return Ok(());
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
        | Effect::RemoveCounter { target, .. }
        | Effect::PutCounter { target, .. } => Some(target),
        _ => None,
    };

    // Whether the ability carries any chosen *object* target. The branch
    // resolver in `choose_one_of::resolve_branch` injects a bookkeeping
    // `TargetRef::Player(chooser)` into `ability.targets`, so a plain
    // `is_empty()` check would miss the "no object target was chosen" case for a
    // `ChooseOneOf` branch. Counter placement targets objects, so the
    // source-fallback below keys off the absence of object targets, not raw
    // emptiness.
    let has_object_target = ability
        .targets
        .iter()
        .any(|t| matches!(t, TargetRef::Object(_)));

    // True only for a `ChooseOneOf` branch: `choose_one_of::resolve_branch`
    // injects a bookkeeping `TargetRef::Player(chooser)` into `ability.targets`.
    // This is the signature that distinguishes a branch lifted under a `SelfRef`
    // parent (which surfaces no object target slot) from a chain element whose
    // optional object target slot was offered and skipped (whose `targets` is
    // truly empty). See the `ParentTarget` arm below.
    let has_choice_bookkeeping_player = ability
        .targets
        .iter()
        .any(|t| matches!(t, TargetRef::Player(_)));

    // CR 608.2c: SelfRef is the printed-name anaphor — always resolves to the
    // source object regardless of `ability.targets`. Mirrors the post-#323
    // short-circuit in `targeting::resolved_targets`. Without this, a chained
    // `PutCounter { target: SelfRef }` sub-ability would inherit the parent's
    // targets via chain propagation in `effects::mod.rs::resolve_ability_chain`.
    if let Some(TargetFilter::SelfRef) = target_spec {
        return vec![ability.source_id];
    }

    // CR 608.2c (tier 2 of `resolved_targets`): `None` falls back to the source
    // object when no chosen targets were supplied — preserves the LTB
    // self-trigger anaphor ("put a +1/+1 counter on it"). Chain propagation
    // populates the slot for legitimately targeted sub-abilities, which never
    // reach this arm.
    if matches!(target_spec, Some(TargetFilter::None)) && ability.targets.is_empty() {
        return vec![ability.source_id];
    }

    // CR 608.2c: A `ParentTarget` with no object target slot resolves to the
    // source ONLY for a `ChooseOneOf` of `PutCounter` branches lifted under a
    // `TargetOnly { target: SelfRef }` parent (Reluctant Role Model: "put a
    // flying, lifelink, or +1/+1 counter on it"). The SelfRef parent surfaces
    // no target slot, so the branch's propagated `ability.targets` carries only
    // the bookkeeping `TargetRef::Player(chooser)` that `resolve_branch` injects
    // — the signature that proves no object target was ever offered.
    //
    // CR 608.2b: This must NOT fire when an optional ("up to one target") object
    // slot WAS offered and the controller chose no target (Abigale: "up to one
    // other target creature ... Put ... counters ... on that creature"). There
    // the anaphor "that creature" has no referent, so this part of the effect
    // doesn't happen and no counters are placed. That case leaves `targets`
    // truly empty (no chosen object, no injected chooser), so falling through to
    // the no-op return below is correct — the source must not gain counters.
    if matches!(target_spec, Some(TargetFilter::ParentTarget))
        && !has_object_target
        && has_choice_bookkeeping_player
    {
        return vec![ability.source_id];
    }

    // CR 608.2c + CR 122.1: `ParentTargetSlot { index }` — a later counter
    // instruction that refers to a specific earlier declared target slot ("put a
    // +1/+1 counter on the creature you control", index 0). The counter node's
    // local `ability.targets` may have been replaced with the most-recent parent
    // slot by chain propagation, so resolve against the flattened chain root
    // (single authority in `targeting`), then keep only the object at `index`.
    if let Some(TargetFilter::ParentTargetSlot { index }) = target_spec {
        return crate::game::targeting::resolve_parent_slot_from_root(state, ability, *index)
            .into_iter()
            .filter_map(|target| match target {
                TargetRef::Object(id) => Some(id),
                TargetRef::Player(_) => None,
            })
            .collect();
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
    let (source_filter, counter_type_filter, count, mode, selection, target_filter) =
        match &ability.effect {
            Effect::MoveCounters {
                source,
                counter_type,
                count,
                mode,
                selection,
                target,
            } => (
                source,
                counter_type.as_ref(),
                count.as_ref(),
                *mode,
                *selection,
                target,
            ),
            _ => return Ok(()),
        };

    let source_ids = resolve_counter_transfer_sources(state, ability, source_filter);
    if mode == CounterTransferMode::Move {
        match selection {
            CounterMoveSelection::StackTargetAnyNumber => {
                let dest_ids = resolve_counter_transfer_destinations(
                    state,
                    ability,
                    source_filter,
                    target_filter,
                );
                return resolve_stack_target_move_distribution(
                    state,
                    ability,
                    source_ids,
                    dest_ids,
                    counter_type_filter,
                    events,
                );
            }
            CounterMoveSelection::ResolutionDistributionAnyNumber => {
                return resolve_move_distribution(
                    state,
                    ability,
                    source_ids,
                    counter_type_filter,
                    target_filter,
                    events,
                );
            }
            CounterMoveSelection::StackTarget => {}
        }
    }

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

    if mode != CounterTransferMode::Move {
        // CR 122.1 / CR 122.5: Non-move counter transfers copy counters by
        // placing new counters, so each addition goes through the replacement
        // pipeline rather than the atomic move-counter path.
        let mut additions = Vec::new();
        for source_id in source_ids {
            let source_counters =
                counter_transfer_source_counters(state, source_id, mode, counter_type_filter);
            if source_counters.is_empty() {
                continue;
            }
            let mut remaining = transfer_limit;
            for dest_id in &dest_ids {
                for (ct, available) in &source_counters {
                    let count = remaining.map_or(*available, |limit| limit.min(*available));
                    if count == 0 {
                        continue;
                    }
                    additions.push(object_counter_addition(
                        ability.controller,
                        *dest_id,
                        ct.clone(),
                        count,
                    ));
                    if let Some(limit) = remaining.as_mut() {
                        *limit = limit.saturating_sub(count);
                    }
                }
            }
        }

        let completion =
            PendingEffectResolved::new(EffectKind::from(&ability.effect), ability.source_id);
        for (index, addition) in additions.iter().cloned().enumerate() {
            if !apply_object_counter_addition(state, addition, events) {
                stash_pending_counter_additions(state, additions[index + 1..].to_vec(), completion);
                return Ok(());
            }
        }
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

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
                if !move_counter_with_replacement(
                    state,
                    ability.controller,
                    source_id,
                    dest_id,
                    ct.clone(),
                    count,
                    events,
                ) {
                    return Ok(());
                }
                if let Some(limit) = remaining.as_mut() {
                    *limit = limit.saturating_sub(count);
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

fn resolve_move_distribution(
    state: &mut GameState,
    ability: &ResolvedAbility,
    source_ids: Vec<ObjectId>,
    counter_type_filter: Option<&CounterType>,
    target_filter: &TargetFilter,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Some(source_id) = source_ids.first().copied() else {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    };

    let available = counter_transfer_source_counters(
        state,
        source_id,
        CounterTransferMode::Move,
        counter_type_filter,
    );
    let destinations =
        resolution_counter_move_destinations(state, ability, target_filter, source_id);

    if available.is_empty() || destinations.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    state.waiting_for = WaitingFor::MoveCountersDistribution {
        player: ability.controller,
        source_id,
        counter_type: counter_type_filter.cloned(),
        available,
        destinations,
        pending_effect: Box::new(ability.clone()),
    };
    Ok(())
}

fn resolve_stack_target_move_distribution(
    state: &mut GameState,
    ability: &ResolvedAbility,
    source_ids: Vec<ObjectId>,
    dest_ids: Vec<ObjectId>,
    counter_type_filter: Option<&CounterType>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Some(source_id) = source_ids.first().copied() else {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    };
    let destinations: Vec<ObjectId> = dest_ids
        .into_iter()
        .filter(|dest_id| *dest_id != source_id)
        .take(1)
        .collect();
    let available = counter_transfer_source_counters(
        state,
        source_id,
        CounterTransferMode::Move,
        counter_type_filter,
    );

    if available.is_empty() || destinations.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    state.waiting_for = WaitingFor::MoveCountersDistribution {
        player: ability.controller,
        source_id,
        counter_type: counter_type_filter.cloned(),
        available,
        destinations,
        pending_effect: Box::new(ability.clone()),
    };
    Ok(())
}

fn resolution_counter_move_destinations(
    state: &GameState,
    ability: &ResolvedAbility,
    target_filter: &TargetFilter,
    source_id: ObjectId,
) -> Vec<ObjectId> {
    let effective_filter = crate::game::effects::resolved_object_filter(ability, target_filter);
    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    state
        .battlefield_phased_in_ids()
        .into_iter()
        .filter(|id| *id != source_id)
        .filter(|id| {
            crate::game::filter::matches_target_filter(state, *id, &effective_filter, &ctx)
        })
        .collect()
}

pub(crate) fn validate_and_queue_counter_move_distribution(
    state: &mut GameState,
    selections: &[CounterMoveChoice],
    source_id: ObjectId,
    available: &[(CounterType, u32)],
    destinations: &[ObjectId],
    pending_effect: &ResolvedAbility,
) -> Result<(), EffectError> {
    let mut seen_choices = HashSet::new();
    let mut requested_by_type: Vec<(CounterType, u32)> = Vec::new();
    let mut moves = Vec::new();

    for selection in selections {
        if selection.count == 0 {
            return Err(EffectError::InvalidParam(
                "counter move selections must have positive counts".to_string(),
            ));
        }
        if !destinations.contains(&selection.destination_id) {
            return Err(EffectError::InvalidParam(
                "counter move destination is not legal".to_string(),
            ));
        }
        if !seen_choices.insert((selection.destination_id, selection.counter_type.clone())) {
            return Err(EffectError::InvalidParam(
                "counter move destination and counter type pairs must be unique".to_string(),
            ));
        }

        if let Some((_, total)) = requested_by_type
            .iter_mut()
            .find(|(ct, _)| *ct == selection.counter_type)
        {
            *total = total.saturating_add(selection.count);
        } else {
            requested_by_type.push((selection.counter_type.clone(), selection.count));
        }

        moves.push(PendingCounterMove {
            actor: pending_effect.controller,
            source_id,
            destination_id: selection.destination_id,
            counter_type: selection.counter_type.clone(),
            remove_count: selection.count,
            add_count: selection.count,
        });
    }

    for (counter_type, requested) in requested_by_type {
        let available_count = available
            .iter()
            .find(|(ct, _)| *ct == counter_type)
            .map(|(_, count)| *count)
            .unwrap_or(0);
        if requested > available_count {
            return Err(EffectError::InvalidParam(
                "counter move request exceeds available counters".to_string(),
            ));
        }
    }

    state.pending_counter_moves = Some(PendingCounterMoveQueue {
        remaining: moves,
        effect_kind: EffectKind::from(&pending_effect.effect),
        source_id: pending_effect.source_id,
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
    // CR 107.1c + CR 608.2d: "remove any number of counters" is a resolution-time
    // interactive choice; the parser encodes it as `UpTo { Fixed{-1} }`.
    // Discriminate on the peel FLAG (never on the scalar): if the count wrapper is
    // present, route to the interactive per-type selection. We MUST NOT numerically
    // resolve the inner `Fixed{-1}` — the board-derived per-type `available` counts
    // ARE the legal domain (each type 0..=available; total 0..=Σ, incl. zero,
    // CR 107.1c). Resolving the scalar would collapse into the non-interactive
    // "remove all" branch below and skip the player's choice.
    if let Effect::RemoveCounter {
        count,
        counter_type,
        ..
    } = &ability.effect
    {
        if count.is_up_to() {
            return resolve_remove_interactive(state, ability, counter_type.clone(), events);
        }
    }

    let (counter_type, raw_count) = match &ability.effect {
        Effect::RemoveCounter {
            counter_type,
            count,
            ..
        } => {
            // CR 122.1: Resolve the count against game state so dynamic amounts
            // compose — "remove that many +1/+1 counters" (Protean Hydra class)
            // picks up the prevented-damage amount via `EventContextAmount`.
            // The `-1` "remove all" sentinel survives resolution as `Fixed{-1}`
            // and is keyed off `< 0` below, exactly as before.
            let resolved =
                crate::game::quantity::resolve_quantity_with_targets(state, count, ability);
            (counter_type.clone(), resolved)
        }
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

/// CR 107.1c + CR 608.2d: Resolve "remove any number of counters from [source]"
/// as a resolution-time interactive choice. Derives the public per-type counter
/// budget from the single removal source (the ability's target for Rhys, the
/// Evermore; `SelfRef` for Tetravus) and raises `WaitingFor::RemoveCountersChoice`
/// so the controller picks any per-type subset (0..=available, incl. the empty
/// set). When no counters are available the only legal selection is empty, so we
/// resolve immediately with `last_effect_count = Some(0)` (CR 608.2h) and no
/// prompt.
///
/// ponytail: single-source only — multi-source "from among" removals (Galloping
/// Lizrog, Eventide's Shadow) are out of scope and keep hitting the parser's
/// existing paths.
fn resolve_remove_interactive(
    state: &mut GameState,
    ability: &ResolvedAbility,
    counter_type: Option<CounterType>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Some(source_id) = resolve_defined_or_targets(state, ability)
        .into_iter()
        .next()
    else {
        // No legal source (target left the battlefield, etc.) — finish cleanly.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    };

    // CR 122.1: derive the public per-type counts on the source, honoring the
    // effect's counter-type filter (`Some` → that single type; `None` → every
    // type present, e.g. Rhys "any number of counters").
    let available: Vec<(CounterType, u32)> = state
        .objects
        .get(&source_id)
        .map(|obj| {
            obj.counters
                .iter()
                .filter(|(ct, &v)| {
                    v > 0 && counter_type.as_ref().is_none_or(|filter| filter == *ct)
                })
                .map(|(ct, &v)| (ct.clone(), v))
                .collect()
        })
        .unwrap_or_default();

    // CR 107.1c: "any number" includes zero — an empty board means the only legal
    // choice is the empty set. Resolve without a prompt and stamp 0 so a
    // downstream "create that many" rider (Tetravus) reads 0, not a stale count.
    if available.is_empty() {
        state.last_effect_count = Some(0);
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    state.waiting_for = WaitingFor::RemoveCountersChoice {
        player: ability.controller,
        source_id,
        counter_type,
        available,
        pending_effect: Box::new(ability.clone()),
    };
    Ok(())
}

/// CR 107.1c: Validate a submitted "remove any number of counters" selection
/// against the per-type `available` budget and return the total requested.
///
/// Shared single authority for the per-type constraints of both the effect-path
/// handler (`RemoveCountersChoice`) and the cost-path handler
/// (`handle_remove_counter_distribution_for_cost`, which projects its per-object
/// distribution to per-type first). Enforces: every selected type exists in
/// `available`, per entry `count <= available[type]`, positive counts, and no
/// duplicate type. The empty selection (remove zero) is legal, and omitting an
/// available type is legal.
pub(crate) fn validate_counter_selection(
    available: &[(CounterType, u32)],
    selections: &[CounterRemoveChoice],
) -> Result<u32, EffectError> {
    let mut seen = HashSet::new();
    let mut total = 0u32;
    for selection in selections {
        if selection.count == 0 {
            return Err(EffectError::InvalidParam(
                "counter removal selections must have positive counts".to_string(),
            ));
        }
        if !seen.insert(selection.counter_type.clone()) {
            return Err(EffectError::InvalidParam(
                "counter removal selections must have distinct counter types".to_string(),
            ));
        }
        let available_count = available
            .iter()
            .find(|(ct, _)| *ct == selection.counter_type)
            .map(|(_, count)| *count)
            .unwrap_or(0);
        if selection.count > available_count {
            return Err(EffectError::InvalidParam(
                "counter removal request exceeds available counters".to_string(),
            ));
        }
        total = total.saturating_add(selection.count);
    }
    Ok(total)
}

/// CR 107.1c: Validate a submitted `RemoveCountersChoice` answer and stash the
/// per-type removals into `pending_counter_removals` for
/// `drain_pending_counter_removals` to apply. Mirrors
/// `validate_and_queue_counter_move_distribution` so the `apply()` handler stays
/// a thin dispatcher.
pub(crate) fn validate_and_queue_counter_removal(
    state: &mut GameState,
    selections: &[CounterRemoveChoice],
    source_id: ObjectId,
    available: &[(CounterType, u32)],
    pending_effect: &ResolvedAbility,
) -> Result<(), EffectError> {
    let total = validate_counter_selection(available, selections)?;
    let remaining: Vec<(CounterType, u32)> = selections
        .iter()
        .map(|s| (s.counter_type.clone(), s.count))
        .collect();
    state.pending_counter_removals = Some(PendingCounterRemovalQueue {
        remaining,
        source_id,
        effect_kind: EffectKind::from(&pending_effect.effect),
        source_ability_id: pending_effect.source_id,
        total,
    });
    Ok(())
}

/// CR 107.1c + CR 608.2h: Drain the pending "remove any number of counters"
/// selection one `(counter_type, count)` entry at a time through the
/// single-authority remove pipeline so prevention/modification replacements
/// apply. Mirrors `drain_pending_counter_moves`: re-parks the queue (returning
/// early) when a per-removal replacement surfaces a `ReplacementChoice`, and when
/// the queue empties stamps `last_effect_count = total` BEFORE emitting
/// `EffectResolved` so a downstream "create that many" / "add that much" rider
/// reading `QuantityRef::EventContextAmount` picks up the removed count.
pub(crate) fn drain_pending_counter_removals(state: &mut GameState, events: &mut Vec<GameEvent>) {
    while let Some(mut queue) = state.pending_counter_removals.take() {
        let Some((counter_type, count)) = queue.remaining.first().cloned() else {
            // CR 608.2h: ordering invariant — stamp the total removed before the
            // terminating EffectResolved (and thus before the continuation drains).
            state.last_effect_count = Some(queue.total as i32);
            events.push(GameEvent::EffectResolved {
                kind: queue.effect_kind,
                source_id: queue.source_ability_id,
            });
            continue;
        };
        queue.remaining.remove(0);
        let source_id = queue.source_id;
        state.pending_counter_removals = Some(queue);
        // CR 614.1: single-authority remove pipeline (applies prevention /
        // modification replacements; keeps obj.loyalty / obj.defense in lockstep).
        remove_counter_with_replacement(state, source_id, counter_type, count, events);
        // If a replacement needs a player choice, suspend — the ReplacementChoice
        // resume path re-invokes this drain to finish the remaining removals.
        if matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ControllerRef, FilterProp, QuantityExpr, QuantityModification, ReplacementDefinition,
        TargetChoiceTiming, TargetFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::Zone;

    fn make_counter_ability(effect: Effect, target: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            effect,
            vec![TargetRef::Object(target)],
            ObjectId(100),
            PlayerId(0),
        )
    }

    /// T4 (counter resolver arm) — CR 608.2c + CR 122.1: a `PutCounter` whose
    /// target is `ParentTargetSlot { index }` resolves against the FLATTENED
    /// CHAIN ROOT (from `resolving_stack_entry`), not the node's local targets.
    /// The node's own `targets` here carry only the most-recent parent slot
    /// `[obj1]` (the model-B propagation the arm corrects); slot 0 must still
    /// resolve to `obj0`. Reverting the arm falls through to the local `[obj1]`
    /// for BOTH indices, so the `index: 0 → [obj0]` assertion flips.
    #[test]
    fn resolve_defined_or_targets_parent_target_slot_indexes_chain_root() {
        use crate::types::game_state::{StackEntry, StackEntryKind};

        let mut state = GameState::new_two_player(42);
        let source = ObjectId(99);
        let obj0 = ObjectId(1);
        let obj1 = ObjectId(2);

        // Root two-slot chain: TargetOnly(obj0) → TargetOnly(obj1).
        let root = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(obj0)],
            source,
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(obj1)],
            source,
            PlayerId(0),
        ));
        state.resolving_stack_entry = Some(StackEntry {
            id: ObjectId(500),
            source_id: source,
            controller: PlayerId(0),
            kind: StackEntryKind::ActivatedAbility {
                source_id: source,
                ability: root,
            },
        });

        let put_counter = |index: usize| {
            ResolvedAbility::new(
                Effect::PutCounter {
                    counter_type: crate::types::counter::CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::ParentTargetSlot { index },
                },
                // Local targets = most-recent slot only (model-B inheritance).
                vec![TargetRef::Object(obj1)],
                source,
                PlayerId(0),
            )
        };

        assert_eq!(
            resolve_defined_or_targets(&state, &put_counter(0)),
            vec![obj0]
        );
        assert_eq!(
            resolve_defined_or_targets(&state, &put_counter(1)),
            vec![obj1]
        );
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

    fn install_noncommuting_counter_replacements(state: &mut GameState) {
        let doubler_id = create_object(
            state,
            CardId(900),
            PlayerId(0),
            "Counter Doubler".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&doubler_id)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::AddCounter)
                    .quantity_modification(QuantityModification::DOUBLE),
            );

        let plus_id = create_object(
            state,
            CardId(901),
            PlayerId(0),
            "Counter Plus".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&plus_id)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::AddCounter)
                    .quantity_modification(QuantityModification::Plus { value: 1 }),
            );
    }

    /// Issue #1675 — Canopy Gargantuan: "put a number of +1/+1 counters on each
    /// other creature you control equal to THAT CREATURE's toughness." Each
    /// other creature must receive counters equal to ITS OWN toughness (the
    /// count is re-evaluated per recipient), the source is excluded ("Another"),
    /// and an opponent's creature receives none ("you control").
    #[test]
    fn put_counter_all_per_recipient_toughness() {
        use crate::types::ability::{ObjectScope, QuantityRef};

        let mut state = GameState::new_two_player(42);

        // Canopy Gargantuan (the source).
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Canopy Gargantuan".to_string(),
            Zone::Battlefield,
        );
        mark_creature(&mut state, source);
        {
            let o = state.objects.get_mut(&source).unwrap();
            o.toughness = Some(7);
            o.base_toughness = Some(7);
        }

        // Three OTHER creatures you control with distinct toughness.
        let others: Vec<(ObjectId, i32)> = [(2u64, 3i32), (3, 5), (4, 1)]
            .into_iter()
            .map(|(cid, tough)| {
                let id = create_object(
                    &mut state,
                    CardId(cid),
                    PlayerId(0),
                    format!("Creature {cid}"),
                    Zone::Battlefield,
                );
                mark_creature(&mut state, id);
                let o = state.objects.get_mut(&id).unwrap();
                o.toughness = Some(tough);
                o.base_toughness = Some(tough);
                (id, tough)
            })
            .collect();

        // An opponent's creature — must NOT receive counters ("you control").
        let opp = create_object(
            &mut state,
            CardId(9),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        mark_creature(&mut state, opp);
        {
            let o = state.objects.get_mut(&opp).unwrap();
            o.toughness = Some(4);
            o.base_toughness = Some(4);
        }

        let ability = ResolvedAbility::new(
            Effect::PutCounterAll {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Toughness {
                        scope: ObjectScope::Recipient,
                    },
                },
                target: TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::Another]),
                ),
            },
            vec![],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve_add_all(&mut state, &ability, &mut events).unwrap();

        // Each OTHER creature you control gains counters equal to ITS OWN toughness.
        for (id, tough) in &others {
            assert_eq!(
                state.objects[id]
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0),
                *tough as u32,
                "creature with toughness {tough} must receive {tough} +1/+1 counters"
            );
        }
        // Source ("Another") and the opponent's creature ("you control") get none.
        assert!(
            !state.objects[&source]
                .counters
                .contains_key(&CounterType::Plus1Plus1),
            "source must be excluded by Another"
        );
        assert!(
            !state.objects[&opp]
                .counters
                .contains_key(&CounterType::Plus1Plus1),
            "opponent's creature must be excluded by 'you control'"
        );
    }

    /// Issue #588 (Summon: Good King Mog XII, chapter IV): runtime filter
    /// evaluation must honor Moogle subtype + you control + Another — not
    /// blanket every other permanent when the subtype was unknown at parse time.
    #[test]
    fn resolve_add_all_each_other_moogle_you_control_issue_588() {
        let mut state = GameState::new_two_player(42);

        let source = {
            let id = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Good King Mog XII".to_string(),
                Zone::Battlefield,
            );
            mark_creature(&mut state, id);
            state.objects.get_mut(&id).unwrap().card_types.subtypes =
                vec!["Moogle".to_string(), "Saga".to_string()];
            id
        };

        let ally_moogle = {
            let id = create_object(
                &mut state,
                CardId(2),
                PlayerId(0),
                "Moogle Ally".to_string(),
                Zone::Battlefield,
            );
            mark_creature(&mut state, id);
            state.objects.get_mut(&id).unwrap().card_types.subtypes = vec!["Moogle".to_string()];
            id
        };

        let non_moogle = {
            let id = create_object(
                &mut state,
                CardId(3),
                PlayerId(0),
                "Grizzly Bears".to_string(),
                Zone::Battlefield,
            );
            mark_creature(&mut state, id);
            id
        };

        let opp_moogle = {
            let id = create_object(
                &mut state,
                CardId(4),
                PlayerId(1),
                "Opponent Moogle".to_string(),
                Zone::Battlefield,
            );
            mark_creature(&mut state, id);
            state.objects.get_mut(&id).unwrap().card_types.subtypes = vec!["Moogle".to_string()];
            id
        };

        let land = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Sunlit Marsh".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let ability = ResolvedAbility::new(
            Effect::PutCounterAll {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Typed(
                    TypedFilter::default()
                        .subtype("Moogle".to_string())
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::Another]),
                ),
            },
            vec![],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve_add_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects[&ally_moogle]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            2,
            "other Moogle you control receives two +1/+1 counters"
        );
        assert!(
            !state.objects[&source]
                .counters
                .contains_key(&CounterType::Plus1Plus1),
            "source excluded by Another"
        );
        assert!(
            !state.objects[&non_moogle]
                .counters
                .contains_key(&CounterType::Plus1Plus1),
            "non-Moogle creature excluded by subtype filter"
        );
        assert!(
            !state.objects[&opp_moogle]
                .counters
                .contains_key(&CounterType::Plus1Plus1),
            "opponent Moogle excluded by you control"
        );
        assert!(
            !state.objects[&land]
                .counters
                .contains_key(&CounterType::Plus1Plus1),
            "land excluded — not a Moogle creature"
        );
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
                Effect::PutCounter {
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

        state.layers_dirty = crate::types::game_state::LayersDirty::Clean;
        apply_counter_addition(
            &mut state,
            PlayerId(0),
            obj_id,
            counter_type.clone(),
            1,
            &mut events,
        );
        assert!(state.layers_dirty.is_dirty());

        state.layers_dirty = crate::types::game_state::LayersDirty::Clean;
        apply_counter_removal(&mut state, obj_id, counter_type, 1, &mut events);
        assert!(state.layers_dirty.is_dirty());
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
                    count: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::Any,
                },
                obj_id,
            ),
            &mut events,
        )
        .unwrap();

        assert!(
            !state.objects[&obj_id]
                .counters
                .contains_key(&CounterType::Plus1Plus1),
            "zero-count +1/+1 entry should be pruned after removal"
        );
    }

    #[test]
    fn apply_counter_removal_prunes_zero_entry() {
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
            .insert(CounterType::Generic("charge".to_string()), 1);
        let mut events = Vec::new();

        apply_counter_removal(
            &mut state,
            obj_id,
            CounterType::Generic("charge".to_string()),
            1,
            &mut events,
        );

        assert!(
            state.objects[&obj_id].counters.is_empty(),
            "last charge counter removed should leave an empty map"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::CounterRemoved {
                counter_type: CounterType::Generic(_),
                count: 1,
                ..
            }
        )));
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
                Effect::PutCounter {
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
                Effect::PutCounter {
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
    fn add_counter_replacement_choice_stashes_remaining_targets_and_completion() {
        let mut state = GameState::new_two_player(42);
        install_noncommuting_counter_replacements(&mut state);
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First Creature".to_string(),
            Zone::Battlefield,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second Creature".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(first), TargetRef::Object(second)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_add(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        let pending = state
            .pending_counter_additions
            .as_ref()
            .expect("remaining target should be queued");
        assert_eq!(pending.remaining.len(), 1);
        assert!(matches!(
            pending.remaining[0],
            PendingCounterAddition::Object {
                object_id,
                counter_type: CounterType::Plus1Plus1,
                count: 1,
                ..
            } if object_id == second
        ));
        assert!(matches!(
            pending.completion,
            Some(PendingEffectResolved {
                kind: EffectKind::PutCounter,
                source_id: ObjectId(100),
                player_action: None,
                ..
            })
        ));
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::EffectResolved { .. })));
    }

    #[test]
    fn nested_post_action_pause_preserves_parent_completion() {
        let mut state = GameState::new_two_player(42);
        state.pending_counter_additions = Some(PendingCounterAdditionQueue {
            remaining: vec![PendingCounterAddition::Object {
                actor: PlayerId(0),
                object_id: ObjectId(10),
                counter_type: CounterType::Plus1Plus1,
                count: 1,
            }],
            completion: Some(PendingEffectResolved::with_post_actions_without_effect(
                EffectKind::Token,
                ObjectId(20),
                vec![PendingCounterPostAction::MarkRenowned {
                    object_id: ObjectId(30),
                }],
            )),
        });

        merge_pending_counter_completion_after_nested_pause(
            &mut state,
            PendingEffectResolved::with_post_actions(
                EffectKind::PutCounter,
                ObjectId(40),
                vec![PendingCounterPostAction::MarkMonstrous {
                    object_id: ObjectId(50),
                }],
            ),
        );

        let queue = state
            .pending_counter_additions
            .as_ref()
            .expect("nested queue remains installed");
        assert_eq!(queue.remaining.len(), 1);
        let completion = queue
            .completion
            .as_ref()
            .expect("nested completion remains installed");
        assert_eq!(completion.kind, EffectKind::Token);
        assert_eq!(
            completion.resolution_event,
            PendingEffectResolutionEvent::Suppress
        );
        assert!(matches!(
            completion.post_actions.as_slice(),
            [
                PendingCounterPostAction::MarkRenowned {
                    object_id: ObjectId(30)
                },
                PendingCounterPostAction::MarkMonstrous {
                    object_id: ObjectId(50)
                },
                PendingCounterPostAction::EmitEffectResolved {
                    kind: EffectKind::PutCounter,
                    source_id: ObjectId(40)
                }
            ]
        ));
    }

    #[test]
    fn add_all_counter_replacement_choice_stashes_remaining_objects_and_completion() {
        let mut state = GameState::new_two_player(42);
        install_noncommuting_counter_replacements(&mut state);
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First Creature".to_string(),
            Zone::Battlefield,
        );
        mark_creature(&mut state, first);
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second Creature".to_string(),
            Zone::Battlefield,
        );
        mark_creature(&mut state, second);
        let ability = ResolvedAbility::new(
            Effect::PutCounterAll {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_add_all(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        let pending = state
            .pending_counter_additions
            .as_ref()
            .expect("remaining object should be queued");
        assert_eq!(pending.remaining.len(), 1);
        assert!(matches!(
            pending.remaining[0],
            PendingCounterAddition::Object {
                object_id,
                counter_type: CounterType::Plus1Plus1,
                count: 1,
                ..
            } if object_id == second
        ));
        assert!(matches!(
            pending.completion,
            Some(PendingEffectResolved {
                kind: EffectKind::PutCounterAll,
                source_id: ObjectId(100),
                player_action: None,
                ..
            })
        ));
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::EffectResolved { .. })));
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
    fn multiply_counter_replacement_choice_stashes_remaining_targets_and_completion() {
        let mut state = GameState::new_two_player(42);
        install_noncommuting_counter_replacements(&mut state);
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First Creature".to_string(),
            Zone::Battlefield,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second Creature".to_string(),
            Zone::Battlefield,
        );
        for obj_id in [first, second] {
            state
                .objects
                .get_mut(&obj_id)
                .unwrap()
                .counters
                .insert(CounterType::Plus1Plus1, 1);
        }
        let ability = ResolvedAbility::new(
            Effect::MultiplyCounter {
                counter_type: CounterType::Plus1Plus1,
                multiplier: 2,
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(first), TargetRef::Object(second)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_multiply(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        let pending = state
            .pending_counter_additions
            .as_ref()
            .expect("remaining target should be queued");
        assert_eq!(pending.remaining.len(), 1);
        assert!(matches!(
            pending.remaining[0],
            PendingCounterAddition::Object {
                object_id,
                counter_type: CounterType::Plus1Plus1,
                count: 1,
                ..
            } if object_id == second
        ));
        assert!(matches!(
            pending.completion,
            Some(PendingEffectResolved {
                kind: EffectKind::MultiplyCounter,
                source_id: ObjectId(100),
                player_action: None,
                ..
            })
        ));
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::EffectResolved { .. })));
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
        assert!(
            state.layers_dirty.is_dirty(),
            "layers must be dirtied for P/T counter"
        );
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

    /// CR 122.1 + CR 603.4 + CR 603.10a: Drizzt Do'Urden — "Whenever a creature
    /// dies, if it had power greater than Drizzt's power, put a number of +1/+1
    /// counters on Drizzt equal to the difference." End-to-end through the real
    /// parser: a larger-power creature dying gates the trigger on and puts
    /// `dyingPower - drizztPower` counters (read from LKI, CR 603.10a); an
    /// equal/smaller creature fails the gate and adds none. Fails on revert
    /// (parser leaves the effect Unimplemented / drops the gate → 0 counters).
    #[test]
    fn drizzt_difference_counters_from_dying_creature_lki_power() {
        use crate::game::stack::resolve_top;
        use crate::game::triggers::process_triggers;
        use crate::types::triggers::TriggerMode;

        // Parse Drizzt's dies trigger from Oracle text (real pipeline).
        let parsed = crate::parser::parse_oracle_text(
            "Double strike\n\
             Whenever a creature dies, if it had power greater than Drizzt's power, \
             put a number of +1/+1 counters on Drizzt equal to the difference.",
            "Drizzt Do'Urden",
            &[],
            &["Creature".to_string()],
            &["Elf".to_string(), "Ranger".to_string()],
        );
        let dies_trigger = parsed
            .triggers
            .iter()
            .find(|t| {
                matches!(t.mode, TriggerMode::ChangesZone)
                    && t.execute
                        .as_ref()
                        .is_some_and(|e| matches!(&*e.effect, Effect::PutCounter { .. }))
            })
            .unwrap_or_else(|| panic!("Drizzt dies PutCounter trigger not parsed: {parsed:#?}"))
            .clone();

        // Run the dies scenario with a creature of the given power; return the
        // number of +1/+1 counters Drizzt ends up with.
        let run = |dying_power: i32| -> u32 {
            let mut state = GameState::new_two_player(42);

            let drizzt_id = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Drizzt Do'Urden".to_string(),
                Zone::Battlefield,
            );
            {
                let d = state.objects.get_mut(&drizzt_id).unwrap();
                d.power = Some(2);
                d.toughness = Some(3);
                d.card_types.core_types.push(CoreType::Creature);
                d.trigger_definitions.push(dies_trigger.clone());
            }

            let dying_id = create_object(
                &mut state,
                CardId(2),
                PlayerId(1),
                "Hill Giant".to_string(),
                Zone::Battlefield,
            );
            {
                let g = state.objects.get_mut(&dying_id).unwrap();
                g.power = Some(dying_power);
                g.toughness = Some(3);
                g.card_types.core_types.push(CoreType::Creature);
            }

            let mut events = Vec::new();
            crate::game::zones::move_to_zone(&mut state, dying_id, Zone::Graveyard, &mut events);
            process_triggers(&mut state, &events);
            while !state.stack.is_empty() {
                let mut resolve_events = Vec::new();
                resolve_top(&mut state, &mut resolve_events);
            }

            state.objects[&drizzt_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
        };

        // Larger power (5) than Drizzt (2): gate passes, +1/+1 counters = 5 - 2 = 3.
        assert_eq!(
            run(5),
            3,
            "5-power creature dying should give Drizzt 3 (=5-2) +1/+1 counters"
        );
        // Equal power (2): strict GT gate fails, no counters.
        assert_eq!(run(2), 0, "equal-power creature must not add counters");
        // Smaller power (1): gate fails, no counters.
        assert_eq!(run(1), 0, "smaller-power creature must not add counters");
    }

    /// #5253 (Railway Brawler): "Whenever another creature you control enters,
    /// put X +1/+1 counters on it, where X is its power." End-to-end through the
    /// real parser: an entering creature receives +1/+1 counters equal to ITS
    /// OWN power, not Railway Brawler's. The source and entering creature have
    /// DIFFERENT power so the referent is observable. Reverting the enters lift
    /// leaves the count `scope: Source`, reading Railway Brawler's power (the
    /// #5253 bug) — this assertion goes red.
    #[test]
    fn railway_brawler_counters_entering_creature_by_its_own_power() {
        use crate::game::stack::resolve_top;
        use crate::game::triggers::process_triggers;
        use crate::types::card_type::CoreType;
        use crate::types::triggers::TriggerMode;

        let parsed = crate::parser::parse_oracle_text(
            "Reach, trample\n\
             Whenever another creature you control enters, put X +1/+1 counters on it, \
             where X is its power.",
            "Railway Brawler",
            &[],
            &["Creature".to_string()],
            &["Dinosaur".to_string()],
        );
        let enters_trigger = parsed
            .triggers
            .iter()
            .find(|t| {
                matches!(t.mode, TriggerMode::ChangesZone)
                    && t.execute
                        .as_ref()
                        .is_some_and(|e| matches!(&*e.effect, Effect::PutCounter { .. }))
            })
            .unwrap_or_else(|| {
                panic!("Railway Brawler enters PutCounter trigger not parsed: {parsed:#?}")
            })
            .clone();

        let mut state = GameState::new_two_player(42);

        // Railway Brawler on the battlefield, controller P0, power 5.
        let brawler_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Railway Brawler".to_string(),
            Zone::Battlefield,
        );
        {
            let b = state.objects.get_mut(&brawler_id).unwrap();
            b.power = Some(5);
            b.toughness = Some(5);
            b.card_types.core_types.push(CoreType::Creature);
            b.trigger_definitions.push(enters_trigger);
        }

        // Another creature P0 controls, power 3, currently off the battlefield.
        let entering_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        {
            let e = state.objects.get_mut(&entering_id).unwrap();
            e.power = Some(3);
            e.toughness = Some(3);
            e.card_types.core_types.push(CoreType::Creature);
        }

        // The creature enters the battlefield → fires Railway Brawler's trigger.
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, entering_id, Zone::Battlefield, &mut events);
        process_triggers(&mut state, &events);
        while !state.stack.is_empty() {
            let mut resolve_events = Vec::new();
            resolve_top(&mut state, &mut resolve_events);
        }

        // The entering creature gets counters equal to ITS OWN power (3), NOT
        // Railway Brawler's power (5).
        assert_eq!(
            state.objects[&entering_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            3,
            "entering creature must get +1/+1 counters = its own power (3), not the source's (5)"
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
                token_image_ref: None,
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
                chosen_attributes: Vec::new(),
                counters: lki_counters,
                tapped: false,
                is_suspected: false,
            },
        );

        let ability = ResolvedAbility::new(
            Effect::MoveCounters {
                source: TargetFilter::SelfRef,
                counter_type: None,
                count: None,
                mode: CounterTransferMode::Put,
                selection: CounterMoveSelection::StackTarget,
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
                selection: CounterMoveSelection::StackTarget,
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
    fn atomic_move_counter_add_stage_doubler_removes_one_and_adds_two() {
        use crate::types::ability::{QuantityModification, ReplacementDefinition};
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Source".to_string(),
            Zone::Battlefield,
        );
        let dest_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Counter Destination".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        let mut repl = ReplacementDefinition::new(ReplacementEvent::AddCounter);
        repl.valid_card = Some(TargetFilter::SelfRef);
        repl.quantity_modification = Some(QuantityModification::DOUBLE);
        state
            .objects
            .get_mut(&dest_id)
            .unwrap()
            .replacement_definitions
            .push(repl);

        let mut events = Vec::new();
        assert!(move_counter_with_replacement(
            &mut state,
            PlayerId(0),
            source_id,
            dest_id,
            CounterType::Plus1Plus1,
            1,
            &mut events,
        ));

        assert_eq!(
            state.objects[&source_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0
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
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::CounterAdded {
                object_id,
                counter_type: CounterType::Plus1Plus1,
                count: 2,
            } if *object_id == dest_id
        )));
    }

    #[test]
    fn atomic_move_counter_add_stage_prevention_cancels_whole_move() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Source".to_string(),
            Zone::Battlefield,
        );
        let dest_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Counter Destination".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        install_no_counters_replacement(&mut state, dest_id);

        let mut events = Vec::new();
        assert!(move_counter_with_replacement(
            &mut state,
            PlayerId(0),
            source_id,
            dest_id,
            CounterType::Plus1Plus1,
            1,
            &mut events,
        ));

        assert_eq!(
            state.objects[&source_id]
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
            0
        );
        assert!(!events.iter().any(|event| matches!(
            event,
            GameEvent::CounterRemoved { .. } | GameEvent::CounterAdded { .. }
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
                selection: CounterMoveSelection::StackTarget,
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
                selection: CounterMoveSelection::StackTarget,
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

    #[test]
    fn stack_target_any_number_prompts_for_selected_destination_amount() {
        let mut state = GameState::new_two_player(42);
        let ability_source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Ability Source".to_string(),
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
            .insert(CounterType::Plus1Plus1, 3);
        state
            .objects
            .get_mut(&counter_source_id)
            .unwrap()
            .counters
            .insert(CounterType::Loyalty, 2);

        let ability = ResolvedAbility::new(
            Effect::MoveCounters {
                source: TargetFilter::Any,
                counter_type: None,
                count: None,
                mode: CounterTransferMode::Move,
                selection: CounterMoveSelection::StackTargetAnyNumber,
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

        let WaitingFor::MoveCountersDistribution {
            source_id,
            available,
            destinations,
            ..
        } = &state.waiting_for
        else {
            panic!(
                "expected MoveCountersDistribution, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(*source_id, counter_source_id);
        assert_eq!(destinations, &vec![dest_id]);
        assert!(available.contains(&(CounterType::Plus1Plus1, 3)));
        assert!(available.contains(&(CounterType::Loyalty, 2)));
        assert!(events.is_empty());
    }

    #[test]
    fn distribution_allows_same_destination_for_different_counter_types() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Source".to_string(),
            Zone::Battlefield,
        );
        let dest_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Counter Destination".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::MoveCounters {
                source: TargetFilter::SelfRef,
                counter_type: None,
                count: None,
                mode: CounterTransferMode::Move,
                selection: CounterMoveSelection::ResolutionDistributionAnyNumber,
                target: TargetFilter::Any,
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        validate_and_queue_counter_move_distribution(
            &mut state,
            &[
                CounterMoveChoice {
                    destination_id: dest_id,
                    counter_type: CounterType::Plus1Plus1,
                    count: 1,
                },
                CounterMoveChoice {
                    destination_id: dest_id,
                    counter_type: CounterType::Loyalty,
                    count: 1,
                },
            ],
            source_id,
            &[(CounterType::Plus1Plus1, 1), (CounterType::Loyalty, 1)],
            &[dest_id],
            &ability,
        )
        .unwrap();

        let queued = state.pending_counter_moves.as_ref().unwrap();
        assert_eq!(queued.remaining.len(), 2);
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
        // CR 306.5c + CR 704.5i: a genuinely-tracked planeswalker drained to 0
        // KEEPS its zero loyalty entry so the layer re-derive reports 0 (not the
        // printed base) and the state-based action can fire. (Phantom zeros from
        // removing a counter that was never present are still pruned — see
        // `apply_counter_removal`.)
        assert_eq!(
            obj.counters.get(&CounterType::Loyalty).copied(),
            Some(0),
            "drained loyalty entry must persist at 0, not be pruned away"
        );
        assert_eq!(obj.loyalty, Some(0));
    }

    /// CR 306.5c (hybrid model): removing loyalty from an object that was NOT
    /// counter-tracked (e.g. a clone whose loyalty comes from the Copy layer)
    /// must NOT leave a persistent 0 entry. Only genuinely-tracked counters keep
    /// their 0; a phantom 0 from `or_insert` on an absent counter is pruned, so
    /// the layer re-derive falls back to the object's field value rather than
    /// killing it. Guards the `was_present` condition in `apply_counter_removal`.
    #[test]
    fn remove_untracked_loyalty_does_not_leave_phantom_zero() {
        let mut state = GameState::new_two_player(42);
        let pw_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Cloned PW".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&pw_id).unwrap();
        // Loyalty present as a field (Copy-layer value) but NO loyalty counter.
        obj.loyalty = Some(5);

        let mut events = Vec::new();
        remove_counter_with_replacement(&mut state, pw_id, CounterType::Loyalty, 1, &mut events);

        assert!(
            !state.objects[&pw_id]
                .counters
                .contains_key(&CounterType::Loyalty),
            "removing an untracked loyalty counter must not create a persistent 0 entry",
        );
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

    /// CR 306.5c + CR 704.5i regression: a planeswalker drained to 0 loyalty
    /// must still read `Some(0)` after a layer re-evaluation — not snap back to
    /// its printed `base_loyalty`. Removing the last loyalty counter prunes the
    /// zero-count entry (CR 122.1), so the layer re-derive must treat the absent
    /// key as 0. Pre-fix this returned `Some(4)` (base_loyalty), leaving the
    /// planeswalker unkillable: check_zero_loyalty never saw 0, so neither a
    /// `-N` ability nor lethal damage could ever destroy it.
    #[test]
    fn loyalty_drained_to_zero_stays_zero_after_layer_re_evaluation() {
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
        // Printed loyalty 4; currently at 7 (entered at 4, gained 3).
        obj.base_loyalty = Some(4);
        obj.loyalty = Some(7);
        obj.counters.insert(CounterType::Loyalty, 7);

        let mut events = Vec::new();
        // A "-7" loyalty ability (or 7+ damage) routes through the resolver.
        remove_counter_with_replacement(&mut state, pw_id, CounterType::Loyalty, 7, &mut events);
        assert_eq!(state.objects[&pw_id].loyalty, Some(0));
        // CR 306.5c: the drained loyalty entry persists at 0 (it was genuinely
        // tracked) so the layer re-derive can distinguish "tracked, drained to 0"
        // from "not counter-tracked" (absent entry → fall back to base).
        assert_eq!(
            state.objects[&pw_id]
                .counters
                .get(&CounterType::Loyalty)
                .copied(),
            Some(0),
            "drained loyalty entry must persist at 0",
        );

        // Force layer re-evaluation: the present 0 entry must re-derive to 0,
        // NOT revert to base_loyalty (4).
        evaluate_layers(&mut state);
        assert_eq!(
            state.objects[&pw_id].loyalty,
            Some(0),
            "drained planeswalker must read 0 after layer re-derive, not snap back to printed 4",
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
        doubler_repl.quantity_modification = Some(QuantityModification::DOUBLE);
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
