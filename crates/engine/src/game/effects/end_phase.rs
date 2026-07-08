use crate::game::effects::change_zone::{self, ZoneMoveResult};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntryKind};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 724.1a / CR 724.2a: Triggered abilities that triggered before the
/// end-phase process began but have not been put onto the stack cease to exist.
pub(super) fn clear_preexisting_unstacked_triggers(state: &mut GameState) {
    state.pending_trigger = None;
    state.pending_trigger_entry = None;
    state.pending_trigger_order = None;
    state.pending_trigger_event_batch.clear();
    state.deferred_triggers.clear();
}

/// CR 724.1b / CR 724.2b: Exile every non-resolving object still on the stack.
///
/// `resolve_top` has already popped the resolving object before effect
/// execution and routes that object after the resolver returns. Spell entries
/// move through the replacement-aware zone-change pipeline; non-card stack
/// entries cease to exist when removed from the stack.
pub(super) fn exile_nonresolving_stack_objects(
    state: &mut GameState,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> bool {
    while let Some(entry) = state.stack.pop_back() {
        state.stack_paid_facts.remove(&entry.id);
        state.stack_trigger_event_batches.remove(&entry.id);
        if matches!(entry.kind, StackEntryKind::Spell { .. }) {
            match change_zone::execute_zone_move(
                state,
                entry.id,
                Zone::Stack,
                Zone::Exile,
                source_id,
                None,
                false,
                crate::types::zones::EtbTapState::Unspecified,
                None,
                &[],
                None,
                false,
                None,
                None,
                events,
            ) {
                ZoneMoveResult::Done => {}
                ZoneMoveResult::NeedsChoice(player) => {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    return false;
                }
                ZoneMoveResult::NeedsAuraAttachmentChoice => return false,
            }
        }
    }
    true
}
