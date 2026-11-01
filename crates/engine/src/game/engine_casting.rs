use std::collections::BTreeSet;

use serde::ser::{Impossible, SerializeMap, SerializeStruct};
use serde::{Serialize, Serializer};

use crate::types::ability::{
    AbilityCost, AdditionalCost, BeholdCostAction, TapCreaturesSelectionMode, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CollectEvidenceResume, GameState, PendingCast, PendingManaAbility, PlayerDeckPool,
    SpellCastReversal, SpellCastUndo, WaitingFor,
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
    static CONFIRMED_TAKE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn in_confirmed_take() -> bool {
    CONFIRMED_TAKE.with(|flag| flag.get())
}

/// CR 732.2a + CR 733.1: while a confirmed take replays its recorded choices, the take's own
/// snapshot is the undo authority, so no cast installs a per-cast reversal.
#[must_use]
pub(crate) struct ConfirmedTakeGuard(bool);

impl ConfirmedTakeGuard {
    pub(crate) fn enter() -> Self {
        Self(CONFIRMED_TAKE.with(|flag| flag.replace(true)))
    }
}

impl Drop for ConfirmedTakeGuard {
    fn drop(&mut self) {
        CONFIRMED_TAKE.with(|flag| flag.set(self.0));
    }
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
        let Some(field) = edit.path().first() else {
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
        .get_mut(OBJECTS_KEY)
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
/// is retained in the casting descriptor or written into a save. Only members
/// that can differ from the boundary are serialized (`CastUndoMembers`); the
/// stored inverse is the one the complete-state comparison (`cast_undo_value`)
/// yields.
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
    // Fold the prior inverse into this action's existing boundary. The stored
    // result describes changed state, rather than accumulating a per-click log.
    // A resolution-owned cast instead folds its pre-announcement point.
    let folded: &[SpellCastUndo] = match (cast_start, before.spell_cast_reversal.as_deref()) {
        (Some(boundary), _) => &boundary.from_action_boundary,
        (None, Some(root)) => &root.undo,
        (None, None) => &[],
    };
    let members = CastUndoMembers::changed(before, state, folded);
    let edits = cast_undo_edits(before, state, folded, |side| {
        cast_undo_projected_value(side, &members)
    });
    #[cfg(feature = "test-support")]
    check_cast_undo_reference(state, before, folded, action_start, cast_start, &edits);
    let edits = edits?;
    store_cast_undo(
        std::sync::Arc::make_mut(state.spell_cast_reversal.as_mut().unwrap()),
        action_start,
        edits,
    );
    Ok(())
}

/// CR 733.1: the inverse edits that turn `after` back into `before` once the
/// `folded` inverse of earlier steps is replayed onto `before`. `build` makes
/// both values; the fold and the diff are the same whichever builder is used.
fn cast_undo_edits(
    before: &GameState,
    after: &GameState,
    folded: &[SpellCastUndo],
    build: impl Fn(&GameState) -> Result<serde_json::Value, EngineError>,
) -> Result<Vec<SpellCastUndo>, EngineError> {
    let mut before_value = build(before)?;
    for edit in folded.iter().rev() {
        apply_cast_undo(&mut before_value, edit)?;
    }
    let after_value = build(after)?;
    let mut edits = Vec::new();
    collect_cast_undo(
        Some(&before_value),
        Some(&after_value),
        &mut Vec::new(),
        &mut edits,
    );
    Ok(edits)
}

/// CR 733.1: replace the reversal's inverse with one action's. The journal
/// start moves to that action only while no earlier inverse is retained.
fn store_cast_undo(root: &mut SpellCastReversal, action_start: usize, edits: Vec<SpellCastUndo>) {
    if root.undo.is_empty() {
        root.journal_start = action_start;
    }
    root.undo = edits;
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
    let mut value = serde_json::to_value(state).map_err(cannot_record_cast_undo)?;
    // The descriptor is not part of the compared state. In particular, prior
    // inverse edits must never become input to another inverse. No extra
    // GameState clone is needed for this serialization boundary.
    value
        .as_object_mut()
        .expect("GameState serializes as an object")
        .remove(REVERSAL_KEY);
    carry_object_rules(state, &mut value)?;
    Ok(value)
}

/// Serialized `GameState` key of the object store, a cast-undo value member.
const OBJECTS_KEY: &str = "objects";
/// Serialized `GameState` key of the deck pools, a cast-undo value member.
const DECK_POOLS_KEY: &str = "deck_pools";
/// Serialized `GameState` key of the reversal descriptor, never part of a
/// cast-undo value.
const REVERSAL_KEY: &str = "spell_cast_reversal";

fn cannot_record_cast_undo(error: serde_json::Error) -> EngineError {
    EngineError::InvalidAction(format!("Cannot record cast undo: {error}"))
}

/// CR 733.1: the members of a pair of game states a cast's inverse can need.
/// A member the two states hold in the same allocations serializes identically
/// in both, so leaving it out of both values removes no inverse edit.
#[derive(Debug, PartialEq, Eq)]
struct CastUndoMembers {
    objects: CastUndoObjects,
    deck_pools: CastUndoDeckPools,
}

/// The game objects a cast-undo value serializes.
#[derive(Debug, PartialEq, Eq)]
enum CastUndoObjects {
    /// Every object in the store.
    All,
    /// Only these ids. Every other object is the same allocation in both
    /// states and is named by no folded edit.
    Only(BTreeSet<ObjectId>),
}

/// Whether a cast-undo value serializes the deck pools.
#[derive(Debug, PartialEq, Eq)]
enum CastUndoDeckPools {
    Serialized,
    /// Both states hold every pool in the same storage and no folded edit
    /// addresses the pools.
    Shared,
}

/// Whether one top-level serialized entry belongs to a cast-undo value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryFate {
    Kept,
    Omitted,
}

impl CastUndoObjects {
    /// Adds the object a serialized `objects` key names. A key that names no
    /// object widens the selection to every object.
    fn include_key(&mut self, key: &str) {
        match self {
            Self::All => {}
            Self::Only(ids) => match serde_json::from_str::<ObjectId>(key) {
                Ok(id) => {
                    ids.insert(id);
                }
                Err(_) => *self = Self::All,
            },
        }
    }
}

impl CastUndoMembers {
    /// The members that can differ between `before` and `after` once the
    /// `folded` inverse is replayed onto `before`: everything either state
    /// does not share with the other, plus everything a folded edit addresses.
    fn changed(before: &GameState, after: &GameState, folded: &[SpellCastUndo]) -> Self {
        let mut members = Self {
            objects: CastUndoObjects::Only(before.objects.unshared_ids(&after.objects)),
            deck_pools: if deck_pools_share_storage(&before.deck_pools, &after.deck_pools) {
                CastUndoDeckPools::Shared
            } else {
                CastUndoDeckPools::Serialized
            },
        };
        for edit in folded {
            match edit.path() {
                [member, key, ..] if member == OBJECTS_KEY => members.objects.include_key(key),
                [member, ..] if member == OBJECTS_KEY => members.objects = CastUndoObjects::All,
                [member, ..] if member == DECK_POOLS_KEY => {
                    members.deck_pools = CastUndoDeckPools::Serialized;
                }
                _ => {}
            }
        }
        members
    }

    /// Whether the top-level serialized entry `key` belongs to the value.
    fn fate(&self, key: &str) -> EntryFate {
        match key {
            REVERSAL_KEY => EntryFate::Omitted,
            OBJECTS_KEY => match self.objects {
                CastUndoObjects::All => EntryFate::Kept,
                CastUndoObjects::Only(_) => EntryFate::Omitted,
            },
            DECK_POOLS_KEY => match self.deck_pools {
                CastUndoDeckPools::Serialized => EntryFate::Kept,
                CastUndoDeckPools::Shared => EntryFate::Omitted,
            },
            _ => EntryFate::Kept,
        }
    }
}

/// True when both states hold the same seats' pools, pairwise in the same
/// storage.
fn deck_pools_share_storage(before: &[PlayerDeckPool], after: &[PlayerDeckPool]) -> bool {
    before.len() == after.len()
        && before
            .iter()
            .zip(after)
            .all(|(before, after)| before.shares_storage_with(after))
}

/// CR 733.1: the cast-undo value of `state` restricted to `members`. It equals
/// [`cast_undo_value`] with the omitted members removed and the `Only` objects
/// kept.
fn cast_undo_projected_value(
    state: &GameState,
    members: &CastUndoMembers,
) -> Result<serde_json::Value, EngineError> {
    let mut value = state
        .serialize(CastUndoProjection { members })
        .map_err(cannot_record_cast_undo)?;
    match &members.objects {
        CastUndoObjects::All => {}
        CastUndoObjects::Only(ids) => {
            let objects = crate::types::object_map::serialize_sorted(
                &state.objects.restricted_to(ids),
                serde_json::value::Serializer,
            )
            .map_err(cannot_record_cast_undo)?;
            value
                .as_object_mut()
                .expect("GameState serializes as an object")
                .insert(OBJECTS_KEY.into(), objects);
        }
    }
    carry_object_rules(state, &mut value)?;
    #[cfg(feature = "test-support")]
    super::perf_counters::record_cast_undo_value(
        value
            .get(OBJECTS_KEY)
            .and_then(serde_json::Value::as_object)
            .map_or(0, serde_json::Map::len),
        value
            .get(DECK_POOLS_KEY)
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len),
    );
    Ok(value)
}

/// Serializes a `GameState` as a cast-undo value holding only the top-level
/// entries `members` keeps. Kept entries pass unchanged through serde_json's
/// own map serializer, so they serialize exactly as `serde_json::to_value`
/// serializes them (including flattened members, which arrive through
/// `serialize_entry`).
struct CastUndoProjection<'a> {
    members: &'a CastUndoMembers,
}

/// The top-level entries of a [`CastUndoProjection`].
struct CastUndoEntries<'a> {
    inner: <serde_json::value::Serializer as Serializer>::SerializeMap,
    members: &'a CastUndoMembers,
    /// The fate of the key last passed to `serialize_key`.
    pending: Option<EntryFate>,
}

fn not_a_game_state() -> serde_json::Error {
    serde::ser::Error::custom("a cast-undo value projects a map-shaped game state")
}

impl<'a> Serializer for CastUndoProjection<'a> {
    type Ok = serde_json::Value;
    type Error = serde_json::Error;
    type SerializeSeq = Impossible<serde_json::Value, serde_json::Error>;
    type SerializeTuple = Impossible<serde_json::Value, serde_json::Error>;
    type SerializeTupleStruct = Impossible<serde_json::Value, serde_json::Error>;
    type SerializeTupleVariant = Impossible<serde_json::Value, serde_json::Error>;
    type SerializeMap = CastUndoEntries<'a>;
    type SerializeStruct = CastUndoEntries<'a>;
    type SerializeStructVariant = Impossible<serde_json::Value, serde_json::Error>;

    fn serialize_map(self, len: Option<usize>) -> Result<CastUndoEntries<'a>, serde_json::Error> {
        Ok(CastUndoEntries {
            inner: serde_json::value::Serializer.serialize_map(len)?,
            members: self.members,
            pending: None,
        })
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<CastUndoEntries<'a>, serde_json::Error> {
        self.serialize_map(Some(len))
    }

    fn serialize_bool(self, _: bool) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_i8(self, _: i8) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_i16(self, _: i16) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_i32(self, _: i32) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_i64(self, _: i64) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_u8(self, _: u8) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_u16(self, _: u16) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_u32(self, _: u32) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_u64(self, _: u64) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_f32(self, _: f32) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_f64(self, _: f64) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_char(self, _: char) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_str(self, _: &str) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_bytes(self, _: &[u8]) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_none(self) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_some<T: ?Sized + Serialize>(
        self,
        _: &T,
    ) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_unit(self) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_unit_struct(
        self,
        _: &'static str,
    ) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
    ) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: &T,
    ) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<serde_json::Value, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, serde_json::Error> {
        Err(not_a_game_state())
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, serde_json::Error> {
        Err(not_a_game_state())
    }
}

impl CastUndoEntries<'_> {
    /// The fate of the entry under `key`. Every `GameState` key is a string;
    /// any other key shape is kept, so serde_json treats it as `to_value` does.
    fn fate_of<K: ?Sized + Serialize>(&self, key: &K) -> Result<EntryFate, serde_json::Error> {
        Ok(match key.serialize(serde_json::value::Serializer)? {
            serde_json::Value::String(name) => self.members.fate(&name),
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::Array(_)
            | serde_json::Value::Object(_) => EntryFate::Kept,
        })
    }
}

impl SerializeMap for CastUndoEntries<'_> {
    type Ok = serde_json::Value;
    type Error = serde_json::Error;

    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<(), serde_json::Error> {
        let fate = self.fate_of(key)?;
        self.pending = Some(fate);
        match fate {
            EntryFate::Kept => self.inner.serialize_key(key),
            EntryFate::Omitted => Ok(()),
        }
    }

    fn serialize_value<T: ?Sized + Serialize>(
        &mut self,
        value: &T,
    ) -> Result<(), serde_json::Error> {
        match self.pending.take() {
            Some(EntryFate::Kept) => self.inner.serialize_value(value),
            Some(EntryFate::Omitted) => Ok(()),
            None => Err(serde::ser::Error::custom(
                "a cast-undo value entry arrived without its key",
            )),
        }
    }

    fn serialize_entry<K: ?Sized + Serialize, V: ?Sized + Serialize>(
        &mut self,
        key: &K,
        value: &V,
    ) -> Result<(), serde_json::Error> {
        match self.fate_of(key)? {
            EntryFate::Kept => self.inner.serialize_entry(key, value),
            EntryFate::Omitted => Ok(()),
        }
    }

    fn end(self) -> Result<serde_json::Value, serde_json::Error> {
        SerializeMap::end(self.inner)
    }
}

impl SerializeStruct for CastUndoEntries<'_> {
    type Ok = serde_json::Value;
    type Error = serde_json::Error;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), serde_json::Error> {
        match self.members.fate(key) {
            EntryFate::Kept => SerializeMap::serialize_entry(&mut self.inner, key, value),
            EntryFate::Omitted => Ok(()),
        }
    }

    fn end(self) -> Result<serde_json::Value, serde_json::Error> {
        SerializeMap::end(self.inner)
    }
}

#[cfg(feature = "test-support")]
thread_local! {
    static CAST_UNDO_REFERENCE_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Test support: while held, every cast-undo record on this thread is also
/// computed over the complete serialized states (`cast_undo_value`), and the
/// two `SpellCastReversal`s must be identical; a difference panics with both.
#[cfg(feature = "test-support")]
#[must_use]
pub struct CastUndoReferenceCheck(());

#[cfg(feature = "test-support")]
impl CastUndoReferenceCheck {
    pub fn enable() -> Self {
        CAST_UNDO_REFERENCE_DEPTH.with(|depth| depth.set(depth.get() + 1));
        Self(())
    }
}

#[cfg(feature = "test-support")]
impl Drop for CastUndoReferenceCheck {
    fn drop(&mut self) {
        CAST_UNDO_REFERENCE_DEPTH.with(|depth| depth.set(depth.get() - 1));
    }
}

/// CR 733.1: under [`CastUndoReferenceCheck`], compare the reversal this
/// record stores with the one the complete-state comparison stores.
#[cfg(feature = "test-support")]
fn check_cast_undo_reference(
    state: &GameState,
    before: &GameState,
    folded: &[SpellCastUndo],
    action_start: usize,
    cast_start: Option<&CastStartBoundary>,
    edits: &Result<Vec<SpellCastUndo>, EngineError>,
) {
    if CAST_UNDO_REFERENCE_DEPTH.with(std::cell::Cell::get) == 0 {
        return;
    }
    let reference = cast_undo_edits(before, state, folded, cast_undo_value);
    let root = state
        .spell_cast_reversal
        .as_deref()
        .expect("a recording step owns a reversal");
    let stored = |edits: &Result<Vec<SpellCastUndo>, EngineError>| {
        edits
            .as_ref()
            .map(|edits| {
                let mut reversal = root.clone();
                store_cast_undo(&mut reversal, action_start, edits.clone());
                reversal
            })
            .map_err(ToString::to_string)
    };
    assert_eq!(
        stored(edits),
        stored(&reference),
        "CR 733.1: the changed-member cast-undo record diverged from the complete-state record"
    );
    super::perf_counters::record_cast_undo_reference_check();
    if cast_start.is_some() {
        super::perf_counters::record_cast_undo_cast_start_reference_check();
    }
}

/// Adds each serialized object's `UndecodedObjectRules`, which the ordinary
/// object serializer skips, beside the object (`CAST_UNDO_OBJECT_RULES`).
fn carry_object_rules(state: &GameState, value: &mut serde_json::Value) -> Result<(), EngineError> {
    let objects = value
        .get_mut(OBJECTS_KEY)
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
    Ok(())
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

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_reveal_for_cost(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: PendingCast,
    count: usize,
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    shared_quality: Option<crate::types::ability::SharedQuality>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    casting::handle_reveal_for_cost(
        state,
        player,
        pending_cast,
        count,
        legal_cards,
        chosen,
        shared_quality,
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
    use super::*;
    use crate::types::object_map::sweep_test_support::state_with_objects;
    use std::sync::Arc;

    /// An inverse edit restoring `id`'s untapped status.
    fn untap_edit(id: ObjectId) -> SpellCastUndo {
        SpellCastUndo::Value {
            path: vec![OBJECTS_KEY.into(), id.0.to_string(), "tapped".into()],
            before: Some(serde_json::json!(false)),
        }
    }

    /// True when an edit addresses the object `id`.
    fn names_object(edits: &[SpellCastUndo], id: ObjectId) -> bool {
        let key = id.0.to_string();
        edits.iter().any(|edit| {
            matches!(edit.path(), [member, named, ..] if member == OBJECTS_KEY && *named == key)
        })
    }

    /// Objects the step wrote, plus shared objects a folded edit names, are
    /// serialized; a shared object nothing names is not.
    #[test]
    fn changed_members_name_objects_a_folded_edit_addresses() {
        let (before, ids) = state_with_objects(&[(PlayerId(0), Zone::Battlefield); 3]);
        let (written, folded, untouched) = (ids[0], ids[1], ids[2]);
        let mut after = before.clone();
        after
            .objects
            .get_mut(&written)
            .expect("fixture object")
            .tapped = true;
        assert!(before.objects.shares_object_with(&after.objects, folded));
        assert!(before.objects.shares_object_with(&after.objects, untouched));

        let members = CastUndoMembers::changed(&before, &after, &[untap_edit(folded)]);

        assert_eq!(
            members.objects,
            CastUndoObjects::Only(BTreeSet::from([written, folded]))
        );
        assert_eq!(members.fate(OBJECTS_KEY), EntryFate::Omitted);
        assert_eq!(members.fate(REVERSAL_KEY), EntryFate::Omitted);
        assert_eq!(members.fate("players"), EntryFate::Kept);
    }

    /// A folded edit that addresses the whole store, or a key that names no
    /// object, makes every object part of the value.
    #[test]
    fn changed_members_fall_back_to_every_object_for_an_unresolvable_objects_path() {
        let (before, ids) = state_with_objects(&[(PlayerId(0), Zone::Battlefield); 2]);
        let after = before.clone();
        let whole_store = SpellCastUndo::Value {
            path: vec![OBJECTS_KEY.into()],
            before: Some(serde_json::json!({})),
        };
        let not_an_id = SpellCastUndo::Value {
            path: vec![OBJECTS_KEY.into(), "not-an-id".into(), "x".into()],
            before: None,
        };
        assert_eq!(
            CastUndoMembers::changed(&before, &after, &[]).objects,
            CastUndoObjects::Only(BTreeSet::new())
        );

        let whole = CastUndoMembers::changed(&before, &after, &[whole_store]);
        let unnamed = CastUndoMembers::changed(&before, &after, &[not_an_id, untap_edit(ids[0])]);

        assert_eq!(whole.objects, CastUndoObjects::All);
        assert_eq!(unnamed.objects, CastUndoObjects::All);
        assert_eq!(whole.fate(OBJECTS_KEY), EntryFate::Kept);
    }

    /// Deck pools are left out only while both states share their storage and
    /// no folded edit addresses them; equal content is not enough.
    #[test]
    fn changed_members_serialize_deck_pools_only_when_written_or_folded() {
        let mut before = GameState::new_two_player(42);
        before.deck_pools = vec![
            PlayerDeckPool {
                player: PlayerId(0),
                ..Default::default()
            },
            PlayerDeckPool {
                player: PlayerId(1),
                ..Default::default()
            },
        ];
        let shared = before.clone();
        let mut written = before.clone();
        Arc::make_mut(&mut written.deck_pools[1].current_main);
        assert_eq!(written.deck_pools, before.deck_pools);
        let mut fewer = before.clone();
        fewer.deck_pools.pop();
        let pool_edit = SpellCastUndo::Value {
            path: vec![DECK_POOLS_KEY.into(), "0".into(), "player".into()],
            before: Some(serde_json::json!(0)),
        };

        let unchanged = CastUndoMembers::changed(&before, &shared, &[]);
        assert_eq!(unchanged.deck_pools, CastUndoDeckPools::Shared);
        assert_eq!(unchanged.fate(DECK_POOLS_KEY), EntryFate::Omitted);
        assert_eq!(
            CastUndoMembers::changed(&before, &written, &[]).deck_pools,
            CastUndoDeckPools::Serialized
        );
        assert_eq!(
            CastUndoMembers::changed(&before, &fewer, &[]).deck_pools,
            CastUndoDeckPools::Serialized
        );
        let folded = CastUndoMembers::changed(&before, &shared, &[pool_edit]);
        assert_eq!(folded.deck_pools, CastUndoDeckPools::Serialized);
        assert_eq!(folded.fate(DECK_POOLS_KEY), EntryFate::Kept);
    }

    /// The projection keeps every kept top-level entry exactly as
    /// `serde_json::to_value` serializes it, flattened members included, and
    /// leaves out exactly the omitted ones.
    #[test]
    fn a_complete_projection_matches_the_complete_cast_undo_value() {
        let (mut state, ids) = state_with_objects(&[
            (PlayerId(0), Zone::Battlefield),
            (PlayerId(1), Zone::Hand),
            (PlayerId(0), Zone::Graveyard),
        ]);
        state.product_knowledge_state.library_epochs.push(3);
        state.deck_pools = vec![PlayerDeckPool {
            player: PlayerId(0),
            ..Default::default()
        }];
        state.spell_cast_reversal = Some(Arc::new(SpellCastReversal {
            object_id: ids[0],
            player: PlayerId(0),
            journal_start: 0,
            undo: vec![untap_edit(ids[0])],
            cost_mutations: true,
            library_undo_forbidden: false,
        }));
        let complete = cast_undo_value(&state).expect("the complete value builds");
        assert!(serde_json::to_value(&state)
            .expect("the state serializes")
            .get(REVERSAL_KEY)
            .is_some());
        assert!(complete.get(REVERSAL_KEY).is_none());
        assert!(complete.get("library_knowledge_epochs").is_some());
        assert_eq!(
            complete[OBJECTS_KEY].as_object().map(serde_json::Map::len),
            Some(3)
        );
        assert_eq!(complete[DECK_POOLS_KEY].as_array().map(Vec::len), Some(1));

        let every_member = CastUndoMembers {
            objects: CastUndoObjects::All,
            deck_pools: CastUndoDeckPools::Serialized,
        };
        assert_eq!(
            cast_undo_projected_value(&state, &every_member).expect("the projection builds"),
            complete
        );

        let kept = ids[1].0.to_string();
        let changed_members = CastUndoMembers {
            objects: CastUndoObjects::Only(BTreeSet::from([ids[1]])),
            deck_pools: CastUndoDeckPools::Shared,
        };
        let mut expected = complete.clone();
        let fields = expected.as_object_mut().expect("the value is an object");
        fields.remove(DECK_POOLS_KEY);
        fields
            .get_mut(OBJECTS_KEY)
            .and_then(serde_json::Value::as_object_mut)
            .expect("the value holds objects")
            .retain(|key, _| *key == kept);
        assert_eq!(
            cast_undo_projected_value(&state, &changed_members).expect("the projection builds"),
            expected
        );
    }

    /// CR 733.1: an object an earlier step wrote is shared by this step's
    /// boundary and result, yet its folded inverse must survive. The
    /// changed-member edits equal the complete ones; identity alone does not.
    #[test]
    fn changed_member_edits_equal_complete_edits_with_a_folded_shared_object() {
        let (start, ids) = state_with_objects(&[(PlayerId(0), Zone::Battlefield); 3]);
        let (paid, written) = (ids[0], ids[1]);
        let mut boundary = start.clone();
        boundary
            .objects
            .get_mut(&paid)
            .expect("fixture object")
            .tapped = true;
        let folded = cast_undo_edits(&start, &boundary, &[], cast_undo_value)
            .expect("the earlier step records");
        assert!(names_object(&folded, paid));
        let mut after = boundary.clone();
        after
            .objects
            .get_mut(&written)
            .expect("fixture object")
            .tapped = true;
        assert!(boundary.objects.shares_object_with(&after.objects, paid));

        let members = CastUndoMembers::changed(&boundary, &after, &folded);
        let changed = cast_undo_edits(&boundary, &after, &folded, |side| {
            cast_undo_projected_value(side, &members)
        })
        .expect("the changed-member edits build");
        let complete = cast_undo_edits(&boundary, &after, &folded, cast_undo_value)
            .expect("the complete edits build");

        assert_eq!(changed, complete);
        assert!(names_object(&complete, paid));
        assert!(names_object(&changed, paid));
        assert!(names_object(&complete, written));

        let identity_only = CastUndoMembers {
            objects: CastUndoObjects::Only(boundary.objects.unshared_ids(&after.objects)),
            deck_pools: CastUndoDeckPools::Shared,
        };
        let without_fold = cast_undo_edits(&boundary, &after, &folded, |side| {
            cast_undo_projected_value(side, &identity_only)
        });
        assert_ne!(without_fold.ok(), Some(complete));
    }

    /// CR 733.1: a step that removes one object and adds another, after an
    /// earlier step's folded edit named an object absent from both sides of
    /// it. This reaches the one-sided arms of `ObjectMap::unshared_ids` and
    /// `ObjectMap::restricted_to`'s skip of absent ids together, and the
    /// changed-member edits still equal the complete ones.
    #[test]
    fn changed_member_edits_equal_complete_edits_across_added_and_removed_objects() {
        let (start, ids) = state_with_objects(&[(PlayerId(0), Zone::Battlefield); 3]);
        let (gone_earlier, removed, kept) = (ids[0], ids[1], ids[2]);
        let mut boundary = start.clone();
        boundary
            .objects
            .remove(&gone_earlier)
            .expect("fixture object");
        let folded = cast_undo_edits(&start, &boundary, &[], cast_undo_value)
            .expect("the earlier step records");
        assert!(
            names_object(&folded, gone_earlier),
            "reach-guard: the fold names an object neither side of the next step holds"
        );

        let mut after = boundary.clone();
        let mut moved = after.objects.remove(&removed).expect("fixture object");
        let added = ObjectId(ids.iter().map(|id| id.0).max().expect("fixture ids") + 1);
        moved.id = added;
        after.objects.insert(added, moved);

        let members = CastUndoMembers::changed(&boundary, &after, &folded);
        let changed = cast_undo_edits(&boundary, &after, &folded, |side| {
            cast_undo_projected_value(side, &members)
        })
        .expect("the changed-member edits build");
        let complete = cast_undo_edits(&boundary, &after, &folded, cast_undo_value)
            .expect("the complete edits build");

        assert_eq!(changed, complete);
        assert!(
            names_object(&complete, removed),
            "reach-guard: the removal is recorded"
        );
        assert!(
            names_object(&complete, added),
            "reach-guard: the addition is recorded"
        );
        assert!(!names_object(&complete, kept));
    }
}
