//! Final authority for exact object-status transitions.

use crate::game::game_object::GameObject;
use crate::types::game_state::GameState;
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::resolved_commands::{
    ResolvedObjectStatus, ResolvedObjectStatusCommand, ResolvedObjectStatusReplayInvariantError,
};

/// Constructs, applies, and journals one exact object-status transition.
///
/// Callers resolve replacements and all ordinary legality before this boundary.
/// A no-op status request is intentionally not journaled.
pub fn resolve_and_apply_object_edit(
    state: &mut GameState,
    object_id: ObjectId,
    status: ResolvedObjectStatus,
    new: bool,
) -> Result<bool, ResolvedObjectStatusReplayInvariantError> {
    let object = state.objects.get(&object_id).ok_or(
        ResolvedObjectStatusReplayInvariantError::UnknownObject(object_id),
    )?;
    let reference = ObjectIncarnationRef::from_object(object);
    let expected_old = status_value(state, object, status);
    if expected_old == new {
        return Ok(false);
    }

    let command = ResolvedObjectStatusCommand {
        object: reference,
        status,
        expected_old,
        new,
        cause: state.current_or_begin_rules_execution_node(),
    };
    apply_resolved_object_edit(state, &command)?;
    state
        .resolved_rules_journal
        .record_object_status(command)
        .expect("resolved object status must have a live journal cause");
    Ok(true)
}

/// Applies one exact object-status transition with no replacement, lookup, or
/// inference beyond validating the captured object incarnation and old status.
pub fn apply_resolved_object_edit(
    state: &mut GameState,
    command: &ResolvedObjectStatusCommand,
) -> Result<(), ResolvedObjectStatusReplayInvariantError> {
    let object = state.objects.get(&command.object.object_id).ok_or(
        ResolvedObjectStatusReplayInvariantError::MissingObject(command.object),
    )?;
    let found_reference = ObjectIncarnationRef::from_object(object);
    if found_reference != command.object {
        return Err(ResolvedObjectStatusReplayInvariantError::StaleObject {
            expected: command.object,
            found: found_reference,
        });
    }
    let found_status = status_value(state, object, command.status);
    if found_status != command.expected_old {
        return Err(
            ResolvedObjectStatusReplayInvariantError::StatusPreconditionMismatch {
                status: command.status,
                expected: command.expected_old,
                found: found_status,
            },
        );
    }

    match command.status {
        // CR 701.26a-b: A status transition is valid only from the captured
        // opposite tapped state; replay never silently re-taps or re-untaps.
        ResolvedObjectStatus::Tapped => {
            state
                .objects
                .get_mut(&command.object.object_id)
                .expect("the validated object must remain present")
                .tapped = command.new;
        }
        // CR 701.43d: The per-turn exert record belongs to this exact current
        // object incarnation, never a same-id object that later re-entered.
        ResolvedObjectStatus::Exerted => {
            if command.new {
                state.exerted_this_turn.insert(command.object.object_id);
            } else {
                state.exerted_this_turn.remove(&command.object.object_id);
            }
        }
    }
    Ok(())
}

fn status_value(state: &GameState, object: &GameObject, status: ResolvedObjectStatus) -> bool {
    match status {
        ResolvedObjectStatus::Tapped => object.tapped,
        ResolvedObjectStatus::Exerted => state.exerted_this_turn.contains(&object.id),
    }
}
