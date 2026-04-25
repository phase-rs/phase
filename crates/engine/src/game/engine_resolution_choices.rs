use crate::types::ability::{
    CategoryChooserScope, ChoiceType, ChoiceValue, ChosenAttribute, EffectKind, ResolvedAbility,
    TargetRef,
};
use crate::types::actions::{GameAction, LearnOption};
use crate::types::events::GameEvent;
use crate::types::game_state::{ActionResult, GameState, PendingContinuation, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

use super::casting_costs;
use super::effects;
use super::engine::EngineError;
use super::turns;
use super::zones;

pub(super) enum ResolutionChoiceOutcome {
    WaitingFor(WaitingFor),
    ActionResult(ActionResult),
}

pub(super) fn handles(waiting_for: &WaitingFor) -> bool {
    matches!(
        waiting_for,
        WaitingFor::ScryChoice { .. }
            | WaitingFor::ManifestDreadChoice { .. }
            | WaitingFor::DiscoverChoice { .. }
            | WaitingFor::CascadeChoice { .. }
            | WaitingFor::LearnChoice { .. }
            | WaitingFor::TopOrBottomChoice { .. }
            | WaitingFor::PopulateChoice { .. }
            | WaitingFor::ClashCardPlacement { .. }
            | WaitingFor::VoteChoice { .. }
            | WaitingFor::DigChoice { .. }
            | WaitingFor::SurveilChoice { .. }
            | WaitingFor::RevealChoice { .. }
            | WaitingFor::SearchChoice { .. }
            | WaitingFor::ChooseFromZoneChoice { .. }
            | WaitingFor::DiscardToHandSize { .. }
            | WaitingFor::ConniveDiscard { .. }
            | WaitingFor::DiscardChoice { .. }
            | WaitingFor::EffectZoneChoice { .. }
            | WaitingFor::NamedChoice { .. }
            | WaitingFor::ChooseRingBearer { .. }
            | WaitingFor::ChooseDungeon { .. }
            | WaitingFor::ChooseDungeonRoom { .. }
            | WaitingFor::ChooseLegend { .. }
            | WaitingFor::BattleProtectorChoice { .. }
            | WaitingFor::CategoryChoice { .. }
            | WaitingFor::PayAmountChoice { .. }
    )
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
                crate::types::game_state::PayableResource::Energy => {
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
            }
            // CR 603.7c: Bind the paid amount for downstream chain steps that
            // read `QuantityRef::EventContextAmount` (e.g. "deals that much
            // damage"). `last_effect_count` is the documented fallback slot.
            state.last_effect_count = Some(amount as i32);
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
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
                per_choice_effect,
                controller,
                source_id,
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
            events.push(GameEvent::VoteCast {
                voter: player,
                choice: lower,
                source_id,
            });

            if remaining_votes > 1 {
                // CR 701.38d: Same player still has votes to cast.
                state.waiting_for = WaitingFor::VoteChoice {
                    player,
                    remaining_votes: remaining_votes - 1,
                    options,
                    option_labels,
                    remaining_voters,
                    tallies: new_tallies,
                    per_choice_effect,
                    controller,
                    source_id,
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
                    per_choice_effect,
                    controller,
                    source_id,
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
                    events,
                );
                ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(
                    state, controller, events,
                ))
            }
        }
        (
            WaitingFor::DigChoice {
                player,
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
            match rest_destination {
                Some(Zone::Library) => {
                    for &obj_id in &unkept {
                        zones::move_to_library_position(state, obj_id, false, events);
                    }
                }
                Some(zone) => {
                    for &obj_id in &unkept {
                        zones::move_to_zone(state, obj_id, zone, events);
                    }
                }
                None => {
                    for &obj_id in &unkept {
                        zones::move_to_zone(state, obj_id, Zone::Graveyard, events);
                    }
                }
            }
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
        }
        (
            WaitingFor::SurveilChoice { player, cards },
            GameAction::SelectCards {
                cards: to_graveyard,
            },
        ) => {
            for &obj_id in &to_graveyard {
                if cards.contains(&obj_id) {
                    zones::move_to_zone(state, obj_id, Zone::Graveyard, events);
                }
            }
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
        }
        (
            WaitingFor::RevealChoice {
                player,
                cards,
                filter,
                optional,
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
                effects::drain_pending_continuation(state, events);
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
            if optional {
                state.pending_continuation = None;
            } else if let Some(cont) = state.pending_continuation.as_mut() {
                cont.chain.targets = vec![TargetRef::Object(chosen_id)];
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

            set_priority(state, player);
            if let Some(cont) = state.pending_continuation.as_mut() {
                cont.chain.targets = chosen.iter().map(|&id| TargetRef::Object(id)).collect();
                // CR 701.23a + CR 701.24a: When the searcher is not the caster
                // (e.g., "its controller may search their library, ..., then
                // shuffle" for Assassin's Trophy), propagate the searcher's
                // PlayerId into the continuation chain's targets so downstream
                // untargeted-Shuffle / Library-owner-sensitive effects pick up
                // the correct player via `ability.target_player()`.
                if player != cont.chain.controller {
                    cont.chain.targets.push(TargetRef::Player(player));
                }
            }
            effects::drain_pending_continuation(state, events);
            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
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
                if let Some(ref mut next_sub) = cont.chain.sub_ability {
                    next_sub.targets = unchosen.iter().map(|&id| TargetRef::Object(id)).collect();
                }
            }
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

            for &card_id in &chosen {
                if let effects::discard::DiscardOutcome::NeedsReplacementChoice(choice_player) =
                    effects::discard::discard_as_cost(state, card_id, player, events)
                {
                    state.waiting_for =
                        super::replacement::replacement_choice_waiting_for(choice_player, state);
                    return Ok(action_result_outcome(events, state.waiting_for.clone()));
                }
            }

            state.last_effect_count = Some(chosen.len() as i32);
            events.push(GameEvent::EffectResolved {
                kind: effect_kind,
                source_id,
            });
            ResolutionChoiceOutcome::WaitingFor(finish_with_continuation(state, player, events))
        }
        (
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                up_to,
                source_id,
                effect_kind,
                zone,
                destination,
                enter_tapped,
                enter_transformed,
                under_your_control,
                enters_attacking,
                owner_library: _,
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            if up_to {
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
                EffectKind::ChangeZone => {
                    let dest_zone = destination.ok_or_else(|| {
                        EngineError::InvalidAction(
                            "EffectZoneChoice missing destination for ChangeZone".to_string(),
                        )
                    })?;
                    for &card_id in &chosen {
                        let controller_override = if under_your_control {
                            Some(player)
                        } else {
                            None
                        };
                        match effects::change_zone::execute_zone_move(
                            state,
                            card_id,
                            zone,
                            dest_zone,
                            source_id,
                            None,
                            enter_transformed,
                            enter_tapped,
                            controller_override,
                            &[],
                            events,
                        ) {
                            effects::change_zone::ZoneMoveResult::Done => {
                                if enters_attacking && dest_zone == Zone::Battlefield {
                                    let controller = state
                                        .objects
                                        .get(&card_id)
                                        .map(|obj| obj.controller)
                                        .unwrap_or(player);
                                    super::combat::enter_attacking(
                                        state, card_id, source_id, controller,
                                    );
                                }
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
                other => {
                    return Err(EngineError::InvalidAction(format!(
                        "EffectZoneChoice unsupported for {other:?}"
                    )));
                }
            }

            state.last_effect_count = Some(chosen.len() as i32);
            events.push(GameEvent::EffectResolved {
                kind: effect_kind,
                source_id,
            });
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
            WaitingFor::ChooseRingBearer { player, candidates },
            GameAction::ChooseRingBearer { target },
        ) => {
            if !candidates.contains(&target) {
                return Err(EngineError::InvalidAction(
                    "Invalid ring-bearer choice".to_string(),
                ));
            }
            state.ring_bearer.insert(player, Some(target));
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

            // Advance to next player or sacrifice.
            if remaining_players.is_empty() {
                // All players have chosen — sacrifice everything not kept.
                effects::choose_and_sacrifice_rest::sacrifice_unchosen_from_handler(
                    state, &all_kept, source_id, events,
                );
                set_priority(state, player);
                resume_with_error_propagation(state, events)?;
                ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
            } else {
                match effects::choose_and_sacrifice_rest::advance_to_next_player(
                    state,
                    &categories,
                    chooser_scope,
                    player, // controller for ControllerForAll
                    source_id,
                    &remaining_players,
                    all_kept,
                    events,
                ) {
                    Ok(()) => {
                        // If advance set a new WaitingFor, return it; otherwise priority.
                        if matches!(state.waiting_for, WaitingFor::CategoryChoice { .. }) {
                            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
                        } else {
                            set_priority(state, player);
                            resume_with_error_propagation(state, events)?;
                            ResolutionChoiceOutcome::WaitingFor(state.waiting_for.clone())
                        }
                    }
                    Err(e) => {
                        return Err(EngineError::InvalidAction(format!("{:?}", e)));
                    }
                }
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
