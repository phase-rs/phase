use std::collections::HashMap;

use crate::types::ability::{
    ChoiceType, ChoiceValue, ChosenAttribute, Effect, EffectKind, PaymentCost, QuantityExpr,
    QuantityRef, ResolvedAbility, TargetRef,
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
use super::{casting, casting_costs};

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

/// CR 701.20e / CR 701.23a + CR 401.4: Move the "rest" partition of an
/// interactive selection (Dig's unkept cards, a search-split's non-primary
/// cards) to a concrete destination zone. `Library` routes to the bottom of the
/// owner's library (CR 401.4); every other zone uses the standard cross-zone
/// mover. Extracted from the Dig rest-move block so the search-partition handler
/// reuses the exact same routing.
fn route_rest_partition(
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
                face_down_profile: None,
            },
            primary_targets,
            source_id,
            controller,
        );
        crate::game::effects::change_zone::resolve(state, &change_zone, events)
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
            WaitingFor::ManifestDreadChoice { player, cards },
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

            crate::game::morph::manifest_card(state, player, manifest_id, events)
                .map_err(|error| EngineError::InvalidAction(format!("{error}")))?;

            for card_id in graveyard_cards {
                zones::move_to_zone(state, card_id, Zone::Graveyard, events);
            }

            for &card_id in &cards {
                state.revealed_cards.remove(&card_id);
            }

            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
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
                    Some(crate::types::ability::CastPermissionConstraint::ManaValue {
                        comparator: crate::types::ability::Comparator::LE,
                        value: QuantityExpr::Fixed {
                            value: discover_value as i32,
                        },
                    }),
                    false,
                    cleanup,
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
                zones::move_to_zone(state, hit_card, accept_zone, events);
                // CR 110.5b: the kept card enters tapped when requested.
                if enter_tapped {
                    if let Some(obj) = state.objects.get_mut(&hit_card) {
                        obj.tapped = true;
                    }
                }
                // CR 508.4: "...tapped and attacking" — place the accepted card
                // in combat. `source_id` (the ability source / trigger attacker)
                // supplies the defending player, matching the synchronous path.
                if enters_attacking && accept_zone == Zone::Battlefield {
                    let controller = state
                        .objects
                        .get(&hit_card)
                        .map(|obj| obj.controller)
                        .unwrap_or(player);
                    crate::game::combat::enter_attacking(state, hit_card, source_id, controller);
                }
            } else if decline_zone == rest_destination {
                misses.push(hit_card);
            } else {
                zones::move_to_zone(state, hit_card, decline_zone, events);
            }
            effects::reveal_until::move_rest(state, &misses, rest_destination, events);
            // CR 701.20b: revealed cards have now moved zones — clear markers.
            for &card_id in &misses {
                state.revealed_cards.remove(&card_id);
            }
            state.revealed_cards.remove(&hit_card);
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
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
                    Some(crate::types::ability::CastPermissionConstraint::ManaValue {
                        comparator: crate::types::ability::Comparator::LT,
                        value: QuantityExpr::Fixed {
                            value: source_mv as i32,
                        },
                    }),
                    false,
                    cleanup,
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
                    state, player, hit_card, None, false, cleanup, events,
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
                state, player, chosen, None, false, cleanup, events,
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
                        effects::discard::discard_as_cost(state, card_id, player, events)
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
                    let _ = effects::draw::resolve(state, &draw_ability, events);
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
            },
            GameAction::SubmitPayAmount { amount },
        ) => {
            if amount < min || amount > max {
                return Err(EngineError::InvalidAction(format!(
                    "Submitted pay amount {} outside legal range [{}, {}]",
                    amount, min, max
                )));
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
                    for &obj_id in &unkept {
                        zones::move_to_zone(state, obj_id, zone, events);
                    }
                }
                return Ok(ResolutionChoiceOutcome::WaitingFor(
                    finish_with_continuation(state, player, events),
                ));
            }
            if let Some(kept_zone) = kept_destination {
                for &obj_id in &kept {
                    zones::move_to_zone(state, obj_id, kept_zone, events);
                    if enter_tapped && kept_zone == Zone::Battlefield {
                        if let Some(obj) = state.objects.get_mut(&obj_id) {
                            obj.tapped = true;
                        }
                    }
                }
            }
            // CR 701.20b + CR 608.2c: Publish the kept (revealed) cards as a
            // tracked set so downstream sub_abilities can route them by type
            // via `TargetFilter::TrackedSetFiltered`. Used by Zimone's
            // Experiment — "Put all land cards revealed this way onto the
            // battlefield tapped and put all creature cards revealed this way
            // into your hand" consume this set. Use a fresh tracked set so a
            // parent effect's empty pre-choice publish cannot keep the chain
            // sentinel bound to the wrong set.
            effects::publish_fresh_tracked_set(state, kept.clone());
            // None => Graveyard; map to a concrete zone so the rest mover
            // (shared with the search-split partition) has a single Zone.
            route_rest_partition(
                state,
                &unkept,
                rest_destination.unwrap_or(Zone::Graveyard),
                events,
            );
            if let Some(cont) = state.pending_continuation.as_mut() {
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
            // CR 701.25a: every looked-at card not kept on top is put into the
            // graveyard (emitting its zone-change events).
            for &obj_id in &to_graveyard {
                zones::move_to_zone(state, obj_id, Zone::Graveyard, events);
            }
            // CR 701.25a: the kept cards rest on top of the library in the
            // player's chosen order (top_cards[0] becomes the topmost card).
            let player_state = state
                .players
                .iter_mut()
                .find(|candidate| candidate.id == player)
                .expect("player exists");
            player_state.library.retain(|id| !top_cards.contains(id));
            for (index, &card_id) in top_cards.iter().enumerate() {
                player_state.library.insert(index, card_id);
            }
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
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
                constraint,
                split,
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            // CR 107.1c + CR 701.23d: "up to N" / "any number of" accept 0..=count picks.
            let valid = if up_to {
                chosen.len() <= count
            } else {
                chosen.len() == count
            };
            if !valid {
                return Err(EngineError::InvalidAction(format!(
                    "Must select {}{} card(s), got {}",
                    if up_to { "up to " } else { "exactly " },
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
                        | crate::types::ability::Effect::AddCounter { .. }
                );
                if is_partition {
                    if let Some(ref mut next_sub) = cont.chain.sub_ability {
                        next_sub.targets =
                            unchosen.iter().map(|&id| TargetRef::Object(id)).collect();
                    }
                }
            }
            effects::drain_pending_continuation(state, events);
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
                source_id,
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
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Connive,
                source_id,
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
                    effects::discard::discard_as_cost_with_source(
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
                effects::publish_tracked_set(state, discarded_to_graveyard);
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

            state.last_effect_count = Some(chosen.len() as i32);
            events.push(GameEvent::EffectResolved {
                kind: effect_kind,
                source_id,
            });
            let waiting_for = finish_with_continuation(state, player, events);

            // CR 603.2c: each opponent's discard is a separate occurrence of a
            // `Discarded`-mode trigger event. The resolution-choice dispatch
            // path does not call `run_post_action_pipeline` for a non-settled
            // action, so batch this discard's observer triggers (Waste Not,
            // Megrim, Bone Miser) across the `DiscardChoice` pause — exactly
            // as the `Sacrifice` branch does for dies-triggers.
            if let Some(outcome) = batch_or_drain_observer_triggers(
                state,
                events,
                events_before_effect,
                events_after_move,
            ) {
                return Ok(outcome);
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
                count_param,
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
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
            } else if chosen.len() != count {
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
                        duration: None,
                        track_exiled_by_source,
                    };
                    let chosen_ids: Vec<_> = chosen.to_vec();
                    for (i, card_id) in chosen_ids.iter().enumerate() {
                        match effects::change_zone::process_one_zone_move(
                            state, &ctx, *card_id, events,
                        ) {
                            effects::change_zone::ZoneMoveResult::Done => {}
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
                                        enter_with_counters: ctx.enter_with_counters.clone(),
                                        duration: ctx.duration.clone(),
                                        track_exiled_by_source: ctx.track_exiled_by_source,
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
                                        enter_with_counters: ctx.enter_with_counters.clone(),
                                        duration: ctx.duration.clone(),
                                        track_exiled_by_source: ctx.track_exiled_by_source,
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
                // selection order (first chosen = top).
                EffectKind::PutAtLibraryPosition => {
                    for &card_id in chosen.iter().rev() {
                        super::zones::move_to_library_at_index(state, card_id, Some(0), events);
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

            if let Some(obj_id) = source_id {
                if let Some(attr) = ChosenAttribute::from_choice(choice_type.clone(), &choice) {
                    if let Some(obj) = state.objects.get_mut(&obj_id) {
                        obj.chosen_attributes.push(attr);
                        // CR 607.2d + CR 613.1: Persisted ETB/modal choices (card
                        // name, creature type, card type, color, etc.) can gate
                        // source-dependent continuous or rule effects. Layer
                        // evaluation may have run before the choice was made
                        // (Morophon buffs, Pithing Needle prohibitions, Serra's
                        // Emissary protection, …) — re-run.
                        if matches!(
                            choice_type,
                            ChoiceType::CardName
                                | ChoiceType::CreatureType
                                | ChoiceType::CardType
                                | ChoiceType::BasicLandType
                                | ChoiceType::Color { .. }
                                | ChoiceType::Keyword { .. }
                                // CR 613.1: a persisted "choose a player" gates
                                // CDA P/T that count the chosen player's objects
                                // or zones (Sewer Nemesis, Skyshroud War Beast) —
                                // recompute layers immediately.
                                | ChoiceType::Player
                                | ChoiceType::Opponent
                        ) {
                            crate::game::layers::mark_layers_full(state);
                        }
                    }
                }
            }

            state.last_named_choice = ChoiceValue::from_choice(&choice_type, &choice);

            // CR 608.2c + CR 109.4: A `Choose(Player)`/`Choose(Opponent)`
            // answer binds a resolution-scoped chosen player. Append it to the
            // pending continuation chain's `chosen_players` so the dependent
            // effect (`ControllerRef::ChosenPlayer { index }`) and any later
            // `Choose(Player)` in the same resolution see this choice. The
            // continuation chain carries the list because it is a
            // `ResolvedAbility` — unlike `last_named_choice`, which is a
            // single GameState slot cleared after every drain.
            if matches!(choice_type, ChoiceType::Player | ChoiceType::Opponent) {
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
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
        }
        (WaitingFor::ChooseDungeon { player, options }, GameAction::ChooseDungeon { dungeon }) => {
            if !options.contains(&dungeon) {
                return Err(EngineError::InvalidAction(
                    "Invalid dungeon choice".to_string(),
                ));
            }
            effects::venture::handle_choose_dungeon(state, player, dungeon, events);
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
            effects::venture::handle_choose_room(state, player, dungeon, room_index, events);
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
            for id in to_remove {
                zones::move_to_zone(state, id, Zone::Graveyard, events);
            }
            ResolutionChoiceOutcome::WaitingFor(WaitingFor::Priority {
                player: state.active_player,
            })
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
            crate::game::cipher::handle_encode_choice(state, card_id, creature, events);
            ResolutionChoiceOutcome::WaitingFor(WaitingFor::Priority {
                player: state.active_player,
            })
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

fn starts_with_pay_amount_prompt(ability: &ResolvedAbility) -> bool {
    match &ability.effect {
        Effect::PayCost {
            cost: PaymentCost::Mana { cost },
            ..
        } => casting_costs::cost_has_x(cost),
        Effect::PayCost {
            cost: PaymentCost::Energy { amount },
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
