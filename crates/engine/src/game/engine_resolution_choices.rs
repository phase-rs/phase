// engine-citation-gate: symbol anchors only
use std::collections::{HashMap, HashSet};

use rand::seq::SliceRandom;

use crate::types::ability::{
    AbilityCost, ChoiceType, ChosenAttribute, DigRestOrder, Effect, EffectKind, GuessOutcome,
    LibraryPosition, QuantityExpr, QuantityRef, ResolvedAbility, TargetRef,
};
use crate::types::actions::{GameAction, LearnOption, OutsideGameSelection};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    ActionResult, CastOfferKind, ChosenDamageSource, CopyChosenSelection, GameState,
    OutsideGameChoiceSource, PayableResource, PendingContinuation,
    PendingPlayerScopeSacrificeCompletion, PersistentAxisMaterialization, WaitingFor,
};
use crate::types::identifiers::{ObjectId, TrackedSetId};
use crate::types::resolved_commands::{
    ResolvedInformationAudience, ResolvedInformationEdit, ResolvedInformationLifetime,
};
use crate::types::zones::Zone;

use super::effects;
use super::engine::EngineError;
use super::turns;
use super::zones;
use super::{casting, casting_costs, engine_priority, mana_abilities, public_state};

/// CR 701.23a + CR 614.1: offer every found card as its own replaceable event.
/// Original survivors remain in the printed search continuation; modified cards
/// are delivered independently and therefore cannot be consumed by that
/// continuation's destination or found-card riders.
pub(crate) fn apply_search_found_replacements(
    state: &mut GameState,
    searcher: crate::types::player::PlayerId,
    library_owner: Option<crate::types::player::PlayerId>,
    chosen: &[ObjectId],
    continuation: crate::types::game_state::PendingSearchFoundContinuation,
    reveal: bool,
    events: &mut Vec<GameEvent>,
) -> Result<Vec<ObjectId>, Box<WaitingFor>> {
    // A SearchFound ordering pause must not expose the pre-replacement choice
    // through stale reveal memory. The terminal survivor set repopulates this
    // only after every original-disposition event has finished.
    state.last_revealed_ids.clear();
    let batch = crate::types::game_state::PendingSearchFoundBatch {
        searcher,
        library_owner,
        remaining: chosen
            .iter()
            .filter_map(|id| state.objects.get(id))
            .map(crate::types::identifiers::ObjectIncarnationRef::from_object)
            .collect(),
        survivors: Vec::with_capacity(chosen.len()),
        current: None,
        continuation,
        visibility: reveal.into(),
    };
    let batch = process_search_found_batch(state, batch, events)?;
    if matches!(
        batch.continuation,
        crate::types::game_state::PendingSearchFoundContinuation::Standard { .. }
    ) {
        reveal_search_found_survivors(state, &batch, events);
    }
    Ok(live_search_found_ids(state, &batch.survivors))
}

fn live_search_found_ids(
    state: &GameState,
    identities: &[crate::types::identifiers::ObjectIncarnationRef],
) -> Vec<ObjectId> {
    identities
        .iter()
        .filter(|identity| {
            state
                .objects
                .get(&identity.object_id)
                .is_some_and(|object| object.incarnation == identity.incarnation)
        })
        .map(|identity| identity.object_id)
        .collect()
}

/// CR 614.6 + CR 701.23a: reveal only cards whose original found event still
/// occurs. A replacement-modified card is delivered independently and never
/// becomes part of the printed search instruction's reveal event or public
/// reveal memory.
fn reveal_search_found_survivors(
    state: &mut GameState,
    batch: &crate::types::game_state::PendingSearchFoundBatch,
    events: &mut Vec<GameEvent>,
) {
    if !batch.visibility.is_public() {
        state.last_revealed_ids.clear();
        return;
    }

    let card_ids = live_search_found_ids(state, &batch.survivors);
    state.last_revealed_ids = card_ids.clone();
    for &card_id in &card_ids {
        state.revealed_cards.insert(card_id);
    }
    if !card_ids.is_empty() {
        let card_names = card_ids
            .iter()
            .filter_map(|id| state.objects.get(id).map(|object| object.name.clone()))
            .collect();
        events.push(GameEvent::CardsRevealed {
            player: batch.searcher,
            card_ids,
            card_names,
        });
    }
}

/// CR 616.1 + CR 701.23a: Process the exact unhandled suffix of a found-card
/// batch. Both the SearchFound replacement choice and the modified card's
/// resulting zone move can pause independently; in either case the serialized
/// batch owns every card that has not completed this stage.
fn process_search_found_batch(
    state: &mut GameState,
    mut batch: crate::types::game_state::PendingSearchFoundBatch,
    events: &mut Vec<GameEvent>,
) -> Result<crate::types::game_state::PendingSearchFoundBatch, Box<WaitingFor>> {
    let remaining = std::mem::take(&mut batch.remaining);
    for (index, identity) in remaining.iter().copied().enumerate() {
        if !state
            .objects
            .get(&identity.object_id)
            .is_some_and(|object| object.incarnation == identity.incarnation)
        {
            continue;
        }
        let proposed = crate::types::proposed_event::ProposedEvent::SearchFound {
            searcher: batch.searcher,
            library_owner: batch.library_owner,
            object_id: identity.object_id,
            disposition: crate::types::proposed_event::SearchFoundDisposition::Original,
            applied: Default::default(),
        };
        match super::replacement::replace_event(state, proposed, events) {
            super::replacement::ReplacementResult::Execute(event) => {
                if matches!(
                    batch.continuation,
                    crate::types::game_state::PendingSearchFoundContinuation::Scoped
                ) {
                    freeze_scoped_search_found_event(state, identity, &event, &mut batch.survivors);
                } else if deliver_search_found_event(
                    state,
                    identity,
                    event,
                    &mut batch.survivors,
                    events,
                ) {
                    batch.remaining = remaining[index + 1..].to_vec();
                    state.pending_search_found_batch = Some(batch);
                    return Err(Box::new(state.waiting_for.clone()));
                }
            }
            super::replacement::ReplacementResult::NeedsChoice(player) => {
                batch.remaining = remaining[index + 1..].to_vec();
                batch.current = Some(identity);
                state.pending_search_found_batch = Some(batch);
                let waiting = super::replacement::replacement_choice_waiting_for(player, state);
                state.waiting_for = waiting.clone();
                return Err(Box::new(waiting));
            }
            super::replacement::ReplacementResult::Prevented => {}
        }
    }
    Ok(batch)
}

/// CR 101.4 + CR 701.23i: freeze one terminal found-card disposition without
/// moving the object. The shared scoped delivery materializes every frozen
/// request only after all APNAP choices have completed.
fn freeze_scoped_search_found_event(
    state: &mut GameState,
    identity: crate::types::identifiers::ObjectIncarnationRef,
    event: &crate::types::proposed_event::ProposedEvent,
    survivors: &mut Vec<crate::types::identifiers::ObjectIncarnationRef>,
) {
    let crate::types::proposed_event::ProposedEvent::SearchFound {
        searcher,
        object_id,
        disposition,
        ..
    } = event
    else {
        return;
    };
    if identity.object_id != *object_id
        || !state
            .objects
            .get(object_id)
            .is_some_and(|object| object.incarnation == identity.incarnation)
    {
        return;
    }
    let frozen = crate::types::game_state::FrozenScopedSearchFoundDisposition {
        searcher: *searcher,
        identity,
        disposition: disposition.clone(),
    };
    let Some(pending) = state.pending_scoped_library_search.as_mut() else {
        if matches!(
            disposition,
            crate::types::proposed_event::SearchFoundDisposition::Original
        ) {
            survivors.push(identity);
        }
        return;
    };
    let crate::types::game_state::ScopedLibrarySearchPhase::CollectSelections {
        frozen_dispositions,
        ..
    } = &mut pending.phase
    else {
        return;
    };
    frozen_dispositions.push(frozen);
    if matches!(
        disposition,
        crate::types::proposed_event::SearchFoundDisposition::Original
    ) {
        survivors.push(identity);
    }
}

/// Deliver one terminal SearchFound disposition. Returns `true` when the
/// resulting zone move or snapshotted suffix parked an inner choice.
fn deliver_search_found_event(
    state: &mut GameState,
    identity: crate::types::identifiers::ObjectIncarnationRef,
    event: crate::types::proposed_event::ProposedEvent,
    survivors: &mut Vec<crate::types::identifiers::ObjectIncarnationRef>,
    events: &mut Vec<GameEvent>,
) -> bool {
    let crate::types::proposed_event::ProposedEvent::SearchFound {
        object_id,
        disposition,
        ..
    } = event
    else {
        return false;
    };
    let crate::types::proposed_event::SearchFoundDisposition::Modified(disposition) = disposition
    else {
        if identity.object_id == object_id
            && state
                .objects
                .get(&object_id)
                .is_some_and(|object| object.incarnation == identity.incarnation)
        {
            survivors.push(identity);
        }
        return false;
    };
    let move_result = super::zone_pipeline::move_object(
        state,
        super::zone_pipeline::ZoneMoveRequest::effect(
            object_id,
            disposition.destination,
            disposition.source.object_id,
        ),
        events,
    );
    match move_result {
        super::zone_pipeline::ZoneMoveResult::Done => {
            grant_search_found_permission_after_delivery(
                state,
                object_id,
                disposition.grant,
                events,
            );
            false
        }
        super::zone_pipeline::ZoneMoveResult::NeedsChoice(_)
        | super::zone_pipeline::ZoneMoveResult::NeedsAuraAttachmentChoice => {
            super::zone_pipeline::defer_completion_on_pause(
                state,
                crate::types::game_state::BatchCompletion::SearchFoundZoneDelivery {
                    object_id,
                    grant: disposition.grant,
                },
            );
            true
        }
    }
}

/// CR 611.2b + CR 601.3: A one-shot effect may create a permission that lasts
/// after resolution. Install the bound rider only when the replacement-selected
/// move actually delivered this card to exile; a later zone-change replacement
/// may have redirected that move elsewhere.
pub(crate) fn grant_search_found_permission_after_delivery(
    state: &mut GameState,
    object_id: ObjectId,
    grant: Option<crate::types::proposed_event::BoundSearchFoundGrant>,
    events: &mut Vec<GameEvent>,
) {
    let Some(grant) = grant else {
        return;
    };
    if !state
        .objects
        .get(&object_id)
        .is_some_and(|object| object.zone == Zone::Exile)
    {
        return;
    }

    let mut ability = ResolvedAbility::new(
        Effect::GrantCastingPermission {
            permission: crate::types::ability::CastingPermission::PlayFromExile {
                duration: crate::types::ability::Duration::Permanent,
                granted_to: grant.grantee,
                frequency: crate::types::statics::CastFrequency::Unlimited,
                source_id: None,
                exiled_by_ability_controller: None,
                mana_spend_permission: grant.mana_spend_permission,
                card_filter: None,
                single_use_group: None,
                single_use: false,
                cast_cost_raise: None,
                land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                invalidation: None,
            },
            target: crate::types::ability::TargetFilter::ParentTarget,
            grantee: crate::types::ability::PermissionGrantee::ParentTargetController,
        },
        vec![
            TargetRef::Object(object_id),
            TargetRef::Player(grant.grantee),
        ],
        grant.source.object_id,
        grant.controller,
    );
    if let Some(source) = state
        .objects
        .get(&grant.source.object_id)
        .filter(|source| source.incarnation == grant.source.incarnation)
    {
        ability.set_trigger_source_recursive(super::triggers::trigger_source_context_for_latch(
            state, source,
        ));
    }
    // The canonical grant resolver is the single authority for stamping
    // source/controller/grantee provenance and permission replacement.
    effects::grant_permission::resolve(state, &ability, events)
        .expect("validated SearchFound permission grant must resolve");
}

/// CR 616.1 + CR 701.23a: Resume a parked per-card found-event batch from the
/// exact serialized suffix. The accepted event is already fully bound by the
/// replacement pipeline; it is delivered without a new candidate scan.
pub(crate) fn resume_search_found_after_replacement(
    state: &mut GameState,
    event: crate::types::proposed_event::ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let Some(mut batch) = state.pending_search_found_batch.take() else {
        return Err(EngineError::InvalidAction(
            "missing SearchFound batch resume".to_string(),
        ));
    };
    let current = batch.current.take();
    let event_object = match &event {
        crate::types::proposed_event::ProposedEvent::SearchFound { object_id, .. } => {
            Some(*object_id)
        }
        _ => None,
    };
    if let Some(identity) = current.filter(|identity| {
        event_object == Some(identity.object_id)
            && state
                .objects
                .get(&identity.object_id)
                .is_some_and(|object| object.incarnation == identity.incarnation)
    }) {
        if matches!(
            batch.continuation,
            crate::types::game_state::PendingSearchFoundContinuation::Scoped
        ) {
            freeze_scoped_search_found_event(state, identity, &event, &mut batch.survivors);
        } else if deliver_search_found_event(state, identity, event, &mut batch.survivors, events) {
            state.pending_search_found_batch = Some(batch);
            return Ok(state.waiting_for.clone());
        }
    }

    let Ok(batch) = process_search_found_batch(state, batch, events) else {
        return Ok(state.waiting_for.clone());
    };
    finish_search_found_batch(state, batch, events)
}

/// CR 616.1 + CR 701.23a: Complete a modified found card's inner zone move,
/// then continue the exact saved found-card suffix. Called from the generic
/// zone-batch completion drain after the replacement-selected move delivers.
fn resume_search_found_after_zone_delivery(
    state: &mut GameState,
    object_id: ObjectId,
    grant: Option<crate::types::proposed_event::BoundSearchFoundGrant>,
    events: &mut Vec<GameEvent>,
) {
    grant_search_found_permission_after_delivery(state, object_id, grant, events);
    let Some(batch) = state.pending_search_found_batch.take() else {
        return;
    };
    if let Ok(batch) = process_search_found_batch(state, batch, events) {
        finish_search_found_batch(state, batch, events)
            .expect("SearchFound batch completion must resolve");
    }
}

fn finish_search_found_batch(
    state: &mut GameState,
    batch: crate::types::game_state::PendingSearchFoundBatch,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if matches!(
        &batch.continuation,
        crate::types::game_state::PendingSearchFoundContinuation::Scoped
    ) && state.pending_scoped_library_search.is_none()
    {
        state.pending_search_found_batch = Some(batch);
        return Err(EngineError::InvalidAction(
            "scoped SearchFound resume: missing scoped search resume".to_string(),
        ));
    }
    let player = batch.searcher;
    state.active_search_decision_controls.remove(&player);
    match &batch.continuation {
        crate::types::game_state::PendingSearchFoundContinuation::Scoped => {
            let retry_batch = batch.clone();
            if let Err(error) = effects::scoped_library_search::complete_replaced_selection(
                state,
                batch.searcher,
                batch.survivors,
                events,
            ) {
                state.pending_search_found_batch = Some(retry_batch);
                return Err(EngineError::InvalidAction(format!(
                    "scoped SearchFound resume: {error}"
                )));
            }
            return Ok(state.waiting_for.clone());
        }
        crate::types::game_state::PendingSearchFoundContinuation::Standard { split: None } => {
            reveal_search_found_survivors(state, &batch, events);
        }
        crate::types::game_state::PendingSearchFoundContinuation::Standard {
            split: Some(split),
        } => {
            reveal_search_found_survivors(state, &batch, events);
            let split = split.clone();
            let survivors = live_search_found_ids(state, &batch.survivors);
            let source_id = state
                .active_ability_continuation()
                .map(|continuation| continuation.chain.source_id)
                .or_else(|| survivors.first().copied())
                .unwrap_or(ObjectId(0));
            if survivors.len() > split.primary_count as usize {
                set_priority(state, player);
                state.waiting_for = WaitingFor::SearchPartitionChoice {
                    player,
                    cards: survivors,
                    primary_destination: split.primary_destination,
                    primary_count: split.primary_count,
                    primary_enter_tapped: split.primary_enter_tapped,
                    rest_destination: split.rest_destination,
                    source_id,
                };
                return Ok(state.waiting_for.clone());
            }
            match apply_search_partition(state, &survivors, &[], &split, source_id, player, events)?
            {
                crate::game::zone_pipeline::BatchMoveResult::Done => {}
                crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => {
                    return Ok(state.waiting_for.clone());
                }
            }
            set_priority(state, player);
            // CR 605.3b + CR 616.1: resume-aware — a parked mana-cost cursor
            // settles before (and instead of stranding) the ordinary rider.
            super::engine::resume_pending_continuation_if_priority(state, events)?;
            return Ok(state.waiting_for.clone());
        }
    }

    Ok(
        match finalize_standard_search_selection(
            state,
            player,
            &live_search_found_ids(state, &batch.survivors),
            events,
        ) {
            ResolutionChoiceOutcome::WaitingFor(waiting)
            | ResolutionChoiceOutcome::WaitingForWithInlineTriggers(waiting) => waiting,
            ResolutionChoiceOutcome::ActionResult(result) => result.waiting_for,
        },
    )
}

pub(super) enum ResolutionChoiceOutcome {
    WaitingFor(WaitingFor),
    WaitingForWithInlineTriggers(WaitingFor),
    ActionResult(ActionResult),
}

/// CR 603.2 + CR 603.3b + CR 608.2g: A spell cast while an effect is
/// resolving can finish its announcement at another resolution prompt (for
/// example, Ripple's next revealed-card offer) rather than at Priority. Its
/// `SpellCast` event nevertheless happened and must be collected exactly once;
/// it merely cannot be put onto the stack until the parent resolution finishes.
///
/// This is the single paused cast-during-resolution settlement seam. It covers
/// free casts, casts that pause for targets or payment, and continuation offers
/// uniformly because the final reducer calls it after every successful action.
pub(super) fn park_cast_during_resolution_cast_observers(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    event_start: usize,
    waiting_for: &WaitingFor,
) -> Result<Option<WaitingFor>, EngineError> {
    let cast_announced = events[event_start..]
        .iter()
        .any(|event| matches!(event, GameEvent::SpellCast { .. }));
    let paused_resolution_cast =
        handles(waiting_for) || matches!(waiting_for, WaitingFor::CopyRetarget { .. });
    if state.resolving_stack_entry.is_none() || !paused_resolution_cast || !cast_announced {
        return Ok(None);
    }

    // `run_post_action_pipeline_from` recognizes the non-Priority resolution
    // choice and parks the suffix's observers in `deferred_triggers`; it does
    // not drain them while the parent continuation is still active.
    state.waiting_for = waiting_for.clone();
    let settled = super::engine_priority::run_post_action_pipeline_from(
        state,
        events,
        event_start,
        waiting_for,
        false,
        true,
    )?;
    Ok(Some(settled))
}

/// CR 603.2 + CR 603.3b: After a resolution-choice handler has moved objects
/// (sacrifice, change-zone, bounce, discard) and resolved any reflexive
/// continuation, dispatch the observer triggers (dies-, discarded-, etc.)
/// produced by that move across a possible continuation pause.
///
/// `event_slice_start..event_slice_end` MUST bound the move's OWN events,
/// captured BEFORE the continuation drain so that continuation-produced events
/// are excluded.
///
/// Returns `Some(WaitingFor)` only in the B1 settled case when a drained
/// deferred trigger itself needs player input; the caller must propagate it.
fn batch_or_drain_observer_triggers(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    event_slice_start: usize,
    event_slice_end: usize,
    // CR 603.2c: `true` declares that `events[event_slice_start..event_slice_end]`
    // is exactly one completed logical zone-change owner's completion slice.
    // `LogicalZoneChangeGroup::append_delivery_events` retains EVERY `ZoneChanged`
    // in the slice it is handed, so within such a slice a blanket drop is
    // equivalent to per-occurrence suppression. A collector whose slice is not
    // owner-bounded must use
    // `triggers::filter_already_collected_trigger_events_from` instead.
    zone_changes_are_logically_owned: bool,
) -> Option<ResolutionChoiceOutcome> {
    if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        // B1: this action settled. Merge this slice's observer triggers into
        // the parked queue before draining — otherwise the last segment's
        // triggers (e.g. the final Syphon Mind opponent discard) never enter
        // `deferred_triggers` and are lost when ordering runs (issue #1793).
        let trigger_events: Vec<GameEvent> = events[event_slice_start..event_slice_end]
            .iter()
            .filter(|ev| {
                !matches!(ev, GameEvent::PhaseChanged { .. })
                    && (!zone_changes_are_logically_owned
                        || !matches!(ev, GameEvent::ZoneChanged { .. }))
            })
            .cloned()
            .chain(
                // CR 603.2 + CR 608.2c: a typed completion continuation may
                // publish the player action that the interactive move just
                // completed. Include that semantic event without widening the
                // owner-bounded zone slice to continuation-produced zone moves.
                events[event_slice_end..]
                    .iter()
                    .filter(|event| matches!(event, GameEvent::PlayerPerformedAction { .. }))
                    .cloned(),
            )
            .collect();
        super::triggers::collect_triggers_into_deferred(state, &trigger_events);
        if let Some(wf) = super::triggers::drain_deferred_trigger_queue(state, events) {
            return Some(ResolutionChoiceOutcome::WaitingFor(wf));
        }
        Some(ResolutionChoiceOutcome::WaitingForWithInlineTriggers(
            state.waiting_for.clone(),
        ))
    } else {
        // B2: paused — `run_post_action_pipeline` will not scan this action.
        // Park this move's observer triggers for a later settle.
        let trigger_events: Vec<GameEvent> = events[event_slice_start..event_slice_end]
            .iter()
            .filter(|ev| {
                !matches!(ev, GameEvent::PhaseChanged { .. })
                    && (!zone_changes_are_logically_owned
                        || !matches!(ev, GameEvent::ZoneChanged { .. }))
            })
            .cloned()
            .chain(
                // CR 603.2 + CR 608.2c: see the settled branch above. Park a
                // completion action across a further continuation prompt, but
                // do not fold that continuation's zone changes into this owner.
                events[event_slice_end..]
                    .iter()
                    .filter(|event| matches!(event, GameEvent::PlayerPerformedAction { .. }))
                    .cloned(),
            )
            .collect();
        super::triggers::collect_triggers_into_deferred(state, &trigger_events);
        None
    }
}

/// CR 603.2 + CR 603.3b: Preserve triggers from events that occurred while a
/// resolution choice paused; they trigger now and wait for the next priority
/// window's APNAP placement rather than being lost with the action's event slice.
pub(crate) fn defer_observer_triggers_for_paused_choice(
    state: &mut GameState,
    events: &[GameEvent],
    event_start: usize,
) {
    let trigger_events: Vec<GameEvent> = events[event_start..]
        .iter()
        .filter(|event| !matches!(event, GameEvent::PhaseChanged { .. }))
        .cloned()
        .collect();
    if !trigger_events.is_empty() {
        super::triggers::collect_triggers_into_deferred(state, &trigger_events);
    }
}

/// CR 603.2 + CR 603.3b + CR 701.23: after a search tutor's put/shuffle
/// continuation drains, collect ETB/dies/discards observers before this
/// `SelectCards` action reaches its priority checkpoint. The ordinary
/// post-action drain then puts them on the stack before priority is returned.
///
/// CR 603.2c: this slice spans the whole continuation drain, so it holds both
/// the delivery's logical zone-change owner's occurrences (already collected by
/// `change_zone::resolve` / `zone_pipeline::move_objects_simultaneously_then`)
/// AND zone changes no owner allocated a group for. It is therefore NOT
/// owner-bounded and cannot blanket-drop `ZoneChanged` the way
/// `batch_or_drain_observer_triggers` does; it consults the shared ownership
/// authority instead. That authority's ledger half applies to every event kind,
/// matching the generic priority scan. Without it a fetched land's landfall/ETB
/// observers fire twice.
fn collect_search_observer_triggers(
    state: &mut GameState,
    events: &[GameEvent],
    events_before_drain: usize,
) -> ResolutionChoiceOutcome {
    let uncollected_events = super::triggers::filter_already_collected_trigger_events_from(
        state,
        events,
        events_before_drain,
        &state.consumed_before_priority_trigger_events,
    );
    let trigger_events: Vec<GameEvent> = uncollected_events
        .into_iter()
        .filter(|ev| !matches!(ev, GameEvent::PhaseChanged { .. }))
        .collect();
    if !trigger_events.is_empty() {
        super::triggers::collect_triggers_into_deferred(state, &trigger_events);
    }
    // A search continuation can park another typed resolution frame. Let the
    // shared carrier authority prove that every such frame has drained before
    // retiring the parent and releasing its CR 400.7j self-move link.
    super::engine::settle_resolving_stack_entry_after_continuation_resume(state);
    ResolutionChoiceOutcome::WaitingForWithInlineTriggers(state.waiting_for.clone())
}

pub(super) fn handles(waiting_for: &WaitingFor) -> bool {
    matches!(
        waiting_for,
        WaitingFor::MeldPairChoice { .. }
            | WaitingFor::MeldAttackTargetChoice { .. }
            | WaitingFor::EntryAttackTargetChoice { .. }
            | WaitingFor::ScryChoice { .. }
            | WaitingFor::ArrangePlanarDeckTopChoice { .. }
            | WaitingFor::RedistributeLifeTotals { .. }
            | WaitingFor::CoinFlipKeepChoice { .. }
            | WaitingFor::ManifestDreadChoice { .. }
            | WaitingFor::CastOffer {
                kind: CastOfferKind::Discover { .. },
                ..
            }
            | WaitingFor::CastOffer {
                kind: CastOfferKind::GraveyardPaidCast { .. },
                ..
            }
            | WaitingFor::RevealUntilKeptChoice { .. }
            | WaitingFor::RepeatDecision { .. }
            | WaitingFor::CastOffer {
                kind: CastOfferKind::Cascade { .. },
                ..
            }
            | WaitingFor::CastOffer {
                kind: CastOfferKind::Ripple { .. },
                ..
            }
            | WaitingFor::CastOffer {
                kind: CastOfferKind::FreeCastWindow { .. },
                ..
            }
            | WaitingFor::LearnChoice { .. }
            | WaitingFor::TopOrBottomChoice { .. }
            | WaitingFor::PopulateChoice { .. }
            | WaitingFor::ClashChooseOpponent { .. }
            | WaitingFor::ChooseFromZoneOpponentChooser { .. }
            | WaitingFor::ClashCardPlacement { .. }
            | WaitingFor::VoteChoice { .. }
            | WaitingFor::SeparatePilesChooseOpponent { .. }
            | WaitingFor::SeparatePilesPartition { .. }
            | WaitingFor::SeparatePilesChoice { .. }
            | WaitingFor::DigChoice { .. }
            | WaitingFor::SurveilChoice { .. }
            | WaitingFor::RevealChoice { .. }
            | WaitingFor::SearchChoice { .. }
            | WaitingFor::SearchPartitionChoice { .. }
            | WaitingFor::OutsideGameChoice { .. }
            | WaitingFor::ChooseFromZoneChoice { .. }
            | WaitingFor::BeholdChoice { .. }
            | WaitingFor::ChooseOneOfBranch { .. }
            | WaitingFor::DiscardToHandSize { .. }
            | WaitingFor::ConniveDiscard { .. }
            | WaitingFor::DiscardChoice { .. }
            | WaitingFor::EffectZoneChoice { .. }
            | WaitingFor::DrawnThisTurnTopdeckChoice { .. }
            | WaitingFor::NamedChoice { .. }
            | WaitingFor::OpponentGuess { .. }
            | WaitingFor::SpellbookDraft { .. }
            | WaitingFor::DamageSourceChoice { .. }
            | WaitingFor::ChooseRingBearer { .. }
            | WaitingFor::ChooseRoomDoor { .. }
            | WaitingFor::ChooseDungeon { .. }
            | WaitingFor::ChooseDungeonRoom { .. }
            | WaitingFor::SpecializeColor { .. }
            | WaitingFor::ChooseLegend { .. }
            | WaitingFor::MutateMergeChoice { .. }
            | WaitingFor::CipherEncodeChoice { .. }
            | WaitingFor::CommanderZoneChoice { .. }
            | WaitingFor::BattleProtectorChoice { .. }
            | WaitingFor::CategoryChoice { .. }
            | WaitingFor::EachPlayerCopyChosenSelection { .. }
            | WaitingFor::KeepWithinTotalPowerChoice { .. }
            | WaitingFor::KeepExactPermanentsChoice { .. }
            | WaitingFor::PayAmountChoice { .. }
    )
}

/// CR 608.2c: Expressive Iteration-style dig tails chain a
/// `PutAtLibraryPosition { TrackedSet }` step before exiling from the same
/// looked-at pile. Those continuations publish and route via the **unkept**
/// looked-at cards; generic reveal/keep continuations (Zimone land split)
/// bind only the kept/revealed subset.
fn dig_continuation_needs_full_looked_at_tracked_set(ability: &ResolvedAbility) -> bool {
    let mut current = Some(ability);
    while let Some(sub) = current {
        if matches!(
            &sub.effect,
            Effect::PutAtLibraryPosition {
                target: crate::types::ability::TargetFilter::TrackedSet { .. },
                ..
            }
        ) {
            return true;
        }
        current = sub.sub_ability.as_deref();
    }
    false
}

/// CR 701.20e / CR 701.23a + CR 401.4: Move the "rest" partition of an
/// interactive selection (Dig's unkept cards, a search-split's non-primary
/// cards) to a concrete destination zone. `Library` routes to the bottom of the
/// owner's library (CR 401.4); every other zone uses the standard cross-zone
/// mover. Extracted from the Dig rest-move block so the search-partition handler
/// reuses the exact same routing.
pub(crate) fn route_rest_partition(
    state: &mut GameState,
    rest_ids: &[ObjectId],
    rest_zone: Zone,
    rest_order: DigRestOrder,
    source_id: Option<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> crate::game::zone_pipeline::BatchMoveResult {
    let mut ordered_ids = rest_ids.to_vec();
    if rest_zone == Zone::Library && rest_order == DigRestOrder::Random {
        // CR 400.5 + CR 608.2c: Exact Oracle text requires a randomized
        // remainder; only this rest pile, not the remainder of the library,
        // consumes entropy.
        ordered_ids.shuffle(&mut state.rng);
    }
    route_rest_partition_then(state, &ordered_ids, rest_zone, source_id, None, events)
}

fn route_rest_partition_then(
    state: &mut GameState,
    rest_ids: &[ObjectId],
    rest_zone: Zone,
    source_id: Option<ObjectId>,
    completion: Option<crate::types::game_state::BatchCompletion>,
    events: &mut Vec<GameEvent>,
) -> crate::game::zone_pipeline::BatchMoveResult {
    let requests = rest_ids
        .iter()
        .map(|&obj_id| {
            let request = crate::game::zone_pipeline::ZoneMoveRequest::effect(
                obj_id,
                rest_zone,
                source_id.unwrap_or(obj_id),
            );
            if rest_zone == Zone::Library {
                request.at_library_position(LibraryPosition::Bottom)
            } else {
                request
            }
        })
        .collect();
    crate::game::zone_pipeline::move_objects_simultaneously_then(
        state, requests, completion, events,
    )
}

fn validate_exact_keep_on_top_selection(
    selection: &[ObjectId],
    looked_at: &[ObjectId],
    keep_on_top: usize,
) -> Result<(), EngineError> {
    validate_keep_on_top_selection(selection, looked_at)?;
    if selection.len() != keep_on_top {
        return Err(EngineError::InvalidAction(format!(
            "keep-on-top selection must contain exactly {keep_on_top} cards"
        )));
    }
    Ok(())
}

/// CR 701.22a / CR 701.25a: Scry and surveil put the kept cards on top of the
/// library "in any order", so a legal keep-on-top selection is any duplicate-free
/// subset of the looked-at cards (order is the player's free choice). `apply()` is
/// the validation boundary: a foreign id or a duplicate would corrupt the
/// library `retain`+`insert` (relocating or duplicating a card), so reject both
/// here. Mirrors the order-agnostic subset semantics of `selection_mismatch`.
fn validate_keep_on_top_selection(
    selection: &[ObjectId],
    looked_at: &[ObjectId],
) -> Result<(), EngineError> {
    let mut seen = std::collections::HashSet::new();
    for id in selection {
        if !looked_at.contains(id) {
            return Err(EngineError::InvalidAction(
                "keep-on-top selection contains a card that was not looked at".to_string(),
            ));
        }
        if !seen.insert(*id) {
            return Err(EngineError::InvalidAction(
                "keep-on-top selection contains a duplicate card".to_string(),
            ));
        }
    }
    Ok(())
}

/// CR 401.2 + CR 608.2c: Validate a `DigChoice` keep-selection. A dig
/// ("look at the top N, put [some] into your hand/elsewhere") may only act on
/// the cards it actually looked at, and only on those matching the effect's
/// filter. Mirrors `validate_keep_on_top_selection` (used by scry/surveil) but
/// additionally enforces the filter, since `DigChoice` is one of the freeform
/// card-selection states the multiplayer server forwards unvalidated — so
/// `apply` is the sole legality boundary.
///
/// `looked_at` is the full revealed set; `selectable` is the subset matching the
/// effect's filter (equal to `looked_at` when the effect has no filter, and
/// empty when a filter matched nothing — in which case the only legal selection
/// is empty). Previously the filter check was skipped whenever `selectable` was
/// empty, which let a filtered dig that matched zero cards accept arbitrary
/// object ids — moving cards the effect never looked at into the chooser's hand,
/// or inserting foreign ids into the library and corrupting its order.
fn validate_dig_selection(
    kept: &[ObjectId],
    looked_at: &[ObjectId],
    selectable: &[ObjectId],
) -> Result<(), EngineError> {
    let mut seen = std::collections::HashSet::new();
    for id in kept {
        if !seen.insert(*id) {
            return Err(EngineError::InvalidAction(
                "dig selection contains a duplicate card".to_string(),
            ));
        }
        if !looked_at.contains(id) {
            return Err(EngineError::InvalidAction(
                "dig selection contains a card that was not looked at".to_string(),
            ));
        }
        if !selectable.contains(id) {
            return Err(EngineError::InvalidAction(
                "dig selection contains a card that does not match the effect's filter".to_string(),
            ));
        }
    }
    Ok(())
}

/// CR 701.23a + CR 614.1 / CR 110.5b: Apply a cultivate-class search-destination
/// split. `primary_ids` are routed to `primary_destination` through the full
/// CR 400.7 + CR 608.2c: True when a search continuation's chain relocates the
/// found set to exile (a `ChangeZone { destination: Exile }` anywhere in the
/// chain). Distinguishes name-hate exile searches (whose hand-origin members
/// feed the `ExiledFromHandThisResolution` draw rider) from tutors that put the
/// found card into a hand or onto the battlefield.
fn continuation_exiles_found_set(chain: &ResolvedAbility) -> bool {
    let mut cursor = Some(chain);
    while let Some(def) = cursor {
        if matches!(
            &def.effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                ..
            }
        ) {
            return true;
        }
        cursor = def.sub_ability.as_deref();
    }
    false
}

/// Finalize the ordinary (non-partitioned) SearchChoice continuation. This is
/// the single authority for both the synchronous selection path and a
/// SearchFound batch resumed after one or more nested replacement pauses.
fn finalize_standard_search_selection(
    state: &mut GameState,
    player: crate::types::player::PlayerId,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> ResolutionChoiceOutcome {
    state.active_search_decision_controls.remove(&player);
    set_priority(state, player);
    let events_before_drain = events.len();
    // CR 608.2c: Count cards still in hand immediately before a
    // found-set exile continuation. SearchFound replacements removed from the
    // survivor set are intentionally excluded.
    let continuation_exiles_set = state
        .active_ability_continuation()
        .or_else(|| {
            state
                .outer_ability_continuation_of_active_post_replacement_draw()
                .map(|continuation| &continuation.pending)
        })
        .is_some_and(|cont| continuation_exiles_found_set(&cont.chain));
    if continuation_exiles_set {
        let hand_exiles = chosen
            .iter()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|obj| obj.zone == Zone::Hand)
            })
            .count() as u32;
        state.exiled_from_hand_this_resolution = state
            .exiled_from_hand_this_resolution
            .saturating_add(hand_exiles);
    }
    // CR 608.2c + CR 701.23a: A search choice produces the selected set for
    // any continuation that consumes "the chosen cards" or excludes them from
    // a searched-zone remainder. Publish it before the continuation resolves
    // so a typed `Not(InTrackedSet)` excludes every selected card.
    let continuation_consumes_tracked_set = state
        .active_ability_continuation()
        .or_else(|| {
            state
                .outer_ability_continuation_of_active_post_replacement_draw()
                .map(|continuation| &continuation.pending)
        })
        .is_some_and(|continuation| effects::chain_references_tracked_set(&continuation.chain));
    if continuation_consumes_tracked_set {
        effects::publish_fresh_tracked_set(state, chosen.to_vec());
    }
    let mut has_delivery = false;
    if state.active_ability_continuation().is_some() {
        let mut frame = state
            .take_active_ability_continuation()
            .expect("checked active continuation must be consumable")
            .expect("checked active continuation must exist");
        has_delivery = matches!(frame.pending.chain.effect, Effect::ChangeZone { .. });
        frame.pending.search_attach_host =
            effects::change_zone::resolve_search_continuation_attach_host(
                state,
                &frame.pending.chain,
            );
        state.resolving_continuation_attach_host = frame.pending.search_attach_host;
        let mut targets: Vec<_> = chosen.iter().copied().map(TargetRef::Object).collect();
        // CR 701.23a + CR 701.24a: propagate the semantic searcher for
        // library-owner-sensitive shuffle and tail instructions.
        if player != frame.pending.chain.controller {
            targets.push(TargetRef::Player(player));
        }
        frame.pending.chain.targets = targets.clone();
        propagate_targets_through_search_shuffle(&mut frame.pending.chain, &targets);
        state.push_ability_continuation(frame);
    } else if let Some(continuation) =
        state.outer_ability_continuation_of_active_post_replacement_draw()
    {
        has_delivery = matches!(continuation.pending.chain.effect, Effect::ChangeZone { .. });
        let search_attach_host = effects::change_zone::resolve_search_continuation_attach_host(
            state,
            &continuation.pending.chain,
        );
        let mut targets: Vec<_> = chosen.iter().copied().map(TargetRef::Object).collect();
        if player != continuation.pending.chain.controller {
            targets.push(TargetRef::Player(player));
        }
        let continuation = state
            .outer_ability_continuation_of_active_post_replacement_draw_mut()
            .expect("checked paired continuation must remain resident while the draw is active");
        continuation.pending.search_attach_host = search_attach_host;
        continuation.pending.chain.targets = targets.clone();
        propagate_targets_through_search_shuffle(&mut continuation.pending.chain, &targets);
    }
    if has_delivery {
        state.pending_library_search_delivery = Some(
            crate::types::game_state::LibrarySearchDeliveryResume::Standard { searcher: player },
        );
    } else {
        // CR 701.23a + CR 701.24a: no leading found-card movement belongs to
        // this protocol (fail-to-find or a leading Shuffle), so the zero-move
        // delivery settles before any shuffle/arbitrary tail begins.
        state.active_library_searches.remove(&player);
    }
    // CR 605.3b + CR 616.1: resume-aware — a parked mana-cost cursor settles
    // before (and instead of stranding) the ordinary rider.
    super::engine::resume_pending_continuation_if_priority(state, events)
        .expect("a settled search choice must resume its continuation");
    collect_search_observer_triggers(state, events, events_before_drain)
}

/// CR 800.4a + CR 701.23a: If the exact hidden zone backing an ordinary
/// SearchChoice leaves the game, that search finds nothing. Settle the existing
/// protocol through its normal delivery/shuffle continuation without proposing
/// SearchFound events for stale candidate ids.
pub(crate) fn settle_search_after_zone_owner_elimination(
    state: &mut GameState,
    player: crate::types::player::PlayerId,
    split: Option<crate::types::ability::SearchDestinationSplit>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    state.active_search_decision_controls.remove(&player);
    if let Some(split) = split {
        let source_id = state
            .active_ability_continuation()
            .map(|continuation| continuation.chain.source_id)
            .unwrap_or(ObjectId(0));
        match apply_search_partition(state, &[], &[], &split, source_id, player, events)? {
            crate::game::zone_pipeline::BatchMoveResult::Done => {
                set_priority(state, player);
                super::engine::resume_pending_continuation_if_priority(state, events)?;
            }
            crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => {
                unreachable!("an empty search partition cannot require a replacement choice")
            }
        }
    } else {
        let _ = finalize_standard_search_selection(state, player, &[], events);
    }
    Ok(())
}

/// `change_zone::resolve` ETB pipeline (carrying `enter_tapped` so ETB-tapped
/// REPLACEMENT effects can intercept — "lands you control enter untapped
/// instead"); `rest_ids` are routed to `rest_destination` via the shared rest
/// mover. The `Shuffle` continuation drain is the caller's responsibility.
fn apply_search_partition(
    state: &mut GameState,
    primary_ids: &[ObjectId],
    rest_ids: &[ObjectId],
    split: &crate::types::ability::SearchDestinationSplit,
    source_id: ObjectId,
    controller: crate::types::player::PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<crate::game::zone_pipeline::BatchMoveResult, EngineError> {
    let mut requests = Vec::with_capacity(primary_ids.len());
    for object_id in primary_ids {
        let mut request = crate::game::zone_pipeline::ZoneMoveRequest::effect(
            *object_id,
            split.primary_destination,
            source_id,
        );
        request.mods.enter_tapped = split.primary_enter_tapped;
        requests.push(request);
    }
    Ok(
        crate::game::zone_pipeline::move_objects_simultaneously_then(
            state,
            requests,
            Some(
                crate::types::game_state::BatchCompletion::SearchPartitionPrimaryDelivered {
                    rest_ids: rest_ids.to_vec(),
                    rest_destination: split.rest_destination,
                    source_id,
                    resume: crate::types::game_state::LibrarySearchDeliveryResume::Standard {
                        searcher: controller,
                    },
                },
            ),
            events,
        ),
    )
}

/// CR 701.38: The mutable round state of a `WaitingFor::VoteChoice`. Bundles
/// every field so the single ballot-tally authority
/// ([`append_vote_ballot_and_advance`]) can be shared by both the named
/// (`ChooseOption`) and object (`SubmitVoteCandidate`) submission arms without
/// duplicating the advance/resolve logic across them.
struct VoteRoundState {
    player: crate::types::player::PlayerId,
    remaining_votes: u32,
    options: Vec<String>,
    option_labels: Vec<String>,
    remaining_voters: Vec<(crate::types::player::PlayerId, u32)>,
    tallies: Vec<u32>,
    ballots: crate::im::Vector<(crate::types::player::PlayerId, u32)>,
    // Mirrors `WaitingFor::VoteChoice.per_choice_effect`'s boxed shape so this
    // round-state can be moved into it directly without re-boxing.
    #[allow(clippy::vec_box)]
    per_choice_effect: Vec<Box<crate::types::ability::AbilityDefinition>>,
    controller: crate::types::player::PlayerId,
    source_id: ObjectId,
    actor: crate::types::game_state::VoteActor,
    tally_mode: crate::types::ability::VoteTally,
    candidate_objects: crate::im::Vector<ObjectId>,
    outcome_template: Option<Box<crate::types::ability::AbilityDefinition>>,
    visibility: crate::types::ability::VoteVisibility,
}

/// CR 701.38 + CR 608.2c: The single ballot-tally authority. Records one
/// validated ballot at `idx` for `round.player`, then either advances to the
/// same voter's next vote (CR 701.38d), the next voter (CR 101.4), or — once
/// every voter has voted — fans out the per-choice sub-effects via
/// `vote::resolve_tally` and drains the post-vote continuation.
///
/// `idx` is the ballot index: for named votes it indexes `options`; for object
/// votes it indexes `candidate_objects`. Validation (membership / range) is the
/// caller's responsibility — this function trusts `idx` to be in range for the
/// active tallies vector.
///
/// Secret ballots (`VoteVisibility::Secret`) suppress the per-ballot public
/// `VoteCast` event; the single `VoteResolved` emitted when the queue empties is
/// the simultaneous reveal.
fn append_vote_ballot_and_advance(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    idx: u32,
    round: VoteRoundState,
) -> ResolutionChoiceOutcome {
    let VoteRoundState {
        player,
        remaining_votes,
        options,
        option_labels,
        remaining_voters,
        tallies,
        ballots,
        per_choice_effect,
        controller,
        source_id,
        actor,
        tally_mode,
        candidate_objects,
        outcome_template,
        visibility,
    } = round;

    let mut new_tallies = tallies;
    new_tallies[idx as usize] += 1;
    let mut new_ballots = ballots;
    new_ballots.push_back((player, idx));

    // CR 701.38: Emit the public ballot event unless this is a secret vote —
    // secret ballots are revealed simultaneously at `VoteResolved`, so the
    // per-ballot `VoteCast` is withheld to avoid leaking the choice early.
    if visibility != crate::types::ability::VoteVisibility::Secret {
        let choice_label = options.get(idx as usize).cloned().unwrap_or_default();
        events.push(GameEvent::VoteCast {
            voter: player,
            choice: choice_label,
            source_id,
        });
    }

    if remaining_votes > 1 {
        // CR 701.38d: Same player still has votes to cast.
        state.waiting_for = WaitingFor::VoteChoice {
            player,
            remaining_votes: remaining_votes - 1,
            options,
            option_labels,
            remaining_voters,
            tallies: new_tallies,
            ballots: new_ballots,
            per_choice_effect,
            controller,
            source_id,
            actor,
            tally_mode,
            candidate_objects,
            outcome_template,
            visibility,
        };
        ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
    } else if let Some(((next_player, next_votes), rest)) = remaining_voters.split_first() {
        // CR 101.4: Advance to the next voter in turn order.
        state.waiting_for = WaitingFor::VoteChoice {
            player: *next_player,
            remaining_votes: *next_votes,
            options,
            option_labels,
            remaining_voters: rest.to_vec(),
            tallies: new_tallies,
            ballots: new_ballots,
            per_choice_effect,
            controller,
            source_id,
            actor,
            tally_mode,
            candidate_objects,
            outcome_template,
            visibility,
        };
        ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
    } else {
        // CR 701.38: All votes cast — emit the final tally (the reveal step for
        // secret ballots), fan out per-choice sub-effects, then drain any
        // post-Vote continuation.
        events.push(GameEvent::VoteResolved {
            source_id,
            tallies: options
                .iter()
                .cloned()
                .zip(new_tallies.iter().copied())
                .collect(),
        });
        let candidate_object_ids: Vec<ObjectId> = candidate_objects.iter().copied().collect();
        let _ = effects::vote::resolve_tally(
            state,
            source_id,
            controller,
            &options,
            &per_choice_effect,
            &new_tallies,
            &new_ballots,
            tally_mode,
            &candidate_object_ids,
            outcome_template.as_deref(),
            events,
        );
        ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, controller, events))
    }
}

/// CR 732.1a + CR 732.1b — THE BOUNDARY RE-CHECK IS THE SHORTCUT SYSTEM DOING ITS JOB.
///
/// CR 732.1a: "The rules for taking shortcuts are largely informal. As long as each player in the
/// game understands the intent of each other player, any shortcut system they use is acceptable."
/// This engine IS the table's shortcut system, and CR 732.1b states the job that system performs:
/// the shortcut rules determine "how many times those actions are repeated without having to
/// actually perform them, and HOW THE LOOP IS BROKEN". Defining and enforcing where an elided
/// infinite loop closes is that second clause, not a departure from it.
///
/// THE INVARIANT THIS GATE PROTECTS (full statement, with lemmas, at `types::game_state`'s
/// `scheduled_collapse_axes` doc): ELISION ≡ PERFORMANCE — the engine never advances to a state
/// that performing the proposal's choices would not produce, which is exactly what CR 732.2c means
/// by reaching the ending point "with all game choices contained in the shortcut proposal having
/// been taken". Once an observer appears mid-window, a batched advance would reach a state those
/// choices would NOT produce, and replaying an observer-laden sequence would execute a proposal
/// nobody accepted. Declining to manual play is the only CR 732-faithful option left, so THIS GATE
/// ENFORCES CR 732.2c — the deviation would be either alternative it forecloses.
///
/// So this re-check is not the engine second-guessing an accepted proposal. It is the system
/// establishing, at the point where the elided loop closes, what the table agreed would happen.
/// When the growth is no longer observed the elision no longer describes the board, the shortcut
/// does not close there, and the axis stays `∞` for manual play — every player keeps priority and
/// every choice they would have had. Nobody loses an entitlement they accepted; what they lose is
/// a shortcut that had stopped matching the game.
///
/// Earlier revisions of this doc called the re-check unlicensed. That conceded a rule the code
/// satisfies. The subsystem's single authority for the full four-position reading is
/// `types/game_state.rs`'s `scheduled_collapse_axes` doc; see also `game/derived_views.rs`'s
/// `THE WINDOW'S TIMING IS CR 732.2c'S ADVANCE` block. `derived_views::FamilyCollapseState` still
/// separates `Committed` from the weaker variants, because being right about WHEN the loop closes
/// is not the same as knowing WHAT NUMBER lands, and `∞→N` is a promise about the number.
///
/// CR 732.2a supplies the CONTENT of the re-check, and its antecedent is worth stating exactly so
/// nobody over-reads it: "at any point in the game, THE PLAYER WITH PRIORITY MAY SUGGEST a shortcut
/// … that may be legally taken based on the current game state and THE PREDICTABLE RESULTS of the
/// sequence of choices" — subject = the proposing player, moment = suggestion. It is a legality
/// condition on a PROPOSAL, so it is not self-executing at the boundary; what re-applies it there is
/// the shortcut system's CR 732.1a mandate to close the loop the way the table understood it. The
/// rule says what "predictable results" means; CR 732.1a/1b say who checks and when. The two
/// firewalls this boundary re-calls are `analysis::resource::counter_growth_is_observed` and
/// `analysis::resource::life_growth_is_observed` — named by symbol, never by line.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ObservedGrowth {
    pub(crate) counter: bool,
    pub(crate) life: bool,
}

impl ObservedGrowth {
    /// Evaluated ONCE before the boundary's apply loop, at the same hoist point as the two calls it
    /// replaces. The firewalls scan the board/stack, not the stash, so they need no sequence.
    pub(crate) fn at_boundary(state: &GameState) -> Self {
        Self {
            counter: crate::analysis::resource::counter_growth_is_observed(state),
            life: crate::analysis::resource::life_growth_is_observed(state),
        }
    }
}

/// A way the boundary finishes a stashed item WITHOUT applying it — leaving the axis ∞ with no
/// finite amount reaching it. NO CR GOVERNS THIS ENUM: it is a census of THIS engine loop's own
/// control flow at the boundary described on [`ObservedGrowth`], not a rules behavior
/// (cf. `game/filter.rs`'s `context_free_prop_matches_face` Kleene `AnyOf` arm).
///
/// MEASURED CENSUS of the loop below, not a guess. THE COUNTING UNIT IS THE CONTROL-FLOW STATEMENT
/// (`continue` / `return` / the single `collapsed.push`), because that is the unit an edit adds one
/// of; counting "kinds of exit" instead is what made the earlier version of this paragraph fail to
/// sum. The loop body has exactly FOUR, and they decompose 1 + 2 + 1 = 4:
///   • 1 PUSH — `collapsed.push(item.clone())`, the single apply-succeeded exit.
///   • 2 ITEM-LEVEL NON-PUSH — the `boundary_declines` `continue` ([`BoundaryHold::ObservedGrowth`])
///     and the `active_copy_token()` `return` ([`BoundaryHold::CopyTokenPause`]). These two, and
///     only these two, are what [`possible_hold`] enumerates: 2 statements, 2 variants.
///   • 1 INNER PER-GROWTH SKIP — `!state.battlefield.contains(&g.object)` (CR 400.7: an object that
///     changes zones becomes a new object, so the stale id is skipped). It is a `continue` on the
///     INNER `for g in growths` loop, so its ITEM still reaches the push. It is NOT a hold, and
///     mistaking it for one is the reading error this doc exists to prevent.
/// So: 3 of the 4 statements are non-push, and 2 of those 3 are holds.
/// `boundary_hold_census_matches_the_apply_loop` re-derives all three numbers from this file's own
/// source text, so an added or removed exit reds it instead of silently invalidating this paragraph.
/// Today's loop happens to use only `continue` and `return`, but the census counts `break` and `?`
/// as well — the earlier detector did not, and a `break` skips the push for its item AND every
/// later one, which is the failure this whole enum exists to make impossible.
///
/// This is the badge's question. [`boundary_declines`] answers a strictly narrower one, and a
/// promise derived from it alone is FALSE for `Tokens`, whose only hold is a pause.
///
/// # The citation gate for this subsystem
///
/// STEP (0) IS NOT ONE OF FOUR — IT RUNS FIRST AND CAN END THE SEARCH. Before citing any rule for
/// code in this subsystem, read `types/game_state.rs`'s `scheduled_collapse_axes` doc: it is the
/// SINGLE AUTHORITY for how this subsystem stands under CR 732, and if your question is answered
/// there, cite it rather than re-deriving a reading beside it.
///
/// A WARNING WITH A WORKED EXAMPLE, because this step used to say the opposite. It previously told
/// you that finding an existing "no CR licenses" admission ENDED the search — that looking for a
/// rule was itself the category error. That instruction was wrong, and it was self-sealing: the
/// admissions were written before the reading existed, and the gate then prevented anyone from
/// checking whether they were true. Five citations were tried and rejected on this branch before
/// the reading was found, which is what made the admissions look load-bearing. **An admission in
/// the code is evidence about what a previous author concluded, never evidence about what the
/// rules say.** Re-verify it against the text like any other claim.
///
/// THE CONSTRUCTIVE FORM OF STEP (0): A LICENSE CLAIM MUST NAME ITS INVARIANT. Do not assert that a
/// rule licenses a site; state the property the site preserves and show the rule is about that
/// property. Here the invariant is ELISION ≡ PERFORMANCE (`types::game_state`'s
/// `scheduled_collapse_axes` doc), and CR 732.1a is what covers differences in the SYSTEM'S FORM so
/// long as that invariant holds. A claim with no named invariant is the same error as an admission
/// with no verification — both skip the step where someone could check.
///
/// The remaining steps always
/// apply: (1) EXISTENCE — grep the rule number in `docs/MagicCompRules.txt`;
/// (2) CONTENT, ON BOTH ANTECEDENT AXES — *subject* (who or what the antecedent is about; does it
/// describe this code's actual matched text or predicate?) and *time* (an antecedent fixes a moment;
/// a permission granted at proposal time does not travel past acceptance, and a legality condition
/// on a proposal does not become a continuing-validity condition); (3) NORMATIVE DIRECTION — is this
/// rule the one the code *applies*, or the one the code *deviates from*? Citing the deviated-from
/// rule as a license inverts the annotation's meaning.
///
/// "NO CR GOVERNS THIS" IS STILL A SAFE, CORRECT, FINAL VERDICT WHERE IT IS TRUE — reaching for the
/// next closest-sounding rule remains the error. In-tree precedent that survives: `game/filter.rs`'s
/// `context_free_prop_matches_face` Kleene `AnyOf` arm, which answers an `Option<bool>` question
/// that is not a rules behavior at all.
///
/// THE TWO FAILURE MODES ARE SYMMETRIC, AND THIS SUBSYSTEM HAS NOW COMMITTED BOTH. Citing a
/// closest-sounding rule as a license inverts an annotation's meaning; conceding a deviation the
/// code does not actually commit gives away a rule the implementation satisfies, and it is the
/// harder error to detect because it reads as rigor. Prefer the reading you can defend clause by
/// clause, and where a clause genuinely does not reach the code, say so — but only after checking.
///
/// DO NOT COPY A CITATION SET FROM ONE SITE TO ANOTHER. The parser
/// (`parse_optional_token_substitution_choice` in `oracle_replacement.rs`)
/// answers "what does this card's text create?"; this boundary answers "why does this mint pause?"
/// Same card, different questions, different rules.
///
/// CR 614.1 / CR 614.1a TRAVEL TO THE BOUNDARY WHERE CR 608.2d CANNOT, because 614.1 is
/// EVENT-SCOPED IN THE PERMISSIVE DIRECTION — "replacement effects apply continuously as events
/// happen—they aren't locked in ahead of time" — and 614.1a is DEFINITIONAL, classifying what a
/// replacement effect *is*, so it holds wherever such an effect exists. CR 608.2d is SOURCE-SCOPED
/// — "if an effect OF A SPELL OR ABILITY offers any choices" — and at a source-less mint its
/// antecedent is false by construction. Definitional and continuously-applying rules are
/// site-portable; source-scoped rules are site-local.
///
/// CITE BY SYMBOL OR BY HEADING TEXT, NEVER BY LINE. No survivor list, no exemption for a cited
/// file the change happens not to touch.
///
/// The rule has now failed twice, each time through its own carve-out. The first form exempted
/// files the edit itself re-derives; this change then moved `derived_views.rs`'s CR 732 doctrine block
/// ~120 lines and shipped five citations pointing at unrelated code, which read as "the no-CR
/// precedent does not exist". The second form exempted a cited file *untouched by the change* and
/// named three survivors — but that makes staleness depend on WHO edits rather than on whether the
/// anchor can drift, and every survivor sat in a high-churn file (`replacement.rs` is 9000+ lines)
/// where the next unrelated edit above it reloads the same gun. A carve-out that keeps needing to
/// be renegotiated is the class, not an exception to it.
///
/// So there are none. Every citation in the enrolled files names a symbol or a greppable heading,
/// both of which move WITH the code they point at. Every target converted cleanly — no cited
/// location needed a heading added to it — so no carve-out language was needed either.
///
/// ENFORCED, NOT REQUESTED: `subsystem_citations_are_symbol_anchored` discovers its population by
/// an opt-in marker comment and reds on any surviving line anchor. The rule used to be prose that
/// only a reviewer could apply, which is how it shipped false twice. Read that test's doc for the
/// residual hole it deliberately does not claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum BoundaryHold {
    /// An observer of the growing class appeared accept→boundary, so the batched single-application
    /// would no longer match the sequence's own result, and the engine declines it: `continue`, no
    /// push, ∞ left for manual play. Runtime gate: [`boundary_declines`].
    ///
    /// THIS ARM ENFORCES CR 732.2c. The sentence above states the invariant without naming it: the
    /// batched single-application "would no longer match the sequence's own result" — that is
    /// ELISION ≢ PERFORMANCE, detected. CR 732.2c defines the advance as reaching the ending point
    /// "with all game choices contained in the shortcut proposal having been taken", so the end
    /// state must be the state those choices produce. Applying the batch anyway would land a state
    /// they would NOT produce; replaying an observer-laden sequence would execute a proposal nobody
    /// accepted. Declining to manual play is the only remaining CR 732-faithful option, and it
    /// costs no player a decision — everyone keeps priority and performs the actions. The full
    /// three-route statement with lemmas is at `types::game_state`'s `scheduled_collapse_axes` doc.
    ///
    /// An earlier revision of this doc said "NOT LICENSED BY ANY CR" and listed CR 732.1a/1b/2a/2b
    /// as rules that "look close and are not". That list was built on the assumption that CR 732.2
    /// is the exclusive procedure; CR 732.1a's plain text ("any shortcut system they use is
    /// acceptable") is what refutes it, and CR 732.1b names this exact job — the shortcut rules
    /// determine "how many times those actions are repeated … and how the loop is broken".
    /// Declining an elision is not a deviation from a rule that licenses elision: the ELISION is
    /// what needs a license, and its absence never does.
    ObservedGrowth,
    /// CR 614.1 + CR 614.1a: the fodder mint parked on a replacement choice, so the arm returns
    /// through its pause transaction before the push — zero tokens minted, no finite amount chosen,
    /// axis stays ∞. CR 614.1 is why an "instead" replacement still applies here: replacement effects
    /// "apply continuously as events happen—they aren't locked in ahead of time" and "watch for a
    /// particular event", and "you would create one or more tokens" is exactly the event this mint is.
    /// CR 614.1a supplies only the CLASSIFICATION — that an "instead" effect is a replacement effect.
    ///
    /// WHY THE MINT IS SOURCE-LESS (the `ObjectId(0)` sentinel below): because it happens inside the
    /// engine's deferral window, not during any spell or ability resolution. Under CR 732.2c the
    /// growth would already have been applied at accept and there would be no boundary mint at all —
    /// so 732.2c is the rule this DEVIATES FROM, not the rule that authorizes the sentinel. See
    /// [`ObservedGrowth`] and `types/game_state.rs`'s `scheduled_collapse_axes` doc.
    ///
    /// TWO ingresses, both parking on the one `active_copy_token()` guard. Each names the arm of
    /// `token_copy.rs`'s `replace_event` match that produces it:
    ///   • the `ReplacementResult::NeedsChoice` arm — from ONE optional candidate
    ///     (`replacement.rs`'s `replacement_is_optional` single-candidate branch; CR 614.1a
    ///     "instead", e.g. Jinnie Fay, Jetmir's Second) or from ≥2 materially-ordered candidates
    ///     (`replacement.rs`'s `replacement_ordering_is_material` branch; CR 616.1, whose text
    ///     is scoped to "two or more" and so covers ONLY that ingress);
    ///   • the `Execute` arm's `apply_create_token_after_replacement == false` early return.
    /// NOT a hold: the `ReplacementResult::Prevented` arm mints zero tokens but does not
    /// park, so the arm reaches its push — see [`materialization_certainty`].
    ///
    /// TWO RULES THAT DO NOT APPLY HERE:
    ///   • CR 608.2d — "if an effect OF A SPELL OR ABILITY offers any choices"; at a source-less mint
    ///     that antecedent is false by construction. It is correct at the PARSER
    ///     (`parse_optional_token_substitution_choice` in `oracle_replacement.rs`) and is
    ///     deliberately not imported.
    ///   • CR 614.16 — "if an EFFECT would create one or more tokens"; the parsed tag is
    ///     "if YOU would create" — that parser's literal
    ///     `tag("if you would create one or more tokens, ")`.
    CopyTokenPause,
}

impl BoundaryHold {
    /// The full variant set. Production code never enumerates holds — it branches on
    /// [`boundary_declines`] and on the one `active_copy_token()` guard — so this exists purely
    /// for the completeness half of `boundary_hold_census_matches_the_apply_loop`, which is what
    /// keeps a variant no kind can reach from being added.
    #[cfg(test)]
    pub(crate) const ALL: [BoundaryHold; 2] = [Self::ObservedGrowth, Self::CopyTokenPause];
}

/// The hold this item's KIND can take, independent of live state. `None` => the arm reaches the
/// single `collapsed.push` unconditionally. No CR governs this — it is the control-flow census
/// described on [`BoundaryHold`].
pub(crate) fn possible_hold(item: &PersistentAxisMaterialization) -> Option<BoundaryHold> {
    match item {
        PersistentAxisMaterialization::Tokens(_) => Some(BoundaryHold::CopyTokenPause),
        PersistentAxisMaterialization::Counters(_) | PersistentAxisMaterialization::Life { .. } => {
            Some(BoundaryHold::ObservedGrowth)
        }
        // No non-push exit: `drive_persistent_axis_collapse` holds a `SimulationProbeGuard` so it
        // cannot park, and `break`s to commit the successful prefix on a failed cycle — the arm
        // always reaches the push.
        PersistentAxisMaterialization::DriveSequence { .. } => None,
    }
}

/// What the HUD may promise. `Conditional` iff the kind has a hold. No CR governs this — it is a
/// display promise derived from the census above, not a rules behavior.
///
/// APPLIED-BUT-NULLIFIED is deliberately `Committed`: a `Counters` item whose bearers all left
/// (CR 400.7, the inner stale-id skip), a `Prevented` mint, and a `DriveSequence` committing k<N all
/// PUSH, so the ∞ genuinely ends. The shipped copy promises "a finite amount will be chosen", not a
/// quantity.
pub(crate) fn materialization_certainty(
    item: &PersistentAxisMaterialization,
) -> crate::game::derived_views::CollapseCertainty {
    match possible_hold(item) {
        Some(_) => crate::game::derived_views::CollapseCertainty::Conditional,
        None => crate::game::derived_views::CollapseCertainty::Committed,
    }
}

/// THE boundary's decline gate — the runtime half of [`BoundaryHold::ObservedGrowth`], whose rules
/// frame (CR 732.1a/1b: the shortcut system decides how the loop is broken) is stated there. SINGLE AUTHORITY: the loop branches on this instead of
/// per-arm `if *_observed_now`.
pub(crate) fn boundary_declines(
    item: &PersistentAxisMaterialization,
    observed: ObservedGrowth,
) -> bool {
    match item {
        PersistentAxisMaterialization::Tokens(_)
        | PersistentAxisMaterialization::DriveSequence { .. } => false,
        PersistentAxisMaterialization::Counters(_) => observed.counter,
        PersistentAxisMaterialization::Life { .. } => observed.life,
    }
}

pub(super) fn handle_resolution_choice(
    state: &mut GameState,
    waiting_for: WaitingFor,
    action: GameAction,
    events: &mut Vec<GameEvent>,
) -> Result<ResolutionChoiceOutcome, EngineError> {
    let outcome = match (waiting_for, action) {
        (
            WaitingFor::MeldPairChoice { player, choices },
            GameAction::ChooseMeldPair {
                source_id,
                partner_id,
            },
        ) => {
            let context = choices
                .into_iter()
                .find(|choice| choice.source_id == source_id && choice.partner_id == partner_id)
                .ok_or_else(|| {
                    EngineError::InvalidAction(
                        "meld selection is not one of the offered pairs".to_string(),
                    )
                })?;
            state.waiting_for = WaitingFor::Priority { player };
            crate::game::meld::begin_selected_meld(state, context, events);
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        (
            WaitingFor::MeldAttackTargetChoice {
                player,
                context,
                valid_targets,
            },
            GameAction::ChooseEntryAttackTarget { target },
        ) => {
            // CR 508.4: the entering creature's controller chooses one of the
            // engine-issued defending players, planeswalkers, or battles.
            // `entry_attack_target_defender` applies CR 508.4a if it went stale.
            if !valid_targets.contains(&target) {
                return Err(EngineError::InvalidAction(
                    "entry attack target is not one of the offered destinations".to_string(),
                ));
            }
            state.waiting_for = WaitingFor::Priority { player };
            crate::game::meld::finish_meld_attack_choice(state, context, target, events);
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        (
            WaitingFor::EntryAttackTargetChoice {
                player,
                object_id,
                valid_targets,
            },
            GameAction::ChooseEntryAttackTarget { target },
        ) => {
            // CR 508.4: the entering creature's controller chooses one of the
            // engine-issued defending players, planeswalkers, or battles.
            // `entry_attack_target_defender` applies CR 508.4a if it went stale.
            if !valid_targets.contains(&target) {
                return Err(EngineError::InvalidAction(
                    "entry attack target is not one of the offered destinations".to_string(),
                ));
            }
            state.waiting_for = WaitingFor::Priority { player };
            if let Some(defending_player) =
                crate::game::combat::entry_attack_target_defender(state, player, target)
            {
                crate::game::combat::enter_attacking_at_target(
                    state,
                    object_id,
                    defending_player,
                    target,
                );
            }
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
        }
        (
            WaitingFor::ScryChoice { player, cards },
            GameAction::SelectCards { cards: top_cards },
        ) => {
            let all_cards = cards;
            // CR 701.22a: the keep-on-top set must be a duplicate-free subset of
            // the looked-at cards (any order is legal).
            validate_keep_on_top_selection(&top_cards, &all_cards)?;
            let bottom_cards: Vec<_> = all_cards
                .iter()
                .filter(|id| !top_cards.contains(id))
                .copied()
                .collect();
            let player_state = state
                .players
                .iter_mut()
                .find(|candidate| candidate.id == player)
                .expect("player exists");
            // allow-raw-zone: scry reorder never leaves the library (CR 701.22a).
            player_state.library.retain(|id| !all_cards.contains(id));
            for (index, &card_id) in top_cards.iter().enumerate() {
                // allow-raw-zone: scry reorder never leaves the library (CR 701.22a).
                player_state.library.insert(index, card_id);
            }
            for &card_id in &bottom_cards {
                // allow-raw-zone: scry reorder never leaves the library (CR 701.22a).
                player_state.library.push_back(card_id);
            }
            state.advance_library_knowledge_epoch(player);
            // CR 701.22a + CR 701.22d: a scry event occurs only after the
            // controller has completed its top/bottom choices. Keep both the
            // clamped look count and an explicit `Some(0)` bottom count on the
            // event that observers preserve into their eventual trigger.
            let resumed_events_start = events.len();
            events.push(GameEvent::PlayerPerformedAction {
                player_id: player,
                action: crate::types::events::PlayerActionKind::Scry,
                look_count: Some(all_cards.len() as u32),
                scry_bottom_count: Some(bottom_cards.len() as u32),
                scry_top_count: Some(all_cards.len() as u32 - bottom_cards.len() as u32),
            });
            // CR 401.5 + CR 611.3a: Scry reorders the library top directly (not
            // through the zone-move seam), so a continuous `TopOfLibraryMatches`
            // static must be re-evaluated — self-gated so it's a no-op otherwise.
            crate::game::layers::mark_layers_full_if_top_of_library_static_live(state);
            // CR 603.2 + CR 603.3b: the resumed continuation can perform further
            // observable game actions (e.g. a SECOND "scry N" in the same
            // resolution — "whenever you scry" fires once per scry event) and
            // pause again on another prompt before this action settles to
            // Priority. `run_post_action_pipeline` only scans an action's events
            // at a Priority settlement, so without parking here the resumed
            // slice's events (the second scry's `PlayerPerformedAction`, which
            // carries that scry's own effective look count) are dropped and its
            // trigger is silently lost. Park the resumed slice into
            // `deferred_triggers` (B2, mirroring
            // `batch_or_drain_observer_triggers`); the queue drains with each
            // trigger's own preserved event once resolution truly settles.
            let waiting_for = finish_with_continuation(state, player, events);
            crate::game::triggers::park_observer_triggers_if_paused(
                state,
                events,
                resumed_events_start,
            );
            ResolutionChoiceOutcome::WaitingFor(waiting_for)
        }
        (
            WaitingFor::ArrangePlanarDeckTopChoice {
                player,
                cards,
                keep_on_top,
            },
            GameAction::SelectCards { cards: top_cards },
        ) => {
            validate_exact_keep_on_top_selection(&top_cards, &cards, keep_on_top)?;
            let bottom_cards: Vec<_> = cards
                .iter()
                .filter(|id| !top_cards.contains(id))
                .copied()
                .collect();
            state.planar_deck.retain(|id| !cards.contains(id));
            for (index, &card_id) in top_cards.iter().enumerate() {
                state.planar_deck.insert(index, card_id);
            }
            for card_id in bottom_cards {
                state.planar_deck.push_back(card_id);
            }
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
        }
        (
            WaitingFor::RedistributeLifeTotals { player, options },
            GameAction::SubmitLifeRedistribution { option_index },
        ) => {
            // CR 119.7 + CR 119.8: apply the chosen assignment. Every enumerated
            // option is already legal because the resolver filtered each receiver.
            let option = options.get(option_index).ok_or_else(|| {
                EngineError::InvalidAction(format!(
                    "Life redistribution option {option_index} out of range"
                ))
            })?;
            let assignment = option.assignment.clone();
            match effects::life::apply_life_totals_assignment(
                state,
                &assignment,
                player,
                None,
                events,
            )
            .map_err(|err| EngineError::InvalidAction(err.to_string()))?
            {
                // CR 616.1: a competing replacement installed a choice WaitingFor;
                // the resume path completes the assignment and continuation.
                effects::life::LifeAssignmentOutcome::Deferred => {
                    ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
                }
                effects::life::LifeAssignmentOutcome::Applied => {
                    ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(
                        state, player, events,
                    ))
                }
            }
        }
        (
            WaitingFor::CoinFlipKeepChoice {
                player,
                results,
                keep_count,
            },
            GameAction::SelectCoinFlips { keep_indices },
        ) => {
            // CR 614.1a + CR 705.1: the player must keep exactly `keep_count`
            // distinct, in-range flips and ignore the rest.
            if keep_indices.len() != keep_count {
                return Err(EngineError::InvalidAction(format!(
                    "Must keep exactly {keep_count} coin flip(s), got {}",
                    keep_indices.len()
                )));
            }
            let mut seen = std::collections::HashSet::new();
            for &index in &keep_indices {
                if index >= results.len() {
                    return Err(EngineError::InvalidAction(format!(
                        "Coin flip index {index} out of range"
                    )));
                }
                if !seen.insert(index) {
                    return Err(EngineError::InvalidAction(format!(
                        "Duplicate coin flip index {index}"
                    )));
                }
            }
            let kept: Vec<bool> = keep_indices.iter().map(|&index| results[index]).collect();
            let pending = state
                .take_active_coin_flip_frame()
                .map_err(|error| EngineError::InvalidAction(error.to_string()))?
                .ok_or_else(|| {
                    EngineError::InvalidAction("No active coin-flip frame to resume".to_string())
                })?;
            let next =
                crate::game::effects::flip_coin::resume_after_keep(state, pending, kept, events)
                    .map_err(|error| EngineError::InvalidAction(format!("{error}")))?;
            // CR 608.2c: re-suspended for another interactive choice, else the
            // whole flip effect completed — drain back to Priority.
            let wf = match next {
                Some(wf) => wf,
                None => finish_with_continuation(state, player, events),
            };
            ResolutionChoiceOutcome::WaitingFor(wf)
        }
        (
            WaitingFor::ManifestDreadChoice {
                player,
                cards,
                source_id,
            },
            GameAction::SelectCards {
                cards: selected_cards,
            },
        ) => {
            if selected_cards.len() != 1 || !cards.contains(&selected_cards[0]) {
                return Err(EngineError::InvalidAction(
                    "Must select exactly 1 card from the manifest dread choices".to_string(),
                ));
            }

            let manifest_id = selected_cards[0];
            let graveyard_cards: Vec<_> = cards
                .iter()
                .filter(|&&id| id != manifest_id)
                .copied()
                .collect();

            let face_down = crate::types::ability::FaceDownProfile::vanilla_2_2();
            match crate::game::zone_pipeline::move_object(
                state,
                crate::game::zone_pipeline::ZoneMoveRequest::effect(
                    manifest_id,
                    Zone::Battlefield,
                    source_id,
                )
                // CR 608.2c + CR 701.62a: the same producer as `manifest_card`,
                // reached through the two-card choice instead of synchronously.
                .face_down(face_down)
                .publishing_chain_referent(),
                events,
            ) {
                crate::game::zone_pipeline::ZoneMoveResult::Done => {}
                // CR 303.4f / CR 616.1 + CR 701.62a: the chosen card's manifest
                // entry paused (aura host pick or a replacement-ordering prompt).
                // Defer the non-manifested card's graveyard move + reveal-marker
                // cleanup until the manifest finishes entering — otherwise the
                // other card is graved while the chosen card is still in the
                // library (issue #3245).
                crate::game::zone_pipeline::ZoneMoveResult::NeedsChoice(_)
                | crate::game::zone_pipeline::ZoneMoveResult::NeedsAuraAttachmentChoice => {
                    crate::game::zone_pipeline::defer_completion_on_pause(
                        state,
                        crate::types::game_state::BatchCompletion::RevealRestPile {
                            player,
                            source_id: Some(source_id),
                            rest_cards: graveyard_cards,
                            rest_destination: Zone::Graveyard,
                            rest_order: DigRestOrder::Preserve,
                            clear_markers: cards.clone(),
                            publish_tracked_set: None,
                            emit_reveal_until_resolved: None,
                            // #7467 review round 2: the entry paused, so the
                            // publish below never runs — the completion drain
                            // publishes instead, once the entry completed.
                            manifested_for_continuation: Some(manifest_id),
                        },
                    );
                    return Ok(ResolutionChoiceOutcome::WaitingFor(
                        state.waiting_for.clone(),
                    ));
                }
            }

            // CR 608.2c + CR 701.62a (#7467): the manifested creature enters
            // from THIS continuation, so its `ZoneChanged` never reaches the
            // resolver-side harvest — the chain's tracked set was published
            // EMPTY when the head parked. Re-publish it here so a chained
            // consumer ("Manifest dread X times, then put X +1/+1 counters on
            // each of those creatures" — Valgavoth's Onslaught) binds the
            // creature, the same seam as the search-choice publish above.
            effects::publish_battlefield_object_for_pending_continuation(state, manifest_id);

            // CR 614.6 + CR 701.62a class: route the non-manifested cards to the
            // graveyard through the simultaneous-move batch so each card's own
            // `Moved` redirects (Rest in Peace / Leyline of the Void: "would be
            // put into a graveyard from anywhere → exile instead") fire — a raw
            // `move_to_zone` proposed no per-card ZoneChange and silently skipped
            // them. The reveal-marker cleanup is the post-loop work; it must run
            // exactly once after the whole pile lands, so on a mid-pile CR 616.1
            // pause it is deferred onto the parked batch tail and the drain runs
            // it. The common single-redirect path never pauses and runs cleanup
            // inline below.
            let reqs: Vec<_> = graveyard_cards
                .iter()
                .map(|&card_id| {
                    crate::game::zone_pipeline::ZoneMoveRequest::effect(
                        card_id,
                        Zone::Graveyard,
                        card_id,
                    )
                })
                .collect();
            // The reveal-marker cleanup + continuation drain (the post-loop work)
            // is carried as the batch completion so it runs exactly once whether
            // the pile lands synchronously or across a CR 616.1 pause.
            let completion = crate::types::game_state::BatchCompletion::ManifestDreadCleanup {
                player,
                revealed: cards,
            };
            match crate::game::zone_pipeline::move_objects_simultaneously_then(
                state,
                reqs,
                Some(completion),
                events,
            ) {
                crate::game::zone_pipeline::BatchMoveResult::Done => {
                    // `move_objects_simultaneously_then` already ran the
                    // completion (reveal-marker cleanup + `finish_with_continuation`).
                    ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
                }
                crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => {
                    ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
                }
            }
        }
        (
            WaitingFor::CastOffer {
                player,
                kind:
                    CastOfferKind::Discover {
                        hit_card,
                        exiled_misses,
                        source_id,
                        discover_value,
                    },
            },
            GameAction::DiscoverChoice { choice },
        ) => {
            let cast = matches!(choice, crate::types::actions::CastChoice::Cast);
            if cast {
                // CR 701.57a + CR 608.2g: cast the hit DURING resolution, gated
                // by "resulting spell's mana value is less than or equal to N".
                // The MV check is re-evaluated at finalization (after X), and on
                // rejection the hit goes to the discovering player's hand
                // (`ToHand`) while the misses go to the library bottom.
                let cleanup = crate::types::ability::ResolutionCastCleanup {
                    source_id,
                    exiled_misses,
                    reject_action: crate::types::ability::ResolutionMvRejectAction::ToHand,
                    success_action:
                        crate::types::ability::ResolutionCastSuccessAction::BottomMisses,
                };
                let result = casting::initiate_cast_during_resolution(
                    state,
                    player,
                    hit_card,
                    casting::ResolutionCastRequest {
                        constraint: Some(
                            crate::types::ability::CastPermissionConstraint::ManaValue {
                                comparator: crate::types::ability::Comparator::LE,
                                value: QuantityExpr::Fixed {
                                    value: discover_value as i32,
                                },
                            },
                        ),
                        cast_transformed: false,
                        cleanup,
                        graveyard_replacement: None,
                        cost: crate::types::ability::ResolutionCastCost::Free,
                    },
                    events,
                )?;
                state.waiting_for = result;
                // CR 608.2g + CR 701.57c: casting the discovered card happens
                // DURING this discover's resolution and no player gets priority
                // for it yet, so the discover spell must FINISH resolving now —
                // running any stashed follow-up (Hit the Mother Lode's tapped
                // Treasure creation) before the freshly-cast hit sits on the stack
                // awaiting priority. The to-hand branch reaches the same drain via
                // `finish_with_continuation`; the free-cast branch has no such
                // completion batch, so drain the parked continuation explicitly.
                super::engine::resume_pending_continuation_if_priority(state, events)?;
                ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
            } else {
                // CR 701.57a: decline — hit goes to the discovering player's
                // hand; the misses go to the library bottom in a random order.
                // The raw hit-to-hand move remains outside tranche L1, but its
                // printed tail waits on the replacement-aware bottom batch.
                crate::game::effects::discover::shuffle_to_bottom(
                    state,
                    &exiled_misses,
                    source_id,
                    Some(
                        crate::types::game_state::BatchCompletion::DiscoverDeclined {
                            player,
                            hit_card,
                            source_id,
                        },
                    ),
                    events,
                );
                ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
            }
        }
        // CR 608.2g + CR 609.4b: Paid during-resolution graveyard cast (Quistis
        // Trepe, Tinybones the Pickpocket). Accept → cast the card at its real
        // printed cost through `initiate_cast_during_resolution` with
        // `ResolutionCastCost::FullCost`, which opens a manual mana-payment window
        // and rides the any-type concession onto the grant. Decline → the card
        // stays in the graveyard and resolution continues.
        (
            WaitingFor::CastOffer {
                player,
                kind:
                    CastOfferKind::GraveyardPaidCast {
                        hit_card,
                        mana_spend_permission,
                        graveyard_replacement,
                        cast_transformed,
                        constraint,
                    },
            },
            GameAction::GraveyardPaidCastChoice { choice },
        ) => {
            if matches!(choice, crate::types::actions::CastChoice::Cast) {
                let cleanup = crate::types::ability::ResolutionCastCleanup {
                    source_id: hit_card,
                    exiled_misses: Vec::new(),
                    reject_action: crate::types::ability::ResolutionMvRejectAction::RemainExiled,
                    success_action:
                        crate::types::ability::ResolutionCastSuccessAction::BottomMisses,
                };
                let result = casting::initiate_cast_during_resolution(
                    state,
                    player,
                    hit_card,
                    casting::ResolutionCastRequest {
                        constraint,
                        cast_transformed,
                        cleanup,
                        graveyard_replacement,
                        cost: crate::types::ability::ResolutionCastCost::FullCost {
                            mana_spend_permission,
                        },
                    },
                    events,
                )?;
                ResolutionChoiceOutcome::WaitingFor(result)
            } else {
                // CR 608.2g decline: card stays in the graveyard; nothing is cast.
                ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
            }
        }
        // CR 701.20a + CR 608.2c: "You may put that card onto the battlefield" —
        // the controller routes the kept card after RevealUntil found a hit.
        // Accept → `accept_zone`; decline → `decline_zone`. On decline, when the
        // decline zone IS the rest pile, the hit card joins the misses so the
        // random-order placement covers it in one shuffle (CR 701.20a).
        (
            WaitingFor::RevealUntilKeptChoice {
                player,
                hit_card,
                source_id,
                accept_zone,
                decline_zone,
                enter_tapped,
                enters_attacking,
                revealed_misses,
                rest_destination,
            },
            GameAction::DecideOptionalEffect { accept },
        ) => {
            let mut misses = revealed_misses;
            if accept {
                if accept_zone == Zone::Battlefield {
                    // CR 614.1c + CR 306.5b / CR 310.4b: route the battlefield
                    // entry through the zone-change pipeline so the delivery tail
                    // seeds intrinsic enters-with counters (a kept planeswalker /
                    // battle must enter with its loyalty / defense or it dies to
                    // CR 704.5i) and applies the CR 614.1 tap-state. Mirrors the
                    // synchronous `reveal_until::resolve` battlefield path. The
                    // previous manual `obj.tapped = true` is dropped (the tail does
                    // it from the seeded `EntryMods`).
                    let mut req = crate::game::zone_pipeline::ZoneMoveRequest::effect(
                        hit_card,
                        Zone::Battlefield,
                        source_id,
                    );
                    req.mods.enter_tapped = enter_tapped;
                    req.mods.enters_attacking = enters_attacking;
                    match crate::game::zone_pipeline::move_object(state, req, events) {
                        crate::game::zone_pipeline::ZoneMoveResult::Done => {}
                        // CR 303.4f / CR 616.1: the accepted card's battlefield
                        // entry paused on an as-enters choice. The pause is parked
                        // centrally; defer the rest-pile move + reveal-marker
                        // cleanup onto the batch tail so the drain runs it once the
                        // entry resolves — otherwise the misses strand (the
                        // early-`return` bug). `EffectResolved` was already emitted
                        // before this prompt, so the completion does not re-emit it.
                        crate::game::zone_pipeline::ZoneMoveResult::NeedsChoice(_)
                        | crate::game::zone_pipeline::ZoneMoveResult::NeedsAuraAttachmentChoice => {
                            let mut clear_markers = misses.clone();
                            clear_markers.push(hit_card);
                            crate::game::zone_pipeline::defer_completion_on_pause(
                                state,
                                crate::types::game_state::BatchCompletion::RevealRestPile {
                                    player,
                                    source_id: Some(source_id),
                                    rest_cards: misses,
                                    rest_destination,
                                    rest_order: DigRestOrder::Preserve,
                                    clear_markers,
                                    publish_tracked_set: None,
                                    emit_reveal_until_resolved: None,
                                    manifested_for_continuation: None,
                                },
                            );
                            return Ok(ResolutionChoiceOutcome::WaitingFor(
                                state.waiting_for.clone(),
                            ));
                        }
                    }
                } else {
                    // CR 614.6: a kept card accepted to a non-battlefield zone
                    // (graveyard — Mind Funeral-style "put it into your graveyard"
                    // kept cards, 4 cards — or exile) routes through the pipeline
                    // so a `Moved` graveyard→exile redirect fires. On a CR 616.1
                    // pause, defer the rest-pile move + marker clear onto a
                    // `RevealRestPile` completion (EffectResolved already emitted
                    // before this prompt) and surface the parked prompt.
                    if let Some(outcome) = route_kept_card_or_defer(
                        state,
                        hit_card,
                        accept_zone,
                        source_id,
                        &misses,
                        rest_destination,
                        events,
                    ) {
                        return Ok(outcome);
                    }
                }
            } else if decline_zone == rest_destination {
                misses.push(hit_card);
            } else {
                // CR 614.6: same redirect-consult for a declined kept card sent to
                // a non-rest graveyard/exile destination.
                if let Some(outcome) = route_kept_card_or_defer(
                    state,
                    hit_card,
                    decline_zone,
                    source_id,
                    &misses,
                    rest_destination,
                    events,
                ) {
                    return Ok(outcome);
                }
            }
            // CR 701.20a + CR 614.6: move the rest pile (RIP redirects fire) and
            // run the marker clear + continuation drain as the completion. On a
            // synchronous landing the completion runs inline; on a CR 616.1 pause
            // it defers and the drain runs it once the pile lands. `clear_markers`
            // is the misses plus the kept card (already placed above).
            let mut clear_markers = misses.clone();
            clear_markers.push(hit_card);
            match effects::reveal_until::move_rest_then(
                state,
                &misses,
                rest_destination,
                Some(crate::types::game_state::BatchCompletion::RevealRestPile {
                    player,
                    source_id: Some(source_id),
                    rest_cards: Vec::new(),
                    rest_destination,
                    rest_order: DigRestOrder::Preserve,
                    clear_markers,
                    publish_tracked_set: None,
                    emit_reveal_until_resolved: None,
                    manifested_for_continuation: None,
                }),
                events,
            ) {
                crate::game::zone_pipeline::BatchMoveResult::Done => {
                    // The completion ran inline (`finish_with_continuation`), so
                    // `state.waiting_for` is the post-drain priority/continuation
                    // state.
                    ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
                }
                crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => {
                    ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
                }
            }
        }
        // CR 107.1c + CR 608.2c: "you may repeat this process any number of
        // times" — after one iteration resolved, the controller decides
        // whether to run the process again.
        (
            WaitingFor::RepeatDecision { player, ability },
            GameAction::DecideOptionalEffect { accept },
        ) => {
            if accept {
                // CR 608.2c + CR 107.1c (issue #1032): reset to `Priority`
                // BEFORE re-entering the chain, mirroring the `decline`
                // branch's `finish_with_continuation` reset below and
                // `handle_optional_effect_choice`'s `set_active_priority`
                // reset (engine_payment_choices.rs). Without this,
                // `state.waiting_for` is still the just-answered
                // `RepeatDecision`, which `waits_for_resolution_choice`
                // (effects/mod.rs) matches — the ChangeZone/LoseLife
                // sub-chain following this iteration's RevealTop is then
                // wrongly deferred into `pending_continuation` (accumulating
                // there via `append_to_sub_chain`) instead of resolving
                // immediately, and only drains in one batch when the
                // controller eventually declines.
                set_priority(state, player);
                // Re-resolve one more process pass. `ability` retains
                // `repeat_until: Some(ControllerChoice)`, so this hits the
                // `repeat_until` dispatch, runs `resolve_chain_body` once, and
                // re-sets `WaitingFor::RepeatDecision` (or, on an inner choice,
                // pauses and parks its repeat-until frame). depth = 1: each
                // accept is a fresh top-level `apply()`, so depth never
                // accumulates across prompts and the `depth > 20` guard never
                // applies — CR 107.1c permits looping a whole library.
                effects::resolve_ability_chain(state, &ability, events, 1)
                    .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
                ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
            } else {
                // CR 107.1c: declining ends the loop; drain any trailing chain.
                ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
            }
        }
        (
            WaitingFor::CastOffer {
                player,
                kind:
                    CastOfferKind::Cascade {
                        hit_card,
                        exiled_misses,
                        source_mv,
                        source_id,
                    },
            },
            GameAction::CascadeChoice { choice },
        ) => {
            let cast = matches!(choice, crate::types::actions::CastChoice::Cast);
            if cast {
                // CR 702.85a + CR 608.2g: cast the hit DURING resolution, gated
                // by "resulting spell's mana value is less than this spell's
                // mana value". The MV check is re-evaluated at finalization
                // (after X), and on rejection the hit joins the misses on the
                // library bottom (`BottomWithMisses`).
                let cleanup = crate::types::ability::ResolutionCastCleanup {
                    source_id,
                    exiled_misses,
                    reject_action:
                        crate::types::ability::ResolutionMvRejectAction::BottomWithMisses,
                    success_action:
                        crate::types::ability::ResolutionCastSuccessAction::BottomMisses,
                };
                let result = casting::initiate_cast_during_resolution(
                    state,
                    player,
                    hit_card,
                    casting::ResolutionCastRequest {
                        constraint: Some(
                            crate::types::ability::CastPermissionConstraint::ManaValue {
                                comparator: crate::types::ability::Comparator::LT,
                                value: QuantityExpr::Fixed {
                                    value: source_mv as i32,
                                },
                            },
                        ),
                        cast_transformed: false,
                        cleanup,
                        graveyard_replacement: None,
                        cost: crate::types::ability::ResolutionCastCost::Free,
                    },
                    events,
                )?;
                ResolutionChoiceOutcome::WaitingFor(result)
            } else {
                // CR 702.85a: Caster declines — hit and misses all go to the
                // bottom of the library in a random order together.
                let mut all_to_bottom = exiled_misses;
                all_to_bottom.push(hit_card);
                match crate::game::effects::cascade::shuffle_to_bottom(
                    state,
                    &all_to_bottom,
                    source_id,
                    None,
                    events,
                ) {
                    crate::game::zone_pipeline::BatchMoveResult::Done => {
                        ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(
                            state, player, events,
                        ))
                    }
                    crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => {
                        ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
                    }
                }
            }
        }
        (
            WaitingFor::CastOffer {
                player,
                kind:
                    CastOfferKind::Ripple {
                        hit_card,
                        remaining_hits,
                        revealed_misses,
                        source_id,
                    },
            },
            GameAction::RippleChoice { choice },
        ) => {
            let cast = matches!(choice, crate::types::actions::CastChoice::Cast);
            if cast {
                // CR 702.60a + CR 608.2g: cast the same-named revealed card for
                // free during resolution. No mana-value gate (unlike Cascade); on
                // decline/rollback the hit joins the rest on the library bottom.
                let cleanup = crate::types::ability::ResolutionCastCleanup {
                    source_id,
                    exiled_misses: revealed_misses,
                    reject_action:
                        crate::types::ability::ResolutionMvRejectAction::BottomWithMisses,
                    success_action:
                        crate::types::ability::ResolutionCastSuccessAction::RippleOfferRemaining {
                            remaining_hits,
                        },
                };
                let result = casting::initiate_cast_during_resolution(
                    state,
                    player,
                    hit_card,
                    casting::ResolutionCastRequest {
                        constraint: None,
                        cast_transformed: false,
                        cleanup,
                        graveyard_replacement: None,
                        cost: crate::types::ability::ResolutionCastCost::Free,
                    },
                    events,
                )?;
                ResolutionChoiceOutcome::WaitingFor(result)
            } else {
                // CR 702.60a: declined — the hit and the rest all go to the bottom
                // of the library together.
                let mut all_to_bottom = revealed_misses;
                all_to_bottom.extend(remaining_hits);
                all_to_bottom.push(hit_card);
                match crate::game::effects::cascade::shuffle_to_bottom(
                    state,
                    &all_to_bottom,
                    source_id,
                    Some(
                        crate::types::game_state::BatchCompletion::RippleTerminalComplete {
                            player,
                            source_id,
                            final_cast: None,
                        },
                    ),
                    events,
                ) {
                    crate::game::zone_pipeline::BatchMoveResult::Done => {
                        ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
                    }
                    crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => {
                        ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
                    }
                }
            }
        }
        // CR 608.2g + CR 601.2 + CR 202.3: Invoke Calamity's free-cast window —
        // the controller either picks one candidate to cast for free or declines
        // (`selection: None`) to finish the window. A chosen candidate is cast
        // during resolution via `initiate_cast_during_resolution`; after it
        // resolves, `ResolutionCastSuccessAction::FreeCastOfferRemaining` reduces
        // the budget and re-opens the window. Declining drains the continuation
        // (the "Exile ~" sub-ability).
        (
            WaitingFor::CastOffer {
                player,
                kind:
                    CastOfferKind::FreeCastWindow {
                        candidates,
                        remaining_casts,
                        remaining_mv_budget,
                        filter,
                        zones,
                        graveyard_replacement,
                        source,
                        member_pool,
                    },
            },
            GameAction::FreeCastWindowChoice { selection },
        ) => {
            let Some(chosen) = selection else {
                // CR 601.2: "Up to N" — the controller may stop early. Finish the
                // window and run the continuation (Exile ~).
                return Ok(ResolutionChoiceOutcome::WaitingFor(
                    finish_with_continuation(state, player, events),
                ));
            };
            // CR 608.2c: Validate the choice against the offered candidate set.
            if !candidates.contains(&chosen) {
                return Err(EngineError::InvalidAction(
                    "Selected card is not an eligible free-cast candidate".to_string(),
                ));
            }
            // CR 202.3: Re-check the MV budget at submission so a stale or
            // hand-crafted action cannot exceed the running total.
            if let Some(budget) = remaining_mv_budget {
                let mv = state
                    .objects
                    .get(&chosen)
                    // CR 202.3d + CR 709.4b: the chosen card is off the stack, so
                    // a split card's MV budget is its combined halves — must match
                    // the candidate-eligibility check in free_cast_from_zones.
                    .map(|obj| obj.effective_mana_value())
                    .unwrap_or(0);
                if mv > budget {
                    return Err(EngineError::InvalidAction(
                        "Selected card exceeds the remaining total mana value".to_string(),
                    ));
                }
            }
            // CR 608.2g: Cast the chosen spell during this resolution. The
            // success action re-opens the window with the count decremented and
            // the budget reduced by the spell's resulting mana value; there are
            // no dig misses and a declined finalize-time MV check leaves the card
            // where it is (RemainExiled — never reached here because the
            // per-card MV is pre-checked and these casts carry no resulting-MV
            // permission constraint).
            let cleanup = crate::types::ability::ResolutionCastCleanup {
                source_id: chosen,
                exiled_misses: Vec::new(),
                reject_action: crate::types::ability::ResolutionMvRejectAction::RemainExiled,
                success_action:
                    crate::types::ability::ResolutionCastSuccessAction::FreeCastOfferRemaining {
                        controller: player,
                        remaining_casts,
                        remaining_mv_budget,
                        filter,
                        zones,
                        graveyard_replacement: graveyard_replacement.clone(),
                        source,
                        member_pool,
                    },
            };
            let result = casting::initiate_cast_during_resolution(
                state,
                player,
                chosen,
                casting::ResolutionCastRequest {
                    constraint: None,
                    cast_transformed: false,
                    cleanup,
                    // The window's success action installs this rider exactly
                    // once after the cast finalizes. Passing it through this
                    // request would make `initiate_cast_during_resolution`
                    // install a duplicate synthetic replacement.
                    graveyard_replacement: None,
                    cost: crate::types::ability::ResolutionCastCost::Free,
                },
                events,
            )?;
            // CR 608.2g: when the final free cast is announced, finish the
            // parent spell (including Invoke's self-exile) before priority.
            if matches!(result, WaitingFor::Priority { .. }) {
                ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
            } else {
                ResolutionChoiceOutcome::WaitingFor(result)
            }
        }
        (WaitingFor::LearnChoice { player, hand_cards }, GameAction::LearnDecision { choice }) => {
            match choice {
                LearnOption::Rummage { card_id } => {
                    if !hand_cards.contains(&card_id) {
                        return Err(EngineError::InvalidAction(
                            "Selected card not in hand".to_string(),
                        ));
                    }
                    if let effects::discard::DiscardOutcome::NeedsReplacementChoice(choice_player) =
                        effects::discard::discard_caused_by_effect_with_source(
                            state, card_id, player, None, events,
                        )
                    {
                        let draw = ResolvedAbility::new(
                            crate::types::ability::Effect::Draw {
                                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                                target: crate::types::ability::TargetFilter::Controller,
                            },
                            vec![],
                            ObjectId(0),
                            player,
                        );
                        debug_assert!(
                            state.active_ability_continuation().is_none(),
                            "Learn rummage overwriting active ability continuation"
                        );
                        state.park_ability_continuation(PendingContinuation::new(
                            Box::new(draw),
                            state,
                        ));
                        events.push(GameEvent::EffectResolved {
                            kind: EffectKind::Learn,
                            source_id: ObjectId(0),
                            subject: None,
                        });
                        state.waiting_for = super::replacement::replacement_choice_waiting_for(
                            choice_player,
                            state,
                        );
                        return Ok(action_result_outcome(events, state.waiting_for.clone()));
                    }
                    let draw_ability = ResolvedAbility::new(
                        crate::types::ability::Effect::Draw {
                            count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                            target: crate::types::ability::TargetFilter::Controller,
                        },
                        vec![],
                        ObjectId(0),
                        player,
                    );
                    let _ = effects::resolve_ability_chain(state, &draw_ability, events, 0);
                }
                LearnOption::Skip => {
                    // CR 701.48a: "if you didn't discard a card" — offer the
                    // Lesson search from outside the game.
                    let lesson_search = effects::learn::lesson_search_ability(ObjectId(0), player);
                    let _ = effects::resolve_ability_chain(state, &lesson_search, events, 0);
                    if matches!(state.waiting_for, WaitingFor::OutsideGameChoice { .. }) {
                        events.push(GameEvent::EffectResolved {
                            kind: EffectKind::Learn,
                            source_id: ObjectId(0),
                            subject: None,
                        });
                        return Ok(action_result_outcome(events, state.waiting_for.clone()));
                    }
                }
            }

            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Learn,
                source_id: ObjectId(0),
                subject: None,
            });
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
        }
        (
            WaitingFor::TopOrBottomChoice { player, object_id },
            GameAction::ChooseTopOrBottom { top },
        ) => {
            // CR 614.1 + CR 616.1: The chosen object's Library delivery is
            // an effect-owned zone event, so a `Moved` replacement must settle
            // before its chained resolution tail drains. This legacy waiter does
            // not retain the original ability source; preserve its prior
            // self-anchored attribution for the pipeline request.
            let position = if top {
                LibraryPosition::Top
            } else {
                LibraryPosition::Bottom
            };
            crate::game::zone_pipeline::move_objects_simultaneously_then(
                state,
                vec![crate::game::zone_pipeline::ZoneMoveRequest::effect(
                    object_id,
                    Zone::Library,
                    object_id,
                )
                .at_library_position(position)],
                Some(crate::types::game_state::BatchCompletion::TopOrBottomComplete { player }),
                events,
            );
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        // CR 107.1c + CR 107.14: Commit the chosen amount for a "pay any amount
        // of X" prompt. Deducts the resource, emits the matching resource event,
        // and stamps `last_effect_count` so the next chain step's
        // `QuantityRef::EventContextAmount` resolves to the paid amount.
        (
            WaitingFor::PayAmountChoice {
                player,
                resource,
                min,
                max,
                accumulated,
                source_id,
                pending_mana_ability,
            },
            GameAction::SubmitPayAmount { amount },
        ) => {
            if amount < min || amount > max {
                return Err(EngineError::InvalidAction(format!(
                    "Submitted pay amount {} outside legal range [{}, {}]",
                    amount, min, max
                )));
            }
            if let Some(pending_mana_ability) = pending_mana_ability {
                let mut pending = pending_mana_ability.as_ref().clone();
                // CR 107.1c / CR 107.3a: bind the announced amount to the
                // correct mana-ability cost axis — a removed-counter count
                // (CR 122.1) or an announced X for `Pay X speed` (CR 702.179e).
                match resource {
                    PayableResource::Counters => pending.chosen_counter_count = Some(amount),
                    PayableResource::Speed => pending.chosen_x = Some(amount),
                    other => {
                        return Err(EngineError::InvalidAction(format!(
                            "Unexpected pay-amount resource {other:?} for mana ability"
                        )))
                    }
                }
                let waiting_for =
                    mana_abilities::advance_mana_ability_activation(state, pending, events)?;
                return Ok(ResolutionChoiceOutcome::WaitingFor(waiting_for));
            }
            match resource {
                PayableResource::LoopCollapse { .. } => {
                    // CR 732.2a: an accepted unbounded loop shortcut deferred its finite
                    // count to this phase/step boundary; the controller has now named N.
                    // Apply each stashed persistent-axis materialization, cash out the
                    // collapsed ∞ axes, and re-enter the boundary drain so Priority is
                    // restored in this same action.
                    //
                    // This arm deducts NO resource and MUST early-return: the shared
                    // pay-then-continue tail below is for chained real-resource payments,
                    // not a boundary collapse. Mirrors the `pending_mana_ability`
                    // early-return above. `take_` removes the whole stash even on the
                    // error path so the boundary fixpoint terminates rather than
                    // re-prompting forever.
                    let Some(mut items) = state.take_pending_materialization(player) else {
                        return Err(EngineError::InvalidAction(format!(
                            "LoopCollapse pay-amount for {player:?} has no pending materialization stash"
                        )));
                    };
                    // CR 732.2a pause-safety: process the ONLY pause-prone axis (`Tokens` — its
                    // per-cycle fodder mint can raise an ETB replacement `NeedsChoice`, unlike
                    // the deterministic Counters/Life/DriveSequence axes) LAST. A mixed loop
                    // stashes multiple axes at once (Guide of Souls + Sprout Swarm =
                    // tokens+life; Witherbloom + Sprout Swarm = tokens+counters). Committing the
                    // deterministic axes first means a `Tokens` pause leaves the finite
                    // non-token effects applied exactly once and only the paused `Tokens` axis
                    // ∞ — order-independent of how the stash was registered. Stable: relative
                    // order of the non-Tokens axes is preserved.
                    items.sort_by_key(|i| matches!(i, PersistentAxisMaterialization::Tokens(_)));
                    // FINDING #4 (accept→boundary observer-drift): the observed-growth
                    // firewall ran at ACCEPT, but the controller held priority between
                    // accept and this boundary and could have cast an observer (Heliod /
                    // Corpsejack) of the growing class. The batched δ single-application is
                    // sound only while the class stays UNOBSERVED — `apply_life_gain` fires
                    // a life observer ONCE not N×, and `apply_counter_addition` bypasses the
                    // doubler pipeline. Re-check NOW (the firewall scans the board/stack, not
                    // the stash, so it needs no sequence): if an observer appeared, DECLINE
                    // the batched `Counters`/`Life` apply and leave those ∞ axes marked for
                    // manual play — unambiguously sound (never a wrong count). `Tokens` (N real
                    // ETB events) and `DriveSequence` (N-cycle real replay) honor observers
                    // regardless and always proceed. Only the ACTUALLY-applied items are
                    // cleared, so a declined axis stays ∞.
                    // AXIS-SPECIFIC re-check: an observer of the counter class must not veto a
                    // batched LIFE gain and vice-versa. Each axis re-runs its own firewall —
                    // which is why `ObservedGrowth` carries both answers separately.
                    // CR 732.1a/1b: this re-check is the shortcut system closing the elided loop
                    // where the table understood it would close. The rules frame lives on
                    // `ObservedGrowth::at_boundary` and `BoundaryHold::ObservedGrowth`; do not
                    // re-derive one here.
                    let observed = ObservedGrowth::at_boundary(state);
                    let mut collapsed: Vec<PersistentAxisMaterialization> = Vec::new();
                    for item in &items {
                        // The ONE decline decision — `BoundaryHold::ObservedGrowth` (see its doc
                        // for the CR 732.1a/1b frame).
                        if boundary_declines(item, observed) {
                            continue;
                        }
                        match item {
                            PersistentAxisMaterialization::Tokens(profile) => {
                                // CR 707.2 (+ CR 111.10): mint N tapped copy-tokens of the
                                // fodder profile — a source-less mint, so route through
                                // `drive_copy_token_batches` (`ObjectId(0)` sentinel source).
                                //
                                // CR 732.2a k≡1 INVARIANT: this mints `count: amount` == k·amount
                                // with the per-cycle fodder count k STRUCTURALLY ≡ 1. A `Tokens`
                                // stash is only registered under `if let Some(profile)` in
                                // `materialize_object_growth_shortcut` (engine.rs), whose
                                // `current_period_fodder` derives the profile from
                                // `derived_fodder_class` (`game/engine.rs`), which returns `None`
                                // unless EXACTLY one new battlefield object appeared per period
                                // (`let id = new_ids.next()?; if new_ids.next().is_some() { None }`).
                                // A k>1 period (two+ new objects/cycle) fails that gate ⇒ no `Tokens`
                                // stash ⇒ this arm is never reached for k≠1. So `count: amount` is
                                // EXACT, not a k·N undercount. (Counters/Life instead carry a measured
                                // `per_cycle_delta`, so those axes handle k>1 by construction.)
                                let batch = crate::types::game_state::PendingCopyTokenBatch {
                                    owner: player,
                                    count: amount,
                                    copy: Box::new(crate::types::proposed_event::CopyTokenSpec {
                                        values: profile.clone(),
                                        display_source:
                                            crate::game::game_object::DisplaySource::Token,
                                        printed_ref: None,
                                        token_image_ref: None,
                                        extra_keywords: vec![],
                                        additional_modifications: vec![],
                                        tapped: true,
                                        enters_attacking: false,
                                        sacrifice_at: None,
                                        source_id: ObjectId(0),
                                        controller: player,
                                    }),
                                };
                                crate::game::effects::token_copy::drive_copy_token_batches(
                                    state,
                                    std::collections::VecDeque::from([batch]),
                                    EffectKind::CopyTokenOf,
                                    ObjectId(0),
                                    events,
                                );
                                // DEFENSE-IN-DEPTH AT ACCEPT ONLY. The offer firewall — the
                                // exhaustive fail-closed `_ => Err(RecastAbort)` in
                                // `drive_loop_action_iteration` (cited by symbol; it has no
                                // replacement-/target-choice arm) — runs at CERTIFICATION. It
                                // cannot bind this mint, which happens later: a replacement
                                // effect installed AFTER the accept reaches this pause, and
                                // `med_tokens_boundary_mint_pause_preserves_replacement_choice`
                                // drives exactly that. Both ingresses come out of `token_copy.rs`'s
                                // `replace_event` match: its `NeedsChoice` arm, and its `Execute`
                                // arm's `apply_create_token_after_replacement == false` return.
                                //
                                // CR 614.1 + CR 614.1a: an "instead" replacement is a replacement
                                // effect, and replacement effects "apply continuously as events
                                // happen—they aren't locked in ahead of time", so one installed
                                // after accept still watches this mint event. CR 616.1 covers only
                                // the ≥2-candidate ordering ingress ("if two or more replacement
                                // and/or prevention effects are attempting to modify …").
                                //
                                // CR 732.2c is named here ONLY as the rule this window DEVIATES
                                // FROM: under it the growth would already have been applied at
                                // accept and there would be no boundary mint to pause. See
                                // `BoundaryHold::CopyTokenPause`.
                                //
                                // So: DO NOT advance the phase / mark the axis collapsed /
                                // overwrite the replacement `waiting_for`: preserve the paused
                                // copy-resolution and hand the replacement choice back (game totals
                                // stay correct because the ∞ marks are not cleared). No
                                // `debug_assert!` — the defensive test deliberately drives this
                                // pause, which a debug_assert would panic.
                                // BoundaryHold::CopyTokenPause
                                if state.active_copy_token().is_some() {
                                    // CR 732.2a pause-safe transaction: cash out the axes already
                                    // applied THIS pass (a mixed stash, Edit 1 puts Tokens last) so
                                    // no finite-applied Counters/Life axis is left with a stale ∞
                                    // mark. The still-paused Tokens axis is NOT in `collapsed`, so
                                    // its ∞ axis/pile is preserved for manual play (the loop has
                                    // not closed yet, so the capability still stands; see
                                    // `BoundaryHold::CopyTokenPause`). Do NOT drain the phase — the
                                    // mint is mid-flight.
                                    state.clear_collapsed_materializations(player, &collapsed);
                                    return Ok(ResolutionChoiceOutcome::WaitingFor(
                                        state.waiting_for.clone(),
                                    ));
                                }
                            }
                            PersistentAxisMaterialization::Counters(growths) => {
                                for g in growths {
                                    // CR 400.7: a permanent that left the battlefield
                                    // accept→boundary is skipped (its object id is stale).
                                    if !state.battlefield.contains(&g.object) {
                                        continue;
                                    }
                                    // CR 122.1 single counter authority; the direct `+=` (no
                                    // doubler re-run) is EXACT N×δ because the firewall
                                    // rejected any counter-placement replacement at accept.
                                    crate::game::effects::counters::apply_counter_addition(
                                        state,
                                        player,
                                        g.object,
                                        g.counter.clone(),
                                        g.per_cycle_delta.saturating_mul(amount),
                                        events,
                                    );
                                }
                            }
                            PersistentAxisMaterialization::Life {
                                player: p,
                                per_cycle_delta,
                            } => {
                                // CR 119.3 single life authority; SOUND only because the
                                // firewall rejected any life-gain replacement/observer
                                // (`apply_life_gain` re-runs the replacement pipeline, so a
                                // lump gain would fire an observer ONCE not N×).
                                let _ = crate::game::effects::life::apply_life_gain(
                                    state,
                                    *p,
                                    per_cycle_delta.saturating_mul(amount),
                                    events,
                                );
                            }
                            PersistentAxisMaterialization::DriveSequence {
                                sequence,
                                collapsed_axes: _,
                            } => {
                                // CR 732.2a: replay N real cycles; observers fire each cycle;
                                // no re-offer (the drive holds the simulation guard).
                                crate::game::engine::drive_persistent_axis_collapse(
                                    state, sequence, amount,
                                );
                            }
                        }
                        // The SINGLE push. Reaching here means the growth applied; every other
                        // outcome is a labelled `BoundaryHold` above. `possible_hold` is exactly
                        // the set of arms that can skip this line. "Single" is asserted, not just
                        // asked for: `boundary_apply_loop_region` panics on a second push, because
                        // the exit census reads the text between the sort and the FIRST push.
                        collapsed.push(item.clone());
                    }
                    // CR 732.2a: cash out ONLY the axes actually collapsed (axis-scoped) —
                    // end their ∞ status + stash + pile, PRESERVING any coexisting axis (a
                    // debug infinite-mana capability, or a finding-#4-declined axis). The ∞
                    // display collapses to an ordinary ×N for the collapsed axes (§9).
                    //
                    // FINDING #4 DECLINED-AXIS ∞ LIFECYCLE (CR 732.1b — the shortcut system
                    // determines how the loop is broken; see BoundaryHold::ObservedGrowth): a declined `Counters`/`Life`
                    // axis (`continue`d above without `collapsed.push`) is absent from `collapsed`,
                    // so `clear_collapsed_materializations` — which iterates ONLY `collapsed`
                    // (game_state.rs) — never removes its `unbounded_resources` /
                    // `unbounded_counter_targets` entry. That ∞ mark is an INTENTIONAL capability
                    // marker that PERSISTS (the loop machinery still exists, so the capability is
                    // real), with game totals correct (the declined axis was not applied — no double
                    // count). It is retired by exactly TWO LIVE paths:
                    //   (a) a later GENUINE re-detection re-collapsing it — the empty-stack offer
                    //       hook `try_offer_object_growth_shortcut` (in `game/engine.rs`), which is
                    //       NOT ∞-gated, so a fresh manual re-loop re-offers and re-registers a
                    //       stash; and
                    //   (b) debug toggle-off — `engine_debug.rs`'s `clear_unbounded_loop` call.
                    // NOTE: the enabler-departure clear (`clear_unbounded_loop` from
                    // `zones::apply_zone_exit_cleanup`) is INERT for this object-growth ∞-mark
                    // class, because `materialize_object_growth_shortcut` (engine.rs) never calls
                    // `register_unbounded_loop_enablers` (only the Interactive Path-C arm does), so
                    // `zones.rs`'s `unbounded_loop_enablers.contains(id)` gate never matches an
                    // object-growth mark. It STAYS inert deliberately: `clear_unbounded_loop` drops
                    // SIX maps including `pending_unbounded_materialization`, so registering
                    // enablers here would let one departing token cancel the collapse the table
                    // unanimously accepted (CR 732.2c: the shortcut is taken at the last accept).
                    // The DISPLAY half of follow-up F2 is instead covered live at the projection by
                    // `derived_views::object_growth_backing`, which drops an ∞ row whose entire
                    // registered display set has left the battlefield without touching the stash.
                    // That cover now spans BOTH object-backed families — the token axis reads the
                    // ∞ pile, and the counter axes read the registered `(object, counter)` pairs
                    // that derive each axis — and it applies ONLY while the collapse is still
                    // UNACCEPTED. Once a stash exists for the axis, CR 732.2c has already taken the
                    // shortcut, so the projection's acceptance gate keeps the row even with its
                    // whole backing gone: the growth still lands here, and a row that vanished
                    // before it landed would be the display lying about an agreed result.
                    state.clear_collapsed_materializations(player, &collapsed);
                    // Continue the boundary fixpoint (§7): re-draining either prompts the
                    // next APNAP player with a stash or restores Priority now.
                    crate::game::turns::drain_pending_phase_transition_progress(state, events);
                    return Ok(ResolutionChoiceOutcome::WaitingFor(
                        state.waiting_for.clone(),
                    ));
                }
                PayableResource::Energy => {
                    // CR 107.14: Remove N energy counters from the player.
                    if let Some(energy) = state
                        .players
                        .iter()
                        .find(|candidate| candidate.id == player)
                        .map(|candidate| candidate.energy)
                    {
                        if energy < amount {
                            return Err(EngineError::InvalidAction(format!(
                                "Player {:?} has {} energy, cannot pay {}",
                                player, energy, amount
                            )));
                        }
                        if amount > 0 {
                            state
                                .resolve_and_apply_player_edit(
                                    player,
                                    crate::types::resolved_commands::ResolvedPlayerEdit::Energy {
                                        delta: -(amount as i32),
                                    },
                                )
                                .expect("preflighted resolution energy payment must apply");
                        }
                        events.push(GameEvent::EnergyChanged {
                            player,
                            delta: -(amount as i32),
                        });
                    }
                }
                PayableResource::ManaGeneric { base_cost } => {
                    // CR 107.3f + CR 118.1 + CR 118.12: concretize the chosen
                    // X into the ORIGINAL cost — any colored/generic pips
                    // alongside the X shard (e.g. Elenda and Azor's
                    // `{X}{W}{U}{B}`) survive concretization and are paid
                    // here too. Paying a synthetic all-generic `{N}` cost
                    // instead would silently drop the colored requirements
                    // (#6410).
                    let mut cost = base_cost.clone();
                    cost.concretize_x(amount);
                    if !casting::can_pay_effect_mana_cost_after_auto_tap(
                        state, player, source_id, &cost,
                    ) {
                        return Err(EngineError::InvalidAction(format!(
                            "Player {:?} cannot pay {}",
                            player,
                            cost.mana_value()
                        )));
                    }
                    let _ = casting::pay_unless_cost(state, player, &cost, events);
                }
                PayableResource::Counters => {
                    return Err(EngineError::InvalidAction(
                        "Counter amount choices require a pending mana ability".to_string(),
                    ));
                }
                PayableResource::Speed => {
                    // CR 702.179e: `Pay X speed` only ever arises as a mana-ability
                    // activation cost, which carries a `pending_mana_ability` and
                    // is handled above. Reaching the standalone branch is a bug.
                    return Err(EngineError::InvalidAction(
                        "Speed amount choices require a pending mana ability".to_string(),
                    ));
                }
                PayableResource::Life => {
                    // CR 119.4: pay N life via the life-loss-as-cost authority
                    // (replacement pipeline + CantLoseLife) — NOT inline life
                    // subtraction.
                    let resume_at_resolution_depth = state.resolution_stack.len();
                    match crate::game::life_costs::pay_life_as_cost(state, player, amount, events) {
                        crate::game::life_costs::PayLifeCostResult::Paid { .. } => {}
                        crate::game::life_costs::PayLifeCostResult::PaidWithDeferredSubstitution {
                            ..
                        }
                        | crate::game::life_costs::PayLifeCostResult::DeferredReplacementChoice {
                            ..
                        } => {
                            state.pending_deferred_life_cost_resume = Some(
                                crate::types::game_state::DeferredLifeCostResume::PayAmount {
                                    player,
                                    total: accumulated.saturating_add(amount),
                                    resume_at_resolution_depth,
                                },
                            );
                            return Ok(ResolutionChoiceOutcome::WaitingFor(
                                state.waiting_for.clone(),
                            ));
                        }
                        crate::game::life_costs::PayLifeCostResult::InsufficientLife
                        | crate::game::life_costs::PayLifeCostResult::Prohibited => {
                            return Err(EngineError::InvalidAction(format!(
                                "Player {player:?} cannot pay {amount} life"
                            )))
                        }
                    }
                }
            }
            // CR 603.7c: Bind the paid amount for downstream chain steps that
            // read `QuantityRef::EventContextAmount` (e.g. "deals that much
            // damage"). `last_effect_count` is the documented fallback slot.
            let total = accumulated.saturating_add(amount);
            let waiting_for = finish_pay_amount_choice(state, player, total, events);
            ResolutionChoiceOutcome::WaitingFor(waiting_for)
        }
        (
            WaitingFor::PopulateChoice {
                player,
                valid_tokens,
                source_id,
            },
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(token_id)),
            },
        ) => {
            if !valid_tokens.contains(&token_id) {
                return Err(EngineError::ActionNotAllowed(
                    "Selected token not in valid populate choices".into(),
                ));
            }
            let dummy_ability = ResolvedAbility::new(
                crate::types::ability::Effect::Populate,
                vec![],
                source_id,
                player,
            );
            let _ = effects::populate::create_token_copy(state, token_id, &dummy_ability, events);
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
        }
        (
            WaitingFor::BeholdChoice { player, choices },
            GameAction::SelectCards { cards: chosen },
        ) => {
            // CR 701.4a + CR 608.2d: behold selects exactly ONE object from the
            // mixed-zone candidate set (battlefield-you-control ∪ hand).
            if chosen.len() != 1 {
                return Err(EngineError::InvalidAction(format!(
                    "Behold requires exactly one object, got {}",
                    chosen.len()
                )));
            }
            let chosen_id = chosen[0];
            if !choices.contains(&chosen_id) {
                return Err(EngineError::InvalidAction(
                    "Selected object is not a beholdable candidate".to_string(),
                ));
            }
            // CR 701.4a: reveal the beheld card only if it is a hand card (a
            // controlled battlefield permanent is already public). The non-chosen
            // candidates are never revealed — they stay hidden.
            effects::behold::reveal_if_from_hand(state, player, chosen_id, events);
            // CR 608.2c: the behold was performed → the "if you do, [rider]" gate
            // fires. On the optional Sarkhan path this re-affirms the accept-time
            // clobber (`resolve_optional_effect_decision`); for a mandatory
            // behold-class card it is the sole hook that fires the rider.
            if let Some(frame) = state.active_ability_continuation_frame_mut() {
                frame
                    .pending
                    .chain
                    .set_optional_effect_performed_recursive(true);
            }
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
        }
        (
            WaitingFor::ClashChooseOpponent {
                player,
                candidates,
                ability,
            },
            GameAction::ChooseClashOpponent { opponent },
        ) => {
            // CR 701.30b: The chosen opponent must be one of the offered
            // candidates (a non-eliminated opponent of the clashing player).
            if !candidates.contains(&opponent) {
                return Err(EngineError::InvalidAction(format!(
                    "Chosen clash opponent {opponent:?} is not a legal opponent"
                )));
            }
            effects::clash::perform_clash(state, &ability, opponent, events)
                .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            // CR 701.30a: With at least one revealed card, `perform_clash` queued
            // the APNAP placement (which drains the clash's sub_ability). With
            // both libraries empty, no placement was queued, so drain the stashed
            // sub_ability here and hand priority back to the clashing player.
            if !matches!(state.waiting_for, WaitingFor::ClashCardPlacement { .. }) {
                set_priority(state, player);
                super::engine::resume_pending_continuation_if_priority(state, events)
                    .expect("a settled clash choice must resume its continuation");
            }
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        (
            WaitingFor::ChooseFromZoneOpponentChooser {
                player,
                candidates,
                ability,
            },
            GameAction::ChooseZoneOpponentChooser { opponent },
        ) => {
            // CR 608.2d: The picked opponent must be one of the offered
            // candidates (a live opponent of the choose's controller).
            if !candidates.contains(&opponent) {
                return Err(EngineError::InvalidAction(format!(
                    "Chosen zone-choice opponent {opponent:?} is not a legal opponent"
                )));
            }
            // CR 608.2d: Present the parked zone selection to the picked
            // opponent. This re-enters the standard `ChooseFromZoneChoice`
            // pause (or completes with no choice if the pool emptied), so the
            // already-parked continuation frame is untouched — exactly as if
            // the opponent had been the chooser from the start.
            effects::choose_from_zone::resolve_with_choosing_player(
                state, &ability, opponent, events,
            )
            .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            // With an empty pool the choose completed without a new pause —
            // hand priority back to the controller (mirroring the settled-clash
            // arm above) so the parked continuation can actually drain: the
            // drain helper is gated on `WaitingFor::Priority`, and without
            // `set_priority` the stale opponent-chooser pause would wedge the
            // resolution (CR 608.2d — the choice is skipped, not the rest of
            // the ability).
            if !matches!(state.waiting_for, WaitingFor::ChooseFromZoneChoice { .. }) {
                set_priority(state, player);
                super::engine::resume_pending_continuation_if_priority(state, events)
                    .expect("a settled opponent-chooser pick must resume its continuation");
            }
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        (
            WaitingFor::ClashCardPlacement {
                player,
                card,
                remaining,
            },
            GameAction::ChooseTopOrBottom { top },
        ) => {
            // allow-raw-zone: clash reveal/return keeps the card in its library (CR 701.30a + CR 701.20b).
            zones::move_to_library_position(state, card, top, events);
            if let Some(((next_player, next_card), rest)) = remaining.split_first() {
                state.waiting_for = WaitingFor::ClashCardPlacement {
                    player: *next_player,
                    card: *next_card,
                    remaining: rest.to_vec(),
                };
                ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
            } else {
                ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
            }
        }
        // CR 701.38: Tally a vote, then either advance to the same voter's
        // next vote (CR 701.38d), the next voter (CR 101.4), or — if every
        // voter has voted — fan out the per-choice sub-effects via
        // `vote::resolve_tally` and drain the post-vote continuation.
        (
            WaitingFor::VoteChoice {
                player,
                remaining_votes,
                options,
                option_labels,
                remaining_voters,
                tallies,
                ballots,
                per_choice_effect,
                controller,
                source_id,
                actor,
                tally_mode,
                candidate_objects,
                outcome_template,
                visibility,
            },
            GameAction::ChooseOption { choice },
        ) => {
            // CR 701.38a: Validate the cast vote. Named votes match the choice
            // word against the canonical options list. Object votes (where
            // `candidate_objects` is non-empty) reject the string path — their
            // candidates are not canonical option words and same-named
            // candidates can't be disambiguated by string; those go through
            // `SubmitVoteCandidate`.
            if !candidate_objects.is_empty() {
                return Err(EngineError::InvalidAction(
                    "Object-pool votes require SubmitVoteCandidate, not ChooseOption".into(),
                ));
            }
            let lower = choice.to_lowercase();
            let Some(idx) = options.iter().position(|o| o == &lower) else {
                return Err(EngineError::InvalidAction(format!(
                    "Invalid vote '{}'; valid choices are {:?}",
                    choice, options
                )));
            };
            // CR 608.2c + CR 701.38: Named vote options are a small bounded set
            // (parse_vote_block yields at most a few choices per Oracle text).
            append_vote_ballot_and_advance(
                state,
                events,
                idx as u32,
                VoteRoundState {
                    player,
                    remaining_votes,
                    options,
                    option_labels,
                    remaining_voters,
                    tallies,
                    ballots,
                    per_choice_effect,
                    controller,
                    source_id,
                    actor,
                    tally_mode,
                    candidate_objects,
                    outcome_template,
                    visibility,
                },
            )
        }
        // CR 701.38b: Object-pool vote ballot — the player picks one candidate
        // object by index. Index-based submission disambiguates same-named
        // candidates that `ChooseOption`'s string match cannot. Named votes
        // (empty `candidate_objects`) reject this path.
        (
            WaitingFor::VoteChoice {
                player,
                remaining_votes,
                options,
                option_labels,
                remaining_voters,
                tallies,
                ballots,
                per_choice_effect,
                controller,
                source_id,
                actor,
                tally_mode,
                candidate_objects,
                outcome_template,
                visibility,
            },
            GameAction::SubmitVoteCandidate { candidate_index },
        ) => {
            if (candidate_index as usize) >= candidate_objects.len() {
                return Err(EngineError::InvalidAction(format!(
                    "Invalid vote candidate index {}; {} candidates available",
                    candidate_index,
                    candidate_objects.len()
                )));
            }
            append_vote_ballot_and_advance(
                state,
                events,
                candidate_index,
                VoteRoundState {
                    player,
                    remaining_votes,
                    options,
                    option_labels,
                    remaining_voters,
                    tallies,
                    ballots,
                    per_choice_effect,
                    controller,
                    source_id,
                    actor,
                    tally_mode,
                    candidate_objects,
                    outcome_template,
                    visibility,
                },
            )
        }
        // CR 608.2d + CR 700.3: Controller chose which opponent performs the
        // partition. Validate the choice and transition to SeparatePilesPartition.
        (
            WaitingFor::SeparatePilesChooseOpponent {
                player: _,
                candidates,
                eligible,
                chooser,
                chosen_pile_effect,
                unchosen_pile_effect,
                source_id,
                pile_source,
            },
            GameAction::ChoosePileOpponent { opponent },
        ) => {
            if !candidates.contains(&opponent) {
                return Err(EngineError::InvalidAction(format!(
                    "Chosen pile opponent {opponent:?} is not a legal opponent"
                )));
            }
            state.waiting_for = WaitingFor::SeparatePilesPartition {
                player: opponent,
                eligible,
                remaining_subjects: crate::im::Vector::new(),
                completed: crate::im::Vector::new(),
                chooser,
                chosen_pile_effect,
                unchosen_pile_effect,
                source_id,
                pile_source,
            };
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        // CR 700.3 + CR 700.3a + CR 101.4: Subject submits their partition;
        // pile B is derived as `eligible \ pile_a`. Advance the subject queue
        // (CR 800.4g — eliminated players were filtered out at resolver
        // entry; if the next subject has been eliminated since, the
        // `apnap_order_from` pass at resolution time guarantees they were
        // never queued). When the queue empties, transition to the choice
        // phase.
        (
            WaitingFor::SeparatePilesPartition {
                player,
                eligible,
                mut remaining_subjects,
                mut completed,
                chooser,
                chosen_pile_effect,
                unchosen_pile_effect,
                source_id,
                pile_source,
            },
            GameAction::SubmitPilePartition { pile_a },
        ) => {
            // CR 700.3a: Validate the partition is a subset of `eligible`
            // (no duplicates, no foreign ids). Empty `pile_a` is legal per
            // CR 700.3d.
            use std::collections::HashSet;
            let eligible_set: HashSet<ObjectId> = eligible.iter().copied().collect();
            let mut seen: HashSet<ObjectId> = HashSet::with_capacity(pile_a.len());
            for id in &pile_a {
                if !eligible_set.contains(id) {
                    return Err(EngineError::InvalidAction(format!(
                        "pile A contains object {id:?} not in eligible set"
                    )));
                }
                if !seen.insert(*id) {
                    return Err(EngineError::InvalidAction(format!(
                        "pile A contains duplicate object {id:?}"
                    )));
                }
            }
            let pile_a_vec: crate::im::Vector<ObjectId> = pile_a.iter().copied().collect();
            let pile_b_vec: crate::im::Vector<ObjectId> = eligible
                .iter()
                .copied()
                .filter(|id| !seen.contains(id))
                .collect();
            completed.push_back(crate::types::game_state::PileResult {
                subject: player,
                pile_a: pile_a_vec,
                pile_b: pile_b_vec,
            });
            if let Some((next_pid, next_pool)) = remaining_subjects.pop_front() {
                state.waiting_for = WaitingFor::SeparatePilesPartition {
                    player: next_pid,
                    eligible: next_pool,
                    remaining_subjects,
                    completed,
                    chooser,
                    chosen_pile_effect,
                    unchosen_pile_effect: unchosen_pile_effect.clone(),
                    source_id,
                    pile_source,
                };
                ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
            } else {
                // All subjects partitioned. Transition to chooser phase.
                let (current, pending) = pop_first_pile_result(completed);
                state.waiting_for = WaitingFor::SeparatePilesChoice {
                    player: chooser,
                    pending,
                    current,
                    chosen_pile_effect,
                    unchosen_pile_effect,
                    source_id,
                    pile_source,
                };
                ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
            }
        }
        // CR 700.3 + CR 101.4c: Chooser picks pile A or B for the current
        // subject. The chooser may resolve multiple subjects "in any order"
        // (CR 101.4c) — the engine drains `pending` in completion order and
        // each `ChoosePile` advances one step. When `pending` empties, the
        // sub-effect (sacrifice for Make an Example) fans out over every
        // chosen pile, scoped per subject as controller.
        (
            WaitingFor::SeparatePilesChoice {
                player,
                mut pending,
                current,
                chosen_pile_effect,
                unchosen_pile_effect,
                source_id,
                pile_source,
            },
            GameAction::ChoosePile { pile },
        ) => {
            // CR 101.4c: Resolve this subject's chosen pile NOW (one
            // `Sacrifice` per object), then either park for the next
            // subject's choice or finish. Per-decision resolution matches
            // CR 101.4c ("in any order they choose") — the chooser's
            // submission order IS that order.
            effects::separate_piles::apply_pile_effect(
                state,
                source_id,
                &chosen_pile_effect,
                &[(current.clone(), pile)],
                events,
            )
            .map_err(|e| EngineError::InvalidAction(format!("pile sub-effect: {e:?}")))?;
            // CR 608.2c: Apply unchosen pile sub-effect if present.
            if let Some(ref unchosen_def) = unchosen_pile_effect {
                effects::separate_piles::apply_unchosen_pile_effect(
                    state,
                    source_id,
                    unchosen_def,
                    &[(current, pile)],
                    events,
                )
                .map_err(|e| {
                    EngineError::InvalidAction(format!("unchosen pile sub-effect: {e:?}"))
                })?;
            }
            if let Some(next) = pending.pop_front() {
                state.waiting_for = WaitingFor::SeparatePilesChoice {
                    player,
                    pending,
                    current: next,
                    chosen_pile_effect,
                    unchosen_pile_effect,
                    source_id,
                    pile_source,
                };
                ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
            } else {
                ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
            }
        }
        (
            WaitingFor::DigChoice {
                player,
                library_owner,
                cards,
                keep_count,
                up_to,
                selectable_cards,
                kept_destination,
                rest_destination,
                rest_order,
                enter_tapped,
                enters_attacking,
                source_id: dig_source_id,
                ..
            },
            GameAction::SelectCards { cards: kept },
        ) => {
            if up_to {
                if kept.len() > keep_count {
                    return Err(EngineError::InvalidAction(format!(
                        "Must select at most {} cards, got {}",
                        keep_count,
                        kept.len()
                    )));
                }
            } else {
                // CR 609.3 + CR 101.3: a dig whose filter (or a short library)
                // leaves fewer selectable cards than `keep_count` must keep as
                // many as possible, not reject every selection. Without the
                // clamp no legal action exists in that state —
                // `validate_dig_selection` below requires every kept id to be in
                // `selectable_cards` while this gate demands more ids than it
                // holds — softlocking every controller. Matches the clamp the
                // candidate enumerator (`ai_support/candidates.rs`'s
                // `WaitingFor::DigChoice` arm) and `cheap_reject_candidate`'s own
                // `WaitingFor::DigChoice` arm already apply.
                let required = keep_count.min(selectable_cards.len());
                if kept.len() != required {
                    return Err(EngineError::InvalidAction(format!(
                        "Must select exactly {} cards, got {}",
                        required,
                        kept.len()
                    )));
                }
            }

            // CR 401.2 + CR 608.2c: the keep-selection must be unique, drawn from
            // the cards actually looked at, and (when the dig has a filter) from
            // the filter-matching subset. The previous check skipped filter/look-
            // at validation entirely whenever `selectable_cards` was empty, so a
            // filtered dig that matched nothing accepted arbitrary object ids.
            validate_dig_selection(&kept, &cards, &selectable_cards)?;

            let mut unkept: Vec<_> = cards
                .iter()
                .filter(|id| !kept.contains(id))
                .copied()
                .collect();
            if kept_destination == Some(Zone::Library) {
                let move_unkept_to = {
                    let player_state = state
                        .players
                        .iter_mut()
                        .find(|candidate| candidate.id == library_owner)
                        .expect("player exists");
                    // allow-raw-zone: looked-at cards remain library objects until a keep decision (CR 701.20b/e).
                    player_state.library.retain(|id| !cards.contains(id));
                    for (index, &card_id) in kept.iter().enumerate() {
                        // allow-raw-zone: looked-at cards remain library objects until a keep decision (CR 701.20b/e).
                        player_state.library.insert(index, card_id);
                    }
                    match rest_destination {
                        Some(Zone::Library) => {
                            if rest_order == DigRestOrder::Random {
                                // CR 400.5 + CR 608.2c: Randomize exactly the
                                // unchosen pile immediately before bottom placement.
                                unkept.shuffle(&mut state.rng);
                            }
                            for &obj_id in &unkept {
                                // allow-raw-zone: looked-at cards remain library objects until a keep decision (CR 701.20b/e).
                                player_state.library.push_back(obj_id);
                            }
                            None
                        }
                        Some(zone) => Some(zone),
                        None => Some(Zone::Graveyard),
                    }
                };
                // CR 701.20d: This direct library reorder has the same
                // information boundary as the shared reorder helper. Advance
                // product knowledge before any unkept cards leave the library.
                state.advance_library_knowledge_epoch(library_owner);
                // CR 401.5 + CR 611.3a: Dig kept cards on top by editing the
                // library directly, so a `TopOfLibraryMatches` static must be
                // re-evaluated (self-gated on liveness).
                crate::game::layers::mark_layers_full_if_top_of_library_static_live(state);
                if let Some(zone) = move_unkept_to {
                    // CR 614.6 + CR 603.10a: route the unkept pile through the
                    // zone-change pipeline so a per-card `Moved` graveyard→exile
                    // redirect (Rest in Peace / Leyline of the Void) fires on each
                    // — the raw `move_to_zone` never proposed the inner ZoneChange,
                    // silently dropping those redirects for dig's "the rest into
                    // your graveyard" class. `zone` here is never Library (the
                    // Library case pushed back above and yielded `None`), so the
                    // batch always has a `Moved`-redirect-eligible destination.
                    // CR 400.7: each unkept card anchors its own attribution.
                    //
                    // On a mid-pile CR 616.1 ordering pause, defer the
                    // priority/continuation drain (a cleanup-only `RevealRestPile`
                    // completion: empty pile, no markers/publish, just
                    // `finish_with_continuation`) so it runs once the pile lands,
                    // and surface the parked prompt instead of draining over it.
                    let reqs: Vec<_> = unkept
                        .iter()
                        .map(|&obj_id| {
                            crate::game::zone_pipeline::ZoneMoveRequest::effect(
                                obj_id, zone, obj_id,
                            )
                        })
                        .collect();
                    match crate::game::zone_pipeline::move_objects_simultaneously(
                        state, reqs, events,
                    ) {
                        crate::game::zone_pipeline::BatchMoveResult::Done => {}
                        crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => {
                            crate::game::zone_pipeline::defer_completion_on_pause(
                                state,
                                crate::types::game_state::BatchCompletion::RevealRestPile {
                                    player,
                                    source_id: dig_source_id,
                                    rest_cards: Vec::new(),
                                    rest_destination: zone,
                                    rest_order: DigRestOrder::Preserve,
                                    clear_markers: Vec::new(),
                                    publish_tracked_set: None,
                                    emit_reveal_until_resolved: None,
                                    manifested_for_continuation: None,
                                },
                            );
                            return Ok(ResolutionChoiceOutcome::WaitingFor(
                                state.waiting_for.clone(),
                            ));
                        }
                    }
                }
                return Ok(ResolutionChoiceOutcome::WaitingFor(
                    finish_with_continuation(state, player, events),
                ));
            }
            if let Some(kept_zone) = kept_destination {
                if kept_zone != Zone::Battlefield {
                    // CR 614.1 + CR 616.1 + CR 608.2c: Kept cards leaving the
                    // library are effect-owned zone events. Their destination
                    // delivery, rest-pile routing, tracked-set publication, and
                    // continuation drain are one typed batch tail so none can
                    // run before every kept card has settled.
                    let publish_set = if kept.is_empty() {
                        Vec::new()
                    } else if state.active_ability_continuation().is_some_and(|cont| {
                        dig_continuation_needs_full_looked_at_tracked_set(&cont.chain)
                    }) {
                        unkept.clone()
                    } else {
                        kept.clone()
                    };
                    let defer_rest_routing =
                        state.active_ability_continuation().is_some_and(|cont| {
                            dig_continuation_needs_full_looked_at_tracked_set(&cont.chain)
                        });
                    let reqs = kept
                        .iter()
                        .map(|&obj_id| {
                            crate::game::zone_pipeline::ZoneMoveRequest::effect(
                                obj_id,
                                kept_zone,
                                dig_source_id.unwrap_or(obj_id),
                            )
                        })
                        .collect();
                    crate::game::zone_pipeline::move_objects_simultaneously_then(
                        state,
                        reqs,
                        Some(
                            crate::types::game_state::BatchCompletion::DigKeptDeliveryComplete {
                                player,
                                source_id: dig_source_id,
                                rest_cards: if defer_rest_routing {
                                    Vec::new()
                                } else {
                                    unkept
                                },
                                rest_destination: rest_destination.unwrap_or(Zone::Graveyard),
                                rest_order,
                                publish_tracked_set: publish_set,
                                continuation_targets: kept.clone(),
                            },
                        ),
                        events,
                    );
                    return Ok(ResolutionChoiceOutcome::WaitingFor(
                        state.waiting_for.clone(),
                    ));
                }
                for &obj_id in &kept {
                    // CR 614.1c + CR 306.5b / CR 310.4b: route battlefield
                    // entries through the zone-change pipeline so the delivery
                    // tail seeds intrinsic enters-with counters and applies the
                    // CR 614.1 tap-state. The previous manual `obj.tapped` is
                    // dropped (the tail does it from the seeded EntryMods).
                    // CR 400.7: attribute the entry to the dig's source when
                    // known; otherwise the moved object anchors itself (the
                    // pre-pipeline raw move recorded no source).
                    let mut req = crate::game::zone_pipeline::ZoneMoveRequest::effect(
                        obj_id,
                        Zone::Battlefield,
                        dig_source_id.unwrap_or(obj_id),
                    );
                    req.mods.enter_tapped =
                        crate::types::zones::EtbTapState::from_legacy_bool(enter_tapped);
                    req.mods.enters_attacking = enters_attacking;
                    match crate::game::zone_pipeline::move_object(state, req, events) {
                        crate::game::zone_pipeline::ZoneMoveResult::Done => {}
                        // CR 303.4f / CR 616.1: the kept card's battlefield
                        // entry paused on an as-enters choice (aura host pick /
                        // replacement ordering). The pause is already parked;
                        // defer the rest-pile move + tracked-set publish +
                        // continuation wiring onto the batch tail so the drain
                        // runs it once the entry resolves — otherwise the
                        // unkept cards strand in the library (they were not yet
                        // moved). The drain fires on both the replacement-choice
                        // resume and the aura-attachment resume.
                        //
                        // SCOPING (multi-kept limitation, pre-existing,
                        // strictly no-worse-than-before): this `return` exits
                        // the `for &obj_id in &kept` loop, so if kept card #1
                        // pauses, kept #2+ are NOT moved to the battlefield —
                        // they remain in the library. The deferred completion
                        // only finishes the rest-pile (unkept) move and the
                        // tracked-set publish; it does not resume the kept
                        // loop. The old raw-`move_to_zone` path had the same
                        // ceiling (it could not pause and resume a kept tail
                        // either), so this is no regression. WRINKLE:
                        // `publish_tracked_set: Some(kept.clone())` publishes
                        // ALL kept cards, including the unmoved #2+, so a
                        // downstream sub-ability keyed off the tracked set can
                        // be wired to cards still in the library on this paused
                        // path. Acceptable today because no supported dig card
                        // both keeps 2+ cards to the battlefield AND surfaces an
                        // as-enters pause on the first; revisit if such a card
                        // is added (the fix is a kept-loop continuation, not a
                        // single completion).
                        crate::game::zone_pipeline::ZoneMoveResult::NeedsChoice(_)
                        | crate::game::zone_pipeline::ZoneMoveResult::NeedsAuraAttachmentChoice => {
                            crate::game::zone_pipeline::defer_completion_on_pause(
                                state,
                                crate::types::game_state::BatchCompletion::RevealRestPile {
                                    player,
                                    source_id: dig_source_id,
                                    rest_cards: unkept.clone(),
                                    rest_destination: rest_destination.unwrap_or(Zone::Graveyard),
                                    rest_order,
                                    clear_markers: Vec::new(),
                                    publish_tracked_set: Some(kept.clone()),
                                    emit_reveal_until_resolved: None,
                                    manifested_for_continuation: None,
                                },
                            );
                            return Ok(ResolutionChoiceOutcome::WaitingFor(
                                state.waiting_for.clone(),
                            ));
                        }
                    }
                }
            }
            // CR 701.20b + CR 608.2c: Publish a tracked set for downstream
            // sub_abilities. Reveal/keep continuations (Zimone land split) bind
            // the kept subset; Expressive Iteration's bottom/exile tail binds the
            // unkept looked-at pile when its continuation chains
            // `PutAtLibraryPosition { TrackedSet }`.
            let publish_set = if kept.is_empty() {
                Vec::new()
            } else if state
                .active_ability_continuation()
                .is_some_and(|cont| dig_continuation_needs_full_looked_at_tracked_set(&cont.chain))
            {
                // Expressive Iteration-style bottom/exile tail: downstream
                // `TrackedSet` steps address the unkept looked-at pile only.
                unkept.clone()
            } else {
                kept.clone()
            };
            // None => Graveyard; map to a concrete zone so the rest mover
            // (shared with the search-split partition) has a single Zone.
            // When a continuation owns the unkept pile (Expressive Iteration
            // bottom/exile tail), do not pre-route here. A `Moved` replacement
            // can pause this batch, so the tracked-set publication and
            // continuation wiring stay below the result match: neither may see
            // a pre-redirect rest pile.
            let defer_rest_routing = state
                .active_ability_continuation()
                .is_some_and(|cont| dig_continuation_needs_full_looked_at_tracked_set(&cont.chain));
            if !defer_rest_routing {
                match route_rest_partition(
                    state,
                    &unkept,
                    rest_destination.unwrap_or(Zone::Graveyard),
                    rest_order,
                    dig_source_id,
                    events,
                ) {
                    crate::game::zone_pipeline::BatchMoveResult::Done => {}
                    crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => {
                        // CR 701.20e + CR 616.1: `route_rest_partition` has
                        // parked the undelivered suffix. Its cleanup tail owns
                        // the publication and continuation work, so the drain
                        // performs it exactly once only after the true batch end.
                        crate::game::zone_pipeline::defer_completion_on_pause(
                            state,
                            crate::types::game_state::BatchCompletion::RevealRestPile {
                                player,
                                source_id: dig_source_id,
                                rest_cards: Vec::new(),
                                rest_destination: rest_destination.unwrap_or(Zone::Graveyard),
                                rest_order,
                                clear_markers: Vec::new(),
                                publish_tracked_set: Some(publish_set),
                                emit_reveal_until_resolved: None,
                                manifested_for_continuation: None,
                            },
                        );
                        return Ok(ResolutionChoiceOutcome::WaitingFor(
                            state.waiting_for.clone(),
                        ));
                    }
                }
            }
            effects::publish_fresh_tracked_set(state, publish_set);
            if let Some(frame) = state.active_ability_continuation_frame_mut() {
                // CR 608.2c: ParentTarget continuations (Hideaway conceal, dig
                // conditionals on the kept card) bind to the kept selection.
                // Hand/bottom/exile tails route via TrackedSetFiltered instead.
                frame.pending.chain.targets =
                    kept.iter().map(|&id| TargetRef::Object(id)).collect();
                frame.pending.chain.context.optional_effect_performed = !kept.is_empty();
            }
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
        }
        (
            WaitingFor::SurveilChoice { player, cards },
            GameAction::SelectCards { cards: top_cards },
        ) => {
            // CR 701.25a: To surveil N, put any number of the looked-at cards into
            // your graveyard and the rest on top of your library in any order. The
            // action payload mirrors scry — it is the ordered keep-on-top set;
            // every looked-at card not in it is put into the graveyard.
            let all_cards = cards;
            // CR 701.25a: the keep-on-top set must be a duplicate-free subset of
            // the looked-at cards (any order is legal).
            validate_keep_on_top_selection(&top_cards, &all_cards)?;
            let to_graveyard: Vec<_> = all_cards
                .iter()
                .filter(|id| !top_cards.contains(id))
                .copied()
                .collect();
            // CR 701.25a + CR 614.6: every looked-at card not kept on top is put
            // into the graveyard through the simultaneous-move batch so each
            // card's own `Moved` redirects (Rest in Peace / Leyline of the Void:
            // "would be put into a graveyard from anywhere → exile instead") fire.
            // A raw `move_to_zone` proposed no per-card ZoneChange and silently
            // skipped them. The kept-on-top library placement is the post-loop
            // work; it must run exactly once after the whole pile lands, so on a
            // mid-pile CR 616.1 pause it is deferred onto the parked batch tail
            // and the drain runs it. The common single-redirect path never pauses
            // and runs the placement inline below.
            let reqs: Vec<_> = to_graveyard
                .iter()
                .map(|&obj_id| {
                    crate::game::zone_pipeline::ZoneMoveRequest::effect(
                        obj_id,
                        Zone::Graveyard,
                        obj_id,
                    )
                })
                .collect();
            // The kept-on-top library placement + continuation drain (the
            // post-loop work) is carried as the batch completion so it runs
            // exactly once whether the pile lands synchronously or across a CR
            // 616.1 pause.
            let completion =
                crate::types::game_state::BatchCompletion::SurveilKeepOnTop { player, top_cards };
            crate::game::zone_pipeline::move_objects_simultaneously_then(
                state,
                reqs,
                Some(completion),
                events,
            );
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        (
            WaitingFor::RevealChoice {
                player,
                cards,
                filter,
                optional,
                decline_runs_continuation,
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            // CR 701.20a: Optional reveal prompts (e.g., reveal-lands like Port Town)
            // accept an empty selection to signal "I decline to reveal." The source
            // replacement's decline ability runs via `pending_continuation`, which the
            // effect's resolver populated with the decline branch before the prompt.
            if optional && chosen.is_empty() {
                state
                    .resolve_and_apply_information(
                        &cards,
                        ResolvedInformationAudience::Controller(player),
                        ResolvedInformationLifetime::UntilActionBoundary,
                        ResolvedInformationEdit::Hide,
                    )
                    .expect("reveal-choice cleanup must reference live card occurrences");
                state.private_look_ids.clear();
                state.private_look_player = None;
                set_priority(state, player);
                if decline_runs_continuation {
                    super::engine::resume_pending_continuation_if_priority(state, events)
                        .expect("a settled reveal choice must resume its continuation");
                } else {
                    let _ = state
                        .clear_active_ability_continuation_or_batch_delivery_child()
                        .expect("declined reveal cannot clear a buried continuation");
                }
                return Ok(ResolutionChoiceOutcome::WaitingFor(
                    state.waiting_for.clone(),
                ));
            }
            if chosen.len() != 1 {
                return Err(EngineError::InvalidAction(format!(
                    "Must select exactly 1 card, got {}",
                    chosen.len()
                )));
            }
            let chosen_id = chosen[0];
            if !cards.contains(&chosen_id) {
                return Err(EngineError::InvalidAction(
                    "Selected card not in revealed hand".to_string(),
                ));
            }
            if !matches!(filter, crate::types::ability::TargetFilter::Any)
                && !super::filter::matches_target_filter(
                    state,
                    chosen_id,
                    &filter,
                    &super::filter::FilterContext::from_source(state, chosen_id),
                )
            {
                return Err(EngineError::InvalidAction(
                    "Selected card does not match the required filter".to_string(),
                ));
            }

            state
                .resolve_and_apply_information(
                    &cards,
                    ResolvedInformationAudience::Controller(player),
                    ResolvedInformationLifetime::UntilActionBoundary,
                    ResolvedInformationEdit::Hide,
                )
                .expect("reveal-choice cleanup must reference live card occurrences");
            state.private_look_ids.clear();
            state.private_look_player = None;

            set_priority(state, player);
            // CR 701.20a: For an optional reveal, the stashed continuation is the
            // decline branch (e.g., Tap SelfRef for reveal-lands). The player picked,
            // so decline must NOT run — drop the continuation. Non-optional reveals
            // chain targets into the continuation so the follow-up effect operates
            // on the revealed card (e.g., Thoughtseize's exile).
            if optional && decline_runs_continuation {
                let _ = state
                    .clear_active_ability_continuation_or_batch_delivery_child()
                    .expect("accepted reveal cannot clear a buried continuation");
            } else if let Some(frame) = state.active_ability_continuation_frame_mut() {
                frame.pending.chain.targets = vec![TargetRef::Object(chosen_id)];
                if optional {
                    frame.pending.chain.context.optional_effect_performed = true;
                }
            }
            super::engine::resume_pending_continuation_if_priority(state, events)
                .expect("a settled reveal choice must resume its continuation");
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        (
            WaitingFor::SearchChoice {
                player,
                library_owner,
                cards,
                count,
                reveal,
                up_to,
                allows_partial_find,
                constraint,
                split,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            if effects::scoped_library_search::submit_selection(
                state,
                player,
                library_owner,
                &cards,
                count,
                reveal,
                up_to,
                allows_partial_find,
                &constraint,
                &chosen,
                events,
            )
            .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?
            {
                return Ok(ResolutionChoiceOutcome::WaitingFor(
                    state.waiting_for.clone(),
                ));
            }
            // CR 701.23b/d: "up to N", hidden-zone stated-quality searches, or
            // explicit stated-quality selection constraints accept a short/empty
            // pick. A pure quantity search needs exactly `count`.
            let lower_bounded = up_to || allows_partial_find || constraint.permits_partial_find();
            let valid = if lower_bounded {
                chosen.len() <= count
            } else {
                chosen.len() == count
            };
            if !valid {
                return Err(EngineError::InvalidAction(format!(
                    "Must select {}{} card(s), got {}",
                    if lower_bounded { "up to " } else { "exactly " },
                    count,
                    chosen.len()
                )));
            }
            for card_id in &chosen {
                if !cards.contains(card_id) {
                    return Err(EngineError::InvalidAction(
                        "Selected card not in search results".to_string(),
                    ));
                }
            }
            // CR 608.2c: Enforce the printed-text selection restriction at the
            // submission boundary so the AI candidate filter and the engine
            // resolver agree on legality.
            if !effects::search_library::selection_satisfies_constraint(state, &chosen, &constraint)
            {
                return Err(EngineError::InvalidAction(
                    "Selected cards do not satisfy the search-selection constraint".to_string(),
                ));
            }

            let chosen = match apply_search_found_replacements(
                state,
                player,
                library_owner,
                &chosen,
                crate::types::game_state::PendingSearchFoundContinuation::Standard {
                    split: split.clone(),
                },
                reveal,
                events,
            ) {
                Ok(chosen) => chosen,
                Err(waiting) => {
                    return Ok(ResolutionChoiceOutcome::WaitingFor(*waiting));
                }
            };
            // CR 701.23a + CR 608.2c: Cultivate-class split destination. The
            // found set was just chosen; now partition it. Up to two prompts
            // total (CR 609.3): SearchChoice (done) then SearchPartitionChoice
            // (only when more than primary_count were found).
            if let Some(split) = split {
                // Search-specific control ends before the partition prompt;
                // learned visibility remains until both destination piles land.
                state.active_search_decision_controls.remove(&player);
                // The Shuffle continuation always exists for cultivate-class
                // splits; its `source_id` is the search card. Falls back to the
                // first chosen card's id only in the degenerate no-continuation
                // case (used solely as an event source label).
                let source_id = state
                    .active_ability_continuation()
                    .map(|cont| cont.chain.source_id)
                    .or_else(|| chosen.first().copied())
                    .unwrap_or(ObjectId(0));
                let primary_count = split.primary_count as usize;
                if chosen.len() > primary_count {
                    // CR 608.2d: Genuine choice — the searcher picks which
                    // primary_count cards go to the primary destination.
                    set_priority(state, player);
                    state.waiting_for = WaitingFor::SearchPartitionChoice {
                        player,
                        cards: chosen.clone(),
                        primary_destination: split.primary_destination,
                        primary_count: split.primary_count,
                        primary_enter_tapped: split.primary_enter_tapped,
                        rest_destination: split.rest_destination,
                        source_id,
                    };
                    return Ok(ResolutionChoiceOutcome::WaitingFor(
                        state.waiting_for.clone(),
                    ));
                }
                // CR 609.3 fast-path: found <= primary_count, so ALL chosen go to
                // the primary destination and the rest is empty. No second prompt.
                let events_before_drain = events.len();
                match apply_search_partition(
                    state,
                    &chosen,
                    &[],
                    &split,
                    source_id,
                    player,
                    events,
                )? {
                    crate::game::zone_pipeline::BatchMoveResult::Done => {}
                    crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => {
                        return Ok(ResolutionChoiceOutcome::WaitingFor(
                            state.waiting_for.clone(),
                        ));
                    }
                }
                set_priority(state, player);
                super::engine::resume_pending_continuation_if_priority(state, events)
                    .expect("a settled search choice must resume its continuation");
                return Ok(collect_search_observer_triggers(
                    state,
                    events,
                    events_before_drain,
                ));
            }

            finalize_standard_search_selection(state, player, &chosen, events)
        }
        (
            WaitingFor::SearchPartitionChoice {
                player,
                cards,
                primary_destination,
                primary_count,
                primary_enter_tapped,
                rest_destination,
                source_id,
            },
            GameAction::SelectCards {
                cards: primary_chosen,
            },
        ) => {
            // CR 608.2d: The searcher must choose exactly primary_count cards for
            // the primary destination; this branch is only parked when more than
            // primary_count cards were found.
            if primary_chosen.len() != primary_count as usize {
                return Err(EngineError::InvalidAction(format!(
                    "Must select exactly {} card(s) for the battlefield, got {}",
                    primary_count,
                    primary_chosen.len()
                )));
            }
            for card_id in &primary_chosen {
                if !cards.contains(card_id) {
                    return Err(EngineError::InvalidAction(
                        "Selected card not in the found set".to_string(),
                    ));
                }
            }
            let rest_ids: Vec<ObjectId> = cards
                .iter()
                .filter(|id| !primary_chosen.contains(id))
                .copied()
                .collect();
            let split = crate::types::ability::SearchDestinationSplit {
                primary_destination,
                primary_count,
                primary_enter_tapped,
                rest_destination,
            };
            state.waiting_for = WaitingFor::Priority { player };
            let events_before_partition = events.len();
            match apply_search_partition(
                state,
                &primary_chosen,
                &rest_ids,
                &split,
                source_id,
                player,
                events,
            )? {
                crate::game::zone_pipeline::BatchMoveResult::Done => {}
                crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => {
                    return Ok(ResolutionChoiceOutcome::WaitingFor(
                        state.waiting_for.clone(),
                    ));
                }
            }
            set_priority(state, player);
            super::engine::resume_pending_continuation_if_priority(state, events)
                .expect("a settled search choice must resume its continuation");
            collect_search_observer_triggers(state, events, events_before_partition)
        }
        (
            WaitingFor::OutsideGameChoice {
                player,
                source_id,
                choices,
                count,
                reveal,
                up_to,
                destination,
            },
            GameAction::ChooseOutsideGameCards { selections },
        ) => {
            let valid = if up_to {
                selections.len() <= count
            } else {
                selections.len() == count
            };
            if !valid {
                return Err(EngineError::InvalidAction(format!(
                    "Must select {}{} outside-game card(s), got {}",
                    if up_to { "up to " } else { "exactly " },
                    count,
                    selections.len()
                )));
            }
            // CR 400.11 + CR 406.3: Each selection must match an offered choice
            // and (for sideboard) not exceed the remaining copies. Face-up
            // exile selections are single-object so duplicates of the same
            // object_id are illegal.
            let mut sideboard_counts: HashMap<usize, usize> = HashMap::new();
            let mut exile_seen: std::collections::HashSet<ObjectId> =
                std::collections::HashSet::new();
            for selection in &selections {
                match selection {
                    OutsideGameSelection::Sideboard { sideboard_index } => {
                        *sideboard_counts.entry(*sideboard_index).or_insert(0) += 1;
                    }
                    OutsideGameSelection::FaceUpExile { object_id } => {
                        if !exile_seen.insert(*object_id) {
                            return Err(EngineError::InvalidAction(
                                "Same face-up exile card selected more than once".to_string(),
                            ));
                        }
                    }
                }
            }
            for (sideboard_index, requested_count) in &sideboard_counts {
                let Some(choice) = choices.iter().find(|choice| match &choice.source {
                    OutsideGameChoiceSource::Sideboard {
                        sideboard_index: idx,
                        ..
                    } => idx == sideboard_index,
                    _ => false,
                }) else {
                    return Err(EngineError::InvalidAction(
                        "Selected sideboard slot not in outside-game choices".to_string(),
                    ));
                };
                if *requested_count > choice.count as usize {
                    return Err(EngineError::InvalidAction(
                        "Selected more copies than are available outside the game".to_string(),
                    ));
                }
            }
            for object_id in &exile_seen {
                if !choices.iter().any(|choice| match &choice.source {
                    OutsideGameChoiceSource::FaceUpExile { object_id: oid } => oid == object_id,
                    _ => false,
                }) {
                    return Err(EngineError::InvalidAction(
                        "Selected face-up exile card not in outside-game choices".to_string(),
                    ));
                }
            }

            let mut chosen_ids = Vec::new();
            for selection in selections {
                match selection {
                    OutsideGameSelection::Sideboard { sideboard_index } => {
                        let object_id =
                            effects::search_outside_game::put_sideboard_entry_into_game(
                                state,
                                player,
                                sideboard_index,
                                destination,
                            )
                            .map_err(|error| EngineError::InvalidAction(format!("{error:?}")))?;
                        chosen_ids.push(object_id);
                    }
                    OutsideGameSelection::FaceUpExile { object_id } => {
                        match effects::search_outside_game::put_face_up_exile_into(
                            state,
                            object_id,
                            destination,
                            source_id,
                            player,
                            events,
                        )
                        .map_err(|error| EngineError::InvalidAction(format!("{error:?}")))?
                        {
                            effects::change_zone::ZoneMoveResult::Done => {
                                chosen_ids.push(object_id);
                            }
                            effects::change_zone::ZoneMoveResult::NeedsChoice(choice_player) => {
                                state.waiting_for =
                                    super::replacement::replacement_choice_waiting_for(
                                        choice_player,
                                        state,
                                    );
                                return Ok(action_result_outcome(
                                    events,
                                    state.waiting_for.clone(),
                                ));
                            }
                            effects::change_zone::ZoneMoveResult::NeedsAuraAttachmentChoice => {
                                return Ok(action_result_outcome(
                                    events,
                                    state.waiting_for.clone(),
                                ));
                            }
                        }
                    }
                }
            }

            if reveal {
                state.last_revealed_ids = chosen_ids.clone();
                for &card_id in &chosen_ids {
                    state.revealed_cards.insert(card_id);
                }
                let card_names: Vec<String> = chosen_ids
                    .iter()
                    .filter_map(|id| state.objects.get(id).map(|obj| obj.name.clone()))
                    .collect();
                events.push(GameEvent::CardsRevealed {
                    player,
                    card_ids: chosen_ids.clone(),
                    card_names,
                });
            } else {
                state.last_revealed_ids.clear();
            }

            if let Some(frame) = state.active_ability_continuation_frame_mut() {
                frame.pending.chain.targets =
                    chosen_ids.iter().map(|&id| TargetRef::Object(id)).collect();
            }
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
        }
        (
            WaitingFor::ChooseFromZoneChoice {
                player,
                cards,
                count,
                up_to,
                constraint,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            let valid_count = if up_to {
                chosen.len() <= count
            } else {
                chosen.len() == count
            };
            if !valid_count {
                return Err(EngineError::InvalidAction(format!(
                    "Must select {}{} card(s), got {}",
                    if up_to { "up to " } else { "exactly " },
                    count,
                    chosen.len(),
                )));
            }
            for card_id in &chosen {
                if !cards.contains(card_id) {
                    return Err(EngineError::InvalidAction(
                        "Selected card not in available set".to_string(),
                    ));
                }
            }
            if !effects::choose_from_zone::selection_satisfies_constraint(
                state,
                &chosen,
                constraint.as_ref(),
            ) {
                return Err(EngineError::InvalidAction(
                    "Selected cards do not satisfy the tracked-set choice constraint".to_string(),
                ));
            }

            let unchosen: Vec<_> = cards
                .iter()
                .filter(|id| !chosen.contains(id))
                .copied()
                .collect();
            let priority_player = state
                .active_ability_continuation()
                .map(|cont| cont.chain.controller)
                .unwrap_or(player);
            set_priority(state, priority_player);
            // CR 101.4 + CR 608.2c: A per-player `ChooseFromZone { EachPlayer }`
            // iteration does NOT partition this player's pick into the
            // continuation's target slots — each pick is accumulated into the
            // chain's tracked set by `drain_active_per_player_zone_choice`, and
            // the continuation ("put those cards onto the battlefield") reads
            // that tracked set. Hand the choice straight to the drain so it can
            // accumulate and prompt the next player (Breach the Multiverse).
            if state.active_per_player_zone_choice().is_some() {
                effects::choose_from_zone::drain_active_per_player_zone_choice(
                    state, &chosen, events,
                );
                // Only after every player has been prompted (the drain leaves no
                // pending iteration and is no longer waiting on a choice) does
                // the parked continuation run.
                super::engine::resume_pending_continuation_if_priority(state, events)
                    .expect("a settled zone choice must resume its continuation");
                return Ok(ResolutionChoiceOutcome::WaitingFor(
                    state.waiting_for.clone(),
                ));
            }
            // CR 608.2c + CR 105.1 / CR 205.2a: A per-category-member
            // `Effect::ForEachCategoryExile` iteration accumulates each pick into
            // the chain's tracked set and prompts the next member, exactly like
            // the per-player path (Sanar, Portent of Calamity). The continuation
            // ("from among them" / "put the rest …") reads that tracked set.
            if state.active_per_category_zone_choice().is_some() {
                match effects::choose_from_zone::drain_active_per_category_zone_choice(
                    state, &chosen, events,
                ) {
                    crate::game::zone_pipeline::BatchMoveResult::Done => {}
                    crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => {
                        return Ok(ResolutionChoiceOutcome::WaitingFor(
                            state.waiting_for.clone(),
                        ));
                    }
                }
                super::engine::resume_pending_continuation_if_priority(state, events)
                    .expect("a settled zone choice must resume its continuation");
                return Ok(ResolutionChoiceOutcome::WaitingFor(
                    state.waiting_for.clone(),
                ));
            }
            // CR 608.2c: When the parked continuation consumes the chain's
            // tracked set (a `GrantCastingPermission { target: TrackedSet }` /
            // any `TrackedSet`-referencing downstream effect — e.g. End-Blaze
            // Epiphany's "choose a card exiled this way … you may play that
            // card"), the interactive choose is the producer of that set, so the
            // chosen cards must be published as the fresh tracked set the
            // continuation reads. This mirrors the per-player drain
            // (`publish_fresh_tracked_set`) and the random-choose path
            // (`apply_parent_chain_context`); without it the grant's
            // `TrackedSet(0)` sentinel binds to a stale/empty set and the
            // permission lands nowhere. Gated on the continuation actually
            // referencing a tracked set so non-consuming continuations
            // (partition sub-abilities reading `ParentTarget`) are unaffected.
            let continuation_consumes_tracked_set = state
                .active_ability_continuation()
                .is_some_and(|cont| effects::chain_references_tracked_set(&cont.chain));
            if continuation_consumes_tracked_set {
                effects::publish_fresh_tracked_set(state, chosen.clone());
            }
            // CR 608.2c + CR 608.2d: A counter-kind choice first selects the
            // object whose counters define the legal kinds. Preserve that
            // object's exact public snapshot on the continuation so later
            // "each other ..." text can exclude it without conflating it with
            // the spell's source or a separately declared downstream target.
            let counter_kind_choice = state
                .active_ability_continuation()
                .filter(|cont| {
                    matches!(
                        cont.chain.effect,
                        crate::types::ability::Effect::ChooseCounterKind { .. }
                    )
                })
                .and_then(|_| chosen.first())
                .and_then(|id| {
                    state.objects.get(id).map(|object| {
                        crate::types::ability::CostPaidObjectSnapshot {
                            object_id: *id,
                            lki: object.snapshot_for_mana_spent(),
                        }
                    })
                });
            if let Some(frame) = state.active_ability_continuation_frame_mut() {
                let cont = &mut frame.pending;
                if let Some(snapshot) = counter_kind_choice {
                    if let crate::types::ability::Effect::ChooseCounterKind { target } =
                        &mut cont.chain.effect
                    {
                        *target = crate::types::ability::TargetFilter::SpecificObject {
                            id: snapshot.object_id,
                        };
                    }
                    cont.chain.set_effect_context_object_recursive(snapshot);
                } else {
                    cont.chain.targets = chosen.iter().map(|&id| TargetRef::Object(id)).collect();
                }
                // CR 607.2a + CR 608.2g: A `FreeCastFromZones` continuation
                // over "the other cards exiled this way" (Plargg and Nassari)
                // must confine its offer to THIS resolution's exile batch. The
                // choose's offered pool (`cards`) IS that typed, concrete
                // batch — it was derived from the chain's tracked set the
                // exile clause published within this resolution — so forward
                // the FULL pool (not just `chosen`) as the window head's
                // object targets; the resolver reads them as its member pool
                // and intersects the exile-zone scan with it BEFORE the
                // filter's `Not(InTrackedSet)` chosen-card exclusion. Without
                // this, `ExiledBySource` alone reads the source's complete
                // live linked-exile ledger and a linked nonland card left in
                // exile by a PREVIOUS resolution would be wrongly offered. The
                // window never reads `ParentTarget`, so overriding the generic
                // `targets = chosen` forward is safe for this head.
                if matches!(
                    cont.chain.effect,
                    crate::types::ability::Effect::FreeCastFromZones { .. }
                ) {
                    cont.chain.targets = cards.iter().map(|&id| TargetRef::Object(id)).collect();
                }
                // CR 700.2 + CR 608.2c: The "unchosen" partition is forwarded
                // to the sub-ability ONLY for the zone-partition pattern
                // (`ChooseFromZone`: chosen cards go one place, the rest go
                // another). A counter-placement continuation (Bolster keyword
                // action; Gluntch's "they put counters on a creature they
                // control") is NOT a partition — its `sub_ability` is an
                // independent trailing clause (e.g. the next `Choose`) and
                // must not have the non-picked objects forced into its target
                // list. Gate the forward on the continuation's own effect.
                let is_partition = !matches!(
                    cont.chain.effect,
                    crate::types::ability::Effect::PutCounter { .. }
                        | crate::types::ability::Effect::ChooseCounterKind { .. }
                );
                if is_partition {
                    if let Some(ref mut next_sub) = cont.chain.sub_ability {
                        // CR 707.12a: A `CastCopyOfCard` continuation casts the
                        // copies the player selected ("may cast" is decided
                        // individually per copy), never the ones declined. It is
                        // the buried consumer of a copy-cast choice whose
                        // continuation head is an unrelated prepended sibling
                        // (Mizzix's Mastery's "Exile Mizzix's Mastery"
                        // self-exile), so forcing the un-selected copies onto it
                        // as a partition would cast exactly the copies the player
                        // declined — declining every copy (empty selection) would
                        // otherwise re-derive and cast the whole exiled set. The
                        // chosen copies still reach it by inheriting the head's
                        // `targets` (set to `chosen` above) down the chain.
                        if !matches!(
                            next_sub.effect,
                            crate::types::ability::Effect::CastCopyOfCard { .. }
                        ) {
                            next_sub.targets =
                                unchosen.iter().map(|&id| TargetRef::Object(id)).collect();
                        }
                    }
                }
            }
            // CR 608.2: Restore the paused resolution's trigger context (captured
            // at the `ChooseFromZone` raise) so an `EventContextAmount` ("that
            // many") sub_ability reads the triggering event's amount — Amy Pond:
            // "choose a suspended card you own and remove that many time counters
            // from it". Save/restore (not a bare clear) keeps this re-entrant: a
            // nested `ChooseFromZone` in the drained continuation re-captures, and
            // the `prev` values are restored after the inner drain, leaving no
            // stale leak past the action boundary. Mirrors
            // `WaitingFor::ChooseObjectsSelection` and the
            // optional-effect frame trigger-context round-trip.
            let prev_trigger_event = state.current_trigger_event.clone();
            let prev_trigger_match_count = state.current_trigger_match_count;
            let prev_die_result = state.die_result_this_resolution;
            if let Some(ctx) = state
                .active_ability_continuation_frame_mut()
                .and_then(|frame| frame.choose_zone_trigger_context.take())
            {
                state.current_trigger_event = ctx.event;
                state.current_trigger_match_count = ctx.match_count;
                state.die_result_this_resolution = ctx.die_result;
            }
            super::engine::resume_pending_continuation_if_priority(state, events)
                .expect("a settled zone choice must resume its continuation");
            state.current_trigger_event = prev_trigger_event;
            state.current_trigger_match_count = prev_trigger_match_count;
            state.die_result_this_resolution = prev_die_result;
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        (
            WaitingFor::ChooseOneOfBranch {
                player,
                controller,
                source_id,
                branches,
                branch_descriptions: _,
                parent_targets,
                context,
                continuation,
                replacement_applied,
                remaining_players,
            },
            GameAction::ChooseBranch { index },
        ) => {
            set_priority(state, player);
            effects::choose_one_of::resolve_branch(
                state,
                effects::choose_one_of::BranchSelection {
                    player,
                    controller,
                    source_id,
                    branches,
                    parent_targets,
                    context,
                    continuation,
                    replacement_applied,
                    remaining_players,
                    index,
                },
                events,
            )
            .map_err(|err| EngineError::InvalidAction(err.to_string()))?;
            // CR 614.12a: For an "enters with your choice of counter" replacement
            // (Denry Klin), the entering permanent's battlefield-entry ZoneChanged
            // event was deferred into `state.deferred_entry_events` by the ETB-
            // replacement capture in `engine_replacement.rs` so observers don't
            // fire before the choice is made. Now that `resolve_branch` has folded
            // the chosen counter onto the still-entering permanent, replay the
            // deferred entry through the trigger pipeline so ETB observers see the
            // counter as the permanent enters (pre-entry per CR 614.12a, not a
            // post-entry counter add). For a normal (non-entry) `ChooseOneOf`,
            // `deferred_entry_events` is empty, so this is a no-op — the
            // disambiguator. This is safe because `deferred_entry_events` is
            // populated ONLY by the ETB-replacement capture (sole production
            // write-site), and `CopyTargetChoice` drains it via its own
            // `handle_copy_target_choice` handler, so it is never non-empty during
            // an unrelated `ChooseBranch`.
            let deferred = std::mem::take(&mut state.deferred_entry_events);
            let source_still_on_bf = state
                .objects
                .get(&source_id)
                .is_some_and(|o| o.zone == Zone::Battlefield);
            if !deferred.is_empty() && source_still_on_bf {
                super::triggers::process_triggers(state, &deferred);
                let delayed = super::triggers::check_delayed_triggers(state, &deferred);
                events.extend(delayed);
            }
            // CR 608.2c + CR 122.1: advance any paused resolution chain after the
            // branch resolves. This is the standard post-resolution step every
            // sibling choice handler runs. It no-ops when no `pending_continuation`
            // / a repeat-for frame exists (each drain block is guarded by
            // `if let Some(..) = ..take()`), so it is safe for existing `ChooseOneOf`
            // consumers and for the deferred-entry replay above (mutually exclusive
            // slots). Required so a `repeat_for: DistinctCounterKindsAmong` loop
            // paused on `ChooseOneOfBranch` advances past the first counter kind to
            // prompt for each remaining kind (Bribe Taker).
            super::engine::resume_pending_continuation_if_priority(state, events)
                .expect("a settled branch choice must resume its continuation");
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        (
            WaitingFor::DiscardToHandSize {
                player,
                count,
                cards,
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            if chosen.len() != count {
                return Err(EngineError::InvalidAction(format!(
                    "Must discard exactly {} card(s), got {}",
                    count,
                    chosen.len()
                )));
            }
            for card_id in &chosen {
                if !cards.contains(card_id) {
                    return Err(EngineError::InvalidAction(
                        "Selected card not in hand".to_string(),
                    ));
                }
            }

            let event_start = events.len();
            if turns::finish_cleanup_discard(state, player, &chosen, events) {
                return Ok(action_result_outcome(events, state.waiting_for.clone()));
            }

            // CR 514.3a + CR 603.3 + CR 117.5: cleanup-discard events must pass
            // through the ordinary SBA/trigger settlement before cleanup can end.
            // Synchronize the provisional priority first: this is the authority
            // that normalizes legacy waiting states and derives the authorized
            // priority submitter under turn control.
            let provisional_cleanup_priority = WaitingFor::Priority { player };
            public_state::sync_waiting_for(state, &provisional_cleanup_priority);
            let settled = engine_priority::run_post_action_pipeline_from(
                state,
                events,
                event_start,
                &provisional_cleanup_priority,
                false, // skip_trigger_scan
                false, // skip_deferred_trigger_drain
            )?;
            public_state::sync_waiting_for(state, &settled);

            if matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.is_empty() {
                let _ = turns::advance_phase_once(state, events);
                let advanced = turns::auto_advance(state, events);
                public_state::sync_waiting_for(state, &advanced);
            }

            // The suffix pipeline above already processed this action's discard
            // events, including persistent delayed triggers. Return the completed
            // action rather than entering apply_action's outer full-buffer pipeline,
            // which would otherwise scan those discard events a second time.
            return Ok(action_result_outcome(events, state.waiting_for.clone()));
        }
        (
            WaitingFor::ConniveDiscard {
                player,
                conniver,
                source_id: _,
                cards,
                count,
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            if chosen.len() != count {
                return Err(EngineError::InvalidAction(format!(
                    "Must discard exactly {} card(s), got {}",
                    count,
                    chosen.len()
                )));
            }

            let current_hand: std::collections::HashSet<ObjectId> = state
                .players
                .iter()
                .find(|candidate| candidate.id == player)
                .map(|candidate| candidate.hand.iter().copied().collect())
                .unwrap_or_default();

            for card_id in &chosen {
                if !cards.contains(card_id) {
                    return Err(EngineError::InvalidAction(
                        "Selected card not from connive draw".to_string(),
                    ));
                }
                if !current_hand.contains(card_id) {
                    return Err(EngineError::InvalidAction(
                        "Card no longer in hand".to_string(),
                    ));
                }
            }

            let Some(nonland_count) =
                effects::connive::discard_all_and_count_nonlands(state, &chosen, player, events)
            else {
                return Ok(action_result_outcome(events, state.waiting_for.clone()));
            };

            effects::connive::add_connive_counters(state, &conniver, nonland_count, events);
            // CR 701.50f + CR 701.50b: the EffectResolved carries the CONNIVER's
            // id (LKI if it left the battlefield) so "whenever a creature you
            // control connives" matches the conniving permanent, not the source.
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Connive,
                source_id: conniver.object_id(),
                subject: Some(Box::new(conniver.snapshot)),
            });
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
        }
        (
            WaitingFor::DiscardChoice {
                player,
                count,
                cards,
                source_id,
                effect_kind,
                up_to,
                unless_filter,
                discard_frame,
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            let unless_satisfied = unless_filter.as_ref().is_some_and(|filter| {
                chosen.len() == 1
                    && chosen.iter().all(|&card_id| {
                        crate::game::filter::matches_target_filter(
                            state,
                            card_id,
                            filter,
                            &crate::game::filter::FilterContext::from_source(state, source_id),
                        )
                    })
            });

            if !unless_satisfied {
                if up_to && chosen.len() > count {
                    return Err(EngineError::InvalidAction(format!(
                        "Must discard at most {} card(s), got {}",
                        count,
                        chosen.len()
                    )));
                }
                if !up_to && chosen.len() != count {
                    return Err(EngineError::InvalidAction(format!(
                        "Must discard exactly {} card(s), got {}",
                        count,
                        chosen.len()
                    )));
                }
            }

            // CR 608.2d: A resolving player can't choose one eligible card
            // more than once to satisfy a multi-card discard selection.
            let unique_chosen: HashSet<ObjectId> = chosen.iter().copied().collect();
            if unique_chosen.len() != chosen.len() {
                return Err(EngineError::InvalidAction(
                    "Selected cards must be distinct".to_string(),
                ));
            }

            let current_hand: std::collections::HashSet<ObjectId> = state
                .players
                .iter()
                .find(|candidate| candidate.id == player)
                .map(|candidate| candidate.hand.iter().copied().collect())
                .unwrap_or_default();

            for card_id in &chosen {
                if !cards.contains(card_id) {
                    return Err(EngineError::InvalidAction(
                        "Selected card not in eligible set".to_string(),
                    ));
                }
                if !current_hand.contains(card_id) {
                    return Err(EngineError::InvalidAction(
                        "Card no longer in hand".to_string(),
                    ));
                }
            }

            let chosen_refs = chosen
                .iter()
                .filter_map(|id| state.objects.get(id))
                .map(crate::types::identifiers::ObjectIncarnationRef::from_object)
                .collect::<Vec<_>>();

            let events_before_effect = events.len();
            for (index, &card_id) in chosen.iter().enumerate() {
                if let effects::discard::DiscardOutcome::NeedsReplacementChoice(choice_player) =
                    effects::discard::discard_caused_by_effect_with_source_and_frame(
                        state,
                        card_id,
                        player,
                        Some(source_id),
                        discard_frame,
                        events,
                    )
                {
                    state.waiting_for =
                        super::replacement::replacement_choice_waiting_for(choice_player, state);
                    state.pending_discard_batch = Some(Box::new(
                        crate::types::game_state::PendingDiscardBatch {
                            player,
                            cursor: crate::types::game_state::DiscardBatchCursor::Ordered {
                                remaining: chosen_refs[index + 1..].to_vec(),
                            },
                            completion:
                                crate::types::game_state::PendingDiscardBatchCompletion::DiscardChoice {
                                    chosen: chosen_refs,
                                },
                            source_id,
                            effect_kind,
                            paused_card: crate::types::identifiers::ObjectIncarnationRef::of(
                                card_id,
                                state.objects[&card_id].incarnation,
                            ),
                            discard_frame,
                            fan_out: None,
                            preceding_events: events[events_before_effect..].to_vec(),
                        },
                    ));
                    defer_observer_triggers_for_paused_choice(state, events, events_before_effect);
                    return Ok(action_result_outcome(events, state.waiting_for.clone()));
                }
            }
            let events_after_move = events.len();

            let completion =
                crate::types::game_state::PendingDiscardBatchCompletion::DiscardChoice {
                    chosen: chosen_refs,
                };
            effects::finalize_discard_choice_completion(
                state,
                &completion,
                discard_frame,
                &events[events_before_effect..],
            );
            events.push(GameEvent::EffectResolved {
                kind: effect_kind,
                source_id,
                subject: None,
            });

            // CR 614.12a: this `DiscardChoice` was the interactive payment of an
            // optional `MayCost` replacement's accept (e.g. Mox Diamond's
            // "discard a land card" with multiple eligible lands). The cost is
            // now paid, so resume the parked replacement with the accept index —
            // `continue_replacement` sees `may_cost_paid: true`, pays any
            // `may_cost_remaining`, and finishes entering the permanent. This
            // runs instead of the ordinary continuation drain (there is no
            // `Effect::PayCost` chain behind a replacement-originated discard).
            if state
                .pending_replacement
                .as_ref()
                .is_some_and(|pending| pending.may_cost_paid)
            {
                let waiting_for =
                    super::engine_replacement::handle_replacement_choice(state, 0, events)?;
                if let Some(outcome) = batch_or_drain_observer_triggers(
                    state,
                    events,
                    events_before_effect,
                    events_after_move,
                    false,
                ) {
                    return Ok(outcome);
                }
                return Ok(ResolutionChoiceOutcome::WaitingFor(waiting_for));
            }

            let waiting_for = finish_with_continuation(state, player, events);

            // CR 603.2c: each opponent's discard is a separate occurrence of a
            // `Discarded`-mode trigger event. The resolution-choice dispatch
            // path does not call `run_post_action_pipeline` for a non-settled
            // action, so batch this discard's observer triggers (Waste Not,
            // Megrim, Bone Miser) across the `DiscardChoice` pause — exactly
            // as the `Sacrifice` branch does for dies-triggers.
            //
            // CR 608.2c: A sequential continuation stashed behind this interactive
            // discard (e.g. Shorikai's "then create a Pilot creature token")
            // emits ZoneChanged/TokenCreated during `finish_with_continuation`.
            // Collect BOTH the discard slice and the continuation slice into
            // `deferred_triggers` before a single drain — the discard slice must
            // stay bounded to the move itself, but an early drain after only the
            // discard slice would return `WaitingForWithInlineTriggers` and skip
            // the continuation slice entirely (issue #4245).
            for (start, end) in [
                (events_before_effect, events_after_move),
                (events_after_move, events.len()),
            ] {
                if end <= start {
                    continue;
                }
                let trigger_events: Vec<GameEvent> = events[start..end]
                    .iter()
                    .filter(|ev| !matches!(ev, GameEvent::PhaseChanged { .. }))
                    .cloned()
                    .collect();
                if !trigger_events.is_empty() {
                    super::triggers::collect_triggers_into_deferred(state, &trigger_events);
                }
            }

            if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                if let Some(wf) = super::triggers::drain_deferred_trigger_queue(state, events) {
                    return Ok(ResolutionChoiceOutcome::WaitingFor(wf));
                }
                return Ok(ResolutionChoiceOutcome::WaitingForWithInlineTriggers(
                    waiting_for,
                ));
            }
            ResolutionChoiceOutcome::WaitingFor(waiting_for)
        }
        (
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                min_count,
                up_to,
                source_id,
                effect_kind,
                zone,
                destination,
                enter_tapped,
                enter_transformed,
                enters_under_player,
                enters_attacking,
                owner_library: _,
                track_exiled_by_source,
                face_down_profile,
                enter_with_counters,
                conditional_enter_with_counters,
                count_param,
                library_position,
                is_cost_payment,
                enters_modified_if,
                duration,
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            let legacy_optional_attach_empty = chosen.is_empty()
                && matches!(effect_kind, EffectKind::Attach)
                && !up_to
                && min_count == 1
                && count == 1
                && state.active_ability_continuation().is_some_and(|cont| {
                    cont.chain.targeting_is_optional()
                        && matches!(&cont.chain.effect, Effect::Attach { .. })
                });

            if up_to {
                if chosen.len() < min_count {
                    return Err(EngineError::InvalidAction(format!(
                        "Must select at least {} card(s), got {}",
                        min_count,
                        chosen.len()
                    )));
                }
                if chosen.len() > count {
                    return Err(EngineError::InvalidAction(format!(
                        "Must select at most {} card(s), got {}",
                        count,
                        chosen.len()
                    )));
                }
            } else if !legacy_optional_attach_empty && chosen.len() != count {
                return Err(EngineError::InvalidAction(format!(
                    "Must select exactly {} card(s), got {}",
                    count,
                    chosen.len()
                )));
            }

            // CR 608.2d: A resolving player can't choose an illegal option.
            // Choosing one eligible card more than once is not a legal
            // selection of distinct cards for this effect.
            let unique_chosen: HashSet<ObjectId> = chosen.iter().copied().collect();
            if unique_chosen.len() != chosen.len() {
                return Err(EngineError::InvalidAction(
                    "Selected cards must be distinct".to_string(),
                ));
            }

            for card_id in &chosen {
                if !cards.contains(card_id) {
                    return Err(EngineError::InvalidAction(
                        "Selected card not in eligible set".to_string(),
                    ));
                }
                // CR 400.7: a multi-origin ChangeZone choice freezes its
                // eligible IDs when the prompt is created; the cards can be
                // in different zones, so there is no single zone to recheck.
                let current_zone = state.objects.get(card_id).map(|obj| obj.zone);
                // `PutAtLibraryPosition` permits either Hand or Library source
                // cards. Partition them by current zone at delivery time.
                let is_put_at_library_position_member =
                    matches!(effect_kind, EffectKind::PutAtLibraryPosition)
                        && matches!(current_zone, Some(Zone::Hand | Zone::Library));
                if !matches!(effect_kind, EffectKind::ChangeZone)
                    && current_zone != Some(zone)
                    && !is_put_at_library_position_member
                {
                    return Err(EngineError::InvalidAction(format!(
                        "Selected card is no longer in {:?}",
                        zone
                    )));
                }
            }

            // CR 614.13a (snapshot lifetime): a *single-pick* `ChangeZone` devour
            // entry paused on its as-enters sacrifice WITHOUT stashing a
            // `pending_change_zone_iteration` (only the mass/targeted loop stashes
            // one). So when this sacrifice resolves and no iteration is pending,
            // the single-pick entry's event is over and the pre-entry Devour
            // snapshot's lifetime ends here — mirroring the synchronous Done-branch
            // `take()` in `change_zone::resolve`. The snapshot only gated the
            // (already-built, already-chosen) eligible pool, so clearing it now
            // cannot unconstrain this devourer's own pool. When an iteration IS
            // pending (mass/targeted co-entry, or a nested move during a mass
            // pause), the snapshot is still needed by the remaining members and is
            // cleared by `drain_pending_change_zone_iteration` instead — so this
            // never over-clears a live mass snapshot. No-op when no Devour is in
            // flight (`snapshot == None`).
            if matches!(effect_kind, EffectKind::Sacrifice)
                && state.active_devour_eligible_snapshot().is_some()
                && state
                    .active_change_zone_frame()
                    .is_some_and(|frame| frame.pending.is_none())
            {
                let _ = state
                    .take_active_change_zone_frame()
                    .expect("completed single Devour entry owns the active ChangeZone frame");
            }

            if matches!(effect_kind, EffectKind::Sacrifice)
                && state.pending_player_scope_sacrifice_choice.is_some()
            {
                // CR 101.4: If multiple players make choices for one
                // instruction, collect those choices before the simultaneous
                // sacrifice action happens.
                let outcome = effects::advance_pending_player_scope_sacrifice_choice(
                    state, player, &chosen, events,
                )
                .map_err(|error| EngineError::InvalidAction(error.to_string()))?;
                match outcome {
                    effects::PendingPlayerScopeSacrificeOutcome::WaitingForNextChoice => {
                        return Ok(ResolutionChoiceOutcome::WaitingFor(
                            state.waiting_for.clone(),
                        ));
                    }
                    effects::PendingPlayerScopeSacrificeOutcome::PausedForReplacement => {
                        return Ok(ResolutionChoiceOutcome::WaitingFor(
                            state.waiting_for.clone(),
                        ));
                    }
                    effects::PendingPlayerScopeSacrificeOutcome::Completed {
                        events_before_sacrifice,
                        events_after_sacrifice,
                        sacrificed_count,
                    } => {
                        effects::stamp_active_player_action_completion(
                            state,
                            source_id,
                            crate::types::ability::EffectResolutionResult {
                                cause: crate::types::ability::ThisWayCause::Sacrificed,
                                count: sacrificed_count,
                            },
                        );
                        // CR 614.12a + CR 614.13a: a direct sacrifice selection can be the
                        // complete body of a paused post-replacement dispatch
                        // (Devour). Retire that exact resident before its outer
                        // ChangeZone iteration resumes; a chained ability
                        // continuation remains its owner.
                        if state.active_ability_continuation().is_none() {
                            state.finish_active_paused_post_replacement_dispatch();
                        }
                        set_priority(state, player);
                        resume_with_error_propagation(state, events)?;
                        if let Some(outcome) = batch_or_drain_observer_triggers(
                            state,
                            events,
                            events_before_sacrifice,
                            events_after_sacrifice,
                            false,
                        ) {
                            return Ok(outcome);
                        }
                        return Ok(ResolutionChoiceOutcome::WaitingFor(
                            state.waiting_for.clone(),
                        ));
                    }
                }
            }

            if chosen.is_empty() && matches!(effect_kind, EffectKind::CastFromZone) {
                // CR 608.2c: An empty selection at a hand-pick cast's
                // `EffectZoneChoice` means the player did not cast ("you may cast
                // a permanent spell … from your hand"). A Kellan-class ability
                // carries a `Not(OptionalEffectPerformed)` decline fallback ("If
                // you don't, put a land onto the battlefield"): re-stash it with
                // the performed flag reset and drain it via resume, rather than
                // discarding the continuation (issue #5945).
                // The typed accessor consumes only the active continuation frame;
                // `stash_declined_cast_fallback` parks its fallback through the
                // same typed continuation authority. A subless Electrodominance-
                // style decline (helper returns false) falls through to the
                // consume-and-no-op path below.
                if let Some(frame) = state
                    .take_active_ability_continuation()
                    .expect("declined cast cannot consume a buried continuation")
                {
                    let ability = *frame.pending.chain;
                    if effects::cast_from_zone::stash_declined_cast_fallback(state, &ability) {
                        state.last_effect_count = Some(0);
                        events.push(GameEvent::EffectResolved {
                            kind: effect_kind,
                            source_id,
                            subject: None,
                        });
                        set_priority(state, player);
                        // CR 608.2c: drain the re-stashed land-drop fallback.
                        // `resume_with_error_propagation` only drains under
                        // `WaitingFor::Priority` (set just above), so ordering
                        // `set_priority` before resume is required and correct.
                        resume_with_error_propagation(state, events)?;
                        return Ok(ResolutionChoiceOutcome::WaitingFor(
                            state.waiting_for.clone(),
                        ));
                    }
                    // No decline fallback: the taken continuation is discarded —
                    // the subless decline consumes it without granting a
                    // permission (below).
                    if zone == Zone::Library {
                        // CR 401.4: declining the one-shot self-library peek
                        // cast bottoms every looked-at card in a random order.
                        // `library_position: None` is guaranteed by the sole
                        // `open_private_zone_cast_selection` producer.
                        let looked_at = effects::cast_from_zone::looked_at_controller_library_cards(
                            state,
                            ability.controller,
                        );
                        if matches!(
                            effects::cascade::shuffle_to_bottom(
                                state, &looked_at, source_id, None, events,
                            ),
                            crate::game::zone_pipeline::BatchMoveResult::NeedsChoice
                        ) {
                            return Ok(ResolutionChoiceOutcome::WaitingFor(
                                state.waiting_for.clone(),
                            ));
                        }
                    }
                }
                // CR 609.1 / CR 601.2a: Declining an optional Electrodominance-
                // style hand cast consumes the stashed CastFromZone continuation
                // without granting a permission. Do not call the generic resume
                // path here; the pending ability would re-open the same optional
                // prompt.
                state.last_effect_count = Some(0);
                events.push(GameEvent::EffectResolved {
                    kind: effect_kind,
                    source_id,
                    subject: None,
                });
                set_priority(state, player);
                super::engine::settle_resolving_stack_entry_after_continuation_resume(state);
                return Ok(ResolutionChoiceOutcome::WaitingFor(
                    state.waiting_for.clone(),
                ));
            }

            if chosen.is_empty() && matches!(effect_kind, EffectKind::Attach) {
                let _ = state
                    .clear_active_ability_continuation()
                    .expect("empty attach choice cannot clear a buried continuation");
                state.last_effect_count = Some(0);
                set_priority(state, player);
                resume_with_error_propagation(state, events)?;
                return Ok(ResolutionChoiceOutcome::WaitingFor(
                    state.waiting_for.clone(),
                ));
            }

            if chosen.is_empty() {
                // Issue #423 audit: no cards chosen — this branch moves no
                // objects and emits no battlefield-exit events, so no
                // dies-trigger collection is needed.
                //
                // CR 603.7: Terminal empty `up_to` must still rebind a fresh
                // empty chain tracked set before the continuation drains, or a
                // following `TargetFilter::TrackedSet` can observe a prior
                // non-empty set. Mid-pause empty publishes stay skipped at the
                // NeedsAura / NeedsChoice call sites (`mid_pause: true`).
                if matches!(
                    effect_kind,
                    EffectKind::Sacrifice
                        | EffectKind::ChangeZone
                        | EffectKind::BounceAll
                        | EffectKind::Tap
                        | EffectKind::Untap
                        | EffectKind::PutAtLibraryPosition
                        | EffectKind::CastFromZone
                ) && state.active_ability_continuation().is_some()
                {
                    publish_effect_zone_choice_tracked_set(
                        state,
                        effect_kind,
                        &[],
                        library_position,
                        false,
                    );
                }
                state.last_effect_count = Some(0);
                events.push(GameEvent::EffectResolved {
                    kind: effect_kind,
                    source_id,
                    subject: None,
                });
                let result = match effect_kind {
                    EffectKind::Sacrifice => Some(crate::types::ability::EffectResolutionResult {
                        cause: crate::types::ability::ThisWayCause::Sacrificed,
                        count: 0,
                    }),
                    EffectKind::ChangeZone => destination
                        .and_then(effects::this_way_cause_for_zone)
                        .map(|cause| crate::types::ability::EffectResolutionResult {
                            cause,
                            count: 0,
                        }),
                    _ => None,
                };
                if let Some(result) = result {
                    effects::stamp_active_player_action_completion(state, source_id, result);
                }
                set_priority(state, player);
                resume_with_error_propagation(state, events)?;
                return Ok(ResolutionChoiceOutcome::WaitingFor(
                    state.waiting_for.clone(),
                ));
            }

            let events_before_effect = events.len();
            match effect_kind {
                EffectKind::Sacrifice => {
                    let completion = PendingPlayerScopeSacrificeCompletion {
                        effect_kind: Some(EffectKind::Sacrifice),
                        publish_fresh_tracked_set: state.active_ability_continuation().is_some(),
                        propagate_parent_context: true,
                        ..Default::default()
                    };
                    match effects::perform_collected_player_scope_sacrifices_with_completion(
                        state,
                        source_id,
                        player,
                        vec![(player, chosen.clone())],
                        completion,
                        events,
                    )
                    .map_err(|error| EngineError::InvalidAction(error.to_string()))?
                    {
                        effects::PendingPlayerScopeSacrificeOutcome::WaitingForNextChoice => {
                            unreachable!(
                                "collected effect-zone sacrifices never prompt a new player"
                            )
                        }
                        effects::PendingPlayerScopeSacrificeOutcome::PausedForReplacement => {
                            return Ok(action_result_outcome(events, state.waiting_for.clone()));
                        }
                        effects::PendingPlayerScopeSacrificeOutcome::Completed {
                            events_before_sacrifice,
                            events_after_sacrifice,
                            sacrificed_count,
                        } => {
                            effects::stamp_active_player_action_completion(
                                state,
                                source_id,
                                crate::types::ability::EffectResolutionResult {
                                    cause: crate::types::ability::ThisWayCause::Sacrificed,
                                    count: sacrificed_count,
                                },
                            );
                            // CR 614.12a + CR 614.13a: see the matching player-scope
                            // sacrifice completion above. This EffectZoneChoice
                            // path can complete a Devour drain without an
                            // ability continuation.
                            if state.active_ability_continuation().is_none() {
                                state.finish_active_paused_post_replacement_dispatch();
                            }
                            // CR 608.2c + CR 701.21a: Singular sacrificed referent
                            // for a chained Demonstrative / CostPaidObject consumer
                            // is stamped once at the sacrifice-completion seam
                            // (`perform_player_scope_sacrifices` when
                            // `propagate_parent_context` is set). Do not re-scan
                            // here — a second authority drifts when the snapshot
                            // ladder changes (issue #5925).
                            set_priority(state, player);
                            resume_with_error_propagation(state, events)?;
                            if let Some(outcome) = batch_or_drain_observer_triggers(
                                state,
                                events,
                                events_before_sacrifice,
                                events_after_sacrifice,
                                false,
                            ) {
                                return Ok(outcome);
                            }
                            return Ok(ResolutionChoiceOutcome::WaitingFor(
                                state.waiting_for.clone(),
                            ));
                        }
                    }
                }
                EffectKind::ChangeZone | EffectKind::BounceAll => {
                    let dest_zone = destination.ok_or_else(|| {
                        EngineError::InvalidAction(
                            "EffectZoneChoice missing destination for zone move".to_string(),
                        )
                    })?;
                    let chosen_ids: Vec<_> = chosen.to_vec();
                    let completion_cause = effects::this_way_cause_for_zone(dest_zone);
                    let tracks_player_action_completion = completion_cause.is_some_and(|cause| {
                        effects::active_player_action_completion_requires(state, source_id, cause)
                    });
                    let mut logical_zone_change_group =
                        crate::game::triggers::allocate_logical_zone_change_group(
                            state,
                            &chosen_ids,
                        );
                    let logical_group_event_start = events.len();
                    for (i, card_id) in chosen_ids.iter().enumerate() {
                        let origin = state
                            .objects
                            .get(card_id)
                            .map(|object| object.zone)
                            .unwrap_or(zone);
                        let per_obj_enter_counters =
                            effects::change_zone::enter_with_counters_for_pending_object(
                                state,
                                source_id,
                                *card_id,
                                &enter_with_counters,
                                &conditional_enter_with_counters,
                            );
                        let ctx = effects::change_zone::ChangeZoneIterationCtx {
                            source_id,
                            controller: player,
                            origin: Some(origin),
                            destination: dest_zone,
                            enter_transformed,
                            enter_tapped,
                            enters_under_player,
                            enters_attacking,
                            enter_with_counters: per_obj_enter_counters,
                            conditional_enter_with_counters: vec![],
                            // CR 611.2a + CR 610.3: the duration carried across
                            // the `EffectZoneChoice` round-trip — an
                            // "exile ... until ~ leaves the battlefield" move
                            // must keep its bound on the interactive
                            // multi-candidate path, not just the
                            // single-candidate shortcut (issue #4235 review).
                            duration: duration.clone(),
                            track_exiled_by_source,
                            // CR 708.2a + CR 708.3: thread the face-down profile that
                            // was carried across the `EffectZoneChoice` round-trip into
                            // the move ctx, so a selected face-down `ChangeZone` card
                            // (Yedora-style return paused for selection) enters FACE
                            // DOWN with the specified characteristics instead of
                            // resuming face up and exposing the real object.
                            face_down_profile: face_down_profile.clone(),
                            library_placement: None,
                            // CR 614.12: evaluate the moved-object type gate carried
                            // across the `EffectZoneChoice` round-trip against each
                            // chosen object (Summoner's Grimoire).
                            enters_modified_if: enters_modified_if.clone(),
                            enter_attached_to: None,
                        };
                        let anticipated_pause =
                            effects::change_zone::anticipated_zone_change_delivery(
                                state,
                                *card_id,
                                ctx.destination,
                                ctx.source_id,
                            );
                        let delivery_start = events.len();
                        match effects::change_zone::process_one_zone_move_with_terminal(
                            state, &ctx, *card_id, events,
                        ) {
                            crate::game::zone_pipeline::ZoneMoveTerminalResult::Completed(completion) => {
                                logical_zone_change_group
                                    .record_delivery_completion(*card_id, completion)
                                    .expect("EffectZoneChoice member records its exact terminal outcome");
                                // CR 118.3: When this is a cost-payment exile (e.g., Mimeoplasm),
                                // populate the exile-link index map so the continuation can
                                // reference exiled cards by position (ExiledCardByIndex, ExiledCardPower).
                                if is_cost_payment && dest_zone == Zone::Exile {
                                    super::exile_links::push_exiled_with_source_this_turn(
                                        state, *card_id, source_id,
                                    );
                                }
                            }
                            crate::game::zone_pipeline::ZoneMoveTerminalResult::NeedsAuraAttachmentChoice => {
                                // CR 608.2c + CR 603.7 + CR 303.4f: Publish the
                                // selection before pausing for Aura host choice —
                                // this early return skips the terminal publish
                                // below (Storm Herald "Exile those Auras").
                                publish_effect_zone_choice_tracked_set(
                                    state,
                                    effect_kind,
                                    &chosen_ids,
                                    library_position,
                                    true,
                                );
                                crate::game::triggers::append_and_collect_logical_zone_trigger_segment(
                                    state,
                                    &mut logical_zone_change_group,
                                    &events[logical_group_event_start..],
                                )
                                .expect("paused EffectZoneChoice retains its explicit delivery prefix");
                                state.push_change_zone_iteration(
                                    crate::types::game_state::PendingChangeZoneIteration {
                                        logical_zone_change_group,
                                        paused_current: anticipated_pause.map(|mut boundary| {
                                            boundary
                                                .append_delivery_events(&events[delivery_start..]);
                                            boundary.mark_counted();
                                            boundary
                                        }),
                                        remaining: chosen_ids[i + 1..].to_vec(),
                                        source_id: ctx.source_id,
                                        controller: ctx.controller,
                                        // EffectZoneChoice can select across multiple
                                        // origins (for example, hand and graveyard).
                                        // The paused object's origin must not become
                                        // a gate for the remaining selected cards.
                                        origin: None,
                                        destination: ctx.destination,
                                        enter_transformed: ctx.enter_transformed,
                                        enter_tapped: ctx.enter_tapped,
                                        enters_under_player: ctx.enters_under_player,
                                        enters_attacking: ctx.enters_attacking,
                                        enter_with_counters: enter_with_counters.clone(),
                                        conditional_enter_with_counters:
                                            conditional_enter_with_counters.clone(),
                                        duration: ctx.duration.clone(),
                                        track_exiled_by_source: ctx.track_exiled_by_source,
                                        moved_count: tracks_player_action_completion.then(|| {
                                                i32::try_from(
                                                    effects::change_zone::count_selected_zone_arrivals(
                                                        &events[events_before_effect..],
                                                        &chosen_ids,
                                                        dest_zone,
                                                    ),
                                                )
                                                .expect("selected zone arrivals fit in i32")
                                            }),
                                        // CR 708.2a + CR 708.3: preserve the
                                        // face-down profile across a further pause.
                                        face_down_profile: ctx.face_down_profile.clone(),
                                        library_placement: ctx.library_placement.clone(),
                                        // CR 614.12: preserve the moved-object type
                                        // gate across a further as-enters pause.
                                        enters_modified_if: ctx.enters_modified_if.clone(),
                                        enter_attached_to: None,
                                        effect_kind,
                                    },
                                );
                                return Ok(action_result_outcome(
                                    events,
                                    state.waiting_for.clone(),
                                ));
                            }
                            crate::game::zone_pipeline::ZoneMoveTerminalResult::NeedsChoice(choice_player) => {
                                // CR 614.12b + CR 614.1c + CR 614.13: stash the
                                // unprocessed cards so the drain in
                                // `effects/mod.rs::drain_pending_change_zone_iteration`
                                // resumes the loop after this replacement
                                // choice resolves (issue #535).
                                // CR 608.2c + CR 603.7: Publish selection before
                                // the replacement pause — same early-return gap
                                // as NeedsAuraAttachmentChoice above.
                                publish_effect_zone_choice_tracked_set(
                                    state,
                                    effect_kind,
                                    &chosen_ids,
                                    library_position,
                                    true,
                                );
                                crate::game::triggers::append_and_collect_logical_zone_trigger_segment(
                                    state,
                                    &mut logical_zone_change_group,
                                    &events[logical_group_event_start..],
                                )
                                .expect("paused EffectZoneChoice retains its explicit delivery prefix");
                                state.push_change_zone_iteration(
                                    crate::types::game_state::PendingChangeZoneIteration {
                                        logical_zone_change_group,
                                        paused_current: Some(
                                            state
                                                .pending_zone_change_delivery_from_replacement()
                                                .or_else(|| {
                                                    anticipated_pause.map(|mut boundary| {
                                                        boundary.append_delivery_events(
                                                            &events[delivery_start..],
                                                        );
                                                        boundary
                                                    })
                                                })
                                                .expect("zone-change pause must retain its exact boundary"),
                                        ),
                                        remaining: chosen_ids[i + 1..].to_vec(),
                                        source_id: ctx.source_id,
                                        controller: ctx.controller,
                                        // EffectZoneChoice can select across multiple
                                        // origins (for example, hand and graveyard).
                                        // The paused object's origin must not become
                                        // a gate for the remaining selected cards.
                                        origin: None,
                                        destination: ctx.destination,
                                        enter_transformed: ctx.enter_transformed,
                                        enter_tapped: ctx.enter_tapped,
                                        enters_under_player: ctx.enters_under_player,
                                        enters_attacking: ctx.enters_attacking,
                                        enter_with_counters: enter_with_counters.clone(),
                                        conditional_enter_with_counters:
                                            conditional_enter_with_counters.clone(),
                                        duration: ctx.duration.clone(),
                                        track_exiled_by_source: ctx.track_exiled_by_source,
                                        moved_count: tracks_player_action_completion.then(|| {
                                                i32::try_from(
                                                    effects::change_zone::count_selected_zone_arrivals(
                                                        &events[events_before_effect..],
                                                        &chosen_ids,
                                                        dest_zone,
                                                    ),
                                                )
                                                .expect("selected zone arrivals fit in i32")
                                            }),
                                        // CR 708.2a + CR 708.3: preserve the
                                        // face-down profile across a further pause.
                                        face_down_profile: ctx.face_down_profile.clone(),
                                        library_placement: ctx.library_placement.clone(),
                                        // CR 614.12: preserve the moved-object type
                                        // gate across a further as-enters pause.
                                        enters_modified_if: ctx.enters_modified_if.clone(),
                                        enter_attached_to: None,
                                        effect_kind,
                                    },
                                );
                                state.waiting_for =
                                    super::replacement::replacement_choice_waiting_for(
                                        choice_player,
                                        state,
                                    );
                                return Ok(action_result_outcome(
                                    events,
                                    state.waiting_for.clone(),
                                ));
                            }
                        }
                    }
                    crate::game::triggers::complete_logical_zone_trigger_collection(
                        state,
                        &mut logical_zone_change_group,
                        &mut events[logical_group_event_start..],
                    )
                    .expect("completed EffectZoneChoice owns every terminal member outcome");
                }
                EffectKind::Tap => {
                    for &card_id in &chosen {
                        match effects::tap_untap::process_one_tap(state, card_id, source_id, events)
                        {
                            Ok(effects::tap_untap::TapUntapOutcome::Complete) => {}
                            Ok(effects::tap_untap::TapUntapOutcome::NeedsChoice(choice_player)) => {
                                state.waiting_for =
                                    super::replacement::replacement_choice_waiting_for(
                                        choice_player,
                                        state,
                                    );
                                return Ok(action_result_outcome(
                                    events,
                                    state.waiting_for.clone(),
                                ));
                            }
                            Err(error) => {
                                return Err(EngineError::InvalidAction(error.to_string()));
                            }
                        }
                    }
                }
                EffectKind::Untap => {
                    for &card_id in &chosen {
                        match effects::tap_untap::process_one_untap(state, card_id, events) {
                            Ok(effects::tap_untap::TapUntapOutcome::Complete) => {}
                            Ok(effects::tap_untap::TapUntapOutcome::NeedsChoice(choice_player)) => {
                                state.waiting_for =
                                    super::replacement::replacement_choice_waiting_for(
                                        choice_player,
                                        state,
                                    );
                                return Ok(action_result_outcome(
                                    events,
                                    state.waiting_for.clone(),
                                ));
                            }
                            Err(error) => {
                                return Err(EngineError::InvalidAction(error.to_string()));
                            }
                        }
                    }
                }
                // CR 115.1: Resolution-time selection for PutAtLibraryPosition
                // from a private zone (e.g. Brainstorm's "put two cards from
                // your hand on top of your library"). Cards are placed in
                // selection order (first chosen = top). Expressive Iteration's
                // tracked-set bottom step chains an exile `ParentTarget` tail —
                // detect that continuation shape to honor bottom placement.
                EffectKind::PutAtLibraryPosition => {
                    let library_position = match library_position {
                        Some(LibraryPosition::Bottom) => LibraryPosition::Bottom,
                        Some(LibraryPosition::NthFromTop { n }) => {
                            LibraryPosition::NthFromTop { n }
                        }
                        _ => LibraryPosition::Top,
                    };
                    let library_origin: Vec<ObjectId> = chosen
                        .iter()
                        .copied()
                        .filter(|card_id| {
                            state
                                .objects
                                .get(card_id)
                                .is_some_and(|object| object.zone == Zone::Library)
                        })
                        .collect();
                    let non_library_order = effect_zone_non_library_delivery_order(
                        state,
                        &chosen,
                        &library_origin,
                        &library_position,
                    );

                    if non_library_order.is_empty() {
                        move_library_origin_cards_in_selection_order(
                            state,
                            &chosen,
                            &library_position,
                            events,
                        );
                        if let Some(next_owner) =
                            effects::change_zone::resume_next_mass_library_order_choice(state)
                        {
                            state.priority_player = next_owner;
                            return Ok(ResolutionChoiceOutcome::WaitingFor(
                                state.waiting_for.clone(),
                            ));
                        }
                    } else {
                        // The selected EffectZoneChoice is now consumed. Clear it
                        // before the pipeline may park a CR 616.1 prompt; otherwise
                        // `park_waiting_for` intentionally preserves the stale
                        // choice for Devour's nested-choice path.
                        state.waiting_for = WaitingFor::Priority { player };
                        let requests = non_library_order
                            .into_iter()
                            .map(|card_id| {
                                crate::game::zone_pipeline::ZoneMoveRequest::effect(
                                    card_id,
                                    Zone::Library,
                                    source_id,
                                )
                                .at_library_position(library_position.clone())
                            })
                            .collect();
                        let completion =
                            crate::types::game_state::BatchCompletion::EffectZonePutAtLibraryPositionComplete {
                                player,
                                source_id,
                                chosen: chosen.clone(),
                                library_origin,
                                library_position,
                            };
                        match crate::game::zone_pipeline::move_objects_simultaneously_then(
                            state,
                            requests,
                            Some(completion),
                            events,
                        ) {
                            crate::game::zone_pipeline::BatchMoveResult::Done
                            | crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => {
                                return Ok(ResolutionChoiceOutcome::WaitingFor(
                                    state.waiting_for.clone(),
                                ));
                            }
                        }
                    }
                }
                // CR 608.2d + CR 301.5b: Resolution-time Equipment pick for
                // deferred optional attach (Nahiri, the Lithomancer +2).
                EffectKind::Attach => {
                    let Some(frame) = state
                        .take_active_ability_continuation()
                        .expect("attach choice cannot consume a buried continuation")
                    else {
                        return Err(EngineError::InvalidAction(
                            "Attach EffectZoneChoice missing stashed ability".to_string(),
                        ));
                    };
                    effects::attach::complete_resolution_attachment_choice(
                        &mut *state,
                        *frame.pending.chain,
                        &chosen,
                        events,
                    )
                    .map_err(|e| EngineError::InvalidAction(e.to_string()))?;
                    set_priority(state, player);
                    resume_with_error_propagation(state, events)?;
                    return Ok(ResolutionChoiceOutcome::WaitingFor(
                        state.waiting_for.clone(),
                    ));
                }
                // CR 601.2c + CR 115.1: Resolution-time hand pick for
                // `CastFromZone` (Electrodominance, Baral's Expertise).
                EffectKind::CastFromZone => {
                    let Some(frame) = state
                        .take_active_ability_continuation()
                        .expect("cast choice cannot consume a buried continuation")
                    else {
                        return Err(EngineError::InvalidAction(
                            "CastFromZone EffectZoneChoice missing stashed ability".to_string(),
                        ));
                    };
                    let ability = *frame.pending.chain;
                    if chosen.len() == 1 {
                        let used_during_resolution =
                            effects::cast_from_zone::complete_hand_pick_cast_from_zone(
                                &mut *state,
                                &ability,
                                chosen[0],
                                events,
                            )
                            .map_err(|e| EngineError::InvalidAction(e.to_string()))?;
                        if used_during_resolution {
                            return Ok(ResolutionChoiceOutcome::WaitingFor(
                                state.waiting_for.clone(),
                            ));
                        }
                    } else {
                        let permission_grant =
                            effects::cast_from_zone::grant_lingering_permissions(
                                &mut *state,
                                &ability,
                                &chosen,
                                events,
                            )
                            .map_err(|e| EngineError::InvalidAction(e.to_string()))?;
                        if matches!(
                            permission_grant,
                            effects::cast_from_zone::LingeringPermissionGrantResult::NeedsChoice
                        ) {
                            return Ok(ResolutionChoiceOutcome::WaitingFor(
                                state.waiting_for.clone(),
                            ));
                        }
                    }
                }
                // CR 701.68a: Place `count_param` -1/-1 counters on the creature
                // the controller chose. The choice is non-targeted; the pool was
                // restricted to the controller's creatures in `blight::resolve`,
                // with `count = 1`, `min_count = 1`, `up_to = false` — so `chosen`
                // holds exactly one creature.
                // CR 614.1 / CR 614.1a: route through `add_counter_with_replacement`
                // so counter-doubling/modifying replacement effects apply.
                EffectKind::BlightEffect => {
                    let blighted = chosen[0];
                    // CR 701.68c: Snapshot the chosen creature before the
                    // counter-placement replacement pipeline can pause, so
                    // "the creature you blighted" remains available when the
                    // continuation resumes.
                    if let Some(obj) = state.objects.get(&blighted) {
                        let snapshot = crate::types::ability::CostPaidObjectSnapshot {
                            object_id: blighted,
                            lki: obj.snapshot_for_mana_spent(),
                        };
                        if let Some(frame) = state.active_ability_continuation_frame_mut() {
                            frame
                                .pending
                                .chain
                                .set_effect_context_object_recursive(snapshot);
                        }
                    }
                    if count_param > 0
                        && !effects::counters::add_counter_with_replacement(
                            state,
                            player,
                            blighted,
                            crate::types::counter::CounterType::Minus1Minus1,
                            count_param,
                            events,
                        )
                    {
                        effects::counters::stash_pending_counter_completion(
                            state,
                            effect_kind,
                            source_id,
                        );
                        return Ok(ResolutionChoiceOutcome::WaitingFor(
                            state.waiting_for.clone(),
                        ));
                    }
                }
                // CR 118.3: Cost-payment exile (e.g., Mimeoplasm) uses the same
                // zone-move logic as ChangeZone, but with is_cost_payment always true.
                EffectKind::PayCost => {
                    let dest_zone = destination.ok_or_else(|| {
                        EngineError::InvalidAction(
                            "EffectZoneChoice missing destination for cost payment".to_string(),
                        )
                    })?;
                    let ctx = effects::change_zone::ChangeZoneIterationCtx {
                        source_id,
                        controller: player,
                        origin: Some(zone),
                        destination: dest_zone,
                        enter_transformed,
                        enter_tapped,
                        enters_under_player,
                        enters_attacking,
                        enter_with_counters: vec![],
                        conditional_enter_with_counters: vec![],
                        // CR 118.3: cost-payment exile is unbounded — no
                        // "until ..." duration idiom pays a cost, so the
                        // round-trip `duration` (always `None` for `PayCost`
                        // producers) is deliberately not threaded here.
                        duration: None,
                        track_exiled_by_source,
                        face_down_profile: face_down_profile.clone(),
                        library_placement: None,
                        // CR 614.12: cost-payment exile carries no enter-modifier
                        // gate; thread the (None) round-trip value for consistency.
                        enters_modified_if: enters_modified_if.clone(),
                        enter_attached_to: None,
                    };
                    let events_before_effect = events.len();
                    let chosen_ids: Vec<_> = chosen.to_vec();
                    let mut logical_zone_change_group =
                        crate::game::triggers::allocate_logical_zone_change_group(
                            state,
                            &chosen_ids,
                        );
                    let logical_group_event_start = events.len();
                    for (i, card_id) in chosen_ids.iter().enumerate() {
                        let anticipated_pause =
                            effects::change_zone::anticipated_zone_change_delivery(
                                state,
                                *card_id,
                                ctx.destination,
                                ctx.source_id,
                            );
                        let delivery_start = events.len();
                        match effects::change_zone::process_one_zone_move_with_terminal(
                            state, &ctx, *card_id, events,
                        ) {
                            crate::game::zone_pipeline::ZoneMoveTerminalResult::Completed(completion) => {
                                logical_zone_change_group
                                    .record_delivery_completion(*card_id, completion)
                                    .expect("cost-payment zone move records its exact terminal outcome");
                                // CR 118.3: Populate the exile-link index map for cost-payment exile
                                if dest_zone == Zone::Exile {
                                    super::exile_links::push_exiled_with_source_this_turn(
                                        state, *card_id, source_id,
                                    );
                                }
                            }
                            crate::game::zone_pipeline::ZoneMoveTerminalResult::NeedsAuraAttachmentChoice => {
                                crate::game::triggers::append_and_collect_logical_zone_trigger_segment(
                                    state,
                                    &mut logical_zone_change_group,
                                    &events[logical_group_event_start..],
                                )
                                .expect("paused cost-payment zone move retains its explicit delivery prefix");
                                state.push_change_zone_iteration(
                                    crate::types::game_state::PendingChangeZoneIteration {
                                        logical_zone_change_group,
                                        paused_current: anticipated_pause.map(|mut boundary| {
                                            boundary
                                                .append_delivery_events(&events[delivery_start..]);
                                            boundary.mark_counted();
                                            boundary
                                        }),
                                        remaining: chosen_ids[i + 1..].to_vec(),
                                        source_id: ctx.source_id,
                                        controller: ctx.controller,
                                        origin: ctx.origin,
                                        destination: ctx.destination,
                                        enter_transformed: ctx.enter_transformed,
                                        enter_tapped: ctx.enter_tapped,
                                        enters_under_player: ctx.enters_under_player,
                                        enters_attacking: ctx.enters_attacking,
                                        enter_with_counters: enter_with_counters.clone(),
                                        conditional_enter_with_counters:
                                            conditional_enter_with_counters.clone(),
                                        duration: ctx.duration.clone(),
                                        track_exiled_by_source: ctx.track_exiled_by_source,
                                        moved_count: None,
                                        face_down_profile: ctx.face_down_profile.clone(),
                                        library_placement: ctx.library_placement.clone(),
                                        // CR 614.12: preserve the moved-object type
                                        // gate across a further as-enters pause.
                                        enters_modified_if: ctx.enters_modified_if.clone(),
                                        enter_attached_to: None,
                                        effect_kind,
                                    },
                                );
                                state.waiting_for =
                                    super::replacement::replacement_choice_waiting_for(
                                        player, state,
                                    );
                                return Ok(action_result_outcome(
                                    events,
                                    state.waiting_for.clone(),
                                ));
                            }
                            crate::game::zone_pipeline::ZoneMoveTerminalResult::NeedsChoice(choice_player) => {
                                crate::game::triggers::append_and_collect_logical_zone_trigger_segment(
                                    state,
                                    &mut logical_zone_change_group,
                                    &events[logical_group_event_start..],
                                )
                                .expect("paused cost-payment zone move retains its explicit delivery prefix");
                                state.push_change_zone_iteration(
                                    crate::types::game_state::PendingChangeZoneIteration {
                                        logical_zone_change_group,
                                        paused_current: Some(
                                            state
                                                .pending_zone_change_delivery_from_replacement()
                                                .or_else(|| {
                                                    anticipated_pause.map(|mut boundary| {
                                                        boundary.append_delivery_events(
                                                            &events[delivery_start..],
                                                        );
                                                        boundary
                                                    })
                                                })
                                                .expect("zone-change pause must retain its exact boundary"),
                                        ),
                                        remaining: chosen_ids[i + 1..].to_vec(),
                                        source_id: ctx.source_id,
                                        controller: ctx.controller,
                                        origin: ctx.origin,
                                        destination: ctx.destination,
                                        enter_transformed: ctx.enter_transformed,
                                        enter_tapped: ctx.enter_tapped,
                                        enters_under_player: ctx.enters_under_player,
                                        enters_attacking: ctx.enters_attacking,
                                        enter_with_counters: enter_with_counters.clone(),
                                        conditional_enter_with_counters:
                                            conditional_enter_with_counters.clone(),
                                        duration: ctx.duration.clone(),
                                        track_exiled_by_source: ctx.track_exiled_by_source,
                                        moved_count: None,
                                        face_down_profile: ctx.face_down_profile.clone(),
                                        library_placement: ctx.library_placement.clone(),
                                        // CR 614.12: preserve the moved-object type
                                        // gate across a further as-enters pause.
                                        enters_modified_if: ctx.enters_modified_if.clone(),
                                        enter_attached_to: None,
                                        effect_kind,
                                    },
                                );
                                state.waiting_for =
                                    super::replacement::replacement_choice_waiting_for(
                                        choice_player,
                                        state,
                                    );
                                return Ok(action_result_outcome(
                                    events,
                                    state.waiting_for.clone(),
                                ));
                            }
                        }
                    }
                    crate::game::triggers::complete_logical_zone_trigger_collection(
                        state,
                        &mut logical_zone_change_group,
                        &mut events[logical_group_event_start..],
                    )
                    .expect("completed cost-payment zone move owns every terminal member outcome");
                    let events_after_move = events.len();
                    // CR 614.12a: this `EffectZoneChoice` was the interactive payment of an
                    // optional `MayCost` replacement's accept (e.g. Mimeoplasm's
                    // "exile two creature cards from graveyards"). The cost is
                    // now paid, so resume the parked replacement with the accept index —
                    // `continue_replacement` sees `may_cost_paid: true`, pays any
                    // `may_cost_remaining`, and finishes entering the permanent.
                    if state
                        .pending_replacement
                        .as_ref()
                        .is_some_and(|pending| pending.may_cost_paid)
                    {
                        let waiting_for =
                            super::engine_replacement::handle_replacement_choice(state, 0, events)?;
                        if let Some(outcome) = batch_or_drain_observer_triggers(
                            state,
                            events,
                            events_before_effect,
                            events_after_move,
                            true,
                        ) {
                            return Ok(outcome);
                        }
                        return Ok(ResolutionChoiceOutcome::WaitingFor(waiting_for));
                    }
                }
                other => {
                    return Err(EngineError::InvalidAction(format!(
                        "EffectZoneChoice unsupported for {other:?}"
                    )));
                }
            }

            if let Some(snapshot) =
                effects::parent_referent_context_from_events(state, &events[events_before_effect..])
            {
                if let Some(frame) = state.active_ability_continuation_frame_mut() {
                    frame
                        .pending
                        .chain
                        .set_effect_context_object_recursive(snapshot);
                }
            }
            if matches!(
                effect_kind,
                EffectKind::Sacrifice
                    | EffectKind::ChangeZone
                    | EffectKind::BounceAll
                    | EffectKind::Tap
                    | EffectKind::Untap
                    | EffectKind::PutAtLibraryPosition
                    | EffectKind::CastFromZone
            ) && state.active_ability_continuation().is_some()
            {
                let tracked = if matches!(effect_kind, EffectKind::Sacrifice) {
                    events[events_before_effect..]
                        .iter()
                        .filter_map(|event| match event {
                            GameEvent::PermanentSacrificed { object_id, .. } => Some(*object_id),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                } else {
                    chosen.clone()
                };
                publish_effect_zone_choice_tracked_set(
                    state,
                    effect_kind,
                    &tracked,
                    library_position,
                    false,
                );
            }
            if matches!(effect_kind, EffectKind::ChangeZone) {
                if let Some(cause) = destination.and_then(effects::this_way_cause_for_zone) {
                    let count = effects::change_zone::count_selected_zone_arrivals(
                        &events[events_before_effect..],
                        &chosen,
                        destination.expect("ChangeZone destination checked above"),
                    );
                    effects::stamp_active_player_action_completion(
                        state,
                        source_id,
                        crate::types::ability::EffectResolutionResult { cause, count },
                    );
                }
            }
            state.last_effect_count = Some(chosen.len() as i32);
            events.push(GameEvent::EffectResolved {
                kind: effect_kind,
                source_id,
                subject: None,
            });
            // Mark the end of the battlefield-exit events produced by this
            // handler (Sacrifice / ChangeZone / BounceAll) — the slice
            // `events[events_before_effect..events_after_move]` is the exact
            // set of dies-events whose triggers issue #423 must not lose.
            let events_after_move = events.len();

            // Step B: resolve the reflexive `WhenYouDo` continuation (Grist's
            // `[-2]`). `waiting_for` is still `Priority` here, so
            // `resume_with_error_propagation`'s guard passes and
            // `drain_pending_continuation` runs.
            set_priority(state, player);
            resume_with_error_propagation(state, events)?;

            // CR 603.2 + CR 603.3b: Issue #423 — dispatch the dies-triggers
            // produced by this handler's permanent move (Undying CR 702.93a,
            // Blood Artist-class observers). `PutAtLibraryPosition` moves cards
            // within library/hand and emits no battlefield-exit events.
            let moves_permanents = matches!(
                effect_kind,
                EffectKind::Sacrifice | EffectKind::ChangeZone | EffectKind::BounceAll
            );
            if moves_permanents {
                if matches!(effect_kind, EffectKind::Sacrifice) {
                    // CR 603.10a: the chosen permanents left the battlefield together
                    // in this single resolution event, so co-departing
                    // leaves-the-battlefield observers among them (Blood Artist among
                    // the sacrificed group) observe each other. Stamp only the
                    // sub-slice this handler produced — never the whole events vector —
                    // so earlier sequential departures in this resolution aren't grouped
                    // with these.
                    super::zones::mark_simultaneous_departures(
                        &mut events[events_before_effect..events_after_move],
                        &super::zones::departed_subset(state, &chosen),
                    );
                }
                if let Some(outcome) = batch_or_drain_observer_triggers(
                    state,
                    events,
                    events_before_effect,
                    events_after_move,
                    true,
                ) {
                    return Ok(outcome);
                }
            }
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        (
            WaitingFor::DrawnThisTurnTopdeckChoice {
                player,
                cards,
                count,
                min_count,
                life_payment,
                source_id,
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            match effects::drawn_this_turn_choice::handle_topdeck_choice(
                state,
                effects::drawn_this_turn_choice::TopdeckChoice {
                    player,
                    eligible: &cards,
                    count,
                    min_count,
                    life_payment,
                    source_id,
                    chosen_to_topdeck: &chosen,
                },
                events,
            )
            .map_err(|error| EngineError::InvalidAction(error.to_string()))?
            {
                crate::game::zone_pipeline::BatchMoveResult::Done => {}
                crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => {
                    return Ok(ResolutionChoiceOutcome::WaitingFor(
                        state.waiting_for.clone(),
                    ));
                }
            }
            // Issue #423 audit: `handle_topdeck_choice` moves cards between the
            // hand and the top of the library — never off the battlefield — so
            // it produces no dies-triggers and needs no collection here.
            set_priority(state, player);
            resume_with_error_propagation(state, events)?;
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        (
            WaitingFor::NamedChoice {
                player,
                options,
                choice_type,
                mut source,
                persist_player,
                // MUST stay a wildcard. The published contract is a projection of
                // `choice_type`, and validation below consults that single
                // authority directly (`accepts_free_entry_answer`) — so a client
                // cannot widen its own domain by echoing back a different one.
                // Binding this to a literal instead would make the arm miss every
                // prompt that HAS a contract, i.e. every free-entry answer would
                // fall through to "action not allowed".
                free_entry: _,
            },
            GameAction::ChooseOption { choice },
        ) => {
            if matches!(choice_type, ChoiceType::CardName) {
                let lower = choice.to_lowercase();
                if !state
                    .all_card_names
                    .iter()
                    .any(|name| name.to_lowercase() == lower)
                {
                    return Err(EngineError::InvalidAction(format!(
                        "Invalid card name '{}'",
                        choice
                    )));
                }
            } else if let Some(accepted) = choice_type.accepts_free_entry_answer(&choice) {
                // CR 107.1a/b + CR 608.2d: a free-entry choice has no option list
                // to check membership against, so it is validated by RULE instead
                // — "a number 0 or greater" accepts any nonnegative integer the
                // engine's `i32` quantity domain can represent. Routed through the
                // shared authority on `ChoiceType` so the AI's legal-action
                // enumeration cannot disagree with this seam about what is legal.
                if !accepted {
                    return Err(EngineError::InvalidAction(format!(
                        "Invalid number '{choice}' for this choice"
                    )));
                }
            } else if !options.contains(&choice) {
                return Err(EngineError::InvalidAction(format!(
                    "Invalid choice '{}', must be one of: {:?}",
                    choice, options
                )));
            }
            if source
                .as_ref()
                .is_some_and(|source| !source.has_matching_context())
            {
                return Err(EngineError::InvalidAction(
                    "NamedChoice has incoherent source authority".to_string(),
                ));
            }

            // CR 607.2d + CR 613.1: Persist the chosen attribute on the source
            // (Morophon buffs, Pithing Needle prohibitions, Serra's Emissary
            // protection, Sewer Nemesis CDA, …), recompute layers for the
            // layer-affecting choice kinds, and record `last_named_choice`.
            // Single authority shared with the random `Effect::Choose` resolver.
            let source_id = source
                .as_ref()
                .map(|source| source.prompt.identity.reference.object_id);
            let updated_context = effects::choose::bind_named_choice(
                state,
                &choice_type,
                &choice,
                source.as_mut(),
                persist_player,
            );
            // CR 101.4 + CR 608.2d: additionally record a chosen NUMBER on the
            // player who chose it, so a later clause can read every player's
            // answer back ("the highest number", "each player who didn't choose
            // the lowest number"). Additive to the source binding above.
            effects::choose::record_player_chosen_number(state, player, &choice_type, &choice);
            if let Some(context) = updated_context {
                if let Some(frame) = state.active_ability_continuation_frame_mut() {
                    frame
                        .pending
                        .chain
                        .update_trigger_source_context_in_resolution_segment(context);
                }
            }
            if choice_type.is_card_predicate_guess() {
                events.push(GameEvent::CardPredicateGuessMade {
                    player_id: player,
                    source_id,
                    choice: choice.clone(),
                });
            }

            // CR 608.2c + CR 109.4: A `Choose(Player)`/`Choose(Opponent)`
            // answer binds a resolution-scoped chosen player. Append it to the
            // pending continuation chain's `chosen_players` so the dependent
            // effect (`ControllerRef::ChosenPlayer { index }`) and any later
            // `Choose(Player)` in the same resolution see this choice. The
            // continuation chain carries the list because it is a
            // `ResolvedAbility` — unlike `last_named_choice`, which is a
            // single GameState slot cleared after every drain.
            if matches!(
                choice_type,
                ChoiceType::Player { .. } | ChoiceType::Opponent { .. }
            ) {
                if let Ok(pid) = choice.parse::<u8>() {
                    if let Some(frame) = state.active_ability_continuation_frame_mut() {
                        let mut chosen = frame.pending.chain.chosen_players.clone();
                        chosen.push(crate::types::player::PlayerId(pid));
                        frame.pending.chain.set_chosen_players_recursive(&chosen);
                    }
                }
            }

            let waiting_for = finish_with_continuation(state, player, events);
            if !matches!(waiting_for, WaitingFor::Priority { .. }) {
                state.last_named_choice = None;
                return Ok(ResolutionChoiceOutcome::WaitingFor(waiting_for));
            }
            if let Some(pending) = state.pending_cast.take() {
                if pending.activation_ability_index.is_some() {
                    state.waiting_for =
                        casting_costs::finish_activated_ability_at_payment_boundary(
                            state, player, *pending, events,
                        )?;
                } else {
                    // CR 601.2c + CR 601.2f (mirrors the identical fix for
                    // GameAction::DistributeAmong in engine.rs): a NamedChoice
                    // pause can occur after targets are already known, so the
                    // total cost — including any target-dependent surcharge
                    // (Strive, CR 207.2c) — must be re-derived through the
                    // single cost-determination authority
                    // (`finish_pending_cast_cost_or_pay`) rather than paying
                    // the cost that was locked in earlier in the casting
                    // sequence, before this resumption point. The
                    // clone-and-restore-on-Err mirrors
                    // `finalize_mana_payment`'s `pending_for_restore` pattern
                    // (CR 601.2h, "unpayable costs can't be paid") since
                    // `state.pending_cast` is already taken (`None`) here and
                    // `finish_pending_cast_cost_or_pay`'s downstream chain has
                    // no restore-on-error wrapper of its own.
                    let pending_for_restore = pending.clone();
                    let ability = pending.ability.clone();
                    let cost = pending.cost.clone();
                    state.waiting_for = match casting_costs::finish_pending_cast_cost_or_pay(
                        state, player, *pending, *ability, cost, events,
                    ) {
                        Ok(waiting_for) => waiting_for,
                        Err(err) => {
                            state.pending_cast = Some(pending_for_restore);
                            return Err(err);
                        }
                    };
                }
            } else if let Some(source) = source
                .as_ref()
                .filter(|source| {
                    source.is_exact_object_and_resolution()
                        && source.prompt.identity.expected_zone == Zone::Battlefield
                        && !state.deferred_entry_events.is_empty()
                })
                .map(|source| source.prompt.identity.reference.object_id)
            {
                // CR 603.2 + CR 614.12a (#830): an "As it enters, choose …"
                // replacement (Valgavoth's Lair, the Thriving lands) paused this
                // permanent's battlefield entry on a persisted `NamedChoice`, so
                // the entry's `ZoneChanged` never reached the priority-time
                // trigger collection (`run_post_action_pipeline`). The capture in
                // `engine_replacement::capture_deferred_entry_events_if_mid_entry_choice`
                // stashed that event into `state.deferred_entry_events`; now that
                // the chosen attribute is folded onto the entering permanent,
                // replay it through the shared deferred-entry authority so every
                // ETB observer (constellation like Doomwake Giant, Soul Warden, …)
                // fires against the realized post-choice object. The helper drains
                // the pending continuation (so this arm fully replaces the plain
                // `drain_pending_continuation` below) and surfaces any interactive
                // trigger pause (OrderTriggers / DistributeAmong / target
                // selection) raised by simultaneously-fired observers.
                //
                // Gated on `deferred_entry_events` being non-empty so a non-entry
                // persisted `NamedChoice` (Pithing Needle naming, Morophon type
                // choice) takes the unchanged `else` path below — the no-op
                // disambiguator that keeps the working path byte-for-byte intact.
                // `last_named_choice` is left set across the helper's continuation
                // drain (cleared after, mirroring the plain path) so any dependent
                // continuation reads the answer.
                let replay = crate::game::engine_replacement::replay_deferred_entry_events(
                    state, source, events,
                )?;
                state.last_named_choice = None;
                if let Some(waiting_for) = replay {
                    return Ok(ResolutionChoiceOutcome::WaitingFor(waiting_for));
                }
                return Ok(ResolutionChoiceOutcome::WaitingFor(
                    state.waiting_for.clone(),
                ));
            }
            state.last_named_choice = None;
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        // CR 608.2d + CR 608.2e: an opponent / the defending player answered the
        // guess. Compute correctness against the UNFILTERED state, record the
        // guessed value for downstream reads, stamp the outcome onto the stashed
        // branch chain, then drain it synchronously under the source controller.
        (
            WaitingFor::OpponentGuess {
                player: guesser,
                options,
                choice_type,
                source,
                owner,
                proposition_truth,
            },
            GameAction::ChooseOption { choice },
        ) => {
            // (a) Validate the guess is a legal option.
            if !options.contains(&choice) {
                return Err(EngineError::InvalidAction(format!(
                    "Invalid guess '{}', must be one of: {:?}",
                    choice, options
                )));
            }

            // (b) Correctness, resolved against the unfiltered GameState.
            let owner = owner.as_ref().ok_or_else(|| {
                EngineError::InvalidAction(
                    "OpponentGuess is missing its private answer-time authority".to_string(),
                )
            })?;
            if !source.matches_owner(owner) {
                return Err(EngineError::InvalidAction(
                    "OpponentGuess has incoherent source authority".to_string(),
                ));
            }
            let outcome = if effects::opponent_guess::guess_is_correct(
                &options,
                &choice,
                proposition_truth,
                owner.committed_choice.as_ref(),
            ) {
                GuessOutcome::Correct
            } else {
                GuessOutcome::Incorrect
            };

            // (c) Record the guessed value WITHOUT persisting it to the source.
            // "they lose life equal to the number they guessed" reads the
            // guesser's value via `QuantityRef::Variable` -> `last_named_choice`.
            // Supplying no source binding records that value without pushing a
            // `ChosenAttribute::Number` (only an exact-object binding can push),
            // keeping the source's committed-number history
            // (which drives BOTH the DistinctFromSourceHistory exclusion AND the
            // last-committed read) guesser-free. Only meaningful for a committed
            // number guess; propositions carry no downstream guessed-value read.
            if proposition_truth.is_none() {
                effects::choose::bind_named_choice(state, &choice_type, &choice, None, None);
            }

            // (d) Stamp the outcome across the stashed continuation chain so each
            // branch head re-evaluates `Guessed { outcome }` against it on drain.
            // Also expose the guesser as a front player target so a "they lose
            // life ..." `ParentTarget` anaphor in a branch resolves to them
            // (CR 608.2d — the guesser is the player the branch acts on).
            if let Some(frame) = state.active_ability_continuation_frame_mut() {
                frame
                    .pending
                    .chain
                    .set_guess_outcome_recursive(Some(outcome));
                frame
                    .pending
                    .chain
                    .push_front_player_target_recursive(guesser);
            }

            // (e) Priority to the source CONTROLLER (the player resolving this
            // ability), not the guesser — the guesser only answered a sub-prompt;
            // the resolution continues under the controller (e.g. Seventh
            // Doctor's "you may cast it" CastOffer is to the controller). This
            // also clears the OpponentGuess wait so the drain's guard passes.
            let controller = source.prompt.controller;
            set_priority(state, controller);
            super::engine::resume_pending_continuation_if_priority(state, events)
                .expect("a settled guess choice must resume its continuation");
            // Cleared ONLY after the synchronous drain (mirrors NamedChoice), so
            // the wrong/right branch's "number they guessed" read sees it.
            state.last_named_choice = None;
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        // Alchemy spellbook draft: the player chose a card from the source's
        // spellbook — conjure it, then resume the rest of the ability chain.
        (
            WaitingFor::SpellbookDraft {
                player,
                source_id,
                options,
                destination,
                tapped,
            },
            GameAction::SubmitSpellbookDraft { card },
        ) => {
            crate::game::effects::spellbook::complete_draft(
                state,
                player,
                source_id,
                &options,
                &card,
                destination,
                tapped,
                events,
            )
            .map_err(|e| EngineError::InvalidAction(format!("spellbook draft: {e:?}")))?;
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
        }
        (
            WaitingFor::DamageSourceChoice {
                player,
                source_filter,
                options,
            },
            GameAction::ChooseDamageSource { source },
        ) => {
            if !options.contains(&source) {
                return Err(EngineError::InvalidAction(
                    "Invalid damage source choice".to_string(),
                ));
            }

            state.last_chosen_damage_source = Some(ChosenDamageSource {
                source_id: source,
                source_filter,
            });
            set_priority(state, player);
            super::engine::resume_pending_continuation_if_priority(state, events)
                .expect("a settled damage-source choice must resume its continuation");
            state.last_chosen_damage_source = None;
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        (
            WaitingFor::ChooseRingBearer { player, candidates },
            GameAction::ChooseRingBearer { target },
        ) => {
            if !candidates.contains(&target) {
                return Err(EngineError::InvalidAction(
                    "Invalid ring-bearer choice".to_string(),
                ));
            }
            state.ring_bearer.insert(player, Some(target));
            crate::game::layers::mark_layers_full(state);
            let waiting_for = finish_with_continuation(state, player, events);
            // CR 603.2 + CR 701.54: RingTemptsYou observer triggers are batched
            // while ChooseRingBearer pauses spell resolution (issue #1017).
            if let Some(outcome) =
                batch_or_drain_observer_triggers(state, events, events.len(), events.len(), false)
            {
                return Ok(outcome);
            }
            ResolutionChoiceOutcome::WaitingFor(waiting_for)
        }
        // CR 709.5f-g: the controller picked which door (half) of the targeted
        // Room to lock/unlock. Validate the (op, door) pair is one the prompt
        // offered, apply the primitive (unlocking emits `RoomDoorUnlocked` so
        // CR 709.5h-i triggers fire), then drain the parked continuation.
        (
            WaitingFor::ChooseRoomDoor {
                player,
                object_id,
                options,
            },
            GameAction::ChooseRoomDoor {
                object_id: chosen_object,
                op,
                door,
            },
        ) => {
            if chosen_object != object_id || !options.contains(&(op, door)) {
                return Err(EngineError::InvalidAction(
                    "Invalid room-door choice — not an offered (operation, door)".to_string(),
                ));
            }
            let events_before = events.len();
            effects::set_room_door_lock::apply_door_op(state, object_id, player, op, door, events);
            let waiting_for = finish_with_continuation(state, player, events);
            // CR 603.2 + CR 709.5h-i: an effect-driven unlock can trigger
            // "when you unlock"/"when you fully unlock" abilities; batch or
            // dispatch them now that the choice has resolved.
            if let Some(outcome) =
                batch_or_drain_observer_triggers(state, events, events_before, events.len(), false)
            {
                return Ok(outcome);
            }
            ResolutionChoiceOutcome::WaitingFor(waiting_for)
        }
        (WaitingFor::ChooseDungeon { player, options }, GameAction::ChooseDungeon { dungeon }) => {
            if !options.contains(&dungeon) {
                return Err(EngineError::InvalidAction(
                    "Invalid dungeon choice".to_string(),
                ));
            }
            let events_before_venture = events.len();
            effects::venture::handle_choose_dungeon(state, player, dungeon, events);
            if let Some(waiting_for) = super::engine::begin_pending_trigger_target_selection(state)?
            {
                state.waiting_for = waiting_for.clone();
            }
            // CR 603.2 + CR 309.4c: RoomEntered from the chosen dungeon must dispatch
            // card triggers such as "Whenever you venture into the dungeon" (issue #1297).
            // The resolution-choice path does not run `run_post_action_pipeline`.
            if let Some(outcome) = batch_or_drain_observer_triggers(
                state,
                events,
                events_before_venture,
                events.len(),
                false,
            ) {
                return Ok(outcome);
            }
            if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                return Ok(ResolutionChoiceOutcome::WaitingFor(
                    state.waiting_for.clone(),
                ));
            }
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
        }
        (
            WaitingFor::ChooseDungeonRoom {
                player,
                dungeon,
                options,
                ..
            },
            GameAction::ChooseDungeonRoom { room_index },
        ) => {
            if !options.contains(&room_index) {
                return Err(EngineError::InvalidAction(
                    "Invalid dungeon room choice".to_string(),
                ));
            }
            let events_before_venture = events.len();
            effects::venture::handle_choose_room(state, player, dungeon, room_index, events);
            if let Some(waiting_for) = super::engine::begin_pending_trigger_target_selection(state)?
            {
                state.waiting_for = waiting_for.clone();
            }
            if let Some(outcome) = batch_or_drain_observer_triggers(
                state,
                events,
                events_before_venture,
                events.len(),
                false,
            ) {
                return Ok(outcome);
            }
            if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                return Ok(ResolutionChoiceOutcome::WaitingFor(
                    state.waiting_for.clone(),
                ));
            }
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
        }
        (
            WaitingFor::SpecializeColor {
                player,
                object_id,
                options,
            },
            GameAction::ChooseSpecializeColor { color },
        ) => {
            if !options.contains(&color) {
                return Err(EngineError::InvalidAction(
                    "Invalid specialize color choice".to_string(),
                ));
            }
            effects::specialize::handle_choose_specialize_color(
                state, player, object_id, &options, color, events,
            )?;
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
        }
        (WaitingFor::ChooseLegend { candidates, .. }, GameAction::ChooseLegend { keep }) => {
            if !candidates.contains(&keep) {
                return Err(EngineError::InvalidAction(
                    "Invalid legend choice — not a candidate".to_string(),
                ));
            }
            let to_remove: Vec<_> = candidates
                .iter()
                .filter(|&&id| id != keep)
                .copied()
                .collect();
            // CR 704.5j + CR 614.6 + CR 603.10a: the losing legends are put into
            // their owners' graveyards simultaneously as a single state-based
            // action. Route them through the zone-change pipeline so a `Moved`
            // graveyard→exile redirect (Rest in Peace / Leyline of the Void)
            // fires on each — the raw `move_to_zone` never proposed the inner
            // ZoneChange, silently dropping those redirects. `move_objects_
            // simultaneously` co-stamps the departures so leaves-the-battlefield
            // observers see each other (CR 603.10a). The legends move themselves
            // as an SBA (no external source), so each anchors its own
            // attribution. A CR 616.1 ordering choice mid-batch parks the prompt
            // and stashes the undelivered tail; surface the parked prompt instead
            // of clobbering it with `Priority`.
            let reqs: Vec<_> = to_remove
                .into_iter()
                .map(|id| {
                    crate::game::zone_pipeline::ZoneMoveRequest::effect(id, Zone::Graveyard, id)
                })
                .collect();
            match crate::game::zone_pipeline::move_objects_simultaneously(state, reqs, events) {
                crate::game::zone_pipeline::BatchMoveResult::Done => {
                    ResolutionChoiceOutcome::WaitingFor(WaitingFor::Priority {
                        player: state.active_player,
                    })
                }
                crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => {
                    ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
                }
            }
        }
        // CR 702.140c + CR 730.2a: The mutate spell's controller chose whether the
        // spell merges on top of or under the target creature. `merge::handle_mutate_
        // merge_choice` validates the actor, performs the merge (CR 730.2), and
        // returns to priority so the `Mutated` event's triggers/SBAs are processed.
        (
            WaitingFor::MutateMergeChoice { player, .. },
            GameAction::ChooseMutateMergeSide { side },
        ) => {
            let waiting =
                crate::game::merge::handle_mutate_merge_choice(state, player, side, events)?;
            ResolutionChoiceOutcome::WaitingFor(waiting)
        }
        // CR 702.99a: The resolving Cipher spell's controller chose a creature to
        // encode the card on (or declined). `cipher::handle_encode_choice`
        // exiles+links on accept or routes the card to its graveyard on decline,
        // then resolution is complete — return to priority so the resulting zone
        // change's triggers/SBAs are processed.
        (WaitingFor::CipherEncodeChoice { card_id, .. }, GameAction::CipherEncode { creature }) => {
            // CR 616.1: a declined cipher card hitting a graveyard→exile redirect
            // can surface a replacement-ordering choice, which `handle_encode_choice`
            // parks centrally via `move_object`. Surface the parked prompt instead
            // of clobbering it with `Priority`; otherwise resolution is complete,
            // so return to priority and let the resulting zone change's triggers /
            // SBAs process.
            // CR 702.99a: the offer's own frame owns this prompt (issue #7470),
            // so consume it BEFORE the card moves — the encode's zone change can
            // park frames of its own, and a stale owner underneath them would
            // fail `validate` at the next prompt. This holds for BOTH answers:
            // a decline (`creature: None`) ends the offer just as an acceptance
            // does, so it must consume the owner just as an acceptance does.
            //
            // The error is surfaced rather than swallowed: it means some other
            // frame is sitting on top of this prompt's owner, which is the exact
            // corruption this frame was introduced to make impossible. `Ok(None)`
            // is not that — it is an empty stack, i.e. no owner to leave stale,
            // which is what a game saved before this frame existed restores as.
            state
                .take_active_cipher_encode_frame()
                .map_err(|error| EngineError::InvalidAction(error.to_string()))?;
            match crate::game::cipher::handle_encode_choice(state, card_id, creature, events) {
                crate::game::zone_pipeline::ZoneMoveResult::Done => {
                    ResolutionChoiceOutcome::WaitingFor(WaitingFor::Priority {
                        player: state.active_player,
                    })
                }
                crate::game::zone_pipeline::ZoneMoveResult::NeedsChoice(_)
                | crate::game::zone_pipeline::ZoneMoveResult::NeedsAuraAttachmentChoice => {
                    ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
                }
            }
        }
        // CR 903.9a: Owner decides whether to return their commander to the command zone.
        // Decline leaves it in its current zone and marks that stay so the SBA
        // loop does not re-ask; a settled zone change clears that mark for a
        // fresh arrival.
        (
            WaitingFor::CommanderZoneChoice { commander_id, .. },
            GameAction::DecideOptionalEffect { accept },
        ) => {
            if accept {
                // CR 614.1 + CR 616.1: The owner-elected return is a replaceable
                // zone change. Preserve a centrally parked replacement prompt
                // rather than clobbering it with Priority; its generic resume
                // boundary finishes delivery and returns to priority.
                let mut request = crate::game::zone_pipeline::ZoneMoveRequest::state_based_action(
                    commander_id,
                    Zone::Command,
                );
                request.cause = crate::game::zone_pipeline::ZoneChangeCause::CommanderRuleReturn;
                match crate::game::zone_pipeline::move_object(state, request, events) {
                    crate::game::zone_pipeline::ZoneMoveResult::Done => {
                        ResolutionChoiceOutcome::WaitingFor(WaitingFor::Priority {
                            player: state.active_player,
                        })
                    }
                    crate::game::zone_pipeline::ZoneMoveResult::NeedsChoice(_)
                    | crate::game::zone_pipeline::ZoneMoveResult::NeedsAuraAttachmentChoice => {
                        ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
                    }
                }
            } else {
                state.commander_declined_zone_return.insert(commander_id);
                ResolutionChoiceOutcome::WaitingFor(WaitingFor::Priority {
                    player: state.active_player,
                })
            }
        }
        // CR 310.11 + CR 704.5w + CR 704.5x: controller assigns the battle's new
        // protector. Re-running the SBA fixpoint (via the Priority resumption) will
        // find any remaining battles still needing reassignment.
        (
            WaitingFor::BattleProtectorChoice {
                battle_id,
                candidates,
                ..
            },
            GameAction::ChooseBattleProtector { protector },
        ) => {
            if !candidates.contains(&protector) {
                return Err(EngineError::InvalidAction(
                    "Invalid battle protector choice — not a candidate".to_string(),
                ));
            }
            if let Some(obj) = state.objects.get_mut(&battle_id) {
                obj.chosen_attributes
                    .retain(|a| !matches!(a, ChosenAttribute::Player(_)));
                obj.chosen_attributes
                    .push(ChosenAttribute::Player(protector));
            }
            ResolutionChoiceOutcome::WaitingFor(WaitingFor::Priority {
                player: state.active_player,
            })
        }
        // CR 101.4 + CR 701.21a: Player selected one permanent per type category.
        (
            WaitingFor::CategoryChoice {
                player,
                target_player: _,
                categories,
                chooser_scope,
                choose_filter,
                sacrifice_filter,
                source_controller,
                eligible_per_category,
                source_id,
                remaining_players,
                mut all_kept,
                scoped_players,
            },
            GameAction::SelectCategoryPermanents { choices },
        ) => {
            // Validate: choices length must match categories length.
            if choices.len() != categories.len() {
                return Err(EngineError::InvalidAction(format!(
                    "Must provide exactly {} choices, got {}",
                    categories.len(),
                    choices.len()
                )));
            }

            // Validate each choice is eligible for its category. A permanent can
            // legally satisfy multiple category slots (artifact creature, etc.);
            // dedupe only when building the final protected set.
            let mut chosen_this_round = Vec::new();
            for (i, choice) in choices.iter().enumerate() {
                let Some(obj_id) = choice else {
                    if !eligible_per_category[i].is_empty() {
                        return Err(EngineError::InvalidAction(format!(
                            "Must choose a permanent for category {:?}",
                            categories[i]
                        )));
                    }
                    continue;
                };
                if !eligible_per_category[i].contains(obj_id) {
                    return Err(EngineError::InvalidAction(format!(
                        "Object {:?} is not eligible for category {:?}",
                        obj_id, categories[i]
                    )));
                }
                if !chosen_this_round.contains(obj_id) {
                    chosen_this_round.push(*obj_id);
                }
            }

            // Accumulate kept permanents.
            all_kept.extend(chosen_this_round);

            // Issue #423 (Correction 1): `sacrifice_unchosen` moves permanents
            // to the graveyard via `sacrifice_permanent`. Mark where those
            // dies-events begin so the B2 branch below can batch their triggers.
            let events_before_sacrifice = events.len();
            // Clear `state.waiting_for` to a sentinel before advancing.
            // `advance_to_next_player` / `sacrifice_unchosen` only WRITE
            // `state.waiting_for` when they pause (a fresh `CategoryChoice` for
            // the next chooser, or a replacement choice). When they auto-resolve
            // and sacrifice, they leave `state.waiting_for` untouched — so
            // without this reset the stale `CategoryChoice` of the chooser we
            // just handled would still be present, and the `CategoryChoice`
            // check below would wrongly treat a completed sacrifice as a pause.
            set_priority(state, player);
            // Advance to next player or sacrifice.
            if remaining_players.is_empty() {
                // All players have chosen — sacrifice everything not kept.
                effects::choose_and_sacrifice_rest::sacrifice_unchosen_from_handler(
                    state,
                    &all_kept,
                    &scoped_players,
                    &sacrifice_filter,
                    source_id,
                    source_controller,
                    events,
                )
                .map_err(|error| EngineError::InvalidAction(error.to_string()))?;
            } else if let Err(e) = effects::choose_and_sacrifice_rest::advance_to_next_player(
                state,
                &categories,
                chooser_scope,
                source_controller,
                source_id,
                &remaining_players,
                all_kept,
                &choose_filter,
                &sacrifice_filter,
                &scoped_players,
                events,
            ) {
                return Err(EngineError::InvalidAction(format!("{:?}", e)));
            }
            // If a sacrifice round set a fresh `CategoryChoice`, the run paused
            // before any sacrifice — return directly.
            if matches!(state.waiting_for, WaitingFor::CategoryChoice { .. }) {
                ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
            } else {
                // The sacrifice (if any) is complete. Mark its event slice.
                let events_after_sacrifice = events.len();
                // CR 603.10a + CR 608.2f + CR 701.21a: the permanents sacrificed by
                // `sacrifice_unchosen` (keep-one-sacrifice-rest: Cataclysm,
                // Tragic Arrogance) left the battlefield together in this single
                // resolution event, so a co-departing leaves-the-battlefield
                // observer among them (Blood Artist) observes the rest. Stamp the
                // sacrifice sub-slice before the B1/B2 trigger dispatch reads it.
                super::zones::stamp_simultaneous_from_slice(
                    state,
                    &mut events[events_before_sacrifice..events_after_sacrifice],
                );
                // Step B: if the sacrifice did not itself pause (no replacement
                // choice was raised by `sacrifice_unchosen`), resolve any
                // reflexive continuation. `state.waiting_for` is the `Priority`
                // sentinel set before the advance unless a replacement choice
                // was raised — in which case the continuation stays parked.
                if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    resume_with_error_propagation(state, events)?;
                }
                // CR 603.2 + CR 603.3b: Issue #423 (Correction 1) — dispatch the
                // dies-triggers from `sacrifice_unchosen` (Undying CR 702.93a,
                // Blood Artist-class observers). Mirrors the `EffectZoneChoice`
                // Sacrifice arm: B1 (`Priority`) lets `run_post_action_pipeline`
                // scan this action's events and drains any prior parked queue;
                // B2 (paused) batches this action's sacrifice events for a
                // later drain.
                if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    if let Some(wf) = super::triggers::drain_deferred_trigger_queue(state, events) {
                        return Ok(ResolutionChoiceOutcome::WaitingFor(wf));
                    }
                } else {
                    let trigger_events: Vec<GameEvent> = events
                        [events_before_sacrifice..events_after_sacrifice]
                        .iter()
                        .filter(|ev| !matches!(ev, GameEvent::PhaseChanged { .. }))
                        .cloned()
                        .collect();
                    super::triggers::collect_triggers_into_deferred(state, &trigger_events);
                }
                ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
            }
        }
        // CR 101.4 + CR 707.2 + CR 122.1: One player submitted their ordered
        // 1..=max selection for `EachPlayerCopyChosen`. Collect APNAP choices
        // first; copy/counter actions run only after the complete set is known.
        (
            WaitingFor::EachPlayerCopyChosenSelection {
                player,
                eligible,
                min,
                max,
                choose_filter,
                copy_modifications,
                scale,
                choose_scope,
                source_id,
                source_controller,
                remaining_players,
                mut all_choices,
                scoped_players,
                trigger_event,
            },
            GameAction::SelectTargets { targets },
        ) => {
            // CR 707.2: validate an ordered, distinct, in-eligible selection of
            // size `min..=max`. Order is load-bearing (index 0 copied, index 1
            // scales).
            if targets.len() < min as usize || targets.len() > max as usize {
                return Err(EngineError::InvalidAction(format!(
                    "EachPlayerCopyChosen: must choose between {min} and {max} objects, got {}",
                    targets.len()
                )));
            }
            let mut chosen: Vec<ObjectId> = Vec::with_capacity(targets.len());
            for t in &targets {
                if !eligible.contains(t) {
                    return Err(EngineError::InvalidAction(
                        "EachPlayerCopyChosen: selected object not eligible".to_string(),
                    ));
                }
                let TargetRef::Object(id) = t else {
                    return Err(EngineError::InvalidAction(
                        "EachPlayerCopyChosen: selection must be objects".to_string(),
                    ));
                };
                if chosen.contains(id) {
                    return Err(EngineError::InvalidAction(
                        "EachPlayerCopyChosen: duplicate object in selection".to_string(),
                    ));
                }
                if !effects::each_player_copy_chosen::is_live_eligible_choice(
                    state,
                    player,
                    *id,
                    &choose_filter,
                    choose_scope,
                    source_id,
                    source_controller,
                ) {
                    return Err(EngineError::InvalidAction(
                        "EachPlayerCopyChosen: selected object no longer eligible".to_string(),
                    ));
                }
                chosen.push(*id);
            }
            all_choices.push(CopyChosenSelection { player, chosen });
            let params = effects::each_player_copy_chosen::CopyChosenParams {
                choose_filter,
                min,
                max,
                copy_modifications,
                scale,
                choose_scope,
                source_id,
                source_controller,
                scoped_players,
                trigger_event: trigger_event.clone(),
            };
            // Priority sentinel — `advance_to_next_player` writes `waiting_for`
            // only when it prompts the next chooser or the action phase pauses.
            let events_before = events.len();
            set_priority(state, player);
            // CR 608.2: restore the phenomenon trigger event across the
            // collection/action continuation.
            let previous_trigger_event = state.current_trigger_event.clone();
            state.current_trigger_event = trigger_event;
            let drive_result = effects::each_player_copy_chosen::advance_to_next_player(
                state,
                remaining_players,
                all_choices,
                &params,
                events,
            );
            state.current_trigger_event = previous_trigger_event;
            if let Err(e) = drive_result {
                return Err(EngineError::InvalidAction(format!("{e:?}")));
            }
            // CR 603.2 + CR 603.3b: trigger bookkeeping across the paused walk,
            // mirroring the `CategoryChoice` arm. If the walk settled back to
            // Priority, drain any triggers deferred by earlier paused rounds;
            // otherwise (a later player's selection or a replacement choice is
            // now pending) batch this action's events for a later drain so the
            // created tokens' ETB observers are not dropped.
            if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                if let Some(wf) = super::triggers::drain_deferred_trigger_queue(state, events) {
                    return Ok(ResolutionChoiceOutcome::WaitingFor(wf));
                }
            } else {
                let trigger_events: Vec<GameEvent> = events[events_before..]
                    .iter()
                    .filter(|ev| !matches!(ev, GameEvent::PhaseChanged { .. }))
                    .cloned()
                    .collect();
                super::triggers::collect_triggers_into_deferred(state, &trigger_events);
            }
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        // CR 107.1c + CR 701.21a (Slaughter the Strong): player kept a subset of
        // their creatures within the total-power cap; sacrifice the rest.
        (
            WaitingFor::KeepWithinTotalPowerChoice {
                player,
                target_player: _,
                eligible,
                cap,
                choose_filter,
                sacrifice_filter,
                chooser_scope,
                source_id,
                source_controller,
                remaining_players,
                mut all_kept,
                scoped_players,
            },
            GameAction::ChooseKeptCreatures { kept },
        ) => {
            // Validate: every kept creature is eligible, and the combined power of
            // the (deduped) kept set is within the cap.
            let mut chosen = Vec::new();
            for id in &kept {
                if !eligible.contains(id) {
                    return Err(EngineError::InvalidAction(format!(
                        "Creature {id:?} is not eligible to keep"
                    )));
                }
                if !chosen.contains(id) {
                    chosen.push(*id);
                }
            }
            let kept_power = effects::choose_and_sacrifice_rest::total_power(state, &chosen);
            if kept_power > cap {
                return Err(EngineError::InvalidAction(format!(
                    "Kept creatures' total power {kept_power} exceeds {cap}"
                )));
            }
            all_kept.extend(chosen);

            // `step_total_power` either pauses for the next chooser or, when no
            // players remain, sacrifices the unchosen (stamping that slice itself).
            let events_before_sacrifice = events.len();
            set_priority(state, player);
            effects::choose_and_sacrifice_rest::step_total_power(
                state,
                source_id,
                source_controller,
                chooser_scope,
                &remaining_players,
                all_kept,
                &choose_filter,
                &sacrifice_filter,
                cap,
                &scoped_players,
                events,
            )
            .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;

            if matches!(
                state.waiting_for,
                WaitingFor::KeepWithinTotalPowerChoice { .. }
            ) {
                ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
            } else {
                let events_after_sacrifice = events.len();
                if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    resume_with_error_propagation(state, events)?;
                }
                if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    if let Some(wf) = super::triggers::drain_deferred_trigger_queue(state, events) {
                        return Ok(ResolutionChoiceOutcome::WaitingFor(wf));
                    }
                } else {
                    let trigger_events: Vec<GameEvent> = events
                        [events_before_sacrifice..events_after_sacrifice]
                        .iter()
                        .filter(|ev| !matches!(ev, GameEvent::PhaseChanged { .. }))
                        .cloned()
                        .collect();
                    super::triggers::collect_triggers_into_deferred(state, &trigger_events);
                }
                ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
            }
        }
        // CR 101.4 + CR 701.21a: An exact-cardinality keeper choice. The
        // collected protected sets are handed to the shared scope-sacrifice
        // queue only once every scoped player has chosen, so replacement
        // choices cannot bypass later sacrifices or the parked continuation.
        (
            WaitingFor::KeepExactPermanentsChoice {
                player,
                target_player: _,
                eligible,
                required_count,
                choose_filter,
                sacrifice_filter,
                chooser_scope,
                source_id,
                source_controller,
                remaining_players,
                mut all_kept,
                scoped_players,
            },
            GameAction::ChooseKeptPermanents { kept },
        ) => {
            if kept.len() != required_count {
                return Err(EngineError::InvalidAction(format!(
                    "Must keep exactly {required_count} permanent(s), got {}",
                    kept.len()
                )));
            }
            let mut chosen = Vec::with_capacity(kept.len());
            for id in &kept {
                if !eligible.contains(id) {
                    return Err(EngineError::InvalidAction(format!(
                        "Permanent {id:?} is not eligible to keep"
                    )));
                }
                if chosen.contains(id) {
                    return Err(EngineError::InvalidAction(
                        "Cannot keep the same permanent more than once".to_string(),
                    ));
                }
                chosen.push(*id);
            }
            all_kept.extend(chosen);

            let events_before_sacrifice = events.len();
            set_priority(state, player);
            effects::choose_and_sacrifice_rest::step_exact_count(
                state,
                source_id,
                source_controller,
                chooser_scope,
                &remaining_players,
                all_kept,
                &choose_filter,
                &sacrifice_filter,
                required_count,
                &scoped_players,
                events,
            )
            .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;

            if matches!(
                state.waiting_for,
                WaitingFor::KeepExactPermanentsChoice { .. }
            ) {
                return Ok(ResolutionChoiceOutcome::WaitingFor(
                    state.waiting_for.clone(),
                ));
            }

            let events_after_sacrifice = events.len();
            if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                resume_with_error_propagation(state, events)?;
            }
            if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                if let Some(wf) = super::triggers::drain_deferred_trigger_queue(state, events) {
                    return Ok(ResolutionChoiceOutcome::WaitingFor(wf));
                }
            } else {
                let trigger_events: Vec<GameEvent> = events
                    [events_before_sacrifice..events_after_sacrifice]
                    .iter()
                    .filter(|ev| !matches!(ev, GameEvent::PhaseChanged { .. }))
                    .cloned()
                    .collect();
                super::triggers::collect_triggers_into_deferred(state, &trigger_events);
            }
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        (waiting_for, action) => {
            return Err(EngineError::ActionNotAllowed(format!(
                "Cannot perform {:?} while waiting for {:?}",
                action, waiting_for
            )));
        }
    };

    Ok(outcome)
}

fn action_result_outcome(
    events: &mut Vec<GameEvent>,
    waiting_for: WaitingFor,
) -> ResolutionChoiceOutcome {
    ResolutionChoiceOutcome::ActionResult(ActionResult {
        events: std::mem::take(events),
        waiting_for,
        log_entries: vec![],
    })
}

/// CR 608.2c + CR 603.7: Publish the EffectZoneChoice selection as the chain
/// tracked set when a continuation will consume it ("those Auras", plotted
/// cards, etc.).
///
/// Must also run on mid-delivery pauses (`NeedsAuraAttachmentChoice` /
/// replacement `NeedsChoice`): those early-return before the terminal publish
/// at the end of the EffectZoneChoice arm, and Storm Herald's delayed exile
/// would otherwise bind `TrackedSet` against an unbound sentinel.
///
/// `mid_pause`: when true, an empty selection is not published yet (Aura host
/// choice / replacement ordering still open). When false (terminal completion),
/// an empty `up_to` selection must rebind a fresh empty chain set so a following
/// `TargetFilter::TrackedSet` cannot reuse a prior non-empty set.
fn publish_effect_zone_choice_tracked_set(
    state: &mut GameState,
    effect_kind: EffectKind,
    chosen: &[ObjectId],
    library_position: Option<LibraryPosition>,
    mid_pause: bool,
) {
    if !matches!(
        effect_kind,
        EffectKind::Sacrifice
            | EffectKind::ChangeZone
            | EffectKind::BounceAll
            | EffectKind::Tap
            | EffectKind::Untap
            | EffectKind::PutAtLibraryPosition
            | EffectKind::CastFromZone
    ) || state.active_ability_continuation().is_none()
    {
        return;
    }
    // Distinguish mid-pause "nothing to publish yet" from a genuine empty
    // narrowed set (PutAtLibraryPosition Bottom). The latter must still rebind
    // `chain_tracked_set_id` so a chained TrackedSet exile cannot re-select
    // cards that just left the library (CR 608.2c).
    let mut narrowed = false;
    let tracked = if matches!(effect_kind, EffectKind::Sacrifice) {
        // Sacrifice publishes from PermanentSacrificed events at the completion
        // seam; callers pass the sacrificed ids already.
        chosen.to_vec()
    } else if matches!(effect_kind, EffectKind::PutAtLibraryPosition)
        && matches!(library_position, Some(LibraryPosition::Bottom))
    {
        narrowed = true;
        // CR 608.2c: Expressive Iteration's bottom pick narrows the tracked set
        // to the remaining looked-at library cards so the chained exile step
        // cannot re-select the bottomed card.
        state
            .chain_tracked_set_id
            .and_then(|id| state.tracked_object_sets.get(&id).cloned())
            .unwrap_or_default()
            .into_iter()
            .filter(|id| !chosen.contains(id))
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|obj| obj.zone == Zone::Library)
            })
            .collect()
    } else {
        chosen.to_vec()
    };
    // Pause-only: skip empty until the selection is terminal. Terminal empty
    // (including narrowed-to-empty Bottom) must still rebind.
    if tracked.is_empty() && mid_pause && !narrowed {
        return;
    }
    let tracked_id = TrackedSetId(state.next_tracked_set_id);
    state.next_tracked_set_id += 1;
    state.tracked_object_sets.insert(tracked_id, tracked);
    state.chain_tracked_set_id = Some(tracked_id);
}

fn set_priority(state: &mut GameState, player: crate::types::player::PlayerId) {
    state.waiting_for = WaitingFor::Priority { player };
    state.priority_player = player;
}

/// CR 614.6 + CR 616.1: Move a reveal-until *kept* card to a non-battlefield
/// destination (`accept_zone` / `decline_zone`) through the zone-change pipeline
/// so a `Moved` graveyard→exile redirect (Rest in Peace / Leyline of the Void)
/// fires on it — the 4 `kept_destination: Graveyard` reveal-until cards (Mind
/// Funeral class) previously dropped that redirect via the raw mover.
///
/// Returns `Some(parked_outcome)` when the move pauses on a CR 616.1 ordering
/// choice: the rest-pile move + reveal-marker clear are deferred onto a
/// `RevealRestPile` completion (so the misses do not strand and the cleanup runs
/// once on resume), and the caller must return that outcome. Returns `None` when
/// the move completed synchronously and the caller should proceed to move the
/// rest pile inline. `emit_reveal_until_resolved` is `None` — the kept-choice
/// path already emitted `EffectResolved` before this prompt.
fn route_kept_card_or_defer(
    state: &mut GameState,
    hit_card: ObjectId,
    destination: Zone,
    source_id: ObjectId,
    misses: &[ObjectId],
    rest_destination: Zone,
    events: &mut Vec<GameEvent>,
) -> Option<ResolutionChoiceOutcome> {
    let player = state
        .objects
        .get(&hit_card)
        .map(|obj| obj.controller)
        .unwrap_or(state.active_player);
    let mut req =
        crate::game::zone_pipeline::ZoneMoveRequest::effect(hit_card, destination, source_id);
    if destination == Zone::Library {
        req = req.at_library_position(LibraryPosition::Bottom);
    }
    match crate::game::zone_pipeline::move_object(state, req, events) {
        crate::game::zone_pipeline::ZoneMoveResult::Done => None,
        crate::game::zone_pipeline::ZoneMoveResult::NeedsChoice(_)
        | crate::game::zone_pipeline::ZoneMoveResult::NeedsAuraAttachmentChoice => {
            let mut clear_markers = misses.to_vec();
            clear_markers.push(hit_card);
            crate::game::zone_pipeline::defer_completion_on_pause(
                state,
                crate::types::game_state::BatchCompletion::RevealRestPile {
                    player,
                    source_id: Some(source_id),
                    rest_cards: misses.to_vec(),
                    rest_destination,
                    rest_order: DigRestOrder::Preserve,
                    clear_markers,
                    publish_tracked_set: None,
                    emit_reveal_until_resolved: None,
                    manifested_for_continuation: None,
                },
            );
            Some(ResolutionChoiceOutcome::WaitingFor(
                state.waiting_for.clone(),
            ))
        }
    }
}

fn starts_with_pay_amount_prompt(ability: &ResolvedAbility) -> bool {
    match &ability.effect {
        Effect::PayCost {
            cost: AbilityCost::Mana { cost },
            scale: None,
            ..
        } => casting_costs::cost_has_x(cost),
        Effect::PayCost {
            cost: AbilityCost::PayEnergy { amount },
            ..
        } => matches!(
            amount,
            QuantityExpr::Ref {
                qty: QuantityRef::Variable { name },
            } if name == "X"
        ),
        _ => false,
    }
}

/// CR 700.3: Pop the first `PileResult` from a completed ledger, returning
/// it alongside the remaining queue. Helper for the partition→choice
/// transition.
fn pop_first_pile_result(
    mut completed: crate::im::Vector<crate::types::game_state::PileResult>,
) -> (
    crate::types::game_state::PileResult,
    crate::im::Vector<crate::types::game_state::PileResult>,
) {
    let first = completed
        .pop_front()
        .expect("at least one completed pile result");
    (first, completed)
}

fn effect_zone_library_placement_order(
    chosen: &[ObjectId],
    library_position: &LibraryPosition,
) -> Vec<ObjectId> {
    match library_position {
        LibraryPosition::Top => chosen.iter().rev().copied().collect(),
        LibraryPosition::Bottom | LibraryPosition::NthFromTop { .. } => chosen.to_vec(),
        LibraryPosition::BeneathTop { .. } | LibraryPosition::RandomWithinTop { .. } => {
            unreachable!("EffectZoneChoice normalizes unsupported library positions to Top")
        }
    }
}

/// CR 401.4: Deliver Hand-origin cards in the order whose resulting relative
/// order matches the raw mixed-source placement. The later Library-only replay
/// can change their interleaving with library cards, but never their own order.
fn effect_zone_non_library_delivery_order(
    state: &GameState,
    chosen: &[ObjectId],
    library_origin: &[ObjectId],
    library_position: &LibraryPosition,
) -> Vec<ObjectId> {
    let mut owners = Vec::new();
    for &card_id in chosen {
        let owner = state.objects[&card_id].owner;
        if !owners.contains(&owner) {
            owners.push(owner);
        }
    }

    let mut delivery_order = Vec::new();
    for owner in owners {
        let library = state
            .players
            .iter()
            .find(|player| player.id == owner)
            .expect("library owner exists")
            .library
            .iter()
            .copied()
            .collect();
        let desired = replay_effect_zone_library_placement(
            state,
            owner,
            library,
            chosen,
            chosen,
            library_position,
        );
        let non_library: Vec<_> = desired
            .into_iter()
            .filter(|card_id| chosen.contains(card_id) && !library_origin.contains(card_id))
            .collect();
        match library_position {
            LibraryPosition::Top | LibraryPosition::NthFromTop { .. } => {
                delivery_order.extend(non_library.into_iter().rev());
            }
            LibraryPosition::Bottom => delivery_order.extend(non_library),
            LibraryPosition::BeneathTop { .. } | LibraryPosition::RandomWithinTop { .. } => {
                unreachable!("EffectZoneChoice normalizes unsupported library positions to Top")
            }
        }
    }
    delivery_order
}

fn replay_effect_zone_library_placement(
    state: &GameState,
    owner: crate::types::player::PlayerId,
    mut library: Vec<ObjectId>,
    chosen: &[ObjectId],
    placed: &[ObjectId],
    library_position: &LibraryPosition,
) -> Vec<ObjectId> {
    for card_id in effect_zone_library_placement_order(chosen, library_position) {
        if state.objects[&card_id].owner != owner || !placed.contains(&card_id) {
            continue;
        }
        library.retain(|id| *id != card_id);
        match library_position {
            LibraryPosition::Top => library.insert(0, card_id),
            LibraryPosition::Bottom => library.push(card_id),
            LibraryPosition::NthFromTop { n } => {
                let index = (n.saturating_sub(1) as usize).min(library.len());
                library.insert(index, card_id);
            }
            LibraryPosition::BeneathTop { .. } | LibraryPosition::RandomWithinTop { .. } => {
                unreachable!("EffectZoneChoice normalizes unsupported library positions to Top")
            }
        }
    }
    library
}

fn move_library_origin_cards_in_selection_order(
    state: &mut GameState,
    chosen: &[ObjectId],
    library_position: &LibraryPosition,
    events: &mut Vec<GameEvent>,
) {
    match library_position {
        LibraryPosition::Bottom => {
            for &card_id in chosen {
                // allow-raw-zone: in-library reposition is not a zone-change event (CR 401.4 + CR 614.1).
                zones::move_to_library_position(state, card_id, false, events);
            }
        }
        LibraryPosition::Top | LibraryPosition::NthFromTop { .. } => {
            let index = match library_position {
                LibraryPosition::Top => Some(0),
                LibraryPosition::NthFromTop { n } => Some(n.saturating_sub(1) as usize),
                _ => unreachable!("matched library position"),
            };
            let placement_order = effect_zone_library_placement_order(chosen, library_position);
            for card_id in placement_order {
                // allow-raw-zone: in-library reposition is not a zone-change event (CR 401.4 + CR 614.1).
                zones::move_to_library_at_index(state, card_id, index, events);
            }
        }
        LibraryPosition::BeneathTop { .. } | LibraryPosition::RandomWithinTop { .. } => {
            unreachable!("EffectZoneChoice normalizes unsupported library positions to Top")
        }
    }
}

/// CR 401.4 + CR 608.2c: Preserve the raw arm's selected-card interleaving
/// after the Hand-origin delivery batch has settled. Reconstruct each affected
/// library without the newly delivered cards, replay the original placement
/// order using only cards that actually landed in a library, then reposition
/// just the cards that began there into the resulting slots.
fn reposition_library_origins_after_batch_delivery(
    state: &mut GameState,
    chosen: &[ObjectId],
    library_origin: &[ObjectId],
    library_position: &LibraryPosition,
    events: &mut Vec<GameEvent>,
) {
    let library_placed: Vec<_> = chosen
        .iter()
        .copied()
        .filter(|card_id| {
            state
                .objects
                .get(card_id)
                .is_some_and(|object| object.zone == Zone::Library)
        })
        .collect();
    let mut owners = Vec::new();
    for &card_id in library_origin {
        let owner = state.objects[&card_id].owner;
        if !owners.contains(&owner) {
            owners.push(owner);
        }
    }

    for owner in owners {
        let library = state
            .players
            .iter()
            .find(|player| player.id == owner)
            .expect("library owner exists")
            .library
            .iter()
            .copied()
            .filter(|id| library_origin.contains(id) || !chosen.contains(id))
            .collect();
        let desired = replay_effect_zone_library_placement(
            state,
            owner,
            library,
            chosen,
            &library_placed,
            library_position,
        );
        for (index, &card_id) in desired.iter().enumerate().rev() {
            if library_origin.contains(&card_id) {
                // allow-raw-zone: in-library reposition is not a zone-change event (CR 401.4 + CR 614.1).
                zones::move_to_library_at_index(state, card_id, Some(index), events);
            }
        }
    }
}

fn finish_effect_zone_put_at_library_position(
    state: &mut GameState,
    player: crate::types::player::PlayerId,
    source_id: ObjectId,
    chosen: Vec<ObjectId>,
    library_origin: Vec<ObjectId>,
    library_position: LibraryPosition,
    events: &mut Vec<GameEvent>,
) {
    reposition_library_origins_after_batch_delivery(
        state,
        &chosen,
        &library_origin,
        &library_position,
        events,
    );
    if let Some(next_owner) = effects::change_zone::resume_next_mass_library_order_choice(state) {
        state.priority_player = next_owner;
        return;
    }
    if state.active_ability_continuation().is_some() {
        let tracked = if matches!(library_position, LibraryPosition::Bottom) {
            state
                .chain_tracked_set_id
                .and_then(|id| state.tracked_object_sets.get(&id).cloned())
                .unwrap_or_default()
                .into_iter()
                .filter(|id| !chosen.contains(id))
                .filter(|id| {
                    state
                        .objects
                        .get(id)
                        .is_some_and(|object| object.zone == Zone::Library)
                })
                .collect()
        } else {
            chosen.clone()
        };
        let tracked_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state.tracked_object_sets.insert(tracked_id, tracked);
        state.chain_tracked_set_id = Some(tracked_id);
    }
    state.last_effect_count = Some(chosen.len() as i32);
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::PutAtLibraryPosition,
        source_id,
        subject: None,
    });
    finish_with_continuation(state, player, events);
}

fn finish_with_continuation(
    state: &mut GameState,
    player: crate::types::player::PlayerId,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    set_priority(state, player);
    super::engine::resume_pending_continuation_if_priority(state, events)
        .expect("a settled resolution choice must resume its continuation");
    state.waiting_for.clone()
}

/// CR 118.12 + CR 119.4 + CR 616.1: Complete the outer pay-amount action only
/// after any interactive post-replacement child of the life payment has
/// settled. Shared by the direct submit path and the deferred-life resumer.
pub(crate) fn finish_pay_amount_choice(
    state: &mut GameState,
    player: crate::types::player::PlayerId,
    total: u32,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    state.last_effect_count = Some(total as i32);
    let pending_starts_with_pay_amount = state
        .active_ability_continuation()
        .is_some_and(|cont| starts_with_pay_amount_prompt(&cont.chain));
    if !pending_starts_with_pay_amount {
        if let Some(frame) = state.active_ability_continuation_frame_mut() {
            frame.pending.chain.set_chosen_x_recursive(total);
        }
    }
    let mut waiting_for = finish_with_continuation(state, player, events);
    if let WaitingFor::PayAmountChoice {
        accumulated: next_accumulated,
        ..
    } = &mut waiting_for
    {
        *next_accumulated = total;
        state.waiting_for = waiting_for.clone();
    }
    waiting_for
}

/// CR 701.25a / CR 616.1: Run the post-loop cleanup a rest-pile batch deferred
/// when it paused mid-pile. Called by
/// `zone_pipeline::drain_pending_batch_deliveries` the moment the batch tail
/// empties, so the kept-card placement / reveal-marker cleanup and the
/// continuation drain happen exactly once — the same effect the synchronous
/// (never-paused) path runs inline. The result reports whether this completion
/// itself parked another replacement-aware delivery, so the enclosing batch
/// caller cannot overwrite the CR 616.1 choice with a later tail.
pub(crate) fn run_batch_completion(
    state: &mut GameState,
    completion: crate::types::game_state::BatchCompletion,
    events: &mut Vec<GameEvent>,
) -> crate::game::zone_pipeline::BatchMoveResult {
    use crate::types::game_state::BatchCompletion;
    match completion {
        BatchCompletion::ReturnAsAuraNoTargetComplete { source_id } => {
            effects::return_as_aura::complete_no_target_delivery(source_id, events)
        }
        BatchCompletion::ExploreLandDeliveryComplete { explorer_id } => {
            effects::explore::complete_land_delivery(explorer_id, events)
        }
        BatchCompletion::CloakExileDeliveryComplete {
            player,
            source_id,
            members,
            enters_under,
        } => effects::cloak::complete_tracked_set_exile_delivery(
            state,
            player,
            source_id,
            members,
            enters_under,
            events,
        ),
        BatchCompletion::ExileFaceDownPileDeliveryComplete {
            player,
            source_id,
            members,
            required_member_count,
        } => effects::exile_face_down_pile::complete_exile_face_down_pile_delivery(
            state,
            player,
            source_id,
            members,
            required_member_count,
            events,
        ),
        BatchCompletion::ExileFaceDownPileReturnComplete { source_id } => {
            effects::exile_face_down_pile::complete_exile_face_down_pile_return(source_id, events)
        }
        BatchCompletion::CastFromZoneExileDeliveryComplete {
            ability,
            in_place_ids,
            exile_delivery_ids,
        } => effects::cast_from_zone::complete_lingering_permissions_after_exile_delivery(
            state,
            &ability,
            &in_place_ids,
            &exile_delivery_ids,
            events,
        ),
        BatchCompletion::CascadeExileLoopComplete {
            controller,
            source_id,
            source_mv,
            exiled_misses,
            current_card,
        } => effects::cascade::complete_exile_loop_step(
            state,
            controller,
            source_id,
            source_mv,
            exiled_misses,
            current_card,
            events,
        ),
        BatchCompletion::CascadeBottomComplete {
            controller,
            source_id,
            exiled_count,
        } => {
            events.push(GameEvent::CascadeMissed {
                controller,
                source_id,
                exiled_count,
            });
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Cascade,
                source_id,
                subject: None,
            });
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::RippleTerminalComplete {
            player,
            source_id,
            final_cast,
        } => {
            // CR 702.60a + CR 603.3b: The final accepted Ripple cast has not yet emitted SpellCast: this
            // batch completion runs inside its pre-finalization cleanup. Mark the
            // terminal settlement now, but leave both the resolver LKI and parked
            // triggers intact until that spell finishes announcement.
            let matches_resolving_ripple =
                state.resolving_stack_entry.as_ref().is_some_and(|entry| {
                    entry.source_id == source_id
                        && entry.controller == player
                        && matches!(
                            &entry.kind,
                            crate::types::game_state::StackEntryKind::TriggeredAbility {
                                ability, ..
                            } if matches!(ability.effect, Effect::Ripple { .. })
                        )
                });
            if matches_resolving_ripple {
                state.pending_resolution_completion =
                    Some(crate::types::game_state::PendingResolutionCompletion {
                        player,
                        source_id,
                        final_cast,
                    });
                // CR 608.2c: the terminal Ripple instruction is complete. The
                // post-action pipeline owns collecting and ordering the parked
                // cast observers; it will keep the marker through any final-cast
                // announcement or replacement tail.
                state.waiting_for = WaitingFor::Priority { player };
            }
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::DiscoverBottomComplete { source_id } => {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Discover,
                source_id,
                subject: None,
            });
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::DiscoverPlacementComplete { source_id } => {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Discover,
                source_id,
                subject: None,
            });
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::DiscoverDeclined {
            player,
            hit_card,
            source_id,
        } => {
            // CR 701.57a + CR 614.1: The hit's printed hand delivery is its own
            // replaceable resolution effect, after the misses reach the library
            // bottom. Carry only the tail through its batch so a CR 616.1 choice
            // pauses before the Discover completion/continuation run.
            crate::game::zone_pipeline::move_objects_simultaneously_then(
                state,
                vec![crate::game::zone_pipeline::ZoneMoveRequest::effect(
                    hit_card,
                    Zone::Hand,
                    source_id,
                )],
                Some(BatchCompletion::DiscoverDeclinedComplete { player, source_id }),
                events,
            )
        }
        BatchCompletion::DiscoverDeclinedComplete { player, source_id } => {
            // CR 701.57a: the declined hit's hand delivery settled (including
            // any CR 616.1 redirect), so the Discover result and continuation
            // run exactly once.
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Discover,
                source_id,
                subject: None,
            });
            finish_with_continuation(state, player, events);
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::ResolutionCastRejectedToHand {
            player,
            hit_card,
            source_id,
        } => {
            // CR 608.2g + CR 701.57a + CR 614.1: A rejected Discover cast
            // returns the hit through the same replaceable effect-owned Hand
            // delivery. Its priority tail must wait for that delivery to settle.
            crate::game::zone_pipeline::move_objects_simultaneously_then(
                state,
                vec![crate::game::zone_pipeline::ZoneMoveRequest::effect(
                    hit_card,
                    Zone::Hand,
                    source_id,
                )],
                Some(BatchCompletion::ResolutionCastRejectionComplete { player, source_id }),
                events,
            )
        }
        BatchCompletion::ResolutionCastRejectionComplete { player, source_id } => {
            // CR 608.2g + CR 701.57a: the rejected hit's hand delivery settled
            // (including any CR 616.1 redirect), so the Discover result and
            // priority restoration run exactly once.
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Discover,
                source_id,
                subject: None,
            });
            crate::game::priority::clear_priority_passes(state);
            state.waiting_for = WaitingFor::Priority { player };
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::PutOnTopComplete {
            source_id,
            removed_exile_links,
        } => {
            state.exile_links.retain(|link| {
                link.source_id != source_id || !removed_exile_links.contains(&link.exiled_id)
            });
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::PutAtLibraryPosition,
                source_id,
                subject: None,
            });
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::TopOrBottomComplete { player } => {
            finish_with_continuation(state, player, events);
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::EffectZonePutAtLibraryPositionComplete {
            player,
            source_id,
            chosen,
            library_origin,
            library_position,
        } => {
            finish_effect_zone_put_at_library_position(
                state,
                player,
                source_id,
                chosen,
                library_origin,
                library_position,
                events,
            );
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::DigKeptDeliveryComplete {
            player,
            source_id,
            rest_cards,
            rest_destination,
            rest_order,
            publish_tracked_set,
            continuation_targets,
        } => {
            if !rest_cards.is_empty() {
                match route_rest_partition(
                    state,
                    &rest_cards,
                    rest_destination,
                    rest_order,
                    source_id,
                    events,
                ) {
                    crate::game::zone_pipeline::BatchMoveResult::Done => {}
                    crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => {
                        crate::game::zone_pipeline::defer_completion_on_pause(
                            state,
                            BatchCompletion::DigKeptDeliveryComplete {
                                player,
                                source_id,
                                rest_cards: Vec::new(),
                                rest_destination,
                                rest_order,
                                publish_tracked_set,
                                continuation_targets,
                            },
                        );
                        return crate::game::zone_pipeline::BatchMoveResult::NeedsChoice;
                    }
                }
            }
            effects::publish_fresh_tracked_set(state, publish_tracked_set);
            if let Some(frame) = state.active_ability_continuation_frame_mut() {
                frame.pending.chain.targets = continuation_targets
                    .iter()
                    .map(|&id| TargetRef::Object(id))
                    .collect();
                frame.pending.chain.context.optional_effect_performed =
                    !continuation_targets.is_empty();
            }
            finish_with_continuation(state, player, events);
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::ForEachCategoryExileComplete {
            ability,
            pool,
            remaining_member_filters,
            chosen,
        } => {
            effects::choose_from_zone::complete_per_category_exile(
                state,
                ability,
                pool,
                remaining_member_filters,
                chosen,
                events,
            );
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::DrawnThisTurnTopdeckComplete {
            player,
            life_payment,
            payment_count,
            topdecked_count,
            source_id,
        } => {
            effects::drawn_this_turn_choice::complete_topdeck_choice(
                state,
                player,
                life_payment,
                payment_count,
                topdecked_count,
                source_id,
                events,
            );
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::SurveilKeepOnTop { player, top_cards } => {
            surveil_keep_on_top(state, player, &top_cards);
            finish_with_continuation(state, player, events);
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::ManifestDreadCleanup { player, revealed } => {
            for card_id in &revealed {
                state.revealed_cards.remove(card_id);
            }
            finish_with_continuation(state, player, events);
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        // CR 701.20b: The kept card's battlefield entry paused (aura host pick /
        // replacement ordering). Now that it has resolved, move the unkept rest
        // pile, clear the reveal markers, run the dig tracked-set publish +
        // continuation wiring (if any), then drain the continuation — exactly the
        // tail the synchronous path runs inline.
        BatchCompletion::RevealRestPile {
            player,
            source_id,
            rest_cards,
            rest_destination,
            rest_order,
            clear_markers,
            publish_tracked_set,
            emit_reveal_until_resolved,
            manifested_for_continuation,
        } => {
            // The dig path (`publish_tracked_set.is_some()`) routes the rest pile
            // through `route_rest_partition` (ordered library bottom); the
            // reveal-until path routes through `move_rest_then`, including
            // Library-bottom placement and any CR 616.1 pause. Dispatch on the
            // dig-only payload so each site keeps its synchronous semantics.
            if publish_tracked_set.is_some() {
                match route_rest_partition(
                    state,
                    &rest_cards,
                    rest_destination,
                    rest_order,
                    source_id,
                    events,
                ) {
                    crate::game::zone_pipeline::BatchMoveResult::Done => {}
                    crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => {
                        crate::game::zone_pipeline::defer_completion_on_pause(
                            state,
                            BatchCompletion::RevealRestPile {
                                player,
                                source_id,
                                rest_cards: Vec::new(),
                                rest_destination,
                                rest_order,
                                clear_markers,
                                publish_tracked_set,
                                emit_reveal_until_resolved,
                                manifested_for_continuation,
                            },
                        );
                        return crate::game::zone_pipeline::BatchMoveResult::NeedsChoice;
                    }
                }
            } else if !rest_cards.is_empty() {
                // CR 701.20a + CR 616.1: Reveal-until rest piles are fully
                // pipeline-owned, including Library-bottom placement. If a
                // Library-destination `Moved` replacement pauses here, re-stash
                // this completion as cleanup-only so reveal markers and
                // continuation drain run after the pile actually lands.
                let cleanup = BatchCompletion::RevealRestPile {
                    player,
                    source_id,
                    rest_cards: Vec::new(),
                    rest_destination,
                    rest_order: DigRestOrder::Preserve,
                    clear_markers,
                    publish_tracked_set: None,
                    emit_reveal_until_resolved,
                    manifested_for_continuation,
                };
                return effects::reveal_until::move_rest_then(
                    state,
                    &rest_cards,
                    rest_destination,
                    Some(cleanup),
                    events,
                );
            }
            state
                .resolve_and_apply_information(
                    &clear_markers,
                    ResolvedInformationAudience::Controller(player),
                    ResolvedInformationLifetime::UntilActionBoundary,
                    ResolvedInformationEdit::Hide,
                )
                .expect("reveal-rest cleanup must reference live card occurrences");
            if let Some(kept) = publish_tracked_set {
                effects::publish_fresh_tracked_set(state, kept.clone());
                if let Some(frame) = state.active_ability_continuation_frame_mut() {
                    frame.pending.chain.targets =
                        kept.iter().map(|&id| TargetRef::Object(id)).collect();
                    frame.pending.chain.context.optional_effect_performed = !kept.is_empty();
                }
            }
            if let Some(source_id) = emit_reveal_until_resolved {
                events.push(crate::types::events::GameEvent::EffectResolved {
                    kind: crate::types::ability::EffectKind::RevealUntil,
                    source_id,
                    subject: None,
                });
            }
            // CR 608.2c + CR 701.62a (#7467): the paused manifest entry has
            // completed by now — publish its object for the parked consumer,
            // the deferred mirror of the synchronous `ManifestDreadChoice`
            // publish (same gate, same battlefield filter).
            if let Some(manifested) = manifested_for_continuation {
                effects::publish_battlefield_object_for_pending_continuation(state, manifested);
            }
            finish_with_continuation(state, player, events);
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::DigMassPutAllRestComplete {
            player,
            source_id,
            selected,
            destination,
            enter_tapped,
            enters_attacking,
        } => {
            crate::game::effects::dig::move_mass_put_all_selected(
                state,
                player,
                source_id,
                selected,
                destination,
                enter_tapped,
                enters_attacking,
                events,
            );
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::DigMassPutAllComplete {
            player: _,
            source_id,
            selected,
            destination,
        } => {
            // CR 608.2c + CR 614.1: A replacement can redirect an individual
            // selected card, so "those" refers only to cards that actually
            // reached the requested destination after the full batch settles.
            let delivered = selected
                .into_iter()
                .filter(|id| {
                    state
                        .objects
                        .get(id)
                        .is_some_and(|obj| obj.zone == destination)
                })
                .collect();
            effects::publish_fresh_tracked_set(state, delivered);
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Dig,
                source_id,
                subject: None,
            });
            // The synchronous resolver still owns its own sub-ability traversal;
            // on a paused path `engine_replacement` drains the already-stashed
            // continuation after this batch completion returns. Do not drain it
            // here, or a synchronous mass Dig could run an outer continuation
            // ahead of its own child instruction.
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::DigPriorLookRestComplete { player, source_id } => {
            state.private_look_ids.clear();
            state.private_look_player = None;
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Dig,
                source_id,
                subject: None,
            });
            finish_with_continuation(state, player, events);
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        // CR 610.3: the exile-until-leaves return pile has fully landed (after a
        // returned creature's as-enters / aura-host pause resolved). Drop the
        // spent `UntilSourceLeaves` links now — deferred so it runs exactly once
        // after the paused card finished returning, not before. No priority /
        // continuation drain here: this completion rides an SBA-time return
        // (`check_exile_returns`), whose surrounding pipeline owns priority.
        BatchCompletion::RemoveExileLinks { returned_ids } => {
            state
                .exile_links
                .retain(|link| !returned_ids.contains(&link.exiled_id));
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        // CR 702.49 + CR 616.1: the ninja's parked battlefield entry resolved —
        // run the deferred post-entry ninjutsu work (cast-variant tag,
        // CR 702.49c combat placement, CR 702.49a trigger event) exactly once.
        // No priority/continuation drain: ninjutsu is a keyword activation
        // whose surrounding action pipeline owns priority.
        BatchCompletion::NinjutsuPlacement {
            player,
            ninjutsu_obj_id,
            cast_variant,
            defending_player,
            attack_target,
        } => {
            crate::game::keywords::finish_ninjutsu_entry(
                state,
                player,
                ninjutsu_obj_id,
                cast_variant,
                defending_player,
                attack_target,
                events,
            );
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        // CR 701.51 + CR 616.1: the paused Attraction's entry resolved — finish
        // its open bookkeeping, then run the remaining opens of the same
        // instruction (which may themselves pause and re-defer through this
        // same completion; `drain_pending_batch_deliveries` settles and pops
        // the old BatchDelivery frame before calling here, so a fresh park is
        // preserved).
        BatchCompletion::AttractionOpenRemainder {
            player,
            object_id,
            remaining,
        } => {
            crate::game::attractions::finish_attraction_open(state, player, object_id, events);
            if remaining > 0 {
                // CR 609.3 inside: opens as many as possible; never errors.
                let _ =
                    crate::game::attractions::open_attractions(state, player, remaining, events);
            }
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::ContraptionAssembleRemainder {
            player,
            source_id,
            object_id,
            sprocket,
            remaining_after,
        } => {
            crate::game::contraptions::finish_contraption_assembly(
                state, player, object_id, sprocket, events,
            );
            if remaining_after > 0 {
                crate::game::contraptions::continue_assemble_batch(
                    state,
                    player,
                    source_id,
                    remaining_after,
                    events,
                );
            }
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::LibrarySearchDeliverySettled { resume } => match resume {
            crate::types::game_state::LibrarySearchDeliveryResume::Standard { searcher } => {
                state.active_library_searches.remove(&searcher);
                state.active_search_decision_controls.remove(&searcher);
                crate::game::zone_pipeline::BatchMoveResult::Done
            }
            crate::types::game_state::LibrarySearchDeliveryResume::Scoped {
                player,
                source_id,
                search_keys,
                grants,
                after_scope,
            } => {
                // CR 101.4 + CR 701.23i + CR 616.1: the parked batch has finally
                // delivered every selected card. The searched-this-way shuffle tail
                // may now resolve; a failure here would mean corrupted serialized
                // engine state because the completion is created only by the typed
                // scoped-search protocol above.
                for searcher in &search_keys {
                    state.active_library_searches.remove(searcher);
                    state.active_search_decision_controls.remove(searcher);
                }
                state.pending_scoped_library_search = None;
                for (identity, grant) in grants {
                    if state
                        .objects
                        .get(&identity.object_id)
                        .is_some_and(|object| {
                            object.zone == Zone::Exile
                                && object.incarnation == identity.incarnation.saturating_add(1)
                        })
                    {
                        grant_search_found_permission_after_delivery(
                            state,
                            identity.object_id,
                            Some(grant),
                            events,
                        );
                    }
                }
                effects::scoped_library_search::finish_delivery_tail(
                    state,
                    player,
                    source_id,
                    after_scope,
                    events,
                )
                .expect("scoped library search batch completion must resolve");
                crate::game::zone_pipeline::BatchMoveResult::Done
            }
        },
        BatchCompletion::SearchPartitionPrimaryDelivered {
            rest_ids,
            rest_destination,
            source_id,
            resume,
        } => route_rest_partition_then(
            state,
            &rest_ids,
            rest_destination,
            Some(source_id),
            Some(BatchCompletion::LibrarySearchDeliverySettled { resume }),
            events,
        ),
        BatchCompletion::SearchFoundZoneDelivery { object_id, grant } => {
            resume_search_found_after_zone_delivery(state, object_id, grant, events);
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::MeldExile { context } => {
            crate::game::meld::finish_meld_exile(state, context, events);
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::MeldEntry {
            context,
            attack_target,
        } => {
            crate::game::meld::finish_meld_delivery(state, context, attack_target, events);
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
        BatchCompletion::MeldRedirect { source_id } => {
            crate::game::meld::finish_deferred_meld_resolution(state, source_id, events);
            crate::game::zone_pipeline::BatchMoveResult::Done
        }
    }
}

pub(crate) fn settle_pending_library_search_delivery(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) {
    if let Some(resume) = state.pending_library_search_delivery.take() {
        let _ = run_batch_completion(
            state,
            crate::types::game_state::BatchCompletion::LibrarySearchDeliverySettled { resume },
            events,
        );
    }
}

/// CR 701.25a: place the kept surveil cards on top of the player's library in
/// the chosen order (`top_cards[0]` becomes the topmost card). Shared by the
/// synchronous surveil handler and the deferred batch completion so the ordering
/// is identical on both paths.
fn surveil_keep_on_top(
    state: &mut GameState,
    player: crate::types::player::PlayerId,
    top_cards: &[ObjectId],
) {
    zones::reorder_within_library(state, player, top_cards, Some(0));
}

fn resume_with_error_propagation(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    super::engine::resume_pending_continuation_if_priority(state, events)
}

fn propagate_targets_through_search_shuffle(ability: &mut ResolvedAbility, targets: &[TargetRef]) {
    let mut cursor = ability;
    while matches!(cursor.effect, Effect::Shuffle { .. }) {
        let Some(next) = cursor.sub_ability.as_mut() else {
            return;
        };
        if next.targets.is_empty() {
            next.targets = targets.to_vec();
        }
        cursor = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, CastingPermission, Duration, FilterProp,
        ManaSpendPermission, PermissionGrantee, QuantityExpr, ReplacementDefinition,
        ReplacementMode, ReplacementPlayerScope, SearchSelectionConstraint, StaticDefinition,
        TargetFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::proposed_event::ReplacementId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::{ProhibitionScope, StaticMode};

    fn resolution_choice_source(
        state: &GameState,
        object_id: ObjectId,
    ) -> crate::types::game_state::NamedChoiceSource {
        let context = crate::game::triggers::trigger_source_context_for_latch(
            state,
            state.objects.get(&object_id).unwrap(),
        );
        crate::types::game_state::NamedChoiceSource::from_trigger_source(
            context,
            crate::types::game_state::NamedChoiceSourceBinding::ResolutionContext,
        )
    }

    fn search_found_redirect(destination: Zone) -> ReplacementDefinition {
        let execute = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination,
                target: TargetFilter::ParentTarget,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: Vec::new(),
                conditional_enter_with_counters: Vec::new(),
                face_down_profile: None,
                enters_modified_if: None,
            },
        );
        let mut replacement =
            ReplacementDefinition::new(ReplacementEvent::SearchFound).execute(execute);
        replacement.valid_player = Some(ReplacementPlayerScope::AnyPlayer);
        replacement
    }

    fn search_found_redirect_with_grant(
        mana_spend_permission: Option<ManaSpendPermission>,
    ) -> ReplacementDefinition {
        let mut replacement = search_found_redirect(Zone::Exile);
        let execute = replacement
            .execute
            .as_mut()
            .expect("redirect has execution");
        execute.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    duration: Duration::Permanent,
                    granted_to: PlayerId(0),
                    frequency: crate::types::statics::CastFrequency::Unlimited,
                    source_id: None,
                    exiled_by_ability_controller: None,
                    mana_spend_permission,
                    card_filter: None,
                    single_use_group: None,
                    single_use: false,
                    cast_cost_raise: None,
                    land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    invalidation: None,
                },
                target: TargetFilter::ParentTarget,
                grantee: PermissionGrantee::AbilityController,
            },
        )));
        replacement
    }

    fn install_search_found_redirect(
        state: &mut GameState,
        controller: PlayerId,
        card_id: u64,
        destination: Zone,
    ) -> ObjectId {
        let source = create_object(
            state,
            CardId(card_id),
            controller,
            format!("Found-card redirect {card_id}"),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .expect("replacement source exists")
            .replacement_definitions
            .push(search_found_redirect(destination));
        source
    }

    fn install_moved_exile_redirect(
        state: &mut GameState,
        destination: Zone,
        optional: bool,
    ) -> ObjectId {
        let source = create_object(
            state,
            CardId(90_090),
            PlayerId(0),
            "Exile move redirect".to_string(),
            Zone::Battlefield,
        );
        let mut replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: None,
                    destination,
                    target: TargetFilter::Any,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: Vec::new(),
                    conditional_enter_with_counters: Vec::new(),
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            ))
            .destination_zone(Zone::Exile)
            .valid_card(TargetFilter::Any);
        if optional {
            replacement.mode = ReplacementMode::Optional { decline: None };
        }
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions
            .push(replacement);
        source
    }

    fn partition_waiting_state() -> (GameState, ObjectId, ObjectId) {
        let player = PlayerId(0);
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(90_080),
            player,
            "Split search source".to_string(),
            Zone::Battlefield,
        );
        let primary = create_object(
            &mut state,
            CardId(90_081),
            player,
            "Primary found card".to_string(),
            Zone::Library,
        );
        let rest = create_object(
            &mut state,
            CardId(90_082),
            player,
            "Rest found card".to_string(),
            Zone::Library,
        );
        state.active_library_searches.insert(
            crate::types::game_state::ActiveLibrarySearch::try_new(
                player,
                player,
                Some(player),
                vec![player],
                vec![
                    (
                        player,
                        Zone::Library,
                        crate::types::identifiers::ObjectIncarnationRef::from_object(
                            &state.objects[&primary],
                        ),
                    ),
                    (
                        player,
                        Zone::Library,
                        crate::types::identifiers::ObjectIncarnationRef::from_object(
                            &state.objects[&rest],
                        ),
                    ),
                ],
            )
            .unwrap(),
        );
        state.waiting_for = WaitingFor::SearchPartitionChoice {
            player,
            cards: vec![primary, rest],
            primary_destination: Zone::Exile,
            primary_count: 1,
            primary_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            rest_destination: Zone::Hand,
            source_id: source,
        };
        (state, primary, rest)
    }

    fn standard_delivery_state() -> (GameState, ObjectId) {
        let player = PlayerId(0);
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(90_083),
            player,
            "Standard search source".to_string(),
            Zone::Battlefield,
        );
        let found = create_object(
            &mut state,
            CardId(90_084),
            player,
            "Standard found card".to_string(),
            Zone::Library,
        );
        state.active_library_searches.insert(
            crate::types::game_state::ActiveLibrarySearch::try_new(
                player,
                player,
                Some(player),
                vec![player],
                vec![(
                    player,
                    Zone::Library,
                    crate::types::identifiers::ObjectIncarnationRef::from_object(
                        &state.objects[&found],
                    ),
                )],
            )
            .unwrap(),
        );
        state.active_search_decision_controls.insert(
            crate::types::game_state::ActiveSearchDecisionControl {
                searcher: player,
                searched_zone_owner: player,
                authority:
                    crate::types::game_state::ActiveSearchDecisionAuthority::SearcherFallback,
            },
        );
        let delivery = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Exile,
                target: TargetFilter::ParentTarget,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: Vec::new(),
                conditional_enter_with_counters: Vec::new(),
                face_down_profile: None,
                enters_modified_if: None,
            },
            Vec::new(),
            source,
            player,
        );
        state.park_ability_continuation(PendingContinuation::new(Box::new(delivery), &state));
        (state, found)
    }

    #[test]
    fn synchronous_standard_delivery_clears_search_after_movement() {
        let (mut state, found) = standard_delivery_state();
        let mut events = Vec::new();

        finalize_standard_search_selection(&mut state, PlayerId(0), &[found], &mut events);

        assert_eq!(state.objects[&found].zone, Zone::Exile);
        assert!(state.active_search_decision_controls.is_empty());
        assert!(state.active_library_searches.is_empty());
        assert!(state.pending_library_search_delivery.is_none());
    }

    #[test]
    fn paused_standard_delivery_retains_visibility_but_not_decision_control() {
        let (mut state, found) = standard_delivery_state();
        install_moved_exile_redirect(&mut state, Zone::Hand, true);
        let mut events = Vec::new();

        finalize_standard_search_selection(&mut state, PlayerId(0), &[found], &mut events);

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert!(state.active_search_decision_controls.is_empty());
        assert!(state.active_library_searches.get(&PlayerId(0)).is_some());
        assert!(state.pending_library_search_delivery.is_some());

        super::super::engine::apply_as_current(
            &mut state,
            GameAction::ChooseReplacement { index: 1 },
        )
        .unwrap();

        assert_eq!(state.objects[&found].zone, Zone::Exile);
        assert!(state.active_library_searches.is_empty());
        assert!(state.pending_library_search_delivery.is_none());
    }

    #[test]
    fn synchronous_split_delivery_clears_visibility_at_batch_settlement() {
        let (mut state, primary, rest) = partition_waiting_state();

        super::super::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![primary],
            },
        )
        .unwrap();

        assert_eq!(state.objects[&primary].zone, Zone::Exile);
        assert_eq!(state.objects[&rest].zone, Zone::Hand);
        assert!(state.active_library_searches.is_empty());
        assert!(state.pending_library_search_delivery.is_none());
    }

    #[test]
    fn paused_split_delivery_retains_visibility_until_replacement_settles() {
        let (mut state, primary, rest) = partition_waiting_state();
        install_moved_exile_redirect(&mut state, Zone::Hand, true);

        super::super::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![primary],
            },
        )
        .unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert!(state.active_library_searches.get(&PlayerId(0)).is_some());

        super::super::engine::apply_as_current(
            &mut state,
            GameAction::ChooseReplacement { index: 1 },
        )
        .unwrap();

        assert_eq!(state.objects[&primary].zone, Zone::Exile);
        assert_eq!(state.objects[&rest].zone, Zone::Hand);
        assert!(state.active_library_searches.is_empty());
        assert!(state.pending_library_search_delivery.is_none());
    }

    fn mixed_zone_search(controller: PlayerId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                    FilterProp::Named {
                        name: "Target".to_string(),
                    },
                ])),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![Zone::Graveyard, Zone::Library],
            },
            Vec::new(),
            ObjectId(90_099),
            controller,
        )
    }

    #[test]
    fn search_found_redirect_removes_card_from_printed_search_result() {
        let mut state = GameState::new_two_player(42);
        install_search_found_redirect(&mut state, PlayerId(0), 90_001, Zone::Exile);
        let found = create_object(
            &mut state,
            CardId(90_002),
            PlayerId(1),
            "Found card".to_string(),
            Zone::Library,
        );

        let survivors = apply_search_found_replacements(
            &mut state,
            PlayerId(1),
            Some(PlayerId(1)),
            &[found],
            crate::types::game_state::PendingSearchFoundContinuation::Standard { split: None },
            false,
            &mut Vec::new(),
        )
        .expect("one mandatory redirect resolves synchronously");

        assert!(survivors.is_empty());
        assert_eq!(state.objects[&found].zone, Zone::Exile);
    }

    #[test]
    fn search_found_resume_omits_a_stale_current_incarnation() {
        let mut state = GameState::new_two_player(42);
        let found = create_object(
            &mut state,
            CardId(90_003),
            PlayerId(1),
            "Stale found card".to_string(),
            Zone::Library,
        );
        let identity =
            crate::types::identifiers::ObjectIncarnationRef::from_object(&state.objects[&found]);
        state.pending_search_found_batch =
            Some(crate::types::game_state::PendingSearchFoundBatch {
                searcher: PlayerId(1),
                library_owner: Some(PlayerId(1)),
                remaining: Vec::new(),
                survivors: Vec::new(),
                current: Some(identity),
                continuation: crate::types::game_state::PendingSearchFoundContinuation::Standard {
                    split: None,
                },
                visibility: true.into(),
            });
        state.objects.get_mut(&found).unwrap().incarnation += 1;
        let event = crate::types::proposed_event::ProposedEvent::SearchFound {
            searcher: PlayerId(1),
            library_owner: Some(PlayerId(1)),
            object_id: found,
            disposition: crate::types::proposed_event::SearchFoundDisposition::Original,
            applied: Default::default(),
        };
        let mut events = Vec::new();

        resume_search_found_after_replacement(&mut state, event, &mut events).unwrap();

        assert!(state.pending_search_found_batch.is_none());
        assert!(state.last_revealed_ids.is_empty());
        assert!(!state.revealed_cards.contains(&found));
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::CardsRevealed { .. })));
    }

    #[test]
    fn search_found_exile_grant_uses_canonical_permission_resolver() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(90_100),
            PlayerId(0),
            "Found-card grant".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions
            .push(search_found_redirect_with_grant(Some(
                ManaSpendPermission::AnyColor,
            )));
        let found = create_object(
            &mut state,
            CardId(90_101),
            PlayerId(1),
            "Found card".to_string(),
            Zone::Graveyard,
        );

        let survivors = apply_search_found_replacements(
            &mut state,
            PlayerId(1),
            Some(PlayerId(1)),
            &[found],
            crate::types::game_state::PendingSearchFoundContinuation::Standard { split: None },
            false,
            &mut Vec::new(),
        )
        .expect("mixed-zone search provenance makes the found card replaceable");

        assert!(survivors.is_empty());
        assert_eq!(state.objects[&found].zone, Zone::Exile);
        assert!(
            matches!(
                state.objects[&found].casting_permissions.as_slice(),
                [CastingPermission::PlayFromExile {
                    duration: Duration::Permanent,
                    granted_to: PlayerId(0),
                    source_id: Some(grant_source),
                    exiled_by_ability_controller: Some(PlayerId(0)),
                    mana_spend_permission: Some(ManaSpendPermission::AnyColor),
                    ..
                }] if *grant_source == source
            ),
            "unexpected permissions: {:?}",
            state.objects[&found].casting_permissions
        );
    }

    #[test]
    fn search_found_grant_rejects_stored_parent_target_controller_grantee() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(90_110),
            PlayerId(0),
            "Wrong stored grantee".to_string(),
            Zone::Battlefield,
        );
        let mut malformed = search_found_redirect_with_grant(None);
        let child = malformed
            .execute
            .as_mut()
            .unwrap()
            .sub_ability
            .as_mut()
            .unwrap();
        let Effect::GrantCastingPermission { grantee, .. } = child.effect.as_mut() else {
            unreachable!()
        };
        *grantee = PermissionGrantee::ParentTargetController;
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions
            .push(malformed);
        let found = create_object(
            &mut state,
            CardId(90_111),
            PlayerId(1),
            "Found card".to_string(),
            Zone::Library,
        );

        let survivors = apply_search_found_replacements(
            &mut state,
            PlayerId(1),
            Some(PlayerId(1)),
            &[found],
            crate::types::game_state::PendingSearchFoundContinuation::Standard { split: None },
            false,
            &mut Vec::new(),
        )
        .expect("noncanonical stored grantee is ignored");

        assert_eq!(survivors, vec![found]);
        assert!(state.objects[&found].casting_permissions.is_empty());
    }

    #[test]
    fn optional_search_found_grant_accepts_once_and_decline_grants_nothing() {
        fn setup() -> (GameState, ObjectId) {
            let mut state = GameState::new_two_player(42);
            let source = create_object(
                &mut state,
                CardId(90_112),
                PlayerId(0),
                "Optional found grant".to_string(),
                Zone::Battlefield,
            );
            let mut replacement = search_found_redirect_with_grant(None);
            replacement.mode = ReplacementMode::Optional { decline: None };
            state
                .objects
                .get_mut(&source)
                .unwrap()
                .replacement_definitions
                .push(replacement);
            let found = create_object(
                &mut state,
                CardId(90_113),
                PlayerId(1),
                "Found card".to_string(),
                Zone::Library,
            );
            (state, found)
        }

        let (mut accepted, accepted_card) = setup();
        apply_search_found_replacements(
            &mut accepted,
            PlayerId(1),
            Some(PlayerId(1)),
            &[accepted_card],
            crate::types::game_state::PendingSearchFoundContinuation::Standard { split: None },
            false,
            &mut Vec::new(),
        )
        .expect_err("optional replacement prompts");
        super::super::engine::apply_as_current(
            &mut accepted,
            GameAction::ChooseReplacement { index: 0 },
        )
        .expect("accept optional found-card grant");
        assert_eq!(accepted.objects[&accepted_card].zone, Zone::Exile);
        assert_eq!(
            accepted.objects[&accepted_card].casting_permissions.len(),
            1
        );

        let (mut declined, declined_card) = setup();
        apply_search_found_replacements(
            &mut declined,
            PlayerId(1),
            Some(PlayerId(1)),
            &[declined_card],
            crate::types::game_state::PendingSearchFoundContinuation::Standard { split: None },
            false,
            &mut Vec::new(),
        )
        .expect_err("optional replacement prompts");
        super::super::engine::apply_as_current(
            &mut declined,
            GameAction::ChooseReplacement { index: 1 },
        )
        .expect("decline optional found-card grant");
        assert_eq!(declined.objects[&declined_card].zone, Zone::Library);
        assert!(declined.objects[&declined_card]
            .casting_permissions
            .is_empty());
    }

    #[test]
    fn mixed_zone_search_production_path_preserves_or_drops_library_provenance() {
        fn install_found_grant(state: &mut GameState) {
            let source = create_object(
                state,
                CardId(90_114),
                PlayerId(1),
                "Found-card grant".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&source)
                .unwrap()
                .replacement_definitions
                .push(search_found_redirect_with_grant(None));
        }

        let mut active = GameState::new_two_player(42);
        install_found_grant(&mut active);
        let graveyard_card = create_object(
            &mut active,
            CardId(90_115),
            PlayerId(0),
            "Target".to_string(),
            Zone::Graveyard,
        );
        create_object(
            &mut active,
            CardId(90_116),
            PlayerId(0),
            "Target".to_string(),
            Zone::Library,
        );
        effects::search_library::resolve(
            &mut active,
            &mixed_zone_search(PlayerId(0)),
            &mut Vec::new(),
        )
        .unwrap();
        assert!(matches!(
            active.waiting_for,
            WaitingFor::SearchChoice {
                library_owner: Some(PlayerId(0)),
                ..
            }
        ));
        super::super::engine::apply_as_current(
            &mut active,
            GameAction::SelectCards {
                cards: vec![graveyard_card],
            },
        )
        .expect("standard SearchChoice routes the nonlibrary card through SearchFound");
        assert_eq!(active.objects[&graveyard_card].zone, Zone::Exile);
        assert_eq!(active.objects[&graveyard_card].casting_permissions.len(), 1);

        let mut muzzled = GameState::new_two_player(42);
        install_found_grant(&mut muzzled);
        let prohibition = create_object(
            &mut muzzled,
            CardId(90_117),
            PlayerId(1),
            "Search prohibition".to_string(),
            Zone::Battlefield,
        );
        let prohibition_object = muzzled.objects.get_mut(&prohibition).unwrap();
        prohibition_object
            .card_types
            .core_types
            .push(CoreType::Enchantment);
        prohibition_object
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantSearchLibrary {
                cause: ProhibitionScope::Opponents,
            }));
        let graveyard_card = create_object(
            &mut muzzled,
            CardId(90_118),
            PlayerId(0),
            "Target".to_string(),
            Zone::Graveyard,
        );
        create_object(
            &mut muzzled,
            CardId(90_119),
            PlayerId(0),
            "Target".to_string(),
            Zone::Library,
        );
        effects::search_library::resolve(
            &mut muzzled,
            &mixed_zone_search(PlayerId(0)),
            &mut Vec::new(),
        )
        .unwrap();
        assert!(matches!(
            muzzled.waiting_for,
            WaitingFor::SearchChoice {
                library_owner: None,
                ..
            }
        ));
        super::super::engine::apply_as_current(
            &mut muzzled,
            GameAction::SelectCards {
                cards: vec![graveyard_card],
            },
        )
        .expect("muzzled mixed search still resolves its graveyard component");
        assert_eq!(muzzled.objects[&graveyard_card].zone, Zone::Graveyard);
        assert!(muzzled.objects[&graveyard_card]
            .casting_permissions
            .is_empty());
    }

    #[test]
    fn scoped_search_production_path_threads_library_provenance_into_found_event() {
        let mut state = GameState::new_two_player(42);
        let replacement_source = create_object(
            &mut state,
            CardId(90_120),
            PlayerId(0),
            "Found-card grant".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&replacement_source)
            .unwrap()
            .replacement_definitions
            .push(search_found_redirect_with_grant(None));
        let found = create_object(
            &mut state,
            CardId(90_121),
            PlayerId(1),
            "Target".to_string(),
            Zone::Graveyard,
        );
        create_object(
            &mut state,
            CardId(90_123),
            PlayerId(1),
            "Library candidate".to_string(),
            Zone::Library,
        );
        let delivery = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::ParentTarget,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: Vec::new(),
                conditional_enter_with_counters: Vec::new(),
                face_down_profile: None,
                enters_modified_if: None,
            },
            Vec::new(),
            ObjectId(90_122),
            PlayerId(0),
        );
        let search = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![Zone::Graveyard, Zone::Library],
            },
            Vec::new(),
            ObjectId(90_122),
            PlayerId(0),
        )
        .sub_ability(delivery);

        let mut muzzled = state.clone();
        let prohibition = create_object(
            &mut muzzled,
            CardId(90_124),
            PlayerId(0),
            "Search prohibition".to_string(),
            Zone::Battlefield,
        );
        muzzled
            .objects
            .get_mut(&prohibition)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantSearchLibrary {
                cause: ProhibitionScope::Opponents,
            }));

        effects::scoped_library_search::start(
            &mut state,
            &search,
            &[PlayerId(1)],
            None,
            &mut Vec::new(),
        )
        .unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::SearchChoice {
                player: PlayerId(1),
                library_owner: Some(PlayerId(1)),
                ..
            }
        ));
        super::super::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards { cards: vec![found] },
        )
        .expect("scoped SearchChoice routes selection through SearchFound");

        assert_eq!(state.objects[&found].zone, Zone::Exile);
        assert_eq!(state.objects[&found].casting_permissions.len(), 1);
        assert!(state.pending_scoped_library_search.is_none());
        assert!(state.pending_search_found_batch.is_none());

        effects::scoped_library_search::start(
            &mut muzzled,
            &search,
            &[PlayerId(1)],
            None,
            &mut Vec::new(),
        )
        .unwrap();
        assert!(matches!(
            muzzled.waiting_for,
            WaitingFor::SearchChoice {
                player: PlayerId(1),
                library_owner: None,
                ..
            }
        ));
        super::super::engine::apply_as_current(
            &mut muzzled,
            GameAction::SelectCards { cards: vec![found] },
        )
        .expect("muzzled scoped search reaches and completes its nonlibrary choice");
        assert_eq!(muzzled.objects[&found].zone, Zone::Battlefield);
        assert!(muzzled.objects[&found].casting_permissions.is_empty());
    }

    #[test]
    fn search_found_grant_is_not_installed_without_library_provenance() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(90_102),
            PlayerId(0),
            "Found-card grant".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions
            .push(search_found_redirect_with_grant(Some(
                ManaSpendPermission::AnyColor,
            )));
        let selected = create_object(
            &mut state,
            CardId(90_103),
            PlayerId(1),
            "Nonlibrary selection".to_string(),
            Zone::Graveyard,
        );

        let survivors = apply_search_found_replacements(
            &mut state,
            PlayerId(1),
            None,
            &[selected],
            crate::types::game_state::PendingSearchFoundContinuation::Standard { split: None },
            false,
            &mut Vec::new(),
        )
        .expect("nonlibrary-only search is not replaceable");

        assert_eq!(survivors, vec![selected]);
        assert_eq!(state.objects[&selected].zone, Zone::Graveyard);
        assert!(state.objects[&selected].casting_permissions.is_empty());
    }

    #[test]
    fn deferred_search_found_grant_waits_for_real_zone_pipeline_completion() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(90_104),
            PlayerId(0),
            "Found-card grant".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions
            .push(search_found_redirect_with_grant(None));
        install_moved_exile_redirect(&mut state, Zone::Graveyard, true);
        let found = create_object(
            &mut state,
            CardId(90_105),
            PlayerId(1),
            "Delivered card".to_string(),
            Zone::Library,
        );

        apply_search_found_replacements(
            &mut state,
            PlayerId(1),
            Some(PlayerId(1)),
            &[found],
            crate::types::game_state::PendingSearchFoundContinuation::Standard { split: None },
            false,
            &mut Vec::new(),
        )
        .expect_err("optional Moved redirect pauses the SearchFound delivery");
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert!(state.objects[&found].casting_permissions.is_empty());

        super::super::engine::apply_as_current(
            &mut state,
            GameAction::ChooseReplacement { index: 1 },
        )
        .expect("declining the inner redirect completes the original exile move");

        assert_eq!(state.objects[&found].zone, Zone::Exile);
        assert!(matches!(
            state.objects[&found].casting_permissions.as_slice(),
            [CastingPermission::PlayFromExile {
                granted_to: PlayerId(0),
                exiled_by_ability_controller: Some(PlayerId(0)),
                ..
            }]
        ));
    }

    #[test]
    fn real_zone_pipeline_redirect_does_not_install_search_found_grant() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(90_106),
            PlayerId(0),
            "Found-card grant".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions
            .push(search_found_redirect_with_grant(Some(
                ManaSpendPermission::AnyColor,
            )));
        install_moved_exile_redirect(&mut state, Zone::Graveyard, false);
        let found = create_object(
            &mut state,
            CardId(90_107),
            PlayerId(1),
            "Redirected card".to_string(),
            Zone::Library,
        );

        let survivors = apply_search_found_replacements(
            &mut state,
            PlayerId(1),
            Some(PlayerId(1)),
            &[found],
            crate::types::game_state::PendingSearchFoundContinuation::Standard { split: None },
            false,
            &mut Vec::new(),
        )
        .expect("mandatory inner redirect resolves synchronously");

        assert!(survivors.is_empty());
        assert_eq!(state.objects[&found].zone, Zone::Graveyard);
        assert!(state.objects[&found].casting_permissions.is_empty());
    }

    #[test]
    fn search_found_grant_rejects_noncanonical_permission_tree() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(90_108),
            PlayerId(0),
            "Malformed grant".to_string(),
            Zone::Battlefield,
        );
        let mut malformed = search_found_redirect_with_grant(None);
        malformed
            .execute
            .as_mut()
            .unwrap()
            .sub_ability
            .as_mut()
            .unwrap()
            .optional = true;
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions
            .push(malformed);
        let found = create_object(
            &mut state,
            CardId(90_109),
            PlayerId(1),
            "Found card".to_string(),
            Zone::Library,
        );

        let survivors = apply_search_found_replacements(
            &mut state,
            PlayerId(1),
            Some(PlayerId(1)),
            &[found],
            crate::types::game_state::PendingSearchFoundContinuation::Standard { split: None },
            false,
            &mut Vec::new(),
        )
        .expect("malformed definition is ignored");

        assert_eq!(survivors, vec![found]);
        assert_eq!(state.objects[&found].zone, Zone::Library);
    }

    #[test]
    fn search_choice_action_routes_found_card_through_replacement_before_printed_destination() {
        let mut state = GameState::new_two_player(42);
        let source = install_search_found_redirect(&mut state, PlayerId(0), 90_010, Zone::Exile);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions[0]
            .mode = ReplacementMode::Optional { decline: None };
        let found = create_object(
            &mut state,
            CardId(90_011),
            PlayerId(1),
            "Found card".to_string(),
            Zone::Library,
        );
        let printed_destination = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: Vec::new(),
                conditional_enter_with_counters: Vec::new(),
                face_down_profile: None,
                enters_modified_if: None,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        state.park_ability_continuation(crate::types::game_state::PendingContinuation::new(
            Box::new(printed_destination),
            &state,
        ));
        state.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(1),
            library_owner: Some(PlayerId(1)),
            cards: vec![found],
            count: 1,
            reveal: false,
            up_to: false,
            allows_partial_find: false,
            constraint: crate::types::ability::SearchSelectionConstraint::None,
            ordering_hint: Default::default(),
            split: None,
        };

        super::super::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards { cards: vec![found] },
        )
        .expect("the public SearchChoice boundary offers the optional replacement");
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));

        super::super::engine::apply_as_current(
            &mut state,
            GameAction::ChooseReplacement { index: 0 },
        )
        .expect("accepting the redirect resumes the printed search continuation");

        assert_eq!(state.objects[&found].zone, Zone::Exile);
        assert!(state.active_ability_continuation().is_none());
        assert!(state.pending_search_found_batch.is_none());
    }

    #[test]
    fn search_found_redirect_ignores_cards_selected_outside_a_library() {
        let mut state = GameState::new_two_player(42);
        install_search_found_redirect(&mut state, PlayerId(0), 90_008, Zone::Exile);
        let selected = create_object(
            &mut state,
            CardId(90_009),
            PlayerId(1),
            "Selected graveyard card".to_string(),
            Zone::Graveyard,
        );

        let survivors = apply_search_found_replacements(
            &mut state,
            PlayerId(1),
            None,
            &[selected],
            crate::types::game_state::PendingSearchFoundContinuation::Scoped,
            false,
            &mut Vec::new(),
        )
        .expect("a nonlibrary selection has no found-card event");

        assert_eq!(survivors, vec![selected]);
        assert_eq!(state.objects[&selected].zone, Zone::Graveyard);
    }

    #[test]
    fn declining_optional_search_found_redirect_preserves_original_result() {
        let mut state = GameState::new_two_player(42);
        let source = install_search_found_redirect(&mut state, PlayerId(0), 90_003, Zone::Exile);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions[0]
            .mode = ReplacementMode::Optional { decline: None };
        let found = create_object(
            &mut state,
            CardId(90_004),
            PlayerId(1),
            "Found card".to_string(),
            Zone::Library,
        );

        let waiting = apply_search_found_replacements(
            &mut state,
            PlayerId(1),
            Some(PlayerId(1)),
            &[found],
            crate::types::game_state::PendingSearchFoundContinuation::Standard { split: None },
            false,
            &mut Vec::new(),
        )
        .expect_err("an optional redirect prompts");
        assert!(matches!(*waiting, WaitingFor::ReplacementChoice { .. }));

        super::super::engine::apply_as_current(
            &mut state,
            GameAction::ChooseReplacement { index: 1 },
        )
        .expect("decline resumes through the public action boundary");

        assert_eq!(state.objects[&found].zone, Zone::Library);
        assert!(state.pending_search_found_batch.is_none());
    }

    #[test]
    fn mixed_search_found_ordering_preserves_optional_decline() {
        let mut state = GameState::new_two_player(42);
        let optional_source =
            install_search_found_redirect(&mut state, PlayerId(0), 90_012, Zone::Exile);
        state
            .objects
            .get_mut(&optional_source)
            .unwrap()
            .replacement_definitions[0]
            .mode = ReplacementMode::Optional { decline: None };
        let mandatory_source =
            install_search_found_redirect(&mut state, PlayerId(0), 90_013, Zone::Graveyard);
        let found = create_object(
            &mut state,
            CardId(90_014),
            PlayerId(1),
            "Found card".to_string(),
            Zone::Library,
        );

        apply_search_found_replacements(
            &mut state,
            PlayerId(1),
            Some(PlayerId(1)),
            &[found],
            crate::types::game_state::PendingSearchFoundContinuation::Standard { split: None },
            false,
            &mut Vec::new(),
        )
        .expect_err("mixed redirects require CR 616 ordering");
        let optional_index = state
            .pending_replacement
            .as_ref()
            .unwrap()
            .search_found_candidates
            .iter()
            .position(|candidate| candidate.disposition.destination == Zone::Exile)
            .expect("optional exile candidate exists");

        super::super::engine::apply_as_current(
            &mut state,
            GameAction::ChooseReplacement {
                index: optional_index,
            },
        )
        .expect("ordering an optional candidate opens its accept/decline prompt");
        let pending = state
            .pending_replacement
            .as_ref()
            .expect("optional candidate remains parked");
        assert!(pending.is_optional);
        assert_eq!(
            pending.candidates,
            vec![ReplacementId {
                source: optional_source,
                index: 0,
            }]
        );
        assert_eq!(
            pending.search_found_candidates.len(),
            2,
            "the mandatory frozen candidate must survive the nested optional prompt"
        );
        state.objects.remove(&mandatory_source);
        state
            .battlefield
            .retain(|object_id| *object_id != mandatory_source);

        super::super::engine::apply_as_current(
            &mut state,
            GameAction::ChooseReplacement { index: 1 },
        )
        .expect("declining the optional redirect resumes the mandatory candidate");

        assert_eq!(state.objects[&found].zone, Zone::Graveyard);
        assert!(state.pending_search_found_batch.is_none());
    }

    #[test]
    fn multiple_optional_search_found_ordering_declines_one_before_accepting_another() {
        let mut state = GameState::new_two_player(42);
        let exile_source =
            install_search_found_redirect(&mut state, PlayerId(0), 90_015, Zone::Exile);
        let graveyard_source =
            install_search_found_redirect(&mut state, PlayerId(0), 90_016, Zone::Graveyard);
        for source in [exile_source, graveyard_source] {
            state
                .objects
                .get_mut(&source)
                .unwrap()
                .replacement_definitions[0]
                .mode = ReplacementMode::Optional { decline: None };
        }
        let found = create_object(
            &mut state,
            CardId(90_017),
            PlayerId(1),
            "Found card".to_string(),
            Zone::Library,
        );

        apply_search_found_replacements(
            &mut state,
            PlayerId(1),
            Some(PlayerId(1)),
            &[found],
            crate::types::game_state::PendingSearchFoundContinuation::Standard { split: None },
            false,
            &mut Vec::new(),
        )
        .expect_err("multiple optional redirects require CR 616 ordering");
        let exile_index = state
            .pending_replacement
            .as_ref()
            .unwrap()
            .search_found_candidates
            .iter()
            .position(|candidate| candidate.disposition.destination == Zone::Exile)
            .expect("optional exile candidate exists");

        super::super::engine::apply_as_current(
            &mut state,
            GameAction::ChooseReplacement { index: exile_index },
        )
        .expect("ordering the first optional candidate opens accept/decline");
        super::super::engine::apply_as_current(
            &mut state,
            GameAction::ChooseReplacement { index: 1 },
        )
        .expect("declining the first candidate exposes the remaining optional candidate");
        let pending = state
            .pending_replacement
            .as_ref()
            .expect("remaining optional candidate is parked");
        assert!(pending.is_optional);
        assert_eq!(pending.search_found_candidates.len(), 1);
        assert_eq!(
            pending.search_found_candidates[0].disposition.destination,
            Zone::Graveyard
        );

        super::super::engine::apply_as_current(
            &mut state,
            GameAction::ChooseReplacement { index: 0 },
        )
        .expect("accepting the remaining candidate resumes the search");

        assert_eq!(state.objects[&found].zone, Zone::Graveyard);
        assert!(state.pending_search_found_batch.is_none());
    }

    #[test]
    fn search_found_ordering_uses_serialized_grant_after_source_reincarnates() {
        let mut state = GameState::new_two_player(42);
        let grant_source = create_object(
            &mut state,
            CardId(90_005),
            PlayerId(0),
            "Snapshotted grant".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&grant_source)
            .unwrap()
            .replacement_definitions
            .push(search_found_redirect_with_grant(Some(
                ManaSpendPermission::AnyColor,
            )));
        install_search_found_redirect(&mut state, PlayerId(0), 90_006, Zone::Graveyard);
        let found = create_object(
            &mut state,
            CardId(90_007),
            PlayerId(1),
            "Found card".to_string(),
            Zone::Library,
        );

        apply_search_found_replacements(
            &mut state,
            PlayerId(1),
            Some(PlayerId(1)),
            &[found],
            crate::types::game_state::PendingSearchFoundContinuation::Standard { split: None },
            false,
            &mut Vec::new(),
        )
        .expect_err("two redirects require CR 616 ordering");

        let bound = state
            .pending_replacement
            .as_ref()
            .unwrap()
            .search_found_candidates
            .iter()
            .find_map(|candidate| candidate.disposition.grant)
            .expect("grant-bearing candidate was snapshotted before serialization");
        assert_eq!(bound.source.object_id, grant_source);
        assert_eq!(bound.controller, PlayerId(0));
        assert_eq!(bound.grantee, PlayerId(0));
        assert_eq!(
            bound.mana_spend_permission,
            Some(ManaSpendPermission::AnyColor)
        );

        let json = serde_json::to_string(&state).expect("serialize parked ordering choice");
        let mut restored: GameState = serde_json::from_str(&json).expect("restore parked choice");
        let chosen_index = restored
            .pending_replacement
            .as_ref()
            .expect("replacement choice remains parked")
            .search_found_candidates
            .iter()
            .position(|candidate| candidate.disposition.grant.is_some())
            .expect("serialized grant candidate exists");
        let mut reincarnated = crate::game::game_object::GameObject::new(
            grant_source,
            CardId(90_500),
            PlayerId(1),
            "Same id, different source".to_string(),
            Zone::Battlefield,
        );
        reincarnated.incarnation = bound.source.incarnation + 1;
        reincarnated
            .replacement_definitions
            .push(search_found_redirect(Zone::Hand));
        restored.objects.insert(grant_source, reincarnated);

        super::super::engine::apply_as_current(
            &mut restored,
            GameAction::ChooseReplacement {
                index: chosen_index,
            },
        )
        .expect("serialized candidate resumes through the public action boundary");

        assert_eq!(restored.objects[&found].zone, Zone::Exile);
        assert!(matches!(
            restored.objects[&found].casting_permissions.as_slice(),
            [CastingPermission::PlayFromExile {
                granted_to: PlayerId(0),
                source_id: Some(source_id),
                exiled_by_ability_controller: Some(PlayerId(0)),
                mana_spend_permission: Some(ManaSpendPermission::AnyColor),
                ..
            }] if *source_id == grant_source
        ));
        assert!(restored.pending_search_found_batch.is_none());
    }

    /// CR 401.5 + CR 611.3a production-path harness: a battlefield permanent whose
    /// continuous static grants itself Flying as long as the top card of player
    /// 0's library is black (Vampire Nocturnus). Library starts black-on-top over
    /// white. Returns (state, permanent, black, white).
    fn top_gated_flying_library_scenario() -> (GameState, ObjectId, ObjectId, ObjectId) {
        use crate::types::ability::{
            ContinuousModification, FilterProp, StaticCondition, StaticDefinition, TargetFilter,
            TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::keywords::Keyword;
        use crate::types::mana::ManaColor;
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);
        let vampire = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Vampire Nocturnus".to_string(),
            Zone::Battlefield,
        );
        {
            let def = StaticDefinition::new(StaticMode::Continuous)
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Flying,
                }])
                .condition(StaticCondition::TopOfLibraryMatches {
                    filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                        FilterProp::HasColor {
                            color: ManaColor::Black,
                        },
                    ])),
                });
            let obj = state.objects.get_mut(&vampire).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.static_definitions.push(def);
        }
        let black = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Black Card".to_string(),
            Zone::Library,
        );
        let white = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "White Card".to_string(),
            Zone::Library,
        );
        state.objects.get_mut(&black).unwrap().color = vec![ManaColor::Black];
        state.objects.get_mut(&white).unwrap().color = vec![ManaColor::White];
        {
            let p0 = state
                .players
                .iter_mut()
                .find(|p| p.id == PlayerId(0))
                .unwrap();
            p0.library.retain(|&id| id != black && id != white);
            p0.library.push_back(white);
            p0.library.push_front(black); // CR 401.1: front() == top.
        }
        (state, vampire, black, white)
    }

    fn has_flying(state: &GameState, id: ObjectId) -> bool {
        use crate::types::keywords::Keyword;
        state
            .objects
            .get(&id)
            .unwrap()
            .has_keyword(&Keyword::Flying)
    }

    // CR 401.5 + CR 611.3a: Scry reorders the library top by editing the library
    // directly (outside the zone-move seam); the top-gated static must recompute.
    #[test]
    fn scry_reorders_top_and_reevaluates_top_gated_static() {
        let (mut state, vampire, black, white) = top_gated_flying_library_scenario();
        crate::game::layers::flush_layers(&mut state);
        assert!(has_flying(&state, vampire), "black top → Flying granted");

        // Scry 2: keep white on top, black to the bottom.
        let mut events = vec![];
        handle_resolution_choice(
            &mut state,
            WaitingFor::ScryChoice {
                player: PlayerId(0),
                cards: vec![black, white],
            },
            GameAction::SelectCards { cards: vec![white] },
            &mut events,
        )
        .expect("scry resolves");
        crate::game::layers::flush_layers(&mut state);
        assert!(
            !has_flying(&state, vampire),
            "white scryed to top → Flying must be recomputed away"
        );
    }

    // CR 401.5 + CR 611.3a: the shared Surveil keep-on-top helper reorders the
    // library top directly; the top-gated static must recompute.
    #[test]
    fn surveil_keep_on_top_reevaluates_top_gated_static() {
        let (mut state, vampire, _black, white) = top_gated_flying_library_scenario();
        crate::game::layers::flush_layers(&mut state);
        assert!(has_flying(&state, vampire), "black top → Flying granted");

        surveil_keep_on_top(&mut state, PlayerId(0), &[white]);
        crate::game::layers::flush_layers(&mut state);
        assert!(
            !has_flying(&state, vampire),
            "surveil kept white on top → Flying must be recomputed away"
        );
    }

    // CR 401.5 + CR 611.3a: a Dig that keeps a card on top of the library edits the
    // library directly (kept_destination == Library); the static must recompute.
    #[test]
    fn dig_kept_to_library_top_reevaluates_top_gated_static() {
        let (mut state, vampire, black, white) = top_gated_flying_library_scenario();
        crate::game::layers::flush_layers(&mut state);
        assert!(has_flying(&state, vampire), "black top → Flying granted");

        // Dig looks at [black, white]; keep white on top, rest to bottom.
        let mut events = vec![];
        handle_resolution_choice(
            &mut state,
            WaitingFor::DigChoice {
                player: PlayerId(0),
                library_owner: PlayerId(0),
                cards: vec![black, white],
                keep_count: 1,
                up_to: false,
                selectable_cards: vec![black, white],
                kept_destination: Some(Zone::Library),
                rest_destination: Some(Zone::Library),
                rest_order: DigRestOrder::Preserve,
                source_id: None,
                enter_tapped: false,
                enters_attacking: false,
            },
            GameAction::SelectCards { cards: vec![white] },
            &mut events,
        )
        .expect("dig resolves");
        crate::game::layers::flush_layers(&mut state);
        assert!(
            !has_flying(&state, vampire),
            "dig kept white on top → Flying must be recomputed away"
        );
    }

    /// CR 400.5 + CR 608.2c: a Dig's explicit random-bottom instruction draws
    /// exactly once from the game's seeded RNG immediately before the unkept
    /// cards are appended. Its Preserve sibling must retain the look order and
    /// leave the RNG untouched.
    #[test]
    fn dig_choice_library_rest_honors_random_and_preserve_order() {
        fn state_with_looked_at_cards() -> (GameState, [ObjectId; 3], ObjectId) {
            let mut state = GameState::new_two_player(0x6367);
            let _keep = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Keep".to_string(),
                Zone::Library,
            );
            let rest = [
                create_object(
                    &mut state,
                    CardId(2),
                    PlayerId(0),
                    "Rest One".to_string(),
                    Zone::Library,
                ),
                create_object(
                    &mut state,
                    CardId(3),
                    PlayerId(0),
                    "Rest Two".to_string(),
                    Zone::Library,
                ),
                create_object(
                    &mut state,
                    CardId(4),
                    PlayerId(0),
                    "Rest Three".to_string(),
                    Zone::Library,
                ),
            ];
            let below = create_object(
                &mut state,
                CardId(5),
                PlayerId(0),
                "Below Look Window".to_string(),
                Zone::Library,
            );
            // `create_object` appends to the library, making this exact
            // top-to-bottom window `[keep, rest..., below]`.
            (state, rest, below)
        }

        let (mut random_state, rest, below) = state_with_looked_at_cards();
        let keep = random_state.players[0].library[0];
        let mut expected_rest = rest.to_vec();
        let mut expected_rng = random_state.rng.clone();
        expected_rest.shuffle(&mut expected_rng);
        let mut events = Vec::new();
        handle_resolution_choice(
            &mut random_state,
            WaitingFor::DigChoice {
                player: PlayerId(0),
                library_owner: PlayerId(0),
                cards: vec![keep, rest[0], rest[1], rest[2]],
                keep_count: 1,
                up_to: false,
                selectable_cards: vec![keep, rest[0], rest[1], rest[2]],
                kept_destination: Some(Zone::Library),
                rest_destination: Some(Zone::Library),
                rest_order: DigRestOrder::Random,
                source_id: None,
                enter_tapped: false,
                enters_attacking: false,
            },
            GameAction::SelectCards { cards: vec![keep] },
            &mut events,
        )
        .expect("random-bottom Dig choice resolves");

        assert_eq!(
            random_state.players[0]
                .library
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [vec![keep, below], expected_rest].concat(),
            "random rest must occupy the bottom in the seeded permutation"
        );
        assert_eq!(
            random_state.rng.get_word_pos(),
            expected_rng.get_word_pos(),
            "random rest must consume exactly the seeded shuffle stream"
        );

        let (mut preserve_state, rest, below) = state_with_looked_at_cards();
        let keep = preserve_state.players[0].library[0];
        let before_rng = preserve_state.rng.get_word_pos();
        let mut events = Vec::new();
        handle_resolution_choice(
            &mut preserve_state,
            WaitingFor::DigChoice {
                player: PlayerId(0),
                library_owner: PlayerId(0),
                cards: vec![keep, rest[0], rest[1], rest[2]],
                keep_count: 1,
                up_to: false,
                selectable_cards: vec![keep, rest[0], rest[1], rest[2]],
                kept_destination: Some(Zone::Library),
                rest_destination: Some(Zone::Library),
                rest_order: DigRestOrder::Preserve,
                source_id: None,
                enter_tapped: false,
                enters_attacking: false,
            },
            GameAction::SelectCards { cards: vec![keep] },
            &mut events,
        )
        .expect("preserve-bottom Dig choice resolves");

        assert_eq!(
            preserve_state.players[0]
                .library
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![keep, below, rest[0], rest[1], rest[2]],
            "legacy/non-random Dig rest must preserve the looked-at order"
        );
        assert_eq!(
            preserve_state.rng.get_word_pos(),
            before_rng,
            "preserved rest must not consume RNG"
        );
    }

    #[test]
    fn land_nonland_guess_logs_without_persisting_a_source_label() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gollum, Scheming Guide".to_string(),
            Zone::Battlefield,
        );
        let waiting_for = WaitingFor::NamedChoice {
            free_entry: None,
            player: PlayerId(1),
            choice_type: ChoiceType::CardPredicateGuess {
                options: ChoiceType::land_or_nonland_card_predicate_options(),
            },
            options: ChoiceType::card_predicate_labels(
                &ChoiceType::land_or_nonland_card_predicate_options(),
            ),
            source: Some(resolution_choice_source(&state, source_id)),
            persist_player: None,
        };
        let mut events = Vec::new();

        let outcome = handle_resolution_choice(
            &mut state,
            waiting_for,
            GameAction::ChooseOption {
                choice: "Nonland".to_string(),
            },
            &mut events,
        )
        .expect("choice resolves");

        assert!(matches!(outcome, ResolutionChoiceOutcome::WaitingFor(_)));
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::CardPredicateGuessMade {
                player_id,
                source_id: Some(event_source_id),
                choice,
            } if *player_id == PlayerId(1)
                && *event_source_id == source_id
                && choice == "Nonland"
        )));
        let source = state.objects.get(&source_id).expect("source exists");
        assert!(
            source.chosen_attributes.is_empty(),
            "opponent guess labels must not remain rendered on the source card"
        );
    }

    #[test]
    fn land_nonland_kind_choice_does_not_debug_log_or_persist_source_label() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Abundance".to_string(),
            Zone::Battlefield,
        );
        let waiting_for = WaitingFor::NamedChoice {
            free_entry: None,
            player: PlayerId(0),
            choice_type: ChoiceType::CardPredicate {
                options: ChoiceType::land_or_nonland_card_predicate_options(),
            },
            options: ChoiceType::card_predicate_labels(
                &ChoiceType::land_or_nonland_card_predicate_options(),
            ),
            source: Some(resolution_choice_source(&state, source_id)),
            persist_player: None,
        };
        let mut events = Vec::new();

        handle_resolution_choice(
            &mut state,
            waiting_for,
            GameAction::ChooseOption {
                choice: "Land".to_string(),
            },
            &mut events,
        )
        .expect("choice resolves");

        assert!(
            !events
                .iter()
                .any(|event| matches!(event, GameEvent::CardPredicateGuessMade { .. })),
            "ordinary land/nonland kind choices should not produce debug guess logs"
        );
        assert!(
            state
                .objects
                .get(&source_id)
                .expect("source exists")
                .chosen_attributes
                .is_empty(),
            "transient land/nonland kind choices should not render source labels"
        );
    }

    fn insert_planar_deck_plane(
        state: &mut GameState,
        card_id: u32,
        name: &str,
        controller: PlayerId,
    ) -> ObjectId {
        use crate::game::game_object::GameObject;
        use crate::types::card_type::{CardType, CoreType};

        let id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let mut obj = GameObject::new(
            id,
            CardId(u64::from(card_id)),
            controller,
            name.to_string(),
            Zone::Command,
        );
        let mut card_type = CardType::default();
        card_type.core_types.push(CoreType::Plane);
        obj.card_types = card_type;
        obj.face_down = true;
        state.objects.insert(id, obj);
        id
    }

    fn setup_planechase_two_deep(state: &mut GameState) -> (ObjectId, ObjectId, ObjectId) {
        use crate::types::format::FormatConfig;

        let controller = PlayerId(0);
        state.format_config = FormatConfig::planechase();
        let active = insert_planar_deck_plane(state, 1, "Active Plane", controller);
        if let Some(obj) = state.objects.get_mut(&active) {
            obj.face_down = false;
        }
        state.command_zone.push_back(active);
        let deck_top = insert_planar_deck_plane(state, 2, "Deck Top", controller);
        let deck_second = insert_planar_deck_plane(state, 3, "Deck Second", controller);
        state.planar_deck.push_back(deck_top);
        state.planar_deck.push_back(deck_second);
        state.planar_controller = Some(controller);
        (active, deck_top, deck_second)
    }

    #[test]
    fn arrange_planar_deck_top_choice_reorders_deck() {
        use crate::game::planechase::active_plane;

        let mut state = GameState::new_two_player(11);
        let (active, deck_top, deck_second) = setup_planechase_two_deep(&mut state);
        state.waiting_for = WaitingFor::ArrangePlanarDeckTopChoice {
            player: PlayerId(0),
            cards: vec![deck_top, deck_second],
            keep_on_top: 1,
        };

        let waiting_for = state.waiting_for.clone();
        let mut events = Vec::new();
        handle_resolution_choice(
            &mut state,
            waiting_for,
            GameAction::SelectCards {
                cards: vec![deck_second],
            },
            &mut events,
        )
        .expect("arrange choice resolves");

        assert_eq!(state.planar_deck.front(), Some(&deck_second));
        assert_eq!(active_plane(&state), Some(active));
    }

    #[test]
    fn arrange_planar_deck_top_choice_drains_stashed_planeswalk() {
        use crate::game::ability_utils::build_resolved_from_def;
        use crate::game::effects::resolve_ability_chain;
        use crate::game::planechase::active_plane;

        let mut state = GameState::new_two_player(13);
        let (_active, deck_top, deck_second) = setup_planechase_two_deep(&mut state);

        let execute = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ArrangePlanarDeckTop {
                count: QuantityExpr::Fixed { value: 2 },
                keep_on_top: QuantityExpr::Fixed { value: 1 },
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Planeswalk,
        ));
        let resolved = build_resolved_from_def(&execute, ObjectId(100), PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &resolved, &mut events, 0).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ArrangePlanarDeckTopChoice { .. }
        ));
        assert!(matches!(
            state.active_ability_continuation().unwrap().chain.effect,
            Effect::Planeswalk
        ));

        let waiting_for = state.waiting_for.clone();
        handle_resolution_choice(
            &mut state,
            waiting_for,
            GameAction::SelectCards {
                cards: vec![deck_second],
            },
            &mut events,
        )
        .expect("arrange + planeswalk resolves");

        assert_eq!(active_plane(&state), Some(deck_second));
        assert!(state.planar_deck.contains(&deck_top));
    }

    /// CR 603.7: Terminal `up_to` EffectZoneChoice with zero cards selected must
    /// rebind a fresh empty chain tracked set through the production
    /// `handle_resolution_choice` path so a following TrackedSet consumer cannot
    /// reuse a prior non-empty set. Mid-pause empty publishes stay skipped.
    #[test]
    fn terminal_empty_up_to_effect_zone_choice_rebinds_empty_tracked_set() {
        use crate::types::ability::{
            CastingPermission, Effect, PermissionGrantee, ResolvedAbility,
        };
        use crate::types::game_state::PendingContinuation;
        use crate::types::identifiers::TrackedSetId;
        use crate::types::zones::EtbTapState;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let eligible = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Eligible Aura".to_string(),
            Zone::Graveyard,
        );
        let stale = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Stale".to_string(),
            Zone::Exile,
        );
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![stale]);
        state.next_tracked_set_id = 2;
        state.chain_tracked_set_id = Some(TrackedSetId(1));

        // Mid-pause empty must not rebind (Storm Herald Aura-host pause).
        publish_effect_zone_choice_tracked_set(&mut state, EffectKind::ChangeZone, &[], None, true);
        assert_eq!(state.chain_tracked_set_id, Some(TrackedSetId(1)));
        assert_eq!(
            state.tracked_object_sets.get(&TrackedSetId(1)),
            Some(&vec![stale])
        );

        // Continuation consumes the chain tracked set — must observe the fresh
        // empty set from terminal zero-choice, not the stale prior members.
        state.park_ability_continuation(PendingContinuation::new(
            Box::new(ResolvedAbility::new(
                Effect::GrantCastingPermission {
                    permission: CastingPermission::Plotted { turn_plotted: 0 },
                    target: TargetFilter::TrackedSet {
                        id: TrackedSetId(0),
                    },
                    grantee: PermissionGrantee::ObjectOwner,
                },
                vec![],
                source,
                PlayerId(0),
            )),
            &state,
        ));

        let waiting = WaitingFor::EffectZoneChoice {
            player: PlayerId(0),
            cards: vec![eligible],
            count: 1,
            min_count: 0,
            up_to: true,
            source_id: source,
            effect_kind: EffectKind::ChangeZone,
            zone: Zone::Graveyard,
            destination: Some(Zone::Battlefield),
            enter_tapped: EtbTapState::Unspecified,
            enter_transformed: false,
            enters_under_player: None,
            enters_attacking: false,
            owner_library: false,
            track_exiled_by_source: false,
            face_down_profile: None,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            count_param: 0,
            library_position: None,
            is_cost_payment: false,
            enters_modified_if: None,
            duration: None,
        };
        state.waiting_for = waiting.clone();

        let mut events = Vec::new();
        handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards { cards: vec![] },
            &mut events,
        )
        .expect("terminal empty up_to EffectZoneChoice resolves");

        assert_eq!(
            state.chain_tracked_set_id,
            Some(TrackedSetId(2)),
            "production empty up_to path must rebind a fresh chain tracked set"
        );
        assert!(state
            .tracked_object_sets
            .get(&TrackedSetId(2))
            .is_some_and(|objects| objects.is_empty()));
        assert!(
            state
                .objects
                .get(&stale)
                .is_some_and(|obj| obj.casting_permissions.is_empty()),
            "TrackedSet continuation must not grant against the prior non-empty set"
        );
        assert_eq!(
            state.objects.get(&eligible).map(|obj| obj.zone),
            Some(Zone::Graveyard),
            "zero-choice must leave eligible cards unmoved"
        );
    }

    /// Minimal 1/1 `CopiableValues` for the `Tokens` stash kind — only the VARIANT is under
    /// test here, never the profile contents.
    fn boundary_census_token_profile() -> Box<crate::types::ability::CopiableValues> {
        Box::new(crate::types::ability::CopiableValues {
            name: "Saproling".to_string(),
            mana_cost: crate::types::mana::ManaCost::default(),
            color: vec![],
            card_types: crate::types::card_type::CardType::default(),
            power: Some(1),
            toughness: Some(1),
            loyalty: None,
            printed_loyalty: None,
            keywords: vec![],
            abilities: std::sync::Arc::default(),
            trigger_definitions: std::sync::Arc::default(),
            replacement_definitions: std::sync::Arc::default(),
            static_definitions: std::sync::Arc::default(),
            room_halves: None,
            name_origin: Default::default(),
        })
    }

    /// The boundary apply loop's own source region, sliced out of this file at compile time.
    ///
    /// WHY SOURCE TEXT. `possible_hold`'s wildcard-free `match` is compiler-enforced on the
    /// `PersistentAxisMaterialization` VARIANT axis, but nothing in the type system binds it to the
    /// loop's EXIT axis. Without this slice the census below compares `possible_hold` against a
    /// hand-transcribed `vec![..]` — i.e. against itself — so adding an item-level non-push exit
    /// reachable by `DriveSequence` would leave `possible_hold` still reporting
    /// `DriveSequence => None ⇒ Committed` and the badge would silently resume promising `∞→N` for
    /// a collapse that never lands. That is MED-2 recurring, invisibly. Reading engine source with
    /// `include_str!` is the in-house technique for exactly this
    /// (`tests/integration/cr_annotations.rs`); it does not recurse, so a file may read itself.
    ///
    /// THE REGION STARTS AT THE SORT, NOT AT THE `for`. The Tokens-last ordering is the reason the
    /// `CopyTokenPause` `return` cannot strand a still-unapplied non-`Tokens` item — which is the
    /// other way `DriveSequence => Committed` becomes a lie — so dropping it must red this test too.
    ///
    /// Panics rather than degrading into a file-wide scan if any anchor moves.
    fn boundary_apply_loop_region() -> &'static str {
        const SRC: &str = include_str!("engine_resolution_choices.rs");
        const TEST_MOD: &str = "#[cfg(test)]\nmod tests {";
        const SORT: &str =
            "items.sort_by_key(|i| matches!(i, PersistentAxisMaterialization::Tokens(_)))";
        const OPEN: &str = "for item in &items {";
        const CLOSE: &str = "collapsed.push(item.clone());";

        // PRODUCTION ONLY. The three anchors below are also `const` string literals in THIS module,
        // so an un-truncated search silently falls through to the test's own source when an anchor
        // is deleted from the loop: the `Tokens`-last drop probe reported `(0, 0)` — red, but for
        // the wrong reason, with a `possible_hold` message pointing at a mutation that was really a
        // missing sort. Truncating first makes a deleted anchor a named panic instead.
        let production = SRC
            .find(TEST_MOD)
            .map(|at| &SRC[..at])
            .expect("this file's inline test module header");

        let sort = production
            .find(SORT)
            .expect("the Tokens-last stash ordering that keeps the pause from stranding items");
        let open = production[sort..]
            .find(OPEN)
            .map(|at| at + sort)
            .expect("the boundary apply loop opener");
        // SINGLE-PUSH INVARIANT, IN CODE. `close` takes the FIRST push after the opener, so a push
        // inserted higher in the body silently truncates the region and drops every exit below it
        // from the census below — defeating it without failing it. The invariant used to be stated
        // only in prose on the push itself.
        assert_eq!(
            production.matches(CLOSE).count(),
            1,
            "the boundary apply loop must contain exactly one `collapsed.push(item.clone());`; a \
             second one silently narrows the census region instead of failing it"
        );
        let close = production[open..]
            .find(CLOSE)
            .map(|at| at + open)
            .expect("the single collapsed.push");
        &production[sort..close]
    }

    /// The citation rule on [`BoundaryHold`] shipped false twice as prose, each time through a
    /// carve-out only a reviewer could apply. This is the executable form.
    ///
    /// MEASURED, not feared: at `BASE_SHA` this file already carried five line anchors, and four of
    /// them pointed at unrelated code — `derived_fodder_class` was cited ~2500 lines from where it
    /// lives, `try_offer_object_growth_shortcut` ~4200. Each was accurate when it was written.
    /// That is the whole argument for symbol anchors, and it is why the rule needed a gate rather
    /// than a stricter sentence.
    ///
    /// POPULATION IS DISCOVERED, NOT LISTED. The guard walks the crate and enforces on every file
    /// carrying the opt-in marker comment, so a newly enrolling file is covered the moment it opts
    /// in, and a hardcoded list cannot drift out of sync with the class it names.
    /// A whole-crate sweep was measured and rejected as out of scope, not as unnecessary:
    /// `crates/engine/src` carries 216 such anchors across 61 files (`game/engine.rs` alone 29),
    /// ~20x this change. Regenerate that census with:
    ///
    /// ```text
    /// grep -rnoP '[A-Za-z0-9_./-]*[A-Za-z0-9_-]\.rs:[0-9]' crates/engine/src | wc -l
    /// ```
    ///
    /// THE RESIDUAL HOLE, NAMED: a file that adds line-anchored citations WITHOUT the marker is not
    /// covered — opting in is voluntary. This gate keeps the enrolled class honest; it does not
    /// claim the 61 files that never enrolled. Closing that hole means enrolling them, not widening
    /// this assertion.
    ///
    /// Two anchor forms are caught, because both shipped here: the named `<file>.rs:<line>` and the
    /// bare `:<line>` back-reference used once the file has been named earlier in the same block.
    /// A symbol-keyed sweep cannot see the bare form, which is exactly how the first pass
    /// undercounted; sweep by pattern, never from a previous census.
    ///
    /// Production text only, where a file has an inline test module: a test module necessarily
    /// writes such anchors into its own prose and messages.
    #[test]
    fn subsystem_citations_are_symbol_anchored() {
        const MARKER: &str = "engine-citation-gate: symbol anchors only";
        const TEST_MOD: &str = "#[cfg(test)]\nmod tests {";
        // Enrolment floor. Not a list — a non-vacuity guard, so a broken walk or a renamed marker
        // reds instead of passing on an empty population. Raise it when a file joins.
        const ENROLLED_FLOOR: usize = 8;

        // `CR 732.2a` has no colon; `std::vec` no digit; `field:1` and `{"Life":1}` have a name or
        // a quote before the colon. A bare back-reference is recognized only after whitespace, a
        // backtick or `(`, which is how every one of them was actually written.
        fn line_anchored(line: &str) -> bool {
            line.match_indices(':')
                .filter(|(at, _)| line[at + 1..].starts_with(|c: char| c.is_ascii_digit()))
                .any(|(at, _)| match line[..at].chars().next_back() {
                    Some(c) if c.is_alphanumeric() || c == '_' => line[..at]
                        .rsplit(|c: char| !(c.is_alphanumeric() || "._-/".contains(c)))
                        .next()
                        .is_some_and(|word| word.contains('.')),
                    Some(' ' | '\t' | '`' | '(') => true,
                    _ => false,
                })
        }

        fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    rs_files(&path, out);
                } else if path.extension().is_some_and(|x| x == "rs") {
                    out.push(path);
                }
            }
        }

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        rs_files(&root.join("src"), &mut files);
        rs_files(&root.join("tests"), &mut files);
        files.sort();

        let mut enrolled = 0usize;
        let mut offenders: Vec<String> = Vec::new();
        for path in &files {
            let Ok(src) = std::fs::read_to_string(path) else {
                continue;
            };
            if !src.contains(MARKER) {
                continue;
            }
            enrolled += 1;
            let production = src.find(TEST_MOD).map_or(src.as_str(), |at| &src[..at]);
            for (n, line) in production.lines().enumerate() {
                if line_anchored(line) {
                    offenders.push(format!(
                        "{} line {} — {}",
                        path.display(),
                        n + 1,
                        line.trim()
                    ));
                }
            }
        }

        assert!(
            enrolled >= ENROLLED_FLOOR,
            "the citation gate found only {enrolled} enrolled files; it must find at least \
             {ENROLLED_FLOOR}. A gate that discovers nothing passes vacuously — check the marker \
             comment and the crate walk before lowering this floor"
        );
        assert!(
            offenders.is_empty(),
            "an enrolled file cites by line, which the rule on `BoundaryHold` forbids: an unrelated \
             edit above the target silently repoints the citation, and four such citations were \
             already stale when this gate was written. Name the symbol or a greppable heading \
             instead.\n{}",
            offenders.join("\n")
        );
    }

    /// Whole-word occurrences of `keyword` in already-comment-stripped code. Bare `str::matches`
    /// counts `should_continue;` and `breakfast`, so a census built on it moves under a rename —
    /// and a census a rename can move is one people learn to silence.
    fn count_exit_keyword(code: &str, keyword: &str) -> usize {
        let is_ident = |c: char| c.is_alphanumeric() || c == '_';
        code.match_indices(keyword)
            .filter(|(at, _)| {
                code[..*at].chars().next_back().is_none_or(|c| !is_ident(c))
                    && code[at + keyword.len()..]
                        .chars()
                        .next()
                        .is_none_or(|c| !is_ident(c))
            })
            .count()
    }

    /// B-1: `possible_hold` is the boundary apply loop's own non-push-exit census, so it must
    /// agree with that loop kind-for-kind, and every `BoundaryHold` variant must be claimed by
    /// at least one kind.
    ///
    /// REVERT-PROBE (matched positive AND negative in this one test):
    ///   (a) `Tokens => None` ⇒ the `Tokens` hold/certainty assertions flip ⇒ RED.
    ///   (b) `DriveSequence => Some(BoundaryHold::ObservedGrowth)` ⇒ the only `Committed` kind
    ///       flips ⇒ RED.
    ///   (c) a third `BoundaryHold` variant claimed by no kind ⇒ the completeness assertion reds.
    ///   (d) ADD any item-level non-push exit to the loop ⇒ the exit-axis assertion reds. All four
    ///       exit forms are counted — `continue`, `return`, `break`, `?`. The first two alone were
    ///       not enough: a `break` skips the push for its item AND every later one, which is the
    ///       MED-2 shape this census exists to catch, and it went uncounted.
    ///   (e) REMOVE one (e.g. delete the `boundary_declines` guard) ⇒ it reds the other way.
    ///   (f) drop the `items.sort_by_key(..)` ⇒ `boundary_apply_loop_region` panics ⇒ RED.
    ///   (g) ADD a second `collapsed.push(item.clone());` ⇒ the single-push assertion in
    ///       `boundary_apply_loop_region` panics ⇒ RED. Without it a push inserted higher in the
    ///       body truncates the census region and drops the exits below it, silently.
    #[test]
    fn boundary_hold_census_matches_the_apply_loop() {
        use crate::game::derived_views::CollapseCertainty;

        let kinds = [
            PersistentAxisMaterialization::Tokens(boundary_census_token_profile()),
            PersistentAxisMaterialization::Counters(vec![]),
            PersistentAxisMaterialization::Life {
                player: PlayerId(0),
                per_cycle_delta: 2,
            },
            PersistentAxisMaterialization::DriveSequence {
                sequence: vec![],
                collapsed_axes: vec![],
            },
        ];

        let holds: Vec<Option<BoundaryHold>> = kinds.iter().map(possible_hold).collect();
        assert_eq!(
            holds,
            vec![
                Some(BoundaryHold::CopyTokenPause),
                Some(BoundaryHold::ObservedGrowth),
                Some(BoundaryHold::ObservedGrowth),
                None,
            ],
            "possible_hold must mirror the boundary loop's three non-push exits: the Tokens \
             pause, and the Counters/Life observer declines. DriveSequence has none."
        );

        let certainties: Vec<CollapseCertainty> =
            kinds.iter().map(materialization_certainty).collect();
        assert_eq!(
            certainties,
            vec![
                CollapseCertainty::Conditional,
                CollapseCertainty::Conditional,
                CollapseCertainty::Conditional,
                CollapseCertainty::Committed,
            ],
            "certainty is Conditional iff the kind has a hold — DriveSequence is the only \
             Committed kind, which is why ∞→N is reserved for it"
        );

        // COMPLETENESS: no BoundaryHold variant may exist that no kind can reach.
        let mut claimed: Vec<BoundaryHold> = holds.into_iter().flatten().collect();
        claimed.sort();
        claimed.dedup();
        assert_eq!(
            claimed,
            BoundaryHold::ALL.to_vec(),
            "every BoundaryHold variant must be claimed by at least one materialization kind"
        );

        // EXIT-AXIS BINDING — the half the two assertions above cannot supply, because they compare
        // `possible_hold` against a transcription of itself. Counting unit and decomposition are the
        // ones stated on `BoundaryHold`: 4 control-flow statements = 1 push + 2 item-level non-push
        // + 1 inner per-growth skip. `crate::source_census::code_lines` is the shared rule:
        // whole-line AND trailing comment text removed, so prose ABOUT `continue`/`return`
        // cannot inflate the count from either position.
        let code: String = crate::source_census::code_lines(boundary_apply_loop_region());
        // The counters read raw text, and a string literal is not a comment, so one carrying the
        // word `break` (or a `?`) would be counted as control flow — a red no reader could act on.
        // There are none in the loop today; keep it that way, or teach the counters to skip them.
        assert!(
            !code.contains('"'),
            "the boundary apply loop must carry no string literal — the exit census counts raw text"
        );
        // Whole-word so an identifier ending in a keyword cannot inflate the count.
        let continues = count_exit_keyword(&code, "continue");
        let returns = count_exit_keyword(&code, "return");
        // `break` and `?` are exits the earlier `matches("continue;")` / `matches("return ")` pair
        // could not see, which made claim (d) above false: a `break` skips the push for THIS item
        // and every later one — the exact MED-2 shape — and `foo()?` leaves the function outright.
        // Not hypothetical vocabulary: `possible_hold`'s own doc describes
        // `drive_persistent_axis_collapse` as one that `break`s to commit a successful prefix.
        let breaks = count_exit_keyword(&code, "break");
        // Deliberately crude: `?Sized` or `'?'` would OVER-count, and over-counting reds (a human
        // re-derives the census) while under-counting ships MED-2. The two directions are not
        // symmetric, so the cheap matcher is the safe one.
        let tries = code.matches('?').count();
        assert_eq!(
            (continues, returns, breaks, tries),
            (2, 1, 0, 0),
            "the boundary apply loop's control-flow census moved. Re-derive it, then update \
             `possible_hold`, `BoundaryHold`, and this test together — an item-level exit that \
             `possible_hold` does not know about makes the badge promise a collapse that never \
             lands (MED-2)"
        );

        // The inner `for g in growths` stale-id skip (CR 400.7) is the ONE non-push statement whose
        // ITEM still reaches the push, so it is subtracted rather than mapped to a variant. What is
        // left must be exactly the hold set. Adding an item-level exit raises the left side without
        // raising the right; removing one lowers it. Deliberately blind to WHICH kind of statement
        // was added — a new inner skip reds this too, which forces a human to re-derive the census
        // rather than letting the safe case train anyone to ignore it.
        const INNER_PER_GROWTH_SKIPS: usize = 1;
        assert_eq!(
            continues + returns + breaks + tries - INNER_PER_GROWTH_SKIPS,
            BoundaryHold::ALL.len(),
            "every item-level non-push exit in the loop must be a labelled BoundaryHold, and every \
             BoundaryHold must be one of those exits"
        );
    }
}
