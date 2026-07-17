use crate::game::effects::{resolve_ability_chain, search_library};
use crate::game::zone_pipeline::{self, BatchMoveResult, ZoneMoveRequest};
use crate::types::ability::{
    Effect, EffectError, EffectKind, PlayerFilter, ResolvedAbility, SubAbilityLink,
    TargetChoiceTiming, TargetFilter,
};
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::{
    BatchCompletion, GameState, PendingScopedLibrarySearch, WaitingFor,
};
use crate::types::identifiers::ObjectId;
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
    state.pending_scoped_library_search = Some(PendingScopedLibrarySearch {
        ability: Box::new(ability.clone()),
        remaining_players: matching_players.to_vec(),
        selections: Vec::new(),
        current_player: None,
        after_scope,
    });
    advance_to_next_player(state, events)
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
    if pending.current_player != Some(player) || pending.ability.source_id != source_id {
        return Ok(false);
    }

    if accept {
        begin_accepted_search(state, events)?;
    } else {
        let mut pending = state
            .pending_scoped_library_search
            .take()
            .expect("pending scoped search checked above");
        pending.current_player = None;
        state.pending_scoped_library_search = Some(pending);
        set_priority(state, player);
        advance_to_next_player(state, events)?;
    }
    Ok(true)
}

/// CR 701.23b/i: Validate and collect one private search selection. The normal
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
    if pending.current_player != Some(player) {
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
    let mut pending = state
        .pending_scoped_library_search
        .take()
        .expect("pending scoped search checked above");
    pending.selections.push((player, chosen));
    pending.current_player = None;
    state.pending_scoped_library_search = Some(pending);
    set_priority(state, player);
    advance_to_next_player(state, events)?;
    Ok(true)
}

fn advance_to_next_player(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let next = state
        .pending_scoped_library_search
        .as_ref()
        .and_then(|pending| pending.remaining_players.first().copied());
    let Some(player) = next else {
        return deliver_collected_cards(state, events);
    };

    let mut pending = state
        .pending_scoped_library_search
        .take()
        .expect("pending scoped search checked above");
    pending.remaining_players.remove(0);
    pending.current_player = Some(player);
    let optional = pending.ability.optional;
    let source_id = pending.ability.source_id;
    let description = pending.ability.description.clone();
    state.pending_scoped_library_search = Some(pending);

    if optional {
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player,
            source_id,
            description,
            may_trigger_key: None,
        };
        return Ok(());
    }
    begin_accepted_search(state, events)
}

/// CR 701.23a + CR 614.6: Record the cards whose original found events survived
/// replacement, then resume the exact scoped-search continuation.
pub(crate) fn complete_replaced_selection(
    state: &mut GameState,
    player: PlayerId,
    survivors: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let mut pending = state
        .pending_scoped_library_search
        .take()
        .ok_or_else(|| EffectError::InvalidParam("missing scoped search resume".to_string()))?;
    pending.selections.push((player, survivors));
    pending.current_player = None;
    state.pending_scoped_library_search = Some(pending);
    set_priority(state, player);
    advance_to_next_player(state, events)
}

/// CR 701.23a/i: Begin one accepted search using the existing search resolver
/// for filtering, restrictions, local-X evaluation, and private candidate
/// visibility. Its normal delivery continuation is intentionally not followed;
/// this protocol stores the selection until every player has searched.
fn begin_accepted_search(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (player, scoped) = {
        let pending = state
            .pending_scoped_library_search
            .as_ref()
            .expect("accepted search requires pending state");
        let player = pending
            .current_player
            .expect("accepted search requires a current player");
        let mut scoped = (*pending.ability).clone();
        scoped.optional = false;
        scoped.set_original_controller_recursive(pending.ability.controller);
        scoped.set_controller_recursive(player);
        scoped.set_scoped_player_recursive(player);
        (player, scoped)
    };

    let events_before = events.len();
    set_priority(state, player);
    search_library::resolve(state, &scoped, events)?;

    // `search_library::resolve` normally reaches this ledger through
    // `resolve_chain_body`. This protocol deliberately calls the established
    // resolver directly to stop its immediate ParentTarget delivery, so record
    // only the accepted search event here. A decline never enters this function.
    let searched = events[events_before..].iter().any(|event| {
        matches!(
            event,
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::SearchedLibrary,
            } if *player_id == player
        )
    });
    if searched {
        state
            .player_actions_this_way
            .insert((player, PlayerActionKind::SearchedLibrary));
        state
            .player_actions_this_turn
            .push((player, PlayerActionKind::SearchedLibrary));
    }

    if !matches!(state.waiting_for, WaitingFor::SearchChoice { .. }) {
        // A legal accepted search may fail to find, or a search prohibition may
        // make the action impossible. In both cases no card selection prompt is
        // pending; only an actual `SearchedLibrary` event participates in the
        // final searched-this-way shuffle ledger.
        let mut pending = state
            .pending_scoped_library_search
            .take()
            .expect("pending scoped search survives direct resolver");
        if searched {
            pending.selections.push((player, Vec::new()));
        }
        pending.current_player = None;
        state.pending_scoped_library_search = Some(pending);
        advance_to_next_player(state, events)?;
    }
    Ok(())
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
        .take()
        .expect("completion requires pending scoped search");
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
    let selected: Vec<ObjectId> = pending
        .selections
        .iter()
        .flat_map(|(_, cards)| cards.iter().copied())
        .collect();
    let completion = BatchCompletion::ScopedLibrarySearchDelivery {
        player: pending.ability.controller,
        source_id: pending.ability.source_id,
        after_scope: pending.after_scope,
    };
    let reqs = selected
        .iter()
        .copied()
        .map(|id| {
            let mut request = ZoneMoveRequest::effect(id, destination, pending.ability.source_id);
            request.mods.enter_tapped = enter_tapped;
            request
        })
        .collect();
    match zone_pipeline::move_objects_simultaneously_then(state, reqs, Some(completion), events) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine::apply_as_current;
    use crate::game::zones::create_object;
    use crate::types::ability::{PlayerFilter, QuantityExpr, SearchSelectionConstraint};
    use crate::types::actions::GameAction;
    use crate::types::game_state::GameState;
    use crate::types::identifiers::CardId;

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
}
