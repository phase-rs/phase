use crate::game::game_object::RoomDoor;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 709.5c-f: Give a Room permanent an unlocked designation and emit the
/// corresponding trigger event. Returns whether a new designation was gained.
pub fn unlock_door_designation(
    state: &mut GameState,
    object_id: ObjectId,
    player: PlayerId,
    door: RoomDoor,
    events: &mut Vec<GameEvent>,
) -> bool {
    let Some(obj) = state.objects.get_mut(&object_id) else {
        return false;
    };
    if obj.zone != Zone::Battlefield || !obj.card_types.subtypes.iter().any(|s| s == "Room") {
        return false;
    }

    let room_state = obj.room_unlocks.get_or_insert_with(Default::default);
    let outcome = room_state.unlock(door);
    if outcome.changed {
        events.push(GameEvent::RoomDoorUnlocked {
            player_id: player,
            object_id,
            door,
            fully_unlocked: outcome.fully_unlocked,
        });
    }
    outcome.changed
}
