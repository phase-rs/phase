use crate::types::ability::{
    AbilityKind, ContinuousModification, CopyCountStatus, DetachedRemainder, Duration, Effect,
    EffectKind, FilterProp, KeywordAction, ObjectScope, PlayerFilter, QuantityExpr, QuantityRef,
    ResolvedAbility, SiblingCondition, SpellContext, SubAbilityLink, TargetChoiceTiming,
    TargetFilter, TargetRef, TargetSelectionMode, TriggerCondition,
};
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    AutoMayChoice, CastOfferKind, CastingVariant, ExileLink, ExileLinkKind, GameState,
    MayTriggerAutoChoiceKey, MayTriggerOrigin, PendingCounterPostAction, PendingSpellResolution,
    StackEntry, StackEntryKind, StackPaidSnapshot, TriggerSourceContext, WaitingFor,
};
use crate::types::identifiers::{ObjectId, TriggerFiring};
use crate::types::player::PlayerId;
use crate::types::resolved_commands::{
    ResolvedStackEntryFinalizeCommand, ResolvedStackEntryFinalizeReplayInvariantError,
    ResolvedStackPushCommand, ResolvedStackPushOrigin, ResolvedStackPushReplayInvariantError,
    ResolvedStackRemovalCommand, ResolvedStackRemovalReplayInvariantError,
    ResolvedUncommittedTriggerRemovalCommand,
    ResolvedUncommittedTriggerRemovalReplayInvariantError,
};
use crate::types::zones::Zone;

use super::ability_utils::{
    build_target_slots, flatten_targets_in_chain, validate_targets_in_chain,
};
use super::effects;
use super::targeting;
use super::zone_pipeline::{self, ZoneMoveRequest, ZoneMoveResult};

/// Transfers an already-popped stack entry into the active resolution carrier.
pub(super) fn begin_resolving_stack_entry(
    state: &mut GameState,
    entry: StackEntry,
    firing: Option<TriggerFiring>,
) {
    debug_assert!(state.resolving_stack_entry.is_none());
    debug_assert!(state.resolving_trigger_firing.is_none());
    debug_assert_eq!(
        matches!(&entry.kind, StackEntryKind::TriggeredAbility { .. }),
        firing.is_some()
    );
    state.resolving_stack_entry = Some(entry);
    state.resolving_trigger_firing = firing;
}

/// Settles the active resolution carrier after its owning resolution completes.
pub(super) fn finish_resolving_stack_entry(
    state: &mut GameState,
    disposition: super::lifecycle::DelayedTerminalDisposition,
) {
    let entry = state.resolving_stack_entry.take();
    let firing = state.resolving_trigger_firing.take();
    debug_assert!(
        firing.is_none()
            || entry
                .is_some_and(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
    );
    if let Some(firing) = firing {
        super::lifecycle::record_delayed_terminal(firing, disposition);
    }
}

/// CR 405.1: Add an object to the stack.
pub fn push_to_stack(state: &mut GameState, entry: StackEntry, events: &mut Vec<GameEvent>) {
    let trigger_firing = matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        .then_some(TriggerFiring::Ordinary);
    push_to_stack_with_firing(state, entry, trigger_firing, events);
}

/// Push a scheduler-owned triggered ability with its exact firing class.
pub(crate) fn push_triggered_to_stack(
    state: &mut GameState,
    entry: StackEntry,
    firing: TriggerFiring,
    events: &mut Vec<GameEvent>,
) {
    debug_assert!(matches!(
        entry.kind,
        StackEntryKind::TriggeredAbility { .. }
    ));
    push_to_stack_with_firing(state, entry, Some(firing), events);
}

fn push_to_stack_with_firing(
    state: &mut GameState,
    mut entry: StackEntry,
    trigger_firing: Option<TriggerFiring>,
    events: &mut Vec<GameEvent>,
) {
    let source_ref = state
        .objects
        .get(&entry.source_id)
        .map(crate::types::identifiers::ObjectIncarnationRef::from_object);
    // CR 701.27f: an activated or triggered ability of a permanent may
    // transform that permanent only if it has not transformed/converted since
    // the ability was put onto the stack. Spells and keyword actions do not
    // receive this guard.
    if matches!(
        entry.kind,
        StackEntryKind::ActivatedAbility { .. } | StackEntryKind::TriggeredAbility { .. }
    ) {
        if let Some(ability) = entry.ability_mut() {
            // CR 400.7 + CR 113.7a: Capture the source incarnation for every
            // activated or triggered ability, including non-transforming
            // permanents. The transformation guard below has a narrower scope.
            if ability.source_incarnation.is_none() {
                ability
                    .set_source_incarnation_recursive(source_ref.map(|source| source.incarnation));
            }
        }

        let source = state
            .objects
            .get(&entry.source_id)
            .filter(|object| object.back_face.is_some());
        let count = source.map(|object| object.transformation_count);
        if let Some(ability) = entry.ability_mut() {
            // CR 701.27f: delayed triggered abilities already carry their
            // creation-time generation and must not be restamped when fired.
            if ability.context.source_transformation_count.is_none() {
                ability.set_source_transformation_count_recursive(count);
            }
        }
    }
    // CR 400.7 + CR 509.1c: source-referential force-block instructions bind
    // their exact source at the common stack boundary, covering activated and
    // other nontriggered stack abilities as well as normal triggered paths.
    if let Some(ability) = entry.ability_mut() {
        ability.bind_force_block_source_recursive(source_ref);
    }
    events.push(GameEvent::StackPushed {
        object_id: entry.id,
    });
    // CR 733: journal the settled push after every source-referential stamp
    // above has been written into the entry, so the record carries the stamped
    // values themselves rather than the state they were derived from.
    journal_stack_push(state, &entry, trigger_firing, ResolvedStackPushOrigin::Put);
    if let Some(firing) = trigger_firing {
        state.stack_trigger_firings.insert(entry.id, firing);
    }
    state.stack.push_back(entry);
}

/// CR 707.10: Put a *copy* of a spell or ability onto the stack.
///
/// This is the copy-family sibling of [`push_to_stack`], not a wrapper around
/// it. Putting an object onto the stack (CR 405.1 / CR 601.2a) and copying one
/// onto it (CR 707.10) agree that exactly one `StackPushed` is emitted, but
/// they disagree on the two source-referential stamps, so they are separate
/// authorities rather than one funnel:
///
/// * CR 701.27f generation — a copy captures the source's generation at
///   *copy-creation* time and must overwrite whatever the copied ability
///   inherited from the original's earlier stack push. [`push_to_stack`]
///   deliberately guards its stamp with `is_none()` (delayed triggered
///   abilities carry a creation-time generation that firing must not clobber);
///   applying that guard here would leave the copy comparing against the
///   *original's* generation instead of its own.
/// * Force-block binding — deliberately NOT re-bound here. A copied ability
///   already carries the `force_block_attacker` its original was bound to
///   (`ResolvedAbility` is cloned wholesale by the copy effects), and that
///   binding came from the trigger's captured `trigger_source` provenance in
///   `triggers::bind_force_block_attacker_recursive`. Calling
///   `bind_force_block_source_recursive` here would overwrite that exact
///   choice-time referent with a fresh live `state.objects` lookup — a global
///   rescan where CR 707.10b says the copy has the same source as the original.
///   Do not add it.
pub(crate) fn push_copy_to_stack(
    state: &mut GameState,
    mut entry: StackEntry,
    copied_trigger_firing: Option<TriggerFiring>,
    events: &mut Vec<GameEvent>,
) {
    // CR 701.27f: an activated or triggered ability of a permanent may transform
    // that permanent only if it hasn't transformed since the ability was put
    // onto the stack. Copying such an ability puts a NEW ability onto the stack,
    // so the copy compares against the source at copy-creation time rather than
    // the original ability's earlier stack-entry time. Spell copies are outside
    // the rule entirely — CR 701.27f covers only "an activated or triggered
    // ability of a permanent" — so this stamp is inert for spell-copy callers.
    if matches!(
        entry.kind,
        StackEntryKind::ActivatedAbility { .. } | StackEntryKind::TriggeredAbility { .. }
    ) {
        let count = state
            .objects
            .get(&entry.source_id)
            .filter(|object| object.back_face.is_some())
            .map(|object| object.transformation_count);
        if let Some(ability) = entry.ability_mut() {
            ability.set_source_transformation_count_recursive(count);
        }
    }
    events.push(GameEvent::StackPushed {
        object_id: entry.id,
    });
    // CR 733: same journal point as [`push_to_stack`]. The copy's deliberately
    // different stamping is already baked into `entry`, so this records the same
    // operand set under a different origin rather than a sibling command.
    let trigger_firing = matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        .then_some(copied_trigger_firing.unwrap_or(TriggerFiring::Ordinary));
    journal_stack_push(state, &entry, trigger_firing, ResolvedStackPushOrigin::Copy);
    if let Some(firing) = trigger_firing {
        state.stack_trigger_firings.insert(entry.id, firing);
    }
    state.stack.push_back(entry);
}

/// Records one settled stack push for both stack authorities.
///
/// CR 405.2: an object goes on top of everything already on the stack, so the
/// index it will occupy is the current depth. Reading that here — before either
/// caller's `push_back` — is the one piece of shared logic the two authorities
/// could otherwise get out of step on.
fn journal_stack_push(
    state: &mut GameState,
    entry: &StackEntry,
    trigger_firing: Option<TriggerFiring>,
    origin: ResolvedStackPushOrigin,
) {
    let resulting_position = state.stack.len();
    let cause = state.current_or_begin_rules_execution_node();
    let command = ResolvedStackPushCommand {
        entry: Box::new(entry.clone()),
        trigger_firing,
        origin,
        resulting_position,
        cause,
    };
    state
        .resolved_rules_journal
        .record_stack_push(command)
        .expect("resolved stack push must have a live journal cause");
}

/// Installs one already-resolved stack push verbatim.
///
/// CR 405.1 / CR 707.10: the recorded entry is pushed exactly as it was
/// recorded. Nothing is restamped — the CR 701.27f generation, the CR 400.7
/// incarnation, and the CR 509.1c force-block referent all travel inside the
/// entry, so replay never repeats the live `state.objects` lookups that produced
/// them. Re-deriving the force-block binding in particular would swap a
/// choice-time referent for a global rescan (see [`push_copy_to_stack`]).
///
/// Deliberately does NOT require the source object to exist: the ordinary path
/// tolerates a missing source (synthetic game-rule triggers push with
/// `ObjectId(0)`), so a source-existence precondition would reject pushes the
/// engine legitimately performs.
///
/// `StackDepthMismatch` is a deliberate fail-closed canary, not a bug to route
/// around. Stack POPS are not journaled yet (CR 608.1 resolve-pop, CR 603.3c/d
/// abort-pop, CR 701.6a counter-removal, CR 601.2a cast-abort), so once any
/// un-journaled removal runs, the replayed depth diverges from every later
/// recorded position and this check refuses instead of installing an entry
/// somewhere the recording never described. Do not weaken it to make a replay
/// pass; see [`ResolvedStackPushCommand`] for the scheduled gap.
pub fn apply_resolved_stack_push(
    state: &mut GameState,
    command: &ResolvedStackPushCommand,
) -> Result<(), ResolvedStackPushReplayInvariantError> {
    if matches!(command.entry.kind, StackEntryKind::TriggeredAbility { .. })
        != command.trigger_firing.is_some()
    {
        return Err(ResolvedStackPushReplayInvariantError::TriggerFiringShapeMismatch);
    }
    if state.stack.len() != command.resulting_position {
        return Err(ResolvedStackPushReplayInvariantError::StackDepthMismatch {
            expected: command.resulting_position,
            found: state.stack.len(),
        });
    }
    if state.stack.iter().any(|entry| entry.id == command.entry.id) {
        return Err(ResolvedStackPushReplayInvariantError::DuplicateStackEntry(
            command.entry.id,
        ));
    }
    if !state
        .players
        .iter()
        .any(|player| player.id == command.entry.controller)
    {
        return Err(ResolvedStackPushReplayInvariantError::UnknownController(
            command.entry.controller,
        ));
    }

    state.stack.push_back(command.entry.as_ref().clone());
    if let Some(firing) = command.trigger_firing {
        state.stack_trigger_firings.insert(command.entry.id, firing);
    }
    Ok(())
}

/// Installs one already-resolved CR 601.2i cast finalization verbatim.
///
/// CR 601.2i: the recorded finalized entry and paid-facts snapshot are written
/// back exactly as they were recorded. Nothing is re-derived — in particular the
/// entry is located by its recorded CR 405.2 position rather than by repeating
/// the authority's last-match scan, and `distinct_colors_spent` is taken from
/// the snapshot rather than re-read from the object's `colors_spent_to_cast`,
/// which a replayed predecessor need not still agree with.
///
/// Fails closed on any disagreement with the predecessor state: a position past
/// the stack depth, a different entry at that position, a pre-finalize entry
/// that is not the one recorded, or different pre-existing paid facts.
pub fn apply_resolved_stack_entry_finalize(
    state: &mut GameState,
    command: &ResolvedStackEntryFinalizeCommand,
) -> Result<(), ResolvedStackEntryFinalizeReplayInvariantError> {
    let depth = state.stack.len();
    let entry = state.stack.get(command.entry_position).ok_or(
        ResolvedStackEntryFinalizeReplayInvariantError::PositionOutOfRange {
            position: command.entry_position,
            depth,
        },
    )?;
    if entry.id != command.object {
        return Err(
            ResolvedStackEntryFinalizeReplayInvariantError::EntryIdentityMismatch {
                position: command.entry_position,
                expected: command.object,
                found: entry.id,
            },
        );
    }
    if entry.kind != *command.expected_old_kind {
        return Err(
            ResolvedStackEntryFinalizeReplayInvariantError::EntryKindMismatch(
                command.entry_position,
            ),
        );
    }
    // CR 601.2i settles the entry retag and the paid-facts snapshot together, so
    // the snapshot precondition is checked BEFORE either is installed — a replay
    // must not leave a finalized entry behind when the snapshot side is the half
    // that disagrees.
    if state.stack_paid_facts.get(&command.object) != command.expected_old_paid_facts.as_deref() {
        return Err(
            ResolvedStackEntryFinalizeReplayInvariantError::PaidFactsMismatch(command.object),
        );
    }

    state
        .stack
        .get_mut(command.entry_position)
        .expect("the entry was just read at this position")
        .kind = command.resulting_kind.as_ref().clone();
    state.stack_paid_facts.insert(
        command.object,
        command.resulting_paid_facts.as_ref().clone(),
    );
    Ok(())
}

/// Everything one stack removal settles: the entry plus the per-entry side-table
/// rows keyed on it.
///
/// The rows are returned rather than discarded because the resolution pop
/// consumes both — the paid snapshot feeds cost-dependent resolution and the
/// batch feeds CR 603.7c event context. Callers that only need the entry drop
/// the rest.
pub(crate) struct PoppedStackEntry {
    pub entry: StackEntry,
    pub paid_facts: Option<StackPaidSnapshot>,
    pub trigger_event_batch: Option<Vec<GameEvent>>,
    pub trigger_firing: Option<TriggerFiring>,
}

/// Takes the firing classification coupled to a stack entry.
///
/// Current scheduler pushes always install a row for triggered entries. Older
/// persisted states and direct fixture construction can lack that row, in which
/// case the canonical unknown-legacy form preserves the pair without inventing
/// a receipt-eligible delayed identity.
fn take_stack_trigger_firing(state: &mut GameState, entry: &StackEntry) -> Option<TriggerFiring> {
    let firing = state.stack_trigger_firings.remove(&entry.id);
    if matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }) {
        Some(firing.unwrap_or(TriggerFiring::UnknownLegacy))
    } else {
        debug_assert!(firing.is_none());
        None
    }
}

/// CR 405.2: removes one object from the stack at a known index.
///
/// The single authority for every one-entry stack removal — the CR 405.5
/// resolution pop and the drain loops (batched resolution, inert no-op batches,
/// CR 724.1b stack exile) via [`pop_top_stack_entry`], the CR 701.6a counter,
/// and the CR 601.2a / CR 601.2i cast rollbacks. Each call removes exactly one
/// object, so a drain of N entries is N removals rather than one bulk mutation.
///
/// Both side tables are dropped here rather than by the callers because they are
/// keyed on the entry and settle WITH the removal: dropping the entry but
/// leaving `stack_paid_facts` or `stack_trigger_event_batches` behind would
/// strand rows against an id no longer on the stack. The counter and rollback
/// sites previously dropped only `stack_paid_facts` (and the CR 601.2a reject
/// dropped neither), so routing them here also closes those leaks.
///
/// NOT used by [`pop_uncommitted_pending_trigger_entry`], which performs the
/// same mutation. That is deliberate: the CR 603.3d removal is a distinct family
/// with its own record, and routing it through this authority would journal one
/// mutation twice, so a replay would remove two entries where execution removed
/// one.
fn remove_stack_entry_at_unobserved(
    state: &mut GameState,
    index: usize,
) -> Option<PoppedStackEntry> {
    // `im::Vector::remove` panics out of range rather than returning `Option`,
    // so the bound is checked here rather than leaned on.
    if index >= state.stack.len() {
        return None;
    }
    let entry = state.stack.remove(index);
    let paid_facts = state.stack_paid_facts.remove(&entry.id);
    let trigger_event_batch = state.stack_trigger_event_batches.remove(&entry.id);
    let trigger_firing = take_stack_trigger_firing(state, &entry);

    // CR 733: journal once ALL THREE removals have settled, so the record
    // describes a stack the entry has already left. An out-of-range index is the
    // one case that journals nothing — `?` returns above, because no mutation
    // happened at all.
    let cause = state.current_or_begin_rules_execution_node();
    state
        .resolved_rules_journal
        .record_stack_removal(ResolvedStackRemovalCommand {
            entry: Box::new(entry.clone()),
            index,
            resulting_depth: state.stack.len(),
            cause,
        })
        .expect("resolved stack removal must have a live journal cause");

    Some(PoppedStackEntry {
        entry,
        paid_facts,
        trigger_event_batch,
        trigger_firing,
    })
}

/// Removes a stack entry for a non-resolution reason and observes the exact
/// firing only after the entry and side tables have been settled.
pub(super) fn remove_nonresolving_stack_entry_at(
    state: &mut GameState,
    index: usize,
    disposition: super::lifecycle::DelayedTerminalDisposition,
) -> Option<PoppedStackEntry> {
    let popped = remove_stack_entry_at_unobserved(state, index)?;
    if let Some(firing) = popped.trigger_firing {
        super::lifecycle::record_delayed_terminal(firing, disposition);
    }
    Some(popped)
}

/// CR 405.2: removes the topmost object from the stack.
///
/// A thin wrapper over [`remove_stack_entry_at_unobserved`] — the top of an N-deep stack is
/// index N-1 — kept because the resolution and drain callers have no index to
/// pass and reading `remove_stack_entry_at_unobserved(state, state.stack.len() - 1)` at
/// each of them would obscure that they are simply resolving the top object.
pub(crate) fn pop_top_stack_entry(state: &mut GameState) -> Option<PoppedStackEntry> {
    remove_stack_entry_at_unobserved(state, state.stack.len().checked_sub(1)?)
}

/// Removes the top stack entry outside normal resolution.
pub(super) fn pop_nonresolving_top_stack_entry(
    state: &mut GameState,
    disposition: super::lifecycle::DelayedTerminalDisposition,
) -> Option<PoppedStackEntry> {
    let popped = pop_top_stack_entry(state)?;
    if let Some(firing) = popped.trigger_firing {
        super::lifecycle::record_delayed_terminal(firing, disposition);
    }
    Some(popped)
}

/// Replays one already-resolved CR 405.2 stack removal.
///
/// Installs the recorded removal with nothing re-derived: the entry is verified
/// at the RECORDED index rather than located by a fresh scan, which matters
/// because the production sites find it with predicates that can match a
/// different entry on a diverged stack. All preconditions are checked BEFORE any
/// mutation, so a rejected replay leaves the stack and both side tables
/// untouched.
pub fn apply_resolved_stack_removal(
    state: &mut GameState,
    command: &ResolvedStackRemovalCommand,
) -> Result<(), ResolvedStackRemovalReplayInvariantError> {
    // CR 405.2: the predecessor must be exactly one deeper than the record.
    let expected_depth = command.resulting_depth + 1;
    if state.stack.len() != expected_depth {
        return Err(ResolvedStackRemovalReplayInvariantError::DepthMismatch {
            expected: expected_depth,
            found: state.stack.len(),
        });
    }
    let Some(found) = state.stack.get(command.index) else {
        return Err(ResolvedStackRemovalReplayInvariantError::IndexOutOfRange {
            index: command.index,
            depth: state.stack.len(),
        });
    };
    // Compared WHOLE rather than by id: an applier that matched on `id` alone
    // would discard a divergent object that merely reused the identifier.
    if found != command.entry.as_ref() {
        return Err(ResolvedStackRemovalReplayInvariantError::RemovedEntryMismatch);
    }

    // In range: the `get` above returned `Some`, so this cannot panic.
    let entry = state.stack.remove(command.index);
    state.stack_paid_facts.remove(&entry.id);
    state.stack_trigger_event_batches.remove(&entry.id);
    state.stack_trigger_firings.remove(&entry.id);
    Ok(())
}

/// CR 603.3d: removes an uncommitted triggered ability from the stack.
///
/// The "push first, choose second" invariant (see
/// [`GameState::pending_trigger_entry`]) puts a triggered ability on the stack
/// BEFORE its choices are gathered, so the entry is live while a `WaitingFor`
/// fills its slots. CR 603.3d: "If a choice is required when the triggered
/// ability goes on the stack but no legal choices can be made for it, or if a
/// rule or a continuous effect otherwise makes the ability illegal, the ability
/// is simply removed from the stack." This is the single authority for that
/// removal.
///
/// The two side tables are cleared here rather than by the callers because they
/// are keyed on the entry and settle WITH the pop — a removal that dropped the
/// entry but left `stack_paid_facts` or `stack_trigger_event_batches` behind
/// would strand rows against an id no longer on the stack.
///
/// Guarded on the entry still being topmost: the cursor can outlive the entry
/// when another path already removed it, and popping unconditionally would then
/// discard an unrelated stack object.
///
/// Note this deliberately does NOT clear `pending_trigger`; that is a separate
/// piece of construction state owned by
/// `engine::drop_mid_construction_pending_trigger`, which calls this and then
/// clears it.
pub(super) fn pop_uncommitted_pending_trigger_entry(
    state: &mut GameState,
    disposition: super::lifecycle::DelayedTerminalDisposition,
) {
    let Some(entry_id) = state.pending_trigger_entry.take() else {
        // No cursor: nothing was consumed and nothing settled, so there is no
        // mutation to journal.
        return;
    };
    let removed = (state.stack.back().map(|e| e.id) == Some(entry_id))
        .then(|| {
            let entry = state.stack.pop_back().expect("the entry was just observed");
            state.stack_paid_facts.remove(&entry_id);
            state.stack_trigger_event_batches.remove(&entry_id);
            let firing = take_stack_trigger_firing(state, &entry);
            PoppedStackEntry {
                entry,
                paid_facts: None,
                trigger_event_batch: None,
                trigger_firing: firing,
            }
        })
        .map(Box::new);

    if let Some(removed) = removed.as_ref() {
        let pending_firing = state
            .pending_trigger_firing
            .expect("uncommitted trigger removal must retain its pending firing carrier");
        assert_eq!(
            removed.trigger_firing,
            Some(pending_firing),
            "uncommitted trigger removal carriers must agree"
        );
    }

    // CR 733: journal AFTER the removal settles, and journal BOTH outcomes. The
    // `.take()` above is unconditional, so a guard that declines to pop still
    // consumed the cursor — recording only the popping case would leave a replay
    // of the other holding a `pending_trigger_entry` the real execution cleared.
    let cause = state.current_or_begin_rules_execution_node();
    let command = ResolvedUncommittedTriggerRemovalCommand {
        consumed_entry_id: entry_id,
        removed: removed
            .as_ref()
            .map(|removed| Box::new(removed.entry.clone())),
        resulting_depth: state.stack.len(),
        cause,
    };
    state
        .resolved_rules_journal
        .record_uncommitted_trigger_removal(command)
        .expect("resolved uncommitted trigger removal must have a live journal cause");
    if let Some(firing) = removed.and_then(|removed| removed.trigger_firing) {
        super::lifecycle::record_delayed_terminal(firing, disposition);
    }
}

/// Installs one already-resolved CR 603.3d removal verbatim.
///
/// Nothing is re-derived: the entry is compared WHOLE against the recorded one
/// rather than matched by id, so a replay whose stack top merely shares an id
/// fails closed instead of discarding a different object. The two side tables are
/// dropped by the recorded entry's own id.
///
/// Both recorded outcomes are honoured. `removed: None` means the original
/// execution consumed the cursor without popping, so this refuses to pop — and
/// asserts the predecessor agrees, because a replay whose top IS that entry would
/// otherwise silently diverge from the execution being replayed.
pub fn apply_resolved_uncommitted_trigger_removal(
    state: &mut GameState,
    command: &ResolvedUncommittedTriggerRemovalCommand,
) -> Result<(), ResolvedUncommittedTriggerRemovalReplayInvariantError> {
    if state.pending_trigger_entry != Some(command.consumed_entry_id) {
        return Err(
            ResolvedUncommittedTriggerRemovalReplayInvariantError::CursorMismatch {
                expected: command.consumed_entry_id,
                found: state.pending_trigger_entry,
            },
        );
    }
    let top_id = state.stack.back().map(|e| e.id);
    match command.removed.as_deref() {
        Some(recorded) => {
            if state.stack.len() != command.resulting_depth + 1 {
                return Err(
                    ResolvedUncommittedTriggerRemovalReplayInvariantError::DepthMismatch {
                        expected: command.resulting_depth + 1,
                        found: state.stack.len(),
                    },
                );
            }
            if state.stack.back() != Some(recorded) {
                return Err(
                    ResolvedUncommittedTriggerRemovalReplayInvariantError::RemovedEntryMismatch,
                );
            }
        }
        None => {
            if state.stack.len() != command.resulting_depth {
                return Err(
                    ResolvedUncommittedTriggerRemovalReplayInvariantError::DepthMismatch {
                        expected: command.resulting_depth,
                        found: state.stack.len(),
                    },
                );
            }
            if top_id == Some(command.consumed_entry_id) {
                return Err(
                    ResolvedUncommittedTriggerRemovalReplayInvariantError::UnexpectedRemovableEntry(
                        command.consumed_entry_id,
                    ),
                );
            }
        }
    }

    state.pending_trigger_entry = None;
    if command.removed.is_some() {
        state.stack.pop_back();
        state.stack_paid_facts.remove(&command.consumed_entry_id);
        state
            .stack_trigger_event_batches
            .remove(&command.consumed_entry_id);
        state
            .stack_trigger_firings
            .remove(&command.consumed_entry_id);
    }
    Ok(())
}

/// The ability currently represented by a stack entry for presentation.
///
/// A spell is placed on the stack before its cast is finalized (CR 601.2a-b),
/// so its final entry can still hold `None` while the matching `PendingCast`
/// carries the selected modes and targets. Identity is always the stack entry
/// ID; this deliberately never falls back to the top stack entry.
pub(crate) struct EffectiveStackAbility<'a> {
    pub ability: Option<&'a ResolvedAbility>,
    pub is_pending: bool,
}

pub(crate) fn effective_stack_ability<'a>(
    state: &'a GameState,
    entry: &'a StackEntry,
) -> EffectiveStackAbility<'a> {
    if let Some(ability) = entry.ability() {
        return EffectiveStackAbility {
            ability: Some(ability),
            is_pending: false,
        };
    }

    if let Some(pending) = state
        .waiting_for
        .pending_cast_ref()
        .filter(|pending| pending.object_id == entry.id)
    {
        return EffectiveStackAbility {
            ability: Some(&pending.ability),
            is_pending: true,
        };
    }

    if matches!(entry.kind, StackEntryKind::Spell { .. }) {
        if let Some(pending) = state
            .pending_cast
            .as_deref()
            .filter(|pending| pending.object_id == entry.id)
        {
            return EffectiveStackAbility {
                ability: Some(&pending.ability),
                is_pending: true,
            };
        }
    }

    EffectiveStackAbility {
        ability: None,
        is_pending: false,
    }
}

pub(crate) fn restore_alternative_spell_normal_face(state: &mut GameState, object_id: ObjectId) {
    if let Some(obj) = state.objects.get_mut(&object_id) {
        if let Some(normal_face) = obj.back_face.take() {
            let alternative_snapshot = super::printed_cards::snapshot_object_face(obj);
            super::printed_cards::apply_back_face_to_object(obj, normal_face);
            obj.back_face = Some(alternative_snapshot);
        }
    }
}

/// CR 608.2n / CR 608.3 / CR 608.3e: Predicate guard for post-resolution
/// default-zone moves on a resolving spell.
///
/// Spells normally leave the stack as the final part of their resolution —
/// non-permanents go to the graveyard (CR 608.2n), permanents enter the
/// battlefield (CR 608.3), and permanents whose ETB was fully prevented go
/// to the graveyard (CR 608.3e). Each of these default destinations is
/// itself a `move_to_zone(state, id, default, events)` call that runs
/// AFTER `execute_effect` has already had a chance to move the spell
/// elsewhere via its own instructions (e.g., Treasured Find — "Exile ~",
/// or any sub-ability that targets the source via `SelfRef`).
///
/// If the spell's resolution already moved it off the Stack, the default
/// move must be skipped — otherwise the card travels (Exile→Graveyard,
/// Exile→Battlefield, etc.) and undoes its own self-move clause (issue
/// #323). The Stack-residency check is the canonical guard: only spells
/// still on the Stack at the end of resolution receive the post-resolution
/// default destination.
fn spell_still_on_stack(state: &GameState, id: ObjectId) -> bool {
    spell_in_zone(state, id, Zone::Stack)
}

fn spell_in_zone(state: &GameState, id: ObjectId, zone: Zone) -> bool {
    state.objects.get(&id).is_some_and(|obj| obj.zone == zone)
}

fn has_missing_required_stack_targets(state: &GameState, ability: &ResolvedAbility) -> bool {
    if !flatten_targets_in_chain(ability).is_empty() {
        return false;
    }

    match build_target_slots(state, ability) {
        Ok(slots) => slots.iter().any(|slot| !slot.optional),
        Err(_) => true,
    }
}

fn has_no_legal_required_stack_targets(state: &GameState, ability: &ResolvedAbility) -> bool {
    if !flatten_targets_in_chain(ability).is_empty() {
        return false;
    }

    match build_target_slots(state, ability) {
        Ok(slots) => slots
            .iter()
            .any(|slot| !slot.optional && slot.legal_targets.is_empty()),
        Err(_) => true,
    }
}

fn top_pending_trigger_has_no_legal_required_targets(
    state: &mut GameState,
    pending_id: ObjectId,
) -> bool {
    let Some((ability, trigger_event, trigger_events, subject_match_count)) = state
        .stack
        .back()
        .filter(|entry| entry.id == pending_id)
        .and_then(|entry| {
            let ability = entry.ability()?.clone();
            let (trigger_event, subject_match_count) = match &entry.kind {
                StackEntryKind::TriggeredAbility {
                    trigger_event,
                    subject_match_count,
                    ..
                } => (trigger_event.clone(), *subject_match_count),
                _ => (None, None),
            };
            let trigger_events = state
                .stack_trigger_event_batches
                .get(&entry.id)
                .cloned()
                .unwrap_or_else(|| trigger_event.iter().cloned().collect());
            Some((ability, trigger_event, trigger_events, subject_match_count))
        })
    else {
        return false;
    };

    let context_snapshot = super::triggers::push_trigger_event_context(
        state,
        trigger_event.as_ref(),
        &trigger_events,
        subject_match_count,
    );
    let missing_required_targets = has_no_legal_required_stack_targets(state, &ability);
    super::triggers::restore_trigger_event_context(state, context_snapshot);
    missing_required_targets
}

/// CR 614.1a + CR 608.2n + CR 607.2b: The per-object linked source is also the
/// exile-instead marker for Rod of Absorption's resolving-spell rider.
fn stack_exile_linked_source(state: &GameState, object_id: ObjectId) -> Option<ObjectId> {
    state
        .objects
        .get(&object_id)
        .and_then(|obj| obj.exile_from_stack_linked_source)
}

/// CR 608.3e + CR 614.6: A permanent spell whose ETB was fully prevented goes
/// to its owner's graveyard (only if still on the stack — see `spell_still_on_stack`).
/// Routed through the zone pipeline so board-wide `Moved` graveyard→exile
/// redirects (Rest in Peace / Leyline of the Void) fire on the discarded
/// permanent (PLAN §8 Risk #2). Returns the `ZoneMoveResult` so the caller can
/// propagate a CR 616.1 ordering pause (two simultaneous redirects); the common
/// single-redirect / no-redirect path returns `Done`.
fn move_prevented_permanent_spell_to_graveyard_if_still_on_stack(
    state: &mut GameState,
    id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> ZoneMoveResult {
    if spell_still_on_stack(state, id) {
        let req = ZoneMoveRequest::spell_resolution_default(id, Zone::Graveyard);
        zone_pipeline::move_object(state, req, events)
    } else {
        ZoneMoveResult::Done
    }
}

/// CR 608.3 + CR 400.7d: Snapshot cast-link / target facts for a permanent spell
/// paused mid-resolution (delivery-tail `NeedsChoice`, replacement-choice
/// `NeedsChoice`, or CallerEpilogue `CopyTargetChoice`). Single authority so a
/// new cast-metadata field cannot be threaded into only two of three stash sites.
fn pending_spell_resolution_snapshot(
    state: &GameState,
    entry: &StackEntry,
    ability: Option<&ResolvedAbility>,
    casting_variant: CastingVariant,
    actual_mana_spent: u32,
    spell_targets: &[TargetRef],
) -> PendingSpellResolution {
    let obj = state.objects.get(&entry.id);
    let cast_from_zone = ability
        .and_then(|a| a.context.cast_from_zone)
        .or_else(|| obj.and_then(|o| o.cast_from_zone));
    let cast_timing_permission =
        obj.and_then(|o| o.cast_timing_permission.map(|(permission, _)| permission));
    let kickers_paid = ability
        .map(|a| a.context.kickers_paid.clone())
        .unwrap_or_else(|| obj.map(|o| o.kickers_paid.clone()).unwrap_or_default());
    let additional_cost_payment_count = ability
        .map(|a| a.context.additional_cost_payment_count)
        .unwrap_or_else(|| {
            obj.map(|o| o.additional_cost_payment_count)
                .unwrap_or_default()
        });
    let additional_cost_payments = ability
        .map(|a| a.context.additional_cost_payments.clone())
        .unwrap_or_else(|| {
            obj.map(|o| o.additional_cost_payments.clone())
                .unwrap_or_default()
        });
    let convoked_creatures = obj
        .map(|o| o.convoked_creatures.clone())
        .unwrap_or_default();
    PendingSpellResolution {
        object_id: entry.id,
        controller: entry.controller,
        casting_variant,
        cast_from_zone,
        cast_controller: Some(entry.controller),
        cast_timing_permission,
        spell_targets: spell_targets.to_vec(),
        actual_mana_spent,
        kickers_paid,
        additional_cost_payment_count,
        additional_cost_payments,
        convoked_creatures,
    }
}

/// CR 603.4 + CR 608.2k + CR 603.2c + CR 706.2: bind the resolution scope
/// [`resolve_top`] hands to `resolve_ability_chain`, for an entry ALREADY off
/// the stack.
///
/// Returns `false` iff the CR 603.4 intervening-if re-check fails — i.e. the
/// live resolution proposes NOTHING. The caller owns the consequence:
/// [`resolve_top`] pushes `GameEvent::StackResolved` and returns; an analysis
/// caller returns its fail-closed verdict. The event is deliberately NOT pushed
/// here — this function takes no event sink, which is what keeps it callable
/// from the analysis crate.
///
/// CR 608.2k is the rule for the `current_trigger_event` lift: *"If an ability's
/// effect refers to a specific untargeted object that has been previously
/// referred to by that ability's cost or trigger condition, it still affects
/// that object even if the object has changed characteristics."* The
/// `Triggering*` anaphors are exactly untargeted back-references to the object
/// the TRIGGER CONDITION matched (carried on the entry as `trigger_event`); the
/// lift is the mechanism that keeps that object reachable while the ability
/// resolves, and it CLONES the recorded event rather than re-evaluating the
/// condition, so the binding survives characteristic change by construction.
/// CR 608.2h is why a clone rather than a re-derivation is right: the answer is
/// determined only once, when the effect is applied.
///
/// The in-order-written execution of those anaphor arms is CR 608.2c; the
/// batched-subject-count re-stamp is CR 603.2c; the die-roll re-stamp is
/// CR 706.2.
pub(crate) fn bind_resolution_scope(
    state: &mut GameState,
    entry: &StackEntry,
    trigger_event_batch: Option<Vec<GameEvent>>,
) -> bool {
    let triggered = match &entry.kind {
        StackEntryKind::TriggeredAbility {
            condition,
            trigger_event,
            subject_match_count,
            die_result,
            ..
        } => Some(TriggeredResolutionScope {
            condition: condition.as_ref(),
            controller: entry.controller,
            trigger_source: entry
                .ability()
                .and_then(|ability| ability.trigger_source.as_ref()),
            trigger_event: trigger_event.as_ref(),
            subject_match_count: *subject_match_count,
            die_result: *die_result,
        }),
        _ => None,
    };
    bind_triggered_resolution_scope(state, triggered, trigger_event_batch)
}

/// The facts a triggered ability contributes to its own resolution scope,
/// lifted out of `StackEntryKind::TriggeredAbility` so a resolution that owns
/// no stack entry can bind exactly the same scope.
///
/// CR 605.4a triggered mana abilities are the motivating second consumer: they
/// resolve without ever creating a stack object, yet they still need the
/// CR 603.4 recheck, the CR 608.2k event context, the CR 603.2c subject count,
/// and the CR 706.2 die-roll re-stamp to be bound in exactly the order and with
/// exactly the semantics stack resolution uses. Reimplementing that binding
/// beside the immediate dispatcher would be a second authority for CR 603.4.
pub(crate) struct TriggeredResolutionScope<'a> {
    pub condition: Option<&'a TriggerCondition>,
    pub controller: PlayerId,
    pub trigger_source: Option<&'a TriggerSourceContext>,
    pub trigger_event: Option<&'a GameEvent>,
    pub subject_match_count: Option<u32>,
    pub die_result: Option<i32>,
}

/// The decision-and-binding half of [`bind_resolution_scope`], with no stack
/// entry in sight. Returns `false` exactly when the CR 603.4 intervening-if
/// recheck fails, in which case **nothing** has been bound — the caller must
/// abandon the resolution without applying any effect.
///
/// `triggered` is `None` for a non-triggered resolution (a spell, an activated
/// ability, a keyword action); such a scope has no condition, no subject count,
/// and no die result, and reaches only the batch branch below.
pub(crate) fn bind_triggered_resolution_scope(
    state: &mut GameState,
    triggered: Option<TriggeredResolutionScope<'_>>,
    trigger_event_batch: Option<Vec<GameEvent>>,
) -> bool {
    // CR 603.4: Intervening-if condition rechecked at resolution time.
    if let Some(scope) = &triggered {
        if let Some(condition) = scope.condition {
            if !super::triggers::check_trigger_condition_with_source(
                state,
                condition,
                scope.controller,
                scope.trigger_source,
                scope.trigger_event,
            ) {
                return false;
            }
        }
    }

    // CR 608.2k: Set trigger event context for event-context target resolution.
    // TriggeringSpellController, TriggeringSource, etc. read this during resolution.
    match (
        triggered.as_ref().and_then(|scope| scope.trigger_event),
        trigger_event_batch,
    ) {
        (Some(te), batch) => {
            state.current_trigger_event = Some(te.clone());
            state.current_trigger_events = batch.unwrap_or_else(|| vec![te.clone()]);
        }
        (None, Some(trigger_events)) => {
            state.current_trigger_event = trigger_events.first().cloned();
            state.current_trigger_events = trigger_events;
        }
        (None, None) => {}
    }

    // CR 603.2c: Lift the filtered subject count of a batched trigger into
    // resolution scope so `QuantityRef::EventContextAmount` resolves "that
    // many" against the count, not against zero. Set in lockstep with
    // `current_trigger_event` and cleared at every reset site below.
    if let Some(scope) = &triggered {
        state.current_trigger_match_count = scope.subject_match_count;
        // CR 706.2 + CR 706.4 + CR 603.12: re-stamp the carried die-roll result
        // into resolution scope so a reflexive "When you do … the result"
        // sub-ability resolving on its own stack entry (a later apply(), after
        // the original roll's resolution scope cleared) reads the rolled value
        // via the `QuantityRef::EventContextAmount` cascade.
        state.die_result_this_resolution = scope.die_result;
    }

    true
}

/// CR 714.2 + CR 714.2d: The Saga-chapter identity of a stack entry that is
/// about to resolve, or `None` if the entry is not a Saga chapter ability.
struct ResolvingSagaChapter {
    saga: TriggerSourceContext,
    controller: PlayerId,
    chapter: u32,
    final_chapter: u32,
}

/// CR 714.2 + CR 400.7: Classify an about-to-resolve stack entry as a Saga
/// chapter ability, reading everything from the trigger's own source context.
///
/// Deliberately does NOT consult live state by `source_id`. `source_id` is
/// storage identity: a Saga that left and re-entered occupies the same id as a
/// different object, whose chapter abilities — and mana value — are not the ones
/// this ability triggered from. CR 113.7a lets that already-triggered chapter
/// ability resolve anyway, so reading live state would either report the wrong
/// Saga's numbers or (if guarded on incarnation) drop an occurrence that really
/// did resolve.
///
/// `TriggerSourceContext` is the engine's existing answer to exactly this: it
/// was captured when the chapter ability triggered, pins the incarnation in
/// `identity.reference`, and carries that incarnation's `trigger_entries` and
/// `lki`. Both the chapter numbers below and every characteristic an observer
/// can later ask about therefore come from the right object by construction.
fn resolving_saga_chapter(entry: &StackEntry) -> Option<ResolvingSagaChapter> {
    let StackEntryKind::TriggeredAbility { ability, .. } = &entry.kind else {
        return None;
    };
    let occurrence = &ability.trigger_definition_ref.as_ref()?.occurrence;
    let saga = ability.trigger_source.as_ref()?;

    // CR 714.2: chapter numbers come from the chapter-symbol provenance on the
    // source incarnation's own trigger entries, never from a live lore count.
    let chapter = saga
        .trigger_entries
        .iter()
        .find(|entry| &entry.occurrence == occurrence)
        .and_then(|entry| entry.definition.saga_chapter)?;
    // CR 714.2d: greatest chapter number among that same incarnation's chapter
    // abilities.
    let final_chapter = saga
        .trigger_entries
        .iter()
        .filter_map(|entry| entry.definition.saga_chapter)
        .max()?;

    Some(ResolvingSagaChapter {
        saga: saga.clone(),
        controller: entry.controller,
        chapter,
        final_chapter,
    })
}

/// CR 608.2: Resolve the top object on the stack.
pub fn resolve_top(state: &mut GameState, events: &mut Vec<GameEvent>) {
    // CR 603.3c + CR 603.3d: The top of the stack may be a trigger entry that
    // is still being constructed (mode / target / division pending). Such an
    // entry MUST NOT resolve — it is mid-flight while the controller is
    // gathering inputs via the active `WaitingFor`. The
    // `pending_trigger_entry` cursor is cleared when construction completes
    // (target chosen, distribution assigned, etc.); only then is resolution
    // permitted.
    if let Some(pending_id) = state.pending_trigger_entry {
        if state.stack.back().map(|e| e.id) == Some(pending_id) {
            if !top_pending_trigger_has_no_legal_required_targets(state, pending_id) {
                return;
            }
            // CR 603.3d: A stale construction cursor on a malformed trigger
            // with no legal required targets cannot keep a triggered ability
            // suspended forever.
            if let Some(firing) = state.pending_trigger_firing.take() {
                assert_eq!(
                    state.stack_trigger_firings.get(&pending_id).copied(),
                    Some(firing),
                    "stale pending trigger must transfer its firing to the live stack entry"
                );
            }
            state.pending_trigger_entry = None;
            state.pending_trigger = None;
            state.pending_trigger_event_batch.clear();
        }
    }

    // CR 608.2c: A resolution that completed at the preceding Priority
    // boundary must settle its exact carrier before another stack object can
    // begin resolving. A parked continuation remains live and therefore still
    // fails the invariant below rather than being silently cleared.
    super::engine::settle_resolving_stack_entry_after_continuation_resume(state);
    // CR 707.10: A prior resolution must have settled before another stack
    // object can begin resolving. A parked continuation owns its carrier until
    // its own completion or abort path; silently clearing it here would lose a
    // receipt-eligible delayed firing.
    debug_assert!(state.resolving_stack_entry.is_none());
    debug_assert!(state.resolving_trigger_firing.is_none());
    // CR 400.7j: the self-move re-latch is resolution-scoped; clear it alongside
    // `resolving_stack_entry` so it never leaks into the next resolution.
    state.resolution_source_relatch = None;
    // CR 107.3a: the announced activation-X carrier is scoped to the activation that
    // published it. Clear it here so it never leaks into an unrelated resolution; it is
    // republished below for an `ActivatedAbility` entry (and only for that kind).
    state.announced_source_x = None;
    state.turn_up_paid_cost_source = None;

    // CR 405.5: When all players pass in succession, the top object on the stack resolves.
    let Some(PoppedStackEntry {
        entry,
        paid_facts: paid_snapshot,
        trigger_event_batch,
        trigger_firing,
    }) = pop_top_stack_entry(state)
    else {
        return;
    };
    // CR 603.4 + CR 608.2b: transfer the exact firing before any branch can
    // abort, resolve, or park this popped triggered ability.
    begin_resolving_stack_entry(state, entry.clone(), trigger_firing);

    // CR 113.3b: Activated keyword abilities (Equip / Crew / Saddle / Station)
    // resolve via their typed payload — they have no ResolvedAbility/targets
    // to validate and no zone-change routing (the source stays where it is).
    // Returning early keeps the keyword-action branch out of the targeting /
    // fizzle / permanent-spell pipeline below.
    if let StackEntryKind::KeywordAction { action } = entry.kind {
        resolve_keyword_action(state, action, events);
        events.push(GameEvent::StackResolved {
            object_id: entry.id,
        });
        finish_resolving_stack_entry(
            state,
            super::lifecycle::DelayedTerminalDisposition::Resolved,
        );
        state.resolution_source_relatch = None;
        return;
    }

    // CR 603.4: the intervening-if recheck lives inside `bind_resolution_scope`; a `false`
    // return means the condition failed and this entry resolves with no effect. The
    // SETTLEMENT stays HERE, at the caller, and must never move into the helper:
    // `analysis/resource.rs` calls `bind_resolution_scope` on CLONED PROBE BOARDS (five
    // sites), where running terminal delayed-trigger disposition would mutate lifecycle
    // state for a board that is only being measured.
    if !bind_resolution_scope(state, &entry, trigger_event_batch) {
        events.push(GameEvent::StackResolved {
            object_id: entry.id,
        });
        finish_resolving_stack_entry(
            state,
            super::lifecycle::DelayedTerminalDisposition::InterveningIfFalse,
        );
        state.resolution_source_relatch = None;
        return;
    }

    // CR 714.2: Snapshot the Saga-chapter identity while the Saga is still
    // reachable — the chapter ability's own effect may remove it. Only the
    // success path below publishes it; a fizzle (CR 608.2b) or a failed
    // intervening-if (CR 603.4) leaves the stack without resolving.
    let saga_chapter = resolving_saga_chapter(&entry);

    // Extract the resolved ability from the stack entry. `KeywordAction` is
    // handled by the early return above and never reaches this match.
    let (mut ability, is_spell, casting_variant, actual_mana_spent) = match &entry.kind {
        StackEntryKind::Spell {
            ability,
            casting_variant,
            actual_mana_spent,
            ..
        } => (ability.clone(), true, *casting_variant, *actual_mana_spent),
        StackEntryKind::ActivatedAbility { ability, .. } => {
            (Some(ability.clone()), false, CastingVariant::Normal, 0)
        }
        StackEntryKind::TriggeredAbility { ability, .. } => {
            (Some(ability.clone()), false, CastingVariant::Normal, 0)
        }
        StackEntryKind::KeywordAction { .. } => unreachable!(
            "KeywordAction stack entries are resolved via the early-return branch above"
        ),
    };

    // CR 603.7c + CR 120.3 + CR 506.2: A "deals [combat] damage to a player" /
    // "attacks a player" trigger introduces the damaged/attacked player as the
    // event referent. Stamp it onto the resolving ability's `scoped_player`
    // (when not already bound) so `PlayerScope::ScopedPlayer` quantities such as
    // "they lose half their life, rounded up" (Unstoppable Slasher) resolve
    // against that player rather than falling back to the source's controller.
    // Mirrors the Phase-trigger stamping in `triggers::build_triggered_ability`;
    // the parser rebinds these possessives to `ScopedPlayer` in
    // `lower_trigger_ir`.
    if let Some(ability) = ability.as_mut() {
        if ability.scoped_player.is_none() {
            if let Some(pid) = state.current_trigger_event.as_ref().and_then(|event| {
                matches!(
                    event,
                    GameEvent::DamageDealt {
                        target: TargetRef::Player(_),
                        ..
                    } | GameEvent::AttackersDeclared { .. }
                )
                .then(|| targeting::extract_player_from_event(event, state))
                .flatten()
            }) {
                ability.set_scoped_player_recursive(pid);
            }
        }
    }

    // CR 109.4 + CR 115.10a/b (issue #6505): "Target opponent exiles a creature
    // they control and their graveyard" (Strategic Betrayal). The spell targets
    // ONLY the opponent (CR 115.1a); that opponent then CHOOSES a creature they
    // control and exiles their graveyard — so a `ScopedPlayer`-scoped move-object
    // filter must resolve its acting/choosing player against the resolved single
    // player target, not the caster. Sibling of the DamageDealt/AttackersDeclared
    // scoped-player stamp above: bind `scoped_player` from the ability's lone
    // `TargetRef::Player` before the change_zone choosers run at resolution.
    if let Some(ability) = ability.as_mut() {
        if ability.scoped_player.is_none() {
            let single_player_target = ability
                .targets
                .iter()
                .filter(|target| matches!(target, TargetRef::Player(_)))
                .count()
                == 1;
            if single_player_target
                && crate::game::effects::ability_uses_relative_controller_scoped(ability)
            {
                let actor = ability.target_player();
                ability.set_scoped_player_recursive(actor);
            }
        }
    }

    // CR 608.2c: Re-stamp ParentTarget anaphora from the stack entry's trigger
    // event at resolution time (Stationed/VehicleCrewed/Saddled/attack batches).
    // Push-time seeding in `push_pending_trigger_to_stack_with_event_batch` can
    // be skipped on alternate dispatch paths; this guarantees the referent is
    // bound before `execute_effect` when `trigger_event` is present on the entry.
    if let (Some(ability), StackEntryKind::TriggeredAbility { trigger_event, .. }) =
        (ability.as_mut(), &entry.kind)
    {
        let event_ref = trigger_event
            .as_ref()
            .or(state.current_trigger_event.as_ref());
        super::triggers::seed_batched_attack_parent_targets(ability, event_ref);
        super::triggers::seed_event_context_parent_targets(ability, event_ref);
    }

    if ability
        .as_ref()
        .is_some_and(|ability| has_missing_required_stack_targets(state, ability))
    {
        // CR 603.3d: If a triggered ability needs a stack-time target choice and
        // no legal choice was made, remove it from the stack.
        // CR 608.2b: A resolving spell or ability with no legal targets does not
        // resolve.
        events.push(GameEvent::StackResolved {
            object_id: entry.id,
        });
        state.current_trigger_event = None;
        state.current_trigger_events.clear();
        state.current_trigger_match_count = None;
        state.die_result_this_resolution = None;
        finish_resolving_stack_entry(
            state,
            super::lifecycle::DelayedTerminalDisposition::NoLegalChoice,
        );
        state.resolution_source_relatch = None;
        return;
    }

    // Capture targets for Aura attachment after resolution. Prefer the full
    // chain flatten so Enchant targets assigned onto an Aura placeholder are
    // not missed when only a nested sink holds them.
    let spell_targets = ability
        .as_ref()
        .map(|a| {
            let flat = flatten_targets_in_chain(a);
            if flat.is_empty() {
                a.targets.clone()
            } else {
                flat
            }
        })
        .unwrap_or_default();

    // CR 702.103e: As a bestowed Aura spell begins resolving, if its target is
    // illegal it ceases to be bestowed and the effect making it an Aura spell
    // ends — it continues resolving as a creature spell. We detect this BEFORE
    // the standard fizzle check (which would otherwise route the spell to
    // graveyard per CR 608.2b). The revert restores Creature core type and
    // removes the bestow-granted Aura subtype + `enchant creature` keyword;
    // `is_permanent_type` then sees a Creature and routes to the battlefield.
    let mut bestow_reverted_at_resolution = false;
    if casting_variant == CastingVariant::Bestow {
        let target_is_illegal = ability.as_ref().is_some_and(|a| {
            let original = flatten_targets_in_chain(a);
            if original.is_empty() {
                return false;
            }
            let validated = validate_targets_in_chain(state, a);
            let legal = flatten_targets_in_chain(&validated);
            targeting::check_fizzle(&original, &legal)
        });
        let still_bestow_form = state
            .objects
            .get(&entry.id)
            .is_some_and(|o| o.bestow_form.is_some());
        if target_is_illegal && still_bestow_form {
            super::casting::revert_bestow_form(state, entry.id);
            bestow_reverted_at_resolution = true;
        }
    }

    // CR 702.140b-c: A mutating creature spell begins resolving. Mirror the
    // Bestow illegal-target detection (above) — both run BEFORE the generic
    // CR 608.2b fizzle check, because a mutating spell with an illegal target
    // does NOT fizzle to the graveyard: it reverts to a plain creature spell and
    // resolves (CR 702.140b). The LEGAL case diverts entirely:
    //   * CR 702.140b — target illegal: revert to a plain creature spell and
    //     continue resolving (falls through to the normal permanent-spell
    //     battlefield entry below); the fizzle check is suppressed via
    //     `mutate_reverted_at_resolution`.
    //   * CR 702.140c — target legal: the spell does NOT enter the battlefield.
    //     Instead it pauses for the controller's top/bottom choice;
    //     `merge::handle_mutate_merge_choice` performs the merge.
    let mut mutate_reverted_at_resolution = false;
    if casting_variant == CastingVariant::Mutate {
        let mutate_target = spell_targets.iter().find_map(|t| match t {
            crate::types::ability::TargetRef::Object(id) => Some(*id),
            _ => None,
        });
        // CR 608.2b + CR 702.140b: re-check the captured target is STILL legal at
        // resolution — not merely present. A target that stopped being a creature,
        // became Human, or changed owner is now illegal and the spell reverts to a
        // plain creature spell. Re-evaluate against the SAME predicate the
        // cast-offer / target-attachment path used (`casting::mutate_target_filter`)
        // via the shared targeting/filter machinery so the two cannot drift.
        let legal_target = mutate_target.filter(|&id| {
            if !state.battlefield.contains(&id) {
                return false;
            }
            let filter = super::casting::mutate_target_filter();
            let ctx = super::filter::FilterContext::from_source_with_controller(
                entry.id,
                entry.controller,
            );
            super::filter::matches_target_filter(state, id, &filter, &ctx)
        });
        match legal_target {
            Some(target_id) => {
                // CR 702.140c: pause for the top/bottom choice. The merging spell
                // (`entry.id`) has already been popped from the stack.
                state.push_mutate_merge_frame(crate::types::resolution::PendingMutateMerge {
                    merging_id: entry.id,
                    target_id,
                    controller: entry.controller,
                });
                state.waiting_for = crate::types::game_state::WaitingFor::MutateMergeChoice {
                    player: entry.controller,
                    merging_id: entry.id,
                    target_id,
                };
                events.push(GameEvent::StackResolved {
                    object_id: entry.id,
                });
                state.current_trigger_event = None;
                state.current_trigger_events.clear();
                state.current_trigger_match_count = None;
                state.die_result_this_resolution = None;
                return;
            }
            None => {
                // CR 702.140b: illegal target — revert to a plain creature spell
                // and continue resolving via the normal battlefield-entry path.
                // Suppress the fizzle check below so it does not route the spell to
                // the graveyard (it is no longer a targeted mutating spell).
                super::casting::revert_mutate_form(state, entry.id);
                mutate_reverted_at_resolution = true;
            }
        }
    }

    // CR 707.10: Preserve the resolving stack entry so a `CopySpell` carried as
    // the spell's own effect (the Chain cycle's "you may copy this spell")
    // can copy itself even though `resolve_top` has already popped it off the
    // stack — and even after the spell has moved to the graveyard while an
    // optional copy decision is pending. Cleared at the start of the next
    // `resolve_top`.
    // CR 107.3a + CR 107.3i: republish the resolving activated ability's announced X for
    // the duration of its own resolution, so a triggered ability of the SAME object that
    // this resolution causes (Hydra Broodmaster / Polukranos: "when this becomes
    // monstrous, …X…", fired off the `EffectResolved{Monstrosity}` emitted below) reads
    // that X. Deliberately restricted to `ActivatedAbility`: a resolving SPELL must NOT
    // publish, because a permanent it puts onto the battlefield has X = 0 (CR 107.3m) and
    // its ETB trigger reads the spell's X through `GameObject::cost_x_paid` instead.
    if let StackEntryKind::ActivatedAbility {
        source_id,
        ability: activated,
    } = &entry.kind
    {
        state.announced_source_x = activated.chosen_x.map(|x| (*source_id, x));
    }
    let resolution_start_phase = state.phase;

    // Only run targeting validation and effect execution when an ability exists.
    // Permanent spells with no spell ability (ability is None) skip straight to
    // zone-change handling below.
    if let Some(ref ability) = ability {
        let original_targets = flatten_targets_in_chain(ability);
        // CR 702.103e: when a bestowed Aura reverted at the start of resolution,
        // suppress the fizzle check — the spell is no longer an Aura and proceeds
        // to resolve as a creature spell with no remaining target.
        if !original_targets.is_empty()
            && !bestow_reverted_at_resolution
            && !mutate_reverted_at_resolution
        {
            let validated = validate_targets_in_chain(state, ability);
            let legal_targets = flatten_targets_in_chain(&validated);
            if targeting::check_fizzle(&original_targets, &legal_targets) {
                // CR 608.2b: Fizzle — all targets illegal, spell is countered on resolution.
                if is_spell {
                    // CR 702.34a / CR 702.127a / CR 702.180a: Flashback,
                    // Aftermath, and Harmonize exile when leaving the stack
                    // for any reason, including fizzle. This is a STATIC
                    // destination rule (the spell exiles instead of going to
                    // any zone), not a replacement — it is selected here. Escape
                    // (CR 702.138) has no such clause — escaped spells go to
                    // graveyard normally. The Invoke Calamity free-cast rider is
                    // NOT applied here: it is a self-scoped `Moved` replacement
                    // on the spell, consulted by the pipeline below, so it never
                    // double-applies with this static exile (its Graveyard-scoped
                    // def does not match a stack→Exile move).
                    let dest = if casting_variant.replaces_stack_to_graveyard_with_exile() {
                        Zone::Exile
                    } else {
                        Zone::Graveyard
                    };
                    if casting_variant.restores_front_face_after_stack_exit() {
                        restore_alternative_spell_normal_face(state, entry.id);
                    }
                    // CR 608.2n + CR 614.6: route the stack → graveyard/exile
                    // move through the pipeline so self-scoped `Moved` redirects
                    // (the Invoke Calamity rider) and board-wide RIP/Leyline
                    // redirects fire. On a CR 616.1 ordering pause (rider + RIP
                    // = two simultaneous graveyard→exile candidates) the prompt
                    // AND the move are parked by `move_object`; the spell has
                    // left the stack either way, so fall through to the shared
                    // fizzle epilogue below (StackResolved + trigger-context /
                    // die-result clears) exactly as the delivered path does, and
                    // let the replacement-choice resume path deliver the parked
                    // move. A bare early return here leaked stale
                    // cross-resolution context and never emitted StackResolved
                    // (review fix).
                    let req = ZoneMoveRequest::spell_resolution_default(entry.id, dest);
                    let _ = zone_pipeline::move_object(state, req, events);
                }
                events.push(GameEvent::StackResolved {
                    object_id: entry.id,
                });
                state.current_trigger_event = None;
                state.current_trigger_events.clear();
                state.current_trigger_match_count = None;
                // CR 706.2 + CR 706.4: clear the carried die-roll result at the
                // same cross-resolution boundary as the batched subject count.
                state.die_result_this_resolution = None;
                finish_resolving_stack_entry(
                    state,
                    super::lifecycle::DelayedTerminalDisposition::AllTargetsIllegal,
                );
                state.resolution_source_relatch = None;
                return;
            }
            execute_effect(state, &validated, events);
        } else {
            execute_effect(state, ability, events);
        }
    }

    // CR 702.99a: Cipher — on-resolution hook. If the resolving spell carries
    // `Keyword::Cipher`, is represented by a card, and its controller has a
    // creature to host it, pause for the optional "exile this card encoded on a
    // creature you control" choice. The card is held off the stack until the
    // choice completes (mirroring the Mutate merge pause); the choice handler
    // exiles+encodes on accept, or routes the card to its graveyard on decline.
    // Skipped (resolution proceeds to graveyard normally) when there is no legal
    // host. `is_spell` gates out triggered/activated stack entries.
    if is_spell && super::cipher::begin_encode_choice(state, entry.id, entry.controller, events) {
        events.push(GameEvent::StackResolved {
            object_id: entry.id,
        });
        state.current_trigger_event = None;
        state.current_trigger_events.clear();
        state.current_trigger_match_count = None;
        state.die_result_this_resolution = None;
        return;
    }

    // CR 702.xxx: Paradigm (Strixhaven) — first-resolution hook. If the
    // resolving spell carries `Keyword::Paradigm` and this is the first
    // resolution of any spell with this name by the controller (per the
    // reminder text: "After you first resolve a spell with this name"), arm
    // the Paradigm offer: push a `ParadigmPrime` record and mint an
    // `ExileLinkKind::ParadigmSource` link, then override destination routing
    // to Exile. Copies (`is_token`) never arm Paradigm because their card
    // name is derived but they are not "the" spell per the reminder. Assign
    // when WotC publishes SOS CR update.
    let paradigm_armed = if is_spell {
        let obj = state.objects.get(&entry.id);
        let has_paradigm = obj.is_some_and(|o| {
            !o.is_token
                && super::keywords::has_keyword(o, &crate::types::keywords::Keyword::Paradigm)
        });
        if has_paradigm {
            let card_name = obj.map(|o| o.name.clone()).unwrap_or_default();
            super::effects::paradigm::arm_paradigm(state, entry.id, entry.controller, &card_name)
        } else {
            false
        }
    } else {
        false
    };

    // CR 702.88a: Rebound — on-resolve hook. If the resolving spell is a
    // non-permanent spell that carries `Keyword::Rebound`, was cast from
    // its owner's hand, and is not a token, push the next-upkeep delayed
    // triggered ability that offers an optional free recast and override
    // the destination from graveyard to exile.
    // CR 704.5d: tokens cease to exist off the battlefield (gate `!is_token`).
    // CR 603.7a: delayed triggered abilities are created during resolution.
    // CR 603.7d: source of the delayed trigger IS the resolving spell.
    // CR 608.2n: default destination for a resolved instant/sorcery is graveyard.
    // CR 702.88c: multiple instances of rebound on the same spell are
    // redundant — `has_keyword` returns true even if duplicates exist, so
    // arming runs at most once per resolution.
    let rebound_armed = if is_spell && !is_permanent_spell(state, entry.id) {
        let has_rebound = state.objects.get(&entry.id).is_some_and(|o| {
            !o.is_token
                && super::keywords::has_keyword(o, &crate::types::keywords::Keyword::Rebound)
        });
        // CR 601.2a + CR 702.88a: the resolving stack entry has already been
        // popped, so real instant/sorcery spells must read the pre-announcement
        // zone from the local ResolvedAbility context. `spell_cast_origin`
        // remains the fallback for object-stamped placeholder/permanent paths.
        let cast_from_zone = ability
            .as_ref()
            .and_then(|a| a.context.cast_from_zone)
            .or_else(|| super::casting::spell_cast_origin(state, entry.id));
        if has_rebound && cast_from_zone == Some(Zone::Hand) {
            super::effects::rebound::arm_rebound(state, entry.id, entry.controller, events)
        } else {
            false
        }
    } else {
        false
    };

    // CR 702.50a-b: Epic — on-resolve hook. If the resolving spell still
    // carries `Keyword::Epic`, lock its controller out of casting spells for
    // the rest of the game (CR 702.50b) and arm a RECURRING delayed triggered
    // ability that copies the spell at the beginning of each of the
    // controller's upkeeps (CR 702.50a). A copied spell that still has Epic
    // also arms this effect when it resolves; Epic-generated copies do not
    // recurse because `EpicCopy` strips `Keyword::Epic` before pushing them.
    // The Epic spell itself takes the normal destination below (no override);
    // that object is the prototype the upkeep copies clone.
    if is_spell {
        let has_epic = state.objects.get(&entry.id).is_some_and(|o| {
            super::keywords::has_keyword(o, &crate::types::keywords::Keyword::Epic)
        });
        if has_epic {
            if let Some(spell_ability) = ability.clone() {
                super::effects::epic::arm_epic(state, entry.id, entry.controller, *spell_ability);
            }
        }
    }

    // CR 608.2g + CR 608.3: A spell paused on a during-resolution free-cast
    // window remains on the stack and targetable until its continuation ends.
    if is_spell
        && !matches!(
            state.waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::FreeCastWindow { .. },
                ..
            }
        )
    {
        let end_procedure_exiles_resolving_object = ability.as_ref().is_some_and(|ability| {
            matches!(ability.effect, Effect::EndTheTurn)
                || (matches!(ability.effect, Effect::EndCombatPhase)
                    && resolution_start_phase.is_combat())
        });
        let dest = if end_procedure_exiles_resolving_object {
            // CR 724.1b / CR 724.2b: The "end the turn" and "end the combat
            // phase" procedures exile every object on the stack, including the
            // resolving object that `resolve_top` already popped before
            // executing its effect.
            Zone::Exile
        } else if paradigm_armed {
            // CR 702.xxx: Paradigm-armed spell exiles instead of going to
            // graveyard. The ExileLink is already created by arm_paradigm.
            Zone::Exile
        } else if rebound_armed {
            // CR 702.88a: Rebound-armed non-permanent spell exiles instead
            // of going to graveyard — the delayed trigger is already
            // queued by `arm_rebound`.
            Zone::Exile
        } else if casting_variant == CastingVariant::Adventure {
            // CR 715.3d: Adventure spell resolves → exile with casting permission.
            Zone::Exile
        } else if casting_variant == CastingVariant::Omen {
            // CR 720.3d: Omen spell resolves → shuffle into owner's library.
            Zone::Library
        } else if casting_variant == CastingVariant::Harmonize {
            // CR 702.180a: If the harmonize cost was paid, exile this card instead of putting it anywhere else.
            if is_permanent_spell(state, entry.id) {
                Zone::Battlefield
            } else {
                Zone::Exile
            }
        } else if casting_variant == CastingVariant::Aftermath {
            // CR 702.127a: If an aftermath spell was cast from a graveyard,
            // exile it instead of putting it anywhere else any time it would
            // leave the stack.
            Zone::Exile
        } else if casting_variant == CastingVariant::Flashback {
            // CR 702.34a: If the flashback cost was paid, exile this card
            // instead of putting it anywhere else any time it would leave the stack.
            // Flashback only appears on instants/sorceries — unconditional exile is correct.
            Zone::Exile
        } else if (casting_variant.replaces_stack_to_graveyard_with_exile()
            || stack_exile_linked_source(state, entry.id).is_some())
            && !is_permanent_spell(state, entry.id)
        {
            // CR 614.1a + CR 608.2n: Graveyard-cast permission riders ("If a
            // spell cast this way would be put into your graveyard, exile it
            // instead") are a STATIC destination rule selected here. Permanent
            // spells still resolve to the battlefield. The Invoke Calamity
            // free-cast rider is no longer read here — it is a self-scoped
            // `Moved` replacement on the spell, consulted by the pipeline when
            // the spell's stack → graveyard move is delivered below (CR 614.6).
            // Rod of Absorption's per-object linked source is the same kind of
            // STATIC destination rule and is honored here too.
            Zone::Exile
        } else if is_permanent_spell(state, entry.id) {
            // CR 608.3: Permanent spells enter the battlefield.
            Zone::Battlefield
        } else if ability
            .as_ref()
            .is_some_and(|a| a.context.additional_cost_paid)
            && state.objects.get(&entry.id).is_some_and(|o| {
                o.keywords
                    .iter()
                    .any(|k| matches!(k, crate::types::keywords::Keyword::Buyback(_)))
            })
        {
            // CR 702.27a: If the buyback cost was paid, put this spell into its
            // owner's hand instead of into that player's graveyard as it resolves.
            // Buyback appears only on instants/sorceries, so this branch is
            // unreachable for permanent spells. Does NOT redirect on counter
            // (CR 701.5a) or fizzle (CR 608.2b) — buyback applies only "as it
            // resolves."
            Zone::Hand
        } else {
            // CR 608.2n: Non-permanent spells are put into owner's graveyard.
            Zone::Graveyard
        };
        if dest == Zone::Battlefield {
            // CR 707.10f + CR 608.3f: A copy of a permanent spell becomes a token
            // permanent AS it resolves onto the battlefield — BEFORE the ETB
            // replacement pipeline matches the ZoneChange and before the
            // zone-change record snapshots is_token, so token-scoped ETB
            // replacements and enters-the-battlefield trigger filters
            // (FilterProp::Token/NonToken) correctly observe it as a token.
            // Copy-gated → no-op for every non-copy battlefield entry.
            if let Some(obj) = state.objects.get_mut(&entry.id) {
                if obj.is_copy {
                    obj.is_copy = false;
                    obj.is_token = true;
                }
            }
            // CR 614.1c + CR 608.3: Route battlefield entry through the replacement
            // pipeline so ETB replacements (saga lore counters, enter-tapped, etc.) fire.
            let mut proposed = crate::types::proposed_event::ProposedEvent::zone_change(
                entry.id,
                Zone::Stack,
                Zone::Battlefield,
                None,
            );
            // CR 601.2a + CR 110.2 + CR 110.2a (GitHub #696): A cast permanent's
            // controller defaults to whoever cast it, not the card's owner —
            // "that player becomes its controller" (CR 601.2a) when the spell is
            // put on the stack, and per CR 110.2a "that object enters the
            // battlefield under that player's control unless the effect
            // states otherwise." `entry.controller` is the actual caster
            // (stamped at announce_spell_on_stack from the real
            // GameAction::CastSpell dispatch), fixed for the spell's lifetime
            // on the stack. This is a no-op for the overwhelmingly common
            // owner==caster case. A genuine self-ETB "enters under [X]'s
            // control" replacement (enters_under) still wins — it runs later,
            // in replace_event below, and hard-overwrites this default
            // unconditionally.
            if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                controller_override,
                ..
            } = &mut proposed
            {
                *controller_override = Some(entry.controller);
            }
            // CR 702.190b: Sneak-cast permanent enters the battlefield tapped.
            // Seed the ZoneChange so ETB-tapped goes through the replacement
            // pipeline (CR 614.1c).
            if matches!(casting_variant, CastingVariant::Sneak { .. }) {
                if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                    enter_tapped,
                    ..
                } = &mut proposed
                {
                    *enter_tapped = crate::types::proposed_event::EtbTapState::Tapped;
                }
            }
            // CR 712.14a + CR 310.12b: If this spell was finalized from an
            // ExileWithAltCost permission with `cast_transformed`, the permanent
            // enters the battlefield transformed (resolving to its back face).
            // The finalized stack-paid snapshot is authoritative here; the
            // mutable permission list is casting-time authorization, not
            // resolution-time cast metadata.
            if let Some(obj) = state.objects.get(&entry.id) {
                // CR 107.3m + CR 707.10: a resolving copied spell has no new
                // payment snapshot, but inherits the original spell's chosen
                // X on its stack object. Off-stack entry paths pass `None`.
                let resolving_spell_x = paid_snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.x_value)
                    .or(obj.cost_x_paid);
                let cast_transformed = paid_snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.cast_transformed);
                if cast_transformed {
                    if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                        enter_transformed,
                        ..
                    } = &mut proposed
                    {
                        *enter_transformed = true;
                    }
                }
                // CR 306.5b + CR 310.4b + CR 614.1c: Planeswalkers and battles
                // have the intrinsic replacement "This permanent enters with N
                // [loyalty/defense] counters on it." Seed these counters onto
                // the ZoneChange ProposedEvent so Doubling-Season-class
                // AddCounter replacements (CR 614.1a) see and modify them as
                // the replacement pipeline runs.
                // CR 712.14a: For cast_transformed (Craft / ExileWithAltCost) the
                // spell is on the stack with the front face but enters as the back
                // face — read loyalty/defense from the back face directly so the
                // replacement pipeline sees the correct counter count.
                let intrinsic = match (cast_transformed, obj.back_face.as_ref()) {
                    (true, Some(back)) => super::printed_cards::intrinsic_entry_counters_for_face(
                        back.printed_loyalty,
                        back.loyalty,
                        resolving_spell_x,
                        back.defense,
                        &back.card_types,
                    ),
                    _ => super::printed_cards::intrinsic_etb_counters(obj, resolving_spell_x),
                };
                if !intrinsic.is_empty() {
                    if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                        enter_with_counters,
                        ..
                    } = &mut proposed
                    {
                        enter_with_counters.extend(intrinsic);
                    }
                }
            }

            // CR 702.176a: Impending — seed the N time counters into the ZoneChange
            // ProposedEvent BEFORE the replacement pipeline so Doubling Season and
            // similar counter-doubling replacements (CR 614.1a) can modify them.
            // N is read from the `Keyword::Impending { counters, .. }` on the still-
            // stack-resident object; `cast_variant_paid = Impending` is already stamped
            // by `finalize_cast_to_stack` in `casting_costs.rs`.
            if casting_variant == CastingVariant::Impending {
                let impending_counters = state.objects.get(&entry.id).and_then(|obj| {
                    obj.keywords.iter().find_map(|k| match k {
                        crate::types::keywords::Keyword::Impending { counters, .. } => {
                            Some(*counters)
                        }
                        _ => None,
                    })
                });
                if let Some(n) = impending_counters {
                    if n > 0 {
                        if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                            enter_with_counters,
                            ..
                        } = &mut proposed
                        {
                            enter_with_counters.push((CounterType::Time, n));
                        }
                    }
                }
            }

            // CR 702.188a: Web-slinging is a casting alternative cost. Tag the
            // permanent BEFORE the ETB replacement pipeline runs so a
            // `ReplacementCondition::CastVariantPaid` gate (Scarlet Spider's
            // "Sensational Save" enters-with-counters replacement) can read it.
            // `cast_variant_paid` is also written post-resolution for other
            // variants (Sneak/Evoke/Escape), but those have no ETB-replacement
            // gate; web-slinging does, so its write must precede `replace_event`.
            if let CastingVariant::WebSlinging { .. } = casting_variant {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::WebSlinging,
                        state.turn_number,
                    ));
                }
            }

            // CR 702.33d + CR 400.7d + CR 603.4: Normalize the authoritative
            // cast-link provenance onto the stack object BEFORE `replace_event`,
            // so the pipeline's `CastLinkSnapshot` (captured inside
            // `deliver_replaced_zone_change` just before `reset_for_battlefield_entry`
            // clears it per CR 400.7) sees the correct kicker / additional-cost /
            // cast-from-zone values and restores them onto the resulting permanent.
            // The resolving spell's `SpellContext` is authoritative when present;
            // placeholder permanent spells (vanilla / ETB-only creatures with no
            // on-resolve Spell ability) have `ability == None`, so the stack
            // object's already-stamped value is left untouched.
            if let Some(ability) = ability.as_ref() {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.kickers_paid = ability.context.kickers_paid.clone();
                    obj.gift_recipient = ability.context.gift_recipient;
                    obj.additional_cost_payment_count =
                        ability.context.additional_cost_payment_count;
                    obj.additional_cost_payments = ability.context.additional_cost_payments.clone();
                    // CR 400.7d: carry the object paid as a cost to cast this
                    // spell (e.g. the emerge-sacrificed creature) onto the stack
                    // object so the `CastLinkSnapshot` restores it onto the
                    // resulting permanent (Adipose Offspring). `cost_paid_object`
                    // is a field on the resolving `ResolvedAbility` itself, not
                    // on its `context`.
                    obj.cast_cost_paid_object = ability.cost_paid_object.clone();
                    if let Some(cast_from_zone) = ability.context.cast_from_zone {
                        obj.cast_from_zone = Some(cast_from_zone);
                    }
                    obj.cast_controller =
                        ability.context.cast_controller.or(Some(entry.controller));
                }
            }
            match super::replacement::replace_event(state, proposed, events) {
                super::replacement::ReplacementResult::Execute(event) => {
                    if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                        object_id,
                        to,
                        ..
                    } = &event
                    {
                        let object_id = *object_id;
                        let to = *to;
                        // CR 608.3 + 608.2c: Stack-residency guard — see
                        // `spell_still_on_stack`. If `execute_effect` already
                        // moved the spell off the stack via a self-targeted
                        // sub-ability (e.g., a permanent spell whose
                        // resolution self-exiles), skip the default
                        // Stack→Battlefield move and the ETB bookkeeping that
                        // would attach to it. The spell is in its
                        // self-chosen destination — applying ETB-tapped /
                        // counter / transform state to a non-battlefield
                        // zone is meaningless and would corrupt the object.
                        if spell_still_on_stack(state, object_id) {
                            // CR 608.3 + CR 614.1c: The ETB replacement consult
                            // already ran above (`replace_event`); seal the
                            // post-replacement `ZoneChange` with the third mint
                            // path so the shared `zone_pipeline::deliver` tail
                            // applies the entry (move + enter-tapped /
                            // controller-override / enter-with-counters /
                            // enter-transformed / face-down / devour /
                            // EntersWithAdditionalCounters statics / pending ETB
                            // counters), restoring the CR 400.7d cast-link family
                            // via `CastLinkSnapshot` from the values normalized
                            // onto the stack object above. `CallerEpilogue` keeps
                            // the CR 614.12a `post_replacement_continuation` drain
                            // owned by the caller epilogue below (mirrors the
                            // replacement-choice resume path), so the Siege /
                            // Tribute prompt is not double-drained.
                            let Ok(approved) =
                                zone_pipeline::ApprovedZoneChange::approve_post_replacement(event)
                            else {
                                unreachable!("matched ProposedEvent::ZoneChange above");
                            };
                            match zone_pipeline::deliver(
                                state,
                                approved,
                                zone_pipeline::DeliveryCtx {
                                    source_id: None,
                                    exile_links: zone_pipeline::ExileLinkSpec::default(),
                                    drain:
                                        crate::types::game_state::PostReplacementDrainOwner::CallerEpilogue,
                                    // Spell resolution delivers to the battlefield
                                    // or graveyard — never a library placement.
                                    library_placement: None,
                                },
                                events,
                            ) {
                                zone_pipeline::ZoneDeliveryResult::Done => {}
                                // CR 614.1c / CR 616.1 / CR 614.12a: the delivery
                                // tail parked a mid-entry choice (CopyTargetChoice,
                                // NamedChoice, counter branch, …) and stashed the
                                // remaining tail. Surface it without running the
                                // caller epilogue — including CR 608.3c Aura attach,
                                // which has not run yet.
                                //
                                // CR 608.3c + CR 400.7d: stash PendingSpellResolution
                                // so the choice-answer resume can complete Aura
                                // attachment / cast-link stamps — mirrors the
                                // ReplacementResult::NeedsChoice arm below.
                                zone_pipeline::ZoneDeliveryResult::NeedsChoice(_) => {
                                    state.push_spell_resolution(pending_spell_resolution_snapshot(
                                        state,
                                        &entry,
                                        ability.as_deref(),
                                        casting_variant,
                                        actual_mana_spent,
                                        &spell_targets,
                                    ));
                                    events.push(GameEvent::StackResolved {
                                        object_id: entry.id,
                                    });
                                    state.current_trigger_event = None;
                                    state.current_trigger_events.clear();
                                    state.current_trigger_match_count = None;
                                    state.die_result_this_resolution = None;
                                    return;
                                }
                            }
                            // CR 702.146b / CR 702.162a + CR 712.11a + CR
                            // 712.13: Disturb and MTMTE put the spell on the
                            // stack with its back face up. A resolving DFC
                            // spell becomes a permanent with the same face up;
                            // mark the battlefield object transformed without
                            // swapping faces again. Casting-variant-specific, so
                            // it stays caller-side (the pipeline tail only knows
                            // the generic `enter_transformed` face-swap).
                            if matches!(
                                casting_variant,
                                CastingVariant::MoreThanMeetsTheEye | CastingVariant::Disturb
                            ) && to == Zone::Battlefield
                            {
                                let mut marked = false;
                                if let Some(obj) = state.objects.get_mut(&object_id) {
                                    if obj.back_face.is_some() && !obj.transformed {
                                        obj.transformed = true;
                                        marked = true;
                                    }
                                }
                                if marked {
                                    crate::game::layers::mark_layers_full(state);
                                    events.push(GameEvent::Transformed { object_id });
                                }
                            }
                        }
                    }
                    // CR 400.7d + CR 603.4: The cast-link family (cast_from_zone,
                    // cast_timing_permission, convoked_creatures, kickers_paid,
                    // additional_cost_payment_count) is now restored structurally
                    // inside `zone_pipeline::deliver` via `CastLinkSnapshot`,
                    // captured from the values normalized onto the stack object
                    // before `replace_event`. Only the exile-link push and the
                    // CR 709.5c room-door unlock remain caller-side here.
                    if spell_in_zone(state, entry.id, Zone::Battlefield) {
                        if let Some(exiled_id) = ability
                            .as_ref()
                            .and_then(|ability| ability.cost_paid_object.as_ref())
                            .map(|snapshot| snapshot.object_id)
                            .filter(|exiled_id| {
                                state
                                    .objects
                                    .get(exiled_id)
                                    .is_some_and(|obj| obj.zone == Zone::Exile)
                            })
                        {
                            if !state.exile_links.iter().any(|link| {
                                link.source_id == entry.id && link.exiled_id == exiled_id
                            }) {
                                state.exile_links.push(ExileLink {
                                    exiled_id,
                                    source_id: entry.id,
                                    kind: ExileLinkKind::UntilSourceLeaves {
                                        return_zone: Zone::Hand,
                                    },
                                });
                            }
                        }
                        // CR 709.5d: a Room permanent enters with the unlocked
                        // designation for whichever half was cast as a spell — the
                        // right door when its right half was cast, otherwise the
                        // left. `room::live_face_door` reads `modal_back_face`
                        // (still set on the battlefield, see zones.rs), the shared
                        // orientation authority with the unlock-cost lookup.
                        let cast_door = state
                            .objects
                            .get(&entry.id)
                            .map(super::room::live_face_door)
                            .unwrap_or(crate::game::game_object::RoomDoor::Left);
                        super::room::unlock_door_designation(
                            state,
                            entry.id,
                            entry.controller,
                            cast_door,
                            events,
                        );
                    }
                    // CR 614.12a post-replacement drain runs AFTER CR 608.3c Aura
                    // attach below — PersistChosenAttribute needs `attached_to`
                    // before the choice is answered (mirrors dig/CR 303.4f).
                }
                super::replacement::ReplacementResult::Prevented => {
                    // CR 608.3e: Permanent spell's ETB was fully prevented —
                    // the card goes to owner's graveyard instead. Stack-residency
                    // guard (`spell_still_on_stack`): if the spell already
                    // self-moved during `execute_effect` (e.g., a permanent
                    // whose resolution self-exiles before its ETB would have
                    // resolved), skip the prevented-ETB graveyard fallback so
                    // the self-chosen destination is honored (issue #323
                    // class).
                    //
                    // CR 614.6: the prevented permanent's graveyard fallback now
                    // routes through the pipeline, so board-wide RIP/Leyline
                    // graveyard→exile redirects fire. On a CR 616.1 ordering
                    // pause (two simultaneous redirects), the move is parked;
                    // bail with the standard pause epilogue so the
                    // replacement-choice resume path delivers it. (The post-tail
                    // below is all `spell_in_zone(Battlefield)`-gated, so it is a
                    // no-op for a parked-on-stack spell regardless.)
                    match move_prevented_permanent_spell_to_graveyard_if_still_on_stack(
                        state, entry.id, events,
                    ) {
                        ZoneMoveResult::Done => {}
                        ZoneMoveResult::NeedsChoice(_)
                        | ZoneMoveResult::NeedsAuraAttachmentChoice => {
                            events.push(GameEvent::StackResolved {
                                object_id: entry.id,
                            });
                            state.current_trigger_event = None;
                            state.current_trigger_events.clear();
                            state.current_trigger_match_count = None;
                            state.die_result_this_resolution = None;
                            return;
                        }
                    }
                }
                super::replacement::ReplacementResult::NeedsChoice(player) => {
                    // A replacement needs player choice (e.g., Clone "enter as a copy").
                    // Store context so handle_replacement_choice can complete post-resolution.
                    // CR 702.33d + CR 400.7d: Use the authoritative kicker payments
                    // (resolving spell's `SpellContext` when present, else the stack
                    // object's stamped value) so placeholder permanent spells with
                    // `ability == None` are not silently de-kicked when a replacement
                    // needs a player choice. `engine_replacement` restores this onto
                    // the permanent unconditionally after the choice resolves.
                    state.push_spell_resolution(pending_spell_resolution_snapshot(
                        state,
                        &entry,
                        ability.as_deref(),
                        casting_variant,
                        actual_mana_spent,
                        &spell_targets,
                    ));
                    state.waiting_for =
                        super::replacement::replacement_choice_waiting_for(player, state);
                    // Emit StackResolved now — the spell has left the stack even though
                    // the replacement choice is pending.
                    events.push(GameEvent::StackResolved {
                        object_id: entry.id,
                    });
                    state.current_trigger_event = None;
                    state.current_trigger_events.clear();
                    state.current_trigger_match_count = None;
                    // CR 706.2 + CR 706.4: clear the carried die-roll result at
                    // the same cross-resolution boundary as the batched subject
                    // count.
                    state.die_result_this_resolution = None;
                    return;
                }
            }
        } else {
            // CR 608.2n: "As the final part of an instant or sorcery spell's
            // resolution, the spell is put into its owner's graveyard."
            // Stack-residency guard (`spell_still_on_stack`): if the spell's
            // own instructions already moved it off the Stack (e.g., Treasured
            // Find / Arc Blade — "Exile ~", or any sub-ability that targets
            // the source via `SelfRef`), the post-resolution default move must
            // be skipped — otherwise the spell card travels exile→graveyard
            // and undoes its own self-exile clause (issue #323).
            if spell_still_on_stack(state, entry.id) {
                // CR 608.2n + CR 614.6: route the spell's stack → graveyard/exile
                // default move through the pipeline so self-scoped `Moved`
                // redirects (the Invoke Calamity rider) and board-wide
                // RIP/Leyline redirects fire (PLAN §8 Risk #2 — confirmed bug on
                // the old raw-move path). A redirect only matches a Graveyard
                // destination, so flashback/adventure/omen spells (dest already
                // Exile/Library) never engage it. On a CR 616.1 ordering choice
                // (two simultaneous Graveyard→Exile redirects on the same spell),
                // `move_object` parks the prompt; the spell is already off the
                // stack and the dest is Graveyard, so every post-move bookkeeping
                // step below is a no-op (front-face restore / Adventure / Omen /
                // battlefield-entry tail all gate on non-graveyard zones). Mirror
                // the permanent-spell NeedsChoice arm: emit StackResolved + clear
                // trigger context, then bail so the replacement-choice resume
                // path delivers the redirected move.
                let stack_exile_link_source = stack_exile_linked_source(state, entry.id);
                // CR 603.7a + CR 702.170c: snapshot the exile-instead
                // consequence rider BEFORE the move — every zone exit clears the
                // transient rider fields (zones.rs), so the post-move apply site
                // below must read the pre-move value.
                let exile_rider = state
                    .objects
                    .get(&entry.id)
                    .and_then(|o| o.exile_from_stack_rider.clone());
                let req = ZoneMoveRequest::spell_resolution_default(entry.id, dest);
                match zone_pipeline::move_object(state, req, events) {
                    ZoneMoveResult::Done => {
                        // CR 607.2b + CR 406.6: a spell exiled by Rod of
                        // Absorption's per-object linked-source rider is "exiled
                        // with" the trigger source that stamped it. Now that the
                        // pipeline has delivered the move, record the linked-exile
                        // association so the source's linked ability ("cast any
                        // number of cards exiled with this artifact") sees the
                        // accumulating set.
                        // Gate on the object's ACTUAL post-move zone (not the
                        // requested `dest`) so a redirect that diverted the card
                        // away from exile never records a spurious link, while a
                        // redirect INTO exile still records correctly.
                        if spell_in_zone(state, entry.id, Zone::Exile) {
                            if let Some(link_source) = stack_exile_link_source {
                                super::exile_links::push_tracked_by_source(
                                    state,
                                    entry.id,
                                    link_source,
                                );
                            }
                            // CR 603.7a + CR 702.170c: the exile-instead
                            // replacement has now actually been APPLIED (the
                            // spell landed in exile), so this is the moment the
                            // "If you do, ..." consequence is applied — Feather's
                            // return-to-hand delayed trigger, or Lilah's plotted
                            // grant — never earlier (a countered or fizzled
                            // spell's marker was cleared on its stack exit and
                            // never reaches here).
                            if let Some(rider) = exile_rider {
                                effects::exile_resolving_spell::apply_exile_rider(
                                    state,
                                    entry.id,
                                    entry.controller,
                                    stack_exile_link_source.unwrap_or(entry.id),
                                    rider,
                                    events,
                                );
                            }
                        }
                    }
                    ZoneMoveResult::NeedsChoice(_) | ZoneMoveResult::NeedsAuraAttachmentChoice => {
                        // NOTE: the `exile_rider` snapshot is intentionally
                        // dropped on this bail — a parked move here can only be
                        // a Graveyard-destination replacement-ordering prompt
                        // (RIP/Leyline redirects match Graveyard destinations
                        // only), so no stack→Exile move that could apply the
                        // consequence rider can currently park. If a stack→Exile
                        // replacement choice is ever added, the rider must be
                        // carried through the pending-resolution resume path.
                        events.push(GameEvent::StackResolved {
                            object_id: entry.id,
                        });
                        state.current_trigger_event = None;
                        state.current_trigger_events.clear();
                        state.current_trigger_match_count = None;
                        state.die_result_this_resolution = None;
                        return;
                    }
                }
            }
        }

        // CR 400.7 + CR 712.11a: face-swapped stack spells revert to front
        // face when leaving the stack unless they resolved as that face onto
        // the battlefield.
        if casting_variant.restores_front_face_after_stack_exit()
            && !spell_in_zone(state, entry.id, Zone::Battlefield)
        {
            restore_alternative_spell_normal_face(state, entry.id);
        }

        // CR 715.3d: When an Adventure spell resolves to exile, grant
        // AdventureCreature permission so it can be cast from exile.
        if casting_variant == CastingVariant::Adventure {
            if let Some(obj) = state.objects.get_mut(&entry.id) {
                obj.casting_permissions
                    .push(crate::types::ability::CastingPermission::AdventureCreature);
            }
        }
        if casting_variant == CastingVariant::Omen {
            if let Some(owner) = state
                .objects
                .get(&entry.id)
                .filter(|obj| obj.zone == Zone::Library)
                .map(|obj| obj.owner)
            {
                effects::change_zone::shuffle_library(state, owner, events);
            }
        }

        // CR 608.3c: An Aura spell resolving becomes a permanent put onto the
        // battlefield attached to the player or object it was targeting.
        // (NOT CR 303.4f, which explicitly governs Auras entering "by any means
        // other than by resolving as an Aura spell.")
        if spell_in_zone(state, entry.id, Zone::Battlefield) {
            let is_aura = state
                .objects
                .get(&entry.id)
                .map(|obj| obj.card_types.subtypes.iter().any(|s| s == "Aura"))
                .unwrap_or(false);
            if is_aura {
                match spell_targets.first() {
                    // CR 608.3c + CR 608.2b: Object Aura — verify the target is
                    // still a legal host per the Aura's own zone-scoped enchant
                    // ability (`is_valid_attachment_target`, the single legality
                    // authority shared with `attach::resolve` and the SBA
                    // re-check). A battlefield-only Enchant filter still requires
                    // battlefield presence; a graveyard-scoped filter (Animate
                    // Dead) legally accepts a graveyard host. A now-illegal target
                    // leaves the Aura unattached and SBA (CR 704.5m) cleans it up
                    // at the next checkpoint.
                    //
                    // CR 608.3c / CR 303.4a: the host is the spell's chosen
                    // target — never re-consult the Enchant filter (CR 303.4f
                    // non-spell entry) when that target is missing.
                    Some(crate::types::ability::TargetRef::Object(target_id))
                        if crate::game::sba::is_valid_attachment_target(
                            state, entry.id, *target_id,
                        ) =>
                    {
                        effects::attach::attach_to(state, entry.id, *target_id);
                    }
                    Some(crate::types::ability::TargetRef::Object(_)) => {
                        // Target is no longer a legal host — SBA cleanup follows.
                    }
                    // CR 608.3c + CR 702.5d: Player Aura (Curse cycle, Faith's
                    // Fetters-class). Validity check is "player still in game"
                    // — `attach_to_player` makes no liveness check itself, but
                    // `check_unattached_auras` (CR 303.4c) will detach + grave
                    // a Curse whose enchanted player has left the game.
                    Some(crate::types::ability::TargetRef::Player(player_id)) => {
                        effects::attach::attach_to_player(state, entry.id, *player_id);
                    }
                    None => {
                        // CR 303.4g: An Aura entering the battlefield with no
                        // legal target goes to its owner's graveyard. The SBA
                        // path catches this on the next pass.
                    }
                }
            }

            // CR 614.12a: Drain mandatory replacement post-effects (Siege /
            // Tribute opponent-choice, Metamorphic ChoosePermanent
            // CopyTargetChoice, …) stashed while resolving this permanent's
            // ZoneChange. `CallerEpilogue` skipped the DeliveryTail drain, so
            // this site owns the prompt — AFTER CR 608.3c Aura attach above so
            // the Aura is hosted before SBAs / the copy-choice answer, while
            // `PendingSpellResolution.spell_targets` still carries the cast
            // target for the PersistChosenAttribute resume (CR 608.3c /
            // CR 303.4a). Do not push SpellResolution on top of an
            // AbilityContinuation (Tribute/Siege resume is top-only).
            if state.has_post_replacement_drain() {
                state.clear_post_replacement_source();
                if let Some(wf) = super::engine_replacement::apply_pending_post_replacement_effect(
                    state,
                    Some(entry.id),
                    None,
                    Some(crate::types::replacements::ReplacementEvent::Moved),
                    events,
                ) {
                    match wf {
                        // CR 608.3c + CR 614.12a: stash the Aura spell's chosen
                        // host for the copy-choice answer path, then surface the
                        // prompt. Continue the cast-variant epilogue (same as
                        // Tribute NamedChoice) so resolve_top settles normally;
                        // the answer path still prefers spell_targets.
                        WaitingFor::CopyTargetChoice { .. } => {
                            state.push_spell_resolution(pending_spell_resolution_snapshot(
                                state,
                                &entry,
                                ability.as_deref(),
                                casting_variant,
                                actual_mana_spent,
                                &spell_targets,
                            ));
                            state.waiting_for = wf;
                        }
                        WaitingFor::Priority { .. } => {}
                        other => {
                            // Tribute / Siege NamedChoice — surface the prompt
                            // and continue the caller epilogue. Do not push
                            // SpellResolution on top of an AbilityContinuation.
                            state.waiting_for = other;
                        }
                    }
                }
            }

            // CR 702.185a: Warp — when a permanent cast via Warp resolves to the battlefield,
            // create a delayed trigger to exile it at end step with WarpExile permission.
            // Only triggers on the initial Warp cast (CastingVariant::Warp), NOT on re-casts
            // from exile (which use CastingVariant::Normal and stay permanently).
            if casting_variant == CastingVariant::Warp {
                let has_warp = state.objects.get(&entry.id).is_some_and(|obj| {
                    obj.keywords
                        .iter()
                        .any(|k| matches!(k, crate::types::keywords::Keyword::Warp(_)))
                });
                if has_warp {
                    create_warp_delayed_trigger(state, entry.id, entry.controller, events);
                }
                // CR 702.185a + CR 400.7: stamp the per-object warp marker after
                // `reset_for_battlefield_entry` cleared it, mirroring the Evoke /
                // Impending / Suspend stamps below. Read by the target-scoped
                // "if that creature was cast for its warp cost" rider (Full Bore)
                // via `AbilityCondition::CastVariantPaid { subject: Target }`. The
                // marker rides this incarnation only — a zone change makes a new
                // object (CR 400.7) and re-casts from exile use
                // `CastingVariant::Normal`, so the warp tag never persists past
                // the cast turn's end-step exile.
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::Warp,
                        state.turn_number,
                    ));
                }
            }

            // CR 702.190b: Sneak-cast permanent enters tapped (already seeded on
            // the ZoneChange replacement) AND attacking the same defender as the
            // returned creature. Placement is `Some` only for permanent spells;
            // non-permanent Sneak casts (instants/sorceries) resolve normally.
            // Also tag `cast_variant_paid` so the `CastVariantPaid { variant:
            // Sneak }` trigger/ability condition fires on resolved Sneak casts
            // regardless of card type.
            if let CastingVariant::Sneak { placement, .. } = casting_variant {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::Sneak,
                        state.turn_number,
                    ));
                }
                if let Some(p) = placement {
                    super::combat::place_attacking_alongside(
                        state,
                        entry.id,
                        p.defender,
                        p.attack_target,
                        events,
                    );
                }
            }

            // CR 702.188a: Web-slinging's `cast_variant_paid` tag is written
            // before `replace_event` above (so the ETB-replacement gate can
            // read it) — no post-resolution write is needed here.

            // CR 702.74a: Evoke-cast permanent gets the `cast_variant_paid` tag
            // so the synthesized intervening-if ETB sacrifice trigger fires.
            if casting_variant == CastingVariant::Evoke {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::Evoke,
                        state.turn_number,
                    ));
                    // CR 702.74a + CR 611.2 + CR 604.1: install the ETB-sac on
                    // the resolving permanent for granted evoke (keyword lived
                    // on the spell, not the permanent). Idempotent no-op for
                    // printed evoke (already baked into the card face by
                    // `synthesize_evoke`); `process_triggers` later in
                    // `run_post_action_pipeline` reads the live
                    // `trigger_definitions` after the zone change buffers.
                    crate::database::synthesis::ensure_evoke_etb_sac_trigger(obj);
                }
            }
            if let Some(obj) = state.objects.get_mut(&entry.id) {
                crate::database::synthesis::ensure_paid_offspring_etb_copy_triggers(obj);
            }

            // CR 702.103a + CR 702.103b: Bestow-cast permanent gets the
            // `cast_variant_paid` tag so future "if its bestow cost was paid"
            // triggers/conditions can evaluate against the resolved permanent.
            // Tag is set whether the bestow form persisted (legal target →
            // Aura attached) or was reverted at resolution (CR 702.103e
            // illegal-target → resolved as creature) — the audit trail is the
            // *cost* paid, not the form at ETB.
            if casting_variant == CastingVariant::Bestow {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::Bestow,
                        state.turn_number,
                    ));
                }
            }

            // CR 702.138b: Escape-cast permanent is tagged so the "unless it
            // escaped" intervening-if on Phlage, Titan of Fire's Fury (and any
            // future escape-gated ETB trigger) can distinguish escape casts
            // from hard-casts and reanimation. Per CR 702.138b: "A spell or
            // permanent 'escaped' if that spell ... was cast from a graveyard
            // with an escape ability."
            if casting_variant == CastingVariant::Escape {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::Escape,
                        state.turn_number,
                    ));
                }
            }

            // CR 702.117a: Surge-cast permanent is tagged so "if its surge cost
            // was paid" ETB triggers (Reckless Bushwhacker, Tyrant of Valakut)
            // can distinguish a surge cast from a hard-cast. The intervening-if
            // re-checks at resolution (CR 603.4) and the marker must be present.
            if casting_variant == CastingVariant::Surge {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::Surge,
                        state.turn_number,
                    ));
                }
            }

            // CR 702.137a: Spectacle-cast permanent is tagged so "if its
            // spectacle cost was paid" ETB triggers (Rafter Demon) and
            // "...instead" clauses (Rix Maadi Reveler) can distinguish a
            // spectacle cast from a hard-cast.
            if casting_variant == CastingVariant::Spectacle {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::Spectacle,
                        state.turn_number,
                    ));
                }
            }

            // CR 702.76a: Prowl-cast permanent is tagged so "if its prowl cost
            // was paid" ETB triggers (Latchkey Faerie) can distinguish a prowl
            // cast from a hard-cast. The intervening-if re-checks at resolution
            // (CR 603.4) and the marker must be present.
            if casting_variant == CastingVariant::Prowl {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::Prowl,
                        state.turn_number,
                    ));
                }
            }

            // CR 702.176a: Impending-cast permanent gets the `cast_variant_paid`
            // tag re-applied after `reset_for_battlefield_entry` cleared it.
            // The "not a creature" layer fixup and the end-step counter-removal
            // trigger both gate on this marker being present.
            if casting_variant == CastingVariant::Impending {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::Impending,
                        state.turn_number,
                    ));
                }
            }

            // CR 702.62a: Suspend-cast permanent gets the `cast_variant_paid`
            // tag for symmetry with Evoke / Sneak (no synthesized trigger reads
            // it today, but it preserves the audit trail). Additionally, when
            // the resolving spell was a creature, install a transient
            // continuous "has haste" effect that lapses the moment another
            // player gains control of the permanent
            // (CR 702.62a final sentence: "If you cast a creature spell this
            // way, it gains haste until you lose control of the spell or the
            // permanent it becomes."). The layer-6 keyword grant is scoped to
            // the resolving permanent via `TargetFilter::SpecificObject` and
            // gated by `Duration::ForAsLongAs { SourceControllerEquals }` —
            // a Threaten-style control swap flips the predicate false and the
            // static is gathered out of layer evaluation.
            if casting_variant == CastingVariant::Suspend {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::Suspend,
                        state.turn_number,
                    ));
                }

                let is_creature = state
                    .objects
                    .get(&entry.id)
                    .is_some_and(|obj| obj.card_types.core_types.contains(&CoreType::Creature));
                if is_creature {
                    let resolution_controller = entry.controller;
                    let suspended_id = entry.id;
                    state.add_transient_continuous_effect(
                        suspended_id,
                        resolution_controller,
                        Duration::ForAsLongAs {
                            condition:
                                crate::types::ability::StaticCondition::SourceControllerEquals {
                                    player: resolution_controller,
                                },
                        },
                        crate::types::ability::TargetFilter::SpecificObject { id: suspended_id },
                        vec![ContinuousModification::AddKeyword {
                            keyword: crate::types::keywords::Keyword::Haste,
                        }],
                        None,
                    );
                }
            }

            // CR 702.119a-c: Emerge-cast permanent is tagged so "if its emerge
            // cost was paid" ETB instead-clauses (Adipose Offspring) can
            // distinguish an emerge cast from a hard-cast. CR 603.4 re-checks at
            // resolution; the marker is read by
            // `AbilityCondition::CastVariantPaid` / `CastVariantPaidInstead`.
            if casting_variant == CastingVariant::Emerge {
                if let Some(obj) = state.objects.get_mut(&entry.id) {
                    obj.cast_variant_paid = Some((
                        crate::types::ability::CastVariantPaid::Emerge,
                        state.turn_number,
                    ));
                }
            }

            // CR 702.109a: a dash-cast permanent gains haste and is returned to
            // its owner's hand at the beginning of the next end step.
            if casting_variant == CastingVariant::Dash {
                crate::game::dash::install_dash_riders(state, entry.id, entry.controller, events);
            }
            // CR 702.152a: a blitz-cast permanent gains haste and a dies-draw
            // trigger, and is sacrificed at the beginning of the next end step.
            if casting_variant == CastingVariant::Blitz {
                crate::game::blitz::install_blitz_riders(state, entry.id, entry.controller, events);
            }
        }
    }
    // Activated abilities: source stays where it is, no zone movement

    // CR 603.7c: Clear trigger event context after resolution completes.
    state.current_trigger_event = None;
    state.current_trigger_events.clear();
    state.current_trigger_match_count = None;
    // CR 706.2 + CR 706.4: clear the carried die-roll result at the same
    // cross-resolution boundary as the batched subject count.
    state.die_result_this_resolution = None;

    events.push(GameEvent::StackResolved {
        object_id: entry.id,
    });
    // CR 608.2p: "Once all possible steps described in 608.2c–n are completed,
    // any abilities that trigger when that spell or ability resolves trigger."
    // This is the only exit from `resolve_top` on which a triggered ability
    // actually RESOLVED — the fizzle, no-legal-target and failed-intervening-if
    // paths returned earlier, each pushing their own `StackResolved`. Publishing
    // the chapter-resolution event only here is what keeps "whenever the final
    // chapter ability of a Saga you control resolves" (Narci, Fable Singer) from
    // firing on a chapter ability that never did.
    if let Some(chapter) = saga_chapter {
        events.push(GameEvent::SagaChapterAbilityResolved {
            saga: Box::new(chapter.saga),
            controller: chapter.controller,
            chapter: chapter.chapter,
            final_chapter: chapter.final_chapter,
        });
    }
    // The popped object remains the resolving carrier through every typed
    // resolution frame, including a direct optional-choice frame. In particular,
    // a self-moving trigger needs that carrier to establish its CR 400.7j
    // re-entry link after an accepted choice (Ajani, Nacatl Pariah).
    if super::triggers::resolution_completion_can_settle(state)
        && state.active_spell_resolution().is_none()
        && state.pending_resolution_completion.is_none()
    {
        finish_resolving_stack_entry(
            state,
            super::lifecycle::DelayedTerminalDisposition::Resolved,
        );
        state.resolution_source_relatch = None;
    }
}

/// CR 113.3b + CR 113.7a: Resolve an activated keyword ability from the stack.
///
/// The cost has already been paid at announcement. Resolution applies the
/// keyword's effect against last-known information — if a participating
/// object has left its expected zone between announcement and resolution,
/// the effect is either skipped or applied using the snapshot carried on
/// the `KeywordAction` payload (e.g. `Station::snapshot_power`).
fn resolve_keyword_action(
    state: &mut GameState,
    action: KeywordAction,
    events: &mut Vec<GameEvent>,
) {
    match action {
        // CR 702.6a: Attach source Equipment to target creature. If either
        // object has left the battlefield by resolution, the effect does nothing
        // (CR 608.2b — illegal-target check on resolution).
        KeywordAction::Equip {
            equipment_id,
            target_creature_id,
        } => {
            let still_valid = state
                .objects
                .get(&equipment_id)
                .is_some_and(|e| e.zone == Zone::Battlefield)
                && state.objects.get(&target_creature_id).is_some_and(|t| {
                    t.zone == Zone::Battlefield
                        && t.card_types.core_types.contains(&CoreType::Creature)
                });
            if still_valid {
                if let Some(old_target) =
                    effects::attach::attach_to(state, equipment_id, target_creature_id)
                {
                    events.push(GameEvent::Unattached {
                        attachment_id: equipment_id,
                        old_target,
                    });
                }
            }
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Equip,
                source_id: equipment_id,
                subject: None,
            });
        }
        // CR 702.122a: This permanent becomes an artifact creature UEOT.
        KeywordAction::Crew {
            vehicle_id,
            paid_creature_ids,
        } => {
            if let Some(v) = state.objects.get(&vehicle_id) {
                if v.zone == Zone::Battlefield {
                    let controller = v.controller;
                    state.add_transient_continuous_effect(
                        vehicle_id,
                        controller,
                        Duration::UntilEndOfTurn,
                        TargetFilter::SpecificObject { id: vehicle_id },
                        vec![ContinuousModification::AddType {
                            core_type: CoreType::Creature,
                        }],
                        None,
                    );
                }
            }
            events.push(GameEvent::VehicleCrewed {
                vehicle_id,
                creatures: paid_creature_ids,
            });
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Crew,
                source_id: vehicle_id,
                subject: None,
            });
        }
        // CR 702.171a: This permanent becomes saddled UEOT.
        // CR 702.171b: The saddled designation is stored on the GameObject and
        // cleared at end of turn or when it leaves the battlefield.
        KeywordAction::Saddle {
            mount_id,
            paid_creature_ids,
        } => {
            // CR 702.171b + CR 702.171c: single authority shared with the
            // effect-level `BecomeSaddled` path — set the designation, record the
            // saddling creatures, and emit `GameEvent::Saddled`.
            crate::game::effects::saddle::mark_saddled(state, mount_id, paid_creature_ids, events);
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Saddle,
                source_id: mount_id,
                subject: None,
            });
        }
        // CR 702.184a: Put charge counters equal to the tapped creature's power.
        // The power reading was snapshot at announcement (CR 113.7a) so this is
        // safe even if the paid creature has since left the battlefield.
        KeywordAction::Station {
            spacecraft_id,
            paid_creature_id,
            snapshot_power,
        } => {
            let counters_added = snapshot_power.max(0) as u32;
            let spacecraft_controller = state
                .objects
                .get(&spacecraft_id)
                .filter(|sc| sc.zone == Zone::Battlefield)
                .map(|sc| sc.controller);
            if let (Some(controller), true) = (spacecraft_controller, counters_added > 0) {
                if !effects::counters::add_counter_with_replacement(
                    state,
                    controller,
                    spacecraft_id,
                    CounterType::Generic("charge".to_string()),
                    counters_added,
                    events,
                ) {
                    effects::counters::stash_pending_counter_completion_with_actions(
                        state,
                        EffectKind::Station,
                        spacecraft_id,
                        vec![PendingCounterPostAction::RecordStationed {
                            spacecraft_id,
                            creature_id: paid_creature_id,
                            counters_added,
                        }],
                    );
                    return;
                }
            }
            events.push(GameEvent::Stationed {
                spacecraft_id,
                creature_id: paid_creature_id,
                counters_added,
            });
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Station,
                source_id: spacecraft_id,
                subject: None,
            });
        }
    }
}

// ── Tier 3: true batch-resolution of identical token-creating triggers ────
//
// `resolve_next` wraps `resolve_top`. When the top of the stack begins a
// contiguous run of provably-batch-safe identical triggered abilities, it
// resolves the whole run in one step that applies the effect N times — the
// same observable state and (coalesced) event sequence as one-by-one. Any
// uncertainty falls back to the unchanged `resolve_top`. Three layers gate
// eligibility: Layer A (run-identity, `BatchRunKey`), Layer B (handler purity,
// `effects::try_resolve_batch`), Layer C (observer-order-invariance,
// `observers_are_batch_safe`). See the plan trace in `effects/mod.rs`.

/// Sentinel object id used only to build Layer C probe events. `keys_from_event`
/// reads only `record.core_types`/`to` (ETB keys) and the `TokenCreated` variant
/// tag — never the `object_id` — so a sentinel is sound (§2.3 PROBE_ID note).
const PROBE_ID: ObjectId = ObjectId(u64::MAX);

/// CR 608.2: Resolve the next stack object, collapsing a batch-safe run when
/// one begins at the top. Returns the number of stack entries consumed
/// (≥ 1) so the caller can correct the auto-pass baseline (§7.2).
pub fn resolve_next(state: &mut GameState, events: &mut Vec<GameEvent>) -> u32 {
    resolve_next_with_limit(state, events, None)
}

pub fn resolve_next_with_limit(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    max_consumed: Option<u32>,
) -> u32 {
    let max_consumed = max_consumed.unwrap_or(u32::MAX).max(1);
    // CR 603.3c/d: never collapse while the top entry is mid-construction.
    let pending_top = state
        .pending_trigger_entry
        .is_some_and(|pending| state.stack.back().map(|e| e.id) == Some(pending));
    if !pending_top {
        if let Some(consumed) = inert_noop_run_len(state) {
            let consumed = consumed.min(max_consumed);
            if consumed >= 2 {
                crate::game::perf_counters::record_stack_inert_noop_batch(consumed);
                return resolve_inert_noop_batch(state, consumed, events);
            }
        }
        if let Some(run_len) = self_counter_run_len(state) {
            let run_len = run_len.min(max_consumed);
            if run_len >= 2 {
                crate::game::perf_counters::record_stack_batch_candidate();
                if let Some(consumed) = resolve_proven_self_counter_batch(state, events, run_len) {
                    return consumed;
                }
            }
        }
        if let Some(run_len) = fixed_controller_gain_life_run_len(state) {
            let run_len = run_len.min(max_consumed);
            if run_len >= 2 {
                crate::game::perf_counters::record_stack_batch_candidate();
                if let Some(consumed) =
                    resolve_proven_fixed_controller_gain_life_batch(state, events, run_len)
                {
                    return consumed;
                }
            }
        }
        if let Some(run_len) = fixed_opponent_effect_run_len(state) {
            let run_len = run_len.min(max_consumed);
            if run_len >= 2 {
                crate::game::perf_counters::record_stack_batch_candidate();
                if let Some(consumed) =
                    resolve_proven_fixed_opponent_effect_batch(state, events, run_len)
                {
                    return consumed;
                }
            }
        }
        if let Some(run_len) = batch_run_len(state) {
            let run_len = run_len.min(max_consumed);
            if run_len >= 2 {
                crate::game::perf_counters::record_stack_batch_candidate();
                // Layer B FIRST: per-handler purity produces the resolved token
                // spec(s) the Layer C probe needs (HIGH-1) and applies the
                // §2.2a/§2.3a/§3.4 gates internally.
                let ability = state.stack.back().and_then(|e| e.ability()).cloned();
                if let Some(ability) = ability {
                    // Gather the run's per-entry source ids (top-down resolution
                    // order) so the met-copy prefix path can read each entry's
                    // `SelfRef` copy source. Only the top `run_len` contiguous
                    // batch-key-equal entries form the run. This allocates only
                    // on the batch-eligible path (run_len >= 2), never on the
                    // single-resolution hot path.
                    let run_source_ids: Vec<ObjectId> = state
                        .stack
                        .iter()
                        .rev()
                        .take(run_len as usize)
                        .map(|e| e.source_id)
                        .collect();
                    // CR 603.6a + CR 611.2e: deserialize/imported states can
                    // carry an empty derived trigger index. Refresh it before
                    // Layer B so token handlers can cheaply detect broad
                    // observers that would make Layer C refuse anyway.
                    if state.trigger_index.by_key.is_empty()
                        && state.trigger_index.unclassified.is_empty()
                        && !state.battlefield.is_empty()
                    {
                        crate::types::game_state::TriggerIndex::rebuild_from_battlefield(state);
                    }
                    if let Some(plan) =
                        effects::try_resolve_batch(state, &ability, run_len, &run_source_ids)
                    {
                        crate::game::perf_counters::record_stack_batch_plan();
                        if observers_are_batch_safe(state, &plan) {
                            return resolve_batched(state, &plan, &ability, events);
                        }
                        crate::game::perf_counters::record_stack_batch_observer_refusal();
                    }
                }
            }
        }
    }
    resolve_top(state, events);
    1
}

/// Optional post-resolution invariant checked after each `resolve_top` and the
/// subsequent post-action pipeline. Shared settled/event/stack checks always
/// run; class-specific proofs add only what their effect mutates.
enum InertTriggerBatchPipelineInvariant {
    /// Pipeline must leave battlefield counters unchanged (self-counter class).
    UnchangedBattlefieldCounters,
}

/// CR 117.4 + CR 117.5 + CR 608.2 + CR 704.3: Shared authority for proving a
/// contiguous inert triggered-ability run may skip priority. Runs the exact
/// sequential `resolve_top` → post-action-pipeline path on a clone; refuses
/// when any checkpoint creates events, pushes triggers, pauses, or fails the
/// optional class invariant. Callers specialize only run-key / candidate shape.
fn resolve_proven_inert_trigger_batch(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    run_len: u32,
    pipeline_invariant: Option<InertTriggerBatchPipelineInvariant>,
) -> Option<u32> {
    if !priority_checkpoint_is_settled(state) {
        return None;
    }

    let mut proof = state.clone();
    let mut proof_events = Vec::new();
    let default_wf = WaitingFor::Priority {
        player: proof.active_player,
    };
    let initial_len = proof.stack.len();

    for expected_consumed in 1..=run_len as usize {
        let event_start = proof_events.len();
        let stack_before = proof.stack.len();
        // CR 608.2: each ability still resolves individually via `resolve_top`.
        resolve_top(&mut proof, &mut proof_events);
        if stack_before.saturating_sub(proof.stack.len()) != 1 {
            return None;
        }
        if !matches!(proof.waiting_for, WaitingFor::Priority { .. }) {
            return None;
        }

        let events_after_resolution = proof_events.len();
        let stack_after_resolution = proof.stack.len();
        let counters_after_resolution = matches!(
            pipeline_invariant,
            Some(InertTriggerBatchPipelineInvariant::UnchangedBattlefieldCounters)
        )
        .then(|| battlefield_counter_snapshot(&proof));
        // CR 117.5 + CR 704.3 + CR 603.3b: full priority checkpoint after each
        // resolution; refuse when the effect would enqueue observers or other
        // non-inert checkpoint work.
        let wf = super::engine_priority::run_post_action_pipeline_from(
            &mut proof,
            &mut proof_events,
            event_start,
            &default_wf,
            false,
            false,
        )
        .ok()?;
        if !matches!(wf, WaitingFor::Priority { .. })
            || !matches!(proof.waiting_for, WaitingFor::Priority { .. })
            || proof_events.len() != events_after_resolution
            || proof.stack.len() != stack_after_resolution
            || counters_after_resolution
                .is_some_and(|before| battlefield_counter_snapshot(&proof) != before)
            || initial_len.saturating_sub(proof.stack.len()) != expected_consumed
            || !priority_checkpoint_is_settled(&proof)
        {
            return None;
        }
    }

    proof.consumed_before_priority_trigger_events =
        consumed_trigger_event_occurrences(&proof_events);
    *state = proof;
    events.extend(proof_events);
    crate::game::perf_counters::record_stack_batch_plan();
    crate::game::perf_counters::record_stack_batched_entries(run_len);
    Some(run_len)
}

/// CR 117.4 + CR 608.2 + CR 704.3: Self-counter class — shared inert proof plus
/// an unchanged-battlefield-counters pipeline invariant.
fn resolve_proven_self_counter_batch(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    run_len: u32,
) -> Option<u32> {
    resolve_proven_inert_trigger_batch(
        state,
        events,
        run_len,
        Some(InertTriggerBatchPipelineInvariant::UnchangedBattlefieldCounters),
    )
}

fn battlefield_counter_snapshot(
    state: &GameState,
) -> Vec<(ObjectId, std::collections::HashMap<CounterType, u32>)> {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id).map(|obj| (*id, obj.counters.clone())))
        .collect()
}

fn consumed_trigger_event_occurrences(
    events: &[GameEvent],
) -> Vec<crate::game::triggers::ConsumedTriggerEventOccurrence> {
    let mut seen = std::collections::HashMap::new();
    events
        .iter()
        .map(|event| {
            let key = serde_json::to_string(event).expect("GameEvent serializes");
            let count = seen.entry(key).or_insert(0);
            let occurrence = *count;
            *count += 1;
            crate::game::triggers::ConsumedTriggerEventOccurrence {
                event: event.clone(),
                occurrence,
                scope: crate::game::triggers::ConsumedTriggerEventScope::AllCollectors,
            }
        })
        .collect()
}

/// True when resolution has reached a full priority checkpoint with no latent
/// trigger, replacement, or continuation work.  Batch consumers that prove a
/// sequence on a clone share this boundary rather than inferring safety from
/// stack depth alone.
pub(crate) fn priority_checkpoint_is_settled(state: &GameState) -> bool {
    state.pending_replacement.is_none()
        && state.pending_combat_lifelink.is_none()
        && state.pending_trigger.is_none()
        && state.pending_trigger_event_batch.is_empty()
        && state.pending_trigger_entry.is_none()
        && state.deferred_triggers.is_empty()
        && state.pending_trigger_order.is_none()
        && state.current_trigger_event.is_none()
        && state.current_trigger_events.is_empty()
        && state.current_trigger_match_count.is_none()
        && state.die_result_this_resolution.is_none()
        && state.resolution_stack.is_empty()
        && state.pending_miracle_offers.is_empty()
        && state.pending_paradigm_remaining_offers.is_none()
        && state.pending_damage_replacements.is_empty()
        && state.pending_step_end_mana_handlers.is_empty()
        && state.pending_phase_transition_progress.is_none()
        && state.deferred_step_trigger_resume.is_none()
        && state.pending_team_draw_step.is_empty()
        && state.pending_untap_declines.is_empty()
}

#[derive(PartialEq)]
struct SelfCounterRunKey<'a> {
    source_id: ObjectId,
    controller: PlayerId,
    ability: &'a ResolvedAbility,
    description: Option<&'a str>,
    paid: Option<&'a StackPaidSnapshot>,
}

/// CR 603.3b + CR 608.2 + CR 122.1: Length of the top contiguous run of
/// identical triggered abilities that put one +1/+1 counter on their own
/// source. The firing event is intentionally not part of this key: this gate
/// accepts only an event-context-free effect shape, and the clone proof below
/// still resolves every entry with its exact trigger context before committing.
fn self_counter_run_len(state: &GameState) -> Option<u32> {
    let top = state.stack.back()?;
    let top_key = self_counter_run_key(state, top)?;
    let mut len = 1u32;
    for entry in state.stack.iter().rev().skip(1) {
        match self_counter_run_key(state, entry) {
            Some(key) if key == top_key => len += 1,
            _ => break,
        }
    }
    Some(len)
}

fn self_counter_run_key<'a>(
    state: &'a GameState,
    entry: &'a StackEntry,
) -> Option<SelfCounterRunKey<'a>> {
    let StackEntryKind::TriggeredAbility {
        source_id,
        ability,
        condition,
        trigger_event: _,
        description,
        source_name: _,
        subject_match_count: _,
        die_result: _,
        provenance: None,
    } = &entry.kind
    else {
        return None;
    };

    if *source_id != entry.source_id
        || ability.source_id != *source_id
        || condition.is_some()
        || !flatten_targets_in_chain(ability).is_empty()
        || !self_counter_ability_is_batch_candidate(ability)
    {
        return None;
    }

    Some(SelfCounterRunKey {
        source_id: *source_id,
        controller: entry.controller,
        ability,
        description: description.as_deref(),
        paid: state.stack_paid_facts.get(&entry.id),
    })
}

fn self_counter_ability_is_batch_candidate(ability: &ResolvedAbility) -> bool {
    let ResolvedAbility {
        effect,
        targets,
        source_id: _,
        source_incarnation,
        trigger_source,
        trigger_definition_ref,
        force_block_attacker: _,
        target_incarnations: _, // CR 400.7 referent pins; batch candidacy is shape-only
        selected_target_incarnations: _, // CR 400.7 selected-target pins; batch candidacy is shape-only
        controller: _,
        original_controller,
        scoped_player,
        kind,
        sub_ability,
        else_ability,
        duration,
        condition,
        context,
        optional_targeting,
        optional,
        optional_player,
        optional_for,
        multi_target,
        target_constraints,
        target_choice_timing,
        description,
        selected_mode_labels,
        modal_instruction_ordinal,
        detached_remainder,
        repeat_for,
        min_x_value,
        announced_x,
        cant_be_copied,
        copy_count_status,
        forward_result,
        unless_pay,
        distribution,
        distribute,
        player_scope,
        starting_with,
        chosen_x,
        cost_paid_object,
        noted_mana_payment,
        cost_paid_object_ids,
        effect_context_object,
        amassed_army_object,
        ability_index,
        may_trigger_origin,
        target_selection_mode,
        target_chooser,
        chosen_players,
        repeat_until,
        replacement_applied: _,
        sub_link,
        sibling_condition,
        modal,
        mode_abilities,
        parent_target_missing_reason,
    } = ability;

    let self_counter = matches!(
        effect,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SelfRef,
        }
    );

    self_counter
        && targets.is_empty()
        && source_incarnation.is_none()
        && trigger_source.is_none()
        && trigger_definition_ref.is_none()
        && original_controller.is_none()
        && scoped_player.is_none()
        && matches!(kind, AbilityKind::Spell | AbilityKind::Database)
        && sub_ability.is_none()
        && else_ability.is_none()
        && duration.is_none()
        && condition.is_none()
        && *context == SpellContext::default()
        && !*optional_targeting
        && !*optional
        && optional_player.is_none()
        && optional_for.is_none()
        && multi_target.is_none()
        && target_constraints.is_empty()
        && *target_choice_timing == TargetChoiceTiming::Stack
        && description.is_none()
        && selected_mode_labels.is_empty()
        // CR 700.2 + CR 700.2d: a mode root is the head of ONE selected
        // instruction of a modal ability, and its ordinal gates the
        // per-mode reset of the chain-local tracked-set identity in
        // `resolve_ability_chain`. A batch collapses N stack entries into a
        // SINGLE chain entry, so it would fire that per-instruction boundary
        // once instead of N times. That is outside what this batch proof
        // covers, so decline — declining only costs the optimization.
        && modal_instruction_ordinal.is_none()
        && matches!(detached_remainder, DetachedRemainder::NoProducer)
        && repeat_for.is_none()
        && *min_x_value == 0
        // CR 601.2b: an announce-locked X makes this ability's X board-dependent;
        // it is not the vanilla self-counter shape this batch path proves safe.
        && announced_x.is_none()
        && !*cant_be_copied
        && *copy_count_status == CopyCountStatus::Pending
        && !*forward_result
        && unless_pay.is_none()
        && distribution.is_none()
        && distribute.is_none()
        && player_scope.is_none()
        && starting_with.is_none()
        && chosen_x.is_none()
        && cost_paid_object.is_none()
        // Issue #6504: a batched ability must not carry a per-activation
        // noted-mana-payment snapshot either — two sibling copies of a
        // "note the type of mana spent..." ability can carry DIFFERENT
        // payments (that's the whole point of threading it per-activation
        // rather than through a shared mutable latch), so they are never
        // safe to merge into one batched resolution.
        && noted_mana_payment.is_none()
        // CR 117.1 (issue #4948): a batched triggered ability must not carry
        // per-instance cost-paid-object state either — mirrors the
        // `cost_paid_object` gate above. Always empty for triggered
        // abilities today (only cost-payment handlers populate it), kept
        // here so this exhaustive-field check stays correct if that ever
        // changes.
        && cost_paid_object_ids.is_empty()
        && effect_context_object.is_none()
        && amassed_army_object.is_none()
        && ability_index.is_none()
        && may_trigger_origin.is_none()
        && *target_selection_mode == TargetSelectionMode::Chosen
        && target_chooser.is_none()
        && chosen_players.is_empty()
        && repeat_until.is_none()
        && *sub_link == SubAbilityLink::ContinuationStep
        // CR 702.1c ("the same is true") + CR 608.2c (written order): a
        // `ReplicatedOrBranch` per-item keyword-list sibling (Mutable Pupa,
        // Kathril) is not the vanilla batchable shape this proof
        // covers — its independent OR-branch gate must be evaluated per entry.
        && *sibling_condition == SiblingCondition::Dependent
        && modal.is_none()
        && mode_abilities.is_empty()
        && parent_target_missing_reason.is_none()
}

/// CR 117.3b + CR 117.3d + CR 117.5 + CR 608.2 + CR 704.3 + CR 119.3: Fixed
/// controller GainLife class — shared inert proof; life-gain observer refusal
/// is covered by the common event/settled checkpoint checks (CR 119.9).
fn resolve_proven_fixed_controller_gain_life_batch(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    run_len: u32,
) -> Option<u32> {
    resolve_proven_inert_trigger_batch(state, events, run_len, None)
}

struct FixedControllerGainLifeRunKey<'a> {
    controller: PlayerId,
    ability: &'a ResolvedAbility,
    paid: Option<&'a StackPaidSnapshot>,
}

/// CR 603.3b + CR 608.2: Length of the top contiguous run of identical
/// triggered abilities that grant a fixed amount of life to their controller.
/// Keyed `SourceIndependent` with inert provenance ignored so distinct ETB
/// life-gain sources (issue #5946) can share one run when interleaved.
/// Compares each adjacent entry against a single top key without cloning
/// abilities on the trigger-storm path.
fn fixed_controller_gain_life_run_len(state: &GameState) -> Option<u32> {
    let top = state.stack.back()?;
    let top_key = fixed_controller_gain_life_run_key(state, top)?;
    let mut len = 1u32;
    for entry in state.stack.iter().rev().skip(1) {
        match fixed_controller_gain_life_run_key(state, entry) {
            Some(key)
                if key.controller == top_key.controller
                    && key.paid == top_key.paid
                    && inert_trigger_abilities_eq_ignoring_provenance(
                        key.ability,
                        top_key.ability,
                    ) =>
            {
                len += 1
            }
            _ => break,
        }
    }
    Some(len)
}

fn fixed_controller_gain_life_run_key<'a>(
    state: &'a GameState,
    entry: &'a StackEntry,
) -> Option<FixedControllerGainLifeRunKey<'a>> {
    let StackEntryKind::TriggeredAbility {
        source_id: _,
        ability,
        condition,
        trigger_event: _,
        description: _,
        source_name: _,
        subject_match_count: _,
        die_result: _,
        provenance: None,
    } = &entry.kind
    else {
        return None;
    };

    if condition.is_some()
        || !flatten_targets_in_chain(ability).is_empty()
        || !fixed_controller_gain_life_ability_is_batch_candidate(ability)
    {
        return None;
    }

    Some(FixedControllerGainLifeRunKey {
        controller: entry.controller,
        ability,
        paid: state.stack_paid_facts.get(&entry.id),
    })
}

fn fixed_controller_gain_life_ability_is_batch_candidate(ability: &ResolvedAbility) -> bool {
    let ResolvedAbility {
        effect,
        targets,
        source_id: _,
        source_incarnation: _,
        trigger_source: _,
        trigger_definition_ref: _,
        force_block_attacker: _,
        target_incarnations: _, // CR 400.7 referent pins; batch candidacy is shape-only
        selected_target_incarnations: _, // CR 400.7 selected-target pins; batch candidacy is shape-only
        controller: _,
        original_controller: _,
        scoped_player,
        kind,
        sub_ability,
        else_ability,
        duration,
        condition,
        context,
        optional_targeting,
        optional,
        optional_player,
        optional_for,
        multi_target,
        target_constraints,
        target_choice_timing,
        description: _,
        selected_mode_labels,
        modal_instruction_ordinal,
        detached_remainder,
        repeat_for,
        min_x_value,
        announced_x,
        cant_be_copied,
        copy_count_status,
        forward_result,
        unless_pay,
        distribution,
        distribute,
        player_scope,
        starting_with,
        chosen_x,
        cost_paid_object,
        noted_mana_payment,
        cost_paid_object_ids,
        effect_context_object,
        amassed_army_object,
        ability_index: _,
        may_trigger_origin: _,
        target_selection_mode,
        target_chooser,
        chosen_players,
        repeat_until,
        replacement_applied: _,
        sub_link,
        sibling_condition,
        modal,
        mode_abilities,
        parent_target_missing_reason,
    } = ability;

    let fixed_controller_gain_life = matches!(
        effect,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { .. },
            player: TargetFilter::Controller,
        }
    );

    fixed_controller_gain_life
        && targets.is_empty()
        && scoped_player.is_none()
        && matches!(kind, AbilityKind::Spell | AbilityKind::Database)
        && sub_ability.is_none()
        && else_ability.is_none()
        && duration.is_none()
        && condition.is_none()
        && *context == SpellContext::default()
        && !*optional_targeting
        && !*optional
        && optional_player.is_none()
        && optional_for.is_none()
        && multi_target.is_none()
        && target_constraints.is_empty()
        && *target_choice_timing == TargetChoiceTiming::Stack
        && selected_mode_labels.is_empty()
        // CR 700.2 + CR 700.2d: a mode root is the head of ONE selected
        // instruction of a modal ability, and its ordinal gates the
        // per-mode reset of the chain-local tracked-set identity in
        // `resolve_ability_chain`. A batch collapses N stack entries into a
        // SINGLE chain entry, so it would fire that per-instruction boundary
        // once instead of N times. That is outside what this batch proof
        // covers, so decline — declining only costs the optimization.
        && modal_instruction_ordinal.is_none()
        && matches!(detached_remainder, DetachedRemainder::NoProducer)
        && repeat_for.is_none()
        && *min_x_value == 0
        && announced_x.is_none()
        && !*cant_be_copied
        && *copy_count_status == CopyCountStatus::Pending
        && !*forward_result
        && unless_pay.is_none()
        && distribution.is_none()
        && distribute.is_none()
        && player_scope.is_none()
        && starting_with.is_none()
        && chosen_x.is_none()
        && cost_paid_object.is_none()
        && noted_mana_payment.is_none()
        && cost_paid_object_ids.is_empty()
        && effect_context_object.is_none()
        && amassed_army_object.is_none()
        && *target_selection_mode == TargetSelectionMode::Chosen
        && target_chooser.is_none()
        && chosen_players.is_empty()
        && repeat_until.is_none()
        && *sub_link == SubAbilityLink::ContinuationStep
        // CR 702.1c ("the same is true") + CR 608.2c (written order): a
        // `ReplicatedOrBranch` per-item keyword-list sibling (Mutable Pupa,
        // Kathril) is not the vanilla batchable shape this proof
        // covers — its independent OR-branch gate must be evaluated per entry.
        && *sibling_condition == SiblingCondition::Dependent
        && modal.is_none()
        && mode_abilities.is_empty()
        && parent_target_missing_reason.is_none()
}

/// CR 117.3b + CR 117.3d + CR 117.5 + CR 608.2 + CR 704.3: Fixed opponent-
/// scoped effect class — shared inert proof. Zone-change and life-change
/// observers are covered by the common event/settled checkpoint checks.
fn resolve_proven_fixed_opponent_effect_batch(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    run_len: u32,
) -> Option<u32> {
    resolve_proven_inert_trigger_batch(state, events, run_len, None)
}

struct FixedOpponentEffectRunKey<'a> {
    controller: PlayerId,
    ability: &'a ResolvedAbility,
    condition: Option<&'a TriggerCondition>,
    paid: Option<&'a StackPaidSnapshot>,
}

/// CR 603.3b + CR 603.4 + CR 608.2: Length of the top contiguous run of
/// identical triggered abilities that apply a fixed life-loss or mill effect
/// to each opponent. Equal intervening-if conditions are admitted because the
/// shared clone proof rechecks every entry at resolution time before committing.
/// Source provenance is inert for this effect shape, so distinct sources can
/// share one run when all resolution-relevant fields agree.
fn fixed_opponent_effect_run_len(state: &GameState) -> Option<u32> {
    let top = state.stack.back()?;
    let top_key = fixed_opponent_effect_run_key(state, top)?;
    let mut len = 1u32;
    for entry in state.stack.iter().rev().skip(1) {
        match fixed_opponent_effect_run_key(state, entry) {
            Some(key)
                if key.controller == top_key.controller
                    && key.condition == top_key.condition
                    && key.paid == top_key.paid
                    && inert_trigger_abilities_eq_ignoring_provenance(
                        key.ability,
                        top_key.ability,
                    ) =>
            {
                len += 1
            }
            _ => break,
        }
    }
    Some(len)
}

fn fixed_opponent_effect_run_key<'a>(
    state: &'a GameState,
    entry: &'a StackEntry,
) -> Option<FixedOpponentEffectRunKey<'a>> {
    let StackEntryKind::TriggeredAbility {
        source_id: _,
        ability,
        condition,
        trigger_event: _,
        description: _,
        source_name: _,
        subject_match_count: _,
        die_result: _,
        provenance: None,
    } = &entry.kind
    else {
        return None;
    };

    if !flatten_targets_in_chain(ability).is_empty()
        || !fixed_opponent_effect_ability_is_batch_candidate(ability)
    {
        return None;
    }

    Some(FixedOpponentEffectRunKey {
        controller: entry.controller,
        ability,
        condition: condition.as_ref(),
        paid: state.stack_paid_facts.get(&entry.id),
    })
}

fn fixed_opponent_effect_ability_is_batch_candidate(ability: &ResolvedAbility) -> bool {
    let ResolvedAbility {
        effect,
        targets,
        source_id: _,
        source_incarnation: _,
        trigger_source: _,
        trigger_definition_ref: _,
        force_block_attacker: _,
        target_incarnations: _, // CR 400.7 referent pins; batch candidacy is shape-only
        selected_target_incarnations: _, // CR 400.7 selected-target pins; batch candidacy is shape-only
        controller: _,
        original_controller: _,
        scoped_player,
        kind,
        sub_ability,
        else_ability,
        duration,
        condition,
        context,
        optional_targeting,
        optional,
        optional_player,
        optional_for,
        multi_target,
        target_constraints,
        target_choice_timing,
        description: _,
        selected_mode_labels,
        modal_instruction_ordinal,
        detached_remainder,
        repeat_for,
        min_x_value,
        announced_x,
        cant_be_copied,
        copy_count_status,
        forward_result,
        unless_pay,
        distribution,
        distribute,
        player_scope,
        starting_with,
        chosen_x,
        cost_paid_object,
        noted_mana_payment,
        cost_paid_object_ids,
        effect_context_object,
        amassed_army_object,
        ability_index: _,
        may_trigger_origin: _,
        target_selection_mode,
        target_chooser,
        chosen_players,
        repeat_until,
        replacement_applied: _,
        sub_link,
        sibling_condition,
        modal,
        mode_abilities,
        parent_target_missing_reason,
    } = ability;

    let fixed_opponent_effect = matches!(
        effect,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { .. },
            target: None,
        } | Effect::Mill {
            count: QuantityExpr::Fixed { .. },
            target: TargetFilter::Controller,
            destination: Zone::Graveyard,
        }
    );

    fixed_opponent_effect
        && targets.is_empty()
        && scoped_player.is_none()
        && matches!(kind, AbilityKind::Spell | AbilityKind::Database)
        && sub_ability.is_none()
        && else_ability.is_none()
        && duration.is_none()
        && condition.is_none()
        && *context == SpellContext::default()
        && !*optional_targeting
        && !*optional
        && optional_player.is_none()
        && optional_for.is_none()
        && multi_target.is_none()
        && target_constraints.is_empty()
        && *target_choice_timing == TargetChoiceTiming::Stack
        && selected_mode_labels.is_empty()
        // CR 700.2 + CR 700.2d: a mode root is the head of ONE selected
        // instruction of a modal ability, and its ordinal gates the
        // per-mode reset of the chain-local tracked-set identity in
        // `resolve_ability_chain`. A batch collapses N stack entries into a
        // SINGLE chain entry, so it would fire that per-instruction boundary
        // once instead of N times. That is outside what this batch proof
        // covers, so decline — declining only costs the optimization.
        && modal_instruction_ordinal.is_none()
        && matches!(detached_remainder, DetachedRemainder::NoProducer)
        && repeat_for.is_none()
        && *min_x_value == 0
        && announced_x.is_none()
        && !*cant_be_copied
        && *copy_count_status == CopyCountStatus::Pending
        && !*forward_result
        && unless_pay.is_none()
        && distribution.is_none()
        && distribute.is_none()
        && *player_scope == Some(PlayerFilter::Opponent)
        && starting_with.is_none()
        && chosen_x.is_none()
        && cost_paid_object.is_none()
        && noted_mana_payment.is_none()
        && cost_paid_object_ids.is_empty()
        && effect_context_object.is_none()
        && amassed_army_object.is_none()
        && *target_selection_mode == TargetSelectionMode::Chosen
        && target_chooser.is_none()
        && chosen_players.is_empty()
        && repeat_until.is_none()
        && *sub_link == SubAbilityLink::ContinuationStep
        // CR 702.1c ("the same is true") + CR 608.2c (written order): a
        // `ReplicatedOrBranch` per-item keyword-list sibling (Mutable Pupa,
        // Kathril) is not the vanilla batchable shape this proof
        // covers — its independent OR-branch gate must be evaluated per entry.
        && *sibling_condition == SiblingCondition::Dependent
        && modal.is_none()
        && mode_abilities.is_empty()
        && parent_target_missing_reason.is_none()
}

/// CR 608.2: Apply a proven-safe batch. The per-resolution handler body runs
/// `consumed` times (§5.2a — no count-fusion in v1), with the pipeline
/// checkpoint hoisted to once-after by the caller. Per-entry `StackResolved`
/// events are emitted for every consumed entry (§5.4) so the frontend's
/// per-entry fade still works. Returns the number of entries consumed.
///
/// `consumed` equals the full run length for the base-token path, but the
/// copy-prefix path (CR 707.2) may consume a value-equal PREFIX shorter than
/// the run — the divergent tail resolves in a subsequent `resolve_next` step.
///
/// CR 603.4: This path does NOT bump `ability_resolutions_this_turn`. A
/// resolution-count-dependent intervening-if lives as an entry-level condition,
/// and `batch_run_key` refuses any entry with `condition.is_some()`, so no
/// batched run can carry a `NthResolutionThisTurn`-gated condition that the
/// missing counter bump would desynchronize.
fn resolve_batched(
    state: &mut GameState,
    plan: &effects::BatchPlan,
    ability: &crate::types::ability::ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> u32 {
    let consumed = plan.consumed();
    crate::game::perf_counters::record_stack_batched_entries(consumed);
    debug_assert!(state.resolving_stack_entry.is_none());
    debug_assert!(state.resolving_trigger_firing.is_none());
    // CR 400.7j: clear the resolution-scoped self-move re-latch with the entry.
    state.resolution_source_relatch = None;

    // Pop the run's entries (resolution order is back-to-front) through the same
    // authority `resolve_top` uses for a single entry, so the per-entry side
    // tables settle with each removal.
    let mut popped = Vec::with_capacity(consumed as usize);
    for _ in 0..consumed {
        let Some(removed) = pop_top_stack_entry(state) else {
            break;
        };
        popped.push(removed);
    }

    // CR 603.7c: Set the trigger event context once from the (identical) top
    // entry — all popped entries are deep-equal by `BatchRunKey`, so a single
    // set/clear is equivalent to N idempotent sequential set/clear cycles.
    if let Some(top) = popped.first() {
        if let crate::types::game_state::StackEntryKind::TriggeredAbility {
            trigger_event: Some(te),
            subject_match_count,
            die_result,
            ..
        } = &top.entry.kind
        {
            state.current_trigger_event = Some(te.clone());
            state.current_trigger_events = vec![te.clone()];
            state.current_trigger_match_count = *subject_match_count;
            // CR 706.2 + CR 706.4 + CR 603.12: re-stamp the carried die-roll
            // result into resolution scope for a reflexive "When you do … the
            // result" sub-ability (see `resolve_top`).
            state.die_result_this_resolution = *die_result;
        }
    }

    if let Some(popped) = popped.first() {
        begin_resolving_stack_entry(state, popped.entry.clone(), popped.trigger_firing);
    }

    // CR 608.2: Apply the effect N times through the existing per-resolution body.
    plan.execute(state, ability, events);

    // CR 603.7c: Clear trigger context after resolution completes.
    state.current_trigger_event = None;
    state.current_trigger_events.clear();
    state.current_trigger_match_count = None;
    // CR 706.2 + CR 706.4: clear the carried die-roll result at the same
    // cross-resolution boundary as the batched subject count.
    state.die_result_this_resolution = None;

    let popped_count = popped.len() as u32;

    // §5.4: one StackResolved and terminal settlement per consumed entry.
    for (index, popped) in popped.into_iter().enumerate() {
        events.push(GameEvent::StackResolved {
            object_id: popped.entry.id,
        });
        if index == 0 {
            finish_resolving_stack_entry(
                state,
                super::lifecycle::DelayedTerminalDisposition::Resolved,
            );
        } else if let Some(firing) = popped.trigger_firing {
            super::lifecycle::record_delayed_terminal(
                firing,
                super::lifecycle::DelayedTerminalDisposition::Resolved,
            );
        }
    }
    state.resolution_source_relatch = None;

    popped_count
}

/// CR 603.2 + CR 603.3 + CR 603.6a: Layer C — battlefield-wide
/// observer-order-invariance gate. A batched run is order-invariant iff NO
/// battlefield trigger fans out on the token-ETB events the batch will emit.
/// Build the REAL `ZoneChanged` + `TokenCreated` events one produced token
/// emits (from the resolved spec's true characteristics) and route each through
/// the public `candidates_for_event` — the same `keys_from_event` path the real
/// events take downstream, with NO hand-picked key set. If ANY observer is
/// registered for those events — including one on the run's own source (HIGH-2:
/// a source carrying a second observer trigger keyed on the produced token's
/// ETB/TokenCreated must NOT be excluded; doing so would skip the per-trigger
/// priority interleaving CR 603.3 requires) — sequential resolution interleaves
/// it per-token (CR 603.3 topmost-on-stack), so the batch ("all tokens, then
/// all observers") may diverge. Refuse, fall back per-entry. The §2.2a
/// emits-exactly gate makes this two-event probe complete by construction for
/// ALL observer axes.
fn observers_are_batch_safe(state: &mut GameState, plan: &effects::BatchPlan) -> bool {
    for (spec, mana_value) in plan
        .produced_token_specs()
        .into_iter()
        .zip(plan.produced_token_mana_values())
    {
        let record = zone_change_record_from_spec(spec, mana_value);
        let zc = GameEvent::ZoneChanged {
            object_id: PROBE_ID,
            from: None,
            to: Zone::Battlefield,
            record: Box::new(record),
        };
        let tc = GameEvent::TokenCreated {
            object_id: PROBE_ID,
            name: spec.characteristics.display_name.clone(),
            // Synthetic batch-safety probe; the creating source is irrelevant to the
            // observer-shape check, so reuse the probe sentinel id.
            source_id: PROBE_ID,
        };
        for ev in [&zc, &tc] {
            // unclassified ∪ buckets matching keys_from_event(ev). The
            // unclassified bucket (Always/Immediate/dynamic/synthetic-keyword)
            // is unconditionally included → any catch-all observer forces refuse.
            // CR 603.3: any registered observer (including the run's own source)
            // forces sequential resolution so priority interleaves per-token.
            let candidates = crate::game::trigger_index::candidates_for_event(state, ev);
            if !candidates.is_empty() && !observer_candidates_are_inert(state, ev, &candidates) {
                return false;
            }
        }
    }
    true
}

fn observer_candidates_are_inert(
    state: &mut GameState,
    event: &GameEvent,
    candidates: &[ObjectId],
) -> bool {
    let event_keys = crate::game::trigger_index::keys_from_event(event, state);
    for candidate in candidates {
        let Some(source_obj) = state.objects.get(candidate) else {
            continue;
        };
        let source_context = super::triggers::trigger_source_context_for_latch(state, source_obj);
        let controller = source_context.lki.controller;
        let source = source_context.identity.reference;
        let triggers = source_context
            .trigger_entries
            .iter()
            .cloned()
            .enumerate()
            .collect::<Vec<_>>();

        for (trigger_index, entry) in triggers {
            let definition_ref = crate::types::ability::TriggerDefinitionRef {
                source,
                occurrence: entry.occurrence.clone(),
            };
            let trigger = entry.definition;
            let (trigger_keys, unclassified) =
                crate::game::trigger_index::keys_from_trigger_def(&trigger);
            if !unclassified && !trigger_keys.iter().any(|key| event_keys.contains(key)) {
                continue;
            }
            if trigger.condition.as_ref().is_some_and(|condition| {
                !super::triggers::check_trigger_condition_with_source(
                    state,
                    condition,
                    controller,
                    Some(&source_context),
                    Some(event),
                )
            }) {
                continue;
            }

            let mut ability = super::triggers::build_triggered_ability_from_context(
                state,
                &trigger,
                &source_context,
                Some(&definition_ref),
            );
            ability.ability_index = Some(trigger_index);
            ability.may_trigger_origin = Some(MayTriggerOrigin::Definition { definition_ref });
            if !optional_ability_is_inert_under_auto_choice(state, &ability, Some(event)) {
                return false;
            }
        }
    }
    true
}

fn optional_ability_is_inert_under_auto_choice(
    state: &mut GameState,
    ability: &ResolvedAbility,
    trigger_event: Option<&GameEvent>,
) -> bool {
    if !ability.optional {
        return false;
    }
    let Some(origin) = ability.may_trigger_origin.clone() else {
        return false;
    };
    let key = MayTriggerAutoChoiceKey {
        player: ability.controller,
        source_id: ability.source_id,
        origin,
    };
    match state.may_trigger_auto_choice(&key) {
        Some(AutoMayChoice::Decline) => ability.sub_ability.is_none(),
        Some(AutoMayChoice::Accept) => {
            ability_has_no_legal_resolution_targets(state, ability, trigger_event)
        }
        None => false,
    }
}

fn ability_has_no_legal_resolution_targets(
    state: &mut GameState,
    ability: &ResolvedAbility,
    trigger_event: Option<&GameEvent>,
) -> bool {
    if ability.sub_ability.is_some() {
        return false;
    }

    let trigger_events = trigger_event.iter().cloned().cloned().collect::<Vec<_>>();
    let context_snapshot =
        super::triggers::push_trigger_event_context(state, trigger_event, &trigger_events, None);
    let empty = build_target_slots(state, ability).is_ok_and(|slots| {
        // CR 115.1: only effects that DECLARE a target surface a chooseable slot.
        // `extract_target_filter_from_effect` is the single authority the slot
        // builder itself uses — it returns `None` not only for context-refs
        // ("you may draw a card") but for every resolution-time selection
        // (Sacrifice, at-resolution Bounce, put-from-hand ChangeZone/CastFromZone,
        // etc.), all of which are ALWAYS resolvable. Testing raw `target_filter()`
        // here would fork from that authority and silently drop an auto-accepted
        // "you may put a creature from your hand …" trigger (Kaalia et al.).
        (super::triggers::extract_target_filter_from_effect(&ability.effect).is_some()
            && slots.is_empty())
            || (!slots.is_empty() && slots.iter().all(|slot| slot.legal_targets.is_empty()))
    });
    super::triggers::restore_trigger_event_context(state, context_snapshot);
    empty
}

fn inert_noop_run_len(state: &mut GameState) -> Option<u32> {
    let top = state.stack.back()?.clone();
    if !stack_entry_is_inert_noop(state, &top) {
        return None;
    }
    let mut count = 0u32;
    let entries = state.stack.iter().rev().cloned().collect::<Vec<_>>();
    for entry in &entries {
        if count == 0 {
            count += 1;
            continue;
        }
        if !same_inert_noop_run_member(&top, entry) {
            break;
        }
        count += 1;
    }
    Some(count)
}

fn stack_entry_is_inert_noop(state: &mut GameState, entry: &StackEntry) -> bool {
    let StackEntryKind::TriggeredAbility {
        ability,
        condition,
        trigger_event,
        ..
    } = &entry.kind
    else {
        return false;
    };

    if condition.is_some() {
        return false;
    }

    optional_ability_is_inert_under_auto_choice(state, ability, trigger_event.as_ref())
}

fn same_inert_noop_run_member(top: &StackEntry, entry: &StackEntry) -> bool {
    let StackEntryKind::TriggeredAbility {
        ability: top_ability,
        condition: top_condition,
        trigger_event: top_event,
        ..
    } = &top.kind
    else {
        return false;
    };
    let StackEntryKind::TriggeredAbility {
        ability,
        condition,
        trigger_event,
        ..
    } = &entry.kind
    else {
        return false;
    };

    top.source_id == entry.source_id
        && top.controller == entry.controller
        && top_ability == ability
        && top_condition == condition
        && trigger_events_are_equivalent_for_inert_target(top_ability, top_event, trigger_event)
}

fn trigger_events_are_equivalent_for_inert_target(
    ability: &ResolvedAbility,
    a: &Option<GameEvent>,
    b: &Option<GameEvent>,
) -> bool {
    if a == b {
        return true;
    }
    if !change_zone_target_depends_only_on_cost_paid_mana_value(ability) {
        return false;
    }
    zone_changed_mana_context(a.as_ref()) == zone_changed_mana_context(b.as_ref())
}

fn change_zone_target_depends_only_on_cost_paid_mana_value(ability: &ResolvedAbility) -> bool {
    let Effect::ChangeZone { target, .. } = &ability.effect else {
        return false;
    };
    let TargetFilter::Typed(typed) = target else {
        return false;
    };
    typed.properties.iter().all(|prop| {
        matches!(
            prop,
            FilterProp::InZone { .. }
                | FilterProp::Cmc {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectManaValue {
                            scope: ObjectScope::CostPaidObject,
                        },
                    },
                    ..
                }
        )
    })
}

fn zone_changed_mana_context(event: Option<&GameEvent>) -> Option<(u32, PlayerId)> {
    match event {
        Some(GameEvent::ZoneChanged { record, .. }) => Some((record.mana_value, record.controller)),
        _ => None,
    }
}

fn resolve_inert_noop_batch(
    state: &mut GameState,
    consumed: u32,
    events: &mut Vec<GameEvent>,
) -> u32 {
    debug_assert!(state.resolving_stack_entry.is_none());
    debug_assert!(state.resolving_trigger_firing.is_none());
    // CR 400.7j: clear the resolution-scoped self-move re-latch with the entry.
    state.resolution_source_relatch = None;
    for _ in 0..consumed {
        let Some(removed) = pop_top_stack_entry(state) else {
            break;
        };
        events.push(GameEvent::StackResolved {
            object_id: removed.entry.id,
        });
        if let Some(firing) = removed.trigger_firing {
            super::lifecycle::record_delayed_terminal(
                firing,
                super::lifecycle::DelayedTerminalDisposition::Resolved,
            );
        }
    }
    consumed
}

/// CR 603.6a + CR 603.10: Build the faithful `ZoneChangeRecord` a produced
/// token emits, from the resolved `TokenSpec` characteristics. `keys_from_event`
/// reads only `core_types`/`to` for ETB keys, so the record's `core_types`
/// drives the entire probe key set (mirrors `snapshot_for_zone_change`).
fn zone_change_record_from_spec(
    spec: &crate::types::proposed_event::TokenSpec,
    mana_value: u32,
) -> crate::types::game_state::ZoneChangeRecord {
    let ch = &spec.characteristics;
    crate::types::game_state::ZoneChangeRecord {
        object_id: PROBE_ID,
        name: ch.display_name.clone(),
        core_types: ch.core_types.clone(),
        subtypes: ch.subtypes.clone(),
        supertypes: ch.supertypes.clone(),
        keywords: ch.keywords.clone(),
        trigger_definitions: Vec::new(),
        trigger_source_context: None,
        power: ch.power,
        toughness: ch.toughness,
        base_power: ch.power,
        base_toughness: ch.toughness,
        colors: ch.colors.clone(),
        mana_value,
        controller: spec.controller,
        owner: spec.controller,
        from_zone: None,
        cast_from_zone: None,
        played_from_zone: None,
        to_zone: Zone::Battlefield,
        attachments: Vec::new(),
        linked_exile_snapshot: Vec::new(),
        is_token: true,
        combat_status: Default::default(),
        co_departed: Vec::new(),
        attached_to: None,
        entered_incarnation: None,
        turn_zone_change_index: 0,
        recorded_turn_number: 0,
        // A freshly created token is never suspected (CR 701.60b).
        is_suspected: false,
    }
}

/// CR 111.2 + CR 109.4: The run-identity axis along the source dimension. A
/// base token's characteristics and controller are fixed at creation and do not
/// read the creating source, so triggers from DISTINCT sources are
/// resolution-identical and collapse under `SourceIndependent`. Any
/// source-relative effect (a copy that reads its own `SelfRef` source, an
/// attacking/attached token, a source-relative count) keeps a per-source
/// boundary via `Source(id)` so two sources never collapse incorrectly.
#[derive(PartialEq)]
enum BatchSourceAxis {
    SourceIndependent,
    Source(ObjectId),
}

/// Resolution-grade run key (stricter than the display `StackGroupKey`, §4.1).
/// Two adjacent entries join a run iff every field is equal AND the entry is an
/// untargeted `TriggeredAbility` (Layer A). Keyed on `source_axis` + deep-equal
/// `ResolvedAbility` (not display `source_name`), with the flattened target
/// vector required empty (CR 608.2b).
struct BatchRunKey<'a> {
    controller: PlayerId,
    source_axis: BatchSourceAxis,
    ability: &'a ResolvedAbility,
    description: Option<&'a str>,
    paid: Option<&'a StackPaidSnapshot>,
    trigger_event: Option<&'a GameEvent>,
    trigger_firing: Option<TriggerFiring>,
}

/// CR 111.2 + CR 109.4: `ResolvedAbility` embeds `source_id` (and nested sub/
/// else abilities embed their own), so a derived `PartialEq` would treat two
/// otherwise-identical abilities from distinct sources as unequal — defeating
/// the `SourceIndependent` collapse. When both keys are `SourceIndependent` the
/// effect provably reads nothing from the source, so abilities are compared
/// with `source_id` canonicalized away (recursively, on the chain). When either
/// key is `Source(id)`, the per-source boundary already differs, so the regular
/// deep equality (including `source_id`) applies.
impl PartialEq for BatchRunKey<'_> {
    fn eq(&self, other: &Self) -> bool {
        if self.controller != other.controller
            || self.source_axis != other.source_axis
            || self.description != other.description
            || self.paid != other.paid
            || self.trigger_event != other.trigger_event
            || self.trigger_firing != other.trigger_firing
        {
            return false;
        }
        match (&self.source_axis, &other.source_axis) {
            (BatchSourceAxis::SourceIndependent, BatchSourceAxis::SourceIndependent) => {
                abilities_equal_ignoring_source(self.ability, other.ability)
            }
            _ => self.ability == other.ability,
        }
    }
}

/// Compare two resolved abilities for batch-run identity while ignoring the
/// source-object id at every level of the sub/else chain. Cheap clone+normalize
/// only runs on the batch-eligible path. The classifier guarantees the effect
/// reads nothing else from the source, so source-id is the only field allowed
/// to differ across a `SourceIndependent` run.
fn abilities_equal_ignoring_source(a: &ResolvedAbility, b: &ResolvedAbility) -> bool {
    normalize_ability_source(a) == normalize_ability_source(b)
}

/// Clone an ability with `source_id` (and nested sub/else `source_id`s)
/// canonicalized to `ObjectId(0)`, so equality ignores the creating source.
fn normalize_ability_source(ability: &ResolvedAbility) -> ResolvedAbility {
    let mut out = ability.clone();
    out.source_id = ObjectId(0);
    out.sub_ability = out
        .sub_ability
        .map(|sub| Box::new(normalize_ability_source(&sub)));
    out.else_ability = out
        .else_ability
        .map(|alt| Box::new(normalize_ability_source(&alt)));
    out
}

/// Non-allocating structural equality for `SourceIndependent` inert-trigger run
/// identity (issue #5946). Ignores provenance stamps and `source_id` that vary
/// across distinct ETB sources; recursively compares sub/else chains the same
/// way. Exhaustive field disposition so new `ResolvedAbility` fields cannot
/// silently drop out of run identity.
fn inert_trigger_abilities_eq_ignoring_provenance(
    a: &ResolvedAbility,
    b: &ResolvedAbility,
) -> bool {
    let ResolvedAbility {
        effect: a_effect,
        targets: a_targets,
        source_id: _,
        source_incarnation: _,
        trigger_source: _,
        trigger_definition_ref: _,
        force_block_attacker: a_force_block_attacker,
        target_incarnations: a_target_incarnations,
        controller: a_controller,
        original_controller: _,
        scoped_player: a_scoped_player,
        kind: a_kind,
        sub_ability: a_sub_ability,
        else_ability: a_else_ability,
        duration: a_duration,
        condition: a_condition,
        context: a_context,
        optional_targeting: a_optional_targeting,
        optional: a_optional,
        optional_player: a_optional_player,
        optional_for: a_optional_for,
        multi_target: a_multi_target,
        target_constraints: a_target_constraints,
        target_choice_timing: a_target_choice_timing,
        description: _,
        selected_mode_labels: a_selected_mode_labels,
        // CR 700.2: deliberately NOT part of run identity. At the ROOT it is
        // provably `None` — this function is entered ONLY through the three
        // `*_ability_is_batch_candidate` gates, each of which now requires
        // `modal_instruction_ordinal.is_none()`. That guarantee is ONE HOP
        // deep: the `sub_ability`/`else_ability` recursions below re-enter
        // this function directly, without re-checking a gate, so a deeper node
        // could in principle carry an ordinal. Ignoring it is still right —
        // this equality is issue #5946's `SourceIndependent` inert-trigger RUN
        // IDENTITY, not a modal check, and two runs that differ only in which
        // mode produced them are still the same run.
        modal_instruction_ordinal: _,
        // CR 608.2c: split-remainder marker. Guaranteed `NoProducer` ONE HOP
        // upstream by the batch-candidate checks, same as the modal ordinal.
        detached_remainder: _,
        repeat_for: a_repeat_for,
        min_x_value: a_min_x_value,
        announced_x: a_announced_x,
        cant_be_copied: a_cant_be_copied,
        copy_count_status: a_copy_count_status,
        forward_result: a_forward_result,
        unless_pay: a_unless_pay,
        distribution: a_distribution,
        distribute: a_distribute,
        player_scope: a_player_scope,
        starting_with: a_starting_with,
        chosen_x: a_chosen_x,
        cost_paid_object: a_cost_paid_object,
        noted_mana_payment: a_noted_mana_payment,
        cost_paid_object_ids: a_cost_paid_object_ids,
        effect_context_object: a_effect_context_object,
        amassed_army_object: a_amassed_army_object,
        ability_index: _,
        may_trigger_origin: _,
        target_selection_mode: a_target_selection_mode,
        target_chooser: a_target_chooser,
        chosen_players: a_chosen_players,
        repeat_until: a_repeat_until,
        replacement_applied: a_replacement_applied,
        sub_link: a_sub_link,
        sibling_condition: a_sibling_condition,
        modal: a_modal,
        mode_abilities: a_mode_abilities,
        parent_target_missing_reason: a_parent_target_missing_reason,
        selected_target_incarnations: a_selected_target_incarnations,
    } = a;
    let ResolvedAbility {
        effect: b_effect,
        targets: b_targets,
        source_id: _,
        source_incarnation: _,
        trigger_source: _,
        trigger_definition_ref: _,
        force_block_attacker: b_force_block_attacker,
        target_incarnations: b_target_incarnations,
        controller: b_controller,
        original_controller: _,
        scoped_player: b_scoped_player,
        kind: b_kind,
        sub_ability: b_sub_ability,
        else_ability: b_else_ability,
        duration: b_duration,
        condition: b_condition,
        context: b_context,
        optional_targeting: b_optional_targeting,
        optional: b_optional,
        optional_player: b_optional_player,
        optional_for: b_optional_for,
        multi_target: b_multi_target,
        target_constraints: b_target_constraints,
        target_choice_timing: b_target_choice_timing,
        description: _,
        selected_mode_labels: b_selected_mode_labels,
        // CR 700.2: deliberately NOT part of run identity. At the ROOT it is
        // provably `None` — this function is entered ONLY through the three
        // `*_ability_is_batch_candidate` gates, each of which now requires
        // `modal_instruction_ordinal.is_none()`. That guarantee is ONE HOP
        // deep: the `sub_ability`/`else_ability` recursions below re-enter
        // this function directly, without re-checking a gate, so a deeper node
        // could in principle carry an ordinal. Ignoring it is still right —
        // this equality is issue #5946's `SourceIndependent` inert-trigger RUN
        // IDENTITY, not a modal check, and two runs that differ only in which
        // mode produced them are still the same run.
        modal_instruction_ordinal: _,
        // CR 608.2c: split-remainder marker. Guaranteed `NoProducer` ONE HOP
        // upstream by the batch-candidate checks, same as the modal ordinal.
        detached_remainder: _,
        repeat_for: b_repeat_for,
        min_x_value: b_min_x_value,
        announced_x: b_announced_x,
        cant_be_copied: b_cant_be_copied,
        copy_count_status: b_copy_count_status,
        forward_result: b_forward_result,
        unless_pay: b_unless_pay,
        distribution: b_distribution,
        distribute: b_distribute,
        player_scope: b_player_scope,
        starting_with: b_starting_with,
        chosen_x: b_chosen_x,
        cost_paid_object: b_cost_paid_object,
        noted_mana_payment: b_noted_mana_payment,
        cost_paid_object_ids: b_cost_paid_object_ids,
        effect_context_object: b_effect_context_object,
        amassed_army_object: b_amassed_army_object,
        ability_index: _,
        may_trigger_origin: _,
        target_selection_mode: b_target_selection_mode,
        target_chooser: b_target_chooser,
        chosen_players: b_chosen_players,
        repeat_until: b_repeat_until,
        replacement_applied: b_replacement_applied,
        sub_link: b_sub_link,
        sibling_condition: b_sibling_condition,
        modal: b_modal,
        mode_abilities: b_mode_abilities,
        parent_target_missing_reason: b_parent_target_missing_reason,
        selected_target_incarnations: b_selected_target_incarnations,
    } = b;

    a_effect == b_effect
        && a_targets == b_targets
        && a_force_block_attacker == b_force_block_attacker
        // CR 400.7 + CR 603.7c: two otherwise-identical abilities pinned to
        // DIFFERENT incarnations are not the same ability. Participating here
        // keeps this manual comparison in agreement with the type's derived
        // `PartialEq`; disagreeing with the derive would be the actual defect.
        && a_target_incarnations == b_target_incarnations
        && a_selected_target_incarnations == b_selected_target_incarnations
        && a_controller == b_controller
        && a_scoped_player == b_scoped_player
        && a_kind == b_kind
        && match (a_sub_ability, b_sub_ability) {
            (None, None) => true,
            (Some(a_sub), Some(b_sub)) => {
                inert_trigger_abilities_eq_ignoring_provenance(a_sub, b_sub)
            }
            _ => false,
        }
        && match (a_else_ability, b_else_ability) {
            (None, None) => true,
            (Some(a_else), Some(b_else)) => {
                inert_trigger_abilities_eq_ignoring_provenance(a_else, b_else)
            }
            _ => false,
        }
        && a_duration == b_duration
        && a_condition == b_condition
        && a_context == b_context
        && a_optional_targeting == b_optional_targeting
        && a_optional == b_optional
        && a_optional_player == b_optional_player
        && a_optional_for == b_optional_for
        && a_multi_target == b_multi_target
        && a_target_constraints == b_target_constraints
        && a_target_choice_timing == b_target_choice_timing
        && a_selected_mode_labels == b_selected_mode_labels
        && a_repeat_for == b_repeat_for
        && a_min_x_value == b_min_x_value
        && a_announced_x == b_announced_x
        && a_cant_be_copied == b_cant_be_copied
        && a_copy_count_status == b_copy_count_status
        && a_forward_result == b_forward_result
        && a_unless_pay == b_unless_pay
        && a_distribution == b_distribution
        && a_distribute == b_distribute
        && a_player_scope == b_player_scope
        && a_starting_with == b_starting_with
        && a_chosen_x == b_chosen_x
        && a_cost_paid_object == b_cost_paid_object
        && a_noted_mana_payment == b_noted_mana_payment
        && a_cost_paid_object_ids == b_cost_paid_object_ids
        && a_effect_context_object == b_effect_context_object
        && a_amassed_army_object == b_amassed_army_object
        && a_target_selection_mode == b_target_selection_mode
        && a_target_chooser == b_target_chooser
        && a_chosen_players == b_chosen_players
        && a_repeat_until == b_repeat_until
        && a_replacement_applied == b_replacement_applied
        && a_sub_link == b_sub_link
        && a_sibling_condition == b_sibling_condition
        && a_modal == b_modal
        && a_mode_abilities == b_mode_abilities
        && a_parent_target_missing_reason == b_parent_target_missing_reason
}

/// Build the run key for an entry, or `None` if the entry is not a candidate
/// for batch-resolution (Layer A.1/A.4/A.5: must be an untargeted
/// `TriggeredAbility` with no entry-level intervening-if condition).
///
/// No-wildcard discipline: every field of the `TriggeredAbility` variant is
/// destructured explicitly (no `..`) so each is consciously dispositioned —
/// the same exhaustiveness the codebase mandates for match arms, applied to
/// struct destructuring. Field-by-field audit:
/// - `source_id`   — IN KEY via `source_axis` (CR 111.2 + CR 109.4). A base
///   token reads nothing from its source, so `token_effect_is_source_independent`
///   maps it to `SourceIndependent`, collapsing a run across DISTINCT sources
///   (the Scute Swarm O(N²)→O(N) fix). Any source-relative effect maps to
///   `Source(source_id)`, keeping the per-source boundary so two sources never
///   collapse incorrectly.
/// - `ability`     — IN KEY (deep-equal `ResolvedAbility`: identical effect).
/// - `condition`   — RESOLUTION-RELEVANT, NOT in key. CR 603.4: the entry-level
///   intervening-if is rechecked per entry at resolution (`resolve_top`
///   stack.rs:120-140) and the effect is skipped once the condition flips. The
///   batch path applies the effect N times WITHOUT a per-entry recheck, so a
///   run carrying an order-sensitive intervening-if (one the run's own tokens
///   could move across its threshold) would diverge from sequential. We do NOT
///   attempt to prove invariance in v1: any `condition.is_some()` makes the
///   entry NON-batchable, forcing it into a singleton run that falls back to
///   the `resolve_top` path which rechecks correctly. Conservative refuse.
/// - `trigger_event` — IN KEY (event context drives `EventContextAmount`, etc.;
///   differing context must not collapse).
/// - `description` — IN KEY (distinguishes triggers from the same source).
/// - `source_name` — RESOLUTION-IRRELEVANT: a display-only pre-resolved name
///   (game_state.rs:3493-3500) the frontend renders; it derives from
///   `source_id` (already in key) and is never read during resolution. Not in
///   key by design.
/// - `subject_match_count` — RESOLUTION-RELEVANT but PROVABLY EQUAL across a
///   run: it is the CR 603.2c filtered subject count from the firing event
///   batch. `resolve_batched` lifts it into resolution scope from the run's top
///   entry (stack.rs:1135-1145), and `trigger_event` (which carries the firing
///   event) is already in the key — two entries with equal `trigger_event` and
///   equal deep `ability` carry the same batched subject count. It is therefore
///   redundant to key on (would never break a run the other fields kept
///   together) and is correctly applied from the top entry in the batch path.
/// - `die_result` — EXCLUDED for the same reason as `subject_match_count`: it
///   is CR 706.2 resolution data (the carried die-roll result re-stamped from
///   the run's top entry in `resolve_batched`), not run identity. Keying on it
///   would needlessly split runs without changing correctness.
fn batch_run_key<'a>(state: &'a GameState, entry: &'a StackEntry) -> Option<BatchRunKey<'a>> {
    let StackEntryKind::TriggeredAbility {
        source_id,
        ability,
        condition,
        trigger_event,
        description,
        source_name: _,
        subject_match_count: _,
        die_result: _,
        provenance: None,
    } = &entry.kind
    else {
        return None;
    };
    // CR 608.2b: untargeted-only — targets re-check legality per resolution.
    if !flatten_targets_in_chain(ability).is_empty() {
        return None;
    }
    // CR 603.4 (verified docs/MagicCompRules.txt:2588): an entry-level
    // intervening-if is rechecked per entry at resolution and skips the effect
    // once it flips. The batch path does not recheck per entry, so refuse to
    // group any entry carrying one — it becomes a singleton run and falls back
    // to the `resolve_top` path that rechecks correctly.
    if condition.is_some() {
        return None;
    }
    // CR 111.2 + CR 109.4: collapse the source dimension when the base effect
    // reads nothing from the source (a base token's controller/characteristics
    // are fixed at creation), so distinct sources join one run. Otherwise keep
    // a per-source boundary.
    let source_axis = if effects::token::token_effect_is_source_independent(ability) {
        BatchSourceAxis::SourceIndependent
    } else {
        BatchSourceAxis::Source(*source_id)
    };
    Some(BatchRunKey {
        controller: entry.controller,
        source_axis,
        ability,
        description: description.as_deref(),
        paid: state.stack_paid_facts.get(&entry.id),
        trigger_event: trigger_event.as_ref(),
        trigger_firing: state.stack_trigger_firings.get(&entry.id).copied(),
    })
}

/// CR 405.1: Length of the maximal contiguous run of batch-key-equal entries
/// starting at the TOP of the stack (resolution order is back-to-front).
/// Returns `None` when the top entry is not a batch candidate. Contiguous-only:
/// a non-adjacent look-alike across a gap must resolve in true stack order.
fn batch_run_len(state: &GameState) -> Option<u32> {
    let top = state.stack.back()?;
    let top_key = batch_run_key(state, top)?;
    let mut len = 1u32;
    // Walk downward from just below the top.
    for entry in state.stack.iter().rev().skip(1) {
        match batch_run_key(state, entry) {
            Some(key) if key == top_key => len += 1,
            _ => break,
        }
    }
    Some(len)
}

fn execute_effect(
    state: &mut GameState,
    ability: &crate::types::ability::ResolvedAbility,
    events: &mut Vec<GameEvent>,
) {
    // Skip unimplemented effects (logged elsewhere as warnings)
    if matches!(
        ability.effect,
        crate::types::ability::Effect::Unimplemented { .. }
    ) {
        return;
    }
    // Use resolve_ability_chain to support SubAbility/Execute chaining
    let _ = effects::resolve_ability_chain(state, ability, events, 0);
}

pub fn stack_is_empty(state: &GameState) -> bool {
    state.stack.is_empty()
}

// ── Display-only stack pressure + grouping ──────────────────────────────
//
// These are UX pacing/presentation primitives, not a rules concept. No CR
// citation — the Comprehensive Rules say nothing about how quickly the
// client should animate stack resolution or whether identical triggers
// should be collapsed visually. Owned by the engine so every consumer
// (browser, desktop, server) shares one authoritative threshold and one
// authoritative grouping predicate. Frontend maps StackPressure → animation
// multiplier; it never decides what "identical" means or when to skip a
// mount animation.

/// Size at which the stack transitions out of "Normal" animation pacing.
pub const STACK_PRESSURE_ELEVATED: usize = 10;
/// Size at which stack animation must be noticeably faster.
pub const STACK_PRESSURE_RAPID: usize = 30;
/// Size at which per-entry mount animation should be skipped entirely.
pub const STACK_PRESSURE_INSTANT: usize = 100;

/// Display-only pacing bucket for stack resolution animations. Not a rules
/// concept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum StackPressure {
    Normal,
    Elevated,
    Rapid,
    Instant,
}

/// Compute the current stack pressure. Just-in-time — never stored on
/// GameState per CLAUDE.md's "only compute when needed" guideline.
pub fn stack_pressure(state: &GameState) -> StackPressure {
    match state.stack.len() {
        n if n >= STACK_PRESSURE_INSTANT => StackPressure::Instant,
        n if n >= STACK_PRESSURE_RAPID => StackPressure::Rapid,
        n if n >= STACK_PRESSURE_ELEVATED => StackPressure::Elevated,
        _ => StackPressure::Normal,
    }
}

/// A coalesced group of "visually identical" stack entries. The frontend
/// renders one badge per group with `count` as a ×N suffix on the
/// representative card.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StackDisplayGroup {
    /// The first entry in the group — frontend uses its card image/name.
    pub representative: ObjectId,
    /// Number of coalesced entries (always ≥ 1).
    pub count: u32,
    /// All coalesced entry ids, in stack order. Used by UI animations that
    /// need to key per-entry (e.g., fade each out in turn on resolution).
    pub member_ids: Vec<ObjectId>,
}

/// Produce a display-grouped view of the stack. Adjacent entries with the
/// same (source card name, kind discriminant, trigger description) are
/// coalesced. Non-adjacent look-alikes stay separate — coalescing only
/// adjacent entries preserves the actual resolution order for cases like
/// stacked triggers from different sources interleaving.
pub fn stack_display_groups(state: &GameState) -> Vec<StackDisplayGroup> {
    let mut out: Vec<StackDisplayGroup> = Vec::new();
    // Track the previous entry's key alongside the output vector so we can
    // decide "merge or push" in O(1) per entry instead of re-scanning the
    // stack to look up the representative each iteration.
    let mut last_key: Option<StackGroupKey> = None;
    for entry in &state.stack {
        // KeywordAction entries (Equip/Crew/Station/Saddle) carry their
        // target inside the enum variant, not via ResolvedAbility, so the
        // target-aware signature cannot see it. Rather than reach into
        // every keyword payload just to discriminate two consecutive
        // keyword activations (a vanishingly rare scenario), we opt them
        // out of coalescing: always push a fresh group and clear
        // `last_key` so a following non-keyword entry also starts fresh.
        if matches!(entry.kind, StackEntryKind::KeywordAction { .. }) {
            out.push(StackDisplayGroup {
                representative: entry.id,
                count: 1,
                member_ids: vec![entry.id],
            });
            last_key = None;
            continue;
        }
        let key = group_key(state, entry);
        if last_key.as_ref() == Some(&key) {
            let last = out.last_mut().unwrap();
            last.count += 1;
            last.member_ids.push(entry.id);
        } else {
            out.push(StackDisplayGroup {
                representative: entry.id,
                count: 1,
                member_ids: vec![entry.id],
            });
            last_key = Some(key);
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StackGroupKey {
    source_name: String,
    tag: &'static str,
    description: Option<String>,
    selected_mode_labels: Vec<String>,
    targets: Vec<TargetRef>,
    paid: Option<StackPaidSnapshot>,
    is_pending: bool,
    provenance: Option<crate::types::game_state::SyntheticTriggerProvenance>,
}

/// Grouping signature for `stack_display_groups`. Two entries coalesce iff
/// their signatures are equal. Includes the resolved target vector so
/// visually-identical triggers that fire against different targets (e.g.
/// N copies of "target player loses 1 life" picking different players)
/// remain separate — coalescing them would misrepresent the resolution.
fn group_key(state: &GameState, entry: &StackEntry) -> StackGroupKey {
    let source_name = state
        .objects
        .get(&entry.source_id)
        .map(|o| o.name.clone())
        .unwrap_or_default();
    let (tag, description) = match &entry.kind {
        StackEntryKind::Spell { .. } => ("spell", None),
        StackEntryKind::ActivatedAbility { .. } => ("activated", None),
        StackEntryKind::TriggeredAbility { description, .. } => {
            ("triggered", description.as_deref())
        }
        StackEntryKind::KeywordAction { .. } => ("keyword", None),
    };
    let effective_ability = effective_stack_ability(state, entry);
    let targets = effective_ability
        .ability
        .map(flatten_targets_in_chain)
        .unwrap_or_default();
    let selected_mode_labels = effective_ability
        .ability
        .map(|ability| ability.selected_mode_labels.clone())
        .unwrap_or_default();
    let paid = state.stack_paid_facts.get(&entry.id).cloned();
    let provenance = match &entry.kind {
        StackEntryKind::TriggeredAbility { provenance, .. } => provenance.clone(),
        StackEntryKind::Spell { .. }
        | StackEntryKind::ActivatedAbility { .. }
        | StackEntryKind::KeywordAction { .. } => None,
    };
    StackGroupKey {
        source_name,
        tag,
        description: description.map(str::to_owned),
        selected_mode_labels,
        targets,
        paid,
        is_pending: effective_ability.is_pending,
        provenance,
    }
}

/// CR 110.4b: A permanent spell — "an artifact, battle, creature, enchantment,
/// or planeswalker spell." Lands are excluded because they aren't spells
/// (they're played, not cast). Used by resolution paths that distinguish
/// "spell that will enter the battlefield" from "non-permanent spell"
/// (e.g., Sneak's CR 702.190b alongside-attacker placement, which applies
/// only to permanent spells).
pub(crate) fn is_permanent_spell(state: &GameState, object_id: ObjectId) -> bool {
    use crate::types::card_type::CoreType;

    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };
    obj.card_types.core_types.iter().any(|ct| {
        matches!(
            ct,
            CoreType::Artifact
                | CoreType::Battle
                | CoreType::Creature
                | CoreType::Enchantment
                | CoreType::Planeswalker
        )
    })
}

/// CR 702.185a: Create the Warp delayed trigger that exiles the permanent at end step
/// and grants WarpExile casting permission. Shared between resolve_top (Execute path)
/// and engine_replacement (NeedsChoice path).
pub(crate) fn create_warp_delayed_trigger(
    state: &mut GameState,
    object_id: ObjectId,
    controller: crate::types::player::PlayerId,
    events: &mut Vec<GameEvent>,
) {
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, CastingPermission, DelayedTriggerCondition, Effect,
        ResolvedAbility,
    };
    use crate::types::phase::Phase;

    let exile_def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            origin: Some(Zone::Battlefield),
            destination: Zone::Exile,
            target: crate::types::ability::TargetFilter::SelfRef,
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
    )
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GrantCastingPermission {
            permission: CastingPermission::WarpExile {
                castable_after_turn: state.turn_number,
            },
            target: crate::types::ability::TargetFilter::SelfRef,
            grantee: crate::types::ability::PermissionGrantee::AbilityController,
        },
    ));

    let mut delayed_ability =
        ResolvedAbility::new(*exile_def.effect, vec![], object_id, controller);
    if let Some(sub) = exile_def.sub_ability {
        delayed_ability = delayed_ability.sub_ability(ResolvedAbility::new(
            *sub.effect,
            vec![],
            object_id,
            controller,
        ));
    }
    // CR 400.7: bind the delayed self-reference to the exact source authority.
    // A blinked return is a distinct incarnation and cannot satisfy this context.
    if let Some(source) = state.objects.get(&object_id) {
        delayed_ability.set_trigger_source_recursive(
            super::triggers::trigger_source_context_for_latch(state, source),
        );
    }

    super::triggers::install_delayed_trigger(
        state,
        crate::types::game_state::DelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
            ability: Box::new(delayed_ability),
            controller,
            source_id: object_id,
            one_shot: true,
            provenance: crate::types::identifiers::DelayedInstallIdentity::LegacyDelayed,
        },
        events,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::BackFaceData;
    use crate::game::triggers::{check_delayed_triggers, PendingTrigger};
    use crate::game::zones::{self, create_object, move_to_zone};
    use crate::types::ability::{
        CastingPermission, ControllerRef, CopyRetargetPermission, CostPaidObjectSnapshot, Effect,
        ModalChoice, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef, TypeFilter,
        TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{
        AutoMayChoice, MayTriggerAutoChoiceKey, MayTriggerOrigin, PendingCast, StackPaidSnapshot,
        WaitingFor,
    };
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaCost;
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    #[test]
    fn unassigned_distribution_rejects_all_inert_batch_candidates() {
        let self_counter = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: crate::types::counter::CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        assert!(self_counter_ability_is_batch_candidate(&self_counter));
        let mut divided_counter = self_counter.clone();
        divided_counter.distribute = Some(crate::types::game_state::DistributionUnit::Counters(
            "+1/+1".to_string(),
        ));
        assert!(!self_counter_ability_is_batch_candidate(&divided_counter));

        let gain_life = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(2),
            PlayerId(0),
        );
        assert!(fixed_controller_gain_life_ability_is_batch_candidate(
            &gain_life
        ));
        let mut divided_gain = gain_life.clone();
        divided_gain.distribute = Some(crate::types::game_state::DistributionUnit::Life);
        assert!(!fixed_controller_gain_life_ability_is_batch_candidate(
            &divided_gain
        ));

        let mut lose_life = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: None,
            },
            Vec::new(),
            ObjectId(3),
            PlayerId(0),
        );
        lose_life.player_scope = Some(crate::types::ability::PlayerFilter::Opponent);
        assert!(fixed_opponent_effect_ability_is_batch_candidate(&lose_life));
        let mut divided_loss = lose_life.clone();
        divided_loss.distribute = Some(crate::types::game_state::DistributionUnit::Life);
        assert!(!fixed_opponent_effect_ability_is_batch_candidate(
            &divided_loss
        ));
    }

    #[test]
    fn inert_trigger_identity_compares_unassigned_distribution_unit() {
        let mut a = ResolvedAbility::new(Effect::NoOp, Vec::new(), ObjectId(10), PlayerId(0));
        a.distribute = Some(crate::types::game_state::DistributionUnit::Damage);
        let mut same_shape_different_provenance = a.clone();
        same_shape_different_provenance.source_id = ObjectId(11);
        same_shape_different_provenance.ability_index = Some(7);

        assert!(inert_trigger_abilities_eq_ignoring_provenance(
            &a,
            &same_shape_different_provenance
        ));

        same_shape_different_provenance.distribute = None;
        assert!(!inert_trigger_abilities_eq_ignoring_provenance(
            &a,
            &same_shape_different_provenance
        ));
    }

    fn pending_spell_entry(id: ObjectId) -> StackEntry {
        StackEntry {
            id,
            source_id: id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(id.0),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        }
    }

    #[test]
    fn effective_stack_ability_prefers_final_entry_over_matching_inline_pending_cast() {
        let id = ObjectId(10);
        let mut final_ability = ResolvedAbility::new(Effect::NoOp, Vec::new(), id, PlayerId(0));
        final_ability.selected_mode_labels = vec!["Final mode.".to_string()];
        let entry = StackEntry {
            kind: StackEntryKind::Spell {
                card_id: CardId(10),
                ability: Some(Box::new(final_ability)),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
            ..pending_spell_entry(id)
        };

        let mut pending_ability = ResolvedAbility::new(Effect::NoOp, Vec::new(), id, PlayerId(0));
        pending_ability.selected_mode_labels = vec!["Pending mode.".to_string()];
        let pending = PendingCast::new(id, CardId(10), pending_ability, ManaCost::NoCost);
        let mut state = setup();
        state.waiting_for = WaitingFor::ModeChoice {
            player: PlayerId(0),
            modal: ModalChoice::default(),
            pending_cast: Box::new(pending),
            unavailable_modes: Vec::new(),
        };

        let effective = effective_stack_ability(&state, &entry);
        assert!(
            !effective.is_pending,
            "finalized entry must take precedence"
        );
        assert_eq!(
            effective.ability.unwrap().selected_mode_labels,
            ["Final mode."],
            "the matching inline pending cast must not replace a finalized ability",
        );
    }

    #[test]
    fn effective_stack_ability_uses_only_the_matching_inline_pending_cast() {
        let pending_id = ObjectId(11);
        let other_id = ObjectId(12);
        let mut ability = ResolvedAbility::new(Effect::NoOp, Vec::new(), pending_id, PlayerId(0));
        ability.selected_mode_labels = vec!["Selected mode.".to_string()];
        let pending = PendingCast::new(pending_id, CardId(11), ability, ManaCost::NoCost);
        let mut state = setup();
        state.waiting_for = WaitingFor::ModeChoice {
            player: PlayerId(0),
            modal: ModalChoice::default(),
            pending_cast: Box::new(pending),
            unavailable_modes: Vec::new(),
        };
        state.stack.push_back(pending_spell_entry(pending_id));
        state.stack.push_back(pending_spell_entry(other_id));

        let lower = effective_stack_ability(&state, state.stack.front().unwrap());
        assert!(
            lower.is_pending,
            "the matching lower stack entry is pending"
        );
        assert_eq!(
            lower.ability.unwrap().selected_mode_labels,
            ["Selected mode."],
        );
        let top = effective_stack_ability(&state, state.stack.back().unwrap());
        assert!(
            top.ability.is_none(),
            "a nonmatching top entry must not inherit labels"
        );
        assert!(!top.is_pending);
    }

    #[test]
    fn effective_stack_ability_uses_only_the_matching_outer_pending_spell() {
        let pending_id = ObjectId(13);
        let other_id = ObjectId(14);
        let mut ability = ResolvedAbility::new(Effect::NoOp, Vec::new(), pending_id, PlayerId(0));
        ability.selected_mode_labels = vec!["Mana-payment mode.".to_string()];
        let mut state = setup();
        state.waiting_for = WaitingFor::ManaPayment {
            player: PlayerId(0),
            convoke_mode: None,
        };
        state.pending_cast = Some(Box::new(PendingCast::new(
            pending_id,
            CardId(13),
            ability,
            ManaCost::NoCost,
        )));
        state.stack.push_back(pending_spell_entry(pending_id));
        state.stack.push_back(pending_spell_entry(other_id));

        let lower = effective_stack_ability(&state, state.stack.front().unwrap());
        assert!(
            lower.is_pending,
            "matching pending spell is discovered by object id"
        );
        let top = effective_stack_ability(&state, state.stack.back().unwrap());
        assert!(
            top.ability.is_none(),
            "no top-of-stack fallback is permitted"
        );
        assert!(!top.is_pending);
    }

    /// CR 115.1 + CR 603.3d — regression twin for the "don't ask again → Yes
    /// silently answers No" bug, covering the stack.rs consumer
    /// (`optional_ability_is_inert_under_auto_choice` →
    /// `ability_has_no_legal_resolution_targets`, reached by the on-stack
    /// inert-noop fast-forward). An auto-ACCEPTED optional ability that surfaces
    /// no stack-time target slot must NOT be classified inert. The `Sacrifice`
    /// arm (a non-context-ref filter that `extract_target_filter_from_effect`
    /// still declines) is the discriminator a context-ref-only guard would drop.
    #[test]
    fn auto_accepted_no_target_slot_ability_is_not_inert() {
        let source_id = ObjectId(100);
        let origin = MayTriggerOrigin::Printed { trigger_index: 0 };
        for effect in [
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Effect::Sacrifice {
                target: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
        ] {
            let mut state = setup();
            let mut ability = ResolvedAbility::new(effect, vec![], source_id, PlayerId(0));
            ability.optional = true;
            ability.may_trigger_origin = Some(origin.clone());
            state.set_may_trigger_auto_choice(
                MayTriggerAutoChoiceKey {
                    player: PlayerId(0),
                    source_id,
                    origin: origin.clone(),
                },
                AutoMayChoice::Accept,
            );

            assert!(
                !optional_ability_is_inert_under_auto_choice(&mut state, &ability, None),
                "an auto-accepted ability with no stack-time target slot is always \
                 resolvable and must not be suppressed as an inert no-op"
            );
        }
    }

    fn back_face_data(
        name: &str,
        core_type: CoreType,
        loyalty: Option<u32>,
        defense: Option<u32>,
    ) -> BackFaceData {
        let mut card_types = crate::types::card_type::CardType::default();
        card_types.core_types.push(core_type);
        BackFaceData {
            name: name.to_string(),
            power: None,
            toughness: None,
            loyalty,
            printed_loyalty: None,
            defense,
            card_types,
            mana_cost: Default::default(),
            keywords: vec![],
            abilities: vec![],
            trigger_definitions: Default::default(),
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
            color: vec![],
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            layout_kind: None,
            parse_warnings: vec![],
        }
    }

    fn create_aura_on_stack(state: &mut GameState, target_id: ObjectId) -> ObjectId {
        let aura_id = create_object(
            state,
            CardId(100),
            PlayerId(0),
            "Pacifism".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&aura_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.keywords.push(Keyword::Enchant(
                crate::types::ability::TargetFilter::Typed(TypedFilter::creature()),
            ));
        }

        let resolved = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "Aura".to_string(),
                description: None,
            },
            vec![TargetRef::Object(target_id)],
            aura_id,
            PlayerId(0),
        );

        state.stack.push_back(StackEntry {
            id: aura_id,
            source_id: aura_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(100),
                ability: Some(Box::new(resolved)),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        aura_id
    }

    #[test]
    fn targetless_damage_trigger_with_stale_pending_entry_is_removed() {
        let mut state = setup();
        let predator = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Trygon Predator".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&predator)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let off_context_artifact = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Off-context Artifact".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&off_context_artifact)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        let target = TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(
                    TypedFilter::default()
                        .with_type(TypeFilter::Artifact)
                        .controller(ControllerRef::TargetPlayer),
                ),
                TargetFilter::Typed(
                    TypedFilter::default()
                        .with_type(TypeFilter::Enchantment)
                        .controller(ControllerRef::TargetPlayer),
                ),
            ],
        };
        let mut ability = ResolvedAbility::new(
            Effect::Destroy {
                target,
                cant_regenerate: false,
            },
            vec![],
            predator,
            PlayerId(0),
        );
        ability.optional = true;
        let source_context = crate::game::triggers::trigger_source_context_for_latch(
            &state,
            state.objects.get(&predator).expect("fixture source"),
        );
        ability.set_trigger_source_recursive(source_context);

        let trigger_event = GameEvent::DamageDealt {
            source_id: predator,
            target: TargetRef::Player(PlayerId(1)),
            amount: 2,
            is_combat: true,
            excess: 0,
        };
        let description =
            "Whenever this creature deals combat damage to a player, you may destroy target artifact or enchantment that player controls."
                .to_string();
        let entry_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        state.stack.push_back(StackEntry {
            id: entry_id,
            source_id: predator,
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: predator,
                ability: Box::new(ability),
                condition: None,
                trigger_event: Some(trigger_event.clone()),
                description: Some(description.clone()),
                source_name: "Trygon Predator".to_string(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        });
        state.pending_trigger_entry = Some(entry_id);
        state
            .stack_trigger_firings
            .insert(entry_id, TriggerFiring::Ordinary);
        state.pending_trigger_firing = Some(TriggerFiring::Ordinary);
        state.pending_trigger_event_batch = vec![trigger_event.clone()];
        state.pending_trigger = Some(Box::new(PendingTrigger {
            source_id: predator,
            controller: PlayerId(0),
            condition: None,
            ability: Box::new(state.stack.back().unwrap().ability().unwrap().clone()),
            timestamp: state.turn_number,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: Some(trigger_event),
            modal: None,
            mode_abilities: Vec::new(),
            description: Some(description),
            may_trigger_origin: Some(MayTriggerOrigin::Printed { trigger_index: 0 }),
            subject_match_count: None,
            die_result: None,
            provenance: None,
        }));
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert!(state.stack.is_empty());
        assert!(state.pending_trigger_entry.is_none());
        assert!(state.pending_trigger.is_none());
        assert!(!matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ));
        assert!(events
            .iter()
            .any(|event| matches!(event, GameEvent::StackResolved { object_id } if *object_id == entry_id)));
    }

    #[test]
    fn permanent_spell_resolution_links_exiled_cost_paid_object() {
        let mut state = setup();
        let exiled_id = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Exiled Elemental".to_string(),
            Zone::Exile,
        );
        let snapshot = {
            let exiled = state.objects.get(&exiled_id).unwrap();
            CostPaidObjectSnapshot {
                object_id: exiled_id,
                lki: exiled.snapshot_for_mana_spent(),
            }
        };
        let spell_id = create_object(
            &mut state,
            CardId(102),
            PlayerId(0),
            "Champion of the Path".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "behold-cost-regression".to_string(),
                description: None,
            },
            vec![],
            spell_id,
            PlayerId(0),
        );
        ability.set_cost_paid_object_recursive(snapshot);

        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(102),
                ability: Some(Box::new(ability)),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert!(state.battlefield.contains(&spell_id));
        assert!(state.exile_links.iter().any(|link| {
            link.exiled_id == exiled_id
                && link.source_id == spell_id
                && matches!(
                    link.kind,
                    ExileLinkKind::UntilSourceLeaves {
                        return_zone: Zone::Hand
                    }
                )
        }));
    }

    /// CR 110.4b + CR 608.3 + CR 310.4b: Battle spells are permanent spells.
    /// They resolve to the battlefield, not to their owner's graveyard, and
    /// receive their intrinsic defense counters through the ETB replacement
    /// pipeline.
    #[test]
    fn battle_spell_resolves_to_battlefield_with_defense_counters() {
        let mut state = setup();
        let battle_id = create_object(
            &mut state,
            CardId(622),
            PlayerId(0),
            "Test Siege".to_string(),
            Zone::Stack,
        );
        {
            let battle = state.objects.get_mut(&battle_id).unwrap();
            battle.card_types.core_types.push(CoreType::Battle);
            battle.card_types.subtypes.push("Siege".to_string());
            battle.defense = Some(4);
            battle.base_defense = Some(4);
        }

        state.stack.push_back(StackEntry {
            id: battle_id,
            source_id: battle_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(622),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert_eq!(state.objects[&battle_id].zone, Zone::Battlefield);
        assert!(state.battlefield.contains(&battle_id));
        assert!(!state.players[0].graveyard.contains(&battle_id));
        assert_eq!(
            state.objects[&battle_id]
                .counters
                .get(&CounterType::Defense)
                .copied(),
            Some(4)
        );
    }

    /// CR 400.7d + CR 603.4 discriminating pin for the bucket-A migration of the
    /// spell-resolution permanent entry onto `zone_pipeline::deliver`. A kicked
    /// permanent spell with `ability == None` (placeholder permanent spell —
    /// vanilla / ETB-only creature with no on-resolve Spell ability) resolves
    /// the NON-paused Execute arm: the cast link normalized onto the stack
    /// object before `replace_event` must survive `reset_for_battlefield_entry`
    /// (CR 400.7) and land on the resulting permanent, because the migrated path
    /// no longer has the bespoke post-move restore epilogue — it relies entirely
    /// on `CastLinkSnapshot` inside `deliver`. The resume-path pin
    /// (`zone_change_replacement_choice_preserves_cast_link_for_resolving_spell`,
    /// engine_replacement.rs) covers the PAUSED path; this covers the direct
    /// `resolve_top` Execute path the resume pin does not drive.
    #[test]
    fn resolving_permanent_spell_preserves_cast_link_without_ability() {
        use crate::types::ability::{CastTimingPermission, KickerVariant};

        let mut state = setup();
        let spell_id = create_object(
            &mut state,
            CardId(623),
            PlayerId(0),
            "Kicked Vanilla Bear".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            // `finalize_cast_to_stack` stamps the cast link onto the stack
            // object; mirror that establishment for a placeholder permanent
            // spell (no `SpellContext` ability), so the Execute arm's
            // pre-`replace_event` normalization leaves the object value intact
            // and the `CastLinkSnapshot` captures it.
            obj.kickers_paid = vec![KickerVariant::First];
            obj.additional_cost_payment_count = 1;
            obj.convoked_creatures = vec![ObjectId(900)];
            obj.cast_from_zone = Some(Zone::Graveyard);
            obj.cast_controller = Some(PlayerId(0));
            obj.cast_timing_permission =
                Some((CastTimingPermission::AsThoughHadFlash, state.turn_number));
        }

        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(623),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        let obj = &state.objects[&spell_id];
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(
            obj.kickers_paid,
            vec![KickerVariant::First],
            "CR 400.7d: the resolved permanent must keep the kicker payments of \
             the spell that became it — the entry reset cleared them and the \
             migrated Execute arm restores them only via CastLinkSnapshot"
        );
        assert_eq!(obj.additional_cost_payment_count, 1);
        assert_eq!(obj.convoked_creatures, vec![ObjectId(900)]);
        assert_eq!(obj.cast_from_zone, Some(Zone::Graveyard));
        assert_eq!(obj.cast_controller, Some(PlayerId(0)));
        assert_eq!(
            obj.cast_timing_permission,
            Some((CastTimingPermission::AsThoughHadFlash, state.turn_number)),
            "CR 603.4: cast-timing permission is re-stamped with the resolution \
             turn so same-turn trigger gates compare equal"
        );
    }

    /// CR 707.10f + CR 608.3f: A copy of a PERMANENT spell, as it resolves onto
    /// the battlefield, ceases being a copy and becomes a token permanent. Drives
    /// the real spell-resolution → battlefield path (`resolve_top` →
    /// `deliver_replaced_zone_change`). Revert probe: without the copy-gated flip,
    /// `is_copy` stays true, `is_token` stays false, and
    /// `is_represented_by_a_card()` wrongly returns false — and the CR 704.5e SBA
    /// would later sweep the permanent off the battlefield the moment it moved.
    #[test]
    fn resolving_permanent_copy_becomes_a_token() {
        let mut state = setup();
        let spell_id = create_object(
            &mut state,
            CardId(700),
            PlayerId(0),
            "Permanent Copy".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.is_copy = true;
            obj.is_token = false;
        }
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(700),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        let obj = &state.objects[&spell_id];
        assert_eq!(obj.zone, Zone::Battlefield);
        assert!(
            obj.is_token,
            "CR 707.10f: a permanent copy becomes a token as it resolves"
        );
        assert!(
            !obj.is_copy,
            "CR 707.10f: it is no longer a copy of a spell once on the battlefield"
        );
        assert!(
            !obj.is_represented_by_a_card(),
            "CR 111.1: a token permanent is not represented by a card"
        );
    }

    /// Multi-authority negative for the CR 707.10f flip: a REAL permanent
    /// (is_copy = false) resolving to the battlefield stays a card — the flip is
    /// copy-gated and must not turn every entering permanent into a token.
    #[test]
    fn resolving_real_permanent_stays_a_card() {
        let mut state = setup();
        let spell_id = create_object(
            &mut state,
            CardId(701),
            PlayerId(0),
            "Real Bear".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.is_copy = false;
            obj.is_token = false;
        }
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(701),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        let obj = &state.objects[&spell_id];
        assert_eq!(obj.zone, Zone::Battlefield);
        assert!(!obj.is_token, "a real permanent must not become a token");
        assert!(!obj.is_copy);
        assert!(
            obj.is_represented_by_a_card(),
            "a real permanent is still represented by a card"
        );
    }

    /// CR 724.1b: "end the turn" exiles every object on the stack, including
    /// the resolving spell itself. Discriminating against routing the source
    /// through the normal CR 608.2n instant/sorcery graveyard path.
    #[test]
    fn end_the_turn_spell_exiles_resolving_object() {
        let mut state = setup();
        let spell_id = create_object(
            &mut state,
            CardId(724),
            PlayerId(0),
            "Time Stop".to_string(),
            Zone::Stack,
        );
        let ability = ResolvedAbility::new(Effect::EndTheTurn, vec![], spell_id, PlayerId(0));

        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(724),
                ability: Some(Box::new(ability)),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert_eq!(state.objects[&spell_id].zone, Zone::Exile);
        assert!(state.exile.contains(&spell_id));
        assert!(!state.players[0].graveyard.contains(&spell_id));
    }

    #[test]
    fn trigger_event_context_becomes_target_controller() {
        // Set up: triggered ability with BecomesTarget event in trigger_event.
        // Verify: at resolution, current_trigger_event is set so
        // TriggeringSpellController can resolve to the controller of the source.
        let mut state = setup();

        // Create a "spell" object controlled by player 1 that is the source in BecomesTarget
        let spell_id = create_object(
            &mut state,
            CardId(80),
            PlayerId(1),
            "Lightning Bolt".to_string(),
            Zone::Stack,
        );

        let trigger_event = GameEvent::BecomesTarget {
            target: TargetRef::Object(ObjectId(999)), // target doesn't matter for this test
            source_id: spell_id,
            source_controller: PlayerId(0),
        };

        // Build a triggered ability that would want to resolve TriggeringSpellController
        let resolved = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "EventContextTest".to_string(),
                description: None,
            },
            vec![],
            ObjectId(50),
            PlayerId(0),
        );

        let entry_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;

        state.stack.push_back(StackEntry {
            id: entry_id,
            source_id: ObjectId(50),
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: ObjectId(50),
                ability: Box::new(resolved),
                condition: None,
                trigger_event: Some(trigger_event.clone()),
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        });

        // Before resolution, current_trigger_event should be None
        assert!(state.current_trigger_event.is_none());

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        // After resolution, current_trigger_event should be cleared
        assert!(state.current_trigger_event.is_none());

        // Verify the event was set during resolution by checking the resolve happened
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::StackResolved { .. })));

        // Verify event-context resolution works with the trigger event
        // by manually setting and checking the resolution function
        state.current_trigger_event = Some(trigger_event);
        let result = crate::game::targeting::resolve_event_context_target(
            &state,
            &crate::types::ability::TargetFilter::TriggeringSpellController,
            ObjectId(50),
        );
        assert_eq!(result, Some(TargetRef::Player(PlayerId(1))));

        // TriggeringSpellOwner should return the owner
        let result = crate::game::targeting::resolve_event_context_target(
            &state,
            &crate::types::ability::TargetFilter::TriggeringSpellOwner,
            ObjectId(50),
        );
        assert_eq!(result, Some(TargetRef::Player(PlayerId(1))));

        // TriggeringSource should return the source object
        let result = crate::game::targeting::resolve_event_context_target(
            &state,
            &crate::types::ability::TargetFilter::TriggeringSource,
            ObjectId(50),
        );
        assert_eq!(result, Some(TargetRef::Object(spell_id)));

        // Clean up
        state.current_trigger_event = None;
    }

    #[test]
    fn trigger_event_context_no_event_returns_none() {
        let state = setup();
        // With no current_trigger_event, resolution should return None
        let result = crate::game::targeting::resolve_event_context_target(
            &state,
            &crate::types::ability::TargetFilter::TriggeringSpellController,
            ObjectId(1),
        );
        assert!(result.is_none());
    }

    #[test]
    fn aura_resolving_attaches_to_target() {
        let mut state = setup();

        // Create a creature on the battlefield
        let creature = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Create an Aura spell targeting the creature
        let aura_id = create_aura_on_stack(&mut state, creature);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        // Aura should be on the battlefield
        assert!(state.battlefield.contains(&aura_id));
        // Aura should be attached to the creature
        assert_eq!(
            state
                .objects
                .get(&aura_id)
                .unwrap()
                .attached_to
                .and_then(|t| t.as_object()),
            Some(creature)
        );
        // Creature should list the Aura in its attachments
        assert!(state
            .objects
            .get(&creature)
            .unwrap()
            .attachments
            .contains(&aura_id));
    }

    #[test]
    fn aura_fizzles_when_target_left_battlefield() {
        let mut state = setup();

        // Create a creature, then remove it from battlefield before resolution
        let creature = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let aura_id = create_aura_on_stack(&mut state, creature);

        // Remove creature from battlefield before resolution
        state.battlefield.retain(|&id| id != creature);
        if let Some(obj) = state.objects.get_mut(&creature) {
            obj.zone = Zone::Graveyard;
        }
        state.players[1].graveyard.push_back(creature);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        // Aura should fizzle to graveyard (not to battlefield)
        assert!(!state.battlefield.contains(&aura_id));
        assert!(state.players[0].graveyard.contains(&aura_id));
    }

    #[test]
    fn non_aura_permanent_resolving_no_attachment() {
        let mut state = setup();

        // Create a non-Aura enchantment on the stack
        let ench_id = create_object(
            &mut state,
            CardId(60),
            PlayerId(0),
            "Intangible Virtue".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&ench_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Enchantment);

        state.stack.push_back(StackEntry {
            id: ench_id,
            source_id: ench_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(60),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        // Should be on battlefield, not attached to anything
        assert!(state.battlefield.contains(&ench_id));
        assert_eq!(state.objects.get(&ench_id).unwrap().attached_to, None);
    }

    #[test]
    fn multi_target_chain_resolves_remaining_legal_target() {
        let mut state = setup();

        let first_target = create_object(
            &mut state,
            CardId(70),
            PlayerId(1),
            "First Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&first_target).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(3);
            obj.toughness = Some(3);
        }

        let second_target = create_object(
            &mut state,
            CardId(71),
            PlayerId(1),
            "Second Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&second_target).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(3);
            obj.toughness = Some(3);
        }

        let spell_id = create_object(
            &mut state,
            CardId(72),
            PlayerId(0),
            "Twin Bolt".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: crate::types::ability::TargetFilter::Typed(TypedFilter::creature()),
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Object(first_target)],
            spell_id,
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: crate::types::ability::TargetFilter::Typed(TypedFilter::creature()),
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Object(second_target)],
            spell_id,
            PlayerId(0),
        ));

        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(72),
                ability: Some(Box::new(ability)),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        state.battlefield.retain(|&id| id != first_target);
        state.objects.get_mut(&first_target).unwrap().zone = Zone::Graveyard;
        state.players[1].graveyard.push_back(first_target);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert!(state.players[0].graveyard.contains(&spell_id));
        assert_eq!(state.objects[&second_target].damage_marked, 2);
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::DamageDealt {
                    target: TargetRef::Object(target),
                    amount: 2,
                    ..
                } if *target == second_target
            )),
            "expected the remaining legal target to be damaged"
        );
    }

    #[test]
    fn warp_delayed_trigger_grants_warp_exile_not_alt_cost() {
        // CR 702.185a: The delayed trigger should grant WarpExile (normal cost),
        // not ExileWithAltCost (which would use the warp cost).
        use crate::types::ability::CastingPermission;
        use crate::types::game_state::{StackEntry, StackEntryKind};
        use crate::types::mana::ManaCost;

        let mut state = setup();
        state.turn_number = 3;
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Warp Creature".to_string(),
            Zone::Battlefield,
        );
        // Give the object a Warp keyword with a cheap cost {R}
        // and a different normal cost {2}{R}
        let warp_cost = ManaCost::Cost {
            shards: vec![crate::types::mana::ManaCostShard::Red],
            generic: 0,
        };
        let normal_cost = ManaCost::Cost {
            shards: vec![crate::types::mana::ManaCostShard::Red],
            generic: 2,
        };
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.keywords.push(Keyword::Warp(warp_cost));
            obj.mana_cost = normal_cost;
            obj.card_types.core_types.push(CoreType::Creature);
        }

        // Push a stack entry as if cast via Warp
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: crate::types::game_state::CastingVariant::Warp,
                actual_mana_spent: 0,
            },
        });

        // Resolve the stack entry — this should create a Warp delayed trigger
        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        // Verify a delayed trigger was created
        assert_eq!(
            state.delayed_triggers.len(),
            1,
            "should have created one delayed trigger"
        );

        // Check the delayed trigger's sub_ability grants WarpExile
        let trigger = &state.delayed_triggers[0];
        let sub = trigger
            .ability
            .sub_ability
            .as_ref()
            .expect("should have sub_ability");
        match &sub.effect {
            Effect::GrantCastingPermission { permission, .. } => match permission {
                CastingPermission::WarpExile {
                    castable_after_turn,
                } => {
                    assert_eq!(
                        *castable_after_turn, 3,
                        "castable_after_turn should match the turn number at resolution"
                    );
                }
                other => panic!("expected WarpExile, got {other:?}"),
            },
            other => panic!("expected GrantCastingPermission, got {other:?}"),
        }
    }

    #[test]
    fn warp_exile_respects_turn_restriction() {
        // CR 702.185a: WarpExile cards should not be castable on the same turn
        // they were exiled, only after the turn ends.
        use crate::game::casting::spell_objects_available_to_cast;
        use crate::types::ability::CastingPermission;

        let mut state = setup();
        state.turn_number = 3;

        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Warp Creature".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.casting_permissions.push(CastingPermission::WarpExile {
                castable_after_turn: 3,
            });
        }

        // On the same turn (turn 3): should NOT be castable
        let available = spell_objects_available_to_cast(&state, PlayerId(0));
        assert!(
            !available.contains(&obj_id),
            "WarpExile card should NOT be castable on the same turn it was exiled"
        );

        // On the next turn (turn 4): should be castable
        state.turn_number = 4;
        let available = spell_objects_available_to_cast(&state, PlayerId(0));
        assert!(
            available.contains(&obj_id),
            "WarpExile card should be castable after the exile turn ends"
        );
    }

    #[test]
    fn warp_exile_does_not_emit_airbend_event() {
        // CR 702.185a: WarpExile permissions should NOT trigger Airbend events.
        use crate::types::ability::{CastingPermission, Effect, TargetFilter};

        let mut state = setup();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Warp Card".to_string(),
            Zone::Exile,
        );

        let ability = ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: CastingPermission::WarpExile {
                    castable_after_turn: 1,
                },
                target: TargetFilter::SelfRef,
                grantee: crate::types::ability::PermissionGrantee::AbilityController,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        crate::game::effects::grant_permission::resolve(&mut state, &ability, &mut events).unwrap();

        // Verify permission was granted
        let obj = state.objects.get(&obj_id).unwrap();
        assert!(
            obj.casting_permissions
                .iter()
                .any(|p| matches!(p, CastingPermission::WarpExile { .. })),
            "WarpExile permission should be on the object"
        );

        // Verify no Airbend event was emitted
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, crate::types::events::GameEvent::Airbend { .. })),
            "WarpExile should NOT emit Airbend event"
        );
    }

    #[test]
    fn warp_delayed_trigger_does_not_exile_blinked_creature() {
        // CR 400.7: A blinked creature is a new object (higher incarnation).
        // The warp delayed trigger's SelfRef must fail to resolve against the
        // re-entered permanent, leaving it on the battlefield.

        let mut state = setup();
        state.turn_number = 3;
        state.active_player = PlayerId(0);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Quantum Riddler".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.keywords.push(Keyword::Warp(ManaCost::generic(3)));
            obj.mana_cost = ManaCost::generic(4);
            obj.card_types.core_types.push(CoreType::Creature);
        }

        // Push a stack entry as if cast via Warp, then resolve to install the
        // delayed trigger (which now stamps exact source context).
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Warp,
                actual_mana_spent: 0,
            },
        });
        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);
        assert_eq!(state.delayed_triggers.len(), 1);

        // Record the incarnation at the time the delayed trigger was created.
        let stamped_incarnation = state.objects[&obj_id].incarnation;

        // Simulate a blink: exile then return to battlefield.
        move_to_zone(&mut state, obj_id, Zone::Exile, &mut Vec::new());
        move_to_zone(&mut state, obj_id, Zone::Battlefield, &mut Vec::new());

        // The re-entered permanent has a higher incarnation.
        assert!(
            state.objects[&obj_id].incarnation > stamped_incarnation,
            "blink must bump incarnation"
        );
        assert_eq!(state.objects[&obj_id].zone, Zone::Battlefield);

        // Fire the delayed trigger at the next end step.
        state.phase = Phase::End;
        let stacked =
            check_delayed_triggers(&mut state, &[GameEvent::PhaseChanged { phase: Phase::End }]);
        assert!(
            !stacked.is_empty(),
            "the warp delayed trigger still fires (it keys on the phase)"
        );

        // Resolve the delayed trigger — SelfRef should find nothing because
        // the incarnation no longer matches.
        resolve_top(&mut state, &mut Vec::new());

        // The creature must still be on the battlefield.
        assert_eq!(
            state.objects[&obj_id].zone,
            Zone::Battlefield,
            "a blinked warp creature must NOT be exiled by the stale delayed trigger"
        );
    }

    #[test]
    fn warp_delayed_trigger_exiles_same_incarnation_creature_and_grants_recast_permission() {
        // CR 702.185a + CR 400.7: the delayed trigger still finds the same
        // object instance and grants its exile casting permission.
        let mut state = setup();
        state.turn_number = 3;
        state.active_player = PlayerId(0);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Quantum Riddler".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.keywords.push(Keyword::Warp(ManaCost::generic(3)));
            obj.mana_cost = ManaCost::generic(4);
            obj.card_types.core_types.push(CoreType::Creature);
        }

        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Warp,
                actual_mana_spent: 0,
            },
        });
        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);
        assert_eq!(state.delayed_triggers.len(), 1);

        state.phase = Phase::End;
        let stacked =
            check_delayed_triggers(&mut state, &[GameEvent::PhaseChanged { phase: Phase::End }]);
        assert!(
            !stacked.is_empty(),
            "the warp delayed trigger should fire at end step"
        );
        resolve_top(&mut state, &mut Vec::new());

        let obj = &state.objects[&obj_id];
        assert_eq!(
            obj.zone,
            Zone::Exile,
            "an unblinked warp creature should be exiled by its delayed trigger"
        );
        assert!(
            obj.casting_permissions.iter().any(|p| matches!(
                p,
                CastingPermission::WarpExile {
                    castable_after_turn: 3
                }
            )),
            "the exiled warp creature should receive WarpExile permission"
        );
    }

    #[test]
    fn warp_cast_stamps_cast_variant_paid_warp_marker() {
        // CR 702.185a + CR 400.7: a permanent cast for its warp cost must carry
        // the per-object `cast_variant_paid` marker keyed on the cast turn, so the
        // target-scoped "if that creature was cast for its warp cost" rider
        // (Full Bore) can read it. Reverting the stack stamp leaves the marker
        // `None` and this assertion fails.
        let mut state = setup();
        state.turn_number = 5;
        state.active_player = PlayerId(0);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Quantum Riddler".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.keywords.push(Keyword::Warp(ManaCost::generic(3)));
            obj.mana_cost = ManaCost::generic(4);
            obj.card_types.core_types.push(CoreType::Creature);
        }
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Warp,
                actual_mana_spent: 0,
            },
        });
        resolve_top(&mut state, &mut Vec::new());
        assert_eq!(
            state.objects[&obj_id].cast_variant_paid,
            Some((crate::types::ability::CastVariantPaid::Warp, 5)),
            "a warp cast must stamp the per-object Warp marker keyed on the cast turn"
        );
    }

    #[test]
    fn full_bore_grants_trample_haste_only_to_warp_cast_target() {
        // CR 115.1 + CR 608.2c + CR 702.185a: end-to-end production-path test for
        // the whole feature. Warp-cast a creature through the real stack pipeline
        // (stamping its per-object marker), then resolve Full Bore (Pump +
        // target-scoped conditional grant) against it via the real ability
        // resolver. The grant fires because the TARGET was warp-cast even though
        // Full Bore's own source spell was NOT warp-cast — so reverting the
        // evaluator's Target branch to read the source makes the warp-cast
        // creature miss the grant (the source spell has no marker) and the
        // positive assertion fails. The hard-cast creature (no marker) gets the
        // +3/+2 only, proving the condition is load-bearing.
        use crate::game::ability_utils::build_resolved_from_def_with_targets;
        use crate::game::effects::resolve_ability_chain;
        use crate::game::layers::evaluate_layers;
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;

        fn make_creature(state: &mut GameState, id: u64, name: &str) -> ObjectId {
            let obj_id = create_object(
                state,
                CardId(id),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.keywords.push(Keyword::Warp(ManaCost::generic(3)));
            obj.mana_cost = ManaCost::generic(4);
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj_id
        }

        let mut state = setup();
        state.turn_number = 6;
        state.active_player = PlayerId(0);

        // Creature A: warp-cast through the production pipeline → marker stamped.
        let warp_creature = make_creature(&mut state, 1, "Warp Brute");
        state.stack.push_back(StackEntry {
            id: warp_creature,
            source_id: warp_creature,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Warp,
                actual_mana_spent: 0,
            },
        });
        resolve_top(&mut state, &mut Vec::new());
        assert_eq!(
            state.objects[&warp_creature].cast_variant_paid,
            Some((crate::types::ability::CastVariantPaid::Warp, 6)),
            "precondition: warp cast stamped the per-object marker"
        );

        // Creature B: hard-cast (never warp-cast) → no marker.
        let hard_creature = make_creature(&mut state, 2, "Hard Brute");
        assert_eq!(state.objects[&hard_creature].cast_variant_paid, None);

        // Full Bore source spell — itself NOT warp-cast (no marker).
        let full_bore = create_object(
            &mut state,
            CardId(9),
            PlayerId(0),
            "Full Bore".to_string(),
            Zone::Stack,
        );

        let def = parse_effect_chain(
            "Target creature you control gets +3/+2 until end of turn. If that creature was cast for its warp cost, it also gains trample and haste until end of turn.",
            AbilityKind::Spell,
        );

        // Full Bore on the warp-cast creature → +3/+2 AND trample + haste.
        let resolved_a = build_resolved_from_def_with_targets(
            &def,
            full_bore,
            PlayerId(0),
            vec![TargetRef::Object(warp_creature)],
        );
        resolve_ability_chain(&mut state, &resolved_a, &mut Vec::new(), 0).unwrap();
        evaluate_layers(&mut state);
        assert!(
            state.objects[&warp_creature].has_keyword(&Keyword::Trample)
                && state.objects[&warp_creature].has_keyword(&Keyword::Haste),
            "warp-cast target must gain trample AND haste from the target-scoped rider \
             even though the Full Bore source spell was not warp-cast"
        );

        // Full Bore on the hard-cast creature → +3/+2 only, NO trample/haste.
        let resolved_b = build_resolved_from_def_with_targets(
            &def,
            full_bore,
            PlayerId(0),
            vec![TargetRef::Object(hard_creature)],
        );
        resolve_ability_chain(&mut state, &resolved_b, &mut Vec::new(), 0).unwrap();
        evaluate_layers(&mut state);
        assert!(
            !state.objects[&hard_creature].has_keyword(&Keyword::Trample)
                && !state.objects[&hard_creature].has_keyword(&Keyword::Haste),
            "hard-cast target must NOT gain trample/haste — the warp condition is load-bearing"
        );
    }

    #[test]
    fn normal_cast_does_not_stamp_warp_marker() {
        // CR 702.185a: re-casting an exiled warp card (or any non-warp cast) uses
        // `CastingVariant::Normal` and carries NO warp marker, so the warp-scoped
        // rider must NOT fire for it.
        let mut state = setup();
        state.turn_number = 5;
        state.active_player = PlayerId(0);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Quantum Riddler".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.keywords.push(Keyword::Warp(ManaCost::generic(3)));
            obj.mana_cost = ManaCost::generic(4);
            obj.card_types.core_types.push(CoreType::Creature);
        }
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 4,
            },
        });
        resolve_top(&mut state, &mut Vec::new());
        assert_eq!(
            state.objects[&obj_id].cast_variant_paid, None,
            "a normal (non-warp) cast must not stamp the warp marker"
        );
    }

    #[test]
    fn exile_with_alt_cost_still_works() {
        // Regression: ExileWithAltCost (Airbending, etc.) should still be immediately castable.
        use crate::game::casting::spell_objects_available_to_cast;
        use crate::types::ability::CastingPermission;
        use crate::types::mana::ManaCost;

        let mut state = setup();
        state.turn_number = 5;

        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Airbent Card".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.casting_permissions
                .push(CastingPermission::ExileWithAltCost {
                    cost: ManaCost::generic(2),
                    cast_transformed: false,
                    constraint: None,
                    granted_to: None,
                    resolution_cleanup: None,
                    duration: None,

                    graveyard_replacement: None,
                    enters_with_counter: None,
                    enters_with_modifications: Vec::new(),
                    mana_spend_permission: None,
                });
        }

        // Should be immediately castable (no turn restriction)
        let available = spell_objects_available_to_cast(&state, PlayerId(0));
        assert!(
            available.contains(&obj_id),
            "ExileWithAltCost should be immediately castable (no turn restriction)"
        );
    }

    // -----------------------------------------------------------------------
    // Flashback zone routing (CR 702.34a)
    // -----------------------------------------------------------------------

    /// Helper: push a Flashback spell onto the stack and return its ObjectId.
    fn push_flashback_spell(state: &mut GameState, effect: Effect) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let obj_id = create_object(
            state,
            card_id,
            PlayerId(0),
            "Flashback Spell".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
        }
        let resolved = ResolvedAbility::new(effect, vec![], obj_id, PlayerId(0));
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id,
                ability: Some(Box::new(resolved)),
                casting_variant: CastingVariant::Flashback,
                actual_mana_spent: 0,
            },
        });
        obj_id
    }

    fn push_graveyard_permission_spell_with_exile_rider(
        state: &mut GameState,
        effect: Effect,
    ) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let obj_id = create_object(
            state,
            card_id,
            PlayerId(0),
            "Permission Spell".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
        }
        let resolved = ResolvedAbility::new(effect, vec![], obj_id, PlayerId(0));
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id,
                ability: Some(Box::new(resolved)),
                casting_variant: CastingVariant::GraveyardPermission {
                    source: ObjectId(999),
                    frequency: crate::types::statics::CastFrequency::OncePerTurn,
                    slot_type: None,
                    graveyard_destination_replacement: Some(Zone::Exile),
                },
                actual_mana_spent: 0,
            },
        });
        obj_id
    }

    #[test]
    fn flashback_spell_exiles_on_resolution() {
        let mut state = setup();
        let obj_id = push_flashback_spell(
            &mut state,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert_eq!(
            state.objects[&obj_id].zone,
            Zone::Exile,
            "Flashback spell should be exiled on resolution, not sent to graveyard"
        );
    }

    #[test]
    fn graveyard_permission_exile_rider_exiles_on_resolution() {
        let mut state = setup();
        let obj_id = push_graveyard_permission_spell_with_exile_rider(
            &mut state,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert_eq!(
            state.objects[&obj_id].zone,
            Zone::Exile,
            "graveyard permission rider should replace the normal resolution graveyard destination"
        );
    }

    #[test]
    fn flashback_spell_exiles_on_fizzle() {
        let mut state = setup();

        // Create a target creature that we'll remove to cause fizzle
        let target_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(1),
            "Target Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&target_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }
        state.battlefield.push_back(target_id);

        // Push a flashback spell targeting that creature
        let card_id = CardId(state.next_object_id);
        let spell_id = create_object(
            &mut state,
            card_id,
            PlayerId(0),
            "Flashback Bolt".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
        }
        let resolved = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Object(target_id)],
            spell_id,
            PlayerId(0),
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id,
                ability: Some(Box::new(resolved)),
                casting_variant: CastingVariant::Flashback,
                actual_mana_spent: 0,
            },
        });

        // Remove the target to cause fizzle
        zones::move_to_zone(&mut state, target_id, Zone::Graveyard, &mut Vec::new());

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert_eq!(
            state.objects[&spell_id].zone,
            Zone::Exile,
            "Flashback spell should be exiled on fizzle, not sent to graveyard"
        );
    }

    #[test]
    fn stack_pressure_boundaries() {
        let mut state = GameState::new_two_player(42);
        assert_eq!(stack_pressure(&state), StackPressure::Normal);

        // Synthesize entries; kind/source doesn't matter for pressure.
        fn push_n(state: &mut GameState, n: usize) {
            use crate::types::card_type::CoreType;
            use crate::types::identifiers::{CardId, ObjectId};
            let src = crate::game::zones::create_object(
                state,
                CardId(1),
                PlayerId(0),
                "filler".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&src)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
            for i in 0..n {
                state.stack.push_back(StackEntry {
                    id: ObjectId(100_000 + i as u64),
                    source_id: src,
                    controller: PlayerId(0),
                    kind: StackEntryKind::Spell {
                        card_id: CardId(1),
                        ability: None,
                        casting_variant: CastingVariant::default(),
                        actual_mana_spent: 0,
                    },
                });
            }
        }

        // 9 entries → still Normal
        push_n(&mut state, 9);
        assert_eq!(stack_pressure(&state), StackPressure::Normal);
        // 10th crosses Elevated
        push_n(&mut state, 1);
        assert_eq!(stack_pressure(&state), StackPressure::Elevated);
        // 29 total → still Elevated
        push_n(&mut state, 19);
        assert_eq!(stack_pressure(&state), StackPressure::Elevated);
        // 30th crosses Rapid
        push_n(&mut state, 1);
        assert_eq!(stack_pressure(&state), StackPressure::Rapid);
        // 99 total → still Rapid
        push_n(&mut state, 69);
        assert_eq!(stack_pressure(&state), StackPressure::Rapid);
        // 100th crosses Instant
        push_n(&mut state, 1);
        assert_eq!(stack_pressure(&state), StackPressure::Instant);
    }

    #[test]
    fn stack_display_groups_coalesce_identical_triggers() {
        use crate::types::ability::{Effect, ResolvedAbility};
        use crate::types::identifiers::{CardId, ObjectId};

        let mut state = GameState::new_two_player(42);
        let mk_effect = || Effect::Unimplemented {
            name: "test".to_string(),
            description: None,
        };

        // 100 Scute-Swarm-like sources all sharing the same name — each fires
        // its own copy of the ETB trigger. The group key (source name + kind
        // + description) collapses them.
        for i in 0..100 {
            let sid = crate::game::zones::create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Scute Swarm".to_string(),
                Zone::Battlefield,
            );
            state.stack.push_back(StackEntry {
                id: ObjectId(10_000 + i as u64),
                source_id: sid,
                controller: PlayerId(0),
                kind: StackEntryKind::TriggeredAbility {
                    source_id: sid,
                    ability: Box::new(ResolvedAbility::new(mk_effect(), vec![], sid, PlayerId(0))),
                    condition: None,
                    trigger_event: None,
                    description: Some("landfall copy trigger".to_string()),
                    source_name: String::new(),
                    subject_match_count: None,
                    die_result: None,
                    provenance: None,
                },
            });
        }

        let groups = stack_display_groups(&state);
        assert_eq!(
            groups.len(),
            1,
            "100 identical Scute Swarm triggers should collapse to one group"
        );
        assert_eq!(groups[0].count, 100);
        assert_eq!(groups[0].member_ids.len(), 100);
    }

    #[test]
    fn stack_display_groups_keep_different_storm_copy_counts_separate() {
        use crate::types::ability::{Effect, ResolvedAbility};
        use crate::types::game_state::SyntheticTriggerProvenance;
        use crate::types::identifiers::{CardId, ObjectId};

        let mut state = GameState::new_two_player(42);
        let source = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grapeshot".to_string(),
            Zone::Stack,
        );
        for (id, copy_count) in [(ObjectId(10_001), 1), (ObjectId(10_002), 2)] {
            state.stack.push_back(StackEntry {
                id,
                source_id: source,
                controller: PlayerId(0),
                kind: StackEntryKind::TriggeredAbility {
                    source_id: source,
                    ability: Box::new(ResolvedAbility::new(
                        Effect::CopySpell {
                            target: TargetFilter::SelfRef,
                            retarget: CopyRetargetPermission::MayChooseNewTargets,
                            copier: None,
                            additional_modifications: Vec::new(),
                            starting_loyalty_from_casualty_sacrifice: false,
                        },
                        vec![],
                        source,
                        PlayerId(0),
                    )),
                    condition: None,
                    trigger_event: None,
                    description: Some("Storm".to_string()),
                    source_name: "Grapeshot".to_string(),
                    subject_match_count: None,
                    die_result: None,
                    provenance: Some(SyntheticTriggerProvenance::Storm { copy_count }),
                },
            });
        }

        assert_eq!(stack_display_groups(&state).len(), 2);
    }

    #[test]
    fn stack_display_groups_coalesce_identical_triggers_from_distinct_events() {
        use crate::types::ability::{Effect, ResolvedAbility};
        use crate::types::events::GameEvent;
        use crate::types::identifiers::{CardId, ObjectId};

        let mut state = GameState::new_two_player(42);
        let source = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Honored Dreyleader".to_string(),
            Zone::Battlefield,
        );
        let effect = Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SelfRef,
        };
        for (idx, trigger_event) in [
            GameEvent::LifeChanged {
                player_id: PlayerId(0),
                amount: 1,
            },
            GameEvent::LifeChanged {
                player_id: PlayerId(1),
                amount: 1,
            },
        ]
        .into_iter()
        .enumerate()
        {
            state.stack.push_back(StackEntry {
                id: ObjectId(10_000 + idx as u64),
                source_id: source,
                controller: PlayerId(0),
                kind: StackEntryKind::TriggeredAbility {
                    source_id: source,
                    ability: Box::new(ResolvedAbility::new(
                        effect.clone(),
                        vec![],
                        source,
                        PlayerId(0),
                    )),
                    condition: None,
                    trigger_event: Some(trigger_event),
                    description: Some("put a +1/+1 counter on this creature".to_string()),
                    source_name: "Honored Dreyleader".to_string(),
                    subject_match_count: None,
                    die_result: None,
                    provenance: None,
                },
            });
        }

        let groups = stack_display_groups(&state);
        assert_eq!(
            groups.len(),
            1,
            "display grouping must ignore hidden trigger-event identity for identical entries"
        );
        assert_eq!(groups[0].count, 2);
    }

    #[test]
    fn stack_display_groups_distinguish_different_sources() {
        use crate::types::ability::{Effect, ResolvedAbility};
        use crate::types::identifiers::CardId;

        let mut state = GameState::new_two_player(42);
        let s1 = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Scute Swarm".to_string(),
            Zone::Battlefield,
        );
        let s2 = crate::game::zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Impact Tremors".to_string(),
            Zone::Battlefield,
        );
        let mk_effect = || Effect::Unimplemented {
            name: "test".to_string(),
            description: None,
        };
        let mk_entry = |sid| StackEntry {
            id: sid,
            source_id: sid,
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: sid,
                ability: Box::new(ResolvedAbility::new(mk_effect(), vec![], sid, PlayerId(0))),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        };
        state.stack.push_back(mk_entry(s1));
        state.stack.push_back(mk_entry(s2));

        let groups = stack_display_groups(&state);
        assert_eq!(
            groups.len(),
            2,
            "different-named sources must stay separate"
        );
        assert_eq!(groups[0].count, 1);
        assert_eq!(groups[1].count, 1);
    }

    /// Two visually-identical triggers that target different players must NOT
    /// coalesce — coalescing them would misrepresent the resolved targeting.
    /// Regression guard for the target-signature component of `group_key`.
    #[test]
    fn stack_display_groups_distinguish_different_targets() {
        use crate::types::ability::{Effect, ResolvedAbility, TargetRef};
        use crate::types::identifiers::{CardId, ObjectId};

        let mut state = GameState::new_two_player(42);
        let sid = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Syphon Life".to_string(),
            Zone::Battlefield,
        );
        let mk_effect = || Effect::Unimplemented {
            name: "test".to_string(),
            description: None,
        };
        let mk_entry = |id: u64, target: TargetRef| StackEntry {
            id: ObjectId(id),
            source_id: sid,
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: sid,
                ability: Box::new(ResolvedAbility::new(
                    mk_effect(),
                    vec![target],
                    sid,
                    PlayerId(0),
                )),
                condition: None,
                trigger_event: None,
                description: Some("target player loses 1 life".to_string()),
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        };
        state
            .stack
            .push_back(mk_entry(10_001, TargetRef::Player(PlayerId(0))));
        state
            .stack
            .push_back(mk_entry(10_002, TargetRef::Player(PlayerId(1))));

        let groups = stack_display_groups(&state);
        assert_eq!(
            groups.len(),
            2,
            "triggers with divergent targets must not coalesce: got {:?}",
            groups
        );
    }

    #[test]
    fn stack_display_groups_distinguish_selected_spell_modes() {
        let mut state = GameState::new_two_player(42);
        let source = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Brotherhood's End".to_string(),
            Zone::Stack,
        );
        for (id, label) in [
            (ObjectId(10_003), "Deal 3 damage."),
            (ObjectId(10_004), "Destroy artifacts."),
        ] {
            let mut ability = ResolvedAbility::new(Effect::NoOp, Vec::new(), source, PlayerId(0));
            ability.selected_mode_labels = vec![label.to_string()];
            state.stack.push_back(StackEntry {
                id,
                source_id: source,
                controller: PlayerId(0),
                kind: StackEntryKind::Spell {
                    card_id: CardId(1),
                    ability: Some(Box::new(ability)),
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            });
        }

        assert_eq!(
            stack_display_groups(&state).len(),
            2,
            "spells with different selected mode labels must not coalesce",
        );
    }

    #[test]
    fn stack_display_groups_distinguish_pending_modal_spells_from_finalized_spells() {
        let mut state = GameState::new_two_player(42);
        let source = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Brotherhood's End".to_string(),
            Zone::Stack,
        );
        let target = crate::game::zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Sol Ring".to_string(),
            Zone::Battlefield,
        );
        let lower_id = ObjectId(10_005);
        let pending_id = ObjectId(10_006);
        let mut modal_ability = ResolvedAbility::new(
            Effect::NoOp,
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );
        modal_ability.selected_mode_labels = vec![
            "Brotherhood's End deals 3 damage to each creature and each planeswalker.".to_string(),
        ];
        let paid = StackPaidSnapshot {
            actual_mana_spent: 3,
            ..Default::default()
        };
        state.stack.push_back(StackEntry {
            id: lower_id,
            source_id: source,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: Some(Box::new(modal_ability.clone())),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 3,
            },
        });
        state.stack.push_back(StackEntry {
            id: pending_id,
            source_id: source,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 3,
            },
        });
        state.stack_paid_facts.insert(lower_id, paid.clone());
        state.stack_paid_facts.insert(pending_id, paid);
        state.waiting_for = WaitingFor::ModeChoice {
            player: PlayerId(0),
            modal: ModalChoice::default(),
            pending_cast: Box::new(PendingCast::new(
                pending_id,
                CardId(1),
                modal_ability.clone(),
                ManaCost::NoCost,
            )),
            unavailable_modes: Vec::new(),
        };

        let views = crate::game::derived_views::derive_views(&state, None);
        assert_eq!(
            views.stack_display_groups.len(),
            2,
            "an otherwise-identical pending modal spell must not coalesce with a finalized spell",
        );
        assert_eq!(views.stack_display_groups[1].representative, pending_id);
        assert!(
            views.stack_entry_details[&pending_id].is_pending,
            "the pending group representative must retain its casting state",
        );

        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.stack.back_mut().unwrap().kind = StackEntryKind::Spell {
            card_id: CardId(1),
            ability: Some(Box::new(modal_ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 3,
        };
        let finalized_views = crate::game::derived_views::derive_views(&state, None);
        assert_eq!(
            finalized_views.stack_display_groups.len(),
            1,
            "entries with the same finalized state must continue to coalesce",
        );
        assert_eq!(finalized_views.stack_display_groups[0].count, 2);
    }

    #[test]
    fn stack_display_groups_distinguish_chained_targets() {
        use crate::types::ability::{Effect, ResolvedAbility, TargetRef};
        use crate::types::identifiers::{CardId, ObjectId};

        let mut state = GameState::new_two_player(42);
        let sid = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Chained Trigger".to_string(),
            Zone::Battlefield,
        );
        let mk_effect = || Effect::Unimplemented {
            name: "test".to_string(),
            description: None,
        };
        let mk_entry = |id: u64, target: TargetRef| {
            let mut ability = ResolvedAbility::new(mk_effect(), Vec::new(), sid, PlayerId(0));
            ability.sub_ability = Some(Box::new(ResolvedAbility::new(
                mk_effect(),
                vec![target],
                sid,
                PlayerId(0),
            )));
            StackEntry {
                id: ObjectId(id),
                source_id: sid,
                controller: PlayerId(0),
                kind: StackEntryKind::TriggeredAbility {
                    source_id: sid,
                    ability: Box::new(ability),
                    condition: None,
                    trigger_event: None,
                    description: Some("then target player loses 1 life".to_string()),
                    source_name: String::new(),
                    subject_match_count: None,
                    die_result: None,
                    provenance: None,
                },
            }
        };
        state
            .stack
            .push_back(mk_entry(10_001, TargetRef::Player(PlayerId(0))));
        state
            .stack
            .push_back(mk_entry(10_002, TargetRef::Player(PlayerId(1))));

        let groups = stack_display_groups(&state);
        assert_eq!(
            groups.len(),
            2,
            "chained targets must participate in stack grouping; got {:?}",
            groups
        );
    }

    /// KeywordAction entries (Equip/Crew/etc.) carry their targets inside
    /// the enum variant, invisible to the target-aware `group_key`. To
    /// avoid an M1-style target-coalescing bug, `stack_display_groups`
    /// opts keyword-action entries out of coalescing entirely — each gets
    /// its own group regardless of source/target identity. Regression
    /// guard for that behavior.
    #[test]
    fn stack_display_groups_never_coalesce_keyword_actions() {
        use crate::types::ability::KeywordAction;
        use crate::types::identifiers::{CardId, ObjectId};

        let mut state = GameState::new_two_player(42);
        let equip = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bonesplitter".to_string(),
            Zone::Battlefield,
        );
        let creature_a = crate::game::zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Grizzly Bears A".to_string(),
            Zone::Battlefield,
        );
        let creature_b = crate::game::zones::create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Grizzly Bears B".to_string(),
            Zone::Battlefield,
        );
        let mk_entry = |id: u64, target: ObjectId| StackEntry {
            id: ObjectId(id),
            source_id: equip,
            controller: PlayerId(0),
            kind: StackEntryKind::KeywordAction {
                action: KeywordAction::Equip {
                    equipment_id: equip,
                    target_creature_id: target,
                },
            },
        };
        state.stack.push_back(mk_entry(10_001, creature_a));
        state.stack.push_back(mk_entry(10_002, creature_b));

        let groups = stack_display_groups(&state);
        assert_eq!(
            groups.len(),
            2,
            "two Equip activations on different targets must not coalesce; got {:?}",
            groups
        );
    }

    /// CR 702.27a: Build an instant spell on the stack with a draw effect and
    /// a `Keyword::Buyback` on the game object. `buyback_paid` controls
    /// `ability.context.additional_cost_paid`. Returns the spell's object id.
    fn push_buyback_spell(state: &mut GameState, buyback_paid: bool) -> ObjectId {
        use crate::types::keywords::{BuybackCost, Keyword};
        use crate::types::mana::ManaCost;
        let spell_id = create_object(
            state,
            CardId(300),
            PlayerId(0),
            "Whispers of the Muse".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.keywords
                .push(Keyword::Buyback(BuybackCost::Mana(ManaCost::Cost {
                    generic: 5,
                    shards: vec![],
                })));
        }

        let mut resolved = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            spell_id,
            PlayerId(0),
        );
        resolved.context.additional_cost_paid = buyback_paid;

        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(300),
                ability: Some(Box::new(resolved)),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        spell_id
    }

    /// CR 702.27a: When the buyback cost was paid, the spell returns to its
    /// owner's hand instead of the graveyard as it resolves.
    #[test]
    fn buyback_paid_routes_resolving_spell_to_hand() {
        let mut state = setup();
        let spell_id = push_buyback_spell(&mut state, true);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert!(
            state.players[0].hand.contains(&spell_id),
            "buyback-paid spell should return to owner's hand"
        );
        assert!(
            !state.players[0].graveyard.contains(&spell_id),
            "buyback-paid spell must not go to graveyard"
        );
    }

    /// CR 608.2n: Without the buyback cost paid, the non-permanent spell
    /// goes to its owner's graveyard normally.
    #[test]
    fn buyback_not_paid_routes_resolving_spell_to_graveyard() {
        let mut state = setup();
        let spell_id = push_buyback_spell(&mut state, false);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert!(
            state.players[0].graveyard.contains(&spell_id),
            "non-buyback spell should go to owner's graveyard"
        );
        assert!(
            !state.players[0].hand.contains(&spell_id),
            "non-buyback spell must not return to hand"
        );
    }

    /// Helper: build a permanent (creature) spell whose on-resolve ability
    /// self-exiles via `ChangeZone { target: SelfRef, destination: Exile }`.
    /// Pushes it onto the stack and returns its object id.
    ///
    /// This shape doesn't appear in the printed corpus today, but the post-#323
    /// architectural contract is: any spell whose own resolution moves it off
    /// the Stack must NOT also receive the post-resolution default zone move
    /// (Stack→Battlefield for permanents, Stack→Graveyard for non-permanents,
    /// Stack→Graveyard on prevented ETB). The Stack-residency guard
    /// (`spell_still_on_stack`) is the single authority for this gate.
    fn push_self_exiling_permanent_spell(state: &mut GameState) -> ObjectId {
        let spell_id = create_object(
            state,
            CardId(900),
            PlayerId(0),
            "Test Self-Exiling Creature".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
            obj.power = Some(1);
            obj.toughness = Some(1);
        }

        let resolved = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
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
            spell_id,
            PlayerId(0),
        );

        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(900),
                ability: Some(Box::new(resolved)),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        spell_id
    }

    fn push_self_exiling_aura_spell(state: &mut GameState, target_id: ObjectId) -> ObjectId {
        let spell_id = create_object(
            state,
            CardId(901),
            PlayerId(0),
            "Test Self-Exiling Aura".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
        }

        let resolved = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
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
            vec![TargetRef::Object(target_id)],
            spell_id,
            PlayerId(0),
        );

        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(901),
                ability: Some(Box::new(resolved)),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        spell_id
    }

    /// CR 608.3 + CR 608.2c (architectural cleanup, deferred from #323): a
    /// permanent spell whose `execute_effect` self-exiles must NOT be moved to
    /// the battlefield by the post-resolution Stack→Battlefield default. The
    /// Stack-residency guard (`spell_still_on_stack`) is the single authority
    /// — the same predicate already guards the non-permanent CR 608.2n
    /// graveyard default.
    ///
    /// Pre-fix the permanent-resolution branch in `resolve_top` would call
    /// `move_to_zone(state, object_id, to, events)` unconditionally, undoing
    /// the spell's own self-exile clause and corrupting the object's zone
    /// state by treating the exiled card as if it had entered the battlefield
    /// (ETB-tapped, ETB-counters, transform).
    #[test]
    fn permanent_spell_self_exile_skips_battlefield_default() {
        let mut state = setup();
        let spell_id = push_self_exiling_permanent_spell(&mut state);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert_eq!(
            state.objects[&spell_id].zone,
            Zone::Exile,
            "permanent spell with self-exile sub-ability must end in Exile, \
             not Battlefield (post-resolution default must be skipped when the \
             spell already left the Stack during execute_effect)"
        );
        assert!(
            !state.battlefield.contains(&spell_id),
            "self-exiled permanent must NOT be added to the battlefield zone index"
        );
        assert!(
            state.exile.contains(&spell_id),
            "self-exiled permanent must be tracked in the exile zone index"
        );
    }

    #[test]
    fn self_moved_aura_spell_does_not_receive_battlefield_attachment_side_effects() {
        let mut state = setup();
        let target_id = create_object(
            &mut state,
            CardId(902),
            PlayerId(0),
            "Target Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&target_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
        }
        let aura_id = push_self_exiling_aura_spell(&mut state, target_id);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert_eq!(state.objects[&aura_id].zone, Zone::Exile);
        assert!(
            state.objects[&aura_id].attached_to.is_none(),
            "Aura post-resolution attachment must only run after actual battlefield entry"
        );
        assert!(
            !state.objects[&target_id].attachments.contains(&aura_id),
            "target must not point at an Aura that self-exiled during resolution"
        );
    }

    /// CR 608.3e: a permanent spell whose ETB is fully prevented goes to its
    /// owner's graveyard only if it is still on the stack when that fallback is
    /// reached.
    #[test]
    fn prevented_etb_default_only_moves_spell_still_on_stack() {
        let mut state = setup();
        let spell_id = create_object(
            &mut state,
            CardId(903),
            PlayerId(0),
            "Test Prevented Creature".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let mut events = Vec::new();

        move_prevented_permanent_spell_to_graveyard_if_still_on_stack(
            &mut state,
            spell_id,
            &mut events,
        );
        assert_eq!(state.objects[&spell_id].zone, Zone::Graveyard);

        zones::move_to_zone(&mut state, spell_id, Zone::Exile, &mut events);
        move_prevented_permanent_spell_to_graveyard_if_still_on_stack(
            &mut state,
            spell_id,
            &mut events,
        );
        assert_eq!(state.objects[&spell_id].zone, Zone::Exile);
    }

    // ── Tier 3: batch-resolution tests ───────────────────────────────────
    //
    // These drive the REAL resolution pipeline (resolve_next / resolve_top +
    // run_post_action_pipeline) — they are runtime tests, not shape tests.

    mod batch_resolve {
        // Driver internals under test (the stack module).
        use super::super::{
            batch_run_len, effects, fixed_controller_gain_life_run_len,
            fixed_opponent_effect_run_len, observers_are_batch_safe,
            priority_checkpoint_is_settled, resolve_next, resolve_next_with_limit, resolve_top,
            self_counter_run_len,
        };
        // Test fixtures from the parent `tests` module.
        use super::setup;
        use crate::game::triggers;
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityCondition, AbilityDefinition, Comparator, Duration, Effect, FilterProp,
            PlayerFilter, PtValue, QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter,
            TargetRef, TriggerCondition, TriggerDefinition, TypeFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::counter::CounterType;
        use crate::types::events::GameEvent;
        use crate::types::game_state::{GameState, StackEntry, StackEntryKind};
        use crate::types::identifiers::{CardId, ObjectId};
        use crate::types::mana::ManaColor;
        use crate::types::player::PlayerId;
        use crate::types::proposed_event::TokenSpec;
        use crate::types::resolution::PendingProliferateActions;
        use crate::types::triggers::TriggerMode;
        use crate::types::zones::Zone;
        use std::sync::Arc;

        /// A bare Insect Token effect: 1/1 green Insect, Fixed count.
        fn insect_token_effect() -> Effect {
            Effect::Token {
                name: "Insect".to_string(),
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                types: vec!["Creature".to_string()],
                colors: vec![ManaColor::Green],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            }
        }

        /// Put `n` Forests on the battlefield under player 0.
        fn add_lands(state: &mut GameState, n: usize) {
            for _ in 0..n {
                let id = create_object(
                    state,
                    CardId(900),
                    PlayerId(0),
                    "Forest".to_string(),
                    Zone::Battlefield,
                );
                state
                    .objects
                    .get_mut(&id)
                    .unwrap()
                    .card_types
                    .core_types
                    .push(CoreType::Land);
            }
        }

        /// Create a Scute-Swarm-style source permanent (landfall trigger) on the
        /// battlefield and return its id. The landfall trigger registers under
        /// `EnterBattlefield(Some(Land))`, so (mirroring real Scute Swarm) it
        /// never matches the creature-token probe — the source's own trigger does
        /// not block batching, while a creature-keyed observer (CR 603.3) does.
        fn add_scute_source(state: &mut GameState) -> ObjectId {
            let id = create_object(
                state,
                CardId(901),
                PlayerId(0),
                "Scute Swarm".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&id).unwrap();
                obj.card_types.core_types.push(CoreType::Creature);
                let landfall = TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Land],
                        ..Default::default()
                    }));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(landfall.clone());
                obj.trigger_definitions.push(landfall);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(state);
            id
        }

        /// Create a plain creature permanent (no triggers/replacements) under
        /// player 0 with the given P/T and a single subtype, and return its id.
        /// Copy sources for the batch-copy path must be observer-free so the
        /// copy token inherits no ETB-keyed trigger (§2.3a). `name` doubles as
        /// the subtype so distinct names yield distinct copiable values.
        fn add_plain_creature_source(
            state: &mut GameState,
            name: &str,
            power: i32,
            toughness: i32,
        ) -> ObjectId {
            let id = create_object(
                state,
                CardId(910),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&id).unwrap();
                obj.base_power = Some(power);
                obj.base_toughness = Some(toughness);
                obj.power = Some(power);
                obj.toughness = Some(toughness);
                obj.base_card_types = crate::types::card_type::CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Creature],
                    subtypes: vec![name.to_string()],
                };
                obj.card_types = obj.base_card_types.clone();
                obj.base_name = name.to_string();
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(state);
            id
        }

        /// Create a plain planeswalker permanent with printed loyalty. A copy
        /// token of this source enters with loyalty counters (CR 306.5b), so it
        /// must not pass the copy-token ETB-pair batch gate.
        fn add_plain_planeswalker_source(
            state: &mut GameState,
            name: &str,
            loyalty: u32,
        ) -> ObjectId {
            let id = create_object(
                state,
                CardId(911),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&id).unwrap();
                obj.base_loyalty = Some(loyalty);
                obj.loyalty = Some(loyalty);
                obj.base_card_types = crate::types::card_type::CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Planeswalker],
                    subtypes: vec![name.to_string()],
                };
                obj.card_types = obj.base_card_types.clone();
                obj.base_name = name.to_string();
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(state);
            id
        }

        /// Create a real Scute-Swarm-shape copy source: a Creature carrying a
        /// landfall trigger ("Whenever a land enters under your control, ...")
        /// registered under `EnterBattlefield(Some(Land))` in BOTH the base and
        /// live trigger sets, so a CR 707.2 copy of it inherits the landfall
        /// trigger. `name` doubles as the subtype so distinct names yield
        /// distinct copiable values. Unlike `add_plain_creature_source`, the copy
        /// token is NOT observer-free — but its Land-keyed trigger does not
        /// observe its Creature siblings, so the refined §2.3a gate batches it.
        fn add_landfall_creature_source(
            state: &mut GameState,
            name: &str,
            power: i32,
            toughness: i32,
        ) -> ObjectId {
            let id = create_object(
                state,
                CardId(912),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&id).unwrap();
                obj.base_power = Some(power);
                obj.base_toughness = Some(toughness);
                obj.power = Some(power);
                obj.toughness = Some(toughness);
                obj.base_card_types = crate::types::card_type::CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Creature],
                    subtypes: vec![name.to_string()],
                };
                obj.card_types = obj.base_card_types.clone();
                obj.base_name = name.to_string();
                // A landfall trigger keyed EnterBattlefield(Some(Land)) — the
                // actual Scute Swarm shape. It must live in base_trigger_definitions
                // so a copy (CR 707.2) inherits it.
                let landfall = TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Land],
                        ..Default::default()
                    }))
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    ));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(landfall.clone());
                obj.trigger_definitions.push(landfall);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(state);
            id
        }

        /// Create a copy source whose copied token would OBSERVE its in-batch
        /// siblings: a Creature carrying a "whenever a creature you control
        /// enters" trigger registered under `EnterBattlefield(Some(Creature))`.
        /// A CR 707.2 copy inherits it, and the copy's Creature emission DOES
        /// intersect the Creature ETB key, so the refined §2.3a gate must refuse.
        fn add_creature_observer_source(
            state: &mut GameState,
            name: &str,
            power: i32,
            toughness: i32,
        ) -> ObjectId {
            let id = create_object(
                state,
                CardId(913),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&id).unwrap();
                obj.base_power = Some(power);
                obj.base_toughness = Some(toughness);
                obj.power = Some(power);
                obj.toughness = Some(toughness);
                obj.base_card_types = crate::types::card_type::CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Creature],
                    subtypes: vec![name.to_string()],
                };
                obj.card_types = obj.base_card_types.clone();
                obj.base_name = name.to_string();
                let creature_observer = TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        ..Default::default()
                    }))
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    ));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(creature_observer.clone());
                obj.trigger_definitions.push(creature_observer);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(state);
            id
        }

        /// Build a `ConditionInstead`-gated `CopyTokenOf { target: SelfRef }` sub
        /// whose inner condition is "you control >= `threshold` Lands" — disjoint
        /// from the produced Creature copy's core types, so it stays H1-invariant.
        fn copy_instead_sub(src: ObjectId, threshold: i32) -> ResolvedAbility {
            let copy_effect = Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            };
            let mut sub = ResolvedAbility::new(copy_effect, vec![], src, PlayerId(0));
            sub.condition = Some(AbilityCondition::ConditionInstead {
                inner: Box::new(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(TypedFilter {
                                type_filters: vec![TypeFilter::Land],
                                ..Default::default()
                            }),
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: threshold },
                }),
            });
            sub
        }

        /// Push `n` identical untargeted Token triggers from `source_id`.
        fn push_token_triggers(
            state: &mut GameState,
            source_id: ObjectId,
            effect: Effect,
            sub_ability: Option<Box<ResolvedAbility>>,
            n: usize,
        ) {
            for _ in 0..n {
                let entry_id = ObjectId(state.next_object_id);
                state.next_object_id += 1;
                let mut ability =
                    ResolvedAbility::new(effect.clone(), vec![], source_id, PlayerId(0));
                ability.sub_ability = sub_ability.clone();
                state.stack.push_back(StackEntry {
                    id: entry_id,
                    source_id,
                    controller: PlayerId(0),
                    kind: StackEntryKind::TriggeredAbility {
                        source_id,
                        ability: Box::new(ability),
                        condition: None,
                        trigger_event: None,
                        description: Some("Landfall".to_string()),
                        source_name: "Scute Swarm".to_string(),
                        subject_match_count: None,
                        die_result: None,
                        provenance: None,
                    },
                });
            }
        }

        /// Push `n` identical untargeted Token triggers, EACH from a DISTINCT
        /// source object (mirrors a Scute-Swarm board where many copies each
        /// fire their own landfall trigger). Returns the created source ids in
        /// push order. Each source is a plain creature carrying a landfall
        /// trigger keyed on `EnterBattlefield(Some(Land))` — it never observes
        /// the creature-token probe, exactly like the single-source helper's
        /// `add_scute_source`.
        fn push_token_triggers_from_distinct_sources(
            state: &mut GameState,
            effect: Effect,
            sub_ability: Option<Box<ResolvedAbility>>,
            n: usize,
        ) -> Vec<ObjectId> {
            let mut sources = Vec::with_capacity(n);
            for _ in 0..n {
                let src = add_scute_source(state);
                push_token_triggers(state, src, effect.clone(), sub_ability.clone(), 1);
                sources.push(src);
            }
            sources
        }

        fn add_self_counter_source(state: &mut GameState, name: &str) -> ObjectId {
            let source = create_object(
                state,
                CardId(9_000 + state.next_object_id),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
            obj.power = Some(1);
            obj.toughness = Some(1);
            source
        }

        fn self_counter_effect() -> Effect {
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            }
        }

        fn push_self_counter_trigger(
            state: &mut GameState,
            source: ObjectId,
            trigger_event: GameEvent,
        ) {
            let entry_id = ObjectId(state.next_object_id);
            state.next_object_id += 1;
            state.stack.push_back(StackEntry {
                id: entry_id,
                source_id: source,
                controller: PlayerId(0),
                kind: StackEntryKind::TriggeredAbility {
                    source_id: source,
                    ability: Box::new(ResolvedAbility::new(
                        self_counter_effect(),
                        vec![],
                        source,
                        PlayerId(0),
                    )),
                    condition: None,
                    trigger_event: Some(trigger_event),
                    description: Some("put a +1/+1 counter on this creature".to_string()),
                    source_name: state.objects[&source].name.clone(),
                    subject_match_count: None,
                    die_result: None,
                    provenance: None,
                },
            });
        }

        fn fixed_controller_gain_life_effect() -> Effect {
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            }
        }

        fn push_fixed_controller_gain_life_trigger(
            state: &mut GameState,
            source: ObjectId,
            trigger_event: GameEvent,
        ) {
            let entry_id = ObjectId(state.next_object_id);
            state.next_object_id += 1;
            let mut ability = ResolvedAbility::new(
                fixed_controller_gain_life_effect(),
                vec![],
                source,
                PlayerId(0),
            );
            ability.description = Some("you gain 1 life".to_string());
            ability.ability_index = Some(0);
            state.stack.push_back(StackEntry {
                id: entry_id,
                source_id: source,
                controller: PlayerId(0),
                kind: StackEntryKind::TriggeredAbility {
                    source_id: source,
                    ability: Box::new(ability),
                    condition: None,
                    trigger_event: Some(trigger_event),
                    description: Some("Whenever a creature enters, you gain 1 life.".to_string()),
                    source_name: state.objects[&source].name.clone(),
                    subject_match_count: None,
                    die_result: None,
                    provenance: None,
                },
            });
        }

        fn fixed_opponent_lose_life_effect() -> Effect {
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: None,
            }
        }

        fn push_fixed_opponent_lose_life_trigger(
            state: &mut GameState,
            source: ObjectId,
            trigger_event: GameEvent,
            condition: Option<TriggerCondition>,
            source_incarnation: Option<u64>,
        ) {
            let entry_id = ObjectId(state.next_object_id);
            state.next_object_id += 1;
            let mut ability = ResolvedAbility::new(
                fixed_opponent_lose_life_effect(),
                vec![],
                source,
                PlayerId(0),
            );
            ability.player_scope = Some(PlayerFilter::Opponent);
            ability.description = Some("each opponent loses 2 life".to_string());
            ability.ability_index = Some(0);
            ability.source_incarnation = source_incarnation;
            state.stack.push_back(StackEntry {
                id: entry_id,
                source_id: source,
                controller: PlayerId(0),
                kind: StackEntryKind::TriggeredAbility {
                    source_id: source,
                    ability: Box::new(ability),
                    condition,
                    trigger_event: Some(trigger_event),
                    description: Some(
                        "Whenever a creature dies, each opponent loses 2 life.".to_string(),
                    ),
                    source_name: state.objects[&source].name.clone(),
                    subject_match_count: None,
                    die_result: None,
                    provenance: None,
                },
            });
        }

        fn fixed_opponent_mill_effect() -> Effect {
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            }
        }

        fn push_fixed_opponent_mill_trigger(
            state: &mut GameState,
            source: ObjectId,
            trigger_event: GameEvent,
        ) {
            let entry_id = ObjectId(state.next_object_id);
            state.next_object_id += 1;
            let mut ability =
                ResolvedAbility::new(fixed_opponent_mill_effect(), vec![], source, PlayerId(0));
            ability.player_scope = Some(PlayerFilter::Opponent);
            ability.description = Some("each opponent mills a card".to_string());
            ability.ability_index = Some(0);
            state.stack.push_back(StackEntry {
                id: entry_id,
                source_id: source,
                controller: PlayerId(0),
                kind: StackEntryKind::TriggeredAbility {
                    source_id: source,
                    ability: Box::new(ability),
                    condition: None,
                    trigger_event: Some(trigger_event),
                    description: Some(
                        "Whenever another permanent enters, each opponent mills a card."
                            .to_string(),
                    ),
                    source_name: state.objects[&source].name.clone(),
                    subject_match_count: None,
                    die_result: None,
                    provenance: None,
                },
            });
        }

        fn life_event(player_id: PlayerId, amount: i32) -> GameEvent {
            GameEvent::LifeChanged { player_id, amount }
        }

        /// Drive resolution to empty via the BATCH path (`resolve_next`), running
        /// the real post-action pipeline after each step. Returns the per-step
        /// `consumed` counts.
        fn resolve_to_empty_batched(state: &mut GameState) -> Vec<u32> {
            let mut steps = Vec::new();
            let mut guard = 0;
            while !state.stack.is_empty() {
                let mut events = Vec::new();
                let consumed = resolve_next(state, &mut events);
                steps.push(consumed);
                triggers::process_triggers(state, &events);
                crate::game::sba::check_state_based_actions(state, &mut events);
                guard += 1;
                assert!(guard < 10_000, "resolution did not terminate");
            }
            steps
        }

        /// Drive resolution to empty via the SEQUENTIAL path (`resolve_top`),
        /// running the real post-action pipeline after each step.
        fn resolve_to_empty_sequential(state: &mut GameState) {
            let mut guard = 0;
            while !state.stack.is_empty() {
                let mut events = Vec::new();
                resolve_top(state, &mut events);
                triggers::process_triggers(state, &events);
                crate::game::sba::check_state_based_actions(state, &mut events);
                guard += 1;
                assert!(guard < 10_000, "resolution did not terminate");
            }
        }

        /// Test shim: gather the top `run_len` run source ids and invoke the
        /// real `effects::try_resolve_batch`. Mirrors the gather `resolve_next`
        /// performs at the live call site so tests exercise the true signature.
        fn try_batch(
            state: &GameState,
            ability: &ResolvedAbility,
            run_len: u32,
        ) -> Option<effects::BatchPlan> {
            let run_source_ids: Vec<ObjectId> = state
                .stack
                .iter()
                .rev()
                .take(run_len as usize)
                .map(|e| e.source_id)
                .collect();
            effects::try_resolve_batch(state, ability, run_len, &run_source_ids)
        }

        fn token_ids(state: &GameState) -> Vec<ObjectId> {
            state
                .battlefield
                .iter()
                .copied()
                .filter(|id| state.objects.get(id).is_some_and(|o| o.is_token))
                .collect()
        }

        // §9.2 — observer-free positive batch case (sub-6 lands base Insect).
        #[test]
        fn observer_free_batch_equals_sequential() {
            let mut base = setup();
            add_lands(&mut base, 3);
            let src = add_scute_source(&mut base);
            push_token_triggers(&mut base, src, insect_token_effect(), None, 10);

            let mut batched = base.clone();
            let mut sequential = base.clone();

            let steps = resolve_to_empty_batched(&mut batched);
            resolve_to_empty_sequential(&mut sequential);

            // The 10 entries collapsed into a single batched step.
            assert_eq!(steps, vec![10], "expected one 10-entry batch");
            // Exactly 10 Insect tokens, identical to the sequential path.
            assert_eq!(token_ids(&batched).len(), 10);
            assert_eq!(token_ids(&sequential).len(), 10);
            assert_eq!(batched.battlefield.len(), sequential.battlefield.len());
            assert!(batched.stack.is_empty() && sequential.stack.is_empty());
            for id in token_ids(&batched) {
                let o = &batched.objects[&id];
                assert_eq!(o.power, Some(1));
                assert_eq!(o.toughness, Some(1));
                assert!(o.card_types.core_types.contains(&CoreType::Creature));
            }
        }

        #[test]
        fn resolve_next_with_limit_caps_batch_consumption() {
            let mut state = setup();
            add_lands(&mut state, 3);
            let src = add_scute_source(&mut state);
            push_token_triggers(&mut state, src, insect_token_effect(), None, 10);

            let mut events = Vec::new();
            let consumed = resolve_next_with_limit(&mut state, &mut events, Some(4));

            assert_eq!(consumed, 4);
            assert_eq!(state.stack.len(), 6);
            assert_eq!(token_ids(&state).len(), 4);
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
                    .count(),
                4
            );
        }

        #[test]
        fn self_counter_triggers_batch_same_source_prefix_only() {
            crate::game::perf_counters::reset();
            let mut state = setup();
            let lower_source = add_self_counter_source(&mut state, "Other Dreyleader");
            let top_source = add_self_counter_source(&mut state, "Honored Dreyleader");

            push_self_counter_trigger(&mut state, lower_source, life_event(PlayerId(0), 1));
            push_self_counter_trigger(&mut state, top_source, life_event(PlayerId(1), 1));
            push_self_counter_trigger(&mut state, top_source, life_event(PlayerId(0), 2));

            assert_eq!(
                self_counter_run_len(&state),
                Some(2),
                "top contiguous same-source self-counter prefix should be batchable"
            );

            let mut events = Vec::new();
            let consumed = resolve_next(&mut state, &mut events);

            assert_eq!(consumed, 2);
            assert_eq!(state.stack.len(), 1);
            assert_eq!(
                state.objects[&top_source]
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied(),
                Some(2)
            );
            assert!(
                !state.objects[&lower_source]
                    .counters
                    .contains_key(&CounterType::Plus1Plus1),
                "different-source trigger below the prefix must remain unresolved"
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
                    .count(),
                2
            );
            assert_eq!(
                crate::game::perf_counters::snapshot().stack_batched_entries,
                2
            );
        }

        #[test]
        fn self_counter_batch_requires_an_empty_resolution_stack() {
            let mut state = setup();
            state.push_proliferate_frame(PendingProliferateActions {
                actor: PlayerId(0),
                source_id: ObjectId(9_501),
                remaining: 1,
            });

            assert!(
                !priority_checkpoint_is_settled(&state),
                "an active resolution frame makes a skipped priority checkpoint observable"
            );
        }

        /// CR 510.2 + CR 616.1: a parked combat-damage batch is latent
        /// continuation work — the drain still owes life gains, the per-player
        /// aggregate and Phase D's riders — so a batch consumer proving a
        /// sequence on a clone must not treat that state as a settled priority
        /// checkpoint.
        ///
        /// REVERT PROBE: delete the `pending_combat_lifelink.is_none()` conjunct
        /// from `priority_checkpoint_is_settled` — the first assertion fails.
        #[test]
        fn parked_combat_lifelink_is_not_a_settled_priority_checkpoint() {
            let mut state = setup();
            assert!(
                priority_checkpoint_is_settled(&state),
                "reach guard: the fixture is settled BEFORE the record is parked"
            );

            state.pending_combat_lifelink =
                Some(Box::new(crate::types::game_state::PendingCombatLifelink {
                    remaining: std::collections::VecDeque::from(vec![
                        crate::types::game_state::PendingLifelinkGain {
                            controller: PlayerId(0),
                            amount: 3,
                        },
                    ]),
                    batch_events: Vec::new(),
                    damage_to_players: Vec::new(),
                    prevention_tally: Vec::new(),
                    lives_before: vec![20, 20],
                    sub_step: crate::types::game_state::CombatDamageSubStep::Regular,
                }));
            assert!(
                !priority_checkpoint_is_settled(&state),
                "an unfinished combat-damage batch is latent continuation work"
            );

            state.pending_combat_lifelink = None;
            assert!(
                priority_checkpoint_is_settled(&state),
                "once the batch is drained the checkpoint settles again"
            );
        }

        #[test]
        fn self_counter_batch_refuses_when_checkpoint_annihilates_counters() {
            crate::game::perf_counters::reset();
            let mut state = setup();
            let source = add_self_counter_source(&mut state, "Honored Dreyleader");
            state
                .objects
                .get_mut(&source)
                .unwrap()
                .counters
                .insert(CounterType::Minus1Minus1, 1);

            push_self_counter_trigger(&mut state, source, life_event(PlayerId(0), 1));
            push_self_counter_trigger(&mut state, source, life_event(PlayerId(1), 1));

            let mut events = Vec::new();
            let consumed = resolve_next(&mut state, &mut events);

            assert_eq!(
                consumed, 1,
                "CR 704.5q checkpoint work must force single-entry fallback"
            );
            assert_eq!(
                crate::game::perf_counters::snapshot().stack_batched_entries,
                0
            );
        }

        #[test]
        fn self_counter_batch_refuses_when_counter_added_observer_fires() {
            crate::game::perf_counters::reset();
            let mut state = setup();
            let source = add_self_counter_source(&mut state, "Honored Dreyleader");
            let observer = create_object(
                &mut state,
                CardId(9_500),
                PlayerId(0),
                "Counter Watcher".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&observer).unwrap();
                obj.card_types.core_types.push(CoreType::Creature);
                let trig = TriggerDefinition::new(TriggerMode::CounterAdded)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        ..Default::default()
                    }))
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        Effect::NoOp,
                    ));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(trig.clone());
                obj.trigger_definitions.push(trig);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            push_self_counter_trigger(&mut state, source, life_event(PlayerId(0), 1));
            push_self_counter_trigger(&mut state, source, life_event(PlayerId(1), 1));

            let mut events = Vec::new();
            let consumed = resolve_next(&mut state, &mut events);

            assert_eq!(
                consumed, 1,
                "CounterAdded observer must make the clone checkpoint non-inert"
            );
            assert_eq!(
                crate::game::perf_counters::snapshot().stack_batched_entries,
                0
            );
        }

        #[test]
        fn fixed_controller_gain_life_triggers_batch_with_production_stamps() {
            crate::game::perf_counters::reset();
            let mut state = setup();
            let source_a = add_self_counter_source(&mut state, "Bogwater Lumaret A");
            let source_b = add_self_counter_source(&mut state, "Bogwater Lumaret B");
            let etb = life_event(PlayerId(0), 0);
            push_fixed_controller_gain_life_trigger(&mut state, source_a, etb.clone());
            push_fixed_controller_gain_life_trigger(&mut state, source_b, etb.clone());
            push_fixed_controller_gain_life_trigger(&mut state, source_b, etb);

            assert_eq!(
                fixed_controller_gain_life_run_len(&state),
                Some(3),
                "SourceIndependent fixed GainLife run should ignore distinct sources"
            );

            let life_before = state.players[0].life;
            let mut events = Vec::new();
            let consumed = resolve_next(&mut state, &mut events);

            assert_eq!(consumed, 3);
            assert_eq!(state.players[0].life, life_before + 3);
            assert!(state.stack.is_empty());
            assert_eq!(
                crate::game::perf_counters::snapshot().stack_batched_entries,
                3
            );
        }

        #[test]
        fn fixed_controller_gain_life_batch_refuses_when_life_gained_observer_fires() {
            crate::game::perf_counters::reset();
            let mut state = setup();
            let source = add_self_counter_source(&mut state, "Bogwater Lumaret");
            let observer = create_object(
                &mut state,
                CardId(9_600),
                PlayerId(0),
                "Life Watcher".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&observer).unwrap();
                obj.card_types.core_types.push(CoreType::Creature);
                let trig = TriggerDefinition::new(TriggerMode::LifeGained).execute(
                    AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    ),
                );
                Arc::make_mut(&mut obj.base_trigger_definitions).push(trig.clone());
                obj.trigger_definitions.push(trig);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            let etb = life_event(PlayerId(0), 0);
            push_fixed_controller_gain_life_trigger(&mut state, source, etb.clone());
            push_fixed_controller_gain_life_trigger(&mut state, source, etb);

            let mut events = Vec::new();
            let consumed = resolve_next(&mut state, &mut events);

            assert_eq!(
                consumed, 1,
                "CR 119.9 life-gained observers must force single-entry fallback"
            );
            assert_eq!(
                crate::game::perf_counters::snapshot().stack_batched_entries,
                0
            );
        }

        #[test]
        fn fixed_opponent_lose_life_triggers_batch_across_sources_with_equal_conditions() {
            crate::game::perf_counters::reset();
            let mut state = setup();
            let source_a = add_self_counter_source(&mut state, "Hearthhull A");
            let source_b = add_self_counter_source(&mut state, "Hearthhull B");
            let condition = TriggerCondition::LifeTotalGE { minimum: 1 };
            let trigger_event = life_event(PlayerId(0), 0);
            push_fixed_opponent_lose_life_trigger(
                &mut state,
                source_a,
                trigger_event.clone(),
                Some(condition.clone()),
                Some(1),
            );
            push_fixed_opponent_lose_life_trigger(
                &mut state,
                source_b,
                trigger_event.clone(),
                Some(condition.clone()),
                Some(2),
            );
            push_fixed_opponent_lose_life_trigger(
                &mut state,
                source_a,
                trigger_event,
                Some(condition),
                Some(3),
            );

            assert_eq!(
                fixed_opponent_effect_run_len(&state),
                Some(3),
                "fixed opponent life loss should ignore inert source provenance"
            );

            let life_before = state.players[1].life;
            let mut events = Vec::new();
            let consumed = resolve_next(&mut state, &mut events);

            assert_eq!(consumed, 3);
            assert_eq!(state.players[1].life, life_before - 6);
            assert!(state.stack.is_empty());
            assert_eq!(
                crate::game::perf_counters::snapshot().stack_batched_entries,
                3
            );
        }

        #[test]
        fn fixed_opponent_lose_life_batch_rechecks_intervening_if_at_resolution() {
            let mut state = setup();
            let source = add_self_counter_source(&mut state, "Hearthhull");
            let condition = TriggerCondition::LifeTotalGE { minimum: 20 };
            let trigger_event = life_event(PlayerId(0), 0);
            push_fixed_opponent_lose_life_trigger(
                &mut state,
                source,
                trigger_event.clone(),
                Some(condition.clone()),
                None,
            );
            push_fixed_opponent_lose_life_trigger(
                &mut state,
                source,
                trigger_event,
                Some(condition),
                None,
            );
            state.players[0].life = 19;

            let opponent_life_before = state.players[1].life;
            let mut events = Vec::new();
            let consumed = resolve_next(&mut state, &mut events);

            assert_eq!(consumed, 2);
            assert_eq!(state.players[1].life, opponent_life_before);
            assert!(state.stack.is_empty());
        }

        #[test]
        fn fixed_opponent_lose_life_batch_stops_at_different_intervening_if() {
            let mut state = setup();
            let source = add_self_counter_source(&mut state, "Hearthhull");
            let trigger_event = life_event(PlayerId(0), 0);
            let lower_condition = TriggerCondition::LifeTotalGE { minimum: 2 };
            let top_condition = TriggerCondition::LifeTotalGE { minimum: 1 };
            push_fixed_opponent_lose_life_trigger(
                &mut state,
                source,
                trigger_event.clone(),
                Some(lower_condition),
                None,
            );
            push_fixed_opponent_lose_life_trigger(
                &mut state,
                source,
                trigger_event.clone(),
                Some(top_condition.clone()),
                None,
            );
            push_fixed_opponent_lose_life_trigger(
                &mut state,
                source,
                trigger_event,
                Some(top_condition),
                None,
            );

            assert_eq!(
                fixed_opponent_effect_run_len(&state),
                Some(2),
                "a distinct intervening-if must end the contiguous batch"
            );

            let mut events = Vec::new();
            assert_eq!(resolve_next(&mut state, &mut events), 2);
            assert_eq!(state.stack.len(), 1);
        }

        #[test]
        fn fixed_opponent_lose_life_batch_refuses_when_life_lost_observer_fires() {
            crate::game::perf_counters::reset();
            let mut state = setup();
            let source = add_self_counter_source(&mut state, "Hearthhull");
            let observer = create_object(
                &mut state,
                CardId(9_601),
                PlayerId(0),
                "Loss Watcher".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&observer).unwrap();
                obj.card_types.core_types.push(CoreType::Creature);
                let trig =
                    TriggerDefinition::new(TriggerMode::LifeLost).execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        Effect::NoOp,
                    ));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(trig.clone());
                obj.trigger_definitions.push(trig);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            let trigger_event = life_event(PlayerId(0), 0);
            push_fixed_opponent_lose_life_trigger(
                &mut state,
                source,
                trigger_event.clone(),
                None,
                None,
            );
            push_fixed_opponent_lose_life_trigger(&mut state, source, trigger_event, None, None);

            let mut events = Vec::new();
            let consumed = resolve_next(&mut state, &mut events);

            assert_eq!(
                consumed, 1,
                "life-lost observers must force single-entry fallback"
            );
            assert_eq!(
                crate::game::perf_counters::snapshot().stack_batched_entries,
                0
            );
        }

        #[test]
        fn fixed_opponent_mill_triggers_batch() {
            crate::game::perf_counters::reset();
            let mut state = setup();
            let source = add_self_counter_source(&mut state, "Altar of the Brood");
            let milled_cards: Vec<_> = (0..3)
                .map(|index| {
                    create_object(
                        &mut state,
                        CardId(9_700 + index),
                        PlayerId(1),
                        format!("Library Card {index}"),
                        Zone::Library,
                    )
                })
                .collect();
            let trigger_event = life_event(PlayerId(0), 0);
            for _ in 0..3 {
                push_fixed_opponent_mill_trigger(&mut state, source, trigger_event.clone());
            }

            assert_eq!(
                fixed_opponent_effect_run_len(&state),
                Some(3),
                "identical Altar of the Brood triggers should form one inert run"
            );

            let mut events = Vec::new();
            let consumed = resolve_next(&mut state, &mut events);

            assert_eq!(consumed, 3);
            assert!(state.stack.is_empty());
            assert!(milled_cards
                .iter()
                .all(|id| state.objects[id].zone == Zone::Graveyard));
            assert_eq!(
                crate::game::perf_counters::snapshot().stack_batched_entries,
                3
            );
        }

        #[test]
        fn fixed_opponent_mill_batch_refuses_when_mill_observer_fires() {
            crate::game::perf_counters::reset();
            let mut state = setup();
            let source = add_self_counter_source(&mut state, "Altar of the Brood");
            let observer = create_object(
                &mut state,
                CardId(9_701),
                PlayerId(0),
                "Mill Watcher".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&observer).unwrap();
                obj.card_types.core_types.push(CoreType::Creature);
                let trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
                    .origin(Zone::Library)
                    .destination(Zone::Graveyard)
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        Effect::NoOp,
                    ));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(trigger.clone());
                obj.trigger_definitions.push(trigger);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);
            for index in 0..2 {
                create_object(
                    &mut state,
                    CardId(9_710 + index),
                    PlayerId(1),
                    format!("Library Card {index}"),
                    Zone::Library,
                );
            }
            let trigger_event = life_event(PlayerId(0), 0);
            push_fixed_opponent_mill_trigger(&mut state, source, trigger_event.clone());
            push_fixed_opponent_mill_trigger(&mut state, source, trigger_event);

            let mut events = Vec::new();
            let consumed = resolve_next(&mut state, &mut events);

            assert_eq!(
                consumed, 1,
                "a library-to-graveyard observer must preserve the per-entry priority checkpoint"
            );
            assert_eq!(
                crate::game::perf_counters::snapshot().stack_batched_entries,
                0
            );
        }

        // §9.2 — Layer C reports safe on an observer-free board.
        #[test]
        fn observers_are_batch_safe_true_without_observers() {
            let mut state = setup();
            add_lands(&mut state, 3);
            let src = add_scute_source(&mut state);
            push_token_triggers(&mut state, src, insect_token_effect(), None, 5);
            let run_len = batch_run_len(&state).unwrap();
            assert_eq!(run_len, 5);
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            let plan = try_batch(&state, &ability, run_len).unwrap();
            assert!(observers_are_batch_safe(&mut state, &plan));
        }

        // §9.4a — Cathars'-class creature-ETB observer forces refusal + the
        // sequential fall-back produces the DESCENDING per-token distribution.
        #[test]
        fn creature_etb_observer_forces_sequential_descending_counters() {
            let mut state = setup();
            add_lands(&mut state, 3);
            let src = add_scute_source(&mut state);

            // Cathars'-class observer: "Whenever a creature you control enters,
            // put a +1/+1 counter on each creature you control."
            let observer_id = create_object(
                &mut state,
                CardId(902),
                PlayerId(0),
                "Cathars' Crusade".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&observer_id).unwrap();
                obj.card_types.core_types.push(CoreType::Enchantment);
                let put_all = Effect::PutCounterAll {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        ..Default::default()
                    }),
                };
                let trig = TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        ..Default::default()
                    }))
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        put_all,
                    ));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(trig.clone());
                obj.trigger_definitions.push(trig);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            push_token_triggers(&mut state, src, insect_token_effect(), None, 5);

            // Layer C refuses.
            {
                let run_len = batch_run_len(&state).unwrap();
                let ability = state.stack.back().unwrap().ability().unwrap().clone();
                let plan = try_batch(&state, &ability, run_len).unwrap();
                assert!(
                    !observers_are_batch_safe(&mut state, &plan),
                    "creature-ETB observer must force refusal"
                );
            }

            // The batch driver must fall back to one entry at a time.
            let steps = resolve_to_empty_batched(&mut state);
            assert!(
                steps.iter().all(|&c| c == 1),
                "observer board must resolve one-at-a-time, got {steps:?}"
            );

            // CR 603.3: sequential interleaving — token1 (created first) is
            // present for the most subsequent Cathars resolutions, so its
            // +1/+1 counter total is the largest; the last token's is smallest.
            let mut totals: Vec<u32> = token_ids(&state)
                .iter()
                .map(|id| {
                    state.objects[id]
                        .counters
                        .get(&CounterType::Plus1Plus1)
                        .copied()
                        .unwrap_or(0)
                })
                .collect();
            assert_eq!(totals.len(), 5);
            // The distribution is a strict descending permutation 5,4,3,2,1.
            totals.sort_unstable();
            assert_eq!(
                totals,
                vec![1, 2, 3, 4, 5],
                "descending fan-out per CR 603.3"
            );
        }

        // §9.5 — entering +1/+1 counter + live CounterAdded observer: the §2.2a
        // gate refuses BEFORE Layer C is consulted (try_resolve_batch == None),
        // and the sequential fall-back produces the descending distribution.
        #[test]
        fn entering_counter_with_counteradded_observer_refuses_and_falls_back() {
            let mut state = setup();
            add_lands(&mut state, 3);
            let src = add_scute_source(&mut state);

            // CounterAdded observer: "Whenever one or more +1/+1 counters are put
            // on a creature you control, put a +1/+1 counter on each creature
            // you control." Registers under TriggerEventKey::CounterAdded ONLY —
            // NOT under any ETB/TokenCreated key.
            let observer_id = create_object(
                &mut state,
                CardId(903),
                PlayerId(0),
                "Counter Doubler".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&observer_id).unwrap();
                obj.card_types.core_types.push(CoreType::Enchantment);
                let put_all = Effect::PutCounterAll {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        ..Default::default()
                    }),
                };
                let trig = TriggerDefinition::new(TriggerMode::CounterAdded)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        ..Default::default()
                    }))
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        put_all,
                    ));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(trig.clone());
                obj.trigger_definitions.push(trig);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            // A Saproling token that enters WITH a +1/+1 counter.
            let mut saproling = insect_token_effect();
            if let Effect::Token {
                name,
                enter_with_counters,
                ..
            } = &mut saproling
            {
                *name = "Saproling".to_string();
                *enter_with_counters =
                    vec![(CounterType::Plus1Plus1, QuantityExpr::Fixed { value: 1 })];
            }
            push_token_triggers(&mut state, src, saproling, None, 5);

            // §2.2a: spec_emits_only_etb_pair == false ⇒ try_resolve_batch == None.
            {
                let run_len = batch_run_len(&state).unwrap();
                let ability = state.stack.back().unwrap().ability().unwrap().clone();
                assert!(
                    try_batch(&state, &ability, run_len).is_none(),
                    "entering-counter spec must fail the §2.2a gate before Layer C"
                );
            }

            // Driver falls back to one-at-a-time.
            let steps = resolve_to_empty_batched(&mut state);
            assert!(
                steps.iter().all(|&c| c == 1),
                "entering-counter board must resolve sequentially, got {steps:?}"
            );

            // Each token entered with 1 counter; the CounterAdded observer then
            // fans out per token. token1 observes the most subsequent counter
            // events ⇒ descending distribution per CR 603.3.
            let mut totals: Vec<u32> = token_ids(&state)
                .iter()
                .map(|id| {
                    state.objects[id]
                        .counters
                        .get(&CounterType::Plus1Plus1)
                        .copied()
                        .unwrap_or(0)
                })
                .collect();
            assert_eq!(totals.len(), 5);
            totals.sort_unstable();
            // Distinct strictly-descending per-token totals (not uniform).
            assert!(
                totals.windows(2).all(|w| w[0] < w[1]),
                "expected strict descending distribution, got {totals:?}"
            );
        }

        // §9.5 — Layer A/B predicate-shape refusals.
        #[test]
        fn non_fixed_count_is_not_batchable() {
            let mut state = setup();
            add_lands(&mut state, 3);
            let src = add_scute_source(&mut state);
            let mut effect = insect_token_effect();
            if let Effect::Token { count, .. } = &mut effect {
                *count = QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Land],
                            ..Default::default()
                        }),
                    },
                };
            }
            push_token_triggers(&mut state, src, effect, None, 5);
            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            assert!(try_batch(&state, &ability, run_len).is_none());
        }

        #[test]
        fn targeted_trigger_breaks_the_run() {
            let mut state = setup();
            let src = add_scute_source(&mut state);
            // A targeted Token (synthetic) is excluded by the empty-targets gate.
            for _ in 0..3 {
                let entry_id = ObjectId(state.next_object_id);
                state.next_object_id += 1;
                let ability = ResolvedAbility::new(
                    insect_token_effect(),
                    vec![TargetRef::Player(PlayerId(1))],
                    src,
                    PlayerId(0),
                );
                state.stack.push_back(StackEntry {
                    id: entry_id,
                    source_id: src,
                    controller: PlayerId(0),
                    kind: StackEntryKind::TriggeredAbility {
                        source_id: src,
                        ability: Box::new(ability),
                        condition: None,
                        trigger_event: None,
                        description: None,
                        source_name: String::new(),
                        subject_match_count: None,
                        die_result: None,
                        provenance: None,
                    },
                });
            }
            // Targeted entries are not batch candidates → no run key at top.
            assert!(batch_run_len(&state).is_none());
        }

        /// CR 111.2 + CR 109.4: distinct base-token sources now JOIN one run —
        /// the source dimension collapses under `SourceIndependent` because a
        /// base token reads nothing from its creating source. A source-relative
        /// effect (e.g. enters-attacking) keeps a per-source boundary.
        #[test]
        fn mixed_sources_form_a_contiguity_boundary() {
            // Base token: source-independent ⇒ distinct sources JOIN.
            let mut state = setup();
            add_lands(&mut state, 3);
            let src_a = add_scute_source(&mut state);
            let src_b = add_scute_source(&mut state);
            // Bottom: one trigger from src_b; top: 3 from src_a — both base Insect.
            push_token_triggers(&mut state, src_b, insect_token_effect(), None, 1);
            push_token_triggers(&mut state, src_a, insect_token_effect(), None, 3);
            // CR 111.2/109.4: all 4 distinct-source base-token entries form one run.
            assert_eq!(
                batch_run_len(&state),
                Some(4),
                "base tokens from distinct sources must collapse into one run"
            );

            // Source-relative token (enters_attacking) ⇒ Source(id) boundary.
            let mut attacking_effect = insect_token_effect();
            if let Effect::Token {
                ref mut enters_attacking,
                ..
            } = attacking_effect
            {
                *enters_attacking = true;
            }
            let mut state2 = setup();
            add_lands(&mut state2, 3);
            let src_c = add_scute_source(&mut state2);
            let src_d = add_scute_source(&mut state2);
            // Bottom: one from src_d; top: 3 from src_c — source-relative.
            push_token_triggers(&mut state2, src_d, attacking_effect.clone(), None, 1);
            push_token_triggers(&mut state2, src_c, attacking_effect, None, 3);
            // Source-relative effect keeps a per-source boundary: only the top
            // 3 src_c entries form the run.
            assert_eq!(
                batch_run_len(&state2),
                Some(3),
                "source-relative tokens must keep a per-source boundary"
            );
        }

        // §2.2a companion field exclusions.
        #[test]
        fn spec_emits_only_etb_pair_field_exclusions() {
            let base = TokenSpec {
                characteristics: crate::types::proposed_event::TokenCharacteristics {
                    display_name: "Insect".to_string(),
                    power: Some(1),
                    toughness: Some(1),
                    core_types: vec![CoreType::Creature],
                    subtypes: vec!["Insect".to_string()],
                    supertypes: vec![],
                    colors: vec![ManaColor::Green],
                    keywords: vec![],
                },
                script_name: "Insect".to_string(),
                static_abilities: vec![],
                enter_with_counters: vec![],
                tapped: false,
                enters_attacking: false,
                sacrifice_at: None,
                source_id: ObjectId(1),
                controller: PlayerId(0),
                attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
            };
            // Bare spec passes.
            assert!(super::super::effects::token::spec_emits_only_etb_pair(
                &base
            ));

            let mut with_counter = base.clone();
            with_counter.enter_with_counters = vec![(CounterType::Plus1Plus1, 1)];
            assert!(!super::super::effects::token::spec_emits_only_etb_pair(
                &with_counter
            ));

            let mut attacking = base.clone();
            attacking.enters_attacking = true;
            assert!(!super::super::effects::token::spec_emits_only_etb_pair(
                &attacking
            ));

            let mut sac = base.clone();
            sac.sacrifice_at = Some(Duration::UntilEndOfCombat);
            assert!(!super::super::effects::token::spec_emits_only_etb_pair(
                &sac
            ));

            let mut attached = base.clone();
            attached.attach_to = crate::types::proposed_event::TokenHostRequest::Bound(
                crate::game::game_object::AttachTarget::Object(ObjectId(2)),
            );
            assert!(!super::super::effects::token::spec_emits_only_etb_pair(
                &attached
            ));
        }

        // §2.2 + CR 707.2 — ConditionInstead MET copy branch: a single
        // (identical-value) source's met copy-instead swap now BATCHES along the
        // value-equal prefix (whole run), consuming `run_len` entries.
        #[test]
        fn condition_instead_met_copy_branch_refuses() {
            let mut state = setup();
            add_lands(&mut state, 6); // 6 lands → "if you control 6+ lands" is met.
                                      // Observer-free source so the copy token passes the §2.3a gate.
            let src = add_plain_creature_source(&mut state, "Scout", 1, 1);
            let sub = copy_instead_sub(src, 6);

            push_token_triggers(
                &mut state,
                src,
                insect_token_effect(),
                Some(Box::new(sub)),
                5,
            );
            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            // Condition met (6 lands) ⇒ swap to CopyTokenOf. The single source's
            // 5 entries share identical copiable values (CR 707.2), so the copy
            // prefix collapses the whole run into one batch.
            let plan = try_batch(&state, &ability, run_len)
                .expect("met copy-instead with identical values must batch");
            assert_eq!(
                plan.consumed(),
                run_len,
                "identical-source copy prefix must consume the full run"
            );
        }

        // §2.2 — ConditionInstead NOT met + disjoint type ⇒ base Insect batches.
        #[test]
        fn condition_instead_not_met_disjoint_type_batches() {
            let mut state = setup();
            add_lands(&mut state, 3); // < 6 ⇒ base branch.
            let src = add_scute_source(&mut state);

            let copy_effect = Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            };
            let mut sub = ResolvedAbility::new(copy_effect, vec![], src, PlayerId(0));
            sub.condition = Some(AbilityCondition::ConditionInstead {
                inner: Box::new(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(TypedFilter {
                                type_filters: vec![TypeFilter::Land],
                                ..Default::default()
                            }),
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 6 },
                }),
            });

            push_token_triggers(
                &mut state,
                src,
                insect_token_effect(),
                Some(Box::new(sub)),
                5,
            );
            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            // Land count invariant (token is a Creature, condition counts Lands) ⇒
            // base branch is provably stable ⇒ batchable.
            assert!(try_batch(&state, &ability, run_len).is_some());
        }

        // §3.4 — mandatory Doubling-Season-class replacement still batches and
        // produces 2× tokens per resolution.
        #[test]
        fn mandatory_token_doubling_batches_and_doubles() {
            let mut state = setup();
            add_lands(&mut state, 3);
            let src = add_scute_source(&mut state);

            // Doubling Season: mandatory token-count doubling replacement.
            let ds_id = create_object(
                &mut state,
                CardId(904),
                PlayerId(0),
                "Doubling Season".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&ds_id).unwrap();
                obj.card_types.core_types.push(CoreType::Enchantment);
                let repl = doubling_season_replacement();
                Arc::make_mut(&mut obj.base_replacement_definitions).push(repl.clone());
                obj.replacement_definitions.push(repl);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            push_token_triggers(&mut state, src, insect_token_effect(), None, 5);
            let steps = resolve_to_empty_batched(&mut state);
            assert_eq!(
                steps,
                vec![5],
                "mandatory replacement must not block batching"
            );
            // 5 resolutions × 2 (Doubling Season) = 10 Insect tokens.
            assert_eq!(token_ids(&state).len(), 10);
        }

        // §3.4 + CR 614.1a + CR 707.2 (issue #1511): a mandatory token-count
        // doubler applies to a `CopyTokenOf` swap collapsed into the copy-prefix
        // batch — each of the 5 self-copy resolutions creates one copy doubled
        // to two, for 10 copy tokens. Locks in that routing copy-token creation
        // through the `CreateToken` replacement pipeline doubles uniformly on
        // the batched copy path without double-counting.
        #[test]
        fn mandatory_token_doubling_batches_and_doubles_copy_prefix() {
            let mut state = setup();
            add_lands(&mut state, 6); // 6 lands ⇒ the copy-instead branch fires.
            let src = add_plain_creature_source(&mut state, "Scout", 1, 1);
            let sub = copy_instead_sub(src, 6);

            // Doubling Season: mandatory, controller-scoped token-count doubler.
            let ds_id = create_object(
                &mut state,
                CardId(905),
                PlayerId(0),
                "Doubling Season".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&ds_id).unwrap();
                obj.card_types.core_types.push(CoreType::Enchantment);
                let repl = doubling_season_replacement();
                Arc::make_mut(&mut obj.base_replacement_definitions).push(repl.clone());
                obj.replacement_definitions.push(repl);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            push_token_triggers(
                &mut state,
                src,
                insect_token_effect(),
                Some(Box::new(sub)),
                5,
            );
            resolve_to_empty_batched(&mut state);
            // 5 copy resolutions × 2 (Doubling Season) = 10 "Scout" copy tokens.
            let copies = state
                .objects
                .values()
                .filter(|o| o.is_token && o.name == "Scout")
                .count();
            assert_eq!(
                copies, 10,
                "doubler must apply to each batched copy-token resolution (issue #1511)"
            );
        }

        /// Push `n` identical untargeted Token triggers from `source_id`, each
        /// carrying the given entry-level intervening-if `condition` (CR 603.4).
        fn push_token_triggers_with_condition(
            state: &mut GameState,
            source_id: ObjectId,
            effect: Effect,
            condition: TriggerCondition,
            n: usize,
        ) {
            for _ in 0..n {
                let entry_id = ObjectId(state.next_object_id);
                state.next_object_id += 1;
                let ability = ResolvedAbility::new(effect.clone(), vec![], source_id, PlayerId(0));
                state.stack.push_back(StackEntry {
                    id: entry_id,
                    source_id,
                    controller: PlayerId(0),
                    kind: StackEntryKind::TriggeredAbility {
                        source_id,
                        ability: Box::new(ability),
                        condition: Some(condition.clone()),
                        trigger_event: None,
                        description: Some("Landfall".to_string()),
                        source_name: "Scute Swarm".to_string(),
                        subject_match_count: None,
                        die_result: None,
                        provenance: None,
                    },
                });
            }
        }

        // §9.5 HIGH (A4) — entry-level intervening-if that the run's OWN tokens
        // mutate (CR 603.4) MUST NOT batch. The condition reads the live creature
        // count; each created Insect raises it, so resolving the run one-by-one
        // stops firing once the threshold is crossed — producing FEWER tokens
        // than the run length. A batch that dropped the condition would fire all
        // N. This is the discriminating test for the dropped-`condition` defect.
        #[test]
        fn entry_intervening_if_over_run_mutated_count_does_not_batch() {
            let mut state = setup();
            // add_scute_source contributes ONE creature (the source). No lands
            // needed — the trigger entries are pushed directly.
            let src = add_scute_source(&mut state);

            // Intervening-if: "if you control fewer than 3 creatures, create a
            // 1/1 Insect". Each Insect raises the creature count the condition
            // reads — order-sensitive across the run.
            let condition = TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Creature],
                            ..Default::default()
                        }),
                    },
                },
                comparator: Comparator::LT,
                rhs: QuantityExpr::Fixed { value: 3 },
            };

            push_token_triggers_with_condition(
                &mut state,
                src,
                insect_token_effect(),
                condition,
                5,
            );

            // Layer A REFUSES to form a run: an entry carrying an entry-level
            // `condition` is non-batchable (it would become a singleton run that
            // falls back to `resolve_top`, which rechecks per entry per CR 603.4).
            assert!(
                batch_run_len(&state).is_none(),
                "an intervening-if entry must not start a batch run"
            );

            // Driver falls back to one-at-a-time for every entry.
            let steps = resolve_to_empty_batched(&mut state);
            assert!(
                steps.iter().all(|&c| c == 1),
                "intervening-if run must resolve one-at-a-time, got {steps:?}"
            );

            // Sequential semantics: baseline 1 creature (the source). Entry 1
            // sees 1<3 → +token (2). Entry 2 sees 2<3 → +token (3). Entries 3-5
            // see 3, not <3 → skip. Exactly 2 tokens — FEWER than the run of 5.
            // A reverted fix (condition dropped, all 5 batched) would give 5.
            assert_eq!(
                token_ids(&state).len(),
                2,
                "intervening-if must stop firing once the count crosses the threshold"
            );
        }

        // §9.5 HIGH-2 — produced-token-non-observer gate (direct, discriminating):
        // the gate is the INTERSECTION of a trigger's registered keys with the
        // produced token's CR 603.6a emission. A Creature produced token emits
        // exactly {EnterBattlefield(None), EnterBattlefield(Some(Creature)),
        // TokenCreated}. A creature-ETB observer intersects (refused); the real
        // Scute-shape landfall trigger (EnterBattlefield(Some(Land))) does NOT
        // intersect a creature emission and is batch-SAFE (the HIGH fix — the old
        // coarse wildcard gate refused this and the headline repro never batched).
        #[test]
        fn produced_token_non_observer_gate_discriminates() {
            use super::super::effects::token::produced_token_is_non_observer;
            // The produced (copied) token is a Creature: emission =
            // {None, Some(Creature), TokenCreated}.
            let produced_creature = [CoreType::Creature];

            // A creature-ETB observer trigger registers under Some(Creature) ⇒
            // intersects the creature emission ⇒ must fail the gate.
            let etb_observer = TriggerDefinition::new(TriggerMode::ChangesZone)
                .destination(Zone::Battlefield)
                .valid_card(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    ..Default::default()
                }));
            assert!(
                !produced_token_is_non_observer(
                    std::slice::from_ref(&etb_observer),
                    &produced_creature
                ),
                "a creature-ETB-observing produced token must fail the gate"
            );

            // The HEADLINE fix: a landfall trigger registers under
            // EnterBattlefield(Some(Land)). A Creature copy emits no Land key, so
            // the intersection is EMPTY ⇒ the Scute-shape copy is batch-SAFE. The
            // old coarse gate (any EnterBattlefield(_)) refused this and the named
            // repro never collapsed.
            let landfall = TriggerDefinition::new(TriggerMode::ChangesZone)
                .destination(Zone::Battlefield)
                .valid_card(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Land],
                    ..Default::default()
                }));
            assert!(
                produced_token_is_non_observer(std::slice::from_ref(&landfall), &produced_creature),
                "a Land-keyed landfall trigger on a Creature copy does not observe \
                 its creature siblings ⇒ batch-safe (the HIGH fix)"
            );

            // Over-permit guard: a broad permanent-ETB observer registers under
            // the broad EnterBattlefield(None) key, which is in EVERY token's
            // emission ⇒ must still be refused.
            let broad_etb = TriggerDefinition::new(TriggerMode::ChangesZone)
                .destination(Zone::Battlefield)
                .valid_card(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Permanent],
                    ..Default::default()
                }));
            assert!(
                !produced_token_is_non_observer(
                    std::slice::from_ref(&broad_etb),
                    &produced_creature
                ),
                "a broad permanent-ETB observer (None key) intersects every emission ⇒ refused"
            );

            // Symmetry check: the SAME landfall trigger on a LAND copy (emission
            // includes Some(Land)) DOES intersect ⇒ refused. Proves the gate keys
            // off the produced token's real core types, not a fixed assumption.
            assert!(
                !produced_token_is_non_observer(std::slice::from_ref(&landfall), &[CoreType::Land]),
                "a landfall trigger on a Land copy observes its land siblings ⇒ refused"
            );

            // No triggers ⇒ passes (the bare Insect/Servo go-wide case).
            assert!(
                produced_token_is_non_observer(&[], &produced_creature),
                "a trigger-free produced token passes the gate"
            );
        }

        // §9.5 HIGH-2 — produced-token-non-observer gate: a CopyTokenOf run whose
        // copy SOURCE carries an ETB observer trigger must refuse. (The copy
        // branch falls back wholesale in v1 — see B5 — so this confirms a
        // copy-source observer never reaches a batched resolution.)
        #[test]
        fn copy_source_with_etb_observer_refuses_to_batch() {
            let mut state = setup();
            add_lands(&mut state, 3); // < 6 ⇒ base/copy decision routes to copy below.

            // A copy SOURCE permanent that itself carries a creature-ETB observer
            // trigger ("whenever a creature you control enters, ..."). Copies of
            // it would inherit this trigger and observe their siblings.
            let copy_source = create_object(
                &mut state,
                CardId(905),
                PlayerId(0),
                "Observer Source".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&copy_source).unwrap();
                obj.card_types.core_types.push(CoreType::Creature);
                let etb_observer = TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        ..Default::default()
                    }))
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    ));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(etb_observer.clone());
                obj.trigger_definitions.push(etb_observer);
            }

            let src = add_scute_source(&mut state);

            // sub: CopyTokenOf gated by a MET ConditionInstead (lands >= 1) so the
            // copy branch is selected, then assert refusal (copy path falls back
            // wholesale in v1 — so a copy-source observer never batches).
            let copy_effect = Effect::CopyTokenOf {
                target: TargetFilter::SpecificObject { id: copy_source },
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            };
            let mut sub = ResolvedAbility::new(copy_effect, vec![], src, PlayerId(0));
            sub.condition = Some(AbilityCondition::ConditionInstead {
                inner: Box::new(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(TypedFilter {
                                type_filters: vec![TypeFilter::Land],
                                ..Default::default()
                            }),
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
                }),
            });

            push_token_triggers(
                &mut state,
                src,
                insect_token_effect(),
                Some(Box::new(sub)),
                5,
            );
            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            // The instead-swap fires (>= 1 land) ⇒ copy branch ⇒ not batchable in
            // v1 (the copy path produces no TokenSpec and falls back). The gate
            // therefore refuses regardless — confirming a copy-source observer
            // never reaches a batched resolution.
            assert!(
                try_batch(&state, &ability, run_len).is_none(),
                "copy branch (and any copy-source observer) must refuse to batch"
            );
        }

        // §9.5 MEDIUM-1 — interactive/optional replacement gate: an OPTIONAL
        // token-doubling replacement applicable to the produced token must refuse
        // (token_creation_needs_choice == true). The mandatory positive control is
        // covered by `mandatory_token_doubling_batches_and_doubles`.
        #[test]
        fn optional_replacement_refuses_to_batch() {
            let mut state = setup();
            add_lands(&mut state, 3);
            let src = add_scute_source(&mut state);

            // An OPTIONAL ("you may") token-count-doubling replacement.
            let opt_id = create_object(
                &mut state,
                CardId(906),
                PlayerId(0),
                "Optional Doubler".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&opt_id).unwrap();
                obj.card_types.core_types.push(CoreType::Enchantment);
                let repl = optional_token_doubling_replacement();
                Arc::make_mut(&mut obj.base_replacement_definitions).push(repl.clone());
                obj.replacement_definitions.push(repl);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            push_token_triggers(&mut state, src, insect_token_effect(), None, 5);
            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            // The optional replacement could pause for a NeedsChoice prompt
            // mid-batch ⇒ Layer B refuses.
            assert!(
                try_batch(&state, &ability, run_len).is_none(),
                "optional replacement must force fall-back"
            );
        }

        // CR 603.3 (HIGH-2 loophole regression) — the run's OWN source carries a
        // SECOND observer trigger keyed on the produced token's creature-ETB
        // (e.g. "Whenever a land enters, create a 1/1 creature token. Whenever a
        // creature enters, draw a card."). Under CR 603.3 each token-creation and
        // each observer firing goes on the stack one at a time, with priority in
        // between, so batching ("all tokens, then all observers") would skip the
        // priority interleaving and let a player act between resolutions. Layer C
        // MUST refuse. This test would have FALSELY PASSED (batch wrongly allowed)
        // when `observers_are_batch_safe` excluded the run's own source IDs: the
        // creature-ETB candidate == the run source, so the old `run_source_ids`
        // exclusion filtered it out and reported the run batch-safe. With the
        // exclusion removed, any registered observer — including the source's own
        // second trigger — forces sequential resolution.
        #[test]
        fn source_with_own_token_etb_observer_forces_refusal() {
            let mut state = setup();
            add_lands(&mut state, 3);
            // `add_scute_source` registers the LAND-ETB token-creating trigger
            // (the run). It never self-matches the creature-token probe.
            let src = add_scute_source(&mut state);

            // Attach a SECOND trigger to the SAME source, keyed on creature-ETB —
            // exactly the produced token's type. This is the loophole gemini
            // flagged: the run source observing its own produced tokens.
            {
                let obj = state.objects.get_mut(&src).unwrap();
                let creature_observer = TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        ..Default::default()
                    }))
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    ));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(creature_observer.clone());
                obj.trigger_definitions.push(creature_observer);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            push_token_triggers(&mut state, src, insect_token_effect(), None, 5);

            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            let plan = try_batch(&state, &ability, run_len).unwrap();
            // The creature-ETB candidate IS the run source `src`. Pre-fix, the
            // exclusion dropped it and this assertion would FAIL (batch allowed);
            // post-fix it must hold (refuse to batch).
            assert!(
                !observers_are_batch_safe(&mut state, &plan),
                "run source's own token-ETB observer must force sequential resolution (CR 603.3)"
            );

            // End-to-end: the batch driver must fall back to one entry at a time.
            let steps = resolve_to_empty_batched(&mut state);
            assert!(
                steps.iter().all(|&c| c == 1),
                "source-observed run must resolve one-at-a-time, got {steps:?}"
            );
        }

        // §9.4a HIGH-1 regression — a live non-run battlefield observer keyed on a
        // NARROW non-Creature ETB subtype (artifact creature) that the produced
        // token matches must force Layer C to refuse. A round-2-style fixed
        // `Some(Creature)` probe would have MISSED the `Some(Artifact)` bucket.
        #[test]
        fn narrow_artifact_etb_observer_forces_refusal() {
            let mut state = setup();
            add_lands(&mut state, 3);
            let src = add_scute_source(&mut state);

            // Observer narrowed to ARTIFACT ETB: registers ONLY under
            // EnterBattlefield(Some(Artifact)) — NOT under (Some(Creature)).
            let observer_id = create_object(
                &mut state,
                CardId(907),
                PlayerId(0),
                "Artifact Watcher".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&observer_id).unwrap();
                obj.card_types.core_types.push(CoreType::Enchantment);
                let trig = TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Artifact],
                        ..Default::default()
                    }))
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    ));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(trig.clone());
                obj.trigger_definitions.push(trig);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            // The produced token is an ARTIFACT CREATURE (core_types = [Artifact,
            // Creature]) — so the Layer C probe builds a record whose core_types
            // include Artifact and hits the narrow observer's bucket.
            let mut servo = insect_token_effect();
            if let Effect::Token { name, types, .. } = &mut servo {
                *name = "Servo".to_string();
                *types = vec!["Artifact".to_string(), "Creature".to_string()];
            }
            push_token_triggers(&mut state, src, servo, None, 5);

            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            let plan = try_batch(&state, &ability, run_len).unwrap();
            assert!(
                !observers_are_batch_safe(&mut state, &plan),
                "narrow artifact-ETB observer must force refusal (Some(Artifact) bucket)"
            );
        }

        // §9.4a — a meaningful broad-ETB observer (valid_card = Permanent) keyed
        // under EnterBattlefield(None) must still force Layer C to refuse.
        #[test]
        fn kodama_broad_permanent_etb_observer_forces_refusal() {
            let mut state = setup();
            add_lands(&mut state, 3);
            let src = add_scute_source(&mut state);

            // Broad permanent-ETB observer ("whenever another permanent you
            // control enters, ..."): valid_card = Permanent narrows to None ⇒
            // registers under the broad EnterBattlefield(None) key.
            let observer_id = create_object(
                &mut state,
                CardId(908),
                PlayerId(0),
                "Kodama of the East Tree".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&observer_id).unwrap();
                obj.card_types.core_types.push(CoreType::Creature);
                let trig = TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Permanent],
                        ..Default::default()
                    }))
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    ));
                Arc::make_mut(&mut obj.base_trigger_definitions).push(trig.clone());
                obj.trigger_definitions.push(trig);
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            push_token_triggers(&mut state, src, insect_token_effect(), None, 5);

            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            let plan = try_batch(&state, &ability, run_len).unwrap();
            assert!(
                !observers_are_batch_safe(&mut state, &plan),
                "meaningful broad permanent-ETB observer must force Layer C refusal"
            );
        }

        // CR 113.6 + CR 603.3 — the third production consumer of the
        // `candidates_for_event` seam. `observers_are_batch_safe` is the
        // batch-safety gate, so the live-zone guard changes a BATCHING decision
        // here, not only trigger firing: a stale off-battlefield observer used
        // to make `candidates` non-empty and force the conservative sequential
        // path. Dropping it cannot turn a safe batch unsafe, because an
        // observer that cannot legally trigger under CR 113.6 cannot make a
        // batch order-sensitive.
        #[test]
        fn stale_off_battlefield_observer_does_not_force_batch_refusal() {
            // Same broad permanent-ETB observer shape as
            // `kodama_broad_permanent_etb_observer_forces_refusal`.
            let build = || -> (GameState, ObjectId, effects::BatchPlan) {
                let mut state = setup();
                add_lands(&mut state, 3);
                let src = add_scute_source(&mut state);

                let observer_id = create_object(
                    &mut state,
                    CardId(908),
                    PlayerId(0),
                    "Kodama of the East Tree".to_string(),
                    Zone::Battlefield,
                );
                {
                    let obj = state.objects.get_mut(&observer_id).unwrap();
                    obj.card_types.core_types.push(CoreType::Creature);
                    let trig = TriggerDefinition::new(TriggerMode::ChangesZone)
                        .destination(Zone::Battlefield)
                        .valid_card(TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Permanent],
                            ..Default::default()
                        }))
                        .execute(AbilityDefinition::new(
                            crate::types::ability::AbilityKind::Database,
                            Effect::Draw {
                                count: QuantityExpr::Fixed { value: 1 },
                                target: TargetFilter::Controller,
                            },
                        ));
                    Arc::make_mut(&mut obj.base_trigger_definitions).push(trig.clone());
                    obj.trigger_definitions.push(trig);
                }
                // Register while the observer is legitimately on the
                // battlefield. No rebuild can intervene later:
                // `observers_are_batch_safe` consults `candidates_for_event`
                // directly and never calls `ensure_ready`.
                crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

                push_token_triggers(&mut state, src, insect_token_effect(), None, 5);

                let run_len = batch_run_len(&state).unwrap();
                let ability = state.stack.back().unwrap().ability().unwrap().clone();
                let plan = try_batch(&state, &ability, run_len).unwrap();
                (state, observer_id, plan)
            };

            // 1. Positive reach-guard: this observer really is a shape the gate
            //    reacts to. Without it the negative below could be satisfied
            //    vacuously by an observer that never registered under any key.
            {
                let (mut state, _observer_id, plan) = build();
                assert!(
                    !observers_are_batch_safe(&mut state, &plan),
                    "reach-guard: an on-battlefield broad permanent-ETB observer \
                     must force refusal"
                );
            }

            // 2. The delta. Induce the desync AFTER the rebuild, leaving
            //    `state.battlefield` and the index stale.
            let stale = {
                let (mut state, observer_id, plan) = build();
                state.objects.get_mut(&observer_id).unwrap().zone = Zone::Hand;
                let mut probe = state.clone();
                assert!(
                    observers_are_batch_safe(&mut probe, &plan),
                    "CR 113.6: a stale off-battlefield observer must not force \
                     batch refusal"
                );
                state
            };

            // 3. Outcome identity: the batch the guard newly permits resolves
            //    exactly as the sequential path would. This is what pins "the
            //    batching delta is observationally inert" instead of asserting
            //    it. Deliberately NOT asserting the step shape — that would flip
            //    red without the guard and silently promote this into a
            //    falsification vehicle, which it is not.
            let mut batched = stale.clone();
            let mut sequential = stale;
            resolve_to_empty_batched(&mut batched);
            resolve_to_empty_sequential(&mut sequential);
            assert_eq!(
                token_ids(&batched).len(),
                token_ids(&sequential).len(),
                "batched token count must equal sequential"
            );
            assert_eq!(
                batched.battlefield.len(),
                sequential.battlefield.len(),
                "batched battlefield must equal sequential"
            );
        }

        // §9.4b / §9.2 — ConditionInstead DIFFERENTIAL harness: run BOTH the
        // not-met (batches) and met (falls back) cases through the real pipeline
        // and assert each produces the correct final state vs the sequential path.
        #[test]
        fn condition_instead_differential_not_met_and_met() {
            // Build a Scute-Swarm-style state with `lands` lands and 5 landfall
            // Token-or-copy triggers; return it ready to resolve.
            let build = |lands: usize| -> GameState {
                let mut state = setup();
                add_lands(&mut state, lands);
                // Observer-free copy source so the met-copy branch can batch
                // (a copy inherits the source's triggers; an ETB-keyed trigger
                // would fail the §2.3a non-observer gate).
                let src = add_plain_creature_source(&mut state, "Scout", 1, 1);
                let sub = copy_instead_sub(src, 6);
                push_token_triggers(
                    &mut state,
                    src,
                    insect_token_effect(),
                    Some(Box::new(sub)),
                    5,
                );
                state
            };

            // NOT met (3 lands): base Insect branch ⇒ batches; final state equals
            // sequential. Disjoint type (token is Creature, condition counts Lands)
            // proves invariance.
            {
                let base = build(3);
                let mut batched = base.clone();
                let mut sequential = base.clone();
                let steps = resolve_to_empty_batched(&mut batched);
                resolve_to_empty_sequential(&mut sequential);
                assert_eq!(
                    steps,
                    vec![5],
                    "not-met disjoint case must batch in one step"
                );
                assert_eq!(
                    token_ids(&batched).len(),
                    token_ids(&sequential).len(),
                    "batched token count must equal sequential"
                );
                assert_eq!(token_ids(&batched).len(), 5);
                assert_eq!(batched.battlefield.len(), sequential.battlefield.len());
            }

            // MET (6 lands): copy-instead fires ⇒ Layer B copy-prefix batches.
            // The single source's 5 entries share identical copiable values
            // (CR 707.2), and the observer-free copy token passes §2.3a, so the
            // whole run collapses into ONE batched step producing 5 copies —
            // equal to the sequential path.
            {
                let base = build(6);
                let mut batched = base.clone();
                let mut sequential = base.clone();
                let steps = resolve_to_empty_batched(&mut batched);
                resolve_to_empty_sequential(&mut sequential);
                assert_eq!(
                    steps,
                    vec![5],
                    "met copy-instead with identical values must batch in one step, got {steps:?}"
                );
                assert_eq!(
                    token_ids(&batched).len(),
                    5,
                    "5 copy-token resolutions produce 5 tokens"
                );
                assert_eq!(
                    token_ids(&batched).len(),
                    token_ids(&sequential).len(),
                    "batched copy count must equal sequential"
                );
            }
        }

        // CR 111.2 + CR 109.4 — cross-source base-token collapse: K distinct
        // sources each fire one base Insect Token trigger. Because a base token
        // reads nothing from its source, the run-identity source axis is
        // `SourceIndependent` and all K entries form ONE batch (the Scute Swarm
        // O(N²)→O(N) fix). Result equals the sequential path.
        #[test]
        fn cross_source_base_token_forms_one_batch() {
            let mut base = setup();
            add_lands(&mut base, 3);
            let sources = push_token_triggers_from_distinct_sources(
                &mut base,
                insect_token_effect(),
                None,
                7,
            );
            assert_eq!(sources.len(), 7);

            let mut batched = base.clone();
            let mut sequential = base.clone();

            let steps = resolve_to_empty_batched(&mut batched);
            resolve_to_empty_sequential(&mut sequential);

            assert_eq!(
                steps,
                vec![7],
                "7 distinct-source base-token entries must collapse into one batch"
            );
            assert_eq!(token_ids(&batched).len(), 7);
            assert_eq!(token_ids(&sequential).len(), 7);
            assert_eq!(batched.battlefield.len(), sequential.battlefield.len());
        }

        // CR 707.2 — cross-source copy collapse: K distinct sources with
        // IDENTICAL copiable values each fire a met copy-instead self-copy. The
        // value-equal prefix spans the whole run, so all K collapse into one
        // batch producing K copies. Result equals the sequential path.
        #[test]
        fn cross_source_copy_identical_values_forms_one_batch() {
            let mut base = setup();
            add_lands(&mut base, 6); // met ⇒ copy branch fires.

            // K distinct, value-identical observer-free creature sources, each
            // firing a met copy-instead self-copy.
            for _ in 0..5 {
                let src = add_plain_creature_source(&mut base, "Clone Base", 2, 2);
                let sub = copy_instead_sub(src, 6);
                push_token_triggers(
                    &mut base,
                    src,
                    insect_token_effect(),
                    Some(Box::new(sub)),
                    1,
                );
            }

            let mut batched = base.clone();
            let mut sequential = base.clone();

            let steps = resolve_to_empty_batched(&mut batched);
            resolve_to_empty_sequential(&mut sequential);

            assert_eq!(
                steps,
                vec![5],
                "5 identical-value cross-source copies must collapse into one batch, got {steps:?}"
            );
            // 5 copy tokens, all copies of "Clone Base".
            let batched_copies: Vec<_> = token_ids(&batched)
                .into_iter()
                .filter(|id| batched.objects[id].name == "Clone Base")
                .collect();
            assert_eq!(batched_copies.len(), 5);
            assert_eq!(
                token_ids(&batched).len(),
                token_ids(&sequential).len(),
                "batched copy count must equal sequential"
            );
        }

        #[test]
        fn copy_token_with_intrinsic_counters_refuses_batch() {
            let mut state = setup();
            add_lands(&mut state, 6); // met ⇒ copy branch fires.

            let src = add_plain_planeswalker_source(&mut state, "Jace", 3);
            let sub = copy_instead_sub(src, 6);
            push_token_triggers(
                &mut state,
                src,
                insect_token_effect(),
                Some(Box::new(sub)),
                3,
            );

            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            assert!(
                try_batch(&state, &ability, run_len).is_none(),
                "copy-token batch must refuse values that emit intrinsic CounterAdded events"
            );
        }

        // CR 707.2 + CR 707.5 + CR 603.6a — THE HEADLINE Scute Swarm repro:
        // K distinct copy sources are real Scute-Swarm-shape creatures, each
        // carrying a landfall trigger keyed EnterBattlefield(Some(Land)). The
        // copied tokens are CREATURES that inherit the landfall trigger (CR
        // 707.2/707.5). A Creature copy emits {None, Some(Creature), TokenCreated}
        // — the Land-keyed landfall does NOT intersect it, so the §2.3a gate is
        // safe and the whole run STILL collapses into ONE batch. This is
        // DISCRIMINATING: under the OLD coarse gate (any EnterBattlefield(_)
        // rejected) try_resolve_copy_batch returned None and the run resolved
        // one-at-a-time — the named perf bug was never fixed for its own card.
        #[test]
        fn cross_source_copy_with_landfall_trigger_still_batches() {
            let mut base = setup();
            add_lands(&mut base, 6); // met ⇒ copy branch fires.

            // K distinct value-identical Scute-shape sources, each firing a met
            // copy-instead self-copy. Each source (and thus each copy) carries a
            // landfall trigger keyed on Land ETB — exactly Scute Swarm.
            for _ in 0..5 {
                let src = add_landfall_creature_source(&mut base, "Scute Swarm", 1, 1);
                let sub = copy_instead_sub(src, 6);
                push_token_triggers(
                    &mut base,
                    src,
                    insect_token_effect(),
                    Some(Box::new(sub)),
                    1,
                );
            }

            let mut batched = base.clone();
            let mut sequential = base.clone();

            let steps = resolve_to_empty_batched(&mut batched);
            resolve_to_empty_sequential(&mut sequential);

            assert_eq!(
                steps,
                vec![5],
                "the real Scute Swarm shape (landfall on a creature copy) MUST collapse \
                 into one batch — would be all-1 under the old coarse gate, got {steps:?}"
            );
            let batched_copies: Vec<_> = token_ids(&batched)
                .into_iter()
                .filter(|id| batched.objects[id].name == "Scute Swarm")
                .collect();
            assert_eq!(batched_copies.len(), 5, "5 Scute Swarm copies produced");
            assert_eq!(
                token_ids(&batched).len(),
                token_ids(&sequential).len(),
                "batched copy count must equal sequential"
            );
            // The copies carry the inherited landfall trigger (CR 707.2/707.5).
            for id in &batched_copies {
                assert!(
                    !batched.objects[id].trigger_definitions.is_empty(),
                    "the copy must inherit the source's landfall trigger"
                );
            }
        }

        // CR 603.6a (over-permit guard) — a SelfRef copy whose copied token DOES
        // observe its in-batch siblings must STILL refuse. The copy source is a
        // Creature carrying a "whenever a creature you control enters" trigger
        // (EnterBattlefield(Some(Creature))); the Creature copy's emission
        // includes Some(Creature), so the intersection is non-empty ⇒ refused.
        // Proves the refined gate did not become unsafe.
        #[test]
        fn cross_source_copy_with_creature_etb_observer_refuses_batch() {
            let mut state = setup();
            add_lands(&mut state, 6); // met ⇒ copy branch fires.

            for _ in 0..5 {
                let src = add_creature_observer_source(&mut state, "Watcher", 2, 2);
                let sub = copy_instead_sub(src, 6);
                push_token_triggers(
                    &mut state,
                    src,
                    insect_token_effect(),
                    Some(Box::new(sub)),
                    1,
                );
            }

            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            // The copied token observes creature ETB (its siblings) ⇒ the §2.3a
            // intersection is non-empty ⇒ must refuse to batch.
            assert!(
                try_batch(&state, &ability, run_len).is_none(),
                "a copy whose token observes creature-ETB siblings must refuse to batch"
            );
        }

        // CR 707.2 — divergent-tail prefix batching: K cross-source copies where
        // a middle source diverges in copiable values. The contiguous value-equal
        // PREFIX collapses; the divergent tail resolves in subsequent steps. The
        // step pattern proves prefix batching (not all-1, not one vec![K]), and
        // the final token count equals the sequential path.
        #[test]
        fn cross_source_copy_divergent_tail_batches_prefix_then_resolves_rest() {
            let mut base = setup();
            add_lands(&mut base, 6);

            // Push order (resolution order is top-down = LIFO): the LAST pushed
            // entry resolves first. Push the divergent source FIRST so it sits at
            // the BOTTOM and the value-equal sources are at the top.
            //
            // Build: 2 identical "Alpha" sources, then 1 "Beta" (divergent P/T),
            // then 2 more "Alpha". Pushed bottom→top. Resolution order (top→down):
            // Alpha, Alpha, Beta, Alpha, Alpha. The prefix is the top 2 Alphas.
            let specs: [(&str, i32, i32); 5] = [
                ("Alpha", 2, 2),
                ("Alpha", 2, 2),
                ("Beta", 3, 3),
                ("Alpha", 2, 2),
                ("Alpha", 2, 2),
            ];
            for (name, p, t) in specs {
                let src = add_plain_creature_source(&mut base, name, p, t);
                let sub = copy_instead_sub(src, 6);
                push_token_triggers(
                    &mut base,
                    src,
                    insect_token_effect(),
                    Some(Box::new(sub)),
                    1,
                );
            }

            let mut batched = base.clone();
            let mut sequential = base.clone();

            let steps = resolve_to_empty_batched(&mut batched);
            resolve_to_empty_sequential(&mut sequential);

            // The top 2 Alphas batch (prefix), then Beta resolves, then the
            // bottom 2 Alphas batch. NOT all-1 and NOT a single vec![5].
            assert_eq!(
                steps,
                vec![2, 1, 2],
                "prefix batching must collapse the value-equal head, got {steps:?}"
            );
            // 5 copy tokens total (3 Alpha + 1 Beta + ... by name), equal to
            // sequential.
            assert_eq!(token_ids(&batched).len(), 5);
            assert_eq!(
                token_ids(&batched).len(),
                token_ids(&sequential).len(),
                "batched count must equal sequential"
            );
        }

        // CR 608.2c (H1 discriminator) — a met copy that creates LANDS gated on a
        // LAND count must NOT batch: the copy's core types intersect the counted
        // type, so the intervening condition is order-sensitive across the run.
        // This FAILS if the invariance gate is fed the base placeholder core
        // types ([Creature]) and PASSES (refuses) when fed the COPY core types
        // ([Land]).
        #[test]
        fn met_copy_creating_lands_gated_on_land_count_refuses_batch() {
            let mut state = setup();
            add_lands(&mut state, 6); // met ⇒ copy branch fires.

            // Observer-free copy source whose copiable type is LAND (not the
            // base Insect Creature). Copying it produces Land tokens.
            let land_src = create_object(
                &mut state,
                CardId(911),
                PlayerId(0),
                "Mirror Land".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&land_src).unwrap();
                obj.base_card_types = crate::types::card_type::CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Land],
                    subtypes: vec!["Forest".to_string()],
                };
                obj.card_types = obj.base_card_types.clone();
                obj.base_name = "Mirror Land".to_string();
            }
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

            let sub = copy_instead_sub(land_src, 6);
            push_token_triggers(
                &mut state,
                land_src,
                insect_token_effect(),
                Some(Box::new(sub)),
                5,
            );

            let run_len = batch_run_len(&state).unwrap();
            let ability = state.stack.back().unwrap().ability().unwrap().clone();
            // The copy creates Lands; the condition counts Lands ⇒ each created
            // Land flips the count ⇒ order-sensitive ⇒ must refuse.
            assert!(
                try_batch(&state, &ability, run_len).is_none(),
                "a met copy creating Lands gated on a Land count must refuse to batch"
            );
        }

        /// Build an OPTIONAL token-count-doubling replacement ("you may create
        /// twice that many tokens instead").
        fn optional_token_doubling_replacement() -> crate::types::ability::ReplacementDefinition {
            use crate::types::ability::{
                QuantityModification, ReplacementDefinition, ReplacementMode,
            };
            use crate::types::replacements::ReplacementEvent;
            let mut def = ReplacementDefinition::new(ReplacementEvent::CreateToken);
            def.mode = ReplacementMode::Optional { decline: None };
            def.quantity_modification = Some(QuantityModification::DOUBLE);
            def
        }

        /// Build a mandatory token-count-doubling replacement definition
        /// (Doubling Season's "create twice that many tokens instead").
        fn doubling_season_replacement() -> crate::types::ability::ReplacementDefinition {
            use crate::types::ability::{
                QuantityModification, ReplacementDefinition, ReplacementMode,
            };
            use crate::types::replacements::ReplacementEvent;
            let mut def = ReplacementDefinition::new(ReplacementEvent::CreateToken);
            def.mode = ReplacementMode::Mandatory;
            def.quantity_modification = Some(QuantityModification::DOUBLE);
            def
        }

        // ====================================================================
        // Incremental layer-flush performance + correctness regression tests.
        // ====================================================================

        use crate::game::layers::{evaluate_layers, flush_layers, FULL_EVALUATE_LAYERS_COUNT};
        use std::sync::atomic::Ordering;

        /// (A) REAL-BOARD smoke test on the 583-Scute-Swarm `/tmp/gamestate.json`
        /// repro. Resolves a BOUNDED PREFIX of the landfall-trigger stack (each
        /// step: resolve_next + process_triggers + SBA loop, the real pipeline)
        /// and asserts the incremental flush never DEGRADES past one full
        /// `evaluate_layers` per step.
        ///
        /// NOTE: this fixture is NOT an O(N) board, and that is rules-correct. Its
        /// entries are the six-lands branch of Scute Swarm's landfall ("create a
        /// token that's a COPY of Scute Swarm", CR 707.2), so each copy-token
        /// carries the copiable {2}{G} mana cost and genuinely moves green
        /// devotion (CR 700.5). Kruphix's `Not(DevotionGE {G/U,7})` gate can flip
        /// for the whole recipient set on every entry, so per-entry escalation is
        /// MANDATORY (under-escalating would leave stale derived state — CR 611.3a,
        /// the #1 hard rule). The discriminating O(N) guarantee for NON-perturbing
        /// (colorless / non-land) entries is proven by the synthetic per-axis dual
        /// tests below, which can control the entry's characteristics precisely.
        ///
        /// Bounded to a prefix because the FULL 2,891-trigger resolution is
        /// dominated by the O(N²) trigger-scan / SBA pipeline (independent of the
        /// layers fix) and is impractically slow in a debug build.
        ///
        /// Self-skips when `/tmp/gamestate.json` is absent (CI lacks the repro).
        /// `#[ignore]` by default: depends on a local-only 27MB snapshot. Run with
        /// `cargo test -p phase-engine -- --ignored real_scute_board`.
        /// Re-parse every `StaticCondition::Unrecognized` carried in a snapshot's
        /// static definitions through the live `parse_inner_condition`, replacing
        /// any that now parse to a typed condition. The snapshot's stored text has
        /// the "as long as " / "if " prefix already stripped (that's the form the
        /// parser records on a fallback), so the prefix-free inner parser is the
        /// correct entry point. Patches both `static_definitions` (live, layer-
        /// flushed) and `base_static_definitions` (the rebuild source). This makes
        /// a pre-fix snapshot reflect the parser change under test.
        fn normalize_unrecognized_static_conditions(state: &mut GameState) {
            use crate::parser::oracle_nom::condition::parse_inner_condition;
            use crate::types::ability::{StaticCondition, StaticDefinition};
            let reparse = |def: &StaticDefinition| -> StaticDefinition {
                let Some(StaticCondition::Unrecognized { text }) = def.condition.as_ref() else {
                    return def.clone();
                };
                match parse_inner_condition(text) {
                    Ok(("", parsed)) => {
                        let mut new_def = def.clone();
                        new_def.condition = Some(parsed);
                        new_def
                    }
                    _ => def.clone(),
                }
            };
            let ids: Vec<ObjectId> = state.objects.keys().copied().collect();
            for id in ids {
                let Some(obj) = state.objects.get_mut(&id) else {
                    continue;
                };
                let new_live: Vec<StaticDefinition> =
                    obj.static_definitions.iter_all().map(&reparse).collect();
                let new_base: Vec<StaticDefinition> =
                    obj.base_static_definitions.iter().map(&reparse).collect();
                obj.static_definitions = new_live.into();
                obj.base_static_definitions = Arc::new(new_base);
            }
        }

        #[test]
        #[ignore = "requires local /tmp/gamestate.json repro"]
        fn real_scute_board_resolution_is_not_full_eval_per_token() {
            let path = "/tmp/gamestate.json";
            let Ok(contents) = std::fs::read_to_string(path) else {
                eprintln!("skipping: {path} not present");
                return;
            };
            let wrapper: serde_json::Value =
                serde_json::from_str(&contents).expect("repro wrapper must parse");
            let gs_value = wrapper
                .get("gameState")
                .expect("wrapper must have gameState member")
                .clone();
            let mut state: GameState =
                serde_json::from_value(gs_value).expect("gameState must deserialize");

            // This repro was serialized BEFORE the Grist source-zone parser fix,
            // so Grist's "as long as ~ isn't on the battlefield" static is frozen
            // in the snapshot as `StaticCondition::Unrecognized` (which the
            // escalation classifier must treat as conservatively population-
            // sensitive → escalate every step). A snapshot generated by the
            // fixed parser would instead carry `Not(SourceInZone { Battlefield })`,
            // which the classifier proves population-INDEPENDENT. Re-run every
            // `Unrecognized` static condition through the live parser so the board
            // reflects the parser fix under test — this is exactly the AST a fresh
            // export would produce, not a test-only special case.
            normalize_unrecognized_static_conditions(&mut state);

            let stack_size = state.stack.len();
            assert!(
                stack_size > 100,
                "repro must have a large stack (got {stack_size})"
            );

            // First flush rebuilds fully (deserialized snapshot defaults to Full).
            // Reset the counter AFTER that initial mandatory full pass so we only
            // measure per-resolution behavior.
            flush_layers(&mut state);
            FULL_EVALUATE_LAYERS_COUNT.store(0, Ordering::Relaxed);

            const PREFIX_STEPS: usize = 120;
            let mut steps = 0usize;
            let resolve_start = std::time::Instant::now();
            while !state.stack.is_empty() && steps < PREFIX_STEPS {
                let mut events = Vec::new();
                resolve_next(&mut state, &mut events);
                triggers::process_triggers(&mut state, &events);
                crate::game::sba::check_state_based_actions(&mut state, &mut events);
                steps += 1;
            }
            let resolve_elapsed = resolve_start.elapsed();
            let full_evals = FULL_EVALUATE_LAYERS_COUNT.load(Ordering::Relaxed);
            eprintln!(
                "real-board probe: full_evals={full_evals} steps={steps} \
                 wall_clock={resolve_elapsed:?} ({:.1}ms/step)",
                resolve_elapsed.as_secs_f64() * 1000.0 / steps.max(1) as f64
            );

            assert!(
                steps > 20,
                "prefix must resolve enough steps to discriminate (got {steps})"
            );
            // CR 611.3a + CR 611.3b — TRUTH-DELTA SHORT-CIRCUIT: full evals on
            // this repro collapse to NEAR-CONSTANT (measured 4 across 120 steps,
            // down from ~63 before the short-circuit). The board carries board-
            // population-gated statics (Kruphix `Not(DevotionGE {G/U,7})`,
            // Anger/Brawn land-presence, Grist's source-zone gate). The entries
            // are the six-lands branch of Scute Swarm's landfall: "create a token
            // that's a COPY of Scute Swarm" (CR 707.2 — the copy takes the
            // copiable mana cost {2}{G}), so each copy-token carries a GREEN mana
            // symbol and CR 700.5 devotion to green strictly INCREASES on every
            // entry.
            //
            // Under d9a40be71 every such devotion-perturbing entry escalated to a
            // full pass (~1 per copy → ~63). But Kruphix's gate is
            // `Not(DevotionGE 7)`: once green devotion is already >= 7 (it is,
            // early), the gate TRUTH is stable FALSE and never flips again no
            // matter how high devotion climbs. The truth-delta short-circuit
            // recomputes the gate's AFTER truth against the live board and skips
            // escalation when `before == after` — so devotion-perturbing-but-
            // non-flipping entries now stay on the incremental fast path. The few
            // residual full evals are rules-MANDATORY flips (a genuine gate
            // crossing, e.g. an early devotion edge or a land-presence gate
            // flipping once) or Axis-1 escalations; they are NOT
            // under-escalation — `after` is always recomputed authoritatively
            // from the live board (CR 611.3a), so the short-circuit errs only
            // toward over-escalation, never stale derived state.
            //
            // Bound: `full_evals < steps/4 + 8` proves the near-O(1) collapse
            // (the measured 4 sits far under 38) while leaving headroom for the
            // handful of rules-mandatory flips. The per-axis synthetic dual tests
            // above pin the exact short-circuit / escalation decision per axis.
            assert!(
                full_evals < steps / 4 + 8,
                "truth-delta short-circuit must keep full evaluate_layers passes \
                 near-constant: got {full_evals} full passes across {steps} steps \
                 (stack was {stack_size}). Kruphix's `Not(DevotionGE 7)` gate is \
                 stable FALSE once devotion >= 7 (CR 700.5 / CR 611.3a), so \
                 devotion-perturbing copy-token entries must NOT escalate — a count \
                 anywhere near `steps` would mean the short-circuit regressed back \
                 to per-entry escalation."
            );
        }

        /// Build a battlefield with a Devotion-magnitude anthem source plus
        /// pre-existing creatures, then push a single token-creation trigger.
        /// Returns (state, anthem_source_id).
        ///
        /// The anthem is "creatures you control get +X/+X where X = your devotion
        /// to green" — a board-population-dependent magnitude
        /// (`DistinctColorsAmongPermanents`-class via `Devotion`). A token entry
        /// changes devotion, so the magnitude applied to PRE-EXISTING creatures
        /// must re-evaluate; the escalation scan must force a full pass.
        fn devotion_anthem_board() -> GameState {
            use crate::types::ability::DevotionColors;
            use crate::types::ability::{ContinuousModification, StaticDefinition};
            use crate::types::statics::StaticMode;
            let mut state = setup();
            // Two pre-existing green creatures.
            for i in 0..2 {
                let id = create_object(
                    &mut state,
                    CardId(50 + i),
                    PlayerId(0),
                    format!("Bear{i}"),
                    Zone::Battlefield,
                );
                let o = state.objects.get_mut(&id).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                o.base_color = vec![ManaColor::Green];
                o.color = vec![ManaColor::Green];
            }
            // Anthem source: "creatures you control get +X/+X, X = devotion to green".
            let anthem = create_object(
                &mut state,
                CardId(60),
                PlayerId(0),
                "Devotion Anthem".to_string(),
                Zone::Battlefield,
            );
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)));
            sd.modifications = vec![
                ContinuousModification::AddDynamicPower {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::Devotion {
                            colors: DevotionColors::Fixed(vec![ManaColor::Green]),
                        },
                    },
                },
                ContinuousModification::AddDynamicToughness {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::Devotion {
                            colors: DevotionColors::Fixed(vec![ManaColor::Green]),
                        },
                    },
                },
            ];
            {
                let o = state.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
                o.base_color = vec![ManaColor::Green];
                o.color = vec![ManaColor::Green];
            }
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        /// (B1) Population-magnitude escalation: with a devotion-magnitude anthem,
        /// a token entry must escalate the incremental flush to a full pass so
        /// pre-existing creatures' P/T re-evaluate. Dual-run: the normal flush
        /// path must produce a board characteristic-identical to a forced-Full
        /// flush.
        #[test]
        fn devotion_anthem_token_entry_escalates_and_matches_full() {
            let mut base = devotion_anthem_board();
            let src = add_scute_source(&mut base);
            push_token_triggers(&mut base, src, insect_token_effect(), None, 1);

            // Normal path (incremental flush eligible; must escalate).
            let mut normal = base.clone();
            resolve_to_empty_batched(&mut normal);
            flush_layers(&mut normal);

            // Forced-full reference: same resolution, then force a full re-eval.
            let mut forced = base.clone();
            resolve_to_empty_batched(&mut forced);
            forced.layers_dirty = crate::types::game_state::LayersDirty::Full;
            evaluate_layers(&mut forced);

            assert_pt_identical(&normal, &forced, "devotion anthem escalation");
        }

        /// (B2) Recipient-local dynamic ("+1/+1 for each +1/+1 counter on IT",
        /// `CountersOn { Recipient }`) must NOT escalate — it does not read board
        /// population — and the incremental result still matches a full recompute.
        #[test]
        fn recipient_local_dynamic_does_not_escalate_and_matches_full() {
            use crate::types::ability::{ContinuousModification, ObjectScope, StaticDefinition};
            use crate::types::statics::StaticMode;
            let mut base = setup();
            // A creature with a recipient-local self-buff static and a +1/+1 counter.
            let id = create_object(
                &mut base,
                CardId(70),
                PlayerId(0),
                "Recipient Buff".to_string(),
                Zone::Battlefield,
            );
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::SelfRef);
            sd.modifications = vec![ContinuousModification::AddDynamicPower {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::CountersOn {
                        scope: ObjectScope::Recipient,
                        counter_type: Some(CounterType::Plus1Plus1),
                    },
                },
            }];
            {
                let o = base.objects.get_mut(&id).unwrap();
                o.base_power = Some(1);
                o.base_toughness = Some(1);
                o.power = Some(1);
                o.toughness = Some(1);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                o.counters.insert(CounterType::Plus1Plus1, 2);
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
            }
            base.layers_dirty = crate::types::game_state::LayersDirty::Full;

            let src = add_scute_source(&mut base);
            push_token_triggers(&mut base, src, insect_token_effect(), None, 1);

            let mut normal = base.clone();
            // Prove the escalation predicate does NOT fire for this board: after
            // resolving, the dirty state right before flush should be EnteredObjects
            // and the incremental path must apply.
            FULL_EVALUATE_LAYERS_COUNT.store(0, Ordering::Relaxed);
            resolve_to_empty_batched(&mut normal);
            flush_layers(&mut normal);

            let mut forced = base.clone();
            resolve_to_empty_batched(&mut forced);
            forced.layers_dirty = crate::types::game_state::LayersDirty::Full;
            evaluate_layers(&mut forced);

            assert_pt_identical(&normal, &forced, "recipient-local dynamic");
        }

        /// (B-embedded) Population-dependent EMBEDDED THRESHOLD escalation.
        ///
        /// A continuous static whose AFFECTED FILTER is a `PtComparison` with an
        /// `ObjectCount`-backed threshold ("creatures with power <= the number of
        /// creatures you control get +1/+1"). A token entry changes the creature
        /// count, which changes the threshold, which changes whether PRE-EXISTING
        /// creatures match the affected filter. The escalation scan must fire via
        /// the `affected_filter_uses_object_population` → embedded-threshold
        /// `quantity_expr_uses_object_count` recursion, forcing a full pass.
        ///
        /// Dual-run: the normal flush path must produce a board characteristic-
        /// identical to a forced-Full flush.
        #[test]
        fn embedded_threshold_token_entry_escalates_and_matches_full() {
            use crate::types::ability::{
                Comparator, ContinuousModification, FilterProp, PtStat, PtValueScope,
                StaticDefinition,
            };
            use crate::types::statics::StaticMode;
            let mut base = setup();
            // Two pre-existing 1/1 green creatures.
            for i in 0..2 {
                let id = create_object(
                    &mut base,
                    CardId(80 + i),
                    PlayerId(0),
                    format!("Smol{i}"),
                    Zone::Battlefield,
                );
                let o = base.objects.get_mut(&id).unwrap();
                o.base_power = Some(1);
                o.base_toughness = Some(1);
                o.power = Some(1);
                o.toughness = Some(1);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                o.base_color = vec![ManaColor::Green];
                o.color = vec![ManaColor::Green];
            }
            // Anthem source: "creatures you control with power <= (number of
            // creatures you control) get +1/+1" — affected set keyed by an
            // ObjectCount-backed PtComparison threshold.
            let anthem = create_object(
                &mut base,
                CardId(90),
                PlayerId(0),
                "Threshold Anthem".to_string(),
                Zone::Battlefield,
            );
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                properties: vec![FilterProp::PtComparison {
                    stat: PtStat::Power,
                    scope: PtValueScope::Current,
                    comparator: Comparator::LE,
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(TypedFilter {
                                type_filters: vec![TypeFilter::Creature],
                                ..Default::default()
                            }),
                        },
                    },
                }],
                ..Default::default()
            }));
            sd.modifications = vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ];
            {
                let o = base.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
            }
            base.layers_dirty = crate::types::game_state::LayersDirty::Full;

            let src = add_scute_source(&mut base);
            push_token_triggers(&mut base, src, insect_token_effect(), None, 1);

            let mut normal = base.clone();
            resolve_to_empty_batched(&mut normal);
            flush_layers(&mut normal);

            let mut forced = base.clone();
            resolve_to_empty_batched(&mut forced);
            forced.layers_dirty = crate::types::game_state::LayersDirty::Full;
            evaluate_layers(&mut forced);

            assert_pt_identical(&normal, &forced, "embedded-threshold escalation");
        }

        /// (B-condition) Population-dependent source-level CONDITION escalation.
        ///
        /// A continuous anthem static "creatures you control get +1/+1 as long as
        /// you control 3 or more creatures" — a source-level enabling condition
        /// (`QuantityComparison` over `ObjectCount`) that gates the effect for the
        /// WHOLE recipient set (not recipient-local). The board starts one short
        /// of the threshold (2 creatures), so the condition is OFF and no creature
        /// is buffed. A single token entry crosses the threshold (→ 3 creatures),
        /// flipping the condition ON for EVERY pre-existing creature.
        ///
        /// The incremental flush re-derives only the entered token, so without the
        /// condition-axis escalation clause the pre-existing creatures would keep
        /// stale (unbuffed) P/T. The escalation scan must fire via
        /// `static_condition_uses_object_population` →
        /// `quantity_expr_uses_object_count`, forcing a full pass.
        ///
        /// Asserts (a) the entry escalated to a FULL pass (the full-eval counter
        /// incremented exactly once during the normal-path flush) and (b) dual-run
        /// characteristic-identity: the normal flush produces a board identical to
        /// a forced-Full flush, with pre-existing creatures at the flipped-on P/T.
        #[test]
        fn condition_gated_anthem_token_entry_escalates_and_matches_full() {
            use crate::types::ability::{
                Comparator, ContinuousModification, StaticCondition, StaticDefinition,
            };
            use crate::types::statics::StaticMode;
            let mut base = setup();
            // Two pre-existing 2/2 creatures — one short of the ≥3 threshold.
            let mut creature_ids = Vec::new();
            for i in 0..2 {
                let id = create_object(
                    &mut base,
                    CardId(100 + i),
                    PlayerId(0),
                    format!("Gater{i}"),
                    Zone::Battlefield,
                );
                let o = base.objects.get_mut(&id).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                creature_ids.push(id);
            }
            // Anthem source (an Enchantment — does NOT count toward the creature
            // threshold): "creatures you control get +1/+1 as long as you control
            // 3 or more creatures".
            let anthem = create_object(
                &mut base,
                CardId(110),
                PlayerId(0),
                "Condition Anthem".to_string(),
                Zone::Battlefield,
            );
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)));
            sd.modifications = vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ];
            sd.condition = Some(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Creature],
                            ..Default::default()
                        }),
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            });
            {
                let o = base.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
            }
            base.layers_dirty = crate::types::game_state::LayersDirty::Full;

            // Sanity: with 2 creatures the condition is OFF — no buff yet.
            flush_layers(&mut base);
            for &id in &creature_ids {
                let o = base.objects.get(&id).unwrap();
                assert_eq!(o.power, Some(2), "condition should be off below threshold");
            }

            let src = add_scute_source(&mut base);
            push_token_triggers(&mut base, src, insect_token_effect(), None, 1);

            // Normal path: incremental flush eligible; the condition-axis escalation
            // must force a full pass. Reset the counter BEFORE resolution so a
            // flush triggered inside the resolve pipeline (SBA / batch resolve)
            // is counted too — escalation must occur somewhere in the
            // resolve-then-flush window, not necessarily on the final explicit
            // flush (the pipeline may have already drained the EnteredObjects
            // mark by the time we flush below).
            let mut normal = base.clone();
            FULL_EVALUATE_LAYERS_COUNT.store(0, Ordering::Relaxed);
            resolve_to_empty_batched(&mut normal);
            flush_layers(&mut normal);
            let full_evals = FULL_EVALUATE_LAYERS_COUNT.load(Ordering::Relaxed);
            assert!(
                full_evals >= 1,
                "token entry crossing a board-population-gated condition must \
                 escalate the incremental flush to a full pass (got {full_evals})"
            );

            // Forced-full reference. Build AFTER reading the counter so its own
            // `evaluate_layers` does not perturb the measurement above.
            let mut forced = base.clone();
            resolve_to_empty_batched(&mut forced);
            forced.layers_dirty = crate::types::game_state::LayersDirty::Full;
            evaluate_layers(&mut forced);

            // Pre-existing creatures must be flipped ON (3/3), not stale (2/2).
            for &id in &creature_ids {
                let o = normal.objects.get(&id).unwrap();
                assert_eq!(
                    o.power,
                    Some(3),
                    "pre-existing creature must be buffed after the condition flips on"
                );
            }
            assert_pt_identical(&normal, &forced, "condition-gated anthem escalation");
        }

        // ====================================================================
        // ENTRY-AWARE escalation tests (cheap-reject classifier + entry-
        // membership narrowing). Each axis is a DUAL pair: a non-perturbing
        // entry that must NOT escalate (full_evals == 0) AND a perturbing entry
        // that MUST escalate. EVERY no-escalate case ALSO asserts dual-run
        // characteristic-identity (incremental vs forced-Full) — the under-
        // escalation tripwire.
        // ====================================================================

        /// ISOLATED single-flush measurement of the entry-aware escalation
        /// decision. `setup_board` returns a board with the anthem already in
        /// place (still `Full`-dirty). The helper:
        ///   1. flushes the board to Clean (the anthem's initial full pass — NOT
        ///      measured),
        ///   2. invokes `add_entry` to create the entering object and returns its
        ///      id (the closure must `mark_layers_entered` so the dirty lattice is
        ///      `EnteredObjects`),
        ///   3. resets the counter and performs a SINGLE `flush_layers`, capturing
        ///      exactly the entry-aware escalation decision (0 = incremental fast
        ///      path engaged, >=1 = escalated to a full pass),
        ///   4. builds a forced-Full reference from the same post-entry board for
        ///      dual-run characteristic identity.
        ///
        /// This isolates the escalation DECISION from the token-RESOLUTION
        /// pipeline (which does unrelated full passes during `Effect::Token`
        /// resolution / SBA).
        ///
        /// The escalation signal is read RACE-FREE from
        /// `incremental_flush_must_escalate` directly (a pure predicate over the
        /// post-entry board) rather than from the process-wide
        /// `FULL_EVALUATE_LAYERS_COUNT`, which `cargo test`'s parallel runner
        /// would otherwise corrupt. Returns `(incremental_board, escalated,
        /// forced_full_board)`, where `escalated == false` means the entry-aware
        /// fast path engaged and `escalated == true` means the entry forced a full
        /// pass. The dual-run identity (`incremental_board` vs `forced_full_board`)
        /// is the under-escalation tripwire regardless of the decision.
        fn flush_entry_and_forced(
            setup_board: impl Fn() -> GameState,
            add_entry: impl Fn(&mut GameState) -> ObjectId,
        ) -> (GameState, bool, GameState) {
            // Normal path: flush the anthem in, add the entry, read the decision,
            // then flush incrementally (or full, per the decision).
            let mut normal = setup_board();
            flush_layers(&mut normal);
            add_entry(&mut normal);
            let entered_ids: std::collections::BTreeSet<ObjectId> = match &normal.layers_dirty {
                crate::types::game_state::LayersDirty::EnteredObjects(ids) => ids.clone(),
                other => panic!("expected EnteredObjects dirty state, got {other:?}"),
            };
            let escalated =
                crate::game::layers::incremental_flush_must_escalate(&normal, &entered_ids);
            flush_layers(&mut normal);

            // Forced-Full reference: same board + entry, then a full re-eval.
            let mut forced = setup_board();
            flush_layers(&mut forced);
            add_entry(&mut forced);
            forced.layers_dirty = crate::types::game_state::LayersDirty::Full;
            evaluate_layers(&mut forced);
            (normal, escalated, forced)
        }

        /// Create a plain colorless, non-land creature ("Insect"-like) entry and
        /// mark layers entered. Flips no devotion / land-presence gate and matches
        /// no artifact/land filter.
        fn add_colorless_creature_entry(state: &mut GameState, card_id: u64) -> ObjectId {
            add_colorless_creature_entry_under(state, card_id, PlayerId(0))
        }

        /// The same entrant under an explicit controller, for controller-keyed
        /// population fixtures (CR 613.1b).
        fn add_colorless_creature_entry_under(
            state: &mut GameState,
            card_id: u64,
            controller: PlayerId,
        ) -> ObjectId {
            let id = create_object(
                state,
                CardId(card_id),
                controller,
                "Insect".to_string(),
                Zone::Battlefield,
            );
            {
                let o = state.objects.get_mut(&id).unwrap();
                o.base_power = Some(1);
                o.base_toughness = Some(1);
                o.power = Some(1);
                o.toughness = Some(1);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                o.base_color = vec![];
                o.color = vec![];
            }
            crate::game::layers::mark_layers_entered(state, id);
            id
        }

        /// (1a) Devotion gate — NON-perturbing: a colorless creature entering under
        /// a devotion-to-green magnitude anthem flips no green devotion symbol, so
        /// the entry must stay on the incremental path (full_evals==0) AND the
        /// incremental board must match a forced-Full board.
        #[test]
        fn devotion_gate_colorless_entry_does_not_escalate_and_matches_full() {
            let (normal, escalated, forced) = flush_entry_and_forced(devotion_anthem_board, |s| {
                add_colorless_creature_entry(s, 200)
            });
            assert!(
                !escalated,
                "colorless entry flips no green devotion shard — must not escalate"
            );
            assert_pt_identical(&normal, &forced, "devotion gate colorless non-escalation");
        }

        /// (1b) Devotion gate — PERTURBING: a green {G}-cost permanent entering DOES
        /// add a green devotion symbol (CR 700.5 counts mana symbols, so a token's
        /// color alone is irrelevant — the entry must carry a green shard), so the
        /// magnitude on pre-existing creatures changes and the entry MUST escalate.
        #[test]
        fn devotion_gate_green_entry_escalates_and_matches_full() {
            let add_green = |s: &mut GameState| {
                let green = create_object(
                    s,
                    CardId(201),
                    PlayerId(0),
                    "Green Bear".to_string(),
                    Zone::Battlefield,
                );
                {
                    use crate::types::mana::{ManaCost, ManaCostShard};
                    let o = s.objects.get_mut(&green).unwrap();
                    o.base_card_types.core_types = vec![CoreType::Creature];
                    o.card_types.core_types = vec![CoreType::Creature];
                    o.base_color = vec![ManaColor::Green];
                    o.color = vec![ManaColor::Green];
                    o.mana_cost = ManaCost::Cost {
                        shards: vec![ManaCostShard::Green],
                        generic: 0,
                    };
                    o.base_mana_cost = o.mana_cost.clone();
                }
                crate::game::layers::mark_layers_entered(s, green);
                green
            };
            let (normal, escalated, forced) =
                flush_entry_and_forced(devotion_anthem_board, add_green);
            assert!(
                escalated,
                "green {{G}}-cost permanent entry moves devotion — must escalate"
            );
            assert_pt_identical(&normal, &forced, "devotion gate green escalation");
        }

        /// Build a board with an `IsPresent(Land)`-gated anthem: "creatures you
        /// control get +1/+1 as long as you control a land". Two pre-existing
        /// creatures, no land yet (gate OFF).
        fn is_present_land_board() -> GameState {
            use crate::types::ability::{
                ContinuousModification, StaticCondition, StaticDefinition,
            };
            use crate::types::statics::StaticMode;
            let mut state = setup();
            for i in 0..2 {
                let id = create_object(
                    &mut state,
                    CardId(210 + i),
                    PlayerId(0),
                    format!("LandGater{i}"),
                    Zone::Battlefield,
                );
                let o = state.objects.get_mut(&id).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
            }
            let anthem = create_object(
                &mut state,
                CardId(220),
                PlayerId(0),
                "Land Presence Anthem".to_string(),
                Zone::Battlefield,
            );
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)));
            sd.modifications = vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ];
            sd.condition = Some(StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Land))),
            });
            {
                let o = state.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
            }
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        /// (2a) IsPresent(Land) gate — NON-perturbing: a colorless creature entry
        /// does not satisfy the Land filter, so the land-presence gate cannot
        /// flip; must NOT escalate AND must match a forced-Full board.
        #[test]
        fn is_present_land_creature_entry_does_not_escalate_and_matches_full() {
            let (normal, escalated, forced) = flush_entry_and_forced(is_present_land_board, |s| {
                add_colorless_creature_entry(s, 231)
            });
            assert!(
                !escalated,
                "creature entry doesn't match Land filter — gate can't flip, no escalation"
            );
            assert_pt_identical(&normal, &forced, "IsPresent(Land) creature non-escalation");
        }

        /// (2b) IsPresent(Land) gate — PERTURBING: a land entering satisfies the
        /// Land filter and flips the gate from OFF to ON for every pre-existing
        /// creature; MUST escalate AND match a forced-Full board.
        #[test]
        fn is_present_land_land_entry_escalates_and_matches_full() {
            let add_land = |s: &mut GameState| {
                let land = create_object(
                    s,
                    CardId(232),
                    PlayerId(0),
                    "Forest".to_string(),
                    Zone::Battlefield,
                );
                {
                    let o = s.objects.get_mut(&land).unwrap();
                    o.base_card_types.core_types = vec![CoreType::Land];
                    o.card_types.core_types = vec![CoreType::Land];
                }
                crate::game::layers::mark_layers_entered(s, land);
                land
            };
            let (normal, escalated, forced) =
                flush_entry_and_forced(is_present_land_board, add_land);
            assert!(
                escalated,
                "land entry flips IsPresent(Land) ON — must escalate"
            );
            assert_pt_identical(&normal, &forced, "IsPresent(Land) land escalation");
        }

        /// Build a board with a count-anthem magnitude keyed by "artifacts you
        /// control": "creatures you control get +X/+X, X = number of artifacts
        /// you control". Two pre-existing creatures.
        fn artifact_count_anthem_board() -> GameState {
            use crate::types::ability::{
                ContinuousModification, StaticDefinition, TypeFilter as TF, TypedFilter as TFil,
            };
            use crate::types::statics::StaticMode;
            let mut state = setup();
            for i in 0..2 {
                let id = create_object(
                    &mut state,
                    CardId(240 + i),
                    PlayerId(0),
                    format!("CountBear{i}"),
                    Zone::Battlefield,
                );
                let o = state.objects.get_mut(&id).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
            }
            let anthem = create_object(
                &mut state,
                CardId(250),
                PlayerId(0),
                "Artifact Count Anthem".to_string(),
                Zone::Battlefield,
            );
            let artifact_filter = TargetFilter::Typed(TFil::new(TF::Artifact));
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::Typed(TFil::new(TF::Creature)));
            sd.modifications = vec![
                ContinuousModification::AddDynamicPower {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: artifact_filter.clone(),
                        },
                    },
                },
                ContinuousModification::AddDynamicToughness {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: artifact_filter,
                        },
                    },
                },
            ];
            {
                let o = state.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
            }
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        /// Create a colorless artifact (non-land, non-creature) entry and mark
        /// layers entered.
        fn add_artifact_entry(state: &mut GameState, card_id: u64) -> ObjectId {
            let id = create_object(
                state,
                CardId(card_id),
                PlayerId(0),
                "Treasure".to_string(),
                Zone::Battlefield,
            );
            {
                let o = state.objects.get_mut(&id).unwrap();
                o.base_card_types.core_types = vec![CoreType::Artifact];
                o.card_types.core_types = vec![CoreType::Artifact];
            }
            crate::game::layers::mark_layers_entered(state, id);
            id
        }

        /// (3a) Count-anthem (ObjectCount artifacts) — NON-perturbing: a colorless
        /// creature entry doesn't match "artifacts you control", so the magnitude
        /// on pre-existing creatures cannot change; must NOT escalate AND match
        /// full.
        #[test]
        fn count_anthem_nonmatching_entry_does_not_escalate_and_matches_full() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(artifact_count_anthem_board, |s| {
                    add_colorless_creature_entry(s, 251)
                });
            assert!(
                !escalated,
                "creature entry doesn't match artifact count filter — no escalation"
            );
            assert_pt_identical(&normal, &forced, "count-anthem non-matching non-escalation");
        }

        /// (3b) Count-anthem (ObjectCount artifacts) — PERTURBING: an artifact
        /// entry matches the count filter, changing the magnitude applied to
        /// pre-existing creatures; MUST escalate AND match full.
        #[test]
        fn count_anthem_matching_entry_escalates_and_matches_full() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(artifact_count_anthem_board, |s| add_artifact_entry(s, 252));
            assert!(
                escalated,
                "artifact entry matches artifact count filter — must escalate"
            );
            assert_pt_identical(&normal, &forced, "count-anthem matching escalation");
        }

        /// Build a board pairing a GREEN-keyed count magnitude with a color
        /// wash: one enchantment carries "creatures get +X/+X, X = number of
        /// green creatures" AND "creatures are green in addition to their other
        /// colors" (layer 5). Two pre-existing 2/2 Bears — green via the wash,
        /// so the count starts at 2 and both flush to 4/4.
        fn green_count_anthem_with_color_wash_board() -> GameState {
            use crate::types::ability::{ContinuousModification, StaticDefinition};
            use crate::types::statics::StaticMode;
            let mut state = setup();
            for i in 0..2 {
                let id = create_object(
                    &mut state,
                    CardId(280 + i),
                    PlayerId(0),
                    format!("WashBear{i}"),
                    Zone::Battlefield,
                );
                let o = state.objects.get_mut(&id).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                o.base_color = vec![];
                o.color = vec![];
            }
            let anthem = create_object(
                &mut state,
                CardId(290),
                PlayerId(0),
                "Color Wash Count Anthem".to_string(),
                Zone::Battlefield,
            );
            // "creatures are green in addition to their other colors" (layer 5).
            let mut wash = StaticDefinition::new(StaticMode::Continuous);
            wash.affected = Some(TargetFilter::Typed(TypedFilter::creature()));
            wash.modifications = vec![ContinuousModification::AddColor {
                color: ManaColor::Green,
            }];
            // "creatures get +X/+X, X = number of green creatures" (layer 7c).
            let green_creatures = TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                properties: vec![FilterProp::HasColor {
                    color: ManaColor::Green,
                }],
                ..Default::default()
            });
            let mut count = StaticDefinition::new(StaticMode::Continuous);
            count.affected = Some(TargetFilter::Typed(TypedFilter::creature()));
            count.modifications = vec![
                ContinuousModification::AddDynamicPower {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: green_creatures.clone(),
                        },
                    },
                },
                ContinuousModification::AddDynamicToughness {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: green_creatures,
                        },
                    },
                },
            ];
            {
                let o = state.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![wash.clone(), count.clone()]);
                o.static_definitions = vec![wash, count].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
            }
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        /// COLOR channel (CR 613.1e + CR 613.1g): a population keyed on COLOR
        /// whose entrant has that color rewritten by another layer must
        /// escalate. A layer-5 `AddColor` washes the colorless entrant green
        /// before the layer-7c count applies, so a pre-layer probe would see a
        /// colorless entrant, keep the incremental arm, and leave pre-existing
        /// Bears at a stale 4/4 where the correct CR 613 board is 5/5.
        /// `modification_characteristic_writes` classifies the color writers as
        /// `CharacteristicKinds::COLOR` and the counted filter reads the same
        /// kind, so the sets intersect, the gate escalates and the two boards
        /// agree.
        ///
        /// This is the discriminating fixture for that channel: revert the
        /// `COLOR` arm of the classifier and the escalation assertion fails; keep
        /// the arm but break the escalation plumbing and the identity assertion
        /// fails on the Bears' derived power/toughness.
        #[test]
        fn color_change_entry_escalates_when_population_is_color_keyed() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(green_count_anthem_with_color_wash_board, |s| {
                    add_colorless_creature_entry(s, 291)
                });
            assert!(
                escalated,
                "a layer-5 color wash reaching the entrant moves a color-keyed \
                 count — the entry must escalate to a full re-evaluation"
            );
            let bear_pts = |state: &GameState| {
                let mut pts: Vec<(Option<i32>, Option<i32>)> = state
                    .battlefield
                    .iter()
                    .filter_map(|id| state.objects.get(id))
                    .filter(|o| o.name.starts_with("WashBear"))
                    .map(|o| (o.power, o.toughness))
                    .collect();
                pts.sort();
                pts
            };
            // The washed entrant is green by the time the count applies, so the
            // count is 3, not the pre-layer 2.
            assert_eq!(
                bear_pts(&forced),
                vec![(Some(5), Some(5)); 2],
                "full pass counts the washed entrant — the correct CR 613 board"
            );
            assert_eq!(
                bear_pts(&normal),
                bear_pts(&forced),
                "escalated entry must derive the same board as a full re-evaluation"
            );
            assert_pt_identical(&normal, &forced, "color-keyed population escalation");
        }

        /// Build a board pairing a PURE layer-4 type-writer with a
        /// condition-gated fixed anthem — the CONDITION-channel analogue of the
        /// Ashaya CDA regression. Source A: "creatures you control are lands in
        /// addition to their other types" (no dynamic magnitude, no
        /// population-sensitive affected set, no entry replacement). Source B:
        /// "creatures you control get +2/+2 as long as you control four or more
        /// lands" (`QuantityComparison` over `ObjectCount(Land)` — the ONLY
        /// population read on the board, and it lives in a `condition`, not in
        /// any effect's magnitude or affected set). One pre-existing GateBear,
        /// two plain lands: pre-entry land count = 2 lands + 1 creature-as-land
        /// = 3, gate OFF.
        fn type_writer_with_condition_gated_anthem_board() -> GameState {
            use crate::types::ability::{
                Comparator, ContinuousModification, StaticCondition, StaticDefinition,
                TypeFilter as TF, TypedFilter as TFil,
            };
            use crate::types::statics::StaticMode;
            let mut state = setup();
            let bear = create_object(
                &mut state,
                CardId(300),
                PlayerId(0),
                "GateBear".to_string(),
                Zone::Battlefield,
            );
            {
                let o = state.objects.get_mut(&bear).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
            }
            for i in 0..2 {
                let land = create_object(
                    &mut state,
                    CardId(301 + i),
                    PlayerId(0),
                    format!("QuietLand{i}"),
                    Zone::Battlefield,
                );
                let o = state.objects.get_mut(&land).unwrap();
                o.base_card_types.core_types = vec![CoreType::Land];
                o.card_types.core_types = vec![CoreType::Land];
            }
            let type_writer = create_object(
                &mut state,
                CardId(310),
                PlayerId(0),
                "Creatures Are Lands".to_string(),
                Zone::Battlefield,
            );
            let mut writer_sd = StaticDefinition::new(StaticMode::Continuous);
            writer_sd.affected = Some(TargetFilter::Typed(TFil::new(TF::Creature)));
            writer_sd.modifications = vec![ContinuousModification::AddType {
                core_type: CoreType::Land,
            }];
            {
                let o = state.objects.get_mut(&type_writer).unwrap();
                o.base_static_definitions = Arc::new(vec![writer_sd.clone()]);
                o.static_definitions = vec![writer_sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
            }
            let anthem = create_object(
                &mut state,
                CardId(311),
                PlayerId(0),
                "Land Threshold Anthem".to_string(),
                Zone::Battlefield,
            );
            let mut anthem_sd = StaticDefinition::new(StaticMode::Continuous);
            anthem_sd.affected = Some(TargetFilter::Typed(TFil::new(TF::Creature)));
            anthem_sd.modifications = vec![
                ContinuousModification::AddPower { value: 2 },
                ContinuousModification::AddToughness { value: 2 },
            ];
            anthem_sd.condition = Some(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(TFil::new(TF::Land)),
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            });
            {
                let o = state.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![anthem_sd.clone()]);
                o.static_definitions = vec![anthem_sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
            }
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        /// (3c) CONDITION-channel blindness — perturbing only POST-layer:
        /// CR 611.3a + CR 613.1 + CR 613.1d. A plain creature entering is NOT a
        /// land at gate time, so neither Axis 2a (no population-reading effect
        /// magnitude or affected set is live) nor Axis 2b's pre-layer membership
        /// probe fires; but the type-writer makes the entrant a land in layer 4
        /// and the count crosses the anthem's threshold, changing PRE-EXISTING
        /// recipients. The blindness disjunct's condition channel
        /// (`static_condition_characteristic_reads`, unioned into `ReadKinds` by
        /// `live_characteristic_reads`) MUST escalate this entry, and the board
        /// must match a forced-Full pass (GateBear
        /// 4/4, not stale 2/2).
        #[test]
        fn condition_gated_anthem_entry_escalates_when_entrant_types_rewritten() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(type_writer_with_condition_gated_anthem_board, |s| {
                    add_colorless_creature_entry(s, 320)
                });
            assert!(
                escalated,
                "entrant becomes a land in layer 4 and flips the threshold gate — must escalate"
            );
            assert_pt_identical(
                &normal,
                &forced,
                "condition-channel type-rewrite escalation",
            );
            // Vacuity guard: `escalated` depends only on the classifier, so pin
            // that the threshold gate actually flips — GateBear ends the pass
            // at 4/4 (base 2/2 + the now-live +2/+2), not a stale 2/2.
            let gatebear_pt = |state: &GameState| {
                state
                    .battlefield
                    .iter()
                    .filter_map(|id| state.objects.get(id))
                    .find(|o| o.name == "GateBear")
                    .map(|o| (o.power, o.toughness))
                    .unwrap()
            };
            assert_eq!(
                gatebear_pt(&forced),
                (Some(4), Some(4)),
                "the entrant-turned-land crosses the GE-4 land threshold"
            );
        }

        /// Board for the CONTROLLER channel (CR 613.1b). P0 controls two 2/2s
        /// and an anthem whose dynamic magnitude counts "creatures you control",
        /// plus a theft enchantment whose layer-2 `ChangeController` claims
        /// every creature for the enchantment's controller. The theft's affected
        /// filter is deliberately controller-FREE: a controller-keyed affected
        /// filter would make layer 2 self-referential, and the point under test
        /// is the counted population, not the affected set.
        fn controller_keyed_count_anthem_with_control_theft_board() -> GameState {
            use crate::types::ability::{
                ContinuousModification, ControllerRef, StaticDefinition, TypeFilter as TF,
                TypedFilter as TFil,
            };
            use crate::types::statics::StaticMode;
            let mut state = setup();
            for i in 0..2 {
                let id = create_object(
                    &mut state,
                    CardId(380 + i),
                    PlayerId(0),
                    format!("TheftBear{i}"),
                    Zone::Battlefield,
                );
                let o = state.objects.get_mut(&id).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
            }
            let yours = TargetFilter::Typed(TFil {
                type_filters: vec![TF::Creature],
                controller: Some(ControllerRef::You),
                ..Default::default()
            });
            let anthem = create_object(
                &mut state,
                CardId(385),
                PlayerId(0),
                "Ally Count Anthem".to_string(),
                Zone::Battlefield,
            );
            let mut anthem_sd = StaticDefinition::new(StaticMode::Continuous);
            anthem_sd.affected = Some(TargetFilter::Typed(TFil::new(TF::Creature)));
            anthem_sd.modifications = vec![
                ContinuousModification::AddDynamicPower {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: yours.clone(),
                        },
                    },
                },
                ContinuousModification::AddDynamicToughness {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter: yours },
                    },
                },
            ];
            {
                let o = state.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![anthem_sd.clone()]);
                o.static_definitions = vec![anthem_sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
            }
            let thief = create_object(
                &mut state,
                CardId(386),
                PlayerId(0),
                "Mass Mind Control".to_string(),
                Zone::Battlefield,
            );
            let mut theft_sd = StaticDefinition::new(StaticMode::Continuous);
            theft_sd.affected = Some(TargetFilter::Typed(TFil::new(TF::Creature)));
            theft_sd.modifications = vec![ContinuousModification::ChangeController];
            {
                let o = state.objects.get_mut(&thief).unwrap();
                o.base_static_definitions = Arc::new(vec![theft_sd.clone()]);
                o.static_definitions = vec![theft_sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
            }
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        /// (3d) CONTROLLER-channel blindness — CR 613.1b + CR 613.1 + CR 109.3.
        /// The entrant arrives under the OPPONENT, so a pre-layer probe of
        /// "creatures you control" says the count is unperturbed; layer 2 then
        /// hands the entrant to the anthem's controller and the count goes
        /// 2 -> 3, which moves PRE-EXISTING recipients. CR 109.3 puts controller
        /// outside an object's characteristics, so this is not reachable by the
        /// card-type disjunct — `modification_characteristic_writes` has to
        /// classify `ChangeController` as a `CharacteristicKinds::CONTROLLER`
        /// write in its own right for this entry to escalate.
        #[test]
        fn controller_change_entry_escalates_when_population_is_controller_keyed() {
            let (normal, escalated, forced) = flush_entry_and_forced(
                controller_keyed_count_anthem_with_control_theft_board,
                |s| add_colorless_creature_entry_under(s, 390, PlayerId(1)),
            );
            assert!(
                escalated,
                "layer 2 moves the entrant into the counted population — must escalate"
            );
            assert_pt_identical(&normal, &forced, "controller-channel escalation");
            // Vacuity guard: `escalated` depends only on the classifier, so pin
            // that the stolen entrant really does move the count — TheftBears
            // end the pass at 5/5 (base 2/2 + three creatures now controlled),
            // not the pre-layer 4/4.
            let theftbear_pt = |state: &GameState| {
                state
                    .battlefield
                    .iter()
                    .filter_map(|id| state.objects.get(id))
                    .find(|o| o.name == "TheftBear0")
                    .map(|o| (o.power, o.toughness))
                    .unwrap()
            };
            assert_eq!(
                theftbear_pt(&forced),
                (Some(5), Some(5)),
                "the stolen entrant is counted among \"creatures you control\""
            );
        }

        /// (4) MEDIUM-2 — whole-board TALLY affected filter
        /// (`MostPrevalentCreatureTypeIn`). The anthem affects "creatures of the
        /// most prevalent creature type on the battlefield". A creature token
        /// entry whose own type is NOT the anthem's inner concern can STILL flip
        /// which type is most prevalent for PRE-EXISTING creatures, so the entry
        /// MUST escalate UNCONDITIONALLY (independent of any entered-object filter
        /// match) AND match a forced-Full board.
        /// Build a board whose anthem affects "creatures of the most prevalent
        /// creature type on the battlefield" — a whole-board TALLY affected
        /// filter (`MostPrevalentCreatureTypeIn`). Two pre-existing Bears.
        fn most_prevalent_anthem_board() -> GameState {
            use crate::types::ability::{ContinuousModification, FilterProp, StaticDefinition};
            use crate::types::statics::StaticMode;
            let mut base = setup();
            for i in 0..2 {
                let id = create_object(
                    &mut base,
                    CardId(260 + i),
                    PlayerId(0),
                    format!("TallyBear{i}"),
                    Zone::Battlefield,
                );
                let o = base.objects.get_mut(&id).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                o.base_card_types.subtypes = vec!["Bear".to_string()];
                o.card_types.subtypes = vec!["Bear".to_string()];
            }
            let anthem = create_object(
                &mut base,
                CardId(270),
                PlayerId(0),
                "Most Prevalent Anthem".to_string(),
                Zone::Battlefield,
            );
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                properties: vec![FilterProp::MostPrevalentCreatureTypeIn {
                    zone: crate::types::zones::Zone::Battlefield,
                    scope: crate::types::ability::ControllerRef::You,
                }],
                ..Default::default()
            }));
            sd.modifications = vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ];
            {
                let o = base.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
            }
            base.layers_dirty = crate::types::game_state::LayersDirty::Full;
            base
        }

        #[test]
        fn most_prevalent_tally_entry_escalates_unconditionally_and_matches_full() {
            // The entered creature is an "Insect" — a DIFFERENT creature type than
            // the pre-existing Bears, so it does NOT match the anthem's current
            // "most prevalent" membership (Bear), yet adding it changes the tally
            // and so must escalate UNCONDITIONALLY (MEDIUM-2).
            let add_insect = |s: &mut GameState| {
                let id = create_object(
                    s,
                    CardId(271),
                    PlayerId(0),
                    "Insect".to_string(),
                    Zone::Battlefield,
                );
                {
                    let o = s.objects.get_mut(&id).unwrap();
                    o.base_power = Some(1);
                    o.base_toughness = Some(1);
                    o.power = Some(1);
                    o.toughness = Some(1);
                    o.base_card_types.core_types = vec![CoreType::Creature];
                    o.card_types.core_types = vec![CoreType::Creature];
                    o.base_card_types.subtypes = vec!["Insect".to_string()];
                    o.card_types.subtypes = vec!["Insect".to_string()];
                }
                crate::game::layers::mark_layers_entered(s, id);
                id
            };
            let (normal, escalated, forced) =
                flush_entry_and_forced(most_prevalent_anthem_board, add_insect);
            assert!(
                escalated,
                "whole-board tally (MostPrevalentCreatureTypeIn) must escalate \
                 unconditionally on ANY creature entry"
            );
            assert_pt_identical(
                &normal,
                &forced,
                "most-prevalent tally unconditional escalation",
            );
        }

        // ====================================================================
        // Truth-delta short-circuit tests (CR 611.3a + CR 611.3b).
        //
        // A source-level (non-recipient-context) population-gated CONTINUOUS
        // static no longer escalates an incremental flush merely because an
        // entry perturbs its gate INPUT — it escalates only when the gate TRUTH
        // flips. Recipient-context gates, magnitude perturbation (Axis 2a), and
        // key-absent fail-closed all still escalate unconditionally.
        // ====================================================================

        /// Build a board with a SOURCE-LEVEL `Not(DevotionGE {Green, 7})`-gated
        /// anthem ("creatures you control get +1/+1 as long as your devotion to
        /// green is LESS than 7"). The gate is whole-effect on/off (consumed at
        /// collection, `condition: None` on the active effect) and NON-recipient-
        /// context (`condition_uses_recipient_context` is false for `DevotionGE`,
        /// recursed through `Not`). `green_symbols` green mana symbols on the
        /// anthem source set the controller's baseline devotion (CR 700.5), so the
        /// caller controls whether a green {G} entry crosses the threshold-7 edge.
        /// Two pre-existing green creatures are the anthem recipients.
        fn devotion_gated_anthem_board(green_symbols: usize) -> GameState {
            use crate::types::ability::{
                ContinuousModification, StaticCondition, StaticDefinition,
            };
            use crate::types::mana::{ManaCost, ManaCostShard};
            use crate::types::statics::StaticMode;
            let mut state = setup();
            for i in 0..2 {
                let id = create_object(
                    &mut state,
                    CardId(300 + i),
                    PlayerId(0),
                    format!("DevBear{i}"),
                    Zone::Battlefield,
                );
                let o = state.objects.get_mut(&id).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                o.base_color = vec![ManaColor::Green];
                o.color = vec![ManaColor::Green];
            }
            let anthem = create_object(
                &mut state,
                CardId(310),
                PlayerId(0),
                "Devotion-Gated Anthem".to_string(),
                Zone::Battlefield,
            );
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)));
            sd.modifications = vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ];
            // CR 700.5 + CR 611.3a: source-level gate "devotion to green < 7".
            sd.condition = Some(StaticCondition::Not {
                condition: Box::new(StaticCondition::DevotionGE {
                    colors: vec![ManaColor::Green],
                    threshold: 7,
                }),
            });
            let cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green; green_symbols],
                generic: 0,
            };
            {
                let o = state.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
                o.base_color = vec![ManaColor::Green];
                o.color = vec![ManaColor::Green];
                o.mana_cost = cost.clone();
                o.base_mana_cost = cost;
            }
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        /// Add a single green {G}-cost creature entry. Raises green devotion by
        /// exactly one mana symbol (CR 700.5).
        fn add_green_devotion_entry(state: &mut GameState, card_id: u64) -> ObjectId {
            use crate::types::mana::{ManaCost, ManaCostShard};
            let id = create_object(
                state,
                CardId(card_id),
                PlayerId(0),
                "Green Sprout".to_string(),
                Zone::Battlefield,
            );
            {
                let cost = ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 0,
                };
                let o = state.objects.get_mut(&id).unwrap();
                o.base_power = Some(1);
                o.base_toughness = Some(1);
                o.power = Some(1);
                o.toughness = Some(1);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                o.base_color = vec![ManaColor::Green];
                o.color = vec![ManaColor::Green];
                o.mana_cost = cost.clone();
                o.base_mana_cost = cost;
            }
            crate::game::layers::mark_layers_entered(state, id);
            id
        }

        /// (a) GATE-STAYS — the truth-delta short-circuit's discriminating case.
        /// Devotion is already 8 (>= 7), so `Not(DevotionGE 7)` is FALSE (gate
        /// OFF); a green {G} entry raises devotion to 9 — the gate INPUT is
        /// perturbed but its TRUTH stays FALSE. Under d9a40be71 the perturbation
        /// alone forced escalation; the truth-delta short-circuit must now skip
        /// it. `!escalated` FAILS under d9a40be71, PASSES after. `assert_pt_identical`
        /// confirms the incremental board (anthem off → base 2/2 recipients)
        /// matches a forced-full board.
        #[test]
        fn source_condition_gate_unchanged_does_not_escalate_and_matches_full() {
            let (normal, escalated, forced) = flush_entry_and_forced(
                || devotion_gated_anthem_board(8),
                |s| add_green_devotion_entry(s, 320),
            );
            assert!(
                !escalated,
                "green entry perturbs devotion but does not flip the < 7 gate \
                 (8 → 9, still >= 7) — truth-delta short-circuit must not escalate"
            );
            assert_pt_identical(&normal, &forced, "devotion gate unchanged non-escalation");
        }

        /// (b) GATE-FLIPS — baseline devotion 6 (< 7, gate ON, anthem applies
        /// +1/+1); a green {G} entry raises devotion to 7, flipping
        /// `Not(DevotionGE 7)` to FALSE (gate OFF). Every PRE-EXISTING recipient
        /// loses the buff, so the flush MUST escalate. `escalated` + match-full.
        #[test]
        fn source_condition_gate_flip_escalates_and_matches_full() {
            let (normal, escalated, forced) = flush_entry_and_forced(
                || devotion_gated_anthem_board(6),
                |s| add_green_devotion_entry(s, 321),
            );
            assert!(
                escalated,
                "green entry flips the < 7 gate (6 → 7) OFF — pre-existing \
                 recipients lose the anthem, must escalate"
            );
            assert_pt_identical(&normal, &forced, "devotion gate flip escalation");
        }

        /// Build a MULTI-AXIS anthem: BOTH a `Devotion`-backed magnitude (Axis 2a,
        /// population-sensitive) AND a source-level population-gated condition
        /// (`IsPresent(Creature)`, ON and stable). A green {G} entry perturbs the
        /// magnitude on PRE-EXISTING creatures, so Axis 2a must escalate FIRST —
        /// regardless of the condition's stable truth. Pins the multi-axis
        /// ordering (the truth-delta short-circuit must never suppress a magnitude
        /// perturbation).
        fn devotion_magnitude_and_condition_board() -> GameState {
            use crate::types::ability::{
                ContinuousModification, DevotionColors, StaticCondition, StaticDefinition,
            };
            use crate::types::statics::StaticMode;
            let mut state = setup();
            for i in 0..2 {
                let id = create_object(
                    &mut state,
                    CardId(330 + i),
                    PlayerId(0),
                    format!("MultiBear{i}"),
                    Zone::Battlefield,
                );
                let o = state.objects.get_mut(&id).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
                o.base_color = vec![ManaColor::Green];
                o.color = vec![ManaColor::Green];
            }
            let anthem = create_object(
                &mut state,
                CardId(340),
                PlayerId(0),
                "Multi-Axis Anthem".to_string(),
                Zone::Battlefield,
            );
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)));
            sd.modifications = vec![
                ContinuousModification::AddDynamicPower {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::Devotion {
                            colors: DevotionColors::Fixed(vec![ManaColor::Green]),
                        },
                    },
                },
                ContinuousModification::AddDynamicToughness {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::Devotion {
                            colors: DevotionColors::Fixed(vec![ManaColor::Green]),
                        },
                    },
                },
            ];
            // Source-level population gate, ON (creatures exist) and stable.
            sd.condition = Some(StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature))),
            });
            {
                let o = state.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
                o.base_color = vec![ManaColor::Green];
                o.color = vec![ManaColor::Green];
            }
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        /// (c) MULTI-AXIS — magnitude perturbation always escalates (Axis 2a),
        /// even though the source-level condition's truth is stable ON. A green
        /// {G} entry moves green devotion, changing the magnitude applied to
        /// PRE-EXISTING creatures; the truth-delta short-circuit must NOT
        /// suppress this. `escalated` + match-full.
        #[test]
        fn source_condition_and_magnitude_always_escalates() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(devotion_magnitude_and_condition_board, |s| {
                    add_green_devotion_entry(s, 341)
                });
            assert!(
                escalated,
                "magnitude (devotion) perturbation must escalate via Axis 2a \
                 regardless of the stable source-level condition truth"
            );
            assert_pt_identical(&normal, &forced, "multi-axis magnitude escalation");
        }

        /// Build a RECIPIENT-CONTEXT population-gated anthem (the BLOCKER's
        /// discriminating guard). The condition is
        /// `QuantityComparison { ObjectCount { Creature AND Another } GE 3 }` —
        /// "as long as there are at least 3 OTHER creatures". `FilterProp::Another`
        /// makes the count recipient-relative (`filter_uses_recipient` true), so
        /// the gate is RE-EVALUATED PER RECIPIENT (`evaluate_condition_with_recipient`
        /// threads `recipient` into the count, excluding that recipient) and
        /// `source_condition_gate_passes` only OVER-approximates it. It is also
        /// population-sensitive (`ObjectCount`). With 3 pre-existing creatures,
        /// each recipient sees 2 OTHERS (gate OFF). A 4th creature entry makes
        /// each PRE-EXISTING recipient see 3 others → its per-recipient gate flips
        /// ON. A single board-level boolean cannot summarize this, so a
        /// recipient-context gate must ALWAYS escalate (never short-circuit).
        /// Ships green-and-stale WITHOUT the recipient-context exclusion.
        fn recipient_context_count_anthem_board() -> GameState {
            use crate::types::ability::{
                Comparator, ContinuousModification, FilterProp, StaticCondition, StaticDefinition,
            };
            use crate::types::statics::StaticMode;
            let mut state = setup();
            // Three pre-existing creatures (recipients of the anthem).
            for i in 0..3 {
                let id = create_object(
                    &mut state,
                    CardId(350 + i),
                    PlayerId(0),
                    format!("CountBear{i}"),
                    Zone::Battlefield,
                );
                let o = state.objects.get_mut(&id).unwrap();
                o.base_power = Some(2);
                o.base_toughness = Some(2);
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_card_types.core_types = vec![CoreType::Creature];
                o.card_types.core_types = vec![CoreType::Creature];
            }
            let anthem = create_object(
                &mut state,
                CardId(360),
                PlayerId(0),
                "Other-Creatures Anthem".to_string(),
                Zone::Battlefield,
            );
            let mut sd = StaticDefinition::new(StaticMode::Continuous);
            sd.affected = Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)));
            sd.modifications = vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ];
            // Recipient-relative count: "creatures other than the recipient".
            let other_creatures = TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                properties: vec![FilterProp::Another],
                ..Default::default()
            });
            sd.condition = Some(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: other_creatures,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            });
            {
                let o = state.objects.get_mut(&anthem).unwrap();
                o.base_static_definitions = Arc::new(vec![sd.clone()]);
                o.static_definitions = vec![sd].into();
                o.base_card_types.core_types = vec![CoreType::Enchantment];
                o.card_types.core_types = vec![CoreType::Enchantment];
            }
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        /// (d) RECIPIENT-CONTEXT (BLOCKER guard) — a population-gated condition
        /// whose truth is PER-RECIPIENT must always escalate when perturbed, even
        /// though `source_condition_gate_passes` would report a single, possibly-
        /// unchanged board-level value. A 4th creature flips each pre-existing
        /// recipient's "at least 3 other creatures" gate ON, so escalation is
        /// mandatory. `escalated` + match-full. This ships green-and-stale WITHOUT
        /// the recipient-context exclusion (the discriminating BLOCKER guard).
        #[test]
        fn recipient_context_population_condition_always_escalates_and_matches_full() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(recipient_context_count_anthem_board, |s| {
                    add_colorless_creature_entry(s, 361)
                });
            assert!(
                escalated,
                "recipient-context population gate re-evaluates per recipient — \
                 a threshold-edge creature entry flips pre-existing recipients' \
                 gates; must escalate unconditionally (never short-circuit)"
            );
            assert_pt_identical(
                &normal,
                &forced,
                "recipient-context unconditional escalation",
            );
        }

        /// (e) FAIL-CLOSED KEY-ABSENT — when a source-level population-gated
        /// static's key is ABSENT from `static_gate_truth` (e.g. the cache was
        /// never refreshed for it, or it was phased out at the last full eval),
        /// the consult must FAIL CLOSED and escalate. Prime the board, perturb,
        /// then clear the cache before consulting `incremental_flush_must_escalate`
        /// directly — the missing BEFORE truth forces a conservative full pass.
        #[test]
        fn absent_gate_key_escalates() {
            let mut state = devotion_gated_anthem_board(6);
            flush_layers(&mut state);
            // A green entry perturbs the < 7 gate (would flip 6 → 7).
            add_green_devotion_entry(&mut state, 322);
            let entered_ids: std::collections::BTreeSet<ObjectId> = match &state.layers_dirty {
                crate::types::game_state::LayersDirty::EnteredObjects(ids) => ids.clone(),
                other => panic!("expected EnteredObjects, got {other:?}"),
            };
            // Simulate a stale/absent cache: drop every recorded gate truth.
            state.static_gate_truth.clear();
            assert!(
                crate::game::layers::incremental_flush_must_escalate(&state, &entered_ids),
                "absent gate-truth key must fail closed and escalate (invariant 1)"
            );
        }

        // ------------------------------------------------------------------
        // Read/write-kind relation fixtures (CR 613.1).
        //
        // Each board mirrors the color-wash fixture's shape: one enchantment
        // carrying TWO Continuous static definitions, a vanilla entrant (so
        // `entered_object_blocks_incremental` stays quiet), and a divergence
        // that surfaces in power/toughness (all `assert_pt_identical` compares).
        //
        // Non-vacuity invariants, checked per fixture: the entrant must NOT
        // satisfy the population-sensitive read PRE-layer (otherwise Axis 2a
        // escalates and the kind relation goes untested), it must satisfy it
        // POST-layer, and the writer's layer must run strictly before the
        // reading layer.
        // ------------------------------------------------------------------

        /// Install a battlefield enchantment carrying `defs` as both its base
        /// and its live static definitions, matching how the pre-existing
        /// escalation boards install anthems.
        fn install_static_enchantment(
            state: &mut GameState,
            card_id: u64,
            name: &str,
            defs: Vec<crate::types::ability::StaticDefinition>,
        ) -> ObjectId {
            let id = create_object(
                state,
                CardId(card_id),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            let o = state.objects.get_mut(&id).unwrap();
            o.base_static_definitions = Arc::new(defs.clone());
            o.static_definitions = defs.into();
            o.base_card_types.core_types = vec![CoreType::Enchantment];
            o.card_types.core_types = vec![CoreType::Enchantment];
            id
        }

        /// Create a vanilla 2/2 creature with an explicit name and color set.
        fn add_relation_bear(
            state: &mut GameState,
            card_id: u64,
            name: &str,
            colors: Vec<ManaColor>,
        ) -> ObjectId {
            let id = create_object(
                state,
                CardId(card_id),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            let o = state.objects.get_mut(&id).unwrap();
            o.base_power = Some(2);
            o.base_toughness = Some(2);
            o.power = Some(2);
            o.toughness = Some(2);
            o.base_card_types.core_types = vec![CoreType::Creature];
            o.card_types.core_types = vec![CoreType::Creature];
            o.base_color.clone_from(&colors);
            o.color = colors;
            id
        }

        /// Sorted `(power, toughness)` of every battlefield object whose name
        /// starts with `prefix`.
        fn pts_named(state: &GameState, prefix: &str) -> Vec<(Option<i32>, Option<i32>)> {
            let mut pts: Vec<(Option<i32>, Option<i32>)> = state
                .battlefield
                .iter()
                .filter_map(|id| state.objects.get(id))
                .filter(|o| o.name.starts_with(prefix))
                .map(|o| (o.power, o.toughness))
                .collect();
            pts.sort();
            pts
        }

        /// `pts_named`'s twin for boards where a layer-1 `SetName` override
        /// (CR 707.9b) rewrites the live name: selects on the PRINTED name,
        /// which `reset_recipient_to_base` restores at the top of every pass.
        fn pts_base_named(state: &GameState, prefix: &str) -> Vec<(Option<i32>, Option<i32>)> {
            let mut pts: Vec<(Option<i32>, Option<i32>)> = state
                .battlefield
                .iter()
                .filter_map(|id| state.objects.get(id))
                .filter(|o| o.base_name.starts_with(prefix))
                .map(|o| (o.power, o.toughness))
                .collect();
            pts.sort();
            pts
        }

        /// A `Continuous` static definition over `affected` applying `mods`.
        fn continuous_static(
            affected: TargetFilter,
            mods: Vec<crate::types::ability::ContinuousModification>,
        ) -> crate::types::ability::StaticDefinition {
            let mut def = crate::types::ability::StaticDefinition::new(
                crate::types::statics::StaticMode::Continuous,
            );
            def.affected = Some(affected);
            def.modifications = mods;
            def
        }

        /// `AddDynamicPower` + `AddDynamicToughness` off one `ObjectCount`.
        fn dynamic_pt_count(
            counted: TargetFilter,
        ) -> Vec<crate::types::ability::ContinuousModification> {
            use crate::types::ability::ContinuousModification;
            let count = QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter: counted },
            };
            vec![
                ContinuousModification::AddDynamicPower {
                    value: count.clone(),
                },
                ContinuousModification::AddDynamicToughness { value: count },
            ]
        }

        /// (2.1) KEYWORD channel (CR 613.1f). A layer-6 `AddKeyword` reaches the
        /// entrant while the anthem's magnitude counts creatures WITH that
        /// keyword. Pre-layer the entrant has no flying, so every membership
        /// probe reports "no perturbation"; post-layer it does, so the count
        /// moves 2 → 3 and the pre-existing FlyBears go 4/4 → 5/5.
        ///
        /// Discriminating for BOTH halves of the relation: revert the
        /// `AddKeyword` family to EMPTY on the write side, or the `WithKeyword`
        /// family to EMPTY on the read side, and the escalation assertion fails.
        fn flying_count_anthem_with_keyword_grant_board() -> GameState {
            use crate::types::ability::ContinuousModification;
            use crate::types::Keyword;
            let mut state = setup();
            for i in 0..2 {
                add_relation_bear(&mut state, 600 + i, &format!("FlyBear{i}"), vec![]);
            }
            let grant = continuous_static(
                TargetFilter::Typed(TypedFilter::creature()),
                vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Flying,
                }],
            );
            let flying_creatures = TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                properties: vec![FilterProp::WithKeyword {
                    value: Keyword::Flying,
                }],
                ..Default::default()
            });
            let count = continuous_static(
                TargetFilter::Typed(TypedFilter::creature()),
                dynamic_pt_count(flying_creatures),
            );
            install_static_enchantment(&mut state, 610, "Flying Count Anthem", vec![grant, count]);
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        #[test]
        fn keyword_grant_entry_escalates_when_population_is_keyword_keyed() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(flying_count_anthem_with_keyword_grant_board, |s| {
                    add_colorless_creature_entry(s, 611)
                });
            assert!(
                escalated,
                "a layer-6 keyword grant reaching the entrant moves a keyword-keyed \
                 count — the entry must escalate to a full re-evaluation"
            );
            assert_eq!(
                pts_named(&forced, "FlyBear"),
                vec![(Some(5), Some(5)); 2],
                "full pass counts the entrant once it has flying — correct CR 613 board"
            );
            assert_eq!(
                pts_named(&normal, "FlyBear"),
                pts_named(&forced, "FlyBear"),
                "escalated entry must derive the same board as full re-evaluation"
            );
            assert_pt_identical(&normal, &forced, "keyword-keyed escalation");
        }

        /// (2.2) POWER/TOUGHNESS channel (CR 613.1g + CR 613.4b/c). A layer-7b
        /// `SetToughness` reaches the entrant while the anthem's magnitude counts
        /// creatures with toughness ≥ 4 at layer 7c.
        ///
        /// Because power/toughness is ONE kind, a P/T-keyed count anthem is
        /// itself a P/T writer and would satisfy the relation on its own reach.
        /// The count anthem's affected set is therefore "creatures you control"
        /// while the entrant enters under the OPPONENT (mirroring
        /// `controller_keyed_count_anthem_with_control_theft_board`), which makes
        /// `SetToughness`
        /// the only entrant-reaching writer and gives the revert-check SetPT-arm
        /// granularity rather than whole-kind granularity.
        fn tough_count_anthem_with_set_toughness_board() -> GameState {
            use crate::types::ability::{ContinuousModification, PtStat, PtValueScope};
            use crate::types::ControllerRef;
            let mut state = setup();
            for i in 0..2 {
                add_relation_bear(&mut state, 620 + i, &format!("ToughBear{i}"), vec![]);
            }
            // Layer 7b, controller-agnostic: reaches the opponent's entrant too.
            let setter = continuous_static(
                TargetFilter::Typed(TypedFilter::creature()),
                vec![ContinuousModification::SetToughness { value: 4 }],
            );
            let tough_creatures = TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                properties: vec![FilterProp::PtComparison {
                    stat: PtStat::Toughness,
                    scope: PtValueScope::Current,
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: 4 },
                }],
                ..Default::default()
            });
            // Layer 7c, "creatures you control": deliberately EXCLUDES the
            // opponent-controlled entrant.
            let count = continuous_static(
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: Some(ControllerRef::You),
                    ..Default::default()
                }),
                vec![ContinuousModification::AddDynamicPower {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: tough_creatures,
                        },
                    },
                }],
            );
            install_static_enchantment(
                &mut state,
                630,
                "Toughness Count Anthem",
                vec![setter, count],
            );
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        #[test]
        fn pt_change_entry_escalates_when_population_is_pt_keyed() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(tough_count_anthem_with_set_toughness_board, |s| {
                    add_colorless_creature_entry_under(s, 631, PlayerId(1))
                });
            assert!(
                escalated,
                "a layer-7b toughness set reaching the entrant moves a P/T-keyed \
                 count — the entry must escalate to a full re-evaluation"
            );
            assert_eq!(
                pts_named(&forced, "ToughBear"),
                vec![(Some(5), Some(4)); 2],
                "full pass counts the entrant once its toughness is set to 4"
            );
            assert_eq!(
                pts_named(&normal, "ToughBear"),
                pts_named(&forced, "ToughBear"),
                "escalated entry must derive the same board as full re-evaluation"
            );
            assert_pt_identical(&normal, &forced, "P/T-keyed escalation");
        }

        /// (2.3) NAME channel (CR 613.1c + CR 612.8). A layer-3 `SetTextName`
        /// reaches the entrant while the anthem's magnitude counts creatures by
        /// name. Pre-layer the entrant is "Insect" and matches nothing;
        /// post-layer it is a third "Doppelganger" and the pre-existing pair
        /// goes 4/4 → 5/5.
        fn named_count_anthem_with_name_rewrite_board() -> GameState {
            use crate::types::ability::ContinuousModification;
            let mut state = setup();
            for i in 0..2 {
                add_relation_bear(&mut state, 640 + i, "Doppelganger", vec![]);
            }
            let rename = continuous_static(
                TargetFilter::Typed(TypedFilter::creature()),
                vec![ContinuousModification::SetTextName {
                    name: "Doppelganger".to_string(),
                }],
            );
            let doppelgangers = TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                properties: vec![FilterProp::Named {
                    name: "Doppelganger".to_string(),
                }],
                ..Default::default()
            });
            let count = continuous_static(
                TargetFilter::Typed(TypedFilter::creature()),
                dynamic_pt_count(doppelgangers),
            );
            install_static_enchantment(
                &mut state,
                650,
                "Doppelganger Count Anthem",
                vec![rename, count],
            );
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        #[test]
        fn name_change_entry_escalates_when_population_is_name_keyed() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(named_count_anthem_with_name_rewrite_board, |s| {
                    add_colorless_creature_entry(s, 651)
                });
            assert!(
                escalated,
                "a layer-3 name rewrite reaching the entrant moves a name-keyed \
                 count — the entry must escalate to a full re-evaluation"
            );
            // The two pre-existing Doppelgangers are the only 2/2 printed bodies;
            // the entrant is a 1/1, so it cannot be confused with them.
            let bears = |s: &GameState| {
                let mut pts: Vec<(Option<i32>, Option<i32>)> = s
                    .battlefield
                    .iter()
                    .filter_map(|id| s.objects.get(id))
                    .filter(|o| o.base_power == Some(2) && o.base_toughness == Some(2))
                    .map(|o| (o.power, o.toughness))
                    .collect();
                pts.sort();
                pts
            };
            assert_eq!(
                bears(&forced),
                vec![(Some(5), Some(5)); 2],
                "full pass counts the renamed entrant — correct CR 613 board"
            );
            assert_eq!(
                bears(&normal),
                bears(&forced),
                "escalated entry must derive the same board as full re-evaluation"
            );
            assert_pt_identical(&normal, &forced, "name-keyed escalation");
        }

        /// (2.4) NARROWING NEGATIVE. A layer-6 keyword grant reaches the entrant,
        /// but nothing live READS abilities: the only population read is an
        /// artifact count (CR 613.1d) and both affected filters are plain
        /// typelines. `{Abilities, PowerToughness} ∩ {CardTypes} = ∅`, so the
        /// entry must stay on the incremental fast path — a board the previous
        /// one-sided gate had no way to keep there, since it escalated on any
        /// recognized writer plus any population read.
        ///
        /// Revert direction: make `AddKeyword` write `ALL` and the assertion
        /// flips, which is what proves the narrowing is the classifier's doing.
        fn artifact_count_anthem_with_keyword_grant_board() -> GameState {
            use crate::types::ability::ContinuousModification;
            use crate::types::Keyword;
            let mut state = setup();
            for i in 0..2 {
                add_relation_bear(&mut state, 660 + i, &format!("DisjointBear{i}"), vec![]);
            }
            // One pre-existing artifact so the counted population is non-empty.
            let relic = create_object(
                &mut state,
                CardId(662),
                PlayerId(0),
                "Relic".to_string(),
                Zone::Battlefield,
            );
            {
                let o = state.objects.get_mut(&relic).unwrap();
                o.base_card_types.core_types = vec![CoreType::Artifact];
                o.card_types.core_types = vec![CoreType::Artifact];
            }
            let grant = continuous_static(
                TargetFilter::Typed(TypedFilter::creature()),
                vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Flying,
                }],
            );
            let artifacts = TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Artifact],
                ..Default::default()
            });
            let count = continuous_static(
                TargetFilter::Typed(TypedFilter::creature()),
                dynamic_pt_count(artifacts),
            );
            install_static_enchantment(
                &mut state,
                670,
                "Artifact Count With Grant",
                vec![grant, count],
            );
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        #[test]
        fn keyword_grant_entry_stays_incremental_when_population_reads_are_disjoint() {
            use crate::types::Keyword;
            let (normal, escalated, forced) =
                flush_entry_and_forced(artifact_count_anthem_with_keyword_grant_board, |s| {
                    add_colorless_creature_entry(s, 671)
                });
            assert!(
                !escalated,
                "a keyword grant cannot move an artifact-keyed count — \
                 {{Abilities}} ∩ {{CardTypes}} = ∅, so the entry must stay incremental"
            );
            // Non-vacuity: the grant really does reach the entrant, so the
            // narrowing is the kind relation's doing and not a missed match.
            let entrant = normal
                .battlefield
                .iter()
                .filter_map(|id| normal.objects.get(id))
                .find(|o| o.name == "Insect")
                .expect("entrant on battlefield");
            assert!(
                entrant.has_keyword(&Keyword::Flying),
                "the entrant must actually be a recipient of the layer-6 grant"
            );
            assert_pt_identical(&normal, &forced, "disjoint-kind non-escalation");
        }

        /// (2.5) AFFECTED-FILTER READ CHANNEL. This board has NO dynamic
        /// magnitude and NO static condition — the ONLY name read on it lives in
        /// another modification's AFFECTED FILTER. A layer-3 `SetTextName`
        /// renames the entering artifact to "Doppelganger", which adds that name
        /// to the reference set of the buff's `DifferentNameFrom` filter and
        /// therefore REMOVES the pre-existing Doppelganger creature from the
        /// buff's affected set (5/5 → 2/2).
        ///
        /// Pre-layer the entering Treasure does not match the reference filter
        /// (which is keyed on the name it does not yet have), so Axis 2a's
        /// per-entrant narrowing reports "no perturbation" and only the kind
        /// relation can catch this. Revert direction: drop the affected-filter
        /// channel from `live_characteristic_reads` and `ReadKinds` becomes
        /// empty, so the gate exits at stage 2 and the board goes stale.
        fn name_rewrite_with_affected_filter_read_board() -> GameState {
            use crate::types::ability::ContinuousModification;
            let mut state = setup();
            add_relation_bear(&mut state, 680, "Doppelganger", vec![]);
            let rename = continuous_static(
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Artifact],
                    ..Default::default()
                }),
                vec![ContinuousModification::SetTextName {
                    name: "Doppelganger".to_string(),
                }],
            );
            // "each artifact you control named Doppelganger" — the entrant only
            // joins this set AFTER layer 3 renames it.
            let named_artifacts = TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Artifact],
                properties: vec![FilterProp::Named {
                    name: "Doppelganger".to_string(),
                }],
                ..Default::default()
            });
            let buff = continuous_static(
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    properties: vec![FilterProp::DifferentNameFrom {
                        filter: Box::new(named_artifacts),
                    }],
                    ..Default::default()
                }),
                vec![
                    ContinuousModification::AddPower { value: 3 },
                    ContinuousModification::AddToughness { value: 3 },
                ],
            );
            install_static_enchantment(&mut state, 690, "Different Name Buff", vec![rename, buff]);
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        #[test]
        fn name_rewrite_entry_escalates_through_affected_filter_reads() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(name_rewrite_with_affected_filter_read_board, |s| {
                    add_artifact_entry(s, 691)
                });
            assert!(
                escalated,
                "a layer-3 rename feeding another static's AFFECTED FILTER must \
                 escalate — the affected-filter read channel is unconditional"
            );
            let bear = |s: &GameState| {
                s.battlefield
                    .iter()
                    .filter_map(|id| s.objects.get(id))
                    .find(|o| o.base_power == Some(2))
                    .map(|o| (o.power, o.toughness))
                    .expect("pre-existing Doppelganger on battlefield")
            };
            assert_eq!(
                bear(&forced),
                (Some(2), Some(2)),
                "full pass drops the pre-existing Doppelganger out of the buff \
                 once the renamed artifact joins the reference set"
            );
            assert_eq!(
                bear(&normal),
                bear(&forced),
                "escalated entry must derive the same board as full re-evaluation"
            );
            assert_pt_identical(&normal, &forced, "affected-filter read channel");
        }

        /// (2.6) CONDITION READ CHANNEL through the walker's NET-NEW recursion.
        /// The buff is gated by `RecipientMatchesFilter` over a keyword filter —
        /// a condition whose MEMBERSHIP twin
        /// (`static_condition_uses_object_population`) answers `false`, so Axis
        /// 2b never looks at it. Only `static_condition_characteristic_reads`
        /// recursing into that filter puts `Abilities` into `ReadKinds`, which is
        /// what makes the layer-6 grant reaching the entrant intersect.
        ///
        /// The DISCRIMINATING assertion here is the escalation bit: revert the
        /// `SourceMatchesFilter` / `RecipientMatchesFilter` arms to EMPTY and it
        /// flips. The identity assertion is the usual under-escalation tripwire,
        /// not an independent proof of divergence.
        fn condition_keyed_buff_with_keyword_grant_board() -> GameState {
            use crate::types::ability::ContinuousModification;
            use crate::types::{Keyword, StaticCondition};
            let mut state = setup();
            for i in 0..2 {
                add_relation_bear(&mut state, 700 + i, &format!("CondBear{i}"), vec![]);
            }
            let grant = continuous_static(
                TargetFilter::Typed(TypedFilter::creature()),
                vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Flying,
                }],
            );
            let mut buff = continuous_static(
                TargetFilter::Typed(TypedFilter::creature()),
                vec![
                    ContinuousModification::AddPower { value: 3 },
                    ContinuousModification::AddToughness { value: 3 },
                ],
            );
            buff.condition = Some(StaticCondition::RecipientMatchesFilter {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    properties: vec![FilterProp::WithKeyword {
                        value: Keyword::Flying,
                    }],
                    ..Default::default()
                }),
            });
            install_static_enchantment(&mut state, 710, "Flying-Gated Buff", vec![grant, buff]);
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        #[test]
        fn keyword_grant_entry_escalates_through_condition_filter_reads() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(condition_keyed_buff_with_keyword_grant_board, |s| {
                    add_colorless_creature_entry(s, 711)
                });
            assert!(
                escalated,
                "a live condition reading keywords through its own filter must put \
                 Abilities in ReadKinds, so the layer-6 grant intersects and escalates"
            );
            assert_eq!(
                pts_named(&forced, "CondBear"),
                vec![(Some(5), Some(5)); 2],
                "the granted flying satisfies the recipient condition on the full pass"
            );
            assert_pt_identical(&normal, &forced, "condition-filter read channel");
        }

        /// (2.7) CONDITION-ADJACENT NARROWING NEGATIVE. `ChangeController` is the
        /// writer the PREVIOUS gate recognized (it was one of its three
        /// population keys), and it genuinely reaches the entrant here. Every
        /// live read on this board is keyed purely on power/toughness — the
        /// counted filter and both affected filters use a bare `PtComparison`
        /// with no type constraint and no controller scope — so
        /// `{Controller} ∩ {PowerToughness} = ∅` and the control theft cannot
        /// move anything. The old gate escalated this board; the relation keeps
        /// it incremental.
        fn pt_keyed_count_anthem_with_control_theft_board() -> GameState {
            use crate::types::ability::{ContinuousModification, PtStat, PtValueScope};
            let mut state = setup();
            for i in 0..2 {
                add_relation_bear(&mut state, 720 + i, &format!("ScopeBear{i}"), vec![]);
            }
            // Layer 2: steals every object with power ≤ 1, i.e. the 1/1 entrant.
            let thief = continuous_static(
                TargetFilter::Typed(TypedFilter {
                    properties: vec![FilterProp::PtComparison {
                        stat: PtStat::Power,
                        scope: PtValueScope::Current,
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 1 },
                    }],
                    ..Default::default()
                }),
                vec![ContinuousModification::ChangeController],
            );
            let tough_objects = TargetFilter::Typed(TypedFilter {
                properties: vec![FilterProp::PtComparison {
                    stat: PtStat::Toughness,
                    scope: PtValueScope::Current,
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: 2 },
                }],
                ..Default::default()
            });
            let count = continuous_static(
                tough_objects.clone(),
                vec![ContinuousModification::AddDynamicPower {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: tough_objects,
                        },
                    },
                }],
            );
            install_static_enchantment(
                &mut state,
                730,
                "Toughness Count With Theft",
                vec![thief, count],
            );
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        #[test]
        fn control_change_entry_stays_incremental_when_reads_are_pt_only() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(pt_keyed_count_anthem_with_control_theft_board, |s| {
                    add_colorless_creature_entry_under(s, 731, PlayerId(1))
                });
            assert!(
                !escalated,
                "control theft cannot move a purely P/T-keyed population — \
                 {{Controller}} ∩ {{PowerToughness}} = ∅, so the entry stays incremental"
            );
            // Non-vacuity: the layer-2 writer really does reach the entrant, so
            // the old one-sided gate would have escalated this exact board.
            let entrant = normal
                .battlefield
                .iter()
                .filter_map(|id| normal.objects.get(id))
                .find(|o| o.name == "Insect")
                .expect("entrant on battlefield");
            assert_eq!(
                entrant.controller,
                PlayerId(0),
                "the entrant must actually be a recipient of the layer-2 control change"
            );
            assert_pt_identical(&normal, &forced, "controller-vs-P/T non-escalation");
        }

        /// (2.8) CR 613.6 SELF-EXCLUSION CARVE-OUT. One Continuous definition
        /// whose modifications WRITE exactly the kind its OWN affected filter
        /// READS, and nothing else on the board reads anything: the buff is
        /// `AddPower`/`AddToughness` (writes `{PowerToughness}`) over "creatures
        /// with power ≤ 1" (reads `{CardTypes, PowerToughness}`). There is no
        /// dynamic magnitude and no static condition, so the affected filter is
        /// the whole read union.
        ///
        /// CR 613.6 locks the effect's affected-object set the first time the
        /// effect applies and retains it for the rest of the pass, so the buff
        /// cannot push the entrant back out of the set it was just admitted to.
        /// Its own write is therefore not a read it can move, and the entry must
        /// stay incremental.
        ///
        /// Revert direction: drop the per-modification exclusion and test stage 4
        /// against the whole `ReadKinds` union again — `{PowerToughness}` then
        /// intersects its own affected filter's reads and the escalation
        /// assertion flips.
        fn self_reading_pt_buff_board() -> GameState {
            use crate::types::ability::{ContinuousModification, PtStat, PtValueScope};
            let mut state = setup();
            for i in 0..2 {
                add_relation_bear(&mut state, 740 + i, &format!("SelfBear{i}"), vec![]);
            }
            // Layer 7c. Pre-existing 2/2 bears are out of the set (power 2 > 1);
            // the 1/1 entrant is in it, and stays in it after the +3/+3 that
            // would otherwise disqualify it.
            let buff = continuous_static(
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    properties: vec![FilterProp::PtComparison {
                        stat: PtStat::Power,
                        scope: PtValueScope::Current,
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 1 },
                    }],
                    ..Default::default()
                }),
                vec![
                    ContinuousModification::AddPower { value: 3 },
                    ContinuousModification::AddToughness { value: 3 },
                ],
            );
            install_static_enchantment(&mut state, 742, "Self-Reading Buff", vec![buff]);
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        #[test]
        fn pt_writer_entry_stays_incremental_when_only_its_own_affected_filter_reads() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(self_reading_pt_buff_board, |s| {
                    add_colorless_creature_entry(s, 743)
                });
            assert!(
                !escalated,
                "CR 613.6 retains the effect's own affected set, so a modification \
                 cannot move the filter that admitted it — the entry stays incremental"
            );
            let entrant = |s: &GameState| {
                s.battlefield
                    .iter()
                    .filter_map(|id| s.objects.get(id))
                    .find(|o| o.name == "Insect")
                    .map(|o| (o.power, o.toughness))
                    .expect("entrant on battlefield")
            };
            // Non-vacuity: the P/T writer genuinely reaches the entrant, and the
            // retained set keeps `AddToughness` applying even though `AddPower`
            // already pushed current power past the filter's threshold.
            assert_eq!(
                entrant(&forced),
                (Some(4), Some(4)),
                "the entrant must actually be a recipient of the layer-7c buff"
            );
            assert_eq!(
                pts_named(&forced, "SelfBear"),
                vec![(Some(2), Some(2)); 2],
                "pre-existing 2/2 bears never satisfy the power ≤ 1 filter"
            );
            assert_eq!(
                entrant(&normal),
                entrant(&forced),
                "the incremental entry must derive the same board as a full re-evaluation"
            );
            assert_pt_identical(&normal, &forced, "CR 613.6 self-exclusion carve-out");
        }

        // ------------------------------------------------------------------
        // RESOLUTION-CREATED continuous effects (CR 611.2b + CR 611.2c).
        //
        // A `TransientContinuousEffect` is the third `ActiveContinuousEffect`
        // producer, alongside printed statics and granted-inner statics. It is
        // the only one whose affected set is FROZEN (CR 611.2c) while its two
        // gates stay LIVE, so it is the only one where the affected-filter
        // channel reports EMPTY and a gate is the sole live read. Those gates
        // are the "for as long as" DURATION (CR 611.2b) and the retained
        // CONDITION, which is the source definition's own CR 611.3a gate
        // carried along; `transient_effect_is_live` consults exactly that pair.
        // Every board below builds its effect through the single construction
        // authority — see `install_gated_transient` for which production path
        // produces this shape and which does NOT.
        // ------------------------------------------------------------------

        /// Install a resolution-created continuous effect that RETAINS a gate,
        /// through the single construction authority
        /// (`GameState::add_transient_continuous_effect`), one effect per
        /// recipient.
        ///
        /// CR 611.2c: the affected set is already frozen to `SpecificObject` —
        /// a filter that reads NO layer-writable characteristic — which is what
        /// every transient looks like once its effect has begun. The gate rides
        /// alongside and stays live: a `Duration::ForAsLongAs` re-evaluates per
        /// pass because CR 611.2b makes the effect last exactly as long as its
        /// stated condition holds, and a retained `condition` re-evaluates
        /// because it is the source `StaticDefinition`'s own CR 611.3a gate.
        /// That asymmetry (frozen set, live gate) is what these boards
        /// exercise.
        ///
        /// REACHABILITY: `Effect::GenericEffect` is NOT the producer of the
        /// retained-`condition` shape. Per CR 608.2h + CR 611.2d its resolver
        /// determines an in-effect "if <condition>" exactly once, at
        /// resolution, and installs the transient with `condition: None`
        /// (`effects/effect.rs::resolve`). A conditioned transient comes from
        /// riders that hand a `StaticDefinition`'s condition straight to the
        /// constructor — `effects/counter.rs::apply_source_static` (the
        /// `CounterSourceRider::LosesAbilities` rider) is the live example, and
        /// no shipped card gives that rider a condition yet, so the
        /// `condition`-gated boards below are preventive. The
        /// `Duration::ForAsLongAs` shape needs no such caveat: it is what the
        /// parser emits for any "for as long as …" clause it can read
        /// (`parser/oracle_nom/duration.rs`), and gain-control, phasing and
        /// copy effects install it today.
        fn install_gated_transient(
            state: &mut GameState,
            source: ObjectId,
            recipients: &[ObjectId],
            mods: Vec<crate::types::ability::ContinuousModification>,
            duration: Duration,
            condition: Option<crate::types::ability::StaticCondition>,
        ) {
            for &id in recipients {
                state.add_transient_continuous_effect(
                    source,
                    PlayerId(0),
                    duration.clone(),
                    TargetFilter::SpecificObject { id },
                    mods.clone(),
                    condition.clone(),
                );
            }
        }

        /// (3.1) TRANSIENT CONDITION READ CHANNEL. The only live read of card
        /// types on this board lives in the CONDITION of a resolution-created
        /// continuous effect whose affected set is already frozen to
        /// `SpecificObject` (CR 611.2c), i.e. to an EMPTY-read filter. A
        /// separate printed layer-4 `AddType{Land}` (CR 613.1d) reaches the
        /// entrant, so the Land population that condition counts moves for
        /// every pre-existing recipient.
        ///
        /// CR 611.2c freezes the affected SET and nothing else; the retained
        /// gate is the source definition's own CR 611.3a condition, so it keeps
        /// re-evaluating on every pass, here per recipient
        /// (`FilterProp::Another`). Pre-entry each 2/2 recipient sees exactly
        /// 1 OTHER Land so `GE 2` is OFF; post-entry it sees 2 and turns ON
        /// (3/3).
        ///
        /// DISCRIMINATING because the transient is the ONLY condition on the
        /// board: drop the `e.condition` channel from `live_characteristic_reads`
        /// and `ReadKinds` loses CardTypes entirely. Stage 4 then exempts the
        /// layer-4 writer under CR 613.6 — the only other CardTypes read is its
        /// OWN affected filter — the entry stays on the incremental path, and
        /// the pre-existing recipients keep a stale 2/2. The entry-perturbation
        /// probe cannot rescue it: the entrant is not a Land until layer 4 has
        /// run, so `entered_object_perturbs_static_condition` sees no
        /// perturbation. The recipient-context gate is what keeps this board on
        /// the `e.condition` channel — `transient_source_level_condition_read_board`
        /// covers the source-level twin, which that channel never sees.
        fn transient_condition_read_board() -> GameState {
            use crate::types::ability::{ContinuousModification, StaticCondition};
            let mut state = setup();
            let mut bears = Vec::new();
            for i in 0..2 {
                bears.push(add_relation_bear(
                    &mut state,
                    760 + i,
                    &format!("TransientBear{i}"),
                    vec![],
                ));
            }
            // CR 613.1d: printed layer-4 type-changer, unconditional. Every
            // creature is also a Land, so it reaches the entrant and MOVES the
            // counted population.
            let to_land = continuous_static(
                TargetFilter::Typed(TypedFilter::creature()),
                vec![ContinuousModification::AddType {
                    core_type: CoreType::Land,
                }],
            );
            install_static_enchantment(&mut state, 762, "Land Conversion", vec![to_land]);
            let source = install_static_enchantment(&mut state, 763, "Other-Lands Grant", vec![]);
            install_gated_transient(
                &mut state,
                source,
                &bears,
                vec![
                    ContinuousModification::AddPower { value: 1 },
                    ContinuousModification::AddToughness { value: 1 },
                ],
                Duration::UntilEndOfTurn,
                // Recipient-relative: "two or more OTHER Lands".
                Some(StaticCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(TypedFilter {
                                type_filters: vec![TypeFilter::Land],
                                properties: vec![FilterProp::Another],
                                ..Default::default()
                            }),
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 2 },
                }),
            );
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        #[test]
        fn type_rewrite_entry_escalates_through_transient_condition_reads() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(transient_condition_read_board, |s| {
                    add_colorless_creature_entry(s, 764)
                });
            assert!(
                escalated,
                "a resolution-created effect's retained condition is a LIVE read \
                 (CR 611.3a), so a layer-4 type rewrite reaching the entrant moves \
                 the population it counts — the entry must escalate"
            );
            // Non-vacuity: CR 611.2c really did freeze the affected set, so the
            // affected-filter channel reports EMPTY and the condition channel is
            // the only thing that can put CardTypes in ReadKinds.
            assert!(
                !forced.transient_continuous_effects.is_empty()
                    && forced.transient_continuous_effects.iter().all(|tce| {
                        matches!(tce.affected, TargetFilter::SpecificObject { .. })
                            && tce.condition.is_some()
                    }),
                "the grant must have resolved into SpecificObject-bound transients \
                 that still carry their condition"
            );
            // Non-vacuity: the gate is genuinely OFF before the entry, so the 3/3
            // below is the entrant's doing and not an already-buffed board.
            let mut pre = transient_condition_read_board();
            flush_layers(&mut pre);
            assert_eq!(
                pts_named(&pre, "TransientBear"),
                vec![(Some(2), Some(2)); 2],
                "pre-entry each recipient sees only 1 OTHER Land, so GE 2 is OFF"
            );
            assert_eq!(
                pts_named(&forced, "TransientBear"),
                vec![(Some(3), Some(3)); 2],
                "the full pass counts the entrant once layer 4 makes it a Land, so \
                 each recipient sees 2 OTHER Lands and the gate turns ON"
            );
            assert_eq!(
                pts_named(&normal, "TransientBear"),
                pts_named(&forced, "TransientBear"),
                "the escalated entry must derive the same board as a full re-evaluation"
            );
            assert_pt_identical(&normal, &forced, "transient condition read channel");
        }

        /// (3.2) TRANSIENT SOURCE-LEVEL GATE. Same frozen-set/live-gate
        /// asymmetry, but the gate is a plain SOURCE-LEVEL presence check (the
        /// source definition's own CR 611.3a condition, carried onto the
        /// transient) instead of a recipient-context count, and NOTHING on the
        /// board writes the kinds it reads. That makes the entry-perturbation
        /// probe the only disjunct that can catch it: an opponent's creature
        /// entering flips `IsPresent{creature an opponent controls}` from OFF
        /// to ON, and every recipient frozen into the effect's set (CR 611.2c)
        /// goes 2/2 → 5/5.
        ///
        /// DISCRIMINATING: drop the transient walk from
        /// `any_active_static_condition_perturbed_by_entry` and the
        /// printed-static walk sees no condition at all, the kind relation
        /// exits at stage 3 (`{PowerToughness} ∩ {CardTypes, Controller} = ∅`),
        /// and the recipients stay stale at 2/2.
        fn transient_source_level_gate_board() -> GameState {
            use crate::types::ability::{ContinuousModification, StaticCondition};
            use crate::types::ControllerRef;
            let mut state = setup();
            let mut bears = Vec::new();
            for i in 0..2 {
                bears.push(add_relation_bear(
                    &mut state,
                    770 + i,
                    &format!("GatedBear{i}"),
                    vec![],
                ));
            }
            let source =
                install_static_enchantment(&mut state, 772, "Opponent-Gated Grant", vec![]);
            install_gated_transient(
                &mut state,
                source,
                &bears,
                vec![
                    ContinuousModification::AddPower { value: 3 },
                    ContinuousModification::AddToughness { value: 3 },
                ],
                Duration::UntilEndOfTurn,
                // CR 109.5: a resolved effect RETAINS its controller, so "an
                // opponent" is read against P0. OFF on this board.
                Some(StaticCondition::IsPresent {
                    filter: Some(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        controller: Some(ControllerRef::Opponent),
                        ..Default::default()
                    })),
                }),
            );
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        #[test]
        fn opponent_entry_escalates_through_transient_source_level_gate() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(transient_source_level_gate_board, |s| {
                    add_colorless_creature_entry_under(s, 773, PlayerId(1))
                });
            assert!(
                escalated,
                "CR 611.2c freezes a resolved effect's affected SET, not its gate — \
                 an entry that flips a transient's source-level presence gate must \
                 escalate or every frozen recipient keeps a stale board"
            );
            // Non-vacuity: the gate is genuinely OFF before the entry.
            let mut pre = transient_source_level_gate_board();
            flush_layers(&mut pre);
            assert_eq!(
                pts_named(&pre, "GatedBear"),
                vec![(Some(2), Some(2)); 2],
                "pre-entry no opponent controls a creature, so the gate is OFF"
            );
            assert_eq!(
                pts_named(&forced, "GatedBear"),
                vec![(Some(5), Some(5)); 2],
                "the opponent's entrant turns the gate ON for every frozen recipient"
            );
            assert_eq!(
                pts_named(&normal, "GatedBear"),
                pts_named(&forced, "GatedBear"),
                "the escalated entry must derive the same board as a full re-evaluation"
            );
            assert_pt_identical(&normal, &forced, "transient source-level gate");
        }

        /// The name a layer-1 override (CR 707.9b) stamps onto every creature on
        /// the (3.3)/(3.4) boards.
        const OVERRIDDEN_NAME: &str = "Cloned Bear";

        /// "Three or more permanents named `OVERRIDDEN_NAME`", counted
        /// board-wide. Same shape as (3.1)'s gate minus the recipient context:
        /// no `FilterProp::Another`, so `condition_uses_recipient_context` is
        /// false and every gather strips it off the effect it pushes.
        fn overridden_name_count_at_least(count: i32) -> crate::types::ability::StaticCondition {
            crate::types::ability::StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Named {
                            name: OVERRIDDEN_NAME.to_string(),
                        },
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: count },
            }
        }

        /// Two 2/2 recipients, a printed LAYER-1 `SetName` override (CR 707.9b)
        /// over creatures, and one +1/+1 transient per recipient gated on
        /// `install_gate`.
        ///
        /// WHY LAYER 1 and not the layer-4 rewrite (3.1) uses: a source-level
        /// condition and a `ForAsLongAs` duration are both evaluated inside
        /// `gather_transient_continuous_effects`, and `evaluate_layers` gathers
        /// at Step 3 — after layer 1 has been applied and before layers 2-7.
        /// Layer 1 is therefore the ONLY layer whose writes such a gate can see
        /// within one pass. (A retained recipient-context condition is instead
        /// re-checked at APPLY time, which is why (3.1) can use layer 4.)
        /// `prepare_incremental_flush` gathers with NO layer applied at all, so
        /// the entrant is still printed-named there — that divergence is exactly
        /// the staleness these boards catch.
        ///
        /// Nothing else on the board is a creature, so the overridden-name
        /// population is exactly the creature count: 2 before the entry, 3
        /// after, which moves a `GE 3` gate from OFF to ON.
        fn transient_name_count_gate_board(
            install_gate: impl Fn(&mut GameState, ObjectId, &[ObjectId]),
        ) -> GameState {
            use crate::types::ability::ContinuousModification;
            let mut state = setup();
            let mut bears = Vec::new();
            for i in 0..2 {
                bears.push(add_relation_bear(
                    &mut state,
                    780 + i,
                    &format!("NameBear{i}"),
                    vec![],
                ));
            }
            // CR 707.9b: a layer-1 copiable-value name override, which MOVES the
            // counted population by reaching the entrant.
            let rename = continuous_static(
                TargetFilter::Typed(TypedFilter::creature()),
                vec![ContinuousModification::SetName {
                    name: OVERRIDDEN_NAME.to_string(),
                }],
            );
            install_static_enchantment(&mut state, 782, "Mass Renaming", vec![rename]);
            let source = install_static_enchantment(&mut state, 783, "Name-Gated Grant", vec![]);
            install_gate(&mut state, source, &bears);
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        /// (3.3) TRANSIENT SOURCE-LEVEL CONDITION, READ CHANNEL. The twin of
        /// (3.1) with the recipient context removed. A source-level condition
        /// never reaches `ActiveContinuousEffect::condition`:
        /// `gather_transient_continuous_effects` strips it (only a
        /// recipient-context condition is retained), and while the gate is OFF
        /// the effect is not gathered at all. So the ONLY way NameText enters
        /// `ReadKinds` is the walk over `state.transient_continuous_effects`.
        ///
        /// DISCRIMINATING: drop that walk and `ReadKinds` holds only the
        /// CardTypes its affected filters read, which is disjoint from the
        /// layer-1 `SetName` writer's `{NameText}` — the relation exits at
        /// stage 3, the entry stays incremental, and the recipients keep a
        /// stale 2/2 while a full pass says 3/3. The perturbation probe cannot
        /// rescue it either: the entrant still carries its printed name when
        /// the probe runs, so it does not perturb the overridden-name count.
        fn transient_source_level_condition_read_board() -> GameState {
            use crate::types::ability::ContinuousModification;
            transient_name_count_gate_board(|state, source, bears| {
                install_gated_transient(
                    state,
                    source,
                    bears,
                    vec![
                        ContinuousModification::AddPower { value: 1 },
                        ContinuousModification::AddToughness { value: 1 },
                    ],
                    Duration::UntilEndOfTurn,
                    Some(overridden_name_count_at_least(3)),
                )
            })
        }

        #[test]
        fn name_rewrite_entry_escalates_through_transient_source_level_condition_reads() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(transient_source_level_condition_read_board, |s| {
                    add_colorless_creature_entry(s, 784)
                });
            assert!(
                escalated,
                "a source-level gate on a resolution-created effect is stripped from \
                 the gathered effect, so only the transient walk can put NameText in \
                 ReadKinds — a layer-1 name override reaching the entrant must escalate"
            );
            // Non-vacuity, fixture side: the installed gate really is the
            // board-wide count — no `FilterProp::Another`, nothing else
            // recipient-relative — so it is source-level.
            assert!(
                !forced.transient_continuous_effects.is_empty()
                    && forced.transient_continuous_effects.iter().all(|tce| {
                        matches!(tce.affected, TargetFilter::SpecificObject { .. })
                            && tce.condition.as_ref() == Some(&overridden_name_count_at_least(3))
                    }),
                "the fixture must install SpecificObject-bound transients whose gate is \
                 source-level, or the `e.condition` channel would cover this board"
            );
            // Non-vacuity, GATHERED side: asserting the fixture only proves what
            // was installed. Run the real gather and confirm the condition is
            // gone from every effect it produces — that strip is the whole
            // premise of this test, so it is asserted, not inferred.
            let mut gathered = Vec::new();
            crate::game::layers::gather_transient_continuous_effects(&forced, &mut gathered);
            assert!(
                !gathered.is_empty() && gathered.iter().all(|e| e.condition.is_none()),
                "the gather must strip this source-level condition; if it retained it, \
                 `e.condition` would cover the board and the transient walk would be \
                 untested here"
            );
            let mut pre = transient_source_level_condition_read_board();
            flush_layers(&mut pre);
            assert_eq!(
                pts_base_named(&pre, "NameBear"),
                vec![(Some(2), Some(2)); 2],
                "pre-entry only 2 permanents carry the overridden name, so GE 3 is OFF"
            );
            assert_eq!(
                pts_base_named(&forced, "NameBear"),
                vec![(Some(3), Some(3)); 2],
                "layer 1 renames the entrant too, making it the third — the gate turns ON"
            );
            assert_eq!(
                pts_base_named(&normal, "NameBear"),
                pts_base_named(&forced, "NameBear"),
                "the escalated entry must derive the same board as a full re-evaluation"
            );
            assert_pt_identical(&normal, &forced, "transient source-level condition reads");
        }

        /// (3.4) `ForAsLongAs` DURATION, READ CHANNEL. Identical board to (3.3)
        /// with the gate moved from `tce.condition` into
        /// `Duration::ForAsLongAs` (CR 611.2b — the effect lasts exactly as
        /// long as its stated condition holds). `transient_effect_is_live`
        /// evaluates it in the same gather, and no gather ever copies a
        /// duration's condition onto an `ActiveContinuousEffect`, so this gate
        /// is invisible to every channel except the transient walk.
        ///
        /// DISCRIMINATING: drop `transient_duration_condition` from
        /// `transient_gate_conditions` and `ReadKinds` loses NameText exactly
        /// as in (3.3) — recipients keep a stale 2/2.
        fn transient_duration_gate_read_board() -> GameState {
            use crate::types::ability::ContinuousModification;
            transient_name_count_gate_board(|state, source, bears| {
                install_gated_transient(
                    state,
                    source,
                    bears,
                    vec![
                        ContinuousModification::AddPower { value: 1 },
                        ContinuousModification::AddToughness { value: 1 },
                    ],
                    Duration::ForAsLongAs {
                        condition: overridden_name_count_at_least(3),
                    },
                    None,
                )
            })
        }

        #[test]
        fn name_rewrite_entry_escalates_through_transient_duration_gate_reads() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(transient_duration_gate_read_board, |s| {
                    add_colorless_creature_entry(s, 785)
                });
            assert!(
                escalated,
                "CR 611.2b makes a `for as long as` duration a live gate, so the kinds \
                 it reads are live reads — a layer-1 name override reaching the entrant \
                 must escalate"
            );
            // Non-vacuity: the gate lives in the DURATION, not in `condition`,
            // so no `tce.condition` channel could have covered this board.
            assert!(
                !forced.transient_continuous_effects.is_empty()
                    && forced.transient_continuous_effects.iter().all(|tce| {
                        tce.condition.is_none()
                            && matches!(tce.duration, Duration::ForAsLongAs { .. })
                    }),
                "the fixture must gate purely through `Duration::ForAsLongAs`"
            );
            let mut pre = transient_duration_gate_read_board();
            flush_layers(&mut pre);
            assert_eq!(
                pts_base_named(&pre, "NameBear"),
                vec![(Some(2), Some(2)); 2],
                "pre-entry only 2 permanents carry the overridden name, so the \
                 duration has not started"
            );
            assert_eq!(
                pts_base_named(&forced, "NameBear"),
                vec![(Some(3), Some(3)); 2],
                "layer 1 renames the entrant too, making it the third — the duration holds"
            );
            assert_eq!(
                pts_base_named(&normal, "NameBear"),
                pts_base_named(&forced, "NameBear"),
                "the escalated entry must derive the same board as a full re-evaluation"
            );
            assert_pt_identical(&normal, &forced, "transient duration gate reads");
        }

        /// (3.5) `ForAsLongAs` DURATION, PERTURBATION-PROBE CHANNEL. The twin
        /// of (3.2) with the gate moved into the duration: Master Thief's "for
        /// as long as you control this creature" shape, inverted to an
        /// opponent-presence check so an entry can start it. NOTHING on this
        /// board writes the kinds the gate reads, so the read union cannot see
        /// the flip — while the duration is unmet the effect is not gathered at
        /// all and `all_writes` is empty, which exits the kind relation at
        /// stage 1.
        ///
        /// DISCRIMINATING: drop `transient_duration_condition` from
        /// `transient_gate_conditions` and the probe's transient arm sees only
        /// `tce.condition`, which is `None` here — no disjunct fires, the entry
        /// stays incremental, and the frozen recipients keep a stale 2/2 while
        /// a full pass says 5/5.
        fn transient_duration_gate_probe_board() -> GameState {
            use crate::types::ability::{ContinuousModification, StaticCondition};
            use crate::types::ControllerRef;
            let mut state = setup();
            let mut bears = Vec::new();
            for i in 0..2 {
                bears.push(add_relation_bear(
                    &mut state,
                    790 + i,
                    &format!("DurationBear{i}"),
                    vec![],
                ));
            }
            let source =
                install_static_enchantment(&mut state, 792, "Opponent-Gated Duration", vec![]);
            install_gated_transient(
                &mut state,
                source,
                &bears,
                vec![
                    ContinuousModification::AddPower { value: 3 },
                    ContinuousModification::AddToughness { value: 3 },
                ],
                // CR 611.2b + CR 109.5: the duration is re-read every pass and
                // "an opponent" stays bound to the resolver, P0.
                Duration::ForAsLongAs {
                    condition: StaticCondition::IsPresent {
                        filter: Some(TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Creature],
                            controller: Some(ControllerRef::Opponent),
                            ..Default::default()
                        })),
                    },
                },
                None,
            );
            state.layers_dirty = crate::types::game_state::LayersDirty::Full;
            state
        }

        #[test]
        fn opponent_entry_escalates_through_transient_duration_gate() {
            let (normal, escalated, forced) =
                flush_entry_and_forced(transient_duration_gate_probe_board, |s| {
                    add_colorless_creature_entry_under(s, 793, PlayerId(1))
                });
            assert!(
                escalated,
                "CR 611.2c freezes a resolved effect's affected SET, not its duration — \
                 an entry that starts a `for as long as` duration must escalate or every \
                 frozen recipient keeps a stale board"
            );
            // Non-vacuity: the gate lives in the DURATION only.
            assert!(
                !forced.transient_continuous_effects.is_empty()
                    && forced.transient_continuous_effects.iter().all(|tce| {
                        tce.condition.is_none()
                            && matches!(tce.duration, Duration::ForAsLongAs { .. })
                    }),
                "the fixture must gate purely through `Duration::ForAsLongAs`"
            );
            let mut pre = transient_duration_gate_probe_board();
            flush_layers(&mut pre);
            assert_eq!(
                pts_named(&pre, "DurationBear"),
                vec![(Some(2), Some(2)); 2],
                "pre-entry no opponent controls a creature, so the duration never started"
            );
            assert_eq!(
                pts_named(&forced, "DurationBear"),
                vec![(Some(5), Some(5)); 2],
                "the opponent's entrant starts the duration for every frozen recipient"
            );
            assert_eq!(
                pts_named(&normal, "DurationBear"),
                pts_named(&forced, "DurationBear"),
                "the escalated entry must derive the same board as a full re-evaluation"
            );
            assert_pt_identical(&normal, &forced, "transient duration gate probe");
        }

        /// Assert every battlefield object's computed power/toughness/loyalty and
        /// keyword set are identical across two states.
        fn assert_pt_identical(a: &GameState, b: &GameState, label: &str) {
            assert_eq!(
                a.battlefield.len(),
                b.battlefield.len(),
                "{label}: battlefield size mismatch"
            );
            for &id in a.battlefield.iter() {
                let oa = a.objects.get(&id).expect("a object");
                let ob = b.objects.get(&id).expect("b object");
                assert_eq!(oa.power, ob.power, "{label}: power mismatch for {id:?}");
                assert_eq!(
                    oa.toughness, ob.toughness,
                    "{label}: toughness mismatch for {id:?}"
                );
                assert_eq!(
                    oa.keywords, ob.keywords,
                    "{label}: keyword mismatch for {id:?}"
                );
            }
        }

        /// Install a battlefield permanent hosting a token-scoped ETB replacement:
        /// "each token that would enter the battlefield enters tapped." Modeled as a
        /// `ReplacementEvent::ChangeZone` whose `valid_card` is
        /// `Typed(Permanent, [FilterProp::Token])` and whose `execute` self-taps the
        /// entering permanent (CR 701.26a → `EtbTapState::Tapped`). Returns the host
        /// id. The replacement's `valid_card` is matched via
        /// `matches_target_filter_on_battlefield_entry` DURING `replace_event`
        /// (the pre-delivery seam) against the LIVE entering object's `is_token`.
        fn add_token_enters_tapped_replacement(state: &mut GameState) -> ObjectId {
            use crate::types::ability::{
                AbilityKind, EffectScope, ReplacementDefinition, TapStateChange,
            };
            use crate::types::replacements::ReplacementEvent;
            let host = create_object(
                state,
                CardId(970),
                PlayerId(0),
                "Token Taps Down".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&host).unwrap();
                obj.card_types.core_types.push(CoreType::Enchantment);
                let repl = ReplacementDefinition::new(ReplacementEvent::ChangeZone)
                    .valid_card(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Permanent],
                        properties: vec![FilterProp::Token],
                        ..Default::default()
                    }))
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::SetTapState {
                            target: TargetFilter::SelfRef,
                            scope: EffectScope::Single,
                            state: TapStateChange::Tap,
                        },
                    ));
                Arc::make_mut(&mut obj.base_replacement_definitions).push(repl.clone());
                obj.replacement_definitions.push(repl);
            }
            host
        }

        /// Push a permanent-spell COPY (is_copy = true, is_token = false — exactly
        /// the shape `Effect::CastCopyOfCard` produces for a permanent) onto the
        /// stack and return its id.
        fn push_permanent_copy_spell(state: &mut GameState, card_id: u64) -> ObjectId {
            let copy_id = create_object(
                state,
                CardId(card_id),
                PlayerId(0),
                "Permanent Copy".to_string(),
                Zone::Stack,
            );
            {
                let obj = state.objects.get_mut(&copy_id).unwrap();
                obj.card_types.core_types.push(CoreType::Creature);
                obj.is_copy = true;
                obj.is_token = false;
            }
            state.stack.push_back(StackEntry {
                id: copy_id,
                source_id: copy_id,
                controller: PlayerId(0),
                kind: StackEntryKind::Spell {
                    card_id: CardId(card_id),
                    ability: None,
                    casting_variant: super::CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            });
            copy_id
        }

        /// CR 707.10f + CR 608.3f (PRODUCTION-PATH regression, revert-failing):
        /// when a copy of a permanent spell resolves onto the battlefield, a
        /// token-scoped ETB REPLACEMENT ("each token that enters enters tapped")
        /// must OBSERVE the resolving copy as a token during `replace_event` and
        /// therefore apply. This drives the real `resolve_top` →
        /// `dest == Battlefield` block → `super::replacement::replace_event`
        /// (stack.rs) path. The replacement's `valid_card`
        /// (`FilterProp::Token`) is matched by
        /// `matches_target_filter_on_battlefield_entry` against the LIVE entering
        /// object BEFORE the ZoneChange is delivered.
        ///
        /// Revert probe: with the flip at its OLD (late) site in
        /// `zone_pipeline::deliver_replaced_zone_change` (which runs AFTER
        /// `replace_event`), the entering object is still `is_token = false` when
        /// the replacement's token filter is evaluated, so the replacement does
        /// NOT match and the copy enters UNTAPPED — the `tapped` assertion below
        /// flips to false and the test fails.
        #[test]
        fn resolving_permanent_copy_is_observed_as_token_by_etb_replacement() {
            let mut state = setup();
            add_token_enters_tapped_replacement(&mut state);
            let copy_id = push_permanent_copy_spell(&mut state, 972);

            let mut events = Vec::new();
            resolve_top(&mut state, &mut events);

            let copy = &state.objects[&copy_id];
            // Final-state sanity: the copy is now a token permanent (CR 707.10f).
            assert_eq!(copy.zone, Zone::Battlefield);
            assert!(copy.is_token, "CR 707.10f: the resolved copy is a token");
            assert!(
                !copy.is_copy,
                "CR 707.10f: it is no longer a copy of a spell"
            );
            // The discriminating assertion: the token-scoped ETB replacement saw
            // the entering copy as a token at `replace_event` time and tapped it.
            assert!(
                copy.tapped,
                "CR 707.10f: a token-scoped ETB replacement must observe the \
                 resolving permanent copy as a token as it enters — so it enters \
                 tapped. If the flip lands after replace_event, it enters untapped."
            );
        }

        /// Negative control for the token-scoped ETB replacement: a REAL permanent
        /// (is_copy = false) resolving to the battlefield is a nontoken, so the
        /// Token-filtered "enters tapped" replacement must NOT fire — it enters
        /// untapped. Proves the discriminating assertion above keys on token-ness,
        /// not on "every resolving permanent taps."
        #[test]
        fn resolving_real_permanent_is_not_tapped_by_token_replacement() {
            let mut state = setup();
            add_token_enters_tapped_replacement(&mut state);
            let real_id = push_permanent_copy_spell(&mut state, 973);
            // Make it a REAL permanent, not a copy.
            {
                let obj = state.objects.get_mut(&real_id).unwrap();
                obj.is_copy = false;
                obj.is_token = false;
            }

            let mut events = Vec::new();
            resolve_top(&mut state, &mut events);

            let obj = &state.objects[&real_id];
            assert_eq!(obj.zone, Zone::Battlefield);
            assert!(!obj.is_token, "a real permanent is not a token");
            assert!(
                !obj.tapped,
                "a nontoken permanent must not be tapped by a Token-scoped ETB \
                 replacement"
            );
        }
    }

    /// CR 706.2 + CR 706.4 + CR 603.12: A reflexive "When you do … the result"
    /// sub-ability resolves on its OWN `StackEntryKind::TriggeredAbility` entry,
    /// in a later resolution scope than the original roll. The rolled value is
    /// carried on the entry's `die_result` field and re-stamped into
    /// `die_result_this_resolution` by `resolve_top` so the entry's
    /// `EventContextAmount` reads the roll (11), NOT the surviving combat-damage
    /// event amount (6). This is the building-block guard for Ancient Bronze
    /// Dragon's reflexive class (issue #1602, Deliverable 1).
    #[test]
    fn reflexive_entry_lifts_carried_die_result_into_resolution_scope() {
        let mut state = setup();
        // A source object on the battlefield (controller P0).
        let source = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Ancient Bronze Dragon".to_string(),
            Zone::Battlefield,
        );

        // The reflexive sub-ability: "gain life equal to the result".
        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::EventContextAmount,
                },
                player: TargetFilter::Controller,
            },
            vec![],
            source,
            PlayerId(0),
        );

        // Carry die_result: Some(11) onto the entry, alongside a SURVIVING
        // combat-damage trigger event (amount 6). Match-count is None so the
        // die slot is what the cascade must read.
        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id: source,
            target: TargetRef::Player(PlayerId(0)),
            amount: 6,
            is_combat: true,
            excess: 0,
        });
        state.stack.push_back(StackEntry {
            id: source,
            source_id: source,
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: source,
                ability: Box::new(ability),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: "Ancient Bronze Dragon".to_string(),
                subject_match_count: None,
                die_result: Some(11),
                provenance: None,
            },
        });

        let life_before = state.players[0].life;
        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        // Gained 11 (the carried die result), NOT 6 (the combat-damage event).
        assert_eq!(
            state.players[0].life - life_before,
            11,
            "reflexive entry must read the carried die result (11), not the \
             surviving combat-damage amount (6)"
        );
        // The die slot is cleared at the cross-resolution boundary after the
        // entry resolves (mirrors the batched subject-count lifecycle).
        assert_eq!(state.die_result_this_resolution, None);
        assert_eq!(state.current_trigger_match_count, None);
    }

    /// CR 306.5b + CR 712.14a: A permanent spell cast transformed enters as its
    /// back face, so the stack resolution path must seed loyalty counters from
    /// that back face rather than the front-face spell object.
    #[test]
    fn cast_transformed_spell_seeds_back_face_loyalty_counters() {
        let mut state = setup();
        let spell_id = create_object(
            &mut state,
            CardId(623),
            PlayerId(0),
            "Front Creature".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.back_face = Some(back_face_data(
                "Back Planeswalker",
                CoreType::Planeswalker,
                Some(6),
                None,
            ));
        }
        state.stack_paid_facts.insert(
            spell_id,
            StackPaidSnapshot {
                cast_transformed: true,
                ..Default::default()
            },
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(623),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        let obj = &state.objects[&spell_id];
        assert_eq!(obj.zone, Zone::Battlefield);
        assert!(obj.transformed);
        assert_eq!(obj.counters.get(&CounterType::Loyalty).copied(), Some(6));
        assert_eq!(obj.loyalty, Some(6));
    }

    /// CR 310.4b + CR 712.14a: The same cast-transformed stack path must use the
    /// back face's printed defense when the resolving back face is a battle.
    #[test]
    fn cast_transformed_spell_seeds_back_face_defense_counters() {
        let mut state = setup();
        let spell_id = create_object(
            &mut state,
            CardId(624),
            PlayerId(0),
            "Front Creature".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.back_face = Some(back_face_data(
                "Back Siege",
                CoreType::Battle,
                None,
                Some(5),
            ));
        }
        state.stack_paid_facts.insert(
            spell_id,
            StackPaidSnapshot {
                cast_transformed: true,
                ..Default::default()
            },
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(624),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        let obj = &state.objects[&spell_id];
        assert_eq!(obj.zone, Zone::Battlefield);
        assert!(obj.transformed);
        assert_eq!(obj.counters.get(&CounterType::Defense).copied(), Some(5));
        assert_eq!(obj.defense, Some(5));
    }

    /// **§6 R26 — `resolve_top`'s BEHAVIOUR IS UNCHANGED ACROSS THE
    /// `bind_resolution_scope` EXTRACTION.**
    ///
    /// U1 moves the CR 603.4 re-check and the CR 608.2k / CR 603.2c / CR 706.2
    /// resolution-scope binding out of the universal resolution chokepoint into
    /// a shared function the analysis probe can also call. Three matched pairs,
    /// each keyed to one thing a WIDER extraction boundary would have broken —
    /// the boundary this plan struck, which would have pulled the pop, the
    /// keyword-action branch and the `StackResolved` pushes across with it:
    ///
    /// * **(a) CR 113.3b keyword actions still resolve.** The `KeywordAction`
    ///   early return sits ABOVE the extracted region and must stay in
    ///   `resolve_top` (it needs `&mut Vec<GameEvent>`, which the shared
    ///   function deliberately does not take). Equip attaches, and
    ///   `StackResolved` is emitted exactly once.
    /// * **(b) CR 603.4 false ⇒ removed from the stack, does nothing,
    ///   `StackResolved` STILL emitted.** The event is pushed by the CALLER —
    ///   the extracted function returns a bare `bool` and has no event sink, so
    ///   this is the seam the struck `Option<GameState>` signature had no
    ///   channel for. Matched against the condition-TRUE twin, which resolves.
    /// * **(c) CR 107.3m + CR 707.10 `paid_facts` survives.** The pop and its
    ///   `paid_snapshot` binding stayed in `resolve_top`: a permanent spell with
    ///   printed loyalty `X` enters with the snapshot's `x_value` in loyalty
    ///   counters. Matched against the same spell with NO snapshot, which enters
    ///   with none.
    ///
    /// REACH-GUARD on all three: the stack depth decreased by exactly 1, so an
    /// entry that never resolved cannot satisfy a "did nothing" arm vacuously.
    ///
    /// REVERT-PROBES (the plan's, each a single edit): (a) move the
    /// `KeywordAction` branch into `bind_resolution_scope` and return `false`
    /// for it ⇒ the equipment never attaches; (b) delete the
    /// `events.push(StackResolved)` from `resolve_top`'s `false` arm ⇒ the event
    /// assertion flips; (c) move the pop into the shared function so
    /// `paid_snapshot` is dropped ⇒ the spell enters at `cost_x_paid`/0 loyalty.
    #[test]
    fn resolve_top_behaviour_is_unchanged_across_the_bind_resolution_scope_extraction() {
        let resolved_once = |events: &[GameEvent], id: ObjectId| {
            events
                .iter()
                .filter(|e| matches!(e, GameEvent::StackResolved { object_id } if *object_id == id))
                .count()
        };

        // ── (a) CR 113.3b: the keyword-action early return ──
        {
            let mut state = setup();
            let equipment = create_object(
                &mut state,
                CardId(701),
                PlayerId(0),
                "Test Equipment".to_string(),
                Zone::Battlefield,
            );
            let creature = create_object(
                &mut state,
                CardId(702),
                PlayerId(0),
                "Test Bearer".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&creature)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
            let entry_id = ObjectId(7010);
            state.stack.push_back(StackEntry {
                id: entry_id,
                source_id: equipment,
                controller: PlayerId(0),
                kind: StackEntryKind::KeywordAction {
                    action: KeywordAction::Equip {
                        equipment_id: equipment,
                        target_creature_id: creature,
                    },
                },
            });
            let depth = state.stack.len();

            let mut events = Vec::new();
            resolve_top(&mut state, &mut events);

            assert_eq!(state.stack.len(), depth - 1, "reach-guard (a): it resolved");
            assert_eq!(
                state.objects[&equipment].attached_to,
                Some(crate::game::game_object::AttachTarget::Object(creature)),
                "CR 702.6a: the equip keyword action must still attach — its branch \
                 returns EARLY, above the extracted region"
            );
            assert_eq!(
                resolved_once(&events, entry_id),
                1,
                "CR 405.5: exactly one StackResolved for the keyword action"
            );
        }

        // ── (b) CR 603.4: a FALSE intervening-if still emits StackResolved ──
        // `setup()` is a standard-format board (20 starting life), so the
        // intervening-if `LifeTotalGE 5` is TRUE and `LifeTotalGE 99` FALSE.
        for (label, minimum, expect_gain) in [("TRUE", 5, 3i32), ("FALSE", 99, 0)] {
            let mut state = setup();
            let source = create_object(
                &mut state,
                CardId(703),
                PlayerId(0),
                "Conditional Trigger".to_string(),
                Zone::Battlefield,
            );
            let entry_id = ObjectId(7020);
            state.stack.push_back(StackEntry {
                id: entry_id,
                source_id: source,
                controller: PlayerId(0),
                kind: StackEntryKind::TriggeredAbility {
                    source_id: source,
                    ability: Box::new(ResolvedAbility::new(
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 3 },
                            player: TargetFilter::Controller,
                        },
                        vec![],
                        source,
                        PlayerId(0),
                    )),
                    condition: Some(TriggerCondition::LifeTotalGE { minimum }),
                    trigger_event: None,
                    description: None,
                    source_name: "Conditional Trigger".to_string(),
                    subject_match_count: None,
                    die_result: None,
                    provenance: None,
                },
            });
            let depth = state.stack.len();
            let life_before = state.players[0].life;

            let mut events = Vec::new();
            resolve_top(&mut state, &mut events);

            assert_eq!(
                state.stack.len(),
                depth - 1,
                "reach-guard (b/{label}): the entry left the stack either way"
            );
            assert_eq!(
                state.players[0].life - life_before,
                expect_gain,
                "CR 603.4 ({label}): the effect runs only when the intervening-if holds"
            );
            assert_eq!(
                resolved_once(&events, entry_id),
                1,
                "CR 405.5 ({label}): the CALLER pushes StackResolved on BOTH sides of \
                 the extracted check — the shared function takes no event sink"
            );
        }

        // ── (c) CR 107.3m + CR 707.10: the popped paid snapshot survives ──
        for (label, snapshot_x, expected_loyalty) in
            [("snapshot X=3", Some(3u32), 3u32), ("no snapshot", None, 0)]
        {
            let mut state = setup();
            let spell_id = create_object(
                &mut state,
                CardId(704),
                PlayerId(0),
                "X Loyalty Walker".to_string(),
                Zone::Stack,
            );
            {
                let obj = state.objects.get_mut(&spell_id).unwrap();
                obj.card_types.core_types.push(CoreType::Planeswalker);
                obj.printed_loyalty = Some(crate::types::card::PrintedLoyalty::X);
                obj.loyalty = None;
            }
            if let Some(x_value) = snapshot_x {
                state.stack_paid_facts.insert(
                    spell_id,
                    StackPaidSnapshot {
                        x_value: Some(x_value),
                        ..Default::default()
                    },
                );
            }
            state.stack.push_back(StackEntry {
                id: spell_id,
                source_id: spell_id,
                controller: PlayerId(0),
                kind: StackEntryKind::Spell {
                    card_id: CardId(704),
                    ability: None,
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            });
            let depth = state.stack.len();

            let mut events = Vec::new();
            resolve_top(&mut state, &mut events);

            assert_eq!(
                state.stack.len(),
                depth - 1,
                "reach-guard (c/{label}): the spell resolved"
            );
            assert_eq!(
                state.objects[&spell_id].zone,
                Zone::Battlefield,
                "reach-guard (c/{label}): the permanent spell entered the battlefield"
            );
            assert_eq!(
                state.objects[&spell_id]
                    .counters
                    .get(&CounterType::Loyalty)
                    .copied()
                    .unwrap_or(0),
                expected_loyalty,
                "CR 107.3m ({label}): the ETB counter count comes from the POPPED \
                 payment snapshot, which stays bound in `resolve_top`"
            );
        }
    }

    /// **The shared CR 603.4 / CR 608.2k / CR 603.2c / CR 706.2 binder is
    /// entry-shaped only at its adapter.**
    ///
    /// `bind_triggered_resolution_scope` is the authority a stackless CR 605.4a
    /// triggered mana resolution will call — it owns no `StackEntry` and must
    /// therefore reach every binding shape the entry-shaped wrapper reaches.
    /// The delicate part is that baseline's three `if let` blocks are **not**
    /// three independent branches: the event/batch block is an `if / else if`
    /// chain, so a triggered ability carrying `trigger_event: None` and a batch
    /// falls THROUGH to the non-triggered batch arm. A rewrite that keys the
    /// batch arm on "not a triggered ability" silently loses that case.
    ///
    /// Rows, each keyed to one thing the extraction could have broken:
    ///
    /// * **(a)** triggered + `Some(event)` + no batch ⇒ the batch is
    ///   synthesized as a one-element vector from that event;
    /// * **(b)** triggered + `Some(event)` + a batch ⇒ the batch wins and the
    ///   singleton event stays the authoritative one;
    /// * **(c)** triggered + `None` event + a batch ⇒ the fall-through arm, so
    ///   the authoritative event is the batch head;
    /// * **(d)** NOT triggered + a batch ⇒ same arm, but CR 603.2c/CR 706.2
    ///   must NOT be re-stamped, because a spell carries neither;
    /// * **(e)** a false CR 603.4 intervening-if returns `false` having bound
    ///   **nothing** — the caller must be able to abandon without unwinding;
    /// * **(f)** the entry-shaped adapter and a hand-built scope produce
    ///   byte-identical bindings from the same facts.
    ///
    /// REACH-GUARD: every row asserts the pre-call scope is the sentinel value,
    /// so "unchanged" can never be confused with "bound to the same thing".
    ///
    /// REVERT-PROBES: making the batch arm `else if triggered.is_none()` fails
    /// (c); dropping the `(Some(te), batch)` arm's `unwrap_or_else` fails (a);
    /// stamping the count/die outside the `triggered` guard fails (d); binding
    /// before the condition check fails (e).
    #[test]
    fn the_shared_resolution_scope_binder_reaches_every_baseline_binding_shape() {
        let event_a = GameEvent::StackResolved {
            object_id: ObjectId(9001),
        };
        let event_b = GameEvent::StackResolved {
            object_id: ObjectId(9002),
        };
        let sentinel = GameEvent::StackResolved {
            object_id: ObjectId(9999),
        };

        // Every row starts from a distinguishable sentinel scope, so a row that
        // asserts a bound value cannot pass because nothing ran.
        let armed = || {
            let mut state = setup();
            state.current_trigger_event = Some(sentinel.clone());
            state.current_trigger_events = vec![sentinel.clone()];
            state.current_trigger_match_count = Some(77);
            state.die_result_this_resolution = Some(77);
            state
        };
        // ── (a) triggered + event, no batch ⇒ synthesized singleton batch ──
        {
            let mut state = armed();
            assert!(bind_triggered_resolution_scope(
                &mut state,
                Some(TriggeredResolutionScope {
                    condition: None,
                    controller: PlayerId(0),
                    trigger_source: None,
                    trigger_event: Some(&event_a),
                    subject_match_count: Some(4),
                    die_result: Some(6),
                }),
                None,
            ));
            assert_eq!(state.current_trigger_event.as_ref(), Some(&event_a));
            assert_eq!(
                state.current_trigger_events,
                vec![event_a.clone()],
                "CR 608.2k: with no batch the authoritative event IS the batch"
            );
            assert_eq!(state.current_trigger_match_count, Some(4));
            assert_eq!(state.die_result_this_resolution, Some(6));
        }

        // ── (b) triggered + event + batch ⇒ the batch wins ──
        {
            let mut state = armed();
            assert!(bind_triggered_resolution_scope(
                &mut state,
                Some(TriggeredResolutionScope {
                    condition: None,
                    controller: PlayerId(0),
                    trigger_source: None,
                    trigger_event: Some(&event_a),
                    subject_match_count: None,
                    die_result: None,
                }),
                Some(vec![event_a.clone(), event_b.clone()]),
            ));
            assert_eq!(state.current_trigger_event.as_ref(), Some(&event_a));
            assert_eq!(
                state.current_trigger_events,
                vec![event_a.clone(), event_b.clone()]
            );
            assert_eq!(
                state.current_trigger_match_count, None,
                "CR 603.2c: a triggered scope stamps its own None over the sentinel"
            );
        }

        // ── (c) triggered + NO event + batch ⇒ the fall-through arm ──
        {
            let mut state = armed();
            assert!(bind_triggered_resolution_scope(
                &mut state,
                Some(TriggeredResolutionScope {
                    condition: None,
                    controller: PlayerId(0),
                    trigger_source: None,
                    trigger_event: None,
                    subject_match_count: Some(2),
                    die_result: None,
                }),
                Some(vec![event_b.clone(), event_a.clone()]),
            ));
            assert_eq!(
                state.current_trigger_event.as_ref(),
                Some(&event_b),
                "the batch HEAD becomes authoritative — this is the `else if` \
                 fall-through a triggered ability with no singleton event reaches"
            );
            assert_eq!(
                state.current_trigger_events,
                vec![event_b.clone(), event_a.clone()]
            );
            assert_eq!(state.current_trigger_match_count, Some(2));
        }

        // ── (d) not triggered + batch ⇒ count/die are NOT re-stamped ──
        {
            let mut state = armed();
            assert!(bind_triggered_resolution_scope(
                &mut state,
                None,
                Some(vec![event_a.clone()]),
            ));
            assert_eq!(state.current_trigger_event.as_ref(), Some(&event_a));
            assert_eq!(
                (
                    state.current_trigger_match_count,
                    state.die_result_this_resolution
                ),
                (Some(77), Some(77)),
                "CR 603.2c + CR 706.2 belong to a TRIGGERED entry only; a spell must \
                 leave the ambient values exactly as it found them"
            );
        }

        // ── (e) a false CR 603.4 recheck binds nothing at all ──
        {
            let mut state = armed();
            assert!(!bind_triggered_resolution_scope(
                &mut state,
                Some(TriggeredResolutionScope {
                    // `setup()` is a 20-life board, so this is FALSE.
                    condition: Some(&TriggerCondition::LifeTotalGE { minimum: 99 }),
                    controller: PlayerId(0),
                    trigger_source: None,
                    trigger_event: Some(&event_a),
                    subject_match_count: Some(4),
                    die_result: Some(6),
                }),
                Some(vec![event_a.clone(), event_b.clone()]),
            ));
            assert_eq!(
                (
                    state.current_trigger_event.as_ref(),
                    state.current_trigger_events.as_slice(),
                    state.current_trigger_match_count,
                    state.die_result_this_resolution,
                ),
                (
                    Some(&sentinel),
                    [sentinel.clone()].as_slice(),
                    Some(77),
                    Some(77)
                ),
                "CR 603.4: the recheck is the FIRST thing the binder does, so a \
                 refused resolution leaves the caller's scope untouched"
            );
            // The TRUE twin proves the row is not passing on a broken condition.
            let mut state = armed();
            assert!(bind_triggered_resolution_scope(
                &mut state,
                Some(TriggeredResolutionScope {
                    condition: Some(&TriggerCondition::LifeTotalGE { minimum: 5 }),
                    controller: PlayerId(0),
                    trigger_source: None,
                    trigger_event: Some(&event_a),
                    subject_match_count: Some(4),
                    die_result: Some(6),
                }),
                None,
            ));
            assert_eq!(state.current_trigger_match_count, Some(4));
        }

        // ── (f) the entry-shaped adapter agrees with the hand-built scope ──
        {
            let entry = StackEntry {
                id: ObjectId(9100),
                source_id: ObjectId(9101),
                controller: PlayerId(0),
                kind: StackEntryKind::TriggeredAbility {
                    source_id: ObjectId(9101),
                    ability: Box::new(ResolvedAbility::new(
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                            player: TargetFilter::Controller,
                        },
                        vec![],
                        ObjectId(9101),
                        PlayerId(0),
                    )),
                    condition: Some(TriggerCondition::LifeTotalGE { minimum: 5 }),
                    trigger_event: Some(event_a.clone()),
                    description: None,
                    source_name: String::new(),
                    subject_match_count: Some(3),
                    die_result: Some(20),
                    provenance: None,
                },
            };
            let mut via_entry = armed();
            assert!(bind_resolution_scope(
                &mut via_entry,
                &entry,
                Some(vec![event_a.clone(), event_b.clone()]),
            ));
            let mut via_scope = armed();
            assert!(bind_triggered_resolution_scope(
                &mut via_scope,
                Some(TriggeredResolutionScope {
                    condition: Some(&TriggerCondition::LifeTotalGE { minimum: 5 }),
                    controller: PlayerId(0),
                    trigger_source: None,
                    trigger_event: Some(&event_a),
                    subject_match_count: Some(3),
                    die_result: Some(20),
                }),
                Some(vec![event_a.clone(), event_b.clone()]),
            ));
            assert_eq!(
                (
                    via_entry.current_trigger_event,
                    via_entry.current_trigger_events,
                    via_entry.current_trigger_match_count,
                    via_entry.die_result_this_resolution,
                ),
                (
                    via_scope.current_trigger_event,
                    via_scope.current_trigger_events,
                    via_scope.current_trigger_match_count,
                    via_scope.die_result_this_resolution,
                ),
                "the adapter is a projection of the entry onto the shared scope, \
                 not a second binding policy"
            );
        }
    }

    // -----------------------------------------------------------------------
    // C2: resolution-default moves route through the zone pipeline so Moved
    // graveyard→exile redirects (Rest in Peace / Leyline of the Void class)
    // fire on resolved/countered/prevented spells (PLAN §8 Risk #2).
    // -----------------------------------------------------------------------

    /// Install a board-wide Rest in Peace class redirect ("if a card would be
    /// put into a graveyard from anywhere, exile it instead") on a battlefield
    /// permanent. `valid_card: None` → matches any card's graveyard move;
    /// `destination_zone: Graveyard` gates it to graveyard-bound moves only.
    fn install_rest_in_peace(state: &mut GameState) -> ObjectId {
        use crate::types::ability::{AbilityDefinition, AbilityKind, ReplacementDefinition};
        use crate::types::replacements::ReplacementEvent;

        let rip = create_object(
            state,
            CardId(state.next_object_id),
            PlayerId(1),
            "Rest in Peace".to_string(),
            Zone::Battlefield,
        );
        let redirect = ReplacementDefinition::new(ReplacementEvent::Moved)
            .destination_zone(Zone::Graveyard)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    origin: None,
                    target: TargetFilter::SelfRef,
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
            ));
        state
            .objects
            .get_mut(&rip)
            .unwrap()
            .replacement_definitions
            .push(redirect);
        rip
    }

    fn push_plain_instant(state: &mut GameState) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let obj_id = create_object(state, card_id, PlayerId(0), "Bolt".to_string(), Zone::Stack);
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        let resolved = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id,
                ability: Some(Box::new(resolved)),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        obj_id
    }

    /// CR 608.2n + CR 614.6 (issue #2897): a resolving instant carrying its own
    /// shuffle-back graveyard replacement must land in its owner's library, not
    /// the graveyard.
    #[test]
    fn nexus_of_fate_class_shuffle_back_on_resolution() {
        use crate::parser::oracle_replacement::parse_replacement_line;

        let mut state = setup();
        let spell = push_plain_instant(&mut state);
        let repl = parse_replacement_line(
            "If ~ would be put into a graveyard from anywhere, reveal ~ and shuffle it into its \
             owner's library instead.",
            "Nexus of Fate",
        )
        .expect("shuffle-back replacement must parse");
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .replacement_definitions
            .push(repl);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert_eq!(
            state.objects[&spell].zone,
            Zone::Library,
            "shuffle-back replacement must redirect the resolved spell into its owner's library"
        );
        assert!(
            !state.players[0].graveyard.contains(&spell),
            "the spell must not also reach the graveyard"
        );
        assert!(
            state.players[0].library.contains(&spell),
            "the spell must be in its owner's library after resolution"
        );
    }

    /// CR 608.2n + CR 614.6 (PLAN §8 Risk #2 bug-fix): a plain instant resolving
    /// to its owner's graveyard is redirected to exile by a board-wide Rest in
    /// Peace. FAILS on the pre-C2 raw `move_to_zone(state, id, Graveyard, ..)`
    /// delivery, which never proposed the inner ZoneChange and so silently
    /// dropped the redirect (the spell landed in the graveyard).
    #[test]
    fn rest_in_peace_exiles_resolved_instant() {
        let mut state = setup();
        install_rest_in_peace(&mut state);
        let spell = push_plain_instant(&mut state);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert_eq!(
            state.objects[&spell].zone,
            Zone::Exile,
            "Rest in Peace must redirect the resolved instant's graveyard move to exile"
        );
        assert!(
            !state.players[0].graveyard.contains(&spell),
            "the redirected spell must not also reach the graveyard"
        );
    }

    /// CR 702.34a + CR 614.6 (PLAN §8 Risk #2 non-regression): a flashback spell
    /// exiles via its STATIC destination rule (dest selected as Exile pre-
    /// pipeline), so its proposed move is Stack→Exile. A board-wide Rest in
    /// Peace is scoped to `destination_zone: Graveyard` and must NOT match the
    /// stack→exile move — the flashback spell is exiled exactly once with no
    /// double-apply / redirect re-entry.
    #[test]
    fn flashback_spell_exiles_once_with_rest_in_peace_present() {
        let mut state = setup();
        install_rest_in_peace(&mut state);
        let spell = push_flashback_spell(
            &mut state,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert_eq!(
            state.objects[&spell].zone,
            Zone::Exile,
            "flashback spell still exiles via its static destination rule"
        );
        // CR 614.6: exactly one ZoneChange Stack→Exile; the RIP graveyard redirect
        // never fires (its destination scope does not match a stack→exile move),
        // so there is no second redirect move on the same object.
        let exile_moves = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::ZoneChanged { object_id, to, .. }
                        if *object_id == spell && *to == Zone::Exile
                )
            })
            .count();
        assert_eq!(
            exile_moves, 1,
            "flashback must be exiled exactly once — RIP must not double-apply on a stack→exile move"
        );
    }
}
