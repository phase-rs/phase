use crate::types::ability::{
    AbilityCost, AdditionalCost, BeholdCostAction, TapCreaturesSelectionMode, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CollectEvidenceResume, GameState, PendingCast, PendingManaAbility, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;
use crate::types::zones::{ExileCostSourceZone, Zone};

use super::engine::EngineError;
use super::game_object::UndecodedObjectRules;
use super::{casting, casting_costs, mana_abilities};
use casting_costs::{CostSelection, SpellCostPayment};

// A cast can start after its enclosing action consumed a parent choice. Share
// that action's existing rollback boundary and retain only the changed members
// needed to reconstruct the later pre-announcement point. Both live only until
// the input finishes; no additional GameState is cloned for this checkpoint.
pub(super) struct CastStartBoundary {
    pub(super) object_id: ObjectId,
    journal_start: usize,
    from_action_boundary: Vec<crate::types::game_state::SpellCastUndo>,
}

struct CastUndoFrame {
    action_boundary: std::sync::Arc<GameState>,
    cast_start: Option<CastStartBoundary>,
}

thread_local! {
    static CAST_START_BOUNDARIES: std::cell::RefCell<Vec<CastUndoFrame>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

pub(super) struct CastUndoBoundaryGuard;

impl CastUndoBoundaryGuard {
    pub(super) fn enter(action_boundary: std::sync::Arc<GameState>) -> Self {
        CAST_START_BOUNDARIES.with(|boundaries| {
            boundaries.borrow_mut().push(CastUndoFrame {
                action_boundary,
                cast_start: None,
            });
        });
        Self
    }

    pub(super) fn take(&self) -> Option<CastStartBoundary> {
        CAST_START_BOUNDARIES.with(|boundaries| {
            boundaries
                .borrow_mut()
                .last_mut()
                .and_then(|frame| frame.cast_start.take())
        })
    }

    pub(super) fn restore(self, action_boundary: std::sync::Arc<GameState>) -> GameState {
        drop(self);
        std::sync::Arc::unwrap_or_clone(action_boundary)
    }
}

impl Drop for CastUndoBoundaryGuard {
    fn drop(&mut self) {
        CAST_START_BOUNDARIES.with(|boundaries| {
            boundaries.borrow_mut().pop();
        });
    }
}

/// CR 601.2a + CR 733.1: retain the precise pre-announcement point as changes
/// relative to the existing action boundary, rather than cloning another game.
pub(super) fn remember_cast_start(
    state: &GameState,
    object_id: ObjectId,
) -> Result<(), EngineError> {
    CAST_START_BOUNDARIES.with(|boundaries| {
        if let Some(frame) = boundaries.borrow_mut().last_mut() {
            let cast_start = cast_undo_value(state)?;
            let action_boundary = cast_undo_value(&frame.action_boundary)?;
            let mut from_action_boundary = Vec::new();
            collect_cast_undo(
                Some(&cast_start),
                Some(&action_boundary),
                &mut Vec::new(),
                &mut from_action_boundary,
            );
            // Synchronous completed casts no longer need their checkpoints.
            frame.cast_start = Some(CastStartBoundary {
                object_id,
                journal_start: state.resolved_rules_journal.entries().len(),
                from_action_boundary,
            });
        }
        Ok(())
    })
}

/// CR 733.1: restore the whole cast's payments before consuming its cleanup.
/// Bad client answers remain ordinary rejected actions; callers use this only
/// for an explicit cancel or a typed failure of the casting procedure.
pub(super) fn restore_spell_cast_before_cancel(
    state: &mut GameState,
    player: PlayerId,
    pending: &PendingCast,
) -> Result<(), EngineError> {
    if pending.activation_ability_index.is_some() {
        return Ok(());
    }
    let Some(reversal) = state.spell_cast_reversal.take() else {
        if pending.alternative_cost_election
            == crate::types::game_state::SpellAlternativeCostElection::Legacy
        {
            return Err(EngineError::ActionNotAllowed("This saved casting step cannot be canceled safely; finish it or load an earlier save".into()));
        }
        return Ok(());
    };
    if reversal.object_id != pending.object_id || reversal.player != player {
        state.spell_cast_reversal = Some(reversal);
        return Err(EngineError::InvalidAction(
            "Cast reversal owner mismatch".into(),
        ));
    }
    if !reversal.cost_mutations {
        // No earlier non-mana payment needs undoing. Keep legal mana
        // activations and let the ordinary cancellation remove the declaration.
        return Ok(());
    }
    // CR 733.1: a library disclosure, shuffle or non-stack transfer cannot
    // be undone. Keep that payment window intact rather than pretend to cancel.
    if reversal.library_undo_forbidden {
        state.spell_cast_reversal = Some(reversal);
        return Err(EngineError::ActionNotAllowed(
            "Cannot undo a library disclosure, shuffle or transfer".into(),
        ));
    }
    super::play_trace::cast_mana_abilities_reversed(state);
    restore_sparse_cast_fields(state, &reversal)
}

fn restore_sparse_cast_fields(
    state: &mut GameState,
    reversal: &crate::types::game_state::SpellCastReversal,
) -> Result<(), EngineError> {
    let mut value = cast_undo_value(state)?;
    let mut changed = std::collections::BTreeSet::new();
    for edit in reversal.undo.iter().rev() {
        let path = match edit {
            crate::types::game_state::SpellCastUndo::Value { path, .. }
            | crate::types::game_state::SpellCastUndo::Splice { path, .. }
            | crate::types::game_state::SpellCastUndo::Hand { path, .. } => path,
        };
        let Some(field) = path.first() else {
            return Err(EngineError::InvalidAction("Empty cast undo path".into()));
        };
        changed.insert(field.clone());
        apply_cast_undo(&mut value, edit)?;
    }
    // CR 606.3: the ordinary object decoder skips per-permanent rule state.
    // Each serialized object carries it beside itself (`cast_undo_value`), so
    // the captured inverse restores it as one unit. The other skipped object
    // read-outs are derived again by finalization.
    let carried = value
        .get_mut("objects")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| EngineError::InvalidAction("Invalid cast undo objects".into()))?
        .iter_mut()
        .map(|(id, object)| {
            let rules = object
                .as_object_mut()
                .and_then(|object| object.remove(CAST_UNDO_OBJECT_RULES))
                .ok_or_else(|| {
                    EngineError::InvalidAction("Missing cast undo object rules".into())
                })?;
            Ok((
                serde_json::from_str::<ObjectId>(id).map_err(invalid_cast_undo_rules)?,
                serde_json::from_value::<UndecodedObjectRules>(rules)
                    .map_err(invalid_cast_undo_rules)?,
            ))
        })
        .collect::<Result<Vec<_>, EngineError>>()?;
    let mut restored: GameState = serde_json::from_value(value)
        .map_err(|error| EngineError::InvalidAction(format!("Invalid cast undo state: {error}")))?;
    for (id, rules) in carried {
        restored
            .objects
            .get_mut(&id)
            .ok_or_else(|| EngineError::InvalidAction("Missing cast undo rules object".into()))?
            .restore_undecoded_rules(rules);
    }
    state.restore_cast_fields(restored, &changed);
    state.layers_dirty.mark_full();
    Ok(())
}

fn cast_has_library_barrier(state: &GameState, journal_start: usize) -> bool {
    state
        .resolved_rules_journal
        .entries_since(journal_start)
        .iter()
        .any(|entry| match entry.command.as_ref() {
            Some(crate::types::resolved_commands::ResolvedRulesCommand::LibraryShuffle(_)) => true,
            Some(crate::types::resolved_commands::ResolvedRulesCommand::ZoneChange(change)) => {
                change.to == Zone::Library
                    || (change.from == Zone::Library && change.to != Zone::Stack)
            }
            Some(crate::types::resolved_commands::ResolvedRulesCommand::Information(info)) => {
                info.edit == crate::types::resolved_commands::ResolvedInformationEdit::Reveal
                    && info.occurrences.iter().any(|occurrence| {
                        state
                            .objects
                            .get(&occurrence.object_id)
                            .is_some_and(|object| object.zone == Zone::Library)
                    })
            }
            None | Some(
                crate::types::resolved_commands::ResolvedRulesCommand::ManaInsert(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::ManaSpend(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::PlayerEdit(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::ObjectStatus(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::ObjectCounter(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::ObjectTransform(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::Attachment(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::DelayedTriggerInstall(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::ContinuousEffect(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::CombatMembership(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::ControllerOverride(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::EntryProvenance(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::ObjectCease(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::PlayerLeave(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::TokenCreation(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::LedgerEdit(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::FrameTransition(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::TriggerCollection(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::StackPush(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::StackEntryFinalize(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::UncommittedTriggerRemoval(_)
                | crate::types::resolved_commands::ResolvedRulesCommand::StackRemoval(_)
            ) => false,
        })
}

/// CR 733.1: record the inverse of one admitted action using its existing
/// boundary snapshot. Only changed values survive this call; no game snapshot
/// is retained in the casting descriptor or written into a save.
pub(super) fn record_spell_cast_undo(
    state: &mut GameState,
    before: &GameState,
    action_start: usize,
    cast_start: Option<&CastStartBoundary>,
) -> Result<(), EngineError> {
    if state.spell_cast_reversal.is_none() {
        return Ok(());
    }
    let action_start = cast_start.map_or(action_start, |boundary| boundary.journal_start);
    note_spell_cost_mutations(state, action_start);
    let root = state.spell_cast_reversal.as_ref().unwrap();
    if !root.cost_mutations {
        return Ok(());
    }
    let journal_start = root.journal_start.max(action_start);
    if root.library_undo_forbidden || cast_has_library_barrier(state, journal_start) {
        std::sync::Arc::make_mut(state.spell_cast_reversal.as_mut().unwrap())
            .library_undo_forbidden = true;
        return Ok(());
    }
    let mut before_value = cast_undo_value(before)?;
    if let Some(boundary) = cast_start {
        for edit in boundary.from_action_boundary.iter().rev() {
            apply_cast_undo(&mut before_value, edit)?;
        }
    }
    // Fold the prior inverse into this action's existing boundary. The stored
    // result describes changed state, rather than accumulating a per-click log.
    if cast_start.is_none() {
        if let Some(root) = before.spell_cast_reversal.as_ref() {
            for edit in root.undo.iter().rev() {
                apply_cast_undo(&mut before_value, edit)?;
            }
        }
    }
    let after_value = cast_undo_value(state)?;
    let mut edits = Vec::new();
    collect_cast_undo(
        Some(&before_value),
        Some(&after_value),
        &mut Vec::new(),
        &mut edits,
    );
    let root = std::sync::Arc::make_mut(state.spell_cast_reversal.as_mut().unwrap());
    if root.undo.is_empty() {
        root.journal_start = action_start;
    }
    root.undo = edits;
    Ok(())
}

/// CR 733.1 + CR 800.4a: a concession is independent of a casting payment.
/// Rebase its inverse through the ordinary reducer, so canceling the spell can
/// never restore a departed player or remove the departure's journal entries.
/// The reconstructed state is local to this call, never part of the descriptor.
pub(super) fn preserve_independent_cast_action(
    state: &mut GameState,
    before: &GameState,
    authenticated_actor: PlayerId,
    semantic_owner: PlayerId,
    action: crate::types::actions::GameAction,
) -> Result<(), EngineError> {
    let Some(root) = before.spell_cast_reversal.as_ref() else {
        return Ok(());
    };
    if state.spell_cast_reversal.is_none() || !root.cost_mutations || root.library_undo_forbidden {
        return Ok(());
    }
    let mut retained = before.clone();
    retained.spell_cast_reversal = None;
    restore_sparse_cast_fields(&mut retained, root)?;
    super::engine::apply_admitted_recorded_action(
        &mut retained,
        authenticated_actor,
        semantic_owner,
        action,
    )?;
    let retained_value = cast_undo_value(&retained)?;
    let current_value = cast_undo_value(state)?;
    let mut undo = Vec::new();
    collect_cast_undo(
        Some(&retained_value),
        Some(&current_value),
        &mut Vec::new(),
        &mut undo,
    );
    let root = std::sync::Arc::make_mut(state.spell_cast_reversal.as_mut().unwrap());
    root.journal_start = retained.resolved_rules_journal.entries().len();
    root.undo = undo;
    Ok(())
}

fn cast_undo_value(state: &GameState) -> Result<serde_json::Value, EngineError> {
    let mut value = serde_json::to_value(state)
        .map_err(|error| EngineError::InvalidAction(format!("Cannot record cast undo: {error}")))?;
    // The descriptor is not part of the compared state. In particular, prior
    // inverse edits must never become input to another inverse. No extra
    // GameState clone is needed for this serialization boundary.
    value
        .as_object_mut()
        .expect("GameState serializes as an object")
        .remove("spell_cast_reversal");
    let objects = value
        .get_mut("objects")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| EngineError::InvalidAction("Invalid cast undo objects".into()))?;
    for (id, object) in objects.iter_mut() {
        let id = serde_json::from_str::<ObjectId>(id).map_err(invalid_cast_undo_rules)?;
        let rules = state
            .objects
            .get(&id)
            .ok_or_else(|| EngineError::InvalidAction("Missing cast undo rules object".into()))?
            .undecoded_rules();
        object
            .as_object_mut()
            .ok_or_else(|| EngineError::InvalidAction("Invalid cast undo object".into()))?
            .insert(
                CAST_UNDO_OBJECT_RULES.into(),
                serde_json::to_value(rules).map_err(invalid_cast_undo_rules)?,
            );
    }
    Ok(value)
}

/// Serialized key of an object's `UndecodedObjectRules` inside a cast-undo value.
const CAST_UNDO_OBJECT_RULES: &str = "cast_undo_object_rules";

fn invalid_cast_undo_rules(error: serde_json::Error) -> EngineError {
    EngineError::InvalidAction(format!("Invalid cast undo object rules: {error}"))
}

fn cast_undo_preserves(field: &str) -> bool {
    [
        "state_revision",
        "rng_word_pos",
        "rng_seed",
        "play_trace",
        "next_object_id",
        "next_spell_announcement",
        "next_pip_id",
        "next_resolution_cast_offer_id",
        "next_delayed_trigger_token",
        "next_delayed_trigger_instance",
        "next_logical_zone_change_group_id",
        "next_timestamp",
        "next_continuous_effect_id",
        "next_end_effect_group_id",
        "next_tracked_set_id",
        "next_return_result_occurrence_id",
        "next_extra_phase_id",
        "next_resolve_all_consent_epoch",
        "next_interaction_serial",
    ]
    .contains(&field)
}

fn collect_cast_undo(
    before: Option<&serde_json::Value>,
    after: Option<&serde_json::Value>,
    path: &mut Vec<String>,
    edits: &mut Vec<crate::types::game_state::SpellCastUndo>,
) {
    use crate::types::game_state::SpellCastUndo as U;
    use serde_json::Value as V;
    if before == after {
        return;
    }
    match (before, after) {
        (Some(V::Object(before)), Some(V::Object(after))) => {
            let keys: std::collections::BTreeSet<_> = before.keys().chain(after.keys()).collect();
            for key in keys {
                if path.is_empty() && cast_undo_preserves(key) {
                    continue;
                }
                path.push(key.clone());
                collect_cast_undo(before.get(key), after.get(key), path, edits);
                path.pop();
            }
        }
        (Some(V::Array(before)), Some(V::Array(after)))
            if path.len() == 3 && path[0] == "players" && path[2] == "hand" =>
        {
            let removed = before
                .iter()
                .enumerate()
                .filter(|(_, id)| !after.contains(id))
                .map(|(index, id)| (index, id.clone()))
                .collect();
            let added = after
                .iter()
                .filter(|id| !before.contains(id))
                .cloned()
                .collect();
            edits.push(U::Hand {
                path: path.clone(),
                removed,
                added,
            });
        }
        (Some(V::Array(before)), Some(V::Array(after))) if before.len() == after.len() => {
            for (index, (before, after)) in before.iter().zip(after).enumerate() {
                path.push(index.to_string());
                collect_cast_undo(Some(before), Some(after), path, edits);
                path.pop();
            }
        }
        (Some(V::Array(before)), Some(V::Array(after))) => {
            let prefix = before.iter().zip(after).take_while(|(a, b)| a == b).count();
            let suffix = before[prefix..]
                .iter()
                .rev()
                .zip(after[prefix..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            edits.push(U::Splice {
                path: path.clone(),
                index: prefix,
                remove: after.len() - prefix - suffix,
                insert: before[prefix..before.len() - suffix].to_vec(),
            });
        }
        _ => edits.push(U::Value {
            path: path.clone(),
            before: before.cloned(),
        }),
    }
}

fn undo_path_mut<'a>(
    value: &'a mut serde_json::Value,
    path: &[String],
) -> Result<&'a mut serde_json::Value, EngineError> {
    let mut current = value;
    for part in path {
        current = match current {
            serde_json::Value::Object(fields) => fields.get_mut(part),
            serde_json::Value::Array(items) => {
                part.parse::<usize>().ok().and_then(|i| items.get_mut(i))
            }
            _ => None,
        }
        .ok_or_else(|| EngineError::InvalidAction("Cast undo path no longer exists".into()))?;
    }
    Ok(current)
}

fn apply_cast_undo(
    value: &mut serde_json::Value,
    edit: &crate::types::game_state::SpellCastUndo,
) -> Result<(), EngineError> {
    use crate::types::game_state::SpellCastUndo as U;
    match edit {
        U::Value { path, before } => {
            let (last, parent) = path
                .split_last()
                .ok_or_else(|| EngineError::InvalidAction("Empty cast undo path".into()))?;
            match undo_path_mut(value, parent)? {
                serde_json::Value::Object(fields) => match before {
                    Some(before) => {
                        fields.insert(last.clone(), before.clone());
                    }
                    None => {
                        fields.remove(last);
                    }
                },
                serde_json::Value::Array(items) => {
                    let index = last
                        .parse::<usize>()
                        .ok()
                        .filter(|i| *i < items.len())
                        .ok_or_else(|| {
                            EngineError::InvalidAction("Invalid cast undo index".into())
                        })?;
                    items[index] = before.clone().ok_or_else(|| {
                        EngineError::InvalidAction("Missing cast undo value".into())
                    })?;
                }
                _ => {
                    return Err(EngineError::InvalidAction(
                        "Invalid cast undo parent".into(),
                    ))
                }
            }
        }
        U::Splice {
            path,
            index,
            remove,
            insert,
        } => {
            let items = undo_path_mut(value, path)?.as_array_mut().ok_or_else(|| {
                EngineError::InvalidAction("Cast undo splice is not an array".into())
            })?;
            let end = index
                .checked_add(*remove)
                .filter(|end| *index <= items.len() && *end <= items.len())
                .ok_or_else(|| EngineError::InvalidAction("Invalid cast undo splice".into()))?;
            items.splice(*index..end, insert.iter().cloned());
        }
        U::Hand {
            path,
            removed,
            added,
        } => {
            let items = undo_path_mut(value, path)?.as_array_mut().ok_or_else(|| {
                EngineError::InvalidAction("Cast undo hand is not an array".into())
            })?;
            items.retain(|id| !added.contains(id));
            for (index, id) in removed {
                if items.contains(id) {
                    return Err(EngineError::InvalidAction(
                        "Cast undo would duplicate a hand card".into(),
                    ));
                }
                items.insert((*index).min(items.len()), id.clone());
            }
        }
    }
    Ok(())
}

/// CR 601.2h: semantic resource commands of the casting proposal are the
/// evidence of a cost mutation; separate mana nodes keep their own identity.
pub(super) fn note_spell_cost_mutations(state: &mut GameState, action_start: usize) {
    use crate::types::resolved_commands::{ResolvedRulesCommand as C, RulesExecutionNodeRef as N};
    let Some(root) = state.spell_cast_reversal.as_ref() else {
        return;
    };
    if root.cost_mutations {
        return;
    }
    let changed = state
        .resolved_rules_journal
        .entries_since(action_start.max(root.journal_start))
        .iter()
        .any(|entry| {
            if !matches!(entry.node, N::Proposal(_) | N::Payment(_)) {
                return false;
            }
            match entry.command.as_ref() {
                Some(C::ManaSpend(spend)) => matches!(spend.recipient,
                    crate::types::resolved_commands::ManaPaymentRecipient::Object(object)
                        if object.object_id == root.object_id),
                Some(
                    C::PlayerEdit(_)
                    | C::ObjectStatus(_)
                    | C::ObjectCounter(_)
                    | C::ZoneChange(_)
                    | C::Attachment(_)
                    | C::ObjectTransform(_)
                    | C::ControllerOverride(_)
                    | C::ObjectCease(_),
                ) => true,
                None
                | Some(
                    C::ManaInsert(_)
                    | C::DelayedTriggerInstall(_)
                    | C::ContinuousEffect(_)
                    | C::CombatMembership(_)
                    | C::EntryProvenance(_)
                    | C::PlayerLeave(_)
                    | C::TokenCreation(_)
                    | C::Information(_)
                    | C::LedgerEdit(_)
                    | C::LibraryShuffle(_)
                    | C::FrameTransition(_)
                    | C::TriggerCollection(_)
                    | C::StackPush(_)
                    | C::StackEntryFinalize(_)
                    | C::UncommittedTriggerRemoval(_)
                    | C::StackRemoval(_),
                ) => false,
            }
        });
    if changed {
        std::sync::Arc::make_mut(state.spell_cast_reversal.as_mut().unwrap()).cost_mutations = true;
    }
}

pub(super) fn cancel_pending_cast(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: &PendingCast,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if pending_cast.activation_cost_committed {
        return Err(EngineError::ActionNotAllowed(
            "Cannot cancel an activation after a cost is paid".to_string(),
        ));
    }
    // CR 601.2: if a player cannot comply with a casting step, that illegal
    // cast returns to the moment before the spell was proposed. This rules
    // note covers only that incomplete-casting rollback fact.
    //
    // Capture and consume the exact resolution-owned grant before generic
    // rollback removes the placeholder stack entry.  A normal pending cast
    // has no such cleanup and retains the historical Priority result; a
    // resolution cast must instead dispose of its offered card/misses and
    // resume the parked parent exactly once.
    // CR 733.1: return the announcement carrier from action-local custody
    // before replaying its inverse. No events of the undone input are staged.
    super::announcement_triggers::end_action(state, &[]);
    let resolution_cleanup = match pending_cast.casting_permission_index {
        Some(index) => casting::take_resolution_cast_cleanup(
            state,
            player,
            pending_cast.object_id,
            pending_cast.card_id,
            index,
        )?,
        None => None,
    };
    restore_spell_cast_before_cancel(state, player, pending_cast)?;
    // CR 733.1: retain the pending owner's shape until inverse edits have
    // restored it, then retire that owner together with the announcement.
    if state.pending_cast.as_ref().is_some_and(|pending| {
        pending.object_id == pending_cast.object_id
            && pending.activation_ability_index == pending_cast.activation_ability_index
    }) {
        state.pending_cast = None;
    }
    casting::handle_cancel_cast(state, pending_cast, events);
    if let Some(cleanup) = resolution_cleanup {
        return super::engine_resolution_choices::abort_resolution_cast(
            state,
            player,
            pending_cast.object_id,
            cleanup,
            events,
        );
    }
    Ok(WaitingFor::Priority { player })
}

pub(super) fn handle_target_selection_select_targets(
    state: &mut GameState,
    player: PlayerId,
    targets: Vec<crate::types::ability::TargetRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting::handle_select_targets(state, player, targets, events)
}

pub(super) fn handle_target_selection_choose_target(
    state: &mut GameState,
    player: PlayerId,
    target: Option<crate::types::ability::TargetRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting::handle_choose_target(state, player, target, events)
}

pub(super) fn handle_optional_cost_choice(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    cost: &AdditionalCost,
    pay: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_decide_additional_cost(state, player, pending_cast, cost, pay, events)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_defiler_payment(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    life_cost: u32,
    mana_reduction: &crate::types::mana::ManaCost,
    reach: crate::types::statics::CostReductionReach,
    pay: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_defiler_payment(
        state,
        player,
        pending_cast,
        life_cost,
        mana_reduction,
        reach,
        pay,
        events,
    )
}

/// CR 601.2f + CR 602.2b: Apply the caster's elected reduction order to an
/// ACTIVATION. The order is validated as a strict permutation of the prompt's
/// reductions (a malformed election is `InvalidAction` with the prompt left
/// live); a valid one either continues the activation or reverses it.
pub(super) fn resume_activation_cost_election(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: &PendingCast,
    reductions: &[crate::types::casting_costs::CostReductionEntry],
    order: &[usize],
    hybrid_announcement: &[crate::types::mana::ManaCostShard],
    events: &mut Vec<GameEvent>,
) -> Result<casting::ActivationElectionResume, EngineError> {
    // An activation cost has no hybrid symbol any reduction could announce
    // (CR 118.7a: its reductions are generic-only), so the announcement axis is
    // empty and only the order is elected.
    casting::validate_cost_reduction_election(order, hybrid_announcement, reductions, &[])
        .map_err(EngineError::InvalidAction)?;
    let order = order
        .iter()
        .map(|&index| reductions[index].provenance)
        .collect();
    casting::resume_activation_after_cost_election(state, player, pending_cast, order, events)
}

/// CR 601.2b + CR 601.2f: Apply the caster's elected cost-determination choices.
pub(super) fn handle_order_cost_reductions(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    reductions: &[crate::types::casting_costs::CostReductionEntry],
    order: &[usize],
    hybrid_announcement: &[crate::types::mana::ManaCostShard],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_order_cost_reductions(
        state,
        player,
        pending_cast,
        reductions,
        order,
        hybrid_announcement,
        events,
    )
}

pub(super) fn handle_discard_for_cost(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    count: usize,
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting::handle_discard_for_cost(
        state,
        player,
        pending_cast,
        count,
        legal_cards,
        chosen,
        events,
    )
}

pub(super) fn handle_reveal_for_cost(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    count: usize,
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting::handle_reveal_for_cost(
        state,
        player,
        pending_cast,
        count,
        legal_cards,
        chosen,
        events,
    )
}

pub(super) fn handle_activation_cost_one_of_choice(
    state: &mut GameState,
    player: PlayerId,
    pending: PendingCast,
    costs: &[AbilityCost],
    index: usize,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting::handle_activation_cost_one_of_choice(state, player, pending, costs, index, events)
}

pub(super) fn handle_sacrifice_for_cost(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    paid_cost: Option<SpellCostPayment<'_>>,
    selection: CostSelection<'_>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting::handle_sacrifice_for_cost(state, player, pending_cast, paid_cost, selection, events)
}

pub(super) fn handle_return_to_hand_for_cost(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    count: usize,
    permanents: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting::handle_return_to_hand_for_cost(
        state,
        player,
        pending_cast,
        count,
        permanents,
        chosen,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_tap_creatures_for_spell_cost(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    min_count: usize,
    count: usize,
    mode: TapCreaturesSelectionMode,
    creatures: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_tap_creatures_for_spell_cost(
        state,
        player,
        pending_cast,
        min_count,
        count,
        mode,
        creatures,
        chosen,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_behold_for_cost(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    count: usize,
    choices: &[ObjectId],
    action: BeholdCostAction,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_behold_for_cost(
        state,
        player,
        pending_cast,
        count,
        choices,
        action,
        chosen,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_tap_creatures_for_mana_ability(
    state: &mut GameState,
    min_count: usize,
    count: usize,
    mode: TapCreaturesSelectionMode,
    creatures: &[ObjectId],
    pending_mana_ability: &PendingManaAbility,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    mana_abilities::handle_tap_creatures_for_mana_ability(
        state,
        min_count,
        count,
        mode,
        creatures,
        pending_mana_ability,
        chosen,
        events,
    )
}

pub(super) fn handle_discard_for_mana_ability(
    state: &mut GameState,
    count: usize,
    legal_cards: &[ObjectId],
    pending_mana_ability: &PendingManaAbility,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    mana_abilities::handle_discard_for_mana_ability(
        state,
        count,
        legal_cards,
        pending_mana_ability,
        chosen,
        events,
    )
}

pub(super) fn handle_choose_mana_color(
    state: &mut GameState,
    pending_mana_ability: &PendingManaAbility,
    prompt: &crate::types::game_state::ManaChoicePrompt,
    chosen: crate::types::game_state::ManaChoice,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    mana_abilities::handle_choose_mana_color(state, pending_mana_ability, prompt, chosen, events)
}

/// CR 605.3a: Bulk-activate identical, choice-free sibling mana sources with the
/// color just chosen (the player's other Treasures, etc.). Thin forward to the
/// engine authority in `mana_abilities`.
pub(super) fn batch_activate_mana_siblings(
    state: &mut GameState,
    pending_mana_ability: &PendingManaAbility,
    chosen: &crate::types::game_state::ManaChoice,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    mana_abilities::batch_activate_mana_siblings(state, pending_mana_ability, chosen, count, events)
}

pub(super) fn handle_pay_mana_ability_mana(
    state: &mut GameState,
    options: &[Vec<crate::types::mana::ManaType>],
    pending_mana_ability: &PendingManaAbility,
    payment: &[crate::types::mana::ManaType],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    mana_abilities::handle_pay_mana_ability_mana(
        state,
        options,
        pending_mana_ability,
        payment,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_exile_for_cost(
    state: &mut GameState,
    player: PlayerId,
    zone: ExileCostSourceZone,
    pending_cast: PendingCast,
    count: usize,
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_exile_for_cost(
        state,
        player,
        zone,
        pending_cast,
        count,
        legal_cards,
        chosen,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_exile_aggregate_for_cost(
    state: &mut GameState,
    player: PlayerId,
    zone: crate::types::zones::Zone,
    function: crate::types::ability::AggregateFunction,
    property: crate::types::ability::ObjectProperty,
    comparator: crate::types::ability::Comparator,
    value: i32,
    filter: &TargetFilter,
    pending_cast: PendingCast,
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_exile_aggregate_for_cost(
        state,
        player,
        zone,
        function,
        property,
        comparator,
        value,
        filter,
        pending_cast,
        legal_cards,
        chosen,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_exile_permanent_for_cost(
    state: &mut GameState,
    player: PlayerId,
    filter: Option<TargetFilter>,
    pending_cast: PendingCast,
    count: usize,
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_exile_permanent_for_cost(
        state,
        player,
        filter,
        pending_cast,
        count,
        legal_cards,
        chosen,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_exile_materials_for_cost(
    state: &mut GameState,
    player: PlayerId,
    materials: TargetFilter,
    pending_cast: PendingCast,
    bounds: (usize, usize),
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting_costs::handle_exile_materials_for_cost(
        state,
        player,
        materials,
        pending_cast,
        bounds,
        legal_cards,
        chosen,
        events,
    )
}

pub(super) fn handle_collect_evidence_cancel(
    state: &mut GameState,
    player: PlayerId,
    resume: &CollectEvidenceResume,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if let CollectEvidenceResume::Casting { pending_cast, .. } = resume {
        let pending_cast = state
            .pending_cast
            .clone()
            .unwrap_or_else(|| pending_cast.clone());
        return cancel_pending_cast(state, player, &pending_cast, events);
    }
    Ok(WaitingFor::Priority { player })
}

pub(super) fn handle_harmonize_tap_choice(
    state: &mut GameState,
    player: PlayerId,
    eligible_creatures: &[ObjectId],
    pending_cast: PendingCast,
    creature_id: Option<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let mut pending = pending_cast;

    if let Some(creature_id) = creature_id {
        if !eligible_creatures.contains(&creature_id) {
            return Err(EngineError::ActionNotAllowed(
                "Creature not eligible for Harmonize tap".into(),
            ));
        }

        let obj = state
            .objects
            .get(&creature_id)
            .ok_or_else(|| EngineError::InvalidAction("Creature no longer exists".into()))?;
        if obj.zone != Zone::Battlefield || obj.tapped {
            return Err(EngineError::InvalidAction(
                "Creature is no longer eligible for Harmonize tap".into(),
            ));
        }

        let power = obj.power.unwrap_or(0).max(0) as u32;

        // CR 701.26a + CR 508.1f: route the Harmonize tap through the single
        // authority so a "can't become tapped" creature is refused.
        crate::game::restrictions::tap_permanent_for_cost(state, creature_id, events)?;

        if let ManaCost::Cost {
            ref mut generic, ..
        } = pending.cost
        {
            *generic = generic.saturating_sub(power);
        }
    }

    let base_cost = pending.base_cost.clone();
    let lock = casting_costs::CostLockInput::from_pending(&pending);
    casting_costs::pay_and_push_adventure(
        state,
        player,
        pending.object_id,
        pending.card_id,
        *pending.ability,
        &pending.cost,
        base_cost,
        pending.casting_variant,
        pending.casting_permission_index,
        pending.cast_timing_permission,
        pending.distribute,
        pending.origin_zone,
        pending.payment_mode,
        lock,
        events,
    )
}
