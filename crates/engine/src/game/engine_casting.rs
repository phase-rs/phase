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
    from_action_boundary: CastUndoLists,
}

/// Which serialization a cast-undo capture produces.
#[derive(Clone, Copy)]
enum CastUndoForm {
    /// CR 733.1: the exact serialization, display-only data included, so a
    /// cancelled cast restores every value.
    Exact,
    /// CR 104.4b + CR 203.1 + CR 206.1: the rules projection of the typed value,
    /// which omits display-only data such as a printing.
    Rules,
}

impl CastUndoForm {
    fn to_value<T: serde::Serialize + ?Sized>(
        self,
        value: &T,
    ) -> serde_json::Result<serde_json::Value> {
        match self {
            Self::Exact => serde_json::to_value(value),
            Self::Rules => crate::types::display_only::rules_projection(value),
        }
    }
}

/// Whether the descriptor being written keeps a comparison list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ComparisonIdentity {
    Projected,
    /// A descriptor saved before the comparison list existed; it keeps none
    /// until its reversal ends.
    Legacy,
}

impl ComparisonIdentity {
    fn of(reversal: &crate::types::game_state::SpellCastReversal) -> Self {
        match reversal.comparison_undo {
            Some(_) => Self::Projected,
            None => Self::Legacy,
        }
    }
}

/// One typed state's captures, taken from the same value. The rules capture is
/// absent for a legacy descriptor.
struct CastUndoValues {
    exact: serde_json::Value,
    rules: Option<serde_json::Value>,
}

impl CastUndoValues {
    fn capture(state: &GameState, identity: ComparisonIdentity) -> Result<Self, EngineError> {
        Ok(Self {
            exact: cast_undo_value(state, CastUndoForm::Exact)?,
            rules: match identity {
                ComparisonIdentity::Projected => Some(cast_undo_value(state, CastUndoForm::Rules)?),
                ComparisonIdentity::Legacy => None,
            },
        })
    }

    /// Apply an earlier inverse to both captures in lockstep. A step without a
    /// comparison list drops the rules capture: a legacy inverse never yields one.
    fn undo(
        &mut self,
        restoration: &[crate::types::game_state::SpellCastUndo],
        comparison: Option<&[crate::types::game_state::SpellCastUndo]>,
    ) -> Result<(), EngineError> {
        for edit in restoration.iter().rev() {
            apply_cast_undo(&mut self.exact, edit)?;
        }
        match (&mut self.rules, comparison) {
            (Some(rules), Some(edits)) => {
                for edit in edits.iter().rev() {
                    apply_cast_undo(rules, edit)?;
                }
            }
            (Some(_), None) => self.rules = None,
            (None, _) => {}
        }
        Ok(())
    }
}

/// The inverse edits from one capture back to an earlier one, in both forms.
struct CastUndoLists {
    restoration: Vec<crate::types::game_state::SpellCastUndo>,
    comparison: Option<Vec<crate::types::game_state::SpellCastUndo>>,
}

impl CastUndoLists {
    /// The edits that turn `after` back into `before`; the comparison list
    /// exists only when both captures have a rules form.
    fn between(before: &CastUndoValues, after: &CastUndoValues) -> Self {
        let diff = |before: &serde_json::Value, after: &serde_json::Value| {
            let mut edits = Vec::new();
            collect_cast_undo(Some(before), Some(after), &mut Vec::new(), &mut edits);
            edits
        };
        Self {
            restoration: diff(&before.exact, &after.exact),
            comparison: match (&before.rules, &after.rules) {
                (Some(before), Some(after)) => Some(diff(before, after)),
                (Some(_), None) | (None, Some(_)) | (None, None) => None,
            },
        }
    }

    fn install(self, root: &mut crate::types::game_state::SpellCastReversal) {
        root.undo = self.restoration;
        root.comparison_undo = self.comparison;
    }
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
            // A fresh descriptor follows this call, and it always carries a
            // comparison list.
            let identity = ComparisonIdentity::Projected;
            let cast_start = CastUndoValues::capture(state, identity)?;
            let action_boundary = CastUndoValues::capture(&frame.action_boundary, identity)?;
            // Synchronous completed casts no longer need their checkpoints.
            frame.cast_start = Some(CastStartBoundary {
                object_id,
                journal_start: state.resolved_rules_journal.entries().len(),
                from_action_boundary: CastUndoLists::between(&cast_start, &action_boundary),
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
    // CR 733.1: cancellation applies the restoration list only.
    let mut value = cast_undo_value(state, CastUndoForm::Exact)?;
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
    // Both lists are captured from the same typed states and composed in
    // lockstep; the descriptor being written decides whether a comparison list
    // is kept.
    let identity = ComparisonIdentity::of(root);
    let mut before_values = CastUndoValues::capture(before, identity)?;
    if let Some(boundary) = cast_start {
        before_values.undo(
            &boundary.from_action_boundary.restoration,
            boundary.from_action_boundary.comparison.as_deref(),
        )?;
    }
    // Fold the prior inverse into this action's existing boundary. The stored
    // result describes changed state, rather than accumulating a per-click log.
    if cast_start.is_none() {
        if let Some(prior) = before.spell_cast_reversal.as_ref() {
            before_values.undo(&prior.undo, prior.comparison_undo.as_deref())?;
        }
    }
    let after_values = CastUndoValues::capture(state, identity)?;
    let lists = CastUndoLists::between(&before_values, &after_values);
    let current = std::sync::Arc::make_mut(state.spell_cast_reversal.as_mut().unwrap());
    if current.undo.is_empty() {
        current.journal_start = action_start;
    }
    lists.install(current);
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
    let identity = ComparisonIdentity::of(root);
    let mut retained = before.clone();
    retained.spell_cast_reversal = None;
    restore_sparse_cast_fields(&mut retained, root)?;
    super::engine::apply_admitted_recorded_action(
        &mut retained,
        authenticated_actor,
        semantic_owner,
        action,
    )?;
    // The rebased descriptor keeps the form of the descriptor being rebased.
    let retained_values = CastUndoValues::capture(&retained, identity)?;
    let current_values = CastUndoValues::capture(state, identity)?;
    let lists = CastUndoLists::between(&retained_values, &current_values);
    let current = std::sync::Arc::make_mut(state.spell_cast_reversal.as_mut().unwrap());
    current.journal_start = retained.resolved_rules_journal.entries().len();
    lists.install(current);
    Ok(())
}

fn cast_undo_value(
    state: &GameState,
    form: CastUndoForm,
) -> Result<serde_json::Value, EngineError> {
    let mut value = form
        .to_value(state)
        .map_err(|error| EngineError::InvalidAction(format!("Cannot record cast undo: {error}")))?;
    // The descriptor is not part of the compared state. In particular, prior
    // inverse edits must never become input to another inverse, and its
    // fragments are already-erased JSON that a rules projection cannot see a
    // printing inside. No extra GameState clone is needed for this
    // serialization boundary.
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
                form.to_value(&rules).map_err(invalid_cast_undo_rules)?,
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card_printing::CardPrinting;
    use crate::types::display_only::DisplayOnly;
    use crate::types::game_state::{SpellCastReversal, SpellCastUndo, ZoneChangeRecord};
    use crate::types::identifiers::CardId;

    fn printing(tag: &str) -> CardPrinting {
        CardPrinting {
            set_code: tag.into(),
            collector_number: "1".into(),
        }
    }

    fn stamp(state: &mut GameState, id: ObjectId, tag: &str) {
        let object = state.objects.get_mut(&id).expect("object exists");
        object.printing = DisplayOnly::new(Some(printing(tag)));
        object.base_printing = DisplayOnly::new(Some(printing(tag)));
    }

    fn conceal(state: &mut GameState, id: ObjectId) {
        state
            .objects
            .get_mut(&id)
            .expect("object exists")
            .conceal_printing();
    }

    /// Two battlefield objects: X carries `x_tag`, Y carries a fixed printing.
    fn board(x_tag: &str) -> (GameState, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let x = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Object X".into(),
            Zone::Battlefield,
        );
        let y = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Object Y".into(),
            Zone::Battlefield,
        );
        stamp(&mut state, x, x_tag);
        stamp(&mut state, y, "SYYY");
        (state, x, y)
    }

    fn with_descriptor(
        state: &mut GameState,
        object_id: ObjectId,
        comparison_undo: Option<Vec<SpellCastUndo>>,
    ) {
        let mut reversal = SpellCastReversal::announced(object_id, PlayerId(0), 0);
        reversal.comparison_undo = comparison_undo;
        reversal.cost_mutations = true;
        state.spell_cast_reversal = Some(Arc::new(reversal));
    }

    fn edit_path(edit: &SpellCastUndo) -> &[String] {
        match edit {
            SpellCastUndo::Value { path, .. }
            | SpellCastUndo::Splice { path, .. }
            | SpellCastUndo::Hand { path, .. } => path,
        }
    }

    fn object_field(id: ObjectId, field: &str) -> Vec<String> {
        vec!["objects".into(), id.0.to_string(), field.into()]
    }

    fn holds_edit(list: &[SpellCastUndo], path: &[String]) -> bool {
        list.iter().any(|edit| edit_path(edit) == path)
    }

    fn holds_printing_of(list: &[SpellCastUndo], id: ObjectId) -> bool {
        holds_edit(list, &object_field(id, "printing"))
            || holds_edit(list, &object_field(id, "base_printing"))
    }

    fn holds_any_printing(list: &[SpellCastUndo]) -> bool {
        list.iter().any(|edit| {
            edit_path(edit)
                .iter()
                .any(|part| part == "printing" || part == "base_printing")
        })
    }

    fn without_printing_of(list: &[SpellCastUndo], id: ObjectId) -> Vec<SpellCastUndo> {
        list.iter()
            .filter(|edit| {
                edit_path(edit) != object_field(id, "printing")
                    && edit_path(edit) != object_field(id, "base_printing")
            })
            .cloned()
            .collect()
    }

    fn captures(state: &GameState) -> CastUndoValues {
        CastUndoValues::capture(state, ComparisonIdentity::Projected).expect("capture")
    }

    fn direct(before: &GameState, after: &GameState) -> CastUndoLists {
        CastUndoLists::between(&captures(before), &captures(after))
    }

    fn spliced_at(list: &[SpellCastUndo]) -> usize {
        match list {
            [SpellCastUndo::Splice { index, .. }] => *index,
            other => panic!("expected exactly one splice, got {other:?}"),
        }
    }

    /// Cast-start flow over `board(x_tag)`: a parent choice changes life and
    /// conceals Y before the cast start; the cost then changes life and conceals X.
    struct CastStartFlow {
        boundary_lists: CastUndoLists,
        reversal: Arc<SpellCastReversal>,
        expected: CastUndoLists,
        x: ObjectId,
        y: ObjectId,
    }

    fn cast_start_flow(x_tag: &str) -> CastStartFlow {
        let (action_boundary, x, y) = board(x_tag);
        let mut cast_start = action_boundary.clone();
        cast_start.players[0].life -= 1;
        conceal(&mut cast_start, y);

        let guard = CastUndoBoundaryGuard::enter(Arc::new(action_boundary.clone()));
        remember_cast_start(&cast_start, x).expect("cast start recorded");
        let boundary = guard.take().expect("the cast start is retained");

        let mut now = cast_start.clone();
        now.players[0].life -= 3;
        conceal(&mut now, x);
        with_descriptor(&mut now, x, Some(Vec::new()));
        record_spell_cast_undo(&mut now, &action_boundary, 0, Some(&boundary)).expect("recorded");

        CastStartFlow {
            boundary_lists: boundary.from_action_boundary,
            reversal: now.spell_cast_reversal.clone().expect("descriptor kept"),
            expected: direct(&cast_start, &now),
            x,
            y,
        }
    }

    // CR 733.1 + CR 104.4b: the cast-start boundary keeps both inverses, so a
    // printing concealed before the cast start folds out of both lists and the
    // comparison list never holds a printing.
    #[test]
    fn cast_start_boundary_records_both_lists_in_lockstep() {
        let a = cast_start_flow("SAAA");
        let b = cast_start_flow("SBBB");

        // Reach: the boundary holds Y's concealment in the restoration list only.
        assert!(holds_printing_of(&a.boundary_lists.restoration, a.y));
        let boundary_comparison = a
            .boundary_lists
            .comparison
            .as_deref()
            .expect("a fresh boundary keeps a comparison list");
        assert!(!boundary_comparison.is_empty(), "the life edit is retained");
        assert!(!holds_any_printing(boundary_comparison));

        // Both final lists are the direct diffs of the cast start against now.
        assert_eq!(a.reversal.undo, a.expected.restoration);
        assert_eq!(a.reversal.comparison_undo, a.expected.comparison);
        let comparison = a.reversal.comparison_undo.as_deref().expect("comparison");
        assert!(holds_edit(
            comparison,
            &["players".into(), "0".into(), "life".into()]
        ));
        assert!(!holds_any_printing(comparison));

        // The concealment before the cast start is folded away; the cost's own is kept.
        assert!(!holds_printing_of(&a.reversal.undo, a.y));
        assert!(holds_edit(&a.reversal.undo, &object_field(a.x, "printing")));
        assert!(holds_edit(
            &a.reversal.undo,
            &object_field(a.x, "base_printing")
        ));

        // The printing-only twin has an equal comparison list and a restoration
        // list that differs by exactly X's two printing edits.
        assert_eq!(a.reversal.comparison_undo, b.reversal.comparison_undo);
        assert_ne!(a.reversal.undo, b.reversal.undo);
        assert_eq!(
            without_printing_of(&a.reversal.undo, a.x),
            without_printing_of(&b.reversal.undo, b.x)
        );
        assert_eq!(a.reversal.undo.len() - 2, {
            without_printing_of(&a.reversal.undo, a.x).len()
        });
    }

    // CR 733.1 + CR 104.4b: composing the prior inverse into the next action's
    // boundary gives the same lists, in both forms, as one direct diff.
    #[test]
    fn folded_inverse_equals_the_direct_inverse_in_both_forms() {
        let (pre, x, _y) = board("SAAA");
        let mut s1 = pre.clone();
        s1.players[0].life -= 2;
        conceal(&mut s1, x);
        with_descriptor(&mut s1, x, Some(Vec::new()));
        record_spell_cast_undo(&mut s1, &pre, 0, None).expect("first action recorded");
        let first = s1.spell_cast_reversal.clone().expect("descriptor");
        assert!(holds_printing_of(&first.undo, x), "reach: s1 concealed X");

        let mut s2 = s1.clone();
        s2.players[1].life -= 2;
        record_spell_cast_undo(&mut s2, &s1, 0, None).expect("second action recorded");
        let folded = s2.spell_cast_reversal.clone().expect("descriptor");

        let expected = direct(&pre, &s2);
        assert_eq!(folded.undo, expected.restoration);
        assert_eq!(folded.comparison_undo, expected.comparison);
        assert_ne!(folded.undo, first.undo, "reach: the fold rewrote the list");
        assert!(holds_printing_of(&folded.undo, x));
        let comparison = folded.comparison_undo.as_deref().expect("comparison");
        assert!(!holds_any_printing(comparison));
    }

    // CR 733.1: a descriptor saved before the comparison list existed gains none
    // through any composition step.
    #[test]
    fn a_legacy_descriptor_gains_no_comparison_list() {
        let (before, x, _y) = board("SAAA");
        let mut state = before.clone();
        state.players[0].life -= 2;
        with_descriptor(&mut state, x, None);
        record_spell_cast_undo(&mut state, &before, 0, None).expect("recorded");
        let reversal = state.spell_cast_reversal.clone().expect("descriptor");
        assert!(
            !reversal.undo.is_empty(),
            "reach: the restoration list was rewritten"
        );
        assert_eq!(reversal.comparison_undo, None);

        let mut values = captures(&state);
        assert!(
            values.rules.is_some(),
            "reach: a projected capture has a rules form"
        );
        values.undo(&[], None).expect("applies");
        assert!(values.rules.is_none());

        let full = captures(&state);
        let legacy = CastUndoValues::capture(&state, ComparisonIdentity::Legacy).expect("capture");
        assert!(legacy.rules.is_none());
        assert_eq!(CastUndoLists::between(&full, &legacy).comparison, None);
        assert_eq!(CastUndoLists::between(&legacy, &full).comparison, None);
        assert_eq!(CastUndoLists::between(&legacy, &legacy).comparison, None);
        assert_eq!(
            CastUndoLists::between(&full, &full).comparison,
            Some(Vec::new())
        );
    }

    fn zone_change_record(state: &GameState, id: ObjectId, tag: &str) -> ZoneChangeRecord {
        let mut object = state.objects.get(&id).expect("object exists").clone();
        object.printing = DisplayOnly::new(Some(printing(tag)));
        object.snapshot_for_zone_change(id, Some(Zone::Battlefield), Zone::Graveyard)
    }

    fn with_records(base: &GameState, records: &[&ZoneChangeRecord]) -> GameState {
        let mut state = base.clone();
        state.zone_changes_this_turn = records.iter().map(|record| (*record).clone()).collect();
        state
    }

    /// Both lists, applied to their own after-capture, reproduce their before-capture.
    fn assert_each_list_is_exact(before: &GameState, after: &GameState) -> CastUndoLists {
        let lists = direct(before, after);
        let mut restored = captures(after);
        restored
            .undo(&lists.restoration, lists.comparison.as_deref())
            .expect("applies");
        let expected = captures(before);
        assert_eq!(restored.exact, expected.exact);
        assert_eq!(restored.rules, expected.rules);
        lists
    }

    // CR 733.1 + CR 104.4b: two records that are equal as rules data but differ in
    // a display-only printing give each form its own splice position, and each list
    // is exact in its own representation.
    #[test]
    fn projected_equal_array_elements_keep_each_list_exact() {
        let (base, x, _y) = board("SAAA");
        let r1 = zone_change_record(&base, x, "S1");
        let r2 = zone_change_record(&base, x, "S2");
        assert!(r1 == r2, "reach: the records are equality-transparent");
        assert_ne!(
            serde_json::to_value(&r1).expect("record serializes"),
            serde_json::to_value(&r2).expect("record serializes"),
            "reach: their exact JSON differs"
        );

        // Removal: [R1, R2] -> [R2].
        let removal = assert_each_list_is_exact(
            &with_records(&base, &[&r1, &r2]),
            &with_records(&base, &[&r2]),
        );
        assert_eq!(spliced_at(&removal.restoration), 0);
        assert_eq!(
            spliced_at(removal.comparison.as_deref().expect("comparison")),
            1
        );

        // Insertion: [R2] -> [R1, R2].
        let insertion = assert_each_list_is_exact(
            &with_records(&base, &[&r2]),
            &with_records(&base, &[&r1, &r2]),
        );
        assert_eq!(spliced_at(&insertion.restoration), 0);
        assert_eq!(
            spliced_at(insertion.comparison.as_deref().expect("comparison")),
            1
        );

        // The removal twin: same splice index, other payload; equal comparison list.
        let twin = assert_each_list_is_exact(
            &with_records(&base, &[&r2, &r1]),
            &with_records(&base, &[&r1]),
        );
        assert_eq!(spliced_at(&twin.restoration), 0);
        assert_ne!(twin.restoration, removal.restoration);
        assert_eq!(twin.comparison, removal.comparison);

        // A rules difference (the record's name) separates the comparison list.
        let mut renamed = r1.clone();
        renamed.name = "Another name".into();
        let rules_twin = assert_each_list_is_exact(
            &with_records(&base, &[&renamed, &r2]),
            &with_records(&base, &[&r2]),
        );
        assert_ne!(rules_twin.comparison, removal.comparison);
    }
}
