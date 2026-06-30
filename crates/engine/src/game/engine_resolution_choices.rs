use std::collections::HashMap;

use crate::types::ability::{
    AbilityCost, ChoiceType, ChosenAttribute, Effect, EffectKind, LibraryPosition, QuantityExpr,
    QuantityRef, ResolvedAbility, TargetRef, ThisWayCause,
};
use crate::types::actions::{GameAction, LearnOption, OutsideGameSelection};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    ActionResult, CastOfferKind, ChosenDamageSource, GameState, OutsideGameChoiceSource,
    PayableResource, PendingContinuation, WaitingFor,
};
use crate::types::identifiers::{ObjectId, TrackedSetId};
use crate::types::mana::ManaCost;
use crate::types::zones::Zone;

use super::effects;
use super::engine::EngineError;
use super::turns;
use super::zones;
use super::{casting, casting_costs, mana_abilities};

pub(super) enum ResolutionChoiceOutcome {
    WaitingFor(WaitingFor),
    WaitingForWithInlineTriggers(WaitingFor),
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

pub(super) fn handles(waiting_for: &WaitingFor) -> bool {
    matches!(
        waiting_for,
        WaitingFor::ScryChoice { .. }
            | WaitingFor::CoinFlipKeepChoice { .. }
            | WaitingFor::ManifestDreadChoice { .. }
            | WaitingFor::CastOffer {
                kind: CastOfferKind::Discover { .. },
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
            | WaitingFor::SeparatePilesPartition { .. }
            | WaitingFor::SeparatePilesChoice { .. }
            | WaitingFor::DigChoice { .. }
            | WaitingFor::SurveilChoice { .. }
            | WaitingFor::RevealChoice { .. }
            | WaitingFor::SearchChoice { .. }
            | WaitingFor::SearchPartitionChoice { .. }
            | WaitingFor::OutsideGameChoice { .. }
            | WaitingFor::ChooseFromZoneChoice { .. }
            | WaitingFor::ChooseOneOfBranch { .. }
            | WaitingFor::DiscardToHandSize { .. }
            | WaitingFor::ConniveDiscard { .. }
            | WaitingFor::DiscardChoice { .. }
            | WaitingFor::EffectZoneChoice { .. }
            | WaitingFor::DrawnThisTurnTopdeckChoice { .. }
            | WaitingFor::NamedChoice { .. }
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
    events: &mut Vec<GameEvent>,
) {
    match rest_zone {
        Zone::Library => {
            for &obj_id in rest_ids {
                zones::move_to_library_position(state, obj_id, false, events);
            }
        }
        zone => {
            // ZONE-PIPELINE GAP (documented deferral): the dig/search "rest into
            // your graveyard" partition (65 Dig cards + the `null`→Graveyard
            // default) is delivered raw here, so a `Moved` graveyard→exile redirect
            // (Rest in Peace / Leyline of the Void) does NOT yet fire on these
            // rest cards. The sibling dig unkept-loop (handle_resolution_choice
            // DigChoice) and the reveal-until rest pile (effects::reveal_until::
            // move_rest_then) ARE migrated; this shared partition helper is not,
            // because it has three callers — two synchronous (search-split at
            // `apply_search_partition`, dig at the kept block) and ONE inside
            // `run_batch_completion`'s RevealRestPile arm.
            //
            // RE-PAUSE CONTRACT STATUS (updated): the "pause from inside a
            // completion" blocker named here previously is RESOLVED — the
            // re-pause contract documented on
            // `zone_pipeline::drain_pending_batch_deliveries` now covers a fresh
            // park raised FROM a completion (the drain `.take()`s the old record
            // BEFORE invoking the completion, so a completion that re-enters the
            // batch machinery installs a clean record; see the
            // `AttractionOpenRemainder` arm, which already pauses + re-defers
            // through this exact path). The ONLY remaining work to migrate this
            // helper onto `move_objects_simultaneously` is threading the
            // `BatchMoveResult::NeedsChoice` return through the two SYNCHRONOUS
            // callers (`apply_search_partition` and the dig kept-block), each of
            // which currently returns `Result<(), _>` / `()` and would need to
            // surface a parked prompt the same way the migrated DigChoice
            // unkept-loop does (`defer_completion_on_pause` + early return). That
            // is a cross-cutting signature change across both callers; tracked for
            // a follow-up so it lands as one reviewable unit rather than a partial
            // migration here. (Practical exposure: a graveyard-redirect on a dig
            // REST pile — the non-kept cards — with RIP/Leyline on the
            // battlefield.)
            for &obj_id in rest_ids {
                zones::move_to_zone(state, obj_id, zone, events);
            }
        }
    }
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
) -> Result<(), EngineError> {
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
            },
            primary_targets,
            source_id,
            controller,
        );
        crate::game::effects::resolve_ability_chain(state, &change_zone, events, 0)
            .map_err(|e| EngineError::InvalidAction(format!("search-split primary move: {e:?}")))?;
    }
    // Rest is never Battlefield across the A/B/C cluster; the standard rest mover
    // (Library => bottom per CR 401.4, else move_to_zone) is correct.
    route_rest_partition(state, rest_ids, split.rest_destination, events);
    Ok(())
}

pub(super) fn handle_resolution_choice(
    state: &mut GameState,
    waiting_for: WaitingFor,
    action: GameAction,
    events: &mut Vec<GameEvent>,
) -> Result<ResolutionChoiceOutcome, EngineError> {
    let outcome = match (waiting_for, action) {
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
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
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
                        exile_instead_of_graveyard_on_resolve: false,
                    },
                    events,
                )?;
                ResolutionChoiceOutcome::WaitingFor(result)
            } else {
                // CR 701.57a: decline — hit goes to the discovering player's
                // hand; the misses go to the library bottom in a random order.
                zones::move_to_zone(state, hit_card, Zone::Hand, events);

                {
                    use rand::seq::SliceRandom;

                    let mut shuffled = exiled_misses;
                    shuffled.shuffle(&mut state.rng);
                    for card_id in shuffled {
                        zones::move_to_library_position(state, card_id, false, events);
                    }
                }

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
                        exile_instead_of_graveyard_on_resolve: false,
                    },
                    events,
                )?;
                ResolutionChoiceOutcome::WaitingFor(result)
            } else {
                // CR 702.85a: Caster declines — hit and misses all go to the
                // bottom of the library in a random order together.
                let mut all_to_bottom = exiled_misses;
                all_to_bottom.push(hit_card);
                crate::game::effects::cascade::shuffle_to_bottom(state, &all_to_bottom, events);

                ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
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
                        exile_instead_of_graveyard_on_resolve: false,
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
                crate::game::effects::cascade::shuffle_to_bottom(state, &all_to_bottom, events);

                ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
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
                    .map(|obj| obj.mana_cost.mana_value())
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
                    exile_instead_of_graveyard_on_resolve: false,
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
                        state.pending_continuation = Some(PendingContinuation::new(Box::new(draw)));
                        events.push(GameEvent::EffectResolved {
                            kind: EffectKind::Learn,
                            source_id: ObjectId(0),
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
                LearnOption::Skip => {}
            }

            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Learn,
                source_id: ObjectId(0),
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
                effects::drain_pending_continuation(state, events);
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
            },
            GameAction::ChooseOption { choice },
        ) => {
            // CR 701.38a: Validate the cast vote. Compare lowercase against
            // the canonical options list; reject anything else.
            let lower = choice.to_lowercase();
            let Some(idx) = options.iter().position(|o| o == &lower) else {
                return Err(EngineError::InvalidAction(format!(
                    "Invalid vote '{}'; valid choices are {:?}",
                    choice, options
                )));
            };
            let mut new_tallies = tallies.clone();
            new_tallies[idx] += 1;
            // CR 608.2c + CR 701.38: Append the per-vote ballot. `idx` is
            // guaranteed to fit in `u8` because `parse_vote_block` rejects
            // any vote AST with more than a few choices (no Magic card has
            // ever exceeded ~3-5 vote options).
            let mut new_ballots = ballots.clone();
            new_ballots.push_back((player, idx as u8));
            events.push(GameEvent::VoteCast {
                voter: player,
                choice: lower,
                source_id,
            });

            if remaining_votes > 1 {
                // CR 701.38d: Same player still has votes to cast — `player`
                // and `actor` are both unchanged.
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
                };
                ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
            } else if let Some(((next_player, next_votes), rest)) = remaining_voters.split_first() {
                // CR 101.4: Advance to the next voter in turn order.
                // `actor` carries forward unchanged: `SubjectActs` re-resolves
                // to whichever player is the next subject on each step, while
                // `Delegated(p)` keeps `p` pinned across subjects.
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
                };
                ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
            } else {
                // CR 701.38: All votes cast — resolve per-choice sub-effects,
                // emit the final tally event, then drain any post-Vote
                // continuation (e.g., a chained effect).
                events.push(GameEvent::VoteResolved {
                    source_id,
                    tallies: options
                        .iter()
                        .cloned()
                        .zip(new_tallies.iter().copied())
                        .collect(),
                });
                let _ = effects::vote::resolve_tally(
                    state,
                    source_id,
                    controller,
                    &options,
                    &per_choice_effect,
                    &new_tallies,
                    &new_ballots,
                    tally_mode,
                    events,
                );
                ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(
                    state, controller, events,
                ))
            }
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
                source_id,
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
                    source_id,
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
                    source_id,
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
                source_id,
            },
            GameAction::ChoosePile { pile },
        ) => {
            // CR 101.4c: Resolve this subject's chosen pile NOW (one
            // `Sacrifice` per object), then either park for the next
            // subject's choice or finish. Per-decision resolution matches
            // CR 101.4c ("in any order they choose") — the chooser's
            // submission order IS that order.
            let _ = effects::separate_piles::apply_pile_effect(
                state,
                source_id,
                &chosen_pile_effect,
                &[(current, pile)],
                events,
            );
            if let Some(next) = pending.pop_front() {
                state.waiting_for = WaitingFor::SeparatePilesChoice {
                    player,
                    pending,
                    current: next,
                    chosen_pile_effect,
                    source_id,
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
            effects::publish_fresh_tracked_set(state, publish_set);
            // None => Graveyard; map to a concrete zone so the rest mover
            // (shared with the search-split partition) has a single Zone.
            // When a continuation owns the unkept pile (Expressive Iteration
            // bottom/exile tail), do not pre-route here.
            let defer_rest_routing = state
                .pending_continuation
                .as_ref()
                .is_some_and(|cont| dig_continuation_needs_full_looked_at_tracked_set(&cont.chain));
            if !defer_rest_routing {
                route_rest_partition(
                    state,
                    &unkept,
                    rest_destination.unwrap_or(Zone::Graveyard),
                    events,
                );
            }
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
                    effects::drain_pending_continuation(state, events);
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
            effects::drain_pending_continuation(state, events);
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
        }
        (
            WaitingFor::SearchChoice {
                player,
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

            if reveal {
                state.last_revealed_ids = chosen.clone();
                for &card_id in &chosen {
                    state.revealed_cards.insert(card_id);
                }
                let card_names: Vec<String> = chosen
                    .iter()
                    .filter_map(|id| state.objects.get(id).map(|obj| obj.name.clone()))
                    .collect();
                events.push(GameEvent::CardsRevealed {
                    player,
                    card_ids: chosen.clone(),
                    card_names,
                });
            } else {
                state.last_revealed_ids.clear();
            }

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
                apply_search_partition(state, &chosen, &[], &split, source_id, player, events)?;
                set_priority(state, player);
                effects::drain_pending_continuation(state, events);
                return Ok(ResolutionChoiceOutcome::WaitingFor(
                    state.waiting_for.clone(),
                ));
            }

            set_priority(state, player);
            if let Some(cont) = state.pending_continuation.as_mut() {
                let mut continuation_targets: Vec<_> =
                    chosen.iter().map(|&id| TargetRef::Object(id)).collect();
                // CR 701.23a + CR 701.24a: When the searcher is not the caster
                // (e.g., "its controller may search their library, ..., then
                // shuffle" for Assassin's Trophy), propagate the searcher's
                // PlayerId into the continuation chain's targets so downstream
                // untargeted-Shuffle / Library-owner-sensitive effects pick up
                // the correct player via `ability.target_player()`.
                if player != cont.chain.controller {
                    continuation_targets.push(TargetRef::Player(player));
                }
                cont.chain.targets = continuation_targets.clone();
                propagate_targets_through_search_shuffle(&mut cont.chain, &continuation_targets);
            }
            effects::drain_pending_continuation(state, events);
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
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
            apply_search_partition(
                state,
                &primary_chosen,
                &rest_ids,
                &split,
                source_id,
                player,
                events,
            )?;
            set_priority(state, player);
            effects::drain_pending_continuation(state, events);
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
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
                effects::drain_pending_continuation(state, events);
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
                effects::drain_pending_continuation(state, events);
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
            effects::drain_pending_continuation(state, events);
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
            effects::drain_pending_continuation(state, events);
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

            // CR 608.2e + CR 609.3: APNAP discard steps accumulate into one
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
            effects::choose::bind_named_choice(state, &choice_type, &choice, source_id);

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

            set_priority(state, player);
            if let Some(pending) = state.pending_cast.take() {
                if let Some(ability_index) = pending.activation_ability_index {
                    state.waiting_for = casting_costs::push_activated_ability_to_stack(
                        state,
                        player,
                        pending.object_id,
                        ability_index,
                        pending.ability,
                        pending.activation_cost.as_ref(),
                        pending.activation_residual,
                        events,
                    )?;
                } else {
                    state.waiting_for = casting_costs::finalize_cast(
                        state,
                        player,
                        pending.object_id,
                        pending.card_id,
                        pending.ability,
                        &pending.cost,
                        pending.casting_variant,
                        pending.cast_timing_permission,
                        pending.origin_zone,
                        events,
                    )?;
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
            } else {
                effects::drain_pending_continuation(state, events);
            }
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
            effects::drain_pending_continuation(state, events);
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
    effects::drain_pending_continuation(state, events);
    state.waiting_for.clone()
}

/// CR 701.25a / manifest dread: run the post-loop cleanup a rest-pile batch
/// deferred when it paused mid-pile. Called by
/// `zone_pipeline::drain_pending_batch_deliveries` the moment the batch tail
/// empties, so the kept-card placement / reveal-marker cleanup and the
/// continuation drain happen exactly once — the same effect the synchronous
/// (never-paused) path runs inline.
pub(crate) fn run_batch_completion(
    state: &mut GameState,
    completion: crate::types::game_state::BatchCompletion,
    events: &mut Vec<GameEvent>,
) {
    use crate::types::game_state::BatchCompletion;
    match completion {
        BatchCompletion::SurveilKeepOnTop { player, top_cards } => {
            surveil_keep_on_top(state, player, &top_cards);
            finish_with_continuation(state, player, events);
        }
        BatchCompletion::ManifestDreadCleanup { player, revealed } => {
            for card_id in &revealed {
                state.revealed_cards.remove(card_id);
            }
            finish_with_continuation(state, player, events);
        }
        // CR 701.20b: The kept card's battlefield entry paused (aura host pick /
        // replacement ordering). Now that it has resolved, move the unkept rest
        // pile, clear the reveal markers, run the dig tracked-set publish +
        // continuation wiring (if any), then drain the continuation — exactly the
        // tail the synchronous path runs inline.
        BatchCompletion::RevealRestPile {
            player,
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
                route_rest_partition(state, &rest_cards, rest_destination, events);
            } else if !rest_cards.is_empty() {
                // CR 701.20a + CR 616.1: Reveal-until rest piles are fully
                // pipeline-owned, including Library-bottom placement. If a
                // Library-destination `Moved` replacement pauses here, re-stash
                // this completion as cleanup-only so reveal markers and
                // continuation drain run after the pile actually lands.
                let cleanup = BatchCompletion::RevealRestPile {
                    player,
                    rest_cards: Vec::new(),
                    rest_destination,
                    clear_markers,
                    publish_tracked_set: None,
                    emit_reveal_until_resolved,
                };
                match effects::reveal_until::move_rest_then(
                    state,
                    &rest_cards,
                    rest_destination,
                    Some(cleanup),
                    events,
                ) {
                    crate::game::zone_pipeline::BatchMoveResult::Done
                    | crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => return,
                }
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
                });
            }
            finish_with_continuation(state, player, events);
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
    let player_state = state
        .players
        .iter_mut()
        .find(|candidate| candidate.id == player)
        .expect("player exists");
    player_state.library.retain(|id| !top_cards.contains(id));
    for (index, &card_id) in top_cards.iter().enumerate() {
        player_state.library.insert(index, card_id);
    }
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
