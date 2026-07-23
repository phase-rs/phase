//! Final authority for composable per-event ledger facts.

use crate::types::ability::TriggerDefinitionRef;
use crate::types::game_state::{GameState, SpellCastRecord};
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::player::PlayerId;
use crate::types::resolved_commands::{
    ledger_edit_is_invalid, ResolvedLedgerEdit, ResolvedLedgerEditCommand,
    ResolvedLedgerEditReplayInvariantError, ResolvedOncePerTurnPermission,
    ResolvedTriggerLedgerEdit,
};
use crate::types::zones::Zone;

/// Constructs, applies, and journals one exact semantic ledger edit.
///
/// The caller has already resolved the event's identity and any dynamic facts.
/// This boundary never re-runs casting, activation, trigger collection, or
/// permission selection.
pub fn resolve_and_apply_ledger_edit(
    state: &mut GameState,
    edit: ResolvedLedgerEdit,
) -> Result<(), ResolvedLedgerEditReplayInvariantError> {
    let command = ResolvedLedgerEditCommand {
        edit,
        cause: state.current_or_begin_rules_execution_node(),
    };
    apply_resolved_ledger_edit(state, &command)?;
    state
        .resolved_rules_journal
        .record_ledger_edit(command)
        .expect("resolved ledger edit must have a live journal cause");
    Ok(())
}

/// CR 601.2i: Append one finalized spell-cast fact without replacing another
/// player's history or an independent cast record.
pub fn record_spell_cast(
    state: &mut GameState,
    player: PlayerId,
    record: SpellCastRecord,
) -> Result<(), ResolvedLedgerEditReplayInvariantError> {
    let expected_turn_history_len = history_len(
        state
            .spells_cast_this_turn_by_player
            .get(&player)
            .map_or(0, |history| history.len()),
    )?;
    let expected_game_history_len = history_len(
        state
            .spells_cast_this_game_by_player
            .get(&player)
            .map_or(0, |history| history.len()),
    )?;
    resolve_and_apply_ledger_edit(
        state,
        ResolvedLedgerEdit::SpellCast {
            player,
            record,
            expected_turn_count: state.spells_cast_this_turn,
            expected_game_count: state
                .spells_cast_this_game
                .get(&player)
                .copied()
                .unwrap_or(0),
            expected_turn_history_len,
            expected_game_history_len,
        },
    )
}

/// CR 602.5b: Increment exactly one activated-ability occurrence's turn and
/// game counters.
pub fn record_ability_activation(
    state: &mut GameState,
    source: ObjectId,
    ability_index: usize,
) -> Result<(), ResolvedLedgerEditReplayInvariantError> {
    let key = (source, ability_index);
    resolve_and_apply_ledger_edit(
        state,
        ResolvedLedgerEdit::AbilityActivated {
            source,
            ability_index,
            expected_turn_count: state
                .activated_abilities_this_turn
                .get(&key)
                .copied()
                .unwrap_or(0),
            expected_game_count: state
                .activated_abilities_this_game
                .get(&key)
                .copied()
                .unwrap_or(0),
        },
    )
}

/// CR 603.2c: Record a fully classified constrained-trigger fact.
pub fn record_trigger_fired(
    state: &mut GameState,
    trigger: TriggerDefinitionRef,
    edit: ResolvedTriggerLedgerEdit,
) -> Result<(), ResolvedLedgerEditReplayInvariantError> {
    resolve_and_apply_ledger_edit(state, ResolvedLedgerEdit::TriggerFired { trigger, edit })
}

/// CR 601.2i: Consume one exact frequency-bounded permission slot.
pub fn consume_once_per_turn_permission(
    state: &mut GameState,
    source: ObjectId,
    permission: ResolvedOncePerTurnPermission,
) -> Result<(), ResolvedLedgerEditReplayInvariantError> {
    resolve_and_apply_ledger_edit(
        state,
        ResolvedLedgerEdit::OncePerTurnPermission { source, permission },
    )
}

/// CR 121.1 + CR 121.2 + CR 121.4: Capture and install one post-replacement
/// draw's exact bookkeeping. The zone-change hub has already installed
/// `drawn_object` before this ledger command is recorded; replay therefore
/// never selects a library card or re-runs replacement effects.
///
/// `drawn_object` is absent only when an attempted draw found an empty library.
/// The returned boolean tells the continuation-only miracle hook whether this
/// command established the player's first draw of the turn.
pub fn resolve_and_apply_cards_drawn(
    state: &mut GameState,
    player: PlayerId,
    drawn_object: Option<ObjectIncarnationRef>,
    attempted_empty_library: bool,
) -> Result<bool, ResolvedLedgerEditReplayInvariantError> {
    let player_state = state
        .players
        .iter()
        .find(|candidate| candidate.id == player)
        .ok_or(ResolvedLedgerEditReplayInvariantError::UnknownPlayer(
            player,
        ))?;
    let expected_drawn_cards_len = history_len(
        state
            .cards_drawn_this_turn
            .get(&player)
            .map_or(0, |drawn_cards| drawn_cards.len()),
    )?;
    let settled_card = drawn_object.is_some();
    let expected_first_card_drawn_this_turn =
        state.first_card_drawn_this_turn.get(&player).copied();
    let resulting_first_card_drawn_this_turn =
        expected_first_card_drawn_this_turn.or_else(|| drawn_object.map(|object| object.object_id));
    let established_first_draw = settled_card && expected_first_card_drawn_this_turn.is_none();
    let resulting_drawn_cards_len = if settled_card {
        expected_drawn_cards_len
            .checked_add(1)
            .ok_or(ResolvedLedgerEditReplayInvariantError::CounterOverflow)?
    } else {
        expected_drawn_cards_len
    };

    resolve_and_apply_ledger_edit(
        state,
        ResolvedLedgerEdit::CardsDrawn {
            player,
            drawn_object,
            attempted_empty_library,
            expected_has_drawn_this_turn: player_state.has_drawn_this_turn,
            resulting_has_drawn_this_turn: if settled_card {
                true
            } else {
                player_state.has_drawn_this_turn
            },
            expected_cards_drawn_this_turn: player_state.cards_drawn_this_turn,
            resulting_cards_drawn_this_turn: if settled_card {
                player_state.cards_drawn_this_turn.saturating_add(1)
            } else {
                player_state.cards_drawn_this_turn
            },
            expected_cards_drawn_this_step: player_state.cards_drawn_this_step,
            resulting_cards_drawn_this_step: if settled_card {
                player_state.cards_drawn_this_step.saturating_add(1)
            } else {
                player_state.cards_drawn_this_step
            },
            expected_drew_from_empty_library: player_state.drew_from_empty_library,
            resulting_drew_from_empty_library: player_state.drew_from_empty_library
                || attempted_empty_library,
            expected_drawn_cards_len,
            resulting_drawn_cards_len,
            expected_first_card_drawn_this_turn,
            resulting_first_card_drawn_this_turn,
        },
    )?;
    Ok(established_first_draw)
}

/// Applies one exact ledger edit without an event dispatcher, replacement
/// pipeline, allocator, or dynamic permission lookup.
pub fn apply_resolved_ledger_edit(
    state: &mut GameState,
    command: &ResolvedLedgerEditCommand,
) -> Result<(), ResolvedLedgerEditReplayInvariantError> {
    match &command.edit {
        ResolvedLedgerEdit::SpellCast {
            player,
            record,
            expected_turn_count,
            expected_game_count,
            expected_turn_history_len,
            expected_game_history_len,
        } => {
            if !state
                .players
                .iter()
                .any(|candidate| candidate.id == *player)
            {
                return Err(ResolvedLedgerEditReplayInvariantError::UnknownPlayer(
                    *player,
                ));
            }
            let turn_history_len = history_len(
                state
                    .spells_cast_this_turn_by_player
                    .get(player)
                    .map_or(0, |history| history.len()),
            )?;
            let game_history_len = history_len(
                state
                    .spells_cast_this_game_by_player
                    .get(player)
                    .map_or(0, |history| history.len()),
            )?;
            if state.spells_cast_this_turn != *expected_turn_count
                || state
                    .spells_cast_this_game
                    .get(player)
                    .copied()
                    .unwrap_or(0)
                    != *expected_game_count
                || turn_history_len != *expected_turn_history_len
                || game_history_len != *expected_game_history_len
            {
                return Err(ResolvedLedgerEditReplayInvariantError::SpellCastPreconditionMismatch);
            }
            let next_turn_count = expected_turn_count.saturating_add(1);
            let next_game_count = expected_game_count
                .checked_add(1)
                .ok_or(ResolvedLedgerEditReplayInvariantError::CounterOverflow)?;
            state.spells_cast_this_turn = next_turn_count;
            state.spells_cast_this_game.insert(*player, next_game_count);
            state
                .spells_cast_this_turn_by_player
                .entry(*player)
                .or_default()
                .push_back(record.clone());
            state
                .spells_cast_this_game_by_player
                .entry(*player)
                .or_default()
                .push_back(record.clone());
        }
        ResolvedLedgerEdit::AbilityActivated {
            source,
            ability_index,
            expected_turn_count,
            expected_game_count,
        } => {
            let key = (*source, *ability_index);
            if state
                .activated_abilities_this_turn
                .get(&key)
                .copied()
                .unwrap_or(0)
                != *expected_turn_count
                || state
                    .activated_abilities_this_game
                    .get(&key)
                    .copied()
                    .unwrap_or(0)
                    != *expected_game_count
            {
                return Err(
                    ResolvedLedgerEditReplayInvariantError::AbilityActivationPreconditionMismatch,
                );
            }
            let next_turn_count = expected_turn_count
                .checked_add(1)
                .ok_or(ResolvedLedgerEditReplayInvariantError::CounterOverflow)?;
            let next_game_count = expected_game_count
                .checked_add(1)
                .ok_or(ResolvedLedgerEditReplayInvariantError::CounterOverflow)?;
            state
                .activated_abilities_this_turn
                .insert(key, next_turn_count);
            state
                .activated_abilities_this_game
                .insert(key, next_game_count);
        }
        ResolvedLedgerEdit::CardsDrawn {
            player,
            drawn_object,
            expected_has_drawn_this_turn,
            resulting_has_drawn_this_turn,
            expected_cards_drawn_this_turn,
            resulting_cards_drawn_this_turn,
            expected_cards_drawn_this_step,
            resulting_cards_drawn_this_step,
            expected_drew_from_empty_library,
            resulting_drew_from_empty_library,
            expected_drawn_cards_len,
            expected_first_card_drawn_this_turn,
            resulting_first_card_drawn_this_turn,
            ..
        } => {
            if ledger_edit_is_invalid(&command.edit) {
                return Err(ResolvedLedgerEditReplayInvariantError::CardsDrawnPreconditionMismatch);
            }
            let Some(player_index) = state
                .players
                .iter()
                .position(|candidate| candidate.id == *player)
            else {
                return Err(ResolvedLedgerEditReplayInvariantError::UnknownPlayer(
                    *player,
                ));
            };
            let current_drawn_cards_len = history_len(
                state
                    .cards_drawn_this_turn
                    .get(player)
                    .map_or(0, |drawn_cards| drawn_cards.len()),
            )?;
            let current_first_card_drawn_this_turn =
                state.first_card_drawn_this_turn.get(player).copied();
            let player_state = &state.players[player_index];
            if player_state.has_drawn_this_turn != *expected_has_drawn_this_turn
                || player_state.cards_drawn_this_turn != *expected_cards_drawn_this_turn
                || player_state.cards_drawn_this_step != *expected_cards_drawn_this_step
                || player_state.drew_from_empty_library != *expected_drew_from_empty_library
                || current_drawn_cards_len != *expected_drawn_cards_len
                || current_first_card_drawn_this_turn != *expected_first_card_drawn_this_turn
            {
                return Err(ResolvedLedgerEditReplayInvariantError::CardsDrawnPreconditionMismatch);
            }
            if let Some(expected) = drawn_object {
                let found = state
                    .objects
                    .get(&expected.object_id)
                    .map(ObjectIncarnationRef::from_object);
                if found != Some(*expected) {
                    return Err(
                        ResolvedLedgerEditReplayInvariantError::DrawnObjectMismatch {
                            expected: *expected,
                            found,
                        },
                    );
                }
                if state.objects[&expected.object_id].zone == Zone::Library {
                    return Err(
                        ResolvedLedgerEditReplayInvariantError::DrawnObjectStillInLibrary(
                            *expected,
                        ),
                    );
                }
            }

            let player_state = &mut state.players[player_index];
            player_state.has_drawn_this_turn = *resulting_has_drawn_this_turn;
            player_state.cards_drawn_this_turn = *resulting_cards_drawn_this_turn;
            player_state.cards_drawn_this_step = *resulting_cards_drawn_this_step;
            player_state.drew_from_empty_library = *resulting_drew_from_empty_library;
            if let Some(object) = drawn_object {
                crate::game::effects::drawn_this_turn_choice::record_drawn_card(
                    state,
                    *player,
                    object.object_id,
                );
            }
            match resulting_first_card_drawn_this_turn {
                Some(object) => {
                    state.first_card_drawn_this_turn.insert(*player, *object);
                }
                None => {
                    state.first_card_drawn_this_turn.remove(player);
                }
            }
        }
        ResolvedLedgerEdit::TriggerFired { trigger, edit } => match edit {
            ResolvedTriggerLedgerEdit::OncePerTurn => {
                if !state.triggers_fired_this_turn.insert(trigger.clone()) {
                    return Err(ResolvedLedgerEditReplayInvariantError::TriggerAlreadyRecorded);
                }
            }
            ResolvedTriggerLedgerEdit::OncePerGame => {
                if !state.triggers_fired_this_game.insert(trigger.clone()) {
                    return Err(ResolvedLedgerEditReplayInvariantError::TriggerAlreadyRecorded);
                }
            }
            ResolvedTriggerLedgerEdit::OncePerOpponentPerTurn { opponent } => {
                if !state
                    .triggers_fired_this_turn_per_opponent
                    .insert((trigger.clone(), *opponent))
                {
                    return Err(ResolvedLedgerEditReplayInvariantError::TriggerAlreadyRecorded);
                }
            }
            ResolvedTriggerLedgerEdit::MaxTimesPerTurn { expected_old } => {
                let found = state
                    .trigger_fire_counts_this_turn
                    .get(trigger)
                    .copied()
                    .unwrap_or(0);
                if found != *expected_old {
                    return Err(
                        ResolvedLedgerEditReplayInvariantError::TriggerCountPreconditionMismatch {
                            expected: *expected_old,
                            found,
                        },
                    );
                }
                let next = expected_old
                    .checked_add(1)
                    .ok_or(ResolvedLedgerEditReplayInvariantError::CounterOverflow)?;
                state
                    .trigger_fire_counts_this_turn
                    .insert(trigger.clone(), next);
            }
        },
        ResolvedLedgerEdit::OncePerTurnPermission { source, permission } => {
            let inserted = match permission {
                ResolvedOncePerTurnPermission::GraveyardCast => {
                    state.graveyard_cast_permissions_used.insert(*source)
                }
                ResolvedOncePerTurnPermission::GraveyardCastPermanentType { permanent_type } => {
                    state
                        .graveyard_cast_permissions_used_per_type
                        .insert((*source, *permanent_type))
                }
                ResolvedOncePerTurnPermission::HandCastFree => {
                    state.hand_cast_free_permissions_used.insert(*source)
                }
                ResolvedOncePerTurnPermission::AlternativeCostGrant => {
                    state.alt_cost_grant_permissions_used.insert(*source)
                }
                ResolvedOncePerTurnPermission::ExilePlay => {
                    state.exile_play_permissions_used.insert(*source)
                }
                ResolvedOncePerTurnPermission::ExileCast => {
                    state.exile_cast_permissions_used.insert(*source)
                }
                ResolvedOncePerTurnPermission::TopOfLibraryCast => {
                    state.top_of_library_cast_permissions_used.insert(*source)
                }
            };
            if !inserted {
                return Err(
                    ResolvedLedgerEditReplayInvariantError::PermissionAlreadyConsumed(*permission),
                );
            }
        }
    }
    Ok(())
}

fn history_len(len: usize) -> Result<u32, ResolvedLedgerEditReplayInvariantError> {
    u32::try_from(len).map_err(|_| ResolvedLedgerEditReplayInvariantError::CounterOverflow)
}
