use crate::game::effects::change_zone::{self, ZoneMoveResult};
use crate::game::stack::pop_nonresolving_top_stack_entry;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntryKind};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 724.1a / CR 724.2a: Triggered abilities that triggered before the
/// end-phase process began but have not been put onto the stack cease to exist.
pub(super) fn clear_preexisting_unstacked_triggers(state: &mut GameState) {
    let mut terminal_firings = Vec::new();
    let pending_firing = state.pending_trigger_firing.take();
    match (state.pending_trigger_entry, pending_firing) {
        (None, Some(firing)) => terminal_firings.push(firing),
        (Some(entry_id), Some(firing)) => {
            assert_eq!(
                state.stack_trigger_firings.get(&entry_id).copied(),
                Some(firing),
                "pending trigger firing must transfer to its live stack entry"
            );
        }
        (_, None) => {}
    }

    if let Some(order) = state.pending_trigger_order.take() {
        terminal_firings.extend(
            order
                .groups
                .into_iter()
                .flat_map(|group| group.triggers.into_iter().map(|context| context.firing())),
        );
    }
    terminal_firings.extend(
        std::mem::take(&mut state.deferred_triggers)
            .into_iter()
            .map(|context| context.firing()),
    );

    crate::game::triggers::release_pending_trigger_construction(state);

    for firing in terminal_firings {
        crate::game::lifecycle::record_delayed_terminal(
            firing,
            crate::game::lifecycle::DelayedTerminalDisposition::EndTurn,
        );
    }
}

/// CR 724.1b / CR 724.2b: "Exile every object on the stack, including the
/// object that's resolving."
///
/// The resolving instant or sorcery was popped from `state.stack` as it began
/// resolving, so it is found through its resolution carrier; it keeps resolving
/// from exile (CR 608.2m). A resolving ability has no object to move. Spells
/// move through the replacement-aware zone-change pipeline; non-card stack
/// entries cease to exist when removed from the stack.
pub(super) fn exile_every_stack_object(
    state: &mut GameState,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> bool {
    if let Some(resolving) =
        crate::game::stack::resolving_instant_or_sorcery_on_stack(state).map(|entry| entry.id)
    {
        if !exile_spell_from_stack(state, resolving, source_id, events) {
            return false;
        }
    }
    while let Some(removed) = pop_nonresolving_top_stack_entry(
        state,
        crate::game::lifecycle::DelayedTerminalDisposition::EndTurn,
    ) {
        let entry = removed.entry;
        if matches!(entry.kind, StackEntryKind::Spell { .. })
            && !exile_spell_from_stack(state, entry.id, source_id, events)
        {
            return false;
        }
    }
    true
}

/// Returns `false` when the move paused for a choice.
fn exile_spell_from_stack(
    state: &mut GameState,
    spell: ObjectId,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> bool {
    match change_zone::execute_zone_move(
        state,
        spell,
        Zone::Stack,
        Zone::Exile,
        source_id,
        None,
        false,
        crate::types::zones::EtbTapState::Unspecified,
        false,
        None,
        &[],
        None,
        false,
        None,
        None,
        events,
    ) {
        ZoneMoveResult::Done => true,
        ZoneMoveResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            false
        }
        ZoneMoveResult::NeedsAuraAttachmentChoice => false,
    }
}
