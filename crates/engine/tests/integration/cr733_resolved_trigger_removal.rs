//! CR733 P2 coverage for the CR 603.3d uncommitted-trigger removal.
//!
//! CR 603.3d: "If a choice is required when the triggered ability goes on the
//! stack but no legal choices can be made for it, or if a rule or a continuous
//! effect otherwise makes the ability illegal, the ability is simply removed
//! from the stack."
//!
//! The engine reaches that removal through the "push first, choose second"
//! invariant: a triggered ability is PUT on the stack and only then are its
//! choices gathered, with `pending_trigger_entry` marking the entry whose slots
//! are still unfilled. Declining an optional "you may" trigger is the production
//! path that abandons one, and both call sites funnel through the single
//! authority `stack::pop_uncommitted_pending_trigger_entry`.
//!
//! FIXTURE NOTE — the trigger must be optional AND MODAL, and both halves were
//! established by measuring which shapes actually reach the authority:
//!
//! * `you may choose one — ...` (this card) — reaches it; the entry is popped.
//! * `choose one — ...` (modal, NOT optional) — reaches the mid-construction
//!   state but is never removed, because there is no may-offer to decline. This
//!   is the control: it proves the removal is driven by the DECLINE, not merely
//!   by a trigger being modal.
//! * `you may destroy target creature` — never reaches the authority at all. A
//!   "you may" attached to an effect is resolved under CR 608.2d when the
//!   ability RESOLVES; the ability is fully constructed when pushed, so
//!   declining it makes it do nothing rather than removing it from the stack.
//!
//! So the may-offer must precede the MODE choice, which is what makes
//! construction genuinely unable to complete (CR 603.3c). Do not drop the
//! "you may", and do not swap the modal for a targeted effect — either change
//! makes every assertion in this file vacuous.
//!
//! The authority has TWO outcomes and both mutate state, which is why the
//! command carries `removed: Option<_>` rather than a bare entry: the cursor is
//! consumed UNCONDITIONALLY, and only then is the pop guarded on the entry still
//! being topmost. A command modelling only the popping case would leave a replay
//! of the other outcome still holding a cursor the real execution cleared.

use engine::game::scenario::{GameScenario, P0};
use engine::game::stack::apply_resolved_uncommitted_trigger_removal;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::resolved_commands::{
    ResolvedRulesCommand, ResolvedUncommittedTriggerRemovalCommand,
    ResolvedUncommittedTriggerRemovalReplayInvariantError,
};

const MAY_MODAL_ORACLE: &str =
    "When this creature enters, you may choose one — Draw a card; or you gain 3 life.";

/// Every removal journaled in `state`, in journal order.
fn removals(state: &GameState) -> Vec<ResolvedUncommittedTriggerRemovalCommand> {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|entry| entry.command.as_ref())
        .filter_map(|command| match command {
            ResolvedRulesCommand::UncommittedTriggerRemoval(command) => {
                Some(command.as_ref().clone())
            }
            _ => None,
        })
        .collect()
}

/// Casts a creature whose ETB is an optional targeted trigger and DECLINES it,
/// which is what drives the CR 603.3d removal through production.
fn declined_optional_trigger_state() -> GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature_to_hand_from_oracle(P0, "Journal Optional", 2, 2, MAY_MODAL_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    // A legal target must exist, or the trigger is dropped for want of targets
    // before it can reach the optional offer — a different code path.
    for name in ["Alpha", "Beta"] {
        scenario.add_card_to_library_top(P0, name);
    }

    let mut runner = scenario.build();
    runner.cast(creature).decline_optional().resolve();
    runner.state().clone()
}

/// Rebuilds the pre-removal predecessor from what the command says it removed.
///
/// The predecessor is mid-construction — after the trigger was pushed, before
/// the decline — so no fixture can capture it directly. Reconstructing it from
/// the recorded values is what makes the replay assertion meaningful: if the
/// applier touched anything the command did not record, the round trip would not
/// land back on the observed post-state.
fn pre_removal_state(
    state: &GameState,
    command: &ResolvedUncommittedTriggerRemovalCommand,
) -> GameState {
    let mut predecessor = state.clone();
    predecessor.pending_trigger_entry = Some(command.consumed_entry_id);
    if let Some(removed) = command.removed.as_deref() {
        predecessor.stack.push_back(removed.clone());
    }
    predecessor
}

#[test]
fn declining_an_optional_trigger_journals_an_exact_removal() {
    let state = declined_optional_trigger_state();

    // The discriminating assertion: the removal is journaled. A raw pop records
    // nothing here. This doubles as the reach guard — it fails if the fixture
    // never reached the mid-construction state at all.
    let commands = removals(&state);
    assert_eq!(
        commands.len(),
        1,
        "declining the optional trigger must journal exactly one CR 603.3d removal"
    );
    let command = &commands[0];
    let removed = command
        .removed
        .as_ref()
        .expect("the declined entry was on top, so the guard held and it was popped");
    assert_eq!(
        removed.id, command.consumed_entry_id,
        "the recorded entry is the one the cursor named"
    );

    // CR 603.3d post-state: the ability is gone and the cursor is consumed.
    assert!(
        !state
            .stack
            .iter()
            .any(|e| e.id == command.consumed_entry_id),
        "the declined trigger must be removed from the stack"
    );
    assert_eq!(
        state.pending_trigger_entry, None,
        "the cursor is consumed by the removal"
    );
    assert_eq!(
        command.resulting_depth,
        state.stack.len(),
        "CR 405.2: the recorded depth is the depth after the removal"
    );

    // Replay-exactness: from the reconstructed predecessor, applying the record
    // reproduces the removal with nothing re-derived.
    let mut replay = pre_removal_state(&state, command);
    apply_resolved_uncommitted_trigger_removal(&mut replay, command)
        .expect("the recorded removal must replay against its predecessor");
    assert!(
        !replay
            .stack
            .iter()
            .any(|e| e.id == command.consumed_entry_id),
        "replay removes the recorded entry"
    );
    assert_eq!(
        replay.pending_trigger_entry, None,
        "replay clears the cursor"
    );
    assert!(
        !replay
            .stack_paid_facts
            .contains_key(&command.consumed_entry_id),
        "replay drops the paid-facts row keyed on the removed entry"
    );
    assert!(
        !replay
            .stack_trigger_event_batches
            .contains_key(&command.consumed_entry_id),
        "replay drops the trigger-batch row keyed on the removed entry"
    );
    assert_eq!(
        replay.stack.len(),
        command.resulting_depth,
        "replay lands on the recorded depth"
    );

    // Re-applying is not idempotent: the cursor is gone, so it fails closed
    // rather than popping an unrelated entry.
    assert!(
        matches!(
            apply_resolved_uncommitted_trigger_removal(&mut replay, command),
            Err(ResolvedUncommittedTriggerRemovalReplayInvariantError::CursorMismatch { .. })
        ),
        "a second application must fail closed on the cursor precondition"
    );
}

/// Each probe diverges exactly one axis, so a rejection can only come from the
/// precondition being probed.
#[test]
fn removal_rejects_a_divergent_predecessor() {
    let state = declined_optional_trigger_state();
    let commands = removals(&state);
    let command = &commands[0];

    // Cursor names a different entry.
    let mut wrong_cursor = pre_removal_state(&state, command);
    wrong_cursor.pending_trigger_entry = Some(ObjectId(9999));
    assert!(matches!(
        apply_resolved_uncommitted_trigger_removal(&mut wrong_cursor, command),
        Err(ResolvedUncommittedTriggerRemovalReplayInvariantError::CursorMismatch { .. })
    ));

    // Cursor correct, but the entry on top is not the recorded one. Comparing
    // the entry WHOLE rather than by id is what catches this — an applier that
    // matched on `id` alone would happily discard a divergent object.
    let mut wrong_top = pre_removal_state(&state, command);
    wrong_top
        .stack
        .back_mut()
        .expect("the reconstructed predecessor has the entry on top")
        .source_id = ObjectId(4242);
    assert!(matches!(
        apply_resolved_uncommitted_trigger_removal(&mut wrong_top, command),
        Err(ResolvedUncommittedTriggerRemovalReplayInvariantError::RemovedEntryMismatch)
    ));
    assert_eq!(
        wrong_top.pending_trigger_entry,
        Some(command.consumed_entry_id),
        "the rejected replay must not have consumed the cursor"
    );
}

/// The `removed: None` outcome — cursor consumed, nothing popped.
///
/// This arm exists because `.take()` runs unconditionally while the pop is
/// guarded. A command recording no pop must ALSO refuse a predecessor where the
/// entry is still on top: replaying it there would clear the cursor and strand
/// the entry, a state the original execution never produced and which nothing
/// would otherwise report.
#[test]
fn a_removal_that_popped_nothing_still_clears_the_cursor() {
    let state = declined_optional_trigger_state();
    let commands = removals(&state);
    let popping = &commands[0];

    let no_pop = ResolvedUncommittedTriggerRemovalCommand {
        consumed_entry_id: popping.consumed_entry_id,
        removed: None,
        resulting_depth: state.stack.len(),
        cause: popping.cause,
    };

    // Predecessor whose top IS that entry: a no-pop record is a divergence.
    let mut still_present = pre_removal_state(&state, popping);
    assert!(
        matches!(
            apply_resolved_uncommitted_trigger_removal(&mut still_present, &no_pop),
            Err(ResolvedUncommittedTriggerRemovalReplayInvariantError::DepthMismatch { .. })
                | Err(
                    ResolvedUncommittedTriggerRemovalReplayInvariantError::UnexpectedRemovableEntry(
                        _
                    )
                )
        ),
        "a no-pop record must not apply where the entry is still removable"
    );
    assert_eq!(
        still_present.pending_trigger_entry,
        Some(popping.consumed_entry_id),
        "the rejected replay must not have consumed the cursor"
    );

    // Predecessor where the entry has already gone: the same record applies and
    // clears the cursor without touching the stack.
    let mut already_gone = state.clone();
    already_gone.pending_trigger_entry = Some(popping.consumed_entry_id);
    let depth_before = already_gone.stack.len();
    apply_resolved_uncommitted_trigger_removal(&mut already_gone, &no_pop)
        .expect("a no-pop removal replays where the entry is already gone");
    assert_eq!(
        already_gone.pending_trigger_entry, None,
        "the cursor is consumed even when nothing was popped"
    );
    assert_eq!(
        already_gone.stack.len(),
        depth_before,
        "a no-pop removal must not touch the stack"
    );
}
