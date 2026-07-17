use std::collections::HashMap;

use crate::types::ability::{
    AbilityCost, ChoiceType, ChosenAttribute, Effect, EffectKind, GuessOutcome, LibraryPosition,
    QuantityExpr, QuantityRef, ResolvedAbility, TargetRef, ThisWayCause,
};
use crate::types::actions::{GameAction, LearnOption, OutsideGameSelection};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    ActionResult, CastOfferKind, ChosenDamageSource, CopyChosenSelection, GameState,
    OutsideGameChoiceSource, PayableResource, PendingContinuation, WaitingFor,
};
use crate::types::identifiers::{ObjectId, TrackedSetId};
use crate::types::mana::ManaCost;
use crate::types::zones::Zone;

use super::effects;
use super::engine::EngineError;
use super::turns;
use super::zones;
use super::{casting, casting_costs, mana_abilities};

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
        remaining: chosen.to_vec(),
        survivors: Vec::with_capacity(chosen.len()),
        continuation,
        visibility: reveal.into(),
    };
    let batch = process_search_found_batch(state, batch, events)?;
    reveal_search_found_survivors(state, &batch, events);
    Ok(batch.survivors)
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

    state.last_revealed_ids = batch.survivors.clone();
    for &card_id in &batch.survivors {
        state.revealed_cards.insert(card_id);
    }
    if !batch.survivors.is_empty() {
        let card_names = batch
            .survivors
            .iter()
            .filter_map(|id| state.objects.get(id).map(|object| object.name.clone()))
            .collect();
        events.push(GameEvent::CardsRevealed {
            player: batch.searcher,
            card_ids: batch.survivors.clone(),
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
    for (index, object_id) in remaining.iter().copied().enumerate() {
        let proposed = crate::types::proposed_event::ProposedEvent::SearchFound {
            searcher: batch.searcher,
            library_owner: batch.library_owner,
            object_id,
            disposition: crate::types::proposed_event::SearchFoundDisposition::Original,
            applied: Default::default(),
        };
        match super::replacement::replace_event(state, proposed, events) {
            super::replacement::ReplacementResult::Execute(event) => {
                if deliver_search_found_event(state, event, &mut batch.survivors, events) {
                    batch.remaining = remaining[index + 1..].to_vec();
                    state.pending_search_found_batch = Some(batch);
                    return Err(Box::new(state.waiting_for.clone()));
                }
            }
            super::replacement::ReplacementResult::NeedsChoice(player) => {
                batch.remaining = remaining[index + 1..].to_vec();
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

/// Deliver one terminal SearchFound disposition. Returns `true` when the
/// resulting zone move or snapshotted suffix parked an inner choice.
fn deliver_search_found_event(
    state: &mut GameState,
    event: crate::types::proposed_event::ProposedEvent,
    survivors: &mut Vec<ObjectId>,
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
        survivors.push(object_id);
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
fn grant_search_found_permission_after_delivery(
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
    ability.source_incarnation = Some(grant.source.incarnation);
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
    if deliver_search_found_event(state, event, &mut batch.survivors, events) {
        state.pending_search_found_batch = Some(batch);
        return Ok(state.waiting_for.clone());
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
    reveal_search_found_survivors(state, &batch, events);
    let player = batch.searcher;
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
        crate::types::game_state::PendingSearchFoundContinuation::Standard { split: None } => {}
        crate::types::game_state::PendingSearchFoundContinuation::Standard {
            split: Some(split),
        } => {
            let split = split.clone();
            let source_id = state
                .pending_continuation
                .as_ref()
                .map(|continuation| continuation.chain.source_id)
                .or_else(|| batch.survivors.first().copied())
                .unwrap_or(ObjectId(0));
            if batch.survivors.len() > split.primary_count as usize {
                set_priority(state, player);
                state.waiting_for = WaitingFor::SearchPartitionChoice {
                    player,
                    cards: batch.survivors,
                    primary_destination: split.primary_destination,
                    primary_count: split.primary_count,
                    primary_enter_tapped: split.primary_enter_tapped,
                    rest_destination: split.rest_destination,
                    source_id,
                };
                return Ok(state.waiting_for.clone());
            }
            match apply_search_partition(
                state,
                &batch.survivors,
                &[],
                &split,
                source_id,
                player,
                events,
            )? {
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
        match finalize_standard_search_selection(state, player, &batch.survivors, events) {
            ResolutionChoiceOutcome::WaitingFor(waiting)
            | ResolutionChoiceOutcome::WaitingForWithInlineTriggers(waiting)
            | ResolutionChoiceOutcome::WaitingForWithParkedObservers(waiting) => waiting,
            ResolutionChoiceOutcome::ActionResult(result) => result.waiting_for,
        },
    )
}

pub(super) enum ResolutionChoiceOutcome {
    WaitingFor(WaitingFor),
    WaitingForWithInlineTriggers(WaitingFor),
    /// CR 603.3b: observer triggers from a completed search put/shuffle were
    /// collected into `deferred_triggers` but must not drain until the caller
    /// receives priority again (issue #5336: Kodama + Nature's Lore).
    WaitingForWithParkedObservers(WaitingFor),
    ActionResult(ActionResult),
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
) -> Option<ResolutionChoiceOutcome> {
    if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        // B1: this action settled. Merge this slice's observer triggers into
        // the parked queue before draining — otherwise the last segment's
        // triggers (e.g. the final Syphon Mind opponent discard) never enter
        // `deferred_triggers` and are lost when ordering runs (issue #1793).
        let trigger_events: Vec<GameEvent> = events[event_slice_start..event_slice_end]
            .iter()
            .filter(|ev| !matches!(ev, GameEvent::PhaseChanged { .. }))
            .cloned()
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
            .filter(|ev| !matches!(ev, GameEvent::PhaseChanged { .. }))
            .cloned()
            .collect();
        super::triggers::collect_triggers_into_deferred(state, &trigger_events);
        None
    }
}

/// CR 603.2 + CR 603.3b + CR 701.23: after a search tutor's put/shuffle
/// continuation drains, park ETB/dies/discards observers for the next priority
/// checkpoint instead of dispatching them while the test harness (or UI) may
/// still be inside the same `SelectCards` action (issue #5336).
fn park_search_observer_triggers(
    state: &mut GameState,
    events: &[GameEvent],
    events_before_drain: usize,
) -> ResolutionChoiceOutcome {
    let trigger_events: Vec<GameEvent> = events[events_before_drain..]
        .iter()
        .filter(|ev| !matches!(ev, GameEvent::PhaseChanged { .. }))
        .cloned()
        .collect();
    if !trigger_events.is_empty() {
        super::triggers::collect_triggers_into_deferred(state, &trigger_events);
    }
    // The parent spell already left the stack; clear the stashed resolving entry
    // so the next priority pass can drain `deferred_triggers`.
    if state.pending_continuation.is_none()
        && matches!(state.waiting_for, WaitingFor::Priority { .. })
    {
        state.resolving_stack_entry = None;
        // CR 400.7j: clear the resolution-scoped self-move re-latch with the entry.
        state.resolution_source_relatch = None;
    }
    ResolutionChoiceOutcome::WaitingForWithParkedObservers(state.waiting_for.clone())
}

pub(super) fn handles(waiting_for: &WaitingFor) -> bool {
    matches!(
        waiting_for,
        WaitingFor::MeldPairChoice { .. }
            | WaitingFor::MeldAttackTargetChoice { .. }
            | WaitingFor::ScryChoice { .. }
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
    source_id: Option<ObjectId>,
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
    crate::game::zone_pipeline::move_objects_simultaneously(state, requests, events)
}

/// CR 701.22a / CR 701.25a: Scry and surveil put the kept cards on top of the
/// library "in any order", so a legal keep-on-top selection is any duplicate-free
/// subset of the looked-at cards (order is the player's free choice). Because the
/// multiplayer server bypasses its candidate-enumeration legality gate for these
/// freeform states (see `WaitingFor::accepts_freeform_card_selection`), `apply()`
/// is the real validation boundary: a foreign id or a duplicate would corrupt the
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
    set_priority(state, player);
    let events_before_drain = events.len();
    // CR 608.2c: Count cards still in hand immediately before a
    // found-set exile continuation. SearchFound replacements removed from the
    // survivor set are intentionally excluded.
    let continuation_exiles_set = state
        .pending_continuation
        .as_ref()
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
    if let Some(mut continuation) = state.pending_continuation.take() {
        continuation.search_attach_host =
            effects::change_zone::resolve_search_continuation_attach_host(
                state,
                &continuation.chain,
            );
        state.search_continuation_attach_host = continuation.search_attach_host;
        let mut targets: Vec<_> = chosen.iter().copied().map(TargetRef::Object).collect();
        // CR 701.23a + CR 701.24a: propagate the semantic searcher for
        // library-owner-sensitive shuffle and tail instructions.
        if player != continuation.chain.controller {
            targets.push(TargetRef::Player(player));
        }
        continuation.chain.targets = targets.clone();
        propagate_targets_through_search_shuffle(&mut continuation.chain, &targets);
        state.pending_continuation = Some(continuation);
    }
    // CR 605.3b + CR 616.1: resume-aware — a parked mana-cost cursor settles
    // before (and instead of stranding) the ordinary rider.
    super::engine::resume_pending_continuation_if_priority(state, events)
        .expect("a settled search choice must resume its continuation");
    park_search_observer_triggers(state, events, events_before_drain)
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
    if !primary_ids.is_empty() {
        // CR 614.1 / CR 110.5b: Synthesize a ChangeZone over the explicit primary
        // targets and route through the ETB pipeline so the permanent enters
        // tapped (and ETB-tapped replacements apply), unlike a bare move_to_zone.
        let primary_targets: Vec<TargetRef> = primary_ids
            .iter()
            .map(|&id| TargetRef::Object(id))
            .collect();
        let change_zone = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: split.primary_destination,
                target: crate::types::ability::TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: split.primary_enter_tapped,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: Vec::new(),
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            primary_targets,
            source_id,
            controller,
        );
        crate::game::effects::resolve_ability_chain(state, &change_zone, events, 0)
            .map_err(|e| EngineError::InvalidAction(format!("search-split primary move: {e:?}")))?;
    }
    // CR 701.23a + CR 401.4 + CR 616.1: Rest is never Battlefield across the
    // A/B/C cluster. Surface a replacement-ordering pause so callers do not
    // drain their shuffle continuation past the still-parked rest-pile batch.
    Ok(route_rest_partition(
        state,
        rest_ids,
        split.rest_destination,
        Some(source_id),
        events,
    ))
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
            player_state.library.retain(|id| !all_cards.contains(id));
            for (index, &card_id) in top_cards.iter().enumerate() {
                player_state.library.insert(index, card_id);
            }
            for &card_id in &bottom_cards {
                player_state.library.push_back(card_id);
            }
            // CR 401.5 + CR 611.3a: Scry reorders the library top directly (not
            // through the zone-move seam), so a continuous `TopOfLibraryMatches`
            // static must be re-evaluated — self-gated so it's a no-op otherwise.
            crate::game::layers::mark_layers_full_if_top_of_library_static_live(state);
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
            let pending = state.pending_coin_flip.take().ok_or_else(|| {
                EngineError::InvalidAction("No pending coin flip to resume".to_string())
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
                .face_down(face_down),
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
                            clear_markers: cards.clone(),
                            publish_tracked_set: None,
                            emit_reveal_until_resolved: None,
                        },
                    );
                    return Ok(ResolutionChoiceOutcome::WaitingFor(
                        state.waiting_for.clone(),
                    ));
                }
            }

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
                ResolutionChoiceOutcome::WaitingFor(result)
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
                                    clear_markers,
                                    publish_tracked_set: None,
                                    emit_reveal_until_resolved: None,
                                },
                            );
                            return Ok(ResolutionChoiceOutcome::WaitingFor(
                                state.waiting_for.clone(),
                            ));
                        }
                    }
                    // CR 508.4: "...tapped and attacking" — place the accepted card
                    // in combat. `source_id` (the ability source / trigger attacker)
                    // supplies the defending player, matching the synchronous path.
                    if enters_attacking {
                        let controller = state
                            .objects
                            .get(&hit_card)
                            .map(|obj| obj.controller)
                            .unwrap_or(player);
                        crate::game::combat::enter_attacking(
                            state, hit_card, source_id, controller,
                        );
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
                    clear_markers,
                    publish_tracked_set: None,
                    emit_reveal_until_resolved: None,
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
                // pauses and stashes `pending_repeat_until`). depth = 1: each
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
                        exile_instead_of_graveyard,
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
                        exile_instead_of_graveyard,
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
                    graveyard_replacement: None,
                    cost: crate::types::ability::ResolutionCastCost::Free,
                },
                events,
            )?;
            ResolutionChoiceOutcome::WaitingFor(result)
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
                            state.pending_continuation.is_none(),
                            "Learn rummage overwriting pending_continuation"
                        );
                        state.pending_continuation =
                            Some(PendingContinuation::new(Box::new(draw), state));
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
            zones::move_to_library_position(state, object_id, top, events);
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
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
                PayableResource::Energy => {
                    // CR 107.14: Remove N energy counters from the player.
                    if let Some(p) = state.players.iter_mut().find(|p| p.id == player) {
                        if p.energy < amount {
                            return Err(EngineError::InvalidAction(format!(
                                "Player {:?} has {} energy, cannot pay {}",
                                player, p.energy, amount
                            )));
                        }
                        p.energy -= amount;
                        events.push(GameEvent::EnergyChanged {
                            player,
                            delta: -(amount as i32),
                        });
                    }
                }
                PayableResource::ManaGeneric { per_x } => {
                    let cost = ManaCost::Cost {
                        shards: vec![],
                        generic: amount.saturating_mul(per_x),
                    };
                    if !casting::can_pay_effect_mana_cost_after_auto_tap(
                        state, player, source_id, &cost,
                    ) {
                        return Err(EngineError::InvalidAction(format!(
                            "Player {:?} cannot pay {} generic mana",
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
                    match crate::game::life_costs::pay_life_as_cost(state, player, amount, events) {
                        crate::game::life_costs::PayLifeCostResult::Paid { .. } => {}
                        _ => {
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
            state.last_effect_count = Some(total as i32);
            let pending_starts_with_pay_amount = state
                .pending_continuation
                .as_ref()
                .is_some_and(|cont| starts_with_pay_amount_prompt(&cont.chain));
            if !pending_starts_with_pay_amount {
                if let Some(cont) = state.pending_continuation.as_mut() {
                    cont.chain.set_chosen_x_recursive(total);
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
            if let Some(cont) = state.pending_continuation.as_mut() {
                cont.chain.set_optional_effect_performed_recursive(true);
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
            WaitingFor::ClashCardPlacement {
                player,
                card,
                remaining,
            },
            GameAction::ChooseTopOrBottom { top },
        ) => {
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
                enter_tapped,
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
            } else if kept.len() != keep_count {
                return Err(EngineError::InvalidAction(format!(
                    "Must select exactly {} cards, got {}",
                    keep_count,
                    kept.len()
                )));
            }

            // CR 401.2 + CR 608.2c: the keep-selection must be unique, drawn from
            // the cards actually looked at, and (when the dig has a filter) from
            // the filter-matching subset. The previous check skipped filter/look-
            // at validation entirely whenever `selectable_cards` was empty, so a
            // filtered dig that matched nothing accepted arbitrary object ids.
            validate_dig_selection(&kept, &cards, &selectable_cards)?;

            let unkept: Vec<_> = cards
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
                    player_state.library.retain(|id| !cards.contains(id));
                    for (index, &card_id) in kept.iter().enumerate() {
                        player_state.library.insert(index, card_id);
                    }
                    match rest_destination {
                        Some(Zone::Library) => {
                            for &obj_id in &unkept {
                                player_state.library.push_back(obj_id);
                            }
                            None
                        }
                        Some(zone) => Some(zone),
                        None => Some(Zone::Graveyard),
                    }
                };
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
                                    clear_markers: Vec::new(),
                                    publish_tracked_set: None,
                                    emit_reveal_until_resolved: None,
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
                for &obj_id in &kept {
                    if kept_zone == Zone::Battlefield {
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
                                        rest_destination: rest_destination
                                            .unwrap_or(Zone::Graveyard),
                                        clear_markers: Vec::new(),
                                        publish_tracked_set: Some(kept.clone()),
                                        emit_reveal_until_resolved: None,
                                    },
                                );
                                return Ok(ResolutionChoiceOutcome::WaitingFor(
                                    state.waiting_for.clone(),
                                ));
                            }
                        }
                    } else {
                        zones::move_to_zone(state, obj_id, kept_zone, events);
                    }
                }
            }
            // CR 701.20b + CR 608.2c: Publish a tracked set for downstream
            // sub_abilities. Reveal/keep continuations (Zimone land split) bind
            // the kept subset; Expressive Iteration's bottom/exile tail binds the
            // unkept looked-at pile when its continuation chains
            // `PutAtLibraryPosition { TrackedSet }`.
            let publish_set =
                if kept.is_empty() {
                    Vec::new()
                } else if state.pending_continuation.as_ref().is_some_and(|cont| {
                    dig_continuation_needs_full_looked_at_tracked_set(&cont.chain)
                }) {
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
                .pending_continuation
                .as_ref()
                .is_some_and(|cont| dig_continuation_needs_full_looked_at_tracked_set(&cont.chain));
            if !defer_rest_routing {
                match route_rest_partition(
                    state,
                    &unkept,
                    rest_destination.unwrap_or(Zone::Graveyard),
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
                                clear_markers: Vec::new(),
                                publish_tracked_set: Some(publish_set),
                                emit_reveal_until_resolved: None,
                            },
                        );
                        return Ok(ResolutionChoiceOutcome::WaitingFor(
                            state.waiting_for.clone(),
                        ));
                    }
                }
            }
            effects::publish_fresh_tracked_set(state, publish_set);
            if let Some(cont) = state.pending_continuation.as_mut() {
                // CR 608.2c: ParentTarget continuations (Hideaway conceal, dig
                // conditionals on the kept card) bind to the kept selection.
                // Hand/bottom/exile tails route via TrackedSetFiltered instead.
                cont.chain.targets = kept.iter().map(|&id| TargetRef::Object(id)).collect();
                cont.chain.context.optional_effect_performed = !kept.is_empty();
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
                for &card_id in &cards {
                    state.revealed_cards.remove(&card_id);
                }
                state.private_look_ids.clear();
                state.private_look_player = None;
                set_priority(state, player);
                if decline_runs_continuation {
                    super::engine::resume_pending_continuation_if_priority(state, events)
                        .expect("a settled reveal choice must resume its continuation");
                } else {
                    state.pending_continuation = None;
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

            for &card_id in &cards {
                state.revealed_cards.remove(&card_id);
            }
            state.private_look_ids.clear();
            state.private_look_player = None;

            set_priority(state, player);
            // CR 701.20a: For an optional reveal, the stashed continuation is the
            // decline branch (e.g., Tap SelfRef for reveal-lands). The player picked,
            // so decline must NOT run — drop the continuation. Non-optional reveals
            // chain targets into the continuation so the follow-up effect operates
            // on the revealed card (e.g., Thoughtseize's exile).
            if optional && decline_runs_continuation {
                state.pending_continuation = None;
            } else if let Some(cont) = state.pending_continuation.as_mut() {
                cont.chain.targets = vec![TargetRef::Object(chosen_id)];
                if optional {
                    cont.chain.context.optional_effect_performed = true;
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
                // The Shuffle continuation always exists for cultivate-class
                // splits; its `source_id` is the search card. Falls back to the
                // first chosen card's id only in the degenerate no-continuation
                // case (used solely as an event source label).
                let source_id = state
                    .pending_continuation
                    .as_ref()
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
                return Ok(park_search_observer_triggers(
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
            park_search_observer_triggers(state, events, events_before_partition)
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

            if let Some(cont) = state.pending_continuation.as_mut() {
                cont.chain.targets = chosen_ids.iter().map(|&id| TargetRef::Object(id)).collect();
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
                .pending_continuation
                .as_ref()
                .map(|cont| cont.chain.controller)
                .unwrap_or(player);
            set_priority(state, priority_player);
            // CR 101.4 + CR 608.2c: A per-player `ChooseFromZone { EachPlayer }`
            // iteration does NOT partition this player's pick into the
            // continuation's target slots — each pick is accumulated into the
            // chain's tracked set by `drain_pending_per_player_zone_choice`, and
            // the continuation ("put those cards onto the battlefield") reads
            // that tracked set. Hand the choice straight to the drain so it can
            // accumulate and prompt the next player (Breach the Multiverse).
            if state.pending_per_player_zone_choice.is_some() {
                effects::choose_from_zone::drain_pending_per_player_zone_choice(
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
            if state.pending_per_category_zone_choice.is_some() {
                effects::choose_from_zone::drain_pending_per_category_zone_choice(
                    state, &chosen, events,
                );
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
                .pending_continuation
                .as_ref()
                .is_some_and(|cont| effects::chain_references_tracked_set(&cont.chain));
            if continuation_consumes_tracked_set {
                effects::publish_fresh_tracked_set(state, chosen.clone());
            }
            if let Some(cont) = state.pending_continuation.as_mut() {
                cont.chain.targets = chosen.iter().map(|&id| TargetRef::Object(id)).collect();
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
                );
                if is_partition {
                    if let Some(ref mut next_sub) = cont.chain.sub_ability {
                        next_sub.targets =
                            unchosen.iter().map(|&id| TargetRef::Object(id)).collect();
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
            // `pending_optional_trigger_event` round-trip.
            let prev_trigger_event = state.current_trigger_event.clone();
            let prev_trigger_match_count = state.current_trigger_match_count;
            let prev_die_result = state.die_result_this_resolution;
            if let Some(ctx) = state.pending_choose_zone_trigger_context.take() {
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
            // / `pending_repeat_iteration` exists (each drain block is guarded by
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

            if turns::finish_cleanup_discard(state, player, &chosen, events) {
                return Ok(action_result_outcome(events, state.waiting_for.clone()));
            }

            turns::advance_phase(state, events);
            return Ok(ResolutionChoiceOutcome::WaitingFor(turns::auto_advance(
                state, events,
            )));
        }
        (
            WaitingFor::ConniveDiscard {
                player,
                conniver_id,
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

            effects::connive::add_connive_counters(state, conniver_id, nonland_count, events);
            // CR 701.50f + CR 701.50b: the EffectResolved carries the CONNIVER's
            // id (LKI if it left the battlefield) so "whenever a creature you
            // control connives" matches the conniving permanent, not the source.
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Connive,
                source_id: conniver_id,
                subject: None,
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

            let events_before_effect = events.len();
            for &card_id in &chosen {
                if let effects::discard::DiscardOutcome::NeedsReplacementChoice(choice_player) =
                    effects::discard::discard_caused_by_effect_with_source(
                        state,
                        card_id,
                        player,
                        Some(source_id),
                        events,
                    )
                {
                    state.waiting_for =
                        super::replacement::replacement_choice_waiting_for(choice_player, state);
                    return Ok(action_result_outcome(events, state.waiting_for.clone()));
                }
            }
            let events_after_move = events.len();

            // CR 608.2e + CR 608.2c: APNAP discard steps accumulate into one
            // tracked set. The discard handler is the single authority for
            // recording the cards it moved — `discard_as_cost_with_source`
            // runs outside `resolve_effect`, so its non-interactive sibling's
            // `next_sub_needs_tracked_set` publish never fires for it. Publish
            // the cards that reached the graveyard here; `chain_tracked_set_id`
            // is preserved across the per-opponent continuation pause, so each
            // opponent's publish extends the same set and the "draw a card for
            // each card discarded this way" tail reads the union.
            // CR 701.9c: only graveyard-bound cards count — a replacement
            // redirect (Madness) to another zone is excluded by the filter.
            let discarded_to_graveyard: Vec<ObjectId> = events[events_before_effect..]
                .iter()
                .filter_map(|ev| match ev {
                    GameEvent::ZoneChanged {
                        object_id,
                        to: Zone::Graveyard,
                        ..
                    } => Some(*object_id),
                    _ => None,
                })
                .collect();
            if !discarded_to_graveyard.is_empty() {
                // CR 608.2c: A `ZoneChangedThisWay` reflexive gate ("When you
                // discard a card this way, …" — Talion's Messenger, The Ancient
                // One) reads `last_zone_changed_ids`. The synchronous resolve path
                // populates that ledger from the discard's `ZoneChanged` events
                // (`effects/mod.rs`), but a discard that paused for an interactive
                // `DiscardChoice` (hand > 1) moves the chosen card HERE, after the
                // parent effect already returned. Re-publish the just-moved cards
                // into the ledger so the deferred gate, re-evaluated when the
                // stashed continuation drains, sees the discarded objects.
                state.last_zone_changed_ids = discarded_to_graveyard.clone();
                // CR 701.9a + CR 608.2c: stamp these members with the producer
                // action `Discarded` so a `caused_by: Some(Discarded)` "discarded
                // this way" consumer counts them while a `caused_by: None`
                // consumer still reads the whole id-only set. The cause is the
                // action, independent of final zone (CR 614.6).
                let with_causes = discarded_to_graveyard
                    .into_iter()
                    .map(|id| (id, Some(ThisWayCause::Discarded)))
                    .collect();
                effects::publish_tracked_set_with_causes(state, with_causes);
            }

            // CR 608.2c: "discard a card. If you do, [effect]" — the IfYouDo
            // sub_ability condition evaluates against optional_effect_performed.
            // Set it on the stashed continuation before draining so the gate
            // evaluates true when at least one card was actually discarded.
            // Mirrors the recursive AutoMayChoice::Accept path in effects/mod.rs.
            if !chosen.is_empty() {
                if let Some(cont) = state.pending_continuation.as_mut() {
                    cont.chain.set_optional_effect_performed_recursive(true);
                }
            }

            // CR 608.2c + CR 400.7j: A reflexive sub deferred across this
            // interactive discard may name the discarded card anaphorically —
            // "When you discard a card this way, target player mills cards equal
            // to ITS mana value" (The Ancient One). The synchronous resolve path
            // captures that referent via `parent_referent_context_from_events`
            // (`effects/mod.rs`); the interactive path moves the card here, after
            // the parent returned, so capture it now and stamp it onto the stashed
            // continuation. The discarded card is in the public graveyard, so its
            // characteristics are read live. Mirrors the `EffectZoneChoice` path.
            if let Some(snapshot) =
                effects::parent_referent_context_from_events(state, &events[events_before_effect..])
            {
                if let Some(cont) = state.pending_continuation.as_mut() {
                    cont.chain.set_effect_context_object_recursive(snapshot);
                }
            }

            state.last_effect_count = Some(chosen.len() as i32);
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
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            let legacy_optional_attach_empty = chosen.is_empty()
                && matches!(effect_kind, EffectKind::Attach)
                && !up_to
                && min_count == 1
                && count == 1
                && state.pending_continuation.as_ref().is_some_and(|cont| {
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

            for card_id in &chosen {
                if !cards.contains(card_id) {
                    return Err(EngineError::InvalidAction(
                        "Selected card not in eligible set".to_string(),
                    ));
                }
                if state.objects.get(card_id).map(|obj| obj.zone) != Some(zone) {
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
                && state.devour_eligible_snapshot.is_some()
                && state.pending_change_zone_iteration.is_none()
            {
                let _ = state.devour_eligible_snapshot.take();
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
                    } => {
                        set_priority(state, player);
                        resume_with_error_propagation(state, events)?;
                        if let Some(outcome) = batch_or_drain_observer_triggers(
                            state,
                            events,
                            events_before_sacrifice,
                            events_after_sacrifice,
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
                // CR 609.1 / CR 601.2a: Declining an optional
                // Electrodominance-style hand cast consumes the stashed
                // CastFromZone continuation without granting a permission. Do
                // not call the generic resume path here; the pending ability
                // would re-open the same optional prompt.
                state.pending_continuation.take();
                state.last_effect_count = Some(0);
                events.push(GameEvent::EffectResolved {
                    kind: effect_kind,
                    source_id,
                    subject: None,
                });
                set_priority(state, player);
                return Ok(ResolutionChoiceOutcome::WaitingFor(
                    state.waiting_for.clone(),
                ));
            }

            if chosen.is_empty() && matches!(effect_kind, EffectKind::Attach) {
                state.pending_continuation.take();
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
                state.last_effect_count = Some(0);
                events.push(GameEvent::EffectResolved {
                    kind: effect_kind,
                    source_id,
                    subject: None,
                });
                set_priority(state, player);
                resume_with_error_propagation(state, events)?;
                return Ok(ResolutionChoiceOutcome::WaitingFor(
                    state.waiting_for.clone(),
                ));
            }

            let events_before_effect = events.len();
            match effect_kind {
                EffectKind::Sacrifice => {
                    for &card_id in &chosen {
                        match super::sacrifice::sacrifice_permanent(state, card_id, player, events)
                        {
                            Ok(super::sacrifice::SacrificeOutcome::Complete) => {}
                            Ok(super::sacrifice::SacrificeOutcome::NeedsReplacementChoice(
                                choice_player,
                            )) => {
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
                EffectKind::ChangeZone | EffectKind::BounceAll => {
                    let dest_zone = destination.ok_or_else(|| {
                        EngineError::InvalidAction(
                            "EffectZoneChoice missing destination for zone move".to_string(),
                        )
                    })?;
                    let chosen_ids: Vec<_> = chosen.to_vec();
                    for (i, card_id) in chosen_ids.iter().enumerate() {
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
                            origin: Some(zone),
                            destination: dest_zone,
                            enter_transformed,
                            enter_tapped,
                            enters_under_player,
                            enters_attacking,
                            enter_with_counters: per_obj_enter_counters,
                            conditional_enter_with_counters: vec![],
                            duration: None,
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
                        match effects::change_zone::process_one_zone_move(
                            state, &ctx, *card_id, events,
                        ) {
                            effects::change_zone::ZoneMoveResult::Done => {
                                // CR 118.3: When this is a cost-payment exile (e.g., Mimeoplasm),
                                // populate the exile-link index map so the continuation can
                                // reference exiled cards by position (ExiledCardByIndex, ExiledCardPower).
                                if is_cost_payment && dest_zone == Zone::Exile {
                                    super::exile_links::push_exiled_with_source_this_turn(
                                        state, *card_id, source_id,
                                    );
                                }
                            }
                            effects::change_zone::ZoneMoveResult::NeedsAuraAttachmentChoice => {
                                state.pending_change_zone_iteration =
                                    Some(crate::types::game_state::PendingChangeZoneIteration {
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
                                        // CR 708.2a + CR 708.3: preserve the
                                        // face-down profile across a further pause.
                                        face_down_profile: ctx.face_down_profile.clone(),
                                        library_placement: ctx.library_placement.clone(),
                                        // CR 614.12: preserve the moved-object type
                                        // gate across a further as-enters pause.
                                        enters_modified_if: ctx.enters_modified_if.clone(),
                                        enter_attached_to: None,
                                        effect_kind,
                                    });
                                return Ok(action_result_outcome(
                                    events,
                                    state.waiting_for.clone(),
                                ));
                            }
                            effects::change_zone::ZoneMoveResult::NeedsChoice(choice_player) => {
                                // CR 614.12b + CR 614.1c + CR 614.13: stash the
                                // unprocessed cards so the drain in
                                // `effects/mod.rs::drain_pending_change_zone_iteration`
                                // resumes the loop after this replacement
                                // choice resolves (issue #535).
                                state.pending_change_zone_iteration =
                                    Some(crate::types::game_state::PendingChangeZoneIteration {
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
                                        // CR 708.2a + CR 708.3: preserve the
                                        // face-down profile across a further pause.
                                        face_down_profile: ctx.face_down_profile.clone(),
                                        library_placement: ctx.library_placement.clone(),
                                        // CR 614.12: preserve the moved-object type
                                        // gate across a further as-enters pause.
                                        enters_modified_if: ctx.enters_modified_if.clone(),
                                        enter_attached_to: None,
                                        effect_kind,
                                    });
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
                EffectKind::PutAtLibraryPosition => match library_position {
                    Some(LibraryPosition::Bottom) => {
                        for &card_id in &chosen {
                            super::zones::move_to_library_position(state, card_id, false, events);
                        }
                    }
                    Some(LibraryPosition::NthFromTop { n }) => {
                        let index = Some(n.saturating_sub(1) as usize);
                        for &card_id in &chosen {
                            super::zones::move_to_library_at_index(state, card_id, index, events);
                        }
                    }
                    _ => {
                        for &card_id in chosen.iter().rev() {
                            super::zones::move_to_library_at_index(state, card_id, Some(0), events);
                        }
                    }
                },
                // CR 608.2d + CR 301.5b: Resolution-time Equipment pick for
                // deferred optional attach (Nahiri, the Lithomancer +2).
                EffectKind::Attach => {
                    let Some(cont) = state.pending_continuation.take() else {
                        return Err(EngineError::InvalidAction(
                            "Attach EffectZoneChoice missing stashed ability".to_string(),
                        ));
                    };
                    effects::attach::complete_resolution_attachment_choice(
                        &mut *state,
                        *cont.chain,
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
                    let Some(cont) = state.pending_continuation.take() else {
                        return Err(EngineError::InvalidAction(
                            "CastFromZone EffectZoneChoice missing stashed ability".to_string(),
                        ));
                    };
                    let ability = *cont.chain;
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
                        effects::cast_from_zone::grant_lingering_permissions(
                            &mut *state,
                            &ability,
                            &chosen,
                            events,
                        )
                        .map_err(|e| EngineError::InvalidAction(e.to_string()))?;
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
                        if let Some(cont) = state.pending_continuation.as_mut() {
                            cont.chain.set_effect_context_object_recursive(snapshot);
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
                    for (i, card_id) in chosen_ids.iter().enumerate() {
                        match effects::change_zone::process_one_zone_move(
                            state, &ctx, *card_id, events,
                        ) {
                            effects::change_zone::ZoneMoveResult::Done => {
                                // CR 118.3: Populate the exile-link index map for cost-payment exile
                                if dest_zone == Zone::Exile {
                                    super::exile_links::push_exiled_with_source_this_turn(
                                        state, *card_id, source_id,
                                    );
                                }
                            }
                            effects::change_zone::ZoneMoveResult::NeedsAuraAttachmentChoice => {
                                state.pending_change_zone_iteration =
                                    Some(crate::types::game_state::PendingChangeZoneIteration {
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
                                    });
                                state.waiting_for =
                                    super::replacement::replacement_choice_waiting_for(
                                        player, state,
                                    );
                                return Ok(action_result_outcome(
                                    events,
                                    state.waiting_for.clone(),
                                ));
                            }
                            effects::change_zone::ZoneMoveResult::NeedsChoice(choice_player) => {
                                state.pending_change_zone_iteration =
                                    Some(crate::types::game_state::PendingChangeZoneIteration {
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
                                    });
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
                if let Some(cont) = state.pending_continuation.as_mut() {
                    cont.chain.set_effect_context_object_recursive(snapshot);
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
            ) && state.pending_continuation.is_some()
            {
                let tracked = if matches!(effect_kind, EffectKind::Sacrifice) {
                    events[events_before_effect..]
                        .iter()
                        .filter_map(|event| match event {
                            GameEvent::PermanentSacrificed { object_id, .. } => Some(*object_id),
                            _ => None,
                        })
                        .collect()
                } else if matches!(effect_kind, EffectKind::PutAtLibraryPosition)
                    && matches!(library_position, Some(LibraryPosition::Bottom))
                    && state.pending_continuation.is_some()
                {
                    // CR 608.2c: Expressive Iteration's bottom pick narrows the
                    // tracked set to the remaining looked-at library cards so the
                    // chained exile step cannot re-select the bottomed card.
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
                    chosen.clone()
                };
                let tracked_id = TrackedSetId(state.next_tracked_set_id);
                state.next_tracked_set_id += 1;
                state.tracked_object_sets.insert(tracked_id, tracked);
                state.chain_tracked_set_id = Some(tracked_id);
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
                if let Some(outcome) = batch_or_drain_observer_triggers(
                    state,
                    events,
                    events_before_effect,
                    events_after_move,
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
            effects::drawn_this_turn_choice::handle_topdeck_choice(
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
            .map_err(|error| EngineError::InvalidAction(error.to_string()))?;
            // Issue #423 audit: `handle_topdeck_choice` moves cards between the
            // hand and the top of the library — never off the battlefield — so
            // it produces no dies-triggers and needs no collection here.
            state.last_effect_count = Some(chosen.len() as i32);
            set_priority(state, player);
            resume_with_error_propagation(state, events)?;
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        (
            WaitingFor::NamedChoice {
                player,
                options,
                choice_type,
                source_id,
                persist_player,
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
            } else if !options.contains(&choice) {
                return Err(EngineError::InvalidAction(format!(
                    "Invalid choice '{}', must be one of: {:?}",
                    choice, options
                )));
            }

            // CR 607.2d + CR 613.1: Persist the chosen attribute on the source
            // (Morophon buffs, Pithing Needle prohibitions, Serra's Emissary
            // protection, Sewer Nemesis CDA, …), recompute layers for the
            // layer-affecting choice kinds, and record `last_named_choice`.
            // Single authority shared with the random `Effect::Choose` resolver.
            effects::choose::bind_named_choice(
                state,
                &choice_type,
                &choice,
                source_id,
                persist_player,
            );
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
                ChoiceType::Player | ChoiceType::Opponent { .. }
            ) {
                if let Ok(pid) = choice.parse::<u8>() {
                    if let Some(cont) = state.pending_continuation.as_mut() {
                        let mut chosen = cont.chain.chosen_players.clone();
                        chosen.push(crate::types::player::PlayerId(pid));
                        cont.chain.set_chosen_players_recursive(&chosen);
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
                        state, player, *pending, ability, cost, events,
                    ) {
                        Ok(waiting_for) => waiting_for,
                        Err(err) => {
                            state.pending_cast = Some(pending_for_restore);
                            return Err(err);
                        }
                    };
                }
            } else if let Some(source) =
                source_id.filter(|_| !state.deferred_entry_events.is_empty())
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
                source_id,
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
            let outcome = if effects::opponent_guess::guess_is_correct(
                state,
                source_id,
                &options,
                &choice,
                proposition_truth,
            ) {
                GuessOutcome::Correct
            } else {
                GuessOutcome::Incorrect
            };

            // (c) Record the guessed value WITHOUT persisting it to the source.
            // "they lose life equal to the number they guessed" reads the
            // guesser's value via `QuantityRef::Variable` -> `last_named_choice`;
            // `source_id: None` sets `last_named_choice` without pushing a
            // `ChosenAttribute::Number` (bind_named_choice gates the push on
            // source_id.is_some()), keeping the source's committed-number history
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
            if let Some(cont) = state.pending_continuation.as_mut() {
                cont.chain.set_guess_outcome_recursive(Some(outcome));
                cont.chain.push_front_player_target_recursive(guesser);
            }

            // (e) Priority to the source CONTROLLER (the player resolving this
            // ability), not the guesser — the guesser only answered a sub-prompt;
            // the resolution continues under the controller (e.g. Seventh
            // Doctor's "you may cast it" CastOffer is to the controller). This
            // also clears the OpponentGuess wait so the drain's guard passes.
            let controller = state
                .objects
                .get(&source_id)
                .map(|o| o.controller)
                .unwrap_or(state.active_player);
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
                batch_or_drain_observer_triggers(state, events, events.len(), events.len())
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
                batch_or_drain_observer_triggers(state, events, events_before, events.len())
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
            if let Some(outcome) =
                batch_or_drain_observer_triggers(state, events, events_before_venture, events.len())
            {
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
            if let Some(outcome) =
                batch_or_drain_observer_triggers(state, events, events_before_venture, events.len())
            {
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
        // Accept = move to command zone; Decline = leave in current zone (marked as
        // declined so SBA doesn't re-ask).
        // Returning to Priority re-runs SBA, which will find any remaining commanders.
        (
            WaitingFor::CommanderZoneChoice { commander_id, .. },
            GameAction::DecideOptionalEffect { accept },
        ) => {
            if accept {
                zones::move_to_zone(state, commander_id, Zone::Command, events);
            } else {
                state.commander_declined_zone_return.insert(commander_id);
            }
            ResolutionChoiceOutcome::WaitingFor(WaitingFor::Priority {
                player: state.active_player,
            })
        }
        // CR 310.10 + CR 704.5w + CR 704.5x: controller assigns the battle's new
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
                );
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
                    clear_markers,
                    publish_tracked_set: None,
                    emit_reveal_until_resolved: None,
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
            clear_markers,
            publish_tracked_set,
            emit_reveal_until_resolved,
        } => {
            // The dig path (`publish_tracked_set.is_some()`) routes the rest pile
            // through `route_rest_partition` (ordered library bottom); the
            // reveal-until path routes through `move_rest_then`, including
            // Library-bottom placement and any CR 616.1 pause. Dispatch on the
            // dig-only payload so each site keeps its synchronous semantics.
            if publish_tracked_set.is_some() {
                match route_rest_partition(state, &rest_cards, rest_destination, source_id, events)
                {
                    crate::game::zone_pipeline::BatchMoveResult::Done => {}
                    crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => {
                        crate::game::zone_pipeline::defer_completion_on_pause(
                            state,
                            BatchCompletion::RevealRestPile {
                                player,
                                source_id,
                                rest_cards: Vec::new(),
                                rest_destination,
                                clear_markers,
                                publish_tracked_set,
                                emit_reveal_until_resolved,
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
                    clear_markers,
                    publish_tracked_set: None,
                    emit_reveal_until_resolved,
                };
                return effects::reveal_until::move_rest_then(
                    state,
                    &rest_cards,
                    rest_destination,
                    Some(cleanup),
                    events,
                );
            }
            for card_id in &clear_markers {
                state.revealed_cards.remove(card_id);
            }
            if let Some(kept) = publish_tracked_set {
                effects::publish_fresh_tracked_set(state, kept.clone());
                if let Some(cont) = state.pending_continuation.as_mut() {
                    cont.chain.targets = kept.iter().map(|&id| TargetRef::Object(id)).collect();
                    cont.chain.context.optional_effect_performed = !kept.is_empty();
                }
            }
            if let Some(source_id) = emit_reveal_until_resolved {
                events.push(crate::types::events::GameEvent::EffectResolved {
                    kind: crate::types::ability::EffectKind::RevealUntil,
                    source_id,
                    subject: None,
                });
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
        } => {
            crate::game::effects::dig::move_mass_put_all_selected(
                state,
                player,
                source_id,
                selected,
                destination,
                enter_tapped,
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
        // same completion; `drain_pending_batch_deliveries` took the old record
        // before calling here, so a fresh park is preserved).
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
        BatchCompletion::ScopedLibrarySearchDelivery {
            player,
            source_id,
            after_scope,
        } => {
            // CR 101.4 + CR 701.23i + CR 616.1: the parked batch has finally
            // delivered every selected card. The searched-this-way shuffle tail
            // may now resolve; a failure here would mean corrupted serialized
            // engine state because the completion is created only by the typed
            // scoped-search protocol above.
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

/// CR 701.25a: place the kept surveil cards on top of the player's library in
/// the chosen order (`top_cards[0]` becomes the topmost card). Shared by the
/// synchronous surveil handler and the deferred batch completion so the ordering
/// is identical on both paths.
fn surveil_keep_on_top(
    state: &mut GameState,
    player: crate::types::player::PlayerId,
    top_cards: &[ObjectId],
) {
    zones::reorder_within_library(state, player, top_cards, 0);
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
        state.pending_continuation = Some(crate::types::game_state::PendingContinuation::new(
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
        assert!(state.pending_continuation.is_none());
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
                source_id: None,
                enter_tapped: false,
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
            player: PlayerId(1),
            choice_type: ChoiceType::CardPredicateGuess {
                options: ChoiceType::land_or_nonland_card_predicate_options(),
            },
            options: ChoiceType::card_predicate_labels(
                &ChoiceType::land_or_nonland_card_predicate_options(),
            ),
            source_id: Some(source_id),
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
            player: PlayerId(0),
            choice_type: ChoiceType::CardPredicate {
                options: ChoiceType::land_or_nonland_card_predicate_options(),
            },
            options: ChoiceType::card_predicate_labels(
                &ChoiceType::land_or_nonland_card_predicate_options(),
            ),
            source_id: Some(source_id),
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
}
