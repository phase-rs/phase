use std::collections::HashSet;

use crate::game::game_object::GameObject;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    AbilityTag, CounterMoveSelection, CounterTransferMode, DelayedTriggerCondition, Duration,
    Effect, EffectError, EffectKind, EventCounterReproductionCount, QuantityExpr, ResolvedAbility,
    TargetChoiceTiming, TargetFilter, TargetRef,
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
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::player::PlayerId;
use crate::types::proposed_event::{CounterMoveStage, CounterPlacement, ProposedEvent};
use crate::types::resolution::FrameGate;
use crate::types::resolved_commands::{
    ResolvedObjectCounterCommand, ResolvedObjectCounterEdit,
    ResolvedObjectCounterReplayInvariantError,
};
use crate::types::zones::Zone;

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
        | CounterType::Finality
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

/// The replacement-aware result of previewing a counter addition.
///
/// This is intentionally an engine-internal decision fact rather than wire
/// state: callers use it while evaluating a currently-bound action, so it must
/// not be serialized or retained across object-incarnation changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CounterAdditionPreview {
    /// The proposed count reaches the object unchanged.
    Applied { count: u32 },
    /// A replacement effect prevents the counter addition.
    Prevented,
    /// Replacement ordering or an optional replacement needs this player's choice.
    ChoiceRequired { player: PlayerId },
    /// Replacement effects change the proposed counter count.
    Transformed { count: u32 },
    /// A replacement rewrites the counter event into a different event class.
    ///
    /// The tactical preview cannot claim that the requested counter was added,
    /// so consumers must handle this explicitly rather than treating it as an
    /// absent preview.
    Unsupported,
}

/// Preview an object-counter addition through the real replacement pipeline.
///
/// Returns `None` when `target` no longer identifies the same object
/// incarnation. The replacement pipeline operates only on an isolated clone,
/// so a tactical caller cannot add pending choices, events, or counters to the
/// live game state.
///
/// CR 122.1 + CR 614.1: Counter placement is subject to applicable
/// replacement effects before the event happens.
pub fn preview_counter_addition(
    state: &GameState,
    actor: PlayerId,
    target: ObjectIncarnationRef,
    counter_type: CounterType,
    count: u32,
) -> Option<CounterAdditionPreview> {
    let object = state.objects.get(&target.object_id)?;
    if ObjectIncarnationRef::from_object(object) != target {
        return None;
    }
    if count == 0 {
        return Some(CounterAdditionPreview::Applied { count });
    }

    let proposed = ProposedEvent::AddCounter {
        placement: CounterPlacement::Object {
            actor,
            object_id: target.object_id,
            counter_type,
        },
        count,
        applied: HashSet::new(),
    };
    let mut preview_state = state.clone();
    let mut events = Vec::new();

    match replacement::replace_event(&mut preview_state, proposed, &mut events) {
        ReplacementResult::Execute(ProposedEvent::AddCounter {
            count: resulting_count,
            ..
        }) if resulting_count == count => Some(CounterAdditionPreview::Applied {
            count: resulting_count,
        }),
        ReplacementResult::Execute(ProposedEvent::AddCounter {
            count: resulting_count,
            ..
        }) => Some(CounterAdditionPreview::Transformed {
            count: resulting_count,
        }),
        // A replacement may redirect the event into a different event class.
        // The counter-placement fact is explicitly unsupported rather than
        // absent, so conservative tactical consumers cannot mistake it for a
        // non-matching ability shape or stale object reference.
        ReplacementResult::Execute(_) => Some(CounterAdditionPreview::Unsupported),
        ReplacementResult::Prevented => Some(CounterAdditionPreview::Prevented),
        ReplacementResult::NeedsChoice(player) => {
            Some(CounterAdditionPreview::ChoiceRequired { player })
        }
    }
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
    state.push_counter_additions(PendingCounterAdditionQueue {
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
        .active_counter_additions_mut()
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
    let Some(queue) = state.active_counter_additions_mut() else {
        park_counter_completion_outside_active_direct_choice(state, completion);
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

/// CR 608.2c + CR 616.1: Park a completion that outlived a paused post-action
/// when no counter-additions queue is active to absorb it.
///
/// The default is unchanged — push the completion as the active inner frame.
/// The one exception is a pause that installed a direct-choice owner (a fresh
/// `ProliferateChoice`, say). That owner must stay at the stack top until its
/// action handler consumes it — `ResolutionStack::validate` rejects a buried
/// direct-choice owner — so pushing on top of it would corrupt the stack exactly
/// the way issue #7384 did. There the completion becomes the owner's PARENT
/// instead and runs once the owner is consumed, preserving the instruction
/// order; and when it owes nothing at all it is dropped rather than parked, so
/// no empty frame is installed above a live prompt.
///
/// Note that a completion parked as a parent is no longer the ACTIVE queue, so a
/// later `append_pending_counter_post_actions` would not find it. No such
/// appender is reachable while a direct-choice prompt is live, and the ordering
/// caveat on `ContinueProliferateActions` records the condition that would
/// change that.
fn park_counter_completion_outside_active_direct_choice(
    state: &mut GameState,
    completion: PendingEffectResolved,
) {
    let active_owns_prompt = state
        .resolution_stack
        .last()
        .is_some_and(|frame| matches!(frame.gate(), FrameGate::DirectChoice(_)));
    if !active_owns_prompt {
        // Every non-direct-choice pause keeps its historical shape, including
        // the empty placeholder frame that a later
        // `append_pending_counter_post_actions` may still land work on.
        stash_pending_counter_additions(state, Vec::new(), completion);
        return;
    }
    if completion.is_noop() {
        return;
    }
    let queue = PendingCounterAdditionQueue {
        remaining: Vec::new(),
        completion: Some(completion),
    };
    if state
        .insert_counter_additions_parent_of_active(queue)
        .is_err()
    {
        // Unreachable from a valid stack: the guard above proves an active child
        // exists, and inserting BELOW the top leaves the top — and so the prompt
        // gate — untouched. The insert validates a CLONE and assigns only on
        // success, so a failure leaves both stack and journal untouched.
        //
        // A failure therefore means the stack was ALREADY invalid, and the two
        // recoveries are not symmetric. Pushing the queue instead would stack an
        // owner above a live prompt, adding a SECOND validate violation to a
        // stack that already has one; dropping the completion forfeits its
        // terminal event but leaves the stack no worse than it was found. The
        // drop is the deliberate choice: compounding stack corruption is what
        // makes this class unrecoverable, and panicking is the very failure mode
        // #7384 reported.
        debug_assert!(
            false,
            "inserting a counter-additions parent below a direct-choice owner must validate"
        );
    }
}

pub(crate) fn drain_pending_counter_additions(state: &mut GameState, events: &mut Vec<GameEvent>) {
    while let Some(mut queue) = state.active_counter_additions().cloned() {
        let Some(next) = queue.remaining.first().cloned() else {
            state
                .take_active_counter_additions()
                .expect("settled counter-additions queue must own the active frame")
                .expect("settled counter-additions frame must exist");
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
                        events.push(GameEvent::EffectResolved {
                            kind,
                            source_id,
                            subject: None,
                        });
                    }
                    PendingEffectResolutionEvent::Suppress => {}
                }
                if let Some(action) = player_action {
                    events.push(GameEvent::PlayerPerformedAction {
                        player_id: action.player_id,
                        action: action.action,
                        look_count: None,
                        scry_bottom_count: None,
                        scry_top_count: None,
                    });
                }
            }
            continue;
        };
        queue.remaining.remove(0);
        state
            .replace_active_counter_additions(queue)
            .expect("re-parked counter-additions queue must own the active frame");
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
            } => {
                super::player_counter::add_player_counter_with_replacement(
                    state,
                    actor,
                    player_id,
                    counter_kind,
                    count,
                    events,
                ) != super::player_counter::PlayerCounterAdditionOutcome::NeedsChoice
            }
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
            events.push(GameEvent::EffectResolved {
                kind,
                source_id,
                subject: None,
            });
            true
        }
        PendingCounterPostAction::RecordPlayerAction { player_id, action } => {
            events.push(GameEvent::PlayerPerformedAction {
                player_id,
                action,
                look_count: None,
                scry_bottom_count: None,
                scry_top_count: None,
            });
            true
        }
        // CR 701.34a: The interrupted proliferate action is now complete —
        // publish it and drive whatever actions the effect still owes. Returns
        // `false` when another `ProliferateChoice` is open; the completion this
        // ran from is empty by construction, so nothing is re-parked and the
        // fresh direct-choice frame is left owning the stack top.
        PendingCounterPostAction::ContinueProliferateActions { pending } => {
            super::proliferate::continue_proliferate_actions(state, pending, events)
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
            crate::game::restrictions::record_token_created(state, object_id);
            // CR 603.6a: finalize the deferred ZoneChanged here, once the
            // token's counters have actually settled, so ETB trigger
            // observers (Altar of the Brood, Soul Warden, etc.) see the
            // Incubator's final counter count rather than firing early on a
            // pre-replacement-choice snapshot (issue #4238).
            //
            // CR 400.7 + CR 608.2i + CR 603.2c: route the record and the emit through the single
            // `from: None → Battlefield` authority so the emitted record carries this turn's real
            // zone-change index instead of the `0` placeholder. The authority calls
            // `record_battlefield_entry` itself, so the co-located call that used to sit above is
            // deleted — keeping it would double-count `battlefield_entries_this_turn`.
            crate::game::zones::record_and_emit_entry_from_no_zone(state, object_id, events);
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
            // CR 400.7 + CR 608.2i + CR 603.2c: route the record and the entry pair through the
            // single `from: None → Battlefield` authority so the emitted `ZoneChanged` carries
            // this turn's real zone-change index. The authority calls `record_battlefield_entry`
            // itself, so the co-located call that used to precede the attachment block is
            // deleted; that also moves the CR 608.2i snapshot point to AFTER `attach_to`'s
            // synchronous layer flush, so an attached token's entry row records its post-flush
            // characteristics (sanctioned by `battlefield_entry_record_for`'s own doc).
            //
            // OBJECT-GONE: if the token is no longer in `state.objects` when its parked counters
            // settle, this route reports NOTHING — no CR 400.7 row, no `ZoneChanged`, and no
            // `TokenCreated`. No guard here: `push_committed_token_entry_events` withholds
            // `TokenCreated` on the authority's own `None` verdict, which is the same predicate
            // that keeps `restrictions::record_token_created` above from writing a row. See that
            // function's doc for the wrong-trigger-fire measurement this prevents.
            super::token::push_committed_token_entry_events(
                state, object_id, name, source_id, events,
            );
            if matches!(sacrifice_at, Some(Duration::UntilEndOfCombat)) {
                let sacrifice_token = DelayedTrigger {
                    condition: DelayedTriggerCondition::AtNextPhase {
                        phase: crate::types::phase::Phase::EndCombat,
                    },
                    ability: Box::new(ResolvedAbility::new(
                        Effect::Sacrifice {
                            target: TargetFilter::Any,
                            count: QuantityExpr::Fixed { value: 1 },
                            min_count: 0,
                        },
                        vec![TargetRef::Object(object_id)],
                        source_id,
                        controller,
                    )),
                    controller,
                    source_id,
                    one_shot: true,
                    provenance: crate::types::identifiers::DelayedInstallIdentity::LegacyDelayed,
                };
                crate::game::triggers::install_delayed_trigger(state, sacrifice_token, events);
            }
            super::token::record_last_created_token(state, object_id);
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
            crate::game::restrictions::record_token_created(state, object_id);
            // CR 400.7 + CR 608.2i + CR 603.2c: route the record and the entry pair through the
            // single `from: None → Battlefield` authority so the emitted `ZoneChanged` carries
            // this turn's real zone-change index. The authority calls `record_battlefield_entry`
            // itself, so the co-located call that used to live here is deleted — keeping it would
            // double-count `battlefield_entries_this_turn` for every `FinalizeCopyTokenEntry`
            // token.
            //
            // OBJECT-GONE: same contract as the `FinalizeTokenEntry` arm above — a token that is
            // no longer in `state.objects` when its parked counters settle reports nothing,
            // because `push_committed_token_entry_events` gates `TokenCreated` on the authority's
            // `None` verdict, so it never disagrees with the existence-guarded
            // `record_token_created` ledger write immediately above it.
            super::token::push_committed_token_entry_events(
                state, object_id, name, source_id, events,
            );
            // The anaphora slot has TWO destinations on this route — ledger 3 and the in-flight
            // copy batch's `created_ids`, which `token_copy.rs`'s drain assigns WHOLESALE back onto
            // ledger 3 — so one guarded call owns both. Guarding the ledger write and pushing the
            // buffer as a separate statement republished the withheld id and overwrote the guarded
            // list with it.
            super::token::record_last_created_copy_batch_token(state, object_id);
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
            super::token_copy::extend_copy_batch_created_ids(state, status.created_ids);
            match completion {
                super::token_copy::CopyTokenApplyCompletion::Completed => true,
                super::token_copy::CopyTokenApplyCompletion::Paused => false,
            }
        }
        PendingCounterPostAction::ContinueCopyTokenEntryAfterAuraHost { object_id, tail } => {
            // CR 303.4f: the host choice is answered and the attach is applied;
            // run the rest of this token's entry (copy exceptions, entry counters,
            // entry events) plus the rest of the batch.
            super::token_copy::continue_copy_token_entry_after_aura_host(
                state, object_id, *tail, events,
            )
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
        PendingCounterPostAction::EmitCommittedCopyTokenEntry { object_id } => {
            // CR 400.7 + CR 616.1: the ETB-counter ordering choice is answered and `BecomeCopy`
            // has run (or, on the pre-`BecomeCopy` commit pause, the copy chain was abandoned and
            // this is as realized as that route gets), so realize the entry inside the drain —
            // before the rest of this action, whether or not that action settles.
            //
            // MEASURED redundancy, stated rather than implied: when the drain's action DOES settle
            // to `Priority` (the Faithful Watchdog fixture in
            // `tests/integration/token_zone_change_index.rs`, and every route the current card pool
            // reaches), `token::realize_settled_token_battlefield_entry` realizes it anyway — from
            // inside `apply_action` ahead of that action's CR 603.2 scan, and, for handlers that
            // never reach that pipeline, from `apply_action_boundary_core`, which now runs
            // `run_post_action_pipeline_from` over the slice it appended. Deleting this call AND the
            // in-`apply_action` one flips no test. It is kept for a drain that does NOT settle in
            // its own action, where this is the only in-action realization point, and because the
            // in-`apply_action` call orders the CR 400.7 row ahead of that action's CR 704.3 SBA
            // pass (CR 704.5f). `false` means an earlier convergence point already realized it
            // (structurally idempotent, `Option::take_if`), which is not an error.
            let _ = super::token::flush_pending_token_battlefield_entry(state, object_id, events);
            if !state.last_created_token_ids.contains(&object_id) {
                super::token::record_last_created_token(state, object_id);
            }
            // DELIBERATELY NOT `record_last_created_copy_batch_token`: this arm RE-SYNCS the batch
            // buffer to the whole guarded ledger rather than appending one id, and the two are not
            // interchangeable — the buffer accumulates across batches while the ledger is reset per
            // batch. Copying the ledger cannot publish an id the ledger's own guard withheld, so
            // this shape needs no second predicate; it needs the source to stay the GUARDED ledger,
            // which is what `state.last_created_token_ids` is after the call above.
            let created_ids = state.last_created_token_ids.clone();
            if let Some(pending) = state.active_copy_token_mut() {
                pending.created_ids = created_ids;
            }
            true
        }
        PendingCounterPostAction::FinishMeldEntry { context } => {
            crate::game::meld::finish_deferred_meld_entry(state, context, events);
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
            exile_controller,
            exile_tracking,
            enters_attacking,
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
                exile_controller,
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
                super::change_zone::ZoneDeliveryResult::Done => {
                    if enters_attacking && to == Zone::Battlefield {
                        let controller = state
                            .objects
                            .get(&object_id)
                            .map(|object| object.controller)
                            .expect("a settled battlefield entrant must exist");
                        // CR 508.4: an entrant joins combat only after its
                        // replacement-modified entry has fully settled.
                        if crate::game::combat::choose_entry_attack_target_or_enter(
                            state, object_id, controller,
                        )
                        .is_some()
                        {
                            return false;
                        }
                    }
                    true
                }
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

    let (object, expected_old) = {
        let Some(object) = state.objects.get(&object_id) else {
            return;
        };
        (
            ObjectIncarnationRef::from_object(object),
            object.counters.get(&counter_type).copied().unwrap_or(0),
        )
    };
    let command = ResolvedObjectCounterCommand {
        object,
        counter_type: counter_type.clone(),
        expected_old,
        edit: ResolvedObjectCounterEdit::Add { actor, count },
        cause: state.current_or_begin_rules_execution_node(),
    };
    if apply_resolved_counter_edit(state, &command).is_err() {
        return;
    }
    state
        .resolved_rules_journal
        .record_object_counter(command)
        .expect("resolved counter addition must have a live journal cause");

    events.push(GameEvent::CounterAdded {
        object_id,
        counter_type,
        count,
        // CR 122.1 + CR 603.2c: record who placed the counters so actor-gated
        // "whenever you/an opponent put counters" triggers can match.
        actor,
    });
}

/// CR 122.1 + CR 122.6: Apply one exact post-replacement counter delivery.
///
/// The command carries the recipient occurrence, prior count, final delivered
/// count, and causal node. This applier never re-enters CR 614's replacement
/// pipeline, so a retained-prefix replay cannot apply Vorinclex/Hardened
/// Scales class replacements twice.
pub fn apply_resolved_counter_edit(
    state: &mut GameState,
    command: &ResolvedObjectCounterCommand,
) -> Result<(), ResolvedObjectCounterReplayInvariantError> {
    let object = state.objects.get(&command.object.object_id).ok_or(
        ResolvedObjectCounterReplayInvariantError::MissingObject(command.object),
    )?;
    let found_reference = ObjectIncarnationRef::from_object(object);
    if found_reference != command.object {
        return Err(ResolvedObjectCounterReplayInvariantError::StaleObject {
            expected: command.object,
            found: found_reference,
        });
    }
    let found_count = object
        .counters
        .get(&command.counter_type)
        .copied()
        .unwrap_or(0);
    if found_count != command.expected_old {
        return Err(
            ResolvedObjectCounterReplayInvariantError::CounterPreconditionMismatch {
                counter_type: command.counter_type.clone(),
                expected: command.expected_old,
                found: found_count,
            },
        );
    }

    let affects_layers = counter_type_affects_layers(&command.counter_type);
    let added_record = {
        let object = state.objects.get_mut(&command.object.object_id).ok_or(
            ResolvedObjectCounterReplayInvariantError::MissingObject(command.object),
        )?;
        match &command.edit {
            ResolvedObjectCounterEdit::Add { actor, count } => {
                if *count == 0 {
                    return Err(ResolvedObjectCounterReplayInvariantError::ZeroCount);
                }
                let next = command.expected_old.checked_add(*count).ok_or(
                    ResolvedObjectCounterReplayInvariantError::CounterOverflow {
                        counter_type: command.counter_type.clone(),
                        previous: command.expected_old,
                        added: *count,
                    },
                )?;
                object.counters.insert(command.counter_type.clone(), next);
                sync_derived_from_counters(object, &command.counter_type);
                crate::types::counter::prune_zero_counters(&mut object.counters);
                Some(CounterAddedRecord {
                    actor: *actor,
                    object_id: object.id,
                    counter_type: command.counter_type.clone(),
                    count: *count,
                    name: object.name.clone(),
                    core_types: object.card_types.core_types.clone(),
                    subtypes: object.card_types.subtypes.clone(),
                    supertypes: object.card_types.supertypes.clone(),
                    keywords: object.keywords.clone(),
                    power: object.power,
                    toughness: object.toughness,
                    // CR 709.4b + CR 202.3d: combined colors / mana value for a
                    // split card off the stack remain part of the event-time fact.
                    colors: object.effective_colors(),
                    mana_value: object.effective_mana_value(),
                    controller: object.controller,
                    owner: object.owner,
                    counters: object
                        .counters
                        .iter()
                        .map(|(counter_type, count)| (counter_type.clone(), *count))
                        .collect(),
                })
            }
            ResolvedObjectCounterEdit::Remove { count } => {
                if *count == 0 {
                    return Err(ResolvedObjectCounterReplayInvariantError::ZeroCount);
                }
                let next = command.expected_old.checked_sub(*count).ok_or(
                    ResolvedObjectCounterReplayInvariantError::CounterPreconditionMismatch {
                        counter_type: command.counter_type.clone(),
                        expected: command.expected_old,
                        found: *count,
                    },
                )?;
                object.counters.insert(command.counter_type.clone(), next);
                sync_derived_from_counters(object, &command.counter_type);

                // CR 122.1 + CR 306.5c: A drained tracked planeswalker keeps a
                // present zero loyalty key so layer re-derivation preserves 0.
                let keep_zero = command.counter_type == CounterType::Loyalty && next == 0;
                crate::types::counter::prune_zero_counters(&mut object.counters);
                if keep_zero {
                    object.counters.insert(command.counter_type.clone(), 0);
                }
                None
            }
        }
    };

    if affects_layers {
        state.layers_dirty.mark_full();
    }
    if let Some(record) = added_record {
        state.counter_added_this_turn.push(record);
    }
    Ok(())
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
    if count == 0 {
        return;
    }
    let (object, expected_old) = {
        let Some(object) = state.objects.get(&object_id) else {
            return;
        };
        (
            ObjectIncarnationRef::from_object(object),
            object.counters.get(&counter_type).copied().unwrap_or(0),
        )
    };
    let removed = expected_old.min(count);
    if removed == 0 {
        return;
    }
    let command = ResolvedObjectCounterCommand {
        object,
        counter_type: counter_type.clone(),
        expected_old,
        edit: ResolvedObjectCounterEdit::Remove { count: removed },
        cause: state.current_or_begin_rules_execution_node(),
    };
    if apply_resolved_counter_edit(state, &command).is_err() {
        return;
    }
    state
        .resolved_rules_journal
        .record_object_counter(command)
        .expect("resolved counter removal must have a live journal cause");

    events.push(GameEvent::CounterRemoved {
        object_id,
        counter_type,
        count: removed,
    });
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
            // Issue #4878: `obj.counters` is a default-RandomState HashMap, so
            // `max_by_key` alone would break ties by per-process hash-iteration
            // order. Tie-break by CounterType's derived Ord for a deterministic
            // choice when two or more types share the max count.
            .max_by(|(ta, &na), (tb, &nb)| na.cmp(&nb).then_with(|| ta.cmp(tb)))
            .map(|(ty, _)| ty.clone()),
    }
}

/// CR 122.1d + CR 101.2: Returns `true` when an active
/// `CountersCantBeRemoved { counter_type }` static's `affected` filter matches
/// the given object for the given counter type. "Can't" effects take precedence
/// over any game action that would remove counters (Fear of Sleep Paralysis).
pub(crate) fn counter_removal_blocked(
    state: &GameState,
    object_id: ObjectId,
    counter_type: &CounterType,
) -> bool {
    use crate::types::statics::StaticMode;
    crate::game::functioning_abilities::battlefield_active_statics(state).any(
        |(source_obj, def)| {
            if let StaticMode::CountersCantBeRemoved {
                counter_type: ref ct,
            } = def.mode
            {
                if ct != counter_type {
                    return false;
                }
                // `def.affected` is Option<TargetFilter>; None means "all permanents".
                match &def.affected {
                    None => true,
                    Some(filter) => crate::game::static_abilities::static_filter_matches(
                        state,
                        &crate::game::static_abilities::StaticCheckContext {
                            target_id: Some(object_id),
                            ..Default::default()
                        },
                        filter,
                        source_obj.id,
                    ),
                }
            } else {
                false
            }
        },
    )
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
    // CR 101.2: "Can't" overrides "can" — if a static prohibits removal of
    // this counter type from this object, bail out immediately.
    if counter_removal_blocked(state, object_id, &counter_type) {
        return;
    }

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
    // CR 101.2: Moving counters away is removal from the source — if a
    // "can't be removed" prohibition covers the source, block the move.
    if counter_removal_blocked(state, counter_move.source_id, &counter_move.counter_type) {
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
    while let Some(mut queue) = state.active_counter_moves().cloned() {
        let Some(next) = queue.remaining.first().cloned() else {
            state
                .take_active_counter_moves()
                .expect("settled counter-moves queue must own the active frame")
                .expect("settled counter-moves frame must exist");
            events.push(GameEvent::EffectResolved {
                kind: queue.effect_kind,
                source_id: queue.source_id,
                subject: None,
            });
            continue;
        };
        queue.remaining.remove(0);
        state
            .replace_active_counter_moves(queue)
            .expect("re-parked counter-moves queue must own the active frame");
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
    let (counter_type, count) = match &ability.effect {
        Effect::PutCounter {
            counter_type,
            count,
            ..
        } => (counter_type.clone(), count.clone()),
        _ => (
            CounterType::Plus1Plus1,
            crate::types::ability::QuantityExpr::Fixed { value: 1 },
        ),
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
        // CR 608.2c: A quantity bound to the resolved recipient (such as
        // Sovereign Okinec Ahau's "the difference") is evaluated separately
        // for each object. Source-relative quantities remain one shared value.
        let count_uses_recipient = crate::game::quantity::quantity_expr_uses_recipient(&count);
        let counter_num_shared = (!count_uses_recipient).then(|| {
            // CR 107.1b: Ability-context resolve preserves the announced X for
            // source-relative counter quantities.
            crate::game::quantity::resolve_quantity_with_targets(state, &count, ability).max(0)
                as u32
        });
        targets
            .into_iter()
            .map(|obj_id| {
                let counter_num = if count_uses_recipient {
                    crate::game::quantity::resolve_quantity_with_targets_and_recipient(
                        state, &count, ability, obj_id,
                    )
                    .max(0) as u32
                } else {
                    counter_num_shared.expect("shared counter quantity must be resolved")
                };
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
        subject: None,
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
                count,
                ..
            } if *added_to == object_id && *count > 0
        )
    });
    if evolved {
        events.push(GameEvent::Evolved { object_id });
    }
}

/// CR 122.1 + CR 603.2c + CR 608.2h: Reproduce onto the effect's target(s) the
/// counters that the triggering counter-placement event just put onto the
/// recipient creature ("put the same number and kind of counters" / "put one of
/// each of those kinds of counters"). The kind→count multiset is read from
/// `state.current_trigger_events` — which, under the per-recipient firing model
/// (`matching_counter_added_events_by_recipient`), holds exactly one recipient's
/// `GameEvent::CounterAdded` occurrences (one per kind placed on it). Unlike
/// `resolve_move` this reads the DELTA the event placed, not the recipient's
/// total counter map. The multiset is snapshotted from the firing's events
/// (CR 608.2h), so later changes to the recipient's counters don't affect it.
pub fn resolve_reproduce_event_counters(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let per_kind_count = match &ability.effect {
        Effect::ReproduceEventCounters { per_kind_count, .. } => *per_kind_count,
        _ => return Ok(()),
    };

    // Fold the firing's `CounterAdded` occurrences into a kind→count multiset,
    // preserving first-seen kind order for deterministic placement/event order.
    let mut reproduced: Vec<(CounterType, u32)> = Vec::new();
    for event in &state.current_trigger_events {
        let GameEvent::CounterAdded {
            counter_type,
            count,
            ..
        } = event
        else {
            continue;
        };
        // CR 122.1: "one of each of those kinds" (PerKind) ignores the event's
        // per-kind magnitude; "the same number and kind" (SameNumber) reproduces
        // exactly what the event placed, summing repeated kinds.
        let amount = match per_kind_count {
            EventCounterReproductionCount::SameNumber => *count,
            EventCounterReproductionCount::PerKind(n) => n,
        };
        if amount == 0 {
            continue;
        }
        match reproduced.iter_mut().find(|(kind, _)| kind == counter_type) {
            Some((_, existing)) => match per_kind_count {
                // SameNumber sums repeated kinds; PerKind is a flat per-kind
                // count, so a repeated kind stays at `n` (already recorded).
                EventCounterReproductionCount::SameNumber => *existing += amount,
                EventCounterReproductionCount::PerKind(_) => {}
            },
            None => reproduced.push((counter_type.clone(), amount)),
        }
    }

    if reproduced.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    let targets = resolve_defined_or_targets(state, ability);
    let additions: Vec<PendingCounterAddition> = targets
        .into_iter()
        .flat_map(|obj_id| {
            reproduced.iter().map(move |(kind, amount)| {
                object_counter_addition(ability.controller, obj_id, kind.clone(), *amount)
            })
        })
        .collect();

    let completion =
        PendingEffectResolved::new(EffectKind::from(&ability.effect), ability.source_id);
    for (index, addition) in additions.iter().cloned().enumerate() {
        if !apply_object_counter_addition(state, addition, events) {
            // CR 614: a replacement choice paused placement — stash the rest so
            // the continuation drains them after the choice resolves.
            stash_pending_counter_additions(state, additions[index + 1..].to_vec(), completion);
            return Ok(());
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
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
    // CR 700.2 + CR 608.2c: both sentinel arms below are ladders whose FIRST rung
    // is `chain_tracked_set_id`, and that rung is what mode scoping acts on — the
    // boundary reset in `resolve_ability_chain` clears it at each mode root, so
    // the chain rung either holds the currently-resolving mode's own set or is
    // absent. The trailing raw `max_by_key` rung is the fallback, and it stays
    // mode-correct for the same reason the other sentinel readers do: the
    // ordering argument written once on `effects::publish_tracked_set`.
    // Deliberately not routed through `targeting::resolve_tracked_set_id`: that
    // authority SKIPS empty sets, and here not skipping is the correct semantics
    // (a chained counter effect refers to the preceding effect's set even when it
    // affected no objects).
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
        subject: None,
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
        subject: None,
    });

    Ok(())
}

/// CR 608.2d + CR 118.12 + CR 122.1: True when a `RemoveCounter` effect used as
/// an optional "you may" gate cannot be performed — none of its resolved target
/// object(s) hold a matching counter that is *permitted to be removed*. "You may
/// remove a charge counter from this artifact" with zero charge counters (Sun
/// Droplet), or with the counter present but frozen by a `CountersCantBeRemoved`
/// static (Fear of Sleep Paralysis class).
///
/// CR 608.2d: a player can't choose an impossible option. Removing a counter that
/// isn't there — or one an effect forbids removing — does nothing, so the
/// up-front "you may" must not be offered. CR 118.12: the `EffectOutcome::
/// OptionalEffectPerformed` rider ("If you do, gain 1 life") checks whether the
/// player chose to perform the action; if the action is impossible the choice is
/// never offered, so the rider must not fire.
///
/// Both the presence check and the removal-prohibition check use the resolver's
/// own authorities (`resolve_defined_or_targets`, `counter_removal_blocked`), so
/// feasibility and resolution can never diverge. A `count > 1` request is still
/// feasible whenever ≥1 permitted counter exists — the resolver removes as many
/// as available (CR 122.1 "as much as possible"), a nonzero action. Returns
/// `false` (feasible) for any non-`RemoveCounter` effect so the caller's other
/// arms are unaffected.
pub(crate) fn remove_counter_optional_is_infeasible(
    state: &GameState,
    ability: &ResolvedAbility,
) -> bool {
    let Effect::RemoveCounter { counter_type, .. } = &ability.effect else {
        return false;
    };
    let targets = resolve_defined_or_targets(state, ability);
    // Feasible iff SOME resolved target holds a matching counter that is not
    // removal-blocked. `counter_type` is `Option<CounterType>`: `Some(ct)`
    // matches that specific kind ("a charge counter"), `None` means any kind
    // ("a counter"). An empty target set is infeasible (nothing to remove from).
    let feasible = targets.iter().any(|obj_id| {
        state.objects.get(obj_id).is_some_and(|obj| {
            obj.counters.iter().any(|(ct, &n)| {
                n > 0
                    && counter_type.as_ref().is_none_or(|expected| expected == ct)
                    && !counter_removal_blocked(state, *obj_id, ct)
            })
        })
    });
    !feasible
}

/// Resolve targeting to object IDs using the typed TargetFilter.
fn resolve_defined_or_targets(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Vec<crate::types::identifiers::ObjectId> {
    let target_spec = match &ability.effect {
        Effect::MultiplyCounter { target, .. }
        | Effect::RemoveCounter { target, .. }
        // CR 122.1 + CR 603.2c: reproduction targets exactly like `PutCounter` —
        // `SelfRef` short-circuits to the source (Captain Marvel), a real target
        // falls through to the chosen-target return (Aragorn).
        | Effect::ReproduceEventCounters { target, .. }
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

    // CR 508.1 + CR 603.2c + CR 608.2c (issue #5949): A batched attack trigger's
    // "each of them"/"those creatures" anaphor (`ParentTarget` with no chosen
    // object target) refers to the whole declared-attackers batch. Route through
    // the shared attack-trigger batch resolver — the exact path `Effect::Pump`
    // uses via `resolved_targets` for Champions from Beyond's "those creatures
    // get +4/+4" — so the counter is placed on every attacker (Vrestin,
    // Menoptra Leader). Non-attack `ParentTarget` contexts return `None` here
    // and fall through to the existing event-context resolution below.
    if matches!(target_spec, Some(TargetFilter::ParentTarget))
        && !has_object_target
        && !has_choice_bookkeeping_player
    {
        if let Some(targets) =
            crate::game::targeting::parent_target_refs_from_attack_trigger_context(state)
        {
            return targets
                .into_iter()
                .filter_map(|target| match target {
                    TargetRef::Object(id) => Some(id),
                    TargetRef::Player(_) => None,
                })
                .collect();
        }
    }

    // CR 608.2c + CR 122.1: `ParentTargetSlot { index }` — a later counter
    // instruction that refers to a specific earlier declared target slot ("put a
    // +1/+1 counter on the creature you control", index 0). The counter node's
    // local `ability.targets` may have been replaced with the most-recent parent
    // slot by chain propagation, so resolve against the flattened chain root
    // (single authority in `targeting`), then keep only the object at `index`.
    //
    // CR 400.7 + CR 603.7c: deliberately NOT pin-filtered — see the standing
    // constraint recorded at the `ParentTargetSlot` arm in
    // `targeting::resolved_object_ids_for_filter_with_context`. Slot numbering
    // is declared, not live, so filtering here would renumber later slots. Do
    // not "complete the pattern" by adding a pin check.
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

    // CR 608.2c: A `ParentTarget` in a chained counter effect refers to the
    // object chosen by the parent instruction. Prefer that propagated object
    // target over the triggering event context; for a landfall trigger, the
    // latter is the entering land rather than the creature chosen by the
    // player. The object guard deliberately excludes the chooser-only target
    // bookkeeping used by `ChooseOneOf` branches above.
    if matches!(target_spec, Some(TargetFilter::ParentTarget)) && has_object_target {
        return ability
            .live_object_targets(state)
            .into_iter()
            .filter_map(|target| match target {
                TargetRef::Object(id) => Some(id),
                TargetRef::Player(_) => None,
            })
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

    // CR 400.7 + CR 603.7c: a delayed counter effect's pinned referent that
    // became a new object is dropped. Substitution only: the source-fallback
    // arms and the attack-batch arm above all key off `has_object_target` /
    // `has_choice_bookkeeping_player`, computed from the RAW `ability.targets`,
    // so those preconditions stay intact and an emptied list here cannot
    // re-bind to a different object. No early return, hence no EffectResolved
    // question.
    //
    // This is `lagrella, the magpie`'s route: her delayed `PutCounter` reaches
    // here with the returned card in `targets`. She is correct only because the
    // pin is never STAMPED for her (her condition names the referent's own
    // entry) — not because this read is exempted.
    ability
        .live_object_targets(state)
        .into_iter()
        .filter_map(|t| {
            if let TargetRef::Object(id) = t {
                Some(id)
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
            subject: None,
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
            subject: None,
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
        subject: None,
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
            subject: None,
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
            subject: None,
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
            subject: None,
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
            subject: None,
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

    state.push_counter_moves(PendingCounterMoveQueue {
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
        subject: None,
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
            subject: None,
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
            subject: None,
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

/// CR 107.1c: Validate a submitted `RemoveCountersChoice` answer and park the
/// per-type removals in the typed `CounterRemovals` frame for
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
    state.push_counter_removals(PendingCounterRemovalQueue {
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
    while let Some(mut queue) = state.active_counter_removals().cloned() {
        let Some((counter_type, count)) = queue.remaining.first().cloned() else {
            // CR 608.2h: ordering invariant — stamp the total removed before the
            // terminating EffectResolved (and thus before the continuation drains).
            state.last_effect_count = Some(queue.total as i32);
            state
                .take_active_counter_removals()
                .expect("settled counter-removals queue must own the active frame")
                .expect("settled counter-removals frame must exist");
            events.push(GameEvent::EffectResolved {
                kind: queue.effect_kind,
                source_id: queue.source_ability_id,
                subject: None,
            });
            continue;
        };
        queue.remaining.remove(0);
        let source_id = queue.source_id;
        state
            .replace_active_counter_removals(queue)
            .expect("re-parked counter-removals queue must own the active frame");
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
    use crate::game::engine::apply_as_current;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ControllerRef, FilterProp, QuantityExpr, QuantityModification, ReplacementDefinition,
        ReplacementMode, TargetChoiceTiming, TargetFilter, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId, ObjectIncarnationRef};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::resolution::{ResolutionFrame, ResolutionStateWire};
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
                ability: Box::new(root),
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

    /// Issue #4878: `resolve_counter_match_for_removal`'s `CounterMatch::Any`
    /// arm used to break count ties with a bare `max_by_key`, which falls back
    /// to `obj.counters`' per-process HashMap (RandomState) iteration order —
    /// a different `CounterType` could be selected for removal across
    /// processes on an identical seed. The fix tie-breaks by `CounterType`'s
    /// derived `Ord`, so a tied object always resolves to the same,
    /// Ord-greatest type. Reverting to bare `max_by_key` makes this assertion
    /// flip to "unspecified" (test would become flaky, not merely wrong).
    #[test]
    fn resolve_counter_match_for_removal_breaks_ties_by_counter_type_ord() {
        use crate::types::counter::{CounterMatch, CounterType};

        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Tied Counters Test".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.counters.insert(CounterType::Plus1Plus1, 2);
        obj.counters.insert(CounterType::Minus1Minus1, 2);

        // CounterType::Minus1Minus1 is declared after Plus1Plus1, so it is the
        // Ord-greater of the two tied types and must win deterministically.
        assert_eq!(
            resolve_counter_match_for_removal(&state, id, &CounterMatch::Any),
            Some(CounterType::Minus1Minus1)
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

    #[test]
    fn preview_counter_addition_reports_applied_without_mutating_live_state() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Preview Target".to_string(),
            Zone::Battlefield,
        );
        let target = ObjectIncarnationRef::from_object(&state.objects[&target_id]);
        let before = state.clone();

        assert_eq!(
            preview_counter_addition(&state, PlayerId(0), target, CounterType::Plus1Plus1, 2,),
            Some(CounterAdditionPreview::Applied { count: 2 })
        );
        assert_eq!(
            state, before,
            "preview must not add counters, events, or replacement-choice state to the live game"
        );
    }

    #[test]
    fn preview_counter_addition_reports_transformed_replacement_without_mutation() {
        let mut state = GameState::new_two_player(42);
        let doubler_id = create_object(
            &mut state,
            CardId(2),
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
        let target_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Counter Preview Target".to_string(),
            Zone::Battlefield,
        );
        let target = ObjectIncarnationRef::from_object(&state.objects[&target_id]);
        let before = state.clone();

        assert_eq!(
            preview_counter_addition(&state, PlayerId(0), target, CounterType::Plus1Plus1, 2,),
            Some(CounterAdditionPreview::Transformed { count: 4 })
        );
        assert_eq!(
            state, before,
            "replacement processing must remain clone-confined"
        );
    }

    #[test]
    fn preview_counter_addition_reports_prevented_replacement() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Counter-Proof Target".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target_id)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::AddCounter)
                    .valid_card(TargetFilter::SelfRef)
                    .quantity_modification(QuantityModification::Prevent),
            );
        let target = ObjectIncarnationRef::from_object(&state.objects[&target_id]);

        assert_eq!(
            preview_counter_addition(&state, PlayerId(0), target, CounterType::Plus1Plus1, 1,),
            Some(CounterAdditionPreview::Prevented)
        );
    }

    #[test]
    fn preview_counter_addition_reports_replacement_choice_required() {
        let mut state = GameState::new_two_player(42);
        install_noncommuting_counter_replacements(&mut state);
        let target_id = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Counter Preview Target".to_string(),
            Zone::Battlefield,
        );
        let target = ObjectIncarnationRef::from_object(&state.objects[&target_id]);

        assert_eq!(
            preview_counter_addition(&state, PlayerId(0), target, CounterType::Plus1Plus1, 1,),
            Some(CounterAdditionPreview::ChoiceRequired {
                player: PlayerId(0)
            })
        );
    }

    #[test]
    fn preview_counter_addition_rejects_stale_object_incarnation() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(6),
            PlayerId(0),
            "Counter Preview Target".to_string(),
            Zone::Battlefield,
        );
        let stale_target = ObjectIncarnationRef::from_object(&state.objects[&target_id]);
        state
            .objects
            .get_mut(&target_id)
            .unwrap()
            .bump_incarnation();

        assert_eq!(
            preview_counter_addition(
                &state,
                PlayerId(0),
                stale_target,
                CounterType::Plus1Plus1,
                1,
            ),
            None,
            "a tactical fact must not apply to a new object that reused the same id"
        );
    }

    fn install_counter_removal_optional_replacement(state: &mut GameState) {
        let replacement_id = create_object(
            state,
            CardId(902),
            PlayerId(0),
            "Counter Removal Optional".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&replacement_id)
            .expect("counter-removal replacement exists")
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::RemoveCounter)
                    .mode(ReplacementMode::Optional { decline: None }),
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
    fn zero_counter_delivery_is_an_ordinary_noop_without_a_command() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();

        apply_counter_addition(
            &mut state,
            PlayerId(0),
            obj_id,
            CounterType::Plus1Plus1,
            0,
            &mut events,
        );
        apply_counter_removal(&mut state, obj_id, CounterType::Plus1Plus1, 0, &mut events);

        assert!(state.objects[&obj_id].counters.is_empty());
        assert!(events.is_empty());
        assert!(state.resolved_rules_journal.entries().is_empty());
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
            .active_counter_additions()
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
        state.push_counter_additions(PendingCounterAdditionQueue {
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
            .active_counter_additions()
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
            .active_counter_additions()
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
            .active_counter_additions()
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
                attachments: Vec::new(),
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
                ..
            } if *object_id == dest_id
        )));
    }

    /// CR 616.1 + CR 122.5: a selected CounterMoves queue remains the sole
    /// runtime owner while each move's add stage chooses among noncommuting
    /// counter replacements. The queue re-parks for every prompt and v2 restores
    /// that real prompt boundary before production replacement actions resume it.
    #[test]
    fn counter_moves_queue_reparks_and_roundtrips_v2_at_replacement_choice() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(910),
            PlayerId(0),
            "Counter Source".to_string(),
            Zone::Battlefield,
        );
        let destination_id = create_object(
            &mut state,
            CardId(911),
            PlayerId(0),
            "Counter Destination".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .expect("counter source exists")
            .counters
            .insert(CounterType::Plus1Plus1, 2);
        install_noncommuting_counter_replacements(&mut state);
        state.push_counter_moves(PendingCounterMoveQueue {
            remaining: vec![
                PendingCounterMove {
                    actor: PlayerId(0),
                    source_id,
                    destination_id,
                    counter_type: CounterType::Plus1Plus1,
                    remove_count: 1,
                    add_count: 1,
                },
                PendingCounterMove {
                    actor: PlayerId(0),
                    source_id,
                    destination_id,
                    counter_type: CounterType::Plus1Plus1,
                    remove_count: 1,
                    add_count: 1,
                },
            ],
            effect_kind: EffectKind::MoveCounters,
            source_id,
        });

        drain_pending_counter_moves(&mut state, &mut Vec::new());
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert!(matches!(
            state.resolution_stack.last(),
            Some(ResolutionFrame::CounterMoves(_))
        ));
        assert_eq!(
            state
                .active_counter_moves()
                .expect("counter queue remains active at its prompt")
                .remaining
                .len(),
            1
        );

        let saved = serde_json::to_value(ResolutionStateWire::from_game_state(state))
            .expect("paused CounterMoves prompt serializes as v2");
        assert_eq!(saved["resolution_state_version"], 2);
        assert!(saved.get("pending_counter_moves").is_none());
        let restored: ResolutionStateWire =
            serde_json::from_value(saved).expect("v2 CounterMoves prompt restores");
        let mut state = restored.into_game_state();

        for _ in 0..8 {
            if !matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }) {
                break;
            }
            apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
                .expect("production replacement action resumes the counter queue");
            if matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }) {
                assert!(matches!(
                    state.resolution_stack.last(),
                    Some(ResolutionFrame::CounterMoves(_))
                ));
            }
        }

        assert!(state.active_counter_moves().is_none());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(
            state.objects[&source_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0
        );
        assert!(
            state.objects[&destination_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
                > 2,
            "both counter moves must complete through the chosen replacement paths"
        );
    }

    /// CR 107.1c + CR 608.2h + CR 616.1: the production counter-removal choice
    /// parks its selected tail in CounterRemovals while each removal offers its
    /// applicable optional replacement. v2 restores that real replacement prompt
    /// before the production actions finish the queue and stamp its total.
    #[test]
    fn counter_removals_queue_reparks_and_roundtrips_v2_at_replacement_choice() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(912),
            PlayerId(0),
            "Counter Removal Source".to_string(),
            Zone::Battlefield,
        );
        let charge = CounterType::Generic("charge".to_string());
        {
            let source = state
                .objects
                .get_mut(&source_id)
                .expect("counter-removal source exists");
            source.counters.insert(CounterType::Plus1Plus1, 1);
            source.counters.insert(charge.clone(), 1);
        }
        install_counter_removal_optional_replacement(&mut state);
        state.waiting_for = WaitingFor::RemoveCountersChoice {
            player: PlayerId(0),
            source_id,
            counter_type: None,
            available: vec![(CounterType::Plus1Plus1, 1), (charge.clone(), 1)],
            pending_effect: Box::new(make_counter_ability(
                Effect::RemoveCounter {
                    counter_type: None,
                    count: QuantityExpr::Fixed { value: -1 },
                    target: TargetFilter::Any,
                },
                source_id,
            )),
        };

        apply_as_current(
            &mut state,
            GameAction::ChooseCountersToRemove {
                selections: vec![
                    CounterRemoveChoice {
                        counter_type: CounterType::Plus1Plus1,
                        count: 1,
                    },
                    CounterRemoveChoice {
                        counter_type: charge.clone(),
                        count: 1,
                    },
                ],
            },
        )
        .expect("production removal choice creates its first replacement prompt");
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert!(matches!(
            state.resolution_stack.last(),
            Some(ResolutionFrame::CounterRemovals(_))
        ));
        assert_eq!(
            state
                .active_counter_removals()
                .expect("counter-removals queue owns its prompt")
                .remaining
                .len(),
            1
        );

        let saved = serde_json::to_value(ResolutionStateWire::from_game_state(state))
            .expect("paused CounterRemovals prompt serializes as v2");
        assert_eq!(saved["resolution_state_version"], 2);
        assert!(saved.get("pending_counter_removals").is_none());
        let restored: ResolutionStateWire =
            serde_json::from_value(saved).expect("v2 CounterRemovals prompt restores");
        let mut state = restored.into_game_state();

        for _ in 0..8 {
            if !matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }) {
                break;
            }
            apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
                .expect("production replacement action resumes the counter-removals queue");
            if matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }) {
                assert!(matches!(
                    state.resolution_stack.last(),
                    Some(ResolutionFrame::CounterRemovals(_))
                ));
            }
        }

        assert!(state.active_counter_removals().is_none());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.last_effect_count, Some(2));
        assert!(state.objects[&source_id].counters.is_empty());
    }

    /// CR 122.1 + CR 616.1: the production multi-target counter-addition
    /// resolver parks its remaining recipients and completion in CounterAdditions
    /// while each recipient's placement chooses among noncommuting replacements.
    /// v2 restores that real prompt before production replacement actions finish
    /// the queue.
    #[test]
    fn counter_additions_queue_reparks_and_roundtrips_v2_at_replacement_choice() {
        let mut state = GameState::new_two_player(42);
        install_noncommuting_counter_replacements(&mut state);
        let first = create_object(
            &mut state,
            CardId(913),
            PlayerId(0),
            "First Counter Recipient".to_string(),
            Zone::Battlefield,
        );
        let second = create_object(
            &mut state,
            CardId(914),
            PlayerId(0),
            "Second Counter Recipient".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(first), TargetRef::Object(second)],
            ObjectId(915),
            PlayerId(0),
        );

        resolve_add(&mut state, &ability, &mut Vec::new())
            .expect("production counter-addition resolver creates its first prompt");
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert!(matches!(
            state.resolution_stack.last(),
            Some(ResolutionFrame::CounterAdditions(_))
        ));
        assert_eq!(
            state
                .active_counter_additions()
                .expect("counter-additions queue owns its prompt")
                .remaining
                .len(),
            1
        );

        let saved = serde_json::to_value(ResolutionStateWire::from_game_state(state))
            .expect("paused CounterAdditions prompt serializes as v2");
        assert_eq!(saved["resolution_state_version"], 2);
        assert!(saved.get("pending_counter_additions").is_none());
        let restored: ResolutionStateWire =
            serde_json::from_value(saved).expect("v2 CounterAdditions prompt restores");
        let mut state = restored.into_game_state();

        for _ in 0..8 {
            if !matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }) {
                break;
            }
            apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
                .expect("production replacement action resumes the counter-additions queue");
            if matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }) {
                assert!(matches!(
                    state.resolution_stack.last(),
                    Some(ResolutionFrame::CounterAdditions(_))
                ));
            }
        }

        assert!(state.active_counter_additions().is_none());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        for object_id in [first, second] {
            assert!(
                state.objects[&object_id]
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0)
                    >= 3,
                "each recipient must resolve through both counter replacements"
            );
        }
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

        let queued = state.active_counter_moves().unwrap();
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

    /// CR 614.1a: No-regression guard for the actor/recipient subject axis. A
    /// Doubling-Season-class doubler (`valid_card: SelfRef`, no `valid_player`,
    /// default `Recipient` subject) must fire regardless of *who* puts the
    /// counters — the recipient axis is orthogonal to the actor. Hostile
    /// fixture: an opponent (P1) is the actor placing counters on a permanent
    /// carrying the doubler, and the counters still double. This must stay green
    /// both before and after the Vorinclex actor-scope change.
    #[test]
    fn selfref_doubler_fires_regardless_of_counter_actor() {
        use crate::types::ability::{
            CounterReplacementSubject, QuantityModification, ReplacementDefinition, TargetFilter,
        };
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Self Doubler".to_string(),
            Zone::Battlefield,
        );
        let mut repl = ReplacementDefinition::new(ReplacementEvent::AddCounter);
        repl.valid_card = Some(TargetFilter::SelfRef);
        repl.quantity_modification = Some(QuantityModification::DOUBLE);
        assert_eq!(
            repl.counter_replacement_subject,
            CounterReplacementSubject::Recipient,
            "a hand-built AddCounter replacement must default to Recipient subject"
        );
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .replacement_definitions
            .push(repl);

        let mut events = Vec::new();
        // Hostile: the OPPONENT (P1) is the actor putting the counters on P0's
        // permanent. A recipient-scoped doubler must still apply.
        add_counter_with_replacement(
            &mut state,
            PlayerId(1),
            obj_id,
            CounterType::Plus1Plus1,
            2,
            &mut events,
        );

        assert_eq!(
            state.objects[&obj_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(4),
            "recipient-scoped SelfRef doubler must double regardless of the actor"
        );
    }

    /// CR 614.1a: THE discriminating test for the actor-vs-recipient subject
    /// axis (Step 3 runtime scoping). Vorinclex's "If you would put …, put twice
    /// that many" doubles the counters *you* put — even on a permanent an
    /// opponent controls (official Vorinclex, Monstrous Raider ruling). This is
    /// the only configuration where the axis is observable: the actor (P0)
    /// differs from the recipient's controller (P1).
    ///
    /// Revert-failing assertion: with `subject = Actor`, the doubler compares the
    /// actor (P0) to Vorinclex's controller (P0) → `You` matches → doubles → 4.
    /// Reverting to `Recipient` would compare the recipient's controller (P1) to
    /// P0 → `You` fails → no doubling → 2.
    #[test]
    fn actor_scoped_doubler_applies_when_actor_differs_from_recipient() {
        use crate::types::ability::{
            CounterReplacementSubject, QuantityModification, ReplacementDefinition,
            ReplacementPlayerScope, TargetFilter,
        };
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        // P0 controls Vorinclex: doubles the counters P0 puts, anywhere.
        let vorinclex = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Vorinclex".to_string(),
            Zone::Battlefield,
        );
        let mut doubling = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .valid_card(TargetFilter::Any)
            .quantity_modification(QuantityModification::DOUBLE)
            .counter_subject(CounterReplacementSubject::Actor);
        doubling.valid_player = Some(ReplacementPlayerScope::You);
        state
            .objects
            .get_mut(&vorinclex)
            .unwrap()
            .replacement_definitions
            .push(doubling);

        // P1's creature receives the counters; P0 is the actor placing them.
        let opp_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Bear".to_string(),
            Zone::Battlefield,
        );

        let mut events = Vec::new();
        add_counter_with_replacement(
            &mut state,
            PlayerId(0), // actor = P0 (Vorinclex's controller)
            opp_creature,
            CounterType::Plus1Plus1,
            2,
            &mut events,
        );

        assert_eq!(
            state.objects[&opp_creature]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(4),
            "actor-scoped You doubler must double counters P0 puts, even on P1's creature \
             (reverting subject to Recipient scopes by P1 and yields 2)"
        );
    }

    /// CR 614.1a: Negative sibling — an opponent (P1) putting counters on their
    /// own creature is NOT doubled by P0's `You`-scoped Vorinclex, because the
    /// actor (P1) is not "you" relative to Vorinclex's controller (P0).
    #[test]
    fn actor_scoped_you_doubler_ignores_opponent_actor() {
        use crate::types::ability::{
            CounterReplacementSubject, QuantityModification, ReplacementDefinition,
            ReplacementPlayerScope, TargetFilter,
        };
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let vorinclex = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Vorinclex".to_string(),
            Zone::Battlefield,
        );
        let mut doubling = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .valid_card(TargetFilter::Any)
            .quantity_modification(QuantityModification::DOUBLE)
            .counter_subject(CounterReplacementSubject::Actor);
        doubling.valid_player = Some(ReplacementPlayerScope::You);
        state
            .objects
            .get_mut(&vorinclex)
            .unwrap()
            .replacement_definitions
            .push(doubling);

        let p1_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "P1 Bear".to_string(),
            Zone::Battlefield,
        );

        let mut events = Vec::new();
        add_counter_with_replacement(
            &mut state,
            PlayerId(1), // actor = P1 (an opponent of Vorinclex's controller)
            p1_creature,
            CounterType::Plus1Plus1,
            2,
            &mut events,
        );

        assert_eq!(
            state.objects[&p1_creature]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(2),
            "a You-scoped doubler must not double the opponent's own counter placement"
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

    // ─── CountersCantBeRemoved gate tests ───────────────────────────────────

    /// Install a CountersCantBeRemoved(Stun) static on `source_id` that
    /// protects permanents controlled by the source's opponents.
    fn install_counters_cant_be_removed_static(state: &mut GameState, source_id: ObjectId) {
        use crate::types::ability::{ControllerRef, StaticDefinition, TargetFilter, TypedFilter};
        use crate::types::statics::StaticMode;
        let def = StaticDefinition::new(StaticMode::CountersCantBeRemoved {
            counter_type: CounterType::Stun,
        })
        .affected(TargetFilter::Typed(
            TypedFilter::permanent().controller(ControllerRef::Opponent),
        ));
        let obj = state.objects.get_mut(&source_id).unwrap();
        obj.static_definitions.push(def);
    }

    /// CR 101.2: `remove_counter_with_replacement` is the single authority for
    /// counter removal. When `CountersCantBeRemoved` prohibits removal, the
    /// counter must remain and no event must fire.
    #[test]
    fn remove_counter_with_replacement_blocked_by_counters_cant_be_removed() {
        let mut state = GameState::new_two_player(42);

        // Player 0 controls the prohibition source (enchantment).
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Fear of Sleep Paralysis".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Enchantment);
        install_counters_cant_be_removed_static(&mut state, source);

        // Player 1 controls a creature with a stun counter.
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Stunned Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .counters
            .insert(CounterType::Stun, 2);

        let mut events = Vec::new();
        remove_counter_with_replacement(&mut state, target, CounterType::Stun, 1, &mut events);

        // Counter must remain unchanged.
        assert_eq!(
            state.objects[&target]
                .counters
                .get(&CounterType::Stun)
                .copied(),
            Some(2),
            "stun counter must not be removed when blocked by CountersCantBeRemoved"
        );
        // No removal event.
        assert!(
            !events.iter().any(|e| matches!(
                e,
                GameEvent::CounterRemoved { object_id, counter_type, .. }
                    if *object_id == target && *counter_type == CounterType::Stun
            )),
            "no CounterRemoved event when removal is blocked"
        );
    }

    /// Inverse: without the prohibition, `remove_counter_with_replacement`
    /// removes the counter normally.
    #[test]
    fn remove_counter_with_replacement_succeeds_without_prohibition() {
        let mut state = GameState::new_two_player(42);

        let target = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Stunned Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .counters
            .insert(CounterType::Stun, 1);

        let mut events = Vec::new();
        remove_counter_with_replacement(&mut state, target, CounterType::Stun, 1, &mut events);

        // Counter must be removed.
        assert!(
            !state.objects[&target]
                .counters
                .contains_key(&CounterType::Stun),
            "stun counter must be removed when no prohibition exists"
        );
        // Removal event must fire.
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::CounterRemoved { object_id, counter_type, .. }
                    if *object_id == target && *counter_type == CounterType::Stun
            )),
            "CounterRemoved event must fire for baseline removal"
        );
    }

    /// CR 101.2: Moving counters away is removal from the source. When
    /// `CountersCantBeRemoved` protects the source, the move must be blocked.
    #[test]
    fn move_counter_blocked_by_counters_cant_be_removed() {
        let mut state = GameState::new_two_player(42);

        // Player 0 controls the prohibition source.
        let prohib = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Fear of Sleep Paralysis".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&prohib)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Enchantment);
        install_counters_cant_be_removed_static(&mut state, prohib);

        // Player 1 controls a creature with a stun counter (protected).
        let source_perm = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Stunned Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_perm)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state
            .objects
            .get_mut(&source_perm)
            .unwrap()
            .counters
            .insert(CounterType::Stun, 1);

        // Destination for the move.
        let dest = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Destination".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&dest)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut events = Vec::new();
        let result = move_counter_with_replacement(
            &mut state,
            PlayerId(1),
            source_perm,
            dest,
            CounterType::Stun,
            1,
            &mut events,
        );

        // Move returns true ("complete, nothing happened") but counter stays.
        assert!(result, "move must return true when blocked");
        assert_eq!(
            state.objects[&source_perm]
                .counters
                .get(&CounterType::Stun)
                .copied(),
            Some(1),
            "stun counter must remain on source when move is blocked"
        );
        assert!(
            !state.objects[&dest]
                .counters
                .contains_key(&CounterType::Stun),
            "destination must not receive the counter when move is blocked"
        );
    }

    /// Inverse: without the prohibition, counter moves succeed normally.
    #[test]
    fn move_counter_succeeds_without_prohibition() {
        let mut state = GameState::new_two_player(42);

        let source_perm = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Stunned Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_perm)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state
            .objects
            .get_mut(&source_perm)
            .unwrap()
            .counters
            .insert(CounterType::Stun, 1);

        let dest = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Destination".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&dest)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut events = Vec::new();
        let result = move_counter_with_replacement(
            &mut state,
            PlayerId(1),
            source_perm,
            dest,
            CounterType::Stun,
            1,
            &mut events,
        );

        assert!(result, "move must succeed without prohibition");
        assert!(
            !state.objects[&source_perm]
                .counters
                .contains_key(&CounterType::Stun),
            "stun counter must be removed from source after move"
        );
        assert_eq!(
            state.objects[&dest]
                .counters
                .get(&CounterType::Stun)
                .copied(),
            Some(1),
            "destination must receive the counter after move"
        );
    }

    /// COHERENCE INVARIANT for EVERY route that defers a token-entry emit past a pause: **none
    /// reports a creation the others do not.** The tuple is `(TokenCreated events,
    /// created_tokens_this_turn rows, last_created_token_ids entries)` — all THREE ledgers a token
    /// creation writes, not the two that share a guard.
    ///
    /// The third entry is why this assertion is a 3-tuple. Gating the emit on the authority's
    /// `record.is_some()` verdict made the event agree with `created_tokens_this_turn` and
    /// `players_who_created_token_this_turn`, but `last_created_token_ids` — the
    /// `TargetFilter::LastCreated` anaphora slot — was written unguarded on every one of these
    /// routes, so the gone path read `(0, 0, 1)`: the change FLIPPED which ledger the event
    /// disagreed with instead of removing the disagreement, and a 2-tuple is exactly the projection
    /// under which that is invisible. `token::record_last_created_token` now carries the same
    /// existence predicate; MEASURED at the pre-fix tip, this test fails on its first arm with
    /// `left: (0, 0, 1)` against `right: (0, 0, 0)`.
    ///
    /// `restrictions::record_token_created` is existence-guarded, so if the emit is not gated on
    /// the same predicate a vanished token puts a live trigger event
    /// (`trigger_matchers::match_token_created`, keyed in `trigger_index`) on the wire that no
    /// ledger row backs — and that matcher then skips its CR 111.2 controller filter entirely, so
    /// it fires for a controller it should have rejected (measurement in
    /// `token::push_committed_token_entry_events`'s doc). This restores the pre-change behaviour
    /// of the deleted `counters::push_token_entry_events`, whose `let Some(obj) = … else { return;
    /// }` head emitted nothing on the gone path.
    ///
    /// COVERAGE IS THE WHOLE CLASS, not an enumeration of files. The single predicate lives inside
    /// `token::push_committed_token_entry_events`, the ONLY production emit of
    /// `GameEvent::TokenCreated`, so every one of its EIGHT callers inherits it. The five arms
    /// below are the five callers whose emit is separated from the object's creation by a pause —
    /// i.e. every caller on which the object can genuinely be gone. The remaining three emit
    /// inside the same call that created the object —
    /// `token::apply_create_token_after_replacement_with_created_ids`,
    /// `gift_delivery::create_gift_token`, and
    /// `token_copy::apply_copy_token_after_replacement_with_created_ids` — and the latter two
    /// additionally `.expect(…)` the record so a vanished object panics rather than disagreeing
    /// silently.
    ///
    /// NO CR settles whether a token that never successfully entered should fire a creation
    /// trigger, because the situation is unreachable in rules terms: CR 704.3 checks state-based
    /// actions only "whenever a player would get priority", so nothing can remove the token between
    /// its creation and the CR 603.6a enters-the-battlefield check. The gone arm is a defensive
    /// engine artifact of deferring the emit past a replacement pause, and the engineering
    /// requirement on it is internal agreement, not a rules verdict.
    ///
    /// TWO-SIDED, each mutant failing the SAME gone-path assertion on EVERY arm:
    /// * MUTANT-DROP — delete the `if record.is_some()` in `push_committed_token_entry_events` ⇒
    ///   `token_created` reads 1 while both turn ledgers read 0.
    /// * MUTANT-TRIVIALIZE — keep the branch's shape, make it `if true` ⇒ same flip, same
    ///   assertion.
    /// * MUTANT-DROP-3 — delete the `contains_key` in `token::record_last_created_token` ⇒ the
    ///   gone path reads `(0, 0, 1)`, which is the pre-fix behaviour and the third entry's own
    ///   revert probe.
    ///
    /// All three leave the `(1, 1, 1)` positive control passing, so no mutant is caught merely by a
    /// fixture that never ran the arm.
    #[test]
    fn a_vanished_counter_paused_token_reports_neither_creation_event_nor_ledger_row() {
        /// The five routes whose token-entry emit is deferred past a pause. All are driven through
        /// their production entry point: every arm is a `PendingCounterPostAction` variant
        /// dispatched by `apply_pending_counter_post_action`, and the last one parks its entry
        /// through that same dispatcher and is then realized by
        /// `token::flush_pending_token_battlefield_entry`, exactly as
        /// `token::realize_settled_token_battlefield_entry` does from `apply_action`.
        enum Route {
            FinalizeTokenEntry,
            FinalizeCopyTokenEntry,
            ApplyCopyTokenModificationsAndFinalize,
            LiminalEntryEmit,
            LiminalEntryFlush,
        }

        fn run(route: &Route, present: bool) -> (usize, usize, usize) {
            let mut state = GameState::new_two_player(42);
            let source_id = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Token Source".to_string(),
                Zone::Battlefield,
            );
            let object_id = create_object(
                &mut state,
                CardId(0),
                PlayerId(0),
                "Saproling".to_string(),
                Zone::Battlefield,
            );
            let liminal_entry =
                |entry_events| PendingCounterPostAction::FinalizeCommittedLiminalTokenEntry {
                    object_id,
                    name: "Saproling".to_string(),
                    source_id,
                    controller: PlayerId(0),
                    enters_attacking: false,
                    attach_to: None,
                    sacrifice_at: None,
                    created_ids: Vec::new(),
                    ability_injection:
                        crate::types::game_state::LiminalTokenAbilityInjection::ResolvedToken,
                    entry_events,
                };
            let action = match route {
                Route::FinalizeCopyTokenEntry => PendingCounterPostAction::FinalizeCopyTokenEntry {
                    object_id,
                    name: "Saproling".to_string(),
                    enters_attacking: false,
                    source_id,
                    controller: PlayerId(0),
                },
                Route::FinalizeTokenEntry => PendingCounterPostAction::FinalizeTokenEntry {
                    object_id,
                    name: "Saproling".to_string(),
                    attach_to: None,
                    sacrifice_at: None,
                    source_id,
                    controller: PlayerId(0),
                },
                // Empty `remaining_modifications`: `apply_token_modifications` returns `true` on an
                // empty slice, so the resume reaches its emit tail — the statement under test —
                // without needing an `, except` body.
                Route::ApplyCopyTokenModificationsAndFinalize => {
                    PendingCounterPostAction::ApplyCopyTokenModificationsAndFinalize {
                        object_id,
                        name: "Saproling".to_string(),
                        enters_attacking: false,
                        source_id,
                        controller: PlayerId(0),
                        remaining_modifications: Vec::new(),
                    }
                }
                Route::LiminalEntryEmit => {
                    liminal_entry(crate::types::game_state::TokenEntryEventEmission::Emit)
                }
                Route::LiminalEntryFlush => {
                    liminal_entry(crate::types::game_state::TokenEntryEventEmission::Suppress)
                }
            };
            if !present {
                // CR 704.5f is the live way to get here (a 0-toughness copy buried while its
                // counter-ordering prompt was open); removing the object models the same end state
                // without staging a whole SBA pass. Removed BEFORE the resume, which is where the
                // window actually is: the object is committed to the battlefield before the
                // counter replacement pauses, so it can only vanish while the prompt is open.
                state.objects.remove(&object_id);
                state.battlefield.retain(|id| *id != object_id);
            }
            let mut events = Vec::new();
            apply_pending_counter_post_action(&mut state, action, &mut events);
            if matches!(route, Route::LiminalEntryFlush) {
                assert!(
                    state
                        .pending_token_battlefield_entry
                        .as_ref()
                        .is_some_and(|pending| pending.object_id == object_id),
                    "REACH-GUARD: the Suppress arm must have parked the entry, or the flush below \
                     is a no-op and its counts prove nothing"
                );
                assert!(
                    crate::game::effects::token::flush_pending_token_battlefield_entry(
                        &mut state,
                        object_id,
                        &mut events,
                    ),
                    "REACH-GUARD: the flush must consume the parked entry"
                );
            }
            (
                events
                    .iter()
                    .filter(|event| matches!(event, GameEvent::TokenCreated { .. }))
                    .count(),
                state.created_tokens_this_turn.len(),
                state.last_created_token_ids.len(),
            )
        }

        for (route, arm) in [
            (Route::FinalizeTokenEntry, "counters::FinalizeTokenEntry"),
            (
                Route::FinalizeCopyTokenEntry,
                "counters::FinalizeCopyTokenEntry",
            ),
            (
                Route::ApplyCopyTokenModificationsAndFinalize,
                "counters::ApplyCopyTokenModificationsAndFinalize -> \
                 token_copy::apply_remaining_token_modifications_after_counter_pause",
            ),
            (
                Route::LiminalEntryEmit,
                "counters::FinalizeCommittedLiminalTokenEntry (entry_events: Emit)",
            ),
            (
                Route::LiminalEntryFlush,
                "counters::FinalizeCommittedLiminalTokenEntry (entry_events: Suppress) -> \
                 token::flush_pending_token_battlefield_entry",
            ),
        ] {
            // POSITIVE CONTROL — the instrument can report non-zero, so the zeros below are a
            // measurement and not a fixture that never ran the arm.
            assert_eq!(
                run(&route, true),
                (1, 1, 1),
                "{arm}: with the token still on the battlefield the route emits exactly one \
                 TokenCreated AND writes exactly one created_tokens_this_turn row AND publishes \
                 exactly one last_created_token_ids entry"
            );

            // THE INVARIANT: event and ALL THREE ledgers agree on the gone path too.
            assert_eq!(
                run(&route, false),
                (0, 0, 0),
                "{arm}: a vanished token must emit NO TokenCreated and appear in NO token-creation \
                 ledger, because the object it names does not exist. Dropping the \
                 `if record.is_some()` in `push_committed_token_entry_events` yields (1, 0, 0) — a \
                 live creation trigger event backed by an empty ledger. Dropping \
                 `token::record_last_created_token`'s existence guard yields (0, 0, 1) — a dead \
                 object id published into the `TargetFilter::LastCreated` anaphora slot"
            );
        }
    }

    /// The SIXTH `last_created_token_ids` writer, and the one the five arms above do not reach:
    /// `PendingCounterPostAction::EmitCommittedCopyTokenEntry`.
    ///
    /// Its shape differs, which is why it gets its own assertion rather than a sixth arm. The
    /// CR 400.7 row and the `created_tokens_this_turn` write happen UPSTREAM of the counter pause
    /// on this route, so the 3-tuple invariant above does not describe it — on the gone path this
    /// variant legitimately leaves an earlier turn-ledger row alone and its `flush` is a no-op.
    /// What DOES apply is the third ledger on its own: a `TargetFilter::LastCreated` reference must
    /// never name an object that is no longer in `state.objects`.
    ///
    /// Two-sided on the guard it covers: deleting `contains_key` in
    /// `token::record_last_created_token` flips the gone row from 0 to 1 while leaving the positive
    /// control at 1, so the assertion discriminates rather than merely running.
    #[test]
    fn a_vanished_copy_token_is_not_published_to_the_last_created_anaphora_slot() {
        fn run(present: bool) -> (bool, usize) {
            let mut state = GameState::new_two_player(42);
            let object_id = create_object(
                &mut state,
                CardId(0),
                PlayerId(0),
                "Copy Token".to_string(),
                Zone::Battlefield,
            );
            assert!(
                state.last_created_token_ids.is_empty(),
                "REACH-GUARD: the slot must start empty, or the count below is not attributable to \
                 this dispatch"
            );
            if !present {
                state.objects.remove(&object_id);
                state.battlefield.retain(|id| *id != object_id);
            }
            let mut events = Vec::new();
            let handled = apply_pending_counter_post_action(
                &mut state,
                PendingCounterPostAction::EmitCommittedCopyTokenEntry { object_id },
                &mut events,
            );
            (
                handled,
                state
                    .last_created_token_ids
                    .iter()
                    .filter(|id| **id == object_id)
                    .count(),
            )
        }

        // POSITIVE CONTROL — this dispatch does publish the slot, so the zero below is a
        // measurement and not a variant that was never matched.
        assert_eq!(
            run(true),
            (true, 1),
            "EmitCommittedCopyTokenEntry with the token present must publish it exactly once to \
             `last_created_token_ids`"
        );
        assert_eq!(
            run(false),
            (true, 0),
            "EmitCommittedCopyTokenEntry with the token gone must publish NOTHING: \
             `TargetFilter::LastCreated` resolves through `state.objects`, so a dead id here is a \
             \"the token you created\" reference to an object that never finished entering"
        );
    }

    /// The COPY-BATCH BUFFER half of the same invariant, and the one every test above is
    /// structurally blind to.
    ///
    /// WHY A SEPARATE TEST RATHER THAN A SIXTH ARM. Every fixture above builds
    /// `GameState::new_two_player(42)`, whose resolution stack has no `ResolutionFrame::CopyToken`,
    /// so `state.active_copy_token_mut()` returns `None` and the copy-batch branch these two routes
    /// carry is NEVER ENTERED. Those tests fail when the ledger-3 guard is reverted, which proves
    /// they are sensitive to the lines that changed — it does not prove they reach the branch where
    /// the guard can be defeated. Sensitivity is not coverage, and the gap it left was real: the
    /// guard shipped with an UNGUARDED `pending.created_ids.push(id)` one line below it at both
    /// sites, republishing the id the guard had just withheld.
    ///
    /// WHAT MAKES THE BUFFER LOAD-BEARING: `token_copy::drain_copy_token_resolution` ends with
    /// `state.last_created_token_ids = pending.created_ids;` — an ASSIGNMENT. So a dead id in the
    /// buffer does not merely duplicate ledger 3, it OVERWRITES the guarded ledger with the
    /// unguarded list. This test therefore measures the state after that drain, which is the
    /// user-visible end state a `TargetFilter::LastCreated` reference actually reads.
    ///
    /// REACHABILITY IS DEMONSTRATED, NOT ASSERTED. The frame is seeded with a SURVIVOR id that
    /// exists only in the buffer, never in ledger 3 before the drain. `survivor_rows == 1` after
    /// the drain is reachable only if the `Some(pending)` branch's container was published, so
    /// deleting the `if let Some(pending)` body inside
    /// `token::record_last_created_copy_batch_token` fails the PRESENT row (the token's own id
    /// never reaches the buffer, so it is gone after the wholesale assign) while the survivor
    /// column still proves the drain itself ran.
    ///
    /// TWO-SIDED, each mutant failing the SAME named assertion — the `gone` row's `dead_rows == 0`:
    /// * MUTANT-DROP — delete the `if !record_last_created_token(…) { return; }` early return in
    ///   `token::record_last_created_copy_batch_token`, i.e. the exact shape that shipped.
    /// * MUTANT-TRIVIALIZE — keep every branch, replace `state.objects.contains_key(&object_id)`
    ///   in `token::record_last_created_token` with `true`.
    ///
    /// Both leave the PRESENT row passing, so neither is caught by a fixture that never ran.
    #[test]
    fn a_vanished_token_never_reaches_the_anaphora_slot_through_the_copy_batch_buffer() {
        use crate::types::game_state::PendingCopyTokenResolution;
        use std::collections::VecDeque;

        /// The two routes that publish a single just-created id while a copy batch is in flight.
        enum Route {
            /// `counters.rs`'s own arm.
            FinalizeCopyTokenEntry,
            /// Dispatched by `counters.rs` into
            /// `token_copy::apply_remaining_token_modifications_after_counter_pause`.
            ApplyCopyTokenModificationsAndFinalize,
        }

        /// `(dead id rows in the buffer, dead id rows in ledger 3 after the drain, survivor rows in
        /// ledger 3 after the drain)`.
        fn run(route: &Route, present: bool) -> (usize, usize, usize) {
            let mut state = GameState::new_two_player(42);
            let source_id = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Token Source".to_string(),
                Zone::Battlefield,
            );
            // An earlier batch's token. It lives ONLY in the buffer, so its presence in ledger 3
            // after the drain is proof the buffer was published there.
            let survivor = create_object(
                &mut state,
                CardId(2),
                PlayerId(0),
                "Earlier Copy".to_string(),
                Zone::Battlefield,
            );
            let object_id = create_object(
                &mut state,
                CardId(0),
                PlayerId(0),
                "Copy Token".to_string(),
                Zone::Battlefield,
            );
            state.push_copy_token(PendingCopyTokenResolution {
                created_ids: vec![survivor],
                remaining: VecDeque::new(),
                effect_kind: EffectKind::CopyTokenOf,
                source_id,
            });
            assert!(
                state.active_copy_token().is_some(),
                "REACH-GUARD: without an active CopyToken frame the branch under test is dead code \
                 and every count below would be vacuous"
            );
            assert!(
                state.last_created_token_ids.is_empty(),
                "REACH-GUARD: ledger 3 must start empty, or the rows below are not attributable to \
                 this dispatch"
            );
            let action = match route {
                Route::FinalizeCopyTokenEntry => PendingCounterPostAction::FinalizeCopyTokenEntry {
                    object_id,
                    name: "Copy Token".to_string(),
                    enters_attacking: false,
                    source_id,
                    controller: PlayerId(0),
                },
                Route::ApplyCopyTokenModificationsAndFinalize => {
                    PendingCounterPostAction::ApplyCopyTokenModificationsAndFinalize {
                        object_id,
                        name: "Copy Token".to_string(),
                        enters_attacking: false,
                        source_id,
                        controller: PlayerId(0),
                        remaining_modifications: Vec::new(),
                    }
                }
            };
            if !present {
                // CR 704.5f: the live shape is a 0-toughness copy buried while its counter-ordering
                // prompt was open. Removed BEFORE the resume, which is where the window is.
                state.objects.remove(&object_id);
                state.battlefield.retain(|id| *id != object_id);
            }
            let mut events = Vec::new();
            apply_pending_counter_post_action(&mut state, action, &mut events);
            let buffered = state
                .active_copy_token()
                .expect("REACH-GUARD: the dispatch must not consume the CopyToken frame")
                .created_ids
                .iter()
                .filter(|id| **id == object_id)
                .count();
            // The frame has no remaining batches, so this drain is exactly the terminal
            // `state.last_created_token_ids = pending.created_ids;` assignment and nothing else.
            crate::game::effects::token_copy::drain_pending_copy_token_resolution(
                &mut state,
                &mut events,
            );
            let rows = |wanted: ObjectId| {
                state
                    .last_created_token_ids
                    .iter()
                    .filter(|id| **id == wanted)
                    .count()
            };
            (buffered, rows(object_id), rows(survivor))
        }

        for (route, arm) in [
            (
                Route::FinalizeCopyTokenEntry,
                "counters::FinalizeCopyTokenEntry",
            ),
            (
                Route::ApplyCopyTokenModificationsAndFinalize,
                "counters::ApplyCopyTokenModificationsAndFinalize -> \
                 token_copy::apply_remaining_token_modifications_after_counter_pause",
            ),
        ] {
            // POSITIVE CONTROL + REACHABILITY DEMONSTRATION. The buffer column can only be 1 if the
            // `Some(pending)` branch executed, and the survivor column can only be 1 if the drain
            // published that buffer onto ledger 3.
            assert_eq!(
                run(&route, true),
                (1, 1, 1),
                "{arm}: with the token present it must reach the copy batch's `created_ids` AND \
                 survive the drain's wholesale assignment onto `last_created_token_ids`, alongside \
                 the earlier batch's token. Deleting the `if let Some(pending)` body in \
                 `token::record_last_created_copy_batch_token` drops this to (0, 0, 1)"
            );

            // THE INVARIANT.
            assert_eq!(
                run(&route, false),
                (0, 0, 1),
                "{arm}: a vanished token must not enter the copy batch's `created_ids`, because \
                 the drain assigns that buffer WHOLESALE onto `last_created_token_ids` — so an \
                 unguarded buffer push both republishes the id ledger 3's guard withheld and \
                 destroys the guarded ledger. The survivor row must stay 1: the drain still runs \
                 and still publishes, which is what makes the two zeros a measurement of the guard \
                 rather than of a drain that never happened"
            );
        }
    }

    /// Building-block rows for `park_counter_completion_outside_active_direct_choice`
    /// (issue #7384). A post-action may pause having installed a direct-choice
    /// owner; `ResolutionStack::validate` rejects a buried direct-choice owner,
    /// so the completion that outlives the pause may not simply be pushed on top
    /// of it.
    mod parking_a_completion_after_a_paused_post_action {
        use super::*;
        use crate::types::game_state::{PendingEffectResolutionEvent, PendingEffectResolved};
        use crate::types::resolution::{FrameKind, PendingProliferateActions};

        fn live_proliferate_prompt() -> GameState {
            let mut state = GameState::new_two_player(42);
            state
                .install_direct_choice_frame(
                    ResolutionFrame::Proliferate(PendingProliferateActions {
                        actor: PlayerId(0),
                        source_id: ObjectId(77),
                        remaining: 1,
                    }),
                    WaitingFor::ProliferateChoice {
                        player: PlayerId(0),
                        eligible: vec![TargetRef::Player(PlayerId(0))],
                    },
                )
                .expect("a proliferate owner installs with its own prompt");
            state
        }

        fn owed_completion() -> PendingEffectResolved {
            PendingEffectResolved::new(EffectKind::Proliferate, ObjectId(77))
        }

        /// THE row this branch exists for: the completion goes BELOW the live
        /// prompt, and the resulting stack still validates. Pushing it on top
        /// instead is exactly the corruption #7384 reported.
        #[test]
        fn a_completion_owed_behind_a_live_prompt_becomes_its_parent() {
            let mut state = live_proliferate_prompt();

            merge_pending_counter_completion_after_nested_pause(&mut state, owed_completion());

            let frames: Vec<_> = state.resolution_stack.iter().map(|f| f.kind()).collect();
            assert_eq!(
                frames,
                vec![FrameKind::CounterAdditions, FrameKind::Proliferate],
                "the owed completion parks BELOW the direct-choice owner, which keeps \
                 the stack top — and so the prompt gate — untouched"
            );
            state
                .resolution_stack
                .validate(&state.waiting_for)
                .expect("a direct-choice owner with a parent completion is a valid stack");
        }

        /// A pause that owes nothing installs no frame at all — an empty owner
        /// above a live prompt would bury it for no benefit.
        #[test]
        fn a_completion_owing_nothing_behind_a_live_prompt_is_dropped() {
            let mut state = live_proliferate_prompt();
            let spent = PendingEffectResolved {
                resolution_event: PendingEffectResolutionEvent::Suppress,
                post_actions: Vec::new(),
                player_action: None,
                ..owed_completion()
            };
            assert!(spent.is_noop(), "the row's premise: nothing is owed");

            merge_pending_counter_completion_after_nested_pause(&mut state, spent);

            let frames: Vec<_> = state.resolution_stack.iter().map(|f| f.kind()).collect();
            assert_eq!(
                frames,
                vec![FrameKind::Proliferate],
                "nothing owed means nothing parked"
            );
        }

        /// The historical shape is preserved for every pause that did NOT
        /// install a direct-choice owner — including an empty completion, whose
        /// placeholder frame a later `append_pending_counter_post_actions` may
        /// still land work on.
        #[test]
        fn a_pause_without_a_live_prompt_still_pushes_its_placeholder_frame() {
            let mut state = GameState::new_two_player(42);
            let spent = PendingEffectResolved {
                resolution_event: PendingEffectResolutionEvent::Suppress,
                post_actions: Vec::new(),
                player_action: None,
                ..owed_completion()
            };
            assert!(spent.is_noop());

            merge_pending_counter_completion_after_nested_pause(&mut state, spent);

            let frames: Vec<_> = state.resolution_stack.iter().map(|f| f.kind()).collect();
            assert_eq!(
                frames,
                vec![FrameKind::CounterAdditions],
                "a non-direct-choice pause keeps the placeholder frame it has always \
                 pushed, so a later append still has a queue to land on"
            );
        }
    }
}
