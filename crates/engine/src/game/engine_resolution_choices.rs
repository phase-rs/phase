use std::collections::HashMap;

use crate::types::ability::{
    CategoryChooserScope, ChoiceType, ChoiceValue, ChosenAttribute, Effect, EffectKind,
    PaymentCost, QuantityExpr, QuantityRef, ResolvedAbility, TargetRef,
};
use crate::types::actions::{GameAction, LearnOption, OutsideGameSelection};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    ActionResult, ChosenDamageSource, GameState, OutsideGameChoiceSource, PayableResource,
    PendingContinuation, WaitingFor,
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
) -> Option<WaitingFor> {
    if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        // B1: this action settled — `run_post_action_pipeline` scans this
        // action's own events; only the prior parked queue needs draining.
        super::triggers::drain_deferred_trigger_queue(state, events)
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
            | WaitingFor::ManifestDreadChoice { .. }
            | WaitingFor::DiscoverChoice { .. }
            | WaitingFor::RevealUntilKeptChoice { .. }
            | WaitingFor::RepeatDecision { .. }
            | WaitingFor::CascadeChoice { .. }
            | WaitingFor::LearnChoice { .. }
            | WaitingFor::TopOrBottomChoice { .. }
            | WaitingFor::PopulateChoice { .. }
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
            | WaitingFor::DamageSourceChoice { .. }
            | WaitingFor::ChooseRingBearer { .. }
            | WaitingFor::ChooseDungeon { .. }
            | WaitingFor::ChooseDungeonRoom { .. }
            | WaitingFor::ChooseLegend { .. }
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
            WaitingFor::DiscoverChoice {
                player,
                hit_card,
                exiled_misses,
            },
            GameAction::DiscoverChoice { choice },
        ) => {
            let cast = matches!(choice, crate::types::actions::CastChoice::Cast);
            if cast {
                if let Some(obj) = state.objects.get_mut(&hit_card) {
                    obj.casting_permissions.push(
                        crate::types::ability::CastingPermission::ExileWithAltCost {
                            cost: crate::types::mana::ManaCost::zero(),
                            cast_transformed: false,
                            constraint: None,
                            // CR 702.190a (Discover): the discovering player is
                            // the only player permitted to cast the hit card.
                            granted_to: Some(player),
                        },
                    );
                }
            } else {
                zones::move_to_zone(state, hit_card, Zone::Hand, events);
            }

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
        // CR 701.20a + CR 608.2c: "You may put that card onto the battlefield" —
        // the controller routes the kept card after RevealUntil found a hit.
        // Accept → `accept_zone`; decline → `decline_zone`. On decline, when the
        // decline zone IS the rest pile, the hit card joins the misses so the
        // random-order placement covers it in one shuffle (CR 701.20a).
        (
            WaitingFor::RevealUntilKeptChoice {
                player,
                hit_card,
                accept_zone,
                decline_zone,
                enter_tapped,
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
            WaitingFor::CascadeChoice {
                player,
                hit_card,
                exiled_misses,
                source_mv,
            },
            GameAction::CascadeChoice { choice },
        ) => {
            let cast = matches!(choice, crate::types::actions::CastChoice::Cast);
            if cast {
                // CR 702.85a: Grant a cast-from-exile permission gated by
                // `CascadeResultingMvBelow`. The second MV check is enforced
                // at cast time in `casting_costs`, where X has been chosen.
                // Bottom-shuffle of misses is deferred to that point so a
                // cast-time rejection can still reach them.
                if let Some(obj) = state.objects.get_mut(&hit_card) {
                    obj.casting_permissions.push(
                        crate::types::ability::CastingPermission::ExileWithAltCost {
                            cost: crate::types::mana::ManaCost::zero(),
                            cast_transformed: false,
                            constraint: Some(
                                crate::types::ability::CastPermissionConstraint::CascadeResultingMvBelow {
                                    source_mv,
                                    exiled_misses,
                                },
                            ),
                            // CR 702.85a (Cascade): the cascading player is the
                            // only player permitted to cast the hit card.
                            granted_to: Some(player),
                        },
                    );
                }
            } else {
                // CR 702.85a: Caster declines — hit and misses all go to the
                // bottom of the library in a random order together.
                let mut all_to_bottom = exiled_misses;
                all_to_bottom.push(hit_card);
                crate::game::effects::cascade::shuffle_to_bottom(state, &all_to_bottom, events);
            }

            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
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

            if kept
                .iter()
                .enumerate()
                .any(|(index, card_id)| kept[index + 1..].contains(card_id))
            {
                return Err(EngineError::InvalidAction(
                    "Selected cards must be unique".to_string(),
                ));
            }

            if !selectable_cards.is_empty() {
                for card_id in &kept {
                    if !selectable_cards.contains(card_id) {
                        return Err(EngineError::InvalidAction(
                            "Selected card does not match filter".to_string(),
                        ));
                    }
                }
            }

            let unkept: Vec<_> = cards
                .iter()
                .filter(|id| !kept.contains(id))
                .copied()
                .collect();
            let kept_zone = kept_destination.unwrap_or(Zone::Hand);
            if kept_zone == Zone::Library {
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
            for &obj_id in &kept {
                zones::move_to_zone(state, obj_id, kept_zone, events);
            }
            // CR 701.33 + CR 701.18: Publish the kept (revealed) cards as a
            // tracked set so downstream sub_abilities can route them by type
            // via `TargetFilter::TrackedSetFiltered`. Used by Zimone's
            // Experiment — "Put all land cards revealed this way onto the
            // battlefield tapped and put all creature cards revealed this way
            // into your hand" consume this set. Safe to populate
            // unconditionally; unused tracked sets are harmless and resolved
            // by the latest-set-wins sentinel binding pass.
            if !kept.is_empty() {
                let tracked_id = crate::types::identifiers::TrackedSetId(state.next_tracked_set_id);
                state.next_tracked_set_id += 1;
                state.tracked_object_sets.insert(tracked_id, kept.clone());
            }
            // CR 701.20e: None => Graveyard; map to a concrete zone so the rest
            // mover (shared with the search-split partition) has a single Zone.
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
            if let Some(wf) = batch_or_drain_observer_triggers(
                state,
                events,
                events_before_effect,
                events_after_move,
            ) {
                return Ok(ResolutionChoiceOutcome::WaitingFor(wf));
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
                    if count_param > 0 {
                        effects::counters::add_counter_with_replacement(
                            state,
                            player,
                            blighted,
                            crate::types::counter::CounterType::Minus1Minus1,
                            count_param,
                            events,
                        );
                    }
                    // CR 701.68c: Snapshot the chosen creature so spells and
                    // abilities that refer back to "the creature you blighted"
                    // resolve to it. The creature stays on the battlefield, so
                    // the snapshot is taken from its live characteristics.
                    if let Some(obj) = state.objects.get(&blighted) {
                        let snapshot = crate::types::ability::CostPaidObjectSnapshot {
                            object_id: blighted,
                            lki: obj.snapshot_for_mana_spent(),
                        };
                        if let Some(cont) = state.pending_continuation.as_mut() {
                            cont.chain.set_effect_context_object_recursive(snapshot);
                        }
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
                if let Some(wf) = batch_or_drain_observer_triggers(
                    state,
                    events,
                    events_before_effect,
                    events_after_move,
                ) {
                    return Ok(ResolutionChoiceOutcome::WaitingFor(wf));
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
            state.layers_dirty = true;
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
                target_player,
                categories,
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

            // Validate each choice is eligible for its category and no duplicates.
            let mut chosen_this_round = Vec::new();
            for (i, choice) in choices.iter().enumerate() {
                if let Some(obj_id) = choice {
                    if !eligible_per_category[i].contains(obj_id) {
                        return Err(EngineError::InvalidAction(format!(
                            "Object {:?} is not eligible for category {:?}",
                            obj_id, categories[i]
                        )));
                    }
                    if chosen_this_round.contains(obj_id) {
                        return Err(EngineError::InvalidAction(format!(
                            "Object {:?} chosen for multiple categories",
                            obj_id
                        )));
                    }
                    chosen_this_round.push(*obj_id);
                }
            }

            // Accumulate kept permanents.
            all_kept.extend(chosen_this_round);

            // Determine chooser_scope from context: if player == target_player, it's EachPlayerSelf.
            // If player != target_player for a non-first player, it's ControllerForAll.
            let chooser_scope = if player == target_player {
                CategoryChooserScope::EachPlayerSelf
            } else {
                CategoryChooserScope::ControllerForAll
            };

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
                    source_id,
                    events,
                );
            } else if let Err(e) = effects::choose_and_sacrifice_rest::advance_to_next_player(
                state,
                &categories,
                chooser_scope,
                player, // controller for ControllerForAll
                source_id,
                &remaining_players,
                all_kept,
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
