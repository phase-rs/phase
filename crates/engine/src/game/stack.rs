use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntry};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

use super::zones;

pub fn push_to_stack(
    state: &mut GameState,
    entry: StackEntry,
    events: &mut Vec<GameEvent>,
) {
    events.push(GameEvent::StackPushed {
        object_id: entry.id,
    });
    state.stack.push(entry);
}

pub fn resolve_top(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let entry = match state.stack.pop() {
        Some(e) => e,
        None => return,
    };

    // Determine destination zone based on card type
    let dest = if is_permanent_type(state, entry.id) {
        Zone::Battlefield
    } else {
        Zone::Graveyard
    };

    zones::move_to_zone(state, entry.id, dest, events);

    events.push(GameEvent::StackResolved {
        object_id: entry.id,
    });
}

pub fn stack_is_empty(state: &GameState) -> bool {
    state.stack.is_empty()
}

fn is_permanent_type(state: &GameState, object_id: ObjectId) -> bool {
    use crate::types::card_type::CoreType;

    let obj = match state.objects.get(&object_id) {
        Some(o) => o,
        None => return false,
    };

    obj.card_types.core_types.iter().any(|ct| {
        matches!(
            ct,
            CoreType::Creature
                | CoreType::Artifact
                | CoreType::Enchantment
                | CoreType::Planeswalker
                | CoreType::Land
        )
    })
}
