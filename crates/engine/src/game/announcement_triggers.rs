//! Trigger custody for an announced spell or activated ability.
//!
//! CR 601.2c + CR 602.2b + CR 603.2: a target becomes a target of a spell or
//! ability as it is announced, and an ability whose trigger event that is
//! triggers then. CR 603.3: the triggered ability waits to be put on the stack
//! until a player would receive priority, i.e. until the announcement has
//! finished. An announcement can pause for player input (mana payment, a cost
//! choice, a division, a replacement choice) after such an event, so the
//! event's buffer can end without reaching the priority pipeline that would
//! ordinarily scan it.
//!
//! This module is the single authority for those events: it stages them into
//! the announcement's private carrier (`PendingCast::activation_trigger_collection`),
//! keeps that carrier with whichever [`PendingCastOwner`] holds the
//! announcement, and commits or drops it when the announcement ends.

use crate::types::events::GameEvent;
use crate::types::game_state::{
    AnnouncementEventScope, GameState, InFlightSpellTriggerCarrier, PendingCast, PendingCastOwner,
    PendingCostMoveResume, WaitingFor,
};
use crate::types::identifiers::ObjectId;

use super::triggers::{
    self, ConsumedTriggerEventOccurrence, ConsumedTriggerEventScope,
    PendingActivationTriggerCollection,
};

/// The live announcement, if it is a spell rather than an activation.
fn live_spell_announcement(state: &GameState) -> Option<ObjectId> {
    let owner = state.live_pending_cast_owners().into_iter().next()?;
    let pending = state.pending_cast_at(owner)?;
    pending
        .activation_ability_index
        .is_none()
        .then_some(pending.object_id)
}

/// Whether an ACTIVATION is announced right now, so its cost events have one
/// custodian: [`end_action`], which stages them into the carrier of the
/// action's resulting prompt.
///
/// CR 602.2a + CR 602.2b: an activated ability is announced like a spell and
/// its costs are paid in the announcement. CR 603.3b: every ability that
/// triggered since a player last received priority goes on the stack in one
/// APNAP batch, so an activation's cost triggers wait for the activation's own
/// and are put on the stack with them. A cost-payment step that parks those
/// events itself (publicly, or in a carrier the action boundary then
/// overwrites) splits that batch or loses them. A spell is deliberately not
/// covered: its paid-cost events stay parked publicly and survive a cancel;
/// routing them into a spell carrier would drop them on cancel, which needs the
/// announcement-reversal machinery and is out of scope here. (Engine policy,
/// compare CR 733.1: an activation is not offered a cancel once a cost is paid,
/// so its carrier is not dropped with paid-cost events in it.)
///
/// `next_wait` is the prompt the paying step is about to return; the live
/// announcement is read from the state and from that prompt, since a step that
/// pauses on the activation's first action has not stored it yet.
pub(crate) fn activation_announcement_is_live(state: &GameState, next_wait: &WaitingFor) -> bool {
    state
        .pending_trigger_carrier_owner()
        .and_then(|owner| state.pending_cast_at(owner))
        .or_else(|| next_wait.pending_cast_ref())
        .is_some_and(|pending| pending.activation_ability_index.is_some())
}

/// The unscanned events in `events[start..]`, after occurrence claims and the
/// queued zone-change witness (CR 603.2c: one trigger per occurrence).
pub(crate) fn unclaimed_events(
    state: &GameState,
    events: &[GameEvent],
    start: usize,
) -> Vec<GameEvent> {
    triggers::filter_already_collected_trigger_events_from(
        state,
        events,
        start,
        &state.consumed_before_priority_trigger_events,
    )
    .into_iter()
    .filter(|event| !matches!(event, GameEvent::PhaseChanged { .. }))
    .collect()
}

/// Claims `events[start..end]` for the ordinary collector. The carrier
/// collects ordinary triggered abilities only, so the claim is
/// `OrdinaryCollectorsOnly`: delayed triggered abilities (CR 603.7b) still see
/// the occurrences exactly as they would without a carrier.
fn claim_ordinary(state: &mut GameState, events: &[GameEvent], start: usize, end: usize) {
    let end = end.min(events.len());
    claim_indices(
        state,
        events,
        start..end,
        ConsumedTriggerEventScope::OrdinaryCollectorsOnly,
    );
}

/// Records the occurrences at `indices` of the full action buffer `events`.
fn claim_indices(
    state: &mut GameState,
    events: &[GameEvent],
    indices: impl IntoIterator<Item = usize>,
    scope: ConsumedTriggerEventScope,
) {
    let occurrences: Vec<ConsumedTriggerEventOccurrence> = indices
        .into_iter()
        .map(|index| ConsumedTriggerEventOccurrence {
            event: events[index].clone(),
            occurrence: triggers::trigger_event_occurrence(events, index),
            scope,
        })
        .collect();
    state
        .consumed_before_priority_trigger_events
        .extend(occurrences);
}

/// Records that an ordinary collector outside this module (a paused cost span)
/// already collected `events[start..end]`, so announcement staging at the end
/// of the action does not collect them a second time.
pub(crate) fn claim_ordinary_collected_span(
    state: &mut GameState,
    events: &[GameEvent],
    start: usize,
    end: usize,
) {
    claim_ordinary(state, events, start, end);
}

/// Events a typed cost root still owns for its own settlement once every
/// selected object has moved (CR 603.2c: collected once, by that owner).
fn retained_by_cost_owners(state: &GameState) -> Vec<GameEvent> {
    let mut retained = super::engine_priority::retained_logical_zone_events(state);
    match state.pending_cost_move_resume.as_ref() {
        Some(
            PendingCostMoveResume::Cast {
                deferred_cost_events,
                ..
            }
            | PendingCostMoveResume::SacrificeForCost {
                deferred_cost_events,
                ..
            },
        ) => retained.extend(deferred_cost_events.iter().cloned()),
        Some(PendingCostMoveResume::ManaAbilityPayment { cursor, .. }) => {
            retained.extend(cursor.deferred_cost_events.iter().cloned());
        }
        Some(
            PendingCostMoveResume::WardSacrificePayment { .. }
            | PendingCostMoveResume::ReplacementMayCost { .. }
            | PendingCostMoveResume::Foretell { .. }
            | PendingCostMoveResume::CollectEvidencePayment { .. }
            | PendingCostMoveResume::UnlessBouncePayment { .. }
            | PendingCostMoveResume::ActivationMillPayment { .. }
            | PendingCostMoveResume::CounterCostRemovalPayment { .. }
            | PendingCostMoveResume::LoyaltyActivation { .. }
            | PendingCostMoveResume::CounterAdditionUnlessPayment { .. }
            | PendingCostMoveResume::RandomDiscardUnlessPayment(..)
            | PendingCostMoveResume::TurnFaceUpCostMove { .. },
        )
        | None => {}
    }
    retained
}

/// Removes the in-flight carrier of the announcing spell `object_id`.
fn take_announcing(
    state: &mut GameState,
    object_id: ObjectId,
) -> Option<Box<PendingActivationTriggerCollection>> {
    state
        .in_flight_spell_trigger_carrier
        .take_if(|carrier| carrier.object_id == object_id)
        .map(|carrier| carrier.collection)
}

/// CR 601.2h + CR 602.2b + CR 603.3: whether a live announcement owns
/// `events[start..]` of the current action buffer — exactly the condition under
/// which [`stage_announcement_trigger_events`] takes custody. `Some` holds the
/// first index the announcement owns (`start`, or later when the announcement
/// began part-way through the span).
fn announcement_owned_from(state: &GameState, start: usize) -> Option<usize> {
    let AnnouncementEventScope::From { start: owned_from } = state.announcement_event_scope else {
        return None;
    };
    state.pending_trigger_carrier_owner()?;
    Some(start.max(owned_from))
}

/// CR 601.2h + CR 602.2b + CR 603.3 + CR 605.4a: the entry for a collector
/// that scans `events[start..]` of the current action buffer inline (a
/// resolution choice, a mana-colour choice, a change-zone iteration). Under a
/// live announcement those events wait in its carrier until the announcement
/// is complete, so this stages and claims the owned part and returns the
/// sub-range the caller still collects itself: empty when the announcement
/// owns the whole span, the whole span when none is live. The caller then does
/// no inline scan, no deferred collection and no `triggers_processed_inline`
/// for what was taken; events a cost root retains are left to that owner.
///
/// Triggered mana abilities still resolve immediately (CR 605.4a): those of a
/// `TapsForMana` or `ManaAbilityProduced` trigger resolve here before custody
/// is taken. The residual is a `ManaAdded`-mode triggered mana ability, which
/// the carrier holds until the announcement's drain; classifying the span's
/// contexts through `triggers::partition_collected_mana_frame_contexts` is the
/// follow-up route.
pub(crate) fn take_inline_trigger_span(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    start: usize,
) -> std::ops::Range<usize> {
    let Some(owned_from) = announcement_owned_from(state, start) else {
        return start..events.len();
    };
    triggers::resolve_tap_mana_triggers_inline(state, events, start);
    stage_announcement_trigger_events(state, events, start);
    start..owned_from.min(events.len())
}

/// CR 601.2c + CR 603.2: stage a spell's announcement triggers while
/// it owns `state.pending_cast` (the spell is still in flight and has not yet
/// parked in an external payment root or waiting prompt), so the staging
/// authority sees the spell before payment can move its targets.
pub(crate) fn stage_with_pending_owner(
    state: &mut GameState,
    pending: &PendingCast,
    events: &[GameEvent],
    event_start: usize,
) {
    let prior_pending = state.pending_cast.replace(Box::new(pending.clone()));
    stage_announcement_trigger_events(state, events, event_start);
    state.pending_cast = prior_pending;
}

/// Stages the not-yet-collected events of `events[event_start..]` into the
/// live announcement's carrier. A no-op when no announcement is live.
///
/// Called while the announcement is still pending: at the start of a
/// post-action pipeline pass and at the end of every action that leaves the
/// announcement paused.
pub(crate) fn stage_announcement_trigger_events(
    state: &mut GameState,
    events: &[GameEvent],
    event_start: usize,
) {
    let Some(start) = announcement_owned_from(state, event_start) else {
        return;
    };
    if start >= events.len() {
        return;
    }
    let Some(owner) = state.pending_trigger_carrier_owner() else {
        return;
    };
    let retained = retained_by_cost_owners(state);
    let new_events: Vec<GameEvent> = unclaimed_events(state, events, start)
        .into_iter()
        .filter(|event| !retained.contains(event))
        .collect();
    state.announcement_event_scope = state.announcement_event_scope.advanced_to(events.len());
    if new_events.is_empty() {
        return;
    }
    if let Some(object_id) = live_spell_announcement(state) {
        let mut collection = take_announcing(state, object_id)
            .or_else(|| {
                state
                    .pending_cast_at_mut(owner)
                    .and_then(|pending| pending.activation_trigger_collection.take())
            })
            .unwrap_or_else(|| Box::new(PendingActivationTriggerCollection::for_spell()));
        collection.collect(state, &new_events);
        debug_assert!(
            state.in_flight_spell_trigger_carrier.is_none(),
            "one announcement is in flight at a time"
        );
        state.in_flight_spell_trigger_carrier = Some(InFlightSpellTriggerCarrier {
            object_id,
            collection,
        });
    } else {
        let Some(pending) = state.pending_cast_at_mut(owner) else {
            return;
        };
        pending.begin_activation_trigger_collection();
        let mut collection = pending
            .activation_trigger_collection
            .take()
            .expect("begin_activation_trigger_collection installs a carrier");
        collection.collect(state, &new_events);
        state.restore_pending_activation_trigger_collection(collection);
    }
    claim_ordinary(state, events, start, events.len());
}

/// CR 601.2a + CR 602.2a: an announcement begins at `events_len` in the
/// current action's buffer. Called by the announcing authority (the spell's
/// stack announcement, the activation's announcement), so an announcement that
/// begins part-way through an action (a cast offered by a resolving ability)
/// owns only the events from its inception on.
pub(crate) fn begin_announcement(state: &mut GameState, events_len: usize) {
    if state.announcement_event_scope == AnnouncementEventScope::Unowned {
        state.announcement_event_scope = AnnouncementEventScope::From { start: events_len };
    }
}

/// CR 800.4a: the departure sweep owns its own events (objects leaving the
/// game). A surviving player's paused announcement does not.
pub(crate) fn disown_events_through(state: &mut GameState, end: usize) {
    state.announcement_event_scope = state.announcement_event_scope.advanced_to(end);
}

/// CR 602.2a + CR 602.2b + CR 700.2a: whether an announcement is already live
/// when an action begins. A live `PendingCastOwner` holds one; so does an
/// activated ability's mode prompt, because its modes are chosen as part of
/// activating it (CR 700.2a) and its `PendingCast` is built only once they are
/// known. A triggered ability's mode prompt is not an announcement.
fn announcement_live_at_action_entry(state: &GameState) -> bool {
    !state.live_pending_cast_owners().is_empty()
        || matches!(
            state.waiting_for,
            WaitingFor::AbilityModeChoice {
                is_activated: true,
                ..
            }
        )
}

/// Action entry: an announcement already live owns the action's buffer from
/// its start; otherwise none does until an announcement begins
/// ([`begin_announcement`]). A spell announcement's carrier also leaves its
/// `PendingCast` for the duration of the action, because spell continuations
/// rebuild their `PendingCast` without it. Activations thread their carrier
/// explicitly.
pub(crate) fn begin_action(state: &mut GameState) {
    debug_assert!(
        state.in_flight_spell_trigger_carrier.is_none(),
        "a spell trigger carrier outlived its action"
    );
    state.announcement_event_scope = if announcement_live_at_action_entry(state) {
        AnnouncementEventScope::From { start: 0 }
    } else {
        AnnouncementEventScope::Unowned
    };
    let Some(object_id) = live_spell_announcement(state) else {
        return;
    };
    let collection = state
        .pending_trigger_carrier_owner()
        .and_then(|owner| state.pending_cast_at_mut(owner))
        .and_then(|pending| pending.activation_trigger_collection.take());
    if let Some(collection) = collection {
        state.in_flight_spell_trigger_carrier = Some(InFlightSpellTriggerCarrier {
            object_id,
            collection,
        });
    }
}

/// Action exit (with `state.waiting_for` already holding the action's
/// result): stage what this action emitted if the announcement is still
/// pending, and return a spell's carrier to the owner now holding the
/// announcement. A carrier whose spell was cancelled is dropped (CR 733.1: no
/// abilities trigger as a result of an undone action).
pub(crate) fn end_action(state: &mut GameState, events: &[GameEvent]) {
    if !matches!(
        state.waiting_for,
        WaitingFor::Priority { .. } | WaitingFor::GameOver { .. }
    ) {
        stage_announcement_trigger_events(state, events, 0);
    }
    let Some(carrier) = state.in_flight_spell_trigger_carrier.take() else {
        return;
    };
    if live_spell_announcement(state) == Some(carrier.object_id) {
        if let Some(pending) = state
            .pending_trigger_carrier_owner()
            .and_then(|owner| state.pending_cast_at_mut(owner))
        {
            pending.activation_trigger_collection = Some(carrier.collection);
        }
        return;
    }
    if state.cancelled_casts.contains(&carrier.object_id) {
        return;
    }
    // A spell that became cast commits at CR 601.2i finalization and an
    // abandoned one at its controller's departure; neither leaves a carrier.
    // What remains is an announcement the game reversed as illegal (a cast
    // during resolution, CR 608.2g, whose proposed spell is rejected, CR
    // 601.2e): no abilities trigger as a result of it (CR 733.1).
    if !state.stack_paid_facts.contains_key(&carrier.object_id) {
        return;
    }
    debug_assert!(
        false,
        "spell trigger carrier for {:?} outlived its CR 601.2i finalization",
        carrier.object_id
    );
    let mut deferred = std::mem::take(&mut state.deferred_triggers);
    carrier.collection.commit_into(state, &mut deferred);
    state.deferred_triggers = deferred;
}

/// CR 601.2i: the spell became cast. Collect the finishing fragment (payment
/// events and `SpellCast`, read against the spell-cast ledger it has just
/// recorded) into the same carrier and commit everything into the deferred
/// queue as one batch, so the announcement's triggers and the cast triggers are
/// put on the stack together (CR 603.3b). The spell counterpart of
/// `casting_costs::push_ability_entry`.
pub(crate) fn commit_spell_announcement(
    state: &mut GameState,
    object_id: ObjectId,
    events: &[GameEvent],
) {
    let scope = std::mem::take(&mut state.announcement_event_scope);
    let Some(collection) = take_announcing(state, object_id) else {
        return;
    };
    let start = match scope {
        AnnouncementEventScope::From { start } => start.min(events.len()),
        AnnouncementEventScope::Unowned => events.len(),
    };
    commit_announcement_span(state, *collection, events, start);
}

/// CR 602.2b + CR 603.3b: the activation reached the stack. A carrier-less
/// activation (untargeted, unpaused) only ends the announcement scope: its cost
/// triggers are collected by the priority pipeline that follows. A carrier
/// collects the finishing fragment and commits it in one batch, exactly as the
/// spell commit does (`commit_announcement_span`).
///
/// No trigger is lost by reading `unclaimed_events`: every writer of
/// `consumed_before_priority_trigger_events` pairs its claim with a collection
/// (the carrier, the deferred queue, a mana frame, a cost owner's own pass), so
/// the filter removes only re-collections. The commit point is unchanged, so
/// nothing is reordered.
pub(crate) fn commit_activation_announcement(
    state: &mut GameState,
    collection: Option<Box<PendingActivationTriggerCollection>>,
    events: &[GameEvent],
) {
    let scope = std::mem::take(&mut state.announcement_event_scope);
    let Some(collection) = collection else {
        return;
    };
    let start = match scope {
        AnnouncementEventScope::From { start } => start.min(events.len()),
        AnnouncementEventScope::Unowned => {
            debug_assert!(
                false,
                "a carrier-bearing activation committed with no announcement scope"
            );
            0
        }
    };
    commit_announcement_span(state, *collection, events, start);
}

/// The commit body shared by spells and activations: collect `events[start..]`
/// not yet claimed (minus what a cost root retains) and the delayed scan into
/// `collection`, commit it into the deferred queue, and claim the span for
/// every collector.
fn commit_announcement_span(
    state: &mut GameState,
    mut collection: PendingActivationTriggerCollection,
    events: &[GameEvent],
    start: usize,
) {
    let retained = retained_by_cost_owners(state);
    let new_events: Vec<GameEvent> = unclaimed_events(state, events, start)
        .into_iter()
        .filter(|event| !retained.contains(event))
        .collect();
    // CR 603.7b: delayed triggered abilities whose trigger event is in this
    // announcement (the finishing `SpellCast` included) trigger with the rest
    // of it. Collected here, before the announcement's settlement can pause on
    // an ordering or target choice that would end the action before the
    // priority pipeline's delayed scan.
    let delayed_events: Vec<GameEvent> = triggers::filter_consumed_trigger_events_from(
        events,
        start,
        triggers::TriggerCollectionRequester::Delayed,
        &state.consumed_before_priority_trigger_events,
    )
    .into_iter()
    .filter(|event| !retained.contains(event))
    .collect();
    collection.collect(state, &new_events);
    let mut deferred = std::mem::take(&mut state.deferred_triggers);
    collection.commit_into(state, &mut deferred);
    state.deferred_triggers = deferred;
    // CR 400.7 + CR 601.2h: the priority pipeline clears the transient cast
    // payment stamps after a non-empty trigger collection; a committed
    // announcement is that collection for its span, so it clears them too.
    if !new_events.is_empty() {
        triggers::clear_post_collection_transients(state);
    }
    // The span is claimed below by its identities in the full buffer.
    triggers::collect_delayed_trigger_contexts_into_deferred(state, &delayed_events);
    // CR 603.2c: each occurrence is collected once, by this commit, for both
    // collectors; events a cost root still owns stay with that root.
    let owned: Vec<usize> = (start..events.len())
        .filter(|&index| !retained.contains(&events[index]))
        .collect();
    claim_indices(
        state,
        events,
        owned,
        ConsumedTriggerEventScope::AllCollectors,
    );
}

/// CR 800.4a: the announcement's controller left the game. The events it
/// produced still occurred (CR 603.2), so the carrier is committed rather than
/// dropped; contexts controlled by a departed player are retired by
/// `commit_into` (CR 800.4d).
pub(crate) fn publish_abandoned_announcement(state: &mut GameState, owner: PendingCastOwner) {
    // CR 603.2 + CR 800.4a: the departures a replacement-paused cost already
    // completed still occurred. The Cast root withholds them from announcement
    // staging (they wait for the group to finish), and abandoning the unpaid
    // current move retires that root, so they are drained and collected here,
    // once, before it goes.
    let raw_completed_prefix = take_completed_cast_prefix(state, owner);
    let completed_prefix = triggers::filter_already_collected_trigger_events_from(
        state,
        &raw_completed_prefix,
        0,
        &[],
    );
    let Some(pending) = state.pending_cast_at_mut(owner) else {
        return;
    };
    let object_id = pending.object_id;
    let is_spell = pending.activation_ability_index.is_none();
    if !completed_prefix.is_empty() && !is_spell {
        pending.begin_activation_trigger_collection();
    }
    let mut collection = pending.activation_trigger_collection.take();
    if is_spell {
        if let Some(carried) = take_announcing(state, object_id) {
            collection = Some(carried);
        }
        if collection.is_none() && !completed_prefix.is_empty() {
            collection = Some(Box::new(PendingActivationTriggerCollection::for_spell()));
        }
    }
    // CR 603.2 + CR 603.7 + CR 800.4d: completed events also belong to
    // surviving delayed observers, independently of the ordinary projection
    // or carrier. The old prefix has no occurrence ordinal in this action.
    triggers::collect_delayed_trigger_contexts_into_deferred(state, &raw_completed_prefix);
    let Some(mut collection) = collection else {
        return;
    };
    if !completed_prefix.is_empty() {
        collection.collect(state, &completed_prefix);
    }
    let mut deferred = std::mem::take(&mut state.deferred_triggers);
    collection.commit_into(state, &mut deferred);
    state.deferred_triggers = deferred;
}

/// Drains the completed-prefix ledger of the typed Cast cost root owned by
/// `owner`, if that root is the one being abandoned.
fn take_completed_cast_prefix(state: &mut GameState, owner: PendingCastOwner) -> Vec<GameEvent> {
    if owner != PendingCastOwner::CostMove {
        return Vec::new();
    }
    match state.pending_cost_move_resume.as_mut() {
        Some(PendingCostMoveResume::Cast {
            deferred_cost_events,
            ..
        }) => std::mem::take(deferred_cost_events),
        Some(
            PendingCostMoveResume::SacrificeForCost { .. }
            | PendingCostMoveResume::WardSacrificePayment { .. }
            | PendingCostMoveResume::ReplacementMayCost { .. }
            | PendingCostMoveResume::Foretell { .. }
            | PendingCostMoveResume::CollectEvidencePayment { .. }
            | PendingCostMoveResume::UnlessBouncePayment { .. }
            | PendingCostMoveResume::ManaAbilityPayment { .. }
            | PendingCostMoveResume::ActivationMillPayment { .. }
            | PendingCostMoveResume::CounterCostRemovalPayment { .. }
            | PendingCostMoveResume::LoyaltyActivation { .. }
            | PendingCostMoveResume::CounterAdditionUnlessPayment { .. }
            | PendingCostMoveResume::RandomDiscardUnlessPayment(_)
            | PendingCostMoveResume::TurnFaceUpCostMove { .. },
        )
        | None => Vec::new(),
    }
}
