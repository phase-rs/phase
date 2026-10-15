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
    AnnouncementEventScope, GameState, InFlightSpellTriggerCarrier, PendingCastOwner,
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

/// The unscanned events in `events[start..]`, after occurrence claims and the
/// queued zone-change witness (CR 603.2c: one trigger per occurrence).
fn unclaimed_events(state: &GameState, events: &[GameEvent], start: usize) -> Vec<GameEvent> {
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
        Some(PendingCostMoveResume::SacrificeForCost {
            deferred_cost_events,
            ..
        }) => retained.extend(deferred_cost_events.iter().cloned()),
        Some(PendingCostMoveResume::ManaAbilityPayment { cursor, .. }) => {
            retained.extend(cursor.deferred_cost_events.iter().cloned());
        }
        Some(
            PendingCostMoveResume::Cast { .. }
            | PendingCostMoveResume::WardSacrificePayment { .. }
            | PendingCostMoveResume::ReplacementMayCost { .. }
            | PendingCostMoveResume::Foretell { .. }
            | PendingCostMoveResume::CollectEvidencePayment { .. }
            | PendingCostMoveResume::UnlessBouncePayment { .. }
            | PendingCostMoveResume::ActivationMillPayment { .. }
            | PendingCostMoveResume::LoyaltyActivation { .. }
            | PendingCostMoveResume::CounterAdditionUnlessPayment { .. }
            | PendingCostMoveResume::RandomDiscardUnlessPayment(..),
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
    let AnnouncementEventScope::From { start: owned_from } = state.announcement_event_scope else {
        return;
    };
    let start = event_start.max(owned_from);
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
    state.announcement_event_scope = if state.live_pending_cast_owners().is_empty() {
        AnnouncementEventScope::Unowned
    } else {
        AnnouncementEventScope::From { start: 0 }
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
    let Some(mut collection) = take_announcing(state, object_id) else {
        return;
    };
    let start = match scope {
        AnnouncementEventScope::From { start } => start.min(events.len()),
        AnnouncementEventScope::Unowned => events.len(),
    };
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

/// CR 602.2b: the activation reached the stack; the announcement no longer
/// owns this action's later events.
pub(crate) fn end_activation_announcement(state: &mut GameState) {
    state.announcement_event_scope = AnnouncementEventScope::Unowned;
}

/// CR 800.4a: the announcement's controller left the game. The events it
/// produced still occurred (CR 603.2), so the carrier is committed rather than
/// dropped; contexts controlled by a departed player are retired by
/// `commit_into` (CR 800.4d).
pub(crate) fn publish_abandoned_announcement(state: &mut GameState, owner: PendingCastOwner) {
    let Some(pending) = state.pending_cast_at_mut(owner) else {
        return;
    };
    let object_id = pending.object_id;
    let is_spell = pending.activation_ability_index.is_none();
    let mut collection = pending.activation_trigger_collection.take();
    if is_spell {
        if let Some(carried) = take_announcing(state, object_id) {
            collection = Some(carried);
        }
    }
    let Some(collection) = collection else {
        return;
    };
    let mut deferred = std::mem::take(&mut state.deferred_triggers);
    collection.commit_into(state, &mut deferred);
    state.deferred_triggers = deferred;
}
