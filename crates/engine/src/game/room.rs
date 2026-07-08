use crate::game::game_object::RoomDoor;
use crate::types::ability::DoorLockOp;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 709.5j: A "door" is a half of a Room permanent. A Room has a left door
/// always and a right door only if it has a back face (the second half of the
/// split card). Returns the doors that actually exist for `object_id`.
fn existing_doors(state: &GameState, object_id: ObjectId) -> Vec<RoomDoor> {
    match state.objects.get(&object_id) {
        // CR 709.5j: the right door is the back face's half — absent on a Room
        // printed without a second half.
        Some(obj) if obj.back_face.is_some() => vec![RoomDoor::Left, RoomDoor::Right],
        Some(_) => vec![RoomDoor::Left],
        None => Vec::new(),
    }
}

/// CR 709.5f-g: The doors of `object_id` eligible for the given operation —
/// locked halves are eligible to be unlocked (CR 709.5f), unlocked halves are
/// eligible to be locked (CR 709.5g). `LockOrUnlock` (Keys to the House, Marina
/// Vendrell) is the union of both: a locked half is offered as an `Unlock`
/// option and an unlocked half as a `Lock` option, so the same door can appear
/// once per applicable operation.
///
/// Single authority for door eligibility — the resolver and the AI candidate
/// generator both call this so the offered set never diverges.
pub fn eligible_doors(
    state: &GameState,
    object_id: ObjectId,
    op: DoorLockOp,
) -> Vec<(DoorLockOp, RoomDoor)> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };
    // CR 709.5f-g: only a battlefield Room has lockable/unlockable doors.
    if obj.zone != Zone::Battlefield || !obj.card_types.subtypes.iter().any(|s| s == "Room") {
        return Vec::new();
    }
    let unlocks = obj.room_unlocks.unwrap_or_default();
    let mut out = Vec::new();
    for door in existing_doors(state, object_id) {
        let is_unlocked = unlocks.is_unlocked(door);
        match op {
            // CR 709.5f: unlock chooses among the locked halves.
            DoorLockOp::Unlock => {
                if !is_unlocked {
                    out.push((DoorLockOp::Unlock, door));
                }
            }
            // CR 709.5g: lock chooses among the unlocked halves.
            DoorLockOp::Lock => {
                if is_unlocked {
                    out.push((DoorLockOp::Lock, door));
                }
            }
            // CR 709.5f + CR 709.5g: offer each door under whichever operation
            // its current state permits.
            DoorLockOp::LockOrUnlock => {
                if is_unlocked {
                    out.push((DoorLockOp::Lock, door));
                } else {
                    out.push((DoorLockOp::Unlock, door));
                }
            }
        }
    }
    out
}

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

/// CR 709.5g: Remove an unlocked designation from a Room permanent. Returns
/// whether a designation was actually removed. Mirror of
/// [`unlock_door_designation`]; no event is emitted because no trigger class in
/// the current card pool fires on a door being locked (unlike CR 709.5h-i for
/// unlocking). A `RoomDoorLocked` event can be added here if such a card appears.
pub fn lock_door_designation(state: &mut GameState, object_id: ObjectId, door: RoomDoor) -> bool {
    let Some(obj) = state.objects.get_mut(&object_id) else {
        return false;
    };
    if obj.zone != Zone::Battlefield || !obj.card_types.subtypes.iter().any(|s| s == "Room") {
        return false;
    }

    let room_state = obj.room_unlocks.get_or_insert_with(Default::default);
    room_state.lock(door)
}
