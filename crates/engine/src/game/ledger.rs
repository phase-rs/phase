//! Final authority for composable per-event ledger facts.

use crate::types::ability::TriggerDefinitionRef;
use crate::types::game_state::{GameState, SpellCastRecord};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::resolved_commands::{
    ResolvedLedgerEdit, ResolvedLedgerEditCommand, ResolvedLedgerEditReplayInvariantError,
    ResolvedOncePerTurnPermission, ResolvedTriggerLedgerEdit,
};

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
