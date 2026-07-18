use crate::game::effects::{resolve_ability_chain, search_library};
use crate::game::zone_pipeline::{self, BatchMoveResult, ZoneMoveRequest};
use crate::types::ability::{
    Effect, EffectError, EffectKind, PlayerFilter, ResolvedAbility, SubAbilityLink,
    TargetChoiceTiming, TargetFilter,
};
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::{
    ActiveSearchDecisionAuthority, BatchCompletion, GameState, LibrarySearchDeliveryResume,
    PendingScopedLibrarySearch, ScopedLibrarySearchPhase, WaitingFor,
};
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 101.4 + CR 701.23i: A scoped self-library search can use the simultaneous
/// protocol only when its immediate continuation is the ordinary parent-target
/// library-to-battlefield delivery. Other search shapes retain the established
/// sequential player-scope resolver rather than being coerced into a subtly
/// different timing model.
pub(crate) fn supports_simultaneous_delivery(ability: &ResolvedAbility) -> bool {
    let Effect::SearchLibrary {
        target_player,
        source_zones,
        ..
    } = &ability.effect
    else {
        return false;
    };
    if target_player.is_some() || source_zones.as_slice() != [Zone::Library] {
        return false;
    }
    let Some(delivery) = ability.sub_ability.as_deref() else {
        return false;
    };
    // The simultaneous protocol deliberately delivers selected cards through
    // typed `ZoneMoveRequest`s rather than `resolve_ability_chain(delivery)`.
    // It may therefore intercept only a semantically plain delivery child;
    // otherwise a child rider, condition, optional branch, target metadata, or
    // other resolution behavior would be silently discarded. Fail closed to
    // the established sequential player-scope resolver for every richer child.
    if !is_plain_parent_target_delivery(delivery) {
        return false;
    }
    matches!(
        &delivery.effect,
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Battlefield,
            target: TargetFilter::ParentTarget,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enters_attacking: false,
            up_to: false,
            enter_with_counters,
            conditional_enter_with_counters,
            face_down_profile: None,
            enters_modified_if: None,
            ..
        } if enter_with_counters.is_empty() && conditional_enter_with_counters.is_empty()
    )
}

/// The player-scope splitter normally peels the once-after-all-searches shuffle
/// off the delivery child so the batch completion can run it after every card
/// has entered. It also peels arbitrary delivery riders into the same tail
/// shape, but those riders must *not* silently opt into this protocol: their
/// timing was not selected by the scoped-search grammar. Inspect the original
/// chain before splitting and permit only the explicitly preserved
/// `searched-this-way` shuffle tail.
pub(crate) fn has_only_detachable_shuffle_tail(ability: &ResolvedAbility) -> bool {
    let Some(delivery) = ability.sub_ability.as_deref() else {
        return false;
    };
    let Some(tail) = delivery.sub_ability.as_deref() else {
        return true;
    };
    matches!(&tail.effect, Effect::Shuffle { .. })
        && matches!(
            &tail.player_scope,
            Some(PlayerFilter::PerformedActionThisWay { .. })
        )
}

/// CR 608.2c: The simultaneous scoped-search shortcut replaces normal child
/// resolution, so only a child whose complete behavior is represented by the
/// supported `Effect::ChangeZone` fields may enter it. Every checked field
/// would otherwise require `resolve_ability_chain` to preserve behavior.
fn is_plain_parent_target_delivery(delivery: &ResolvedAbility) -> bool {
    delivery.source_incarnation.is_none()
        && delivery.source_card_id.is_none()
        && delivery.targets.is_empty()
        && delivery.sub_ability.is_none()
        && delivery.else_ability.is_none()
        && delivery.duration.is_none()
        && delivery.condition.is_none()
        && !delivery.optional_targeting
        && !delivery.optional
        && delivery.optional_for.is_none()
        && delivery.multi_target.is_none()
        && delivery.target_constraints.is_empty()
        && matches!(delivery.target_choice_timing, TargetChoiceTiming::Stack)
        && delivery.target_selection_mode.is_chosen()
        && delivery.target_chooser.is_none()
        && delivery.description.is_none()
        && delivery.repeat_for.is_none()
        && delivery.min_x_value == 0
        && !delivery.cant_be_copied
        && !delivery.forward_result
        && delivery.unless_pay.is_none()
        && delivery.distribution.is_none()
        && delivery.player_scope.is_none()
        && delivery.starting_with.is_none()
        && delivery.repeat_until.is_none()
        && delivery.replacement_applied.is_empty()
        && delivery.sub_link == SubAbilityLink::ContinuationStep
        && delivery.modal.is_none()
        && delivery.mode_abilities.is_empty()
}

/// CR 101.4 + CR 701.23i: Start an APNAP series of private self-library search
/// choices. No selected card changes zones until every scoped player has made
/// their choice, including an optional-search decline/accept decision.
pub(crate) fn start(
    state: &mut GameState,
    ability: &ResolvedAbility,
    matching_players: &[PlayerId],
    after_scope: Option<Box<ResolvedAbility>>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if state.pending_scoped_library_search.is_some()
        || !state.active_library_searches.is_empty()
        || !state.active_search_decision_controls.is_empty()
    {
        return Err(EffectError::SearchAlreadyActive);
    }
    let acceptance_authorities = matching_players
        .iter()
        .copied()
        .map(|player| {
            let controller =
                crate::game::turn_control::authorized_submitter_for_player(state, player);
            (
                player,
                ActiveSearchDecisionAuthority::LatchedController { controller },
            )
        })
        .collect();
    let phase = if ability.optional {
        ScopedLibrarySearchPhase::CollectAcceptance {
            remaining_players: matching_players.to_vec(),
            accepted_players: Vec::new(),
            acceptance_authorities,
            current_player: None,
        }
    } else {
        ScopedLibrarySearchPhase::CollectAcceptance {
            remaining_players: Vec::new(),
            accepted_players: matching_players.to_vec(),
            acceptance_authorities,
            current_player: None,
        }
    };
    let pending = PendingScopedLibrarySearch {
        ability: Box::new(ability.clone()),
        phase,
        after_scope,
    };
    if ability.optional {
        state.pending_scoped_library_search = Some(pending);
        advance_acceptance(state, events)
    } else {
        let mut prepared_state = state.clone();
        prepared_state.pending_scoped_library_search = Some(pending);
        let mut prepared_events = Vec::new();
        prepare_scoped_group(&mut prepared_state, &mut prepared_events)?;
        *state = prepared_state;
        events.extend(prepared_events);
        Ok(())
    }
}

/// Returns `true` only when the current optional prompt belongs to this
/// protocol. The ordinary optional-effect handler must continue to own every
/// other `OptionalEffectChoice`.
pub(crate) fn handle_optional_decision(
    state: &mut GameState,
    accept: bool,
    events: &mut Vec<GameEvent>,
) -> Result<bool, EffectError> {
    let Some(pending) = state.pending_scoped_library_search.as_ref() else {
        return Ok(false);
    };
    let (player, source_id) = match &state.waiting_for {
        WaitingFor::OptionalEffectChoice {
            player, source_id, ..
        } => (*player, *source_id),
        _ => return Ok(false),
    };
    if pending.current_player() != Some(player) || pending.ability.source_id != source_id {
        return Ok(false);
    }

    let mut pending = state
        .pending_scoped_library_search
        .take()
        .expect("pending scoped search checked above");
    let ScopedLibrarySearchPhase::CollectAcceptance {
        accepted_players,
        current_player,
        ..
    } = &mut pending.phase
    else {
        return Ok(false);
    };
    if accept {
        accepted_players.push(player);
    }
    *current_player = None;
    state.pending_scoped_library_search = Some(pending);
    set_priority(state, player);
    advance_acceptance(state, events)?;
    Ok(true)
}

/// CR 701.23b + CR 701.23i: Validate and collect one private search selection. The normal
/// SearchChoice handler performs its zone change immediately; this branch only
/// records the answer and advances APNAP, leaving all delivery for the final
/// simultaneous batch.
#[allow(clippy::too_many_arguments)]
pub(crate) fn submit_selection(
    state: &mut GameState,
    player: PlayerId,
    library_owner: Option<PlayerId>,
    cards: &[ObjectId],
    count: usize,
    reveal: bool,
    up_to: bool,
    allows_partial_find: bool,
    constraint: &crate::types::ability::SearchSelectionConstraint,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<bool, EffectError> {
    let Some(pending) = state.pending_scoped_library_search.as_ref() else {
        return Ok(false);
    };
    if pending.current_player() != Some(player) {
        return Ok(false);
    }

    let lower_bounded = up_to || allows_partial_find || constraint.permits_partial_find();
    let valid_count = if lower_bounded {
        chosen.len() <= count
    } else {
        chosen.len() == count
    };
    if !valid_count {
        return Err(EffectError::InvalidParam(format!(
            "Must select {}{} card(s), got {}",
            if lower_bounded { "up to " } else { "exactly " },
            count,
            chosen.len()
        )));
    }
    for id in chosen {
        if !cards.contains(id) {
            return Err(EffectError::InvalidParam(
                "Selected card not in scoped search results".to_string(),
            ));
        }
    }
    let ScopedLibrarySearchPhase::CollectSelections {
        prepared_choices, ..
    } = &pending.phase
    else {
        return Ok(false);
    };
    let prepared = prepared_choices
        .iter()
        .find(|choice| choice.player == player)
        .ok_or_else(|| EffectError::InvalidParam("missing exact scoped candidates".to_string()))?;
    for id in chosen {
        let Some(exact) = prepared
            .candidates
            .iter()
            .find(|candidate| candidate.object_id == *id)
        else {
            return Err(EffectError::InvalidParam(
                "selected card was not in the exact scoped candidate set".to_string(),
            ));
        };
        if !prepared_candidate_is_live(state, prepared, exact) {
            return Err(EffectError::InvalidParam(
                "selected scoped search card is stale".to_string(),
            ));
        }
    }
    let mut distinct = chosen.to_vec();
    distinct.sort_unstable_by_key(|id| id.0);
    distinct.dedup();
    if distinct.len() != chosen.len() {
        return Err(EffectError::InvalidParam(
            "Cannot select the same search card more than once".to_string(),
        ));
    }
    if !search_library::selection_satisfies_constraint(state, chosen, constraint) {
        return Err(EffectError::InvalidParam(
            "Selected cards do not satisfy the search-selection constraint".to_string(),
        ));
    }

    let announced_selection: Vec<_> = chosen
        .iter()
        .filter_map(|id| state.objects.get(id))
        .map(ObjectIncarnationRef::from_object)
        .collect();
    let pending = state
        .pending_scoped_library_search
        .as_mut()
        .expect("pending scoped search checked above");
    let ScopedLibrarySearchPhase::CollectSelections {
        prepared_choices, ..
    } = &mut pending.phase
    else {
        return Err(EffectError::InvalidParam(
            "scoped selection resumed outside CollectSelections".to_string(),
        ));
    };
    let prepared = prepared_choices
        .iter_mut()
        .find(|choice| choice.player == player)
        .expect("validated scoped choice remains prepared");
    if prepared.announced_selection.is_some() {
        return Err(EffectError::InvalidParam(
            "scoped search selection was already announced".to_string(),
        ));
    }
    prepared.announced_selection = Some(announced_selection);

    let chosen = match crate::game::engine_resolution_choices::apply_search_found_replacements(
        state,
        player,
        library_owner,
        chosen,
        crate::types::game_state::PendingSearchFoundContinuation::Scoped,
        reveal,
        events,
    ) {
        Ok(survivors) => survivors,
        Err(_) => return Ok(true),
    };
    let chosen: Vec<_> = chosen
        .iter()
        .filter_map(|id| state.objects.get(id))
        .map(ObjectIncarnationRef::from_object)
        .collect();
    let mut pending = state
        .pending_scoped_library_search
        .take()
        .expect("pending scoped search checked above");
    let ScopedLibrarySearchPhase::CollectSelections {
        selections,
        current_player,
        pending_reveals,
        ..
    } = &mut pending.phase
    else {
        return Err(EffectError::InvalidParam(
            "scoped selection resumed outside CollectSelections".to_string(),
        ));
    };
    if reveal && !chosen.is_empty() {
        pending_reveals.push((player, chosen.clone()));
    }
    selections.push((player, chosen));
    *current_player = None;
    state.pending_scoped_library_search = Some(pending);
    set_priority(state, player);
    advance_selection(state, events)?;
    Ok(true)
}

/// CR 101.4: Collect simultaneous optional choices in APNAP order before the
/// accepted searches begin.
fn advance_acceptance(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let next = state
        .pending_scoped_library_search
        .as_ref()
        .and_then(|pending| match &pending.phase {
            ScopedLibrarySearchPhase::CollectAcceptance {
                remaining_players, ..
            } => remaining_players.first().copied(),
            _ => None,
        });
    let Some(player) = next else {
        return prepare_scoped_group(state, events);
    };

    let mut pending = state
        .pending_scoped_library_search
        .take()
        .expect("pending scoped search checked above");
    let ScopedLibrarySearchPhase::CollectAcceptance {
        remaining_players,
        current_player,
        ..
    } = &mut pending.phase
    else {
        return Err(EffectError::InvalidParam(
            "acceptance cursor advanced outside CollectAcceptance".to_string(),
        ));
    };
    remaining_players.remove(0);
    *current_player = Some(player);
    let source_id = pending.ability.source_id;
    let description = pending.ability.description.clone();
    state.pending_scoped_library_search = Some(pending);
    state.waiting_for = WaitingFor::OptionalEffectChoice {
        player,
        source_id,
        description,
        may_trigger_key: None,
    };
    Ok(())
}

/// CR 701.23a + CR 614.6: Record the cards whose original found events survived
/// replacement, then resume the exact scoped-search continuation.
pub(crate) fn complete_replaced_selection(
    state: &mut GameState,
    player: PlayerId,
    survivors: Vec<ObjectIncarnationRef>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let mut pending = state
        .pending_scoped_library_search
        .take()
        .ok_or_else(|| EffectError::InvalidParam("missing scoped search resume".to_string()))?;
    let ScopedLibrarySearchPhase::CollectSelections {
        prepared_choices,
        selections,
        current_player,
        pending_reveals,
        ..
    } = &mut pending.phase
    else {
        return Err(EffectError::InvalidParam(
            "scoped SearchFound resumed outside CollectSelections".to_string(),
        ));
    };
    let reveal = prepared_choices
        .iter()
        .find(|choice| choice.player == player)
        .is_some_and(|choice| choice.reveal);
    if reveal && !survivors.is_empty() {
        pending_reveals.push((player, survivors.clone()));
    }
    selections.push((player, survivors));
    *current_player = None;
    state.pending_scoped_library_search = Some(pending);
    set_priority(state, player);
    advance_selection(state, events)
}

/// CR 101.4 + CR 701.23i: after every optional answer is known, prepare every
/// accepted search against one cloned snapshot and commit the complete group
/// atomically before the first APNAP selection prompt.
fn prepare_scoped_group(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let pending = state
        .pending_scoped_library_search
        .as_ref()
        .expect("scoped preparation requires pending state");
    let ScopedLibrarySearchPhase::CollectAcceptance {
        accepted_players, ..
    } = &pending.phase
    else {
        return Err(EffectError::InvalidParam(
            "scoped preparation requires CollectAcceptance".to_string(),
        ));
    };
    let accepted = accepted_players.clone();
    let template = (*pending.ability).clone();
    let mut prepared_group = Vec::with_capacity(accepted.len());
    for player in accepted.iter().copied() {
        let mut scoped = template.clone();
        scoped.optional = false;
        scoped.set_original_controller_recursive(template.controller);
        scoped.set_controller_recursive(player);
        scoped.set_scoped_player_recursive(player);
        prepared_group.push((
            player,
            search_library::prepare_effective_search(state, &scoped)?,
        ));
    }

    let mut prepared_choices = Vec::new();
    let mut selections = Vec::new();
    for (player, prepared) in prepared_group.into_iter() {
        let Some(prepared) = prepared else {
            selections.push((player, Vec::new()));
            continue;
        };
        if prepared.effective_library_owner.is_some() {
            events.push(GameEvent::PlayerPerformedAction {
                player_id: player,
                action: PlayerActionKind::SearchedLibrary,
            });
            state.players_who_searched_library_this_turn.insert(player);
            state
                .player_actions_this_way
                .insert((player, PlayerActionKind::SearchedLibrary));
            state
                .player_actions_this_turn
                .push((player, PlayerActionKind::SearchedLibrary));
        }
        if let Some(search) = prepared.active_search {
            state.active_library_searches.insert(search);
        }
        if let Some(event) = prepared.hidden_event {
            events.push(event);
        }
        state
            .active_search_decision_controls
            .insert(prepared.decision);
        let auto_completed = prepared.candidates.is_empty();
        prepared_choices.push(
            crate::types::game_state::PreparedScopedLibrarySearchChoice {
                player,
                library_owner: prepared.effective_library_owner,
                candidates: prepared.candidates,
                offered_count: auto_completed.then_some(0),
                announced_selection: auto_completed.then(Vec::new),
                filter: prepared.filter,
                count: prepared.count,
                reveal: prepared.reveal,
                up_to: prepared.up_to,
                allows_partial_find: prepared.allows_partial_find,
                constraint: prepared.constraint,
            },
        );
        if auto_completed {
            selections.push((player, Vec::new()));
        }
    }
    state
        .pending_scoped_library_search
        .as_mut()
        .expect("scoped pending survives atomic commit")
        .phase = ScopedLibrarySearchPhase::CollectSelections {
        prepared_choices,
        next_selection_index: 0,
        current_player: None,
        selections,
        frozen_dispositions: Vec::new(),
        pending_reveals: Vec::new(),
    };
    advance_selection(state, events)
}

/// CR 101.4: Collect simultaneous search choices in APNAP order before the
/// selected cards move together.
fn advance_selection(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let choice = {
        let pending = state
            .pending_scoped_library_search
            .as_mut()
            .expect("selection advance requires pending scoped search");
        let ScopedLibrarySearchPhase::CollectSelections {
            prepared_choices,
            next_selection_index,
            selections,
            ..
        } = &mut pending.phase
        else {
            return Err(EffectError::InvalidParam(
                "selection advance requires CollectSelections".to_string(),
            ));
        };
        let mut next_choice = None;
        while let Some(choice) = prepared_choices.get(*next_selection_index).cloned() {
            *next_selection_index += 1;
            if !selections
                .iter()
                .any(|(completed, _)| *completed == choice.player)
            {
                next_choice = Some(choice);
                break;
            }
        }
        next_choice
    };
    let Some(choice) = choice else {
        return deliver_collected_cards(state, events);
    };
    let cards: Vec<ObjectId> = choice
        .candidates
        .iter()
        .filter(|identity| prepared_candidate_is_live(state, &choice, identity))
        .map(|identity| identity.object_id)
        .collect();
    let pending = state
        .pending_scoped_library_search
        .as_mut()
        .expect("selection advance retains pending scoped search");
    let ScopedLibrarySearchPhase::CollectSelections {
        prepared_choices,
        current_player,
        ..
    } = &mut pending.phase
    else {
        unreachable!("phase checked above")
    };
    prepared_choices
        .iter_mut()
        .find(|prepared| prepared.player == choice.player)
        .expect("advanced scoped choice remains prepared")
        .offered_count = Some(cards.len());
    *current_player = Some(choice.player);
    state.waiting_for = WaitingFor::SearchChoice {
        player: choice.player,
        library_owner: choice.library_owner,
        count: choice.count.min(cards.len()),
        cards,
        reveal: choice.reveal,
        up_to: choice.up_to,
        allows_partial_find: choice.allows_partial_find,
        constraint: choice.constraint,
        split: None,
    };
    state.priority_player = choice.player;
    Ok(())
}

/// CR 701.23a + CR 400.7: A prepared search candidate must still be a matching
/// card in the searched zone and the same object incarnation when chosen.
fn prepared_candidate_is_live(
    state: &GameState,
    choice: &crate::types::game_state::PreparedScopedLibrarySearchChoice,
    identity: &crate::types::identifiers::ObjectIncarnationRef,
) -> bool {
    let Some(object) = state.objects.get(&identity.object_id) else {
        return false;
    };
    if object.incarnation != identity.incarnation {
        return false;
    }
    let Some(pending) = state.pending_scoped_library_search.as_ref() else {
        return false;
    };
    let mut ability = (*pending.ability).clone();
    ability.set_controller_recursive(choice.player);
    ability.set_scoped_player_recursive(choice.player);
    let context = crate::game::filter::FilterContext::from_ability(&ability);
    crate::game::filter::matches_target_filter_in_owner_zone(
        state,
        identity.object_id,
        &choice.filter,
        &context,
    )
}

/// CR 701.23i + CR 608.2c: Deliver every selected card as one batch after all
/// choices are made. The immediate child is verified by
/// `supports_simultaneous_delivery`, so its ParentTarget delivery is expressed
/// by the selected object set rather than by a synthetic card-specific effect.
fn deliver_collected_cards(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let pending = state
        .pending_scoped_library_search
        .as_ref()
        .expect("completion requires pending scoped search")
        .clone();
    let change = pending
        .ability
        .sub_ability
        .as_ref()
        .expect("simultaneous scoped search requires a delivery child");
    let Effect::ChangeZone {
        destination,
        enter_tapped,
        ..
    } = change.effect
    else {
        return Err(EffectError::InvalidParam(
            "Scoped search delivery child must be ChangeZone".to_string(),
        ));
    };
    let ScopedLibrarySearchPhase::CollectSelections {
        selections,
        frozen_dispositions,
        pending_reveals,
        ..
    } = &pending.phase
    else {
        return Err(EffectError::InvalidParam(
            "scoped delivery requires CollectSelections".to_string(),
        ));
    };
    let mut normalized = Vec::<crate::types::game_state::FrozenScopedSearchFoundDisposition>::new();
    for frozen in frozen_dispositions {
        if let Some(existing) = normalized
            .iter()
            .find(|existing| existing.identity == frozen.identity)
        {
            if existing.disposition != frozen.disposition {
                return Err(EffectError::ConflictingScopedSearchDisposition);
            }
        } else {
            normalized.push(frozen.clone());
        }
    }
    let search_keys: Vec<PlayerId> = frozen_dispositions
        .iter()
        .map(|frozen| frozen.searcher)
        .chain(selections.iter().map(|(searcher, _)| *searcher))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut grants = Vec::new();
    let requests = normalized
        .into_iter()
        .filter_map(|frozen| {
            let object = state.objects.get(&frozen.identity.object_id)?;
            if object.incarnation != frozen.identity.incarnation {
                return None;
            }
            let (move_destination, source_id) = match frozen.disposition {
                crate::types::proposed_event::SearchFoundDisposition::Original => {
                    (destination, pending.ability.source_id)
                }
                crate::types::proposed_event::SearchFoundDisposition::Modified(disposition) => {
                    if let Some(grant) = disposition.grant {
                        grants.push((frozen.identity, grant));
                    }
                    (disposition.destination, disposition.source.object_id)
                }
            };
            let mut request =
                ZoneMoveRequest::effect(frozen.identity.object_id, move_destination, source_id);
            request.mods.enter_tapped = enter_tapped;
            Some(request)
        })
        .collect();
    let completion = BatchCompletion::LibrarySearchDeliverySettled {
        resume: LibrarySearchDeliveryResume::Scoped {
            player: pending.ability.controller,
            source_id: pending.ability.source_id,
            search_keys: search_keys.clone(),
            grants,
            after_scope: pending.after_scope,
        },
    };
    // CR 701.23e + CR 101.4: no earlier APNAP participant reveals their found
    // card while later participants are still choosing. Publish the accumulated
    // terminal survivors together at the shared-delivery boundary.
    state.last_revealed_ids.clear();
    for (searcher, identities) in pending_reveals {
        let card_ids: Vec<_> = identities
            .iter()
            .filter(|identity| {
                state
                    .objects
                    .get(&identity.object_id)
                    .is_some_and(|object| object.incarnation == identity.incarnation)
            })
            .map(|identity| identity.object_id)
            .collect();
        if !card_ids.is_empty() {
            state.last_revealed_ids.extend(card_ids.iter().copied());
            for card_id in &card_ids {
                state.revealed_cards.insert(*card_id);
            }
            events.push(GameEvent::CardsRevealed {
                player: *searcher,
                card_names: card_ids
                    .iter()
                    .filter_map(|id| state.objects.get(id).map(|object| object.name.clone()))
                    .collect(),
                card_ids,
            });
        }
    }
    state
        .pending_scoped_library_search
        .as_mut()
        .expect("scoped pending survives until delivery completion")
        .phase = ScopedLibrarySearchPhase::Delivering {
        search_keys: search_keys.clone(),
    };
    match zone_pipeline::move_objects_simultaneously_then(state, requests, Some(completion), events)
    {
        // `move_objects_simultaneously_then` owns completion for both its
        // synchronous and replacement-paused paths. Calling `finish_delivery`
        // here as well would run the searched-this-way shuffle tail twice.
        BatchMoveResult::Done | BatchMoveResult::NeedsChoice => Ok(()),
    }
}

pub(crate) fn finish_delivery_tail(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    after_scope: Option<Box<ResolvedAbility>>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChangeZone,
        source_id,
        subject: None,
    });
    set_priority(state, player);
    if let Some(after_scope) = after_scope {
        resolve_ability_chain(state, &after_scope, events, 1)?;
    }
    Ok(())
}

fn set_priority(state: &mut GameState, player: PlayerId) {
    state.waiting_for = WaitingFor::Priority { player };
    state.priority_player = player;
}

/// CR 800.4a + CR 101.4: continue the typed APNAP cursor after elimination
/// removed the participant who owned its current prompt. All participant and
/// exact-object pruning happens in `elimination` before this entry point.
pub(crate) fn resume_after_elimination(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Some(pending) = state.pending_scoped_library_search.as_ref() else {
        return Ok(());
    };
    if pending.current_player().is_some() {
        return Ok(());
    }
    match pending.phase {
        ScopedLibrarySearchPhase::CollectAcceptance { .. } => advance_acceptance(state, events),
        ScopedLibrarySearchPhase::CollectSelections { .. } => advance_selection(state, events),
        ScopedLibrarySearchPhase::Delivering { .. } => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine::apply_as_current;
    use crate::game::zones::create_object;
    use crate::types::ability::{PlayerFilter, QuantityExpr, SearchSelectionConstraint};
    use crate::types::actions::GameAction;
    use crate::types::game_state::GameState;
    use crate::types::identifiers::CardId;

    fn three_player_scoped_search(reveal: bool) -> (GameState, ResolvedAbility, Vec<ObjectId>) {
        let mut state = GameState::new(crate::types::format::FormatConfig::free_for_all(), 3, 42);
        let source = create_object(
            &mut state,
            CardId(60),
            PlayerId(2),
            "Three-player scoped source".to_string(),
            Zone::Battlefield,
        );
        let cards = (0..3)
            .map(|index| {
                create_object(
                    &mut state,
                    CardId(61 + index as u64),
                    PlayerId(index),
                    format!("P{index} library card"),
                    Zone::Library,
                )
            })
            .collect();
        let delivery = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
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
            PlayerId(2),
        );
        let search = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                reveal,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![Zone::Library],
            },
            Vec::new(),
            source,
            PlayerId(2),
        )
        .sub_ability(delivery);
        (state, search, cards)
    }

    /// A child rider cannot be represented by the typed batch move request.
    /// The scoped protocol must reject it and leave the ordinary resolver to
    /// execute both the delivery and its rider.
    #[test]
    fn delivery_rider_fails_closed_to_the_sequential_scope_resolver() {
        let player = PlayerId(0);
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            player,
            "Scoped search source".to_string(),
            Zone::Battlefield,
        );
        let land = create_object(
            &mut state,
            CardId(2),
            player,
            "Library land".to_string(),
            Zone::Library,
        );

        let rider = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
            vec![],
            source,
            player,
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
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![],
            source,
            player,
        )
        .sub_ability(rider);
        let mut ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![Zone::Library],
            },
            vec![],
            source,
            player,
        )
        .sub_ability(delivery);
        ability.player_scope = Some(PlayerFilter::Controller);

        assert!(
            !supports_simultaneous_delivery(&ability),
            "a delivery child with a rider must not enter the batch shortcut"
        );

        let starting_life = state.players[player.0 as usize].life;
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0)
            .expect("the established scoped resolver must accept the richer chain");
        assert!(matches!(state.waiting_for, WaitingFor::SearchChoice { .. }));
        assert!(
            state.pending_scoped_library_search.is_none(),
            "the rejected shape must not leave a simultaneous-search pending state"
        );

        apply_as_current(&mut state, GameAction::SelectCards { cards: vec![land] })
            .expect("the ordinary search choice must deliver the selected card");
        assert_eq!(state.objects[&land].zone, Zone::Battlefield);
        assert_eq!(
            state.players[player.0 as usize].life,
            starting_life + 3,
            "the delivery rider must run through the fallback continuation"
        );
    }

    #[test]
    fn prepares_all_searchers_before_first_choice_and_moves_only_after_final_choice() {
        let mut state = GameState::new(crate::types::format::FormatConfig::free_for_all(), 3, 42);
        let source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Scoped source".to_string(),
            Zone::Battlefield,
        );
        let selected: Vec<_> = (0..3)
            .map(|index| {
                create_object(
                    &mut state,
                    CardId(20 + index as u64),
                    PlayerId(index as u8),
                    format!("Search card {index}"),
                    Zone::Library,
                )
            })
            .collect();
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
            source,
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
                source_zones: vec![Zone::Library],
            },
            Vec::new(),
            source,
            PlayerId(0),
        )
        .sub_ability(delivery);
        let mut events = Vec::new();

        start(
            &mut state,
            &search,
            &[PlayerId(0), PlayerId(1), PlayerId(2)],
            None,
            &mut events,
        )
        .unwrap();

        assert_eq!(state.active_library_searches.iter().count(), 3);
        assert_eq!(state.active_search_decision_controls.iter().count(), 3);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::HiddenSearchViewed { .. }))
                .count(),
            3
        );
        assert!(matches!(
            state.waiting_for,
            WaitingFor::SearchChoice {
                player: PlayerId(0),
                ..
            }
        ));

        for (index, object_id) in selected.iter().copied().enumerate() {
            apply_as_current(
                &mut state,
                GameAction::SelectCards {
                    cards: vec![object_id],
                },
            )
            .unwrap();
            if index < selected.len() - 1 {
                assert!(selected
                    .iter()
                    .all(|id| state.objects[id].zone == Zone::Library));
            }
        }
        assert!(selected
            .iter()
            .all(|id| state.objects[id].zone == Zone::Battlefield));
        assert!(state.active_library_searches.is_empty());
        assert!(state.active_search_decision_controls.is_empty());
    }

    #[test]
    fn reveal_waits_for_every_apnap_selection_and_shared_delivery() {
        let (mut state, search, cards) = three_player_scoped_search(true);
        let mut start_events = Vec::new();
        start(
            &mut state,
            &search,
            &[PlayerId(0), PlayerId(1), PlayerId(2)],
            None,
            &mut start_events,
        )
        .unwrap();

        for (index, card) in cards.iter().copied().enumerate() {
            let result =
                apply_as_current(&mut state, GameAction::SelectCards { cards: vec![card] })
                    .unwrap();
            let revealed: Vec<_> = result
                .events
                .iter()
                .filter_map(|event| match event {
                    GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
                    _ => None,
                })
                .collect();
            if index < cards.len() - 1 {
                assert!(revealed.is_empty());
                assert!(cards
                    .iter()
                    .all(|object_id| state.objects[object_id].zone == Zone::Library));
            } else {
                assert_eq!(revealed.len(), cards.len());
                assert!(cards.iter().all(|object_id| {
                    revealed
                        .iter()
                        .any(|card_ids| card_ids == &vec![*object_id])
                }));
            }
        }
        assert!(cards
            .iter()
            .all(|object_id| state.objects[object_id].zone == Zone::Hand));
        assert_eq!(state.last_revealed_ids, cards);
    }

    #[test]
    fn successive_scoped_reveal_batches_replace_last_revealed_context() {
        use crate::types::game_state::FrozenScopedSearchFoundDisposition;
        use crate::types::identifiers::ObjectIncarnationRef;
        use crate::types::proposed_event::SearchFoundDisposition;

        let (mut state, search, first_cards) = three_player_scoped_search(true);
        let pending_for = |state: &GameState, ability: ResolvedAbility, cards: &[ObjectId]| {
            let selected: Vec<_> = cards
                .iter()
                .enumerate()
                .map(|(index, card)| {
                    (
                        PlayerId(index as u8),
                        ObjectIncarnationRef::from_object(&state.objects[card]),
                    )
                })
                .collect();
            PendingScopedLibrarySearch {
                ability: Box::new(ability),
                phase: ScopedLibrarySearchPhase::CollectSelections {
                    prepared_choices: Vec::new(),
                    next_selection_index: 0,
                    current_player: None,
                    selections: selected
                        .iter()
                        .map(|(player, identity)| (*player, vec![*identity]))
                        .collect(),
                    frozen_dispositions: selected
                        .iter()
                        .map(|(searcher, identity)| FrozenScopedSearchFoundDisposition {
                            searcher: *searcher,
                            identity: *identity,
                            disposition: SearchFoundDisposition::Original,
                        })
                        .collect(),
                    pending_reveals: selected
                        .iter()
                        .map(|(player, identity)| (*player, vec![*identity]))
                        .collect(),
                },
                after_scope: None,
            }
        };

        state.pending_scoped_library_search =
            Some(pending_for(&state, search.clone(), &first_cards));
        deliver_collected_cards(&mut state, &mut Vec::new()).unwrap();
        assert_eq!(state.last_revealed_ids, first_cards);

        let second_cards: Vec<_> = (0..3)
            .map(|index| {
                create_object(
                    &mut state,
                    CardId(70 + index as u64),
                    PlayerId(index),
                    format!("Second P{index} library card"),
                    Zone::Library,
                )
            })
            .collect();
        state.pending_scoped_library_search = Some(pending_for(&state, search, &second_cards));
        deliver_collected_cards(&mut state, &mut Vec::new()).unwrap();

        assert_eq!(state.last_revealed_ids, second_cards);
        assert!(first_cards
            .iter()
            .all(|card| !state.last_revealed_ids.contains(card)));
    }

    #[test]
    fn eliminating_current_selection_player_advances_without_skipping() {
        let (mut state, search, _cards) = three_player_scoped_search(false);
        let mut events = Vec::new();
        start(
            &mut state,
            &search,
            &[PlayerId(0), PlayerId(1), PlayerId(2)],
            None,
            &mut events,
        )
        .unwrap();

        crate::game::elimination::eliminate_player(&mut state, PlayerId(0), &mut events);

        assert!(matches!(
            state.waiting_for,
            WaitingFor::SearchChoice {
                player: PlayerId(1),
                ..
            }
        ));
    }

    #[test]
    fn eliminating_earlier_selection_player_preserves_current_cursor() {
        let (mut state, search, cards) = three_player_scoped_search(false);
        let mut events = Vec::new();
        start(
            &mut state,
            &search,
            &[PlayerId(0), PlayerId(1), PlayerId(2)],
            None,
            &mut events,
        )
        .unwrap();
        apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![cards[0]],
            },
        )
        .unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::SearchChoice {
                player: PlayerId(1),
                ..
            }
        ));

        crate::game::elimination::eliminate_player(&mut state, PlayerId(0), &mut events);

        assert!(matches!(
            state.waiting_for,
            WaitingFor::SearchChoice {
                player: PlayerId(1),
                ..
            }
        ));
        apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![cards[1]],
            },
        )
        .unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::SearchChoice {
                player: PlayerId(2),
                ..
            }
        ));
    }

    #[test]
    fn scoped_phase_deserialization_rejects_duplicate_delivery_keys() {
        let (_state, ability, _cards) = three_player_scoped_search(false);
        let pending = PendingScopedLibrarySearch {
            ability: Box::new(ability),
            phase: ScopedLibrarySearchPhase::Delivering {
                search_keys: vec![PlayerId(0)],
            },
            after_scope: None,
        };
        let mut wire = serde_json::to_value(pending).unwrap();
        wire["phase"]["search_keys"] = serde_json::json!([0, 0]);

        assert!(serde_json::from_value::<PendingScopedLibrarySearch>(wire).is_err());
    }

    #[test]
    fn optional_group_collects_every_answer_before_any_hidden_look() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Optional scoped source".to_string(),
            Zone::Battlefield,
        );
        create_object(
            &mut state,
            CardId(31),
            PlayerId(0),
            "P0 card".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(32),
            PlayerId(1),
            "P1 card".to_string(),
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
            source,
            PlayerId(0),
        );
        let mut search = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![Zone::Library],
            },
            Vec::new(),
            source,
            PlayerId(0),
        )
        .sub_ability(delivery);
        search.optional = true;
        let mut events = Vec::new();
        start(
            &mut state,
            &search,
            &[PlayerId(0), PlayerId(1)],
            None,
            &mut events,
        )
        .unwrap();
        assert!(state.active_library_searches.is_empty());
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::HiddenSearchViewed { .. })));

        apply_as_current(
            &mut state,
            GameAction::DecideOptionalEffect { accept: true },
        )
        .unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice {
                player: PlayerId(1),
                ..
            }
        ));
        assert!(state.active_library_searches.is_empty());
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::HiddenSearchViewed { .. })));

        apply_as_current(
            &mut state,
            GameAction::DecideOptionalEffect { accept: false },
        )
        .unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::SearchChoice {
                player: PlayerId(0),
                ..
            }
        ));
        assert!(state.active_library_searches.get(&PlayerId(0)).is_some());
        assert!(state.active_library_searches.get(&PlayerId(1)).is_none());
    }

    #[test]
    fn conflicting_shared_object_dispositions_fail_without_mutation() {
        use crate::types::game_state::FrozenScopedSearchFoundDisposition;
        use crate::types::identifiers::ObjectIncarnationRef;
        use crate::types::proposed_event::{BoundSearchFoundDisposition, SearchFoundDisposition};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(40),
            PlayerId(0),
            "Scoped source".to_string(),
            Zone::Battlefield,
        );
        let shared = create_object(
            &mut state,
            CardId(41),
            PlayerId(0),
            "Shared found card".to_string(),
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
            source,
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
                source_zones: vec![Zone::Library],
            },
            Vec::new(),
            source,
            PlayerId(0),
        )
        .sub_ability(delivery);
        let identity = ObjectIncarnationRef::from_object(&state.objects[&shared]);
        state.pending_scoped_library_search = Some(PendingScopedLibrarySearch {
            ability: Box::new(search),
            phase: ScopedLibrarySearchPhase::CollectSelections {
                prepared_choices: Vec::new(),
                next_selection_index: 0,
                current_player: None,
                selections: vec![(PlayerId(0), vec![identity]), (PlayerId(1), vec![identity])],
                frozen_dispositions: vec![
                    FrozenScopedSearchFoundDisposition {
                        searcher: PlayerId(0),
                        identity,
                        disposition: SearchFoundDisposition::Original,
                    },
                    FrozenScopedSearchFoundDisposition {
                        searcher: PlayerId(1),
                        identity,
                        disposition: SearchFoundDisposition::Modified(
                            BoundSearchFoundDisposition {
                                destination: Zone::Hand,
                                source: ObjectIncarnationRef::from_object(&state.objects[&source]),
                                grant: None,
                            },
                        ),
                    },
                ],
                pending_reveals: Vec::new(),
            },
            after_scope: None,
        });
        let before = state.clone();
        let mut events = Vec::new();

        assert_eq!(
            deliver_collected_cards(&mut state, &mut events),
            Err(EffectError::ConflictingScopedSearchDisposition)
        );
        assert_eq!(state, before);
        assert!(events.is_empty());
        assert_eq!(state.objects[&shared].zone, Zone::Library);
    }

    #[test]
    fn heterogeneous_scoped_dispositions_park_as_one_exact_simultaneous_batch() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, ReplacementDefinition, ReplacementMode,
        };
        use crate::types::game_state::FrozenScopedSearchFoundDisposition;
        use crate::types::identifiers::ObjectIncarnationRef;
        use crate::types::proposed_event::{BoundSearchFoundDisposition, SearchFoundDisposition};
        use crate::types::replacements::ReplacementEvent;

        let (mut state, search, cards) = three_player_scoped_search(false);
        let source = ObjectIncarnationRef::from_object(&state.objects[&search.source_id]);
        let first = ObjectIncarnationRef::from_object(&state.objects[&cards[0]]);
        let second = ObjectIncarnationRef::from_object(&state.objects[&cards[1]]);
        let redirect_source = create_object(
            &mut state,
            CardId(99),
            PlayerId(2),
            "Optional hand redirect".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&redirect_source)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .mode(ReplacementMode::Optional { decline: None })
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChangeZone {
                            origin: None,
                            destination: Zone::Graveyard,
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
                    .destination_zone(Zone::Hand),
            );
        state.pending_scoped_library_search = Some(PendingScopedLibrarySearch {
            ability: Box::new(search),
            phase: ScopedLibrarySearchPhase::CollectSelections {
                prepared_choices: Vec::new(),
                next_selection_index: 0,
                current_player: None,
                selections: vec![(PlayerId(0), vec![first]), (PlayerId(1), vec![second])],
                frozen_dispositions: vec![
                    FrozenScopedSearchFoundDisposition {
                        searcher: PlayerId(0),
                        identity: first,
                        disposition: SearchFoundDisposition::Original,
                    },
                    FrozenScopedSearchFoundDisposition {
                        searcher: PlayerId(1),
                        identity: second,
                        disposition: SearchFoundDisposition::Modified(
                            BoundSearchFoundDisposition {
                                destination: Zone::Exile,
                                source,
                                grant: None,
                            },
                        ),
                    },
                ],
                pending_reveals: Vec::new(),
            },
            after_scope: None,
        });

        let mut initial_events = Vec::new();
        deliver_collected_cards(&mut state, &mut initial_events).unwrap();

        assert!(
            initial_events.is_empty(),
            "partial batch events stay deferred"
        );
        let parked = state
            .pending_batch_deliveries
            .as_ref()
            .expect("first heterogeneous member must park");
        assert_eq!(parked.attempted, vec![cards[0], cards[1]]);
        assert_eq!(parked.requests.len(), 1);
        assert_eq!(parked.requests[0].object_id, cards[1]);
        assert_eq!(parked.requests[0].destination, Zone::Exile);
        assert!(matches!(
            parked.requests[0].cause,
            crate::types::game_state::PendingBatchZoneChangeCause::Effect { source: saved }
                if saved == source.object_id
        ));

        let result = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 1 })
            .expect("decline the first member's optional redirect");

        assert_eq!(state.objects[&cards[0]].zone, Zone::Hand);
        assert_eq!(state.objects[&cards[1]].zone, Zone::Exile);
        let moved: Vec<_> = result
            .events
            .iter()
            .filter_map(|event| match event {
                GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Library),
                    record,
                    ..
                } if [cards[0], cards[1]].contains(object_id) => Some((*object_id, record.to_zone)),
                _ => None,
            })
            .collect();
        assert_eq!(
            moved.len(),
            2,
            "the resumed action exposes both moves together"
        );
        assert!(state.pending_scoped_library_search.is_none());
        assert!(state.pending_batch_deliveries.is_none());
    }

    #[test]
    fn elimination_advances_the_current_acceptance_cursor() {
        let mut state = GameState::new(crate::types::format::FormatConfig::free_for_all(), 3, 42);
        let source = create_object(
            &mut state,
            CardId(50),
            PlayerId(2),
            "Surviving scoped source".to_string(),
            Zone::Battlefield,
        );
        let delivery = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
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
            PlayerId(2),
        );
        let mut search = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![Zone::Library],
            },
            Vec::new(),
            source,
            PlayerId(2),
        )
        .sub_ability(delivery);
        search.optional = true;
        let mut events = Vec::new();
        start(
            &mut state,
            &search,
            &[PlayerId(0), PlayerId(1), PlayerId(2)],
            None,
            &mut events,
        )
        .unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice {
                player: PlayerId(0),
                ..
            }
        ));

        crate::game::elimination::eliminate_player(&mut state, PlayerId(0), &mut events);

        assert!(matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice {
                player: PlayerId(1),
                ..
            }
        ));
        let pending = state.pending_scoped_library_search.as_ref().unwrap();
        let ScopedLibrarySearchPhase::CollectAcceptance {
            remaining_players,
            acceptance_authorities,
            ..
        } = &pending.phase
        else {
            panic!("expected CollectAcceptance")
        };
        assert!(!remaining_players.contains(&PlayerId(0)));
        assert!(!acceptance_authorities
            .iter()
            .any(|(player, _)| *player == PlayerId(0)));
    }
}
