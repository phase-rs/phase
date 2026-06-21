use crate::game::game_object::RoomDoor;
use crate::game::room;
use crate::types::ability::{
    DoorLockOp, Effect, EffectError, EffectKind, ResolvedAbility, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// CR 709.5f-g + CR 709.5j: Resolve an `Effect::SetRoomDoorLock` — lock or
/// unlock a door (half) of the targeted Room permanent.
///
/// Resolution shape (CR 709.5f/g require choosing the eligible half at
/// resolution, so the choice is dynamic and cannot be a parse-time branch):
/// - 0 eligible doors → legal no-op (CR 609.3: do as much as possible).
/// - exactly 1 eligible (operation fixed) → apply directly, no prompt.
/// - ≥2 eligible, or a `LockOrUnlock` effect where both operations are
///   available → pause on `WaitingFor::ChooseRoomDoor`; the player's
///   `GameAction::ChooseRoomDoor` is applied by the resolution-choice handler.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let op = match &ability.effect {
        Effect::SetRoomDoorLock { op, .. } => *op,
        _ => return Ok(()),
    };

    // CR 115.1: the Room is a declared target. With "up to one target Room"
    // (Ghostly Keybearer) the player may have chosen none — a legal no-op.
    let Some(room_id) = ability.targets.iter().find_map(|t| match t {
        TargetRef::Object(id) => Some(*id),
        _ => None,
    }) else {
        return Ok(());
    };

    let eligible = room::eligible_doors(state, room_id, op);

    // CR 609.3: no eligible door (already fully unlocked for Unlock, fully
    // locked for Lock, or the target left the battlefield) — do as much as
    // possible, which is nothing.
    if eligible.is_empty() {
        emit_resolved(events, ability.source_id);
        return Ok(());
    }

    // Exactly one eligible (op,door) pair: the player has no meaningful choice,
    // so apply it directly.
    if eligible.len() == 1 {
        let (chosen_op, door) = eligible[0];
        apply_door_op(state, room_id, ability.controller, chosen_op, door, events);
        emit_resolved(events, ability.source_id);
        return Ok(());
    }

    // CR 709.5f-g: ≥2 eligible (two locked/unlocked halves, or both operations
    // available under LockOrUnlock) — the controller chooses operation + door.
    state.waiting_for = WaitingFor::ChooseRoomDoor {
        player: ability.controller,
        object_id: room_id,
        options: eligible,
    };
    emit_resolved(events, ability.source_id);
    Ok(())
}

/// CR 709.5f-g: Apply a chosen door operation through the room primitives.
/// Unlocking routes through `unlock_door_designation` so the `RoomDoorUnlocked`
/// event fires (CR 709.5h-i triggers); locking removes the designation with no
/// event (no lock-trigger class exists today).
pub(crate) fn apply_door_op(
    state: &mut GameState,
    room_id: ObjectId,
    player: PlayerId,
    op: DoorLockOp,
    door: RoomDoor,
    events: &mut Vec<GameEvent>,
) {
    match op {
        // CR 709.5f: give the chosen locked half its unlocked designation.
        DoorLockOp::Unlock => {
            room::unlock_door_designation(state, room_id, player, door, events);
        }
        // CR 709.5g: remove the chosen unlocked half's designation.
        DoorLockOp::Lock => {
            room::lock_door_designation(state, room_id, door);
        }
        // CR 709.5f + CR 709.5g: `LockOrUnlock` is resolved into a concrete
        // `Lock`/`Unlock` per door before reaching this point (see
        // `eligible_doors`), so it never appears here. Treat defensively as a
        // no-op rather than panicking on malformed input.
        DoorLockOp::LockOrUnlock => {}
    }
}

fn emit_resolved(events: &mut Vec<GameEvent>, source_id: ObjectId) {
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::SetRoomDoorLock,
        source_id,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::BackFaceData;
    use crate::types::ability::TargetFilter;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::CardId;
    use crate::types::mana::ManaCost;
    use crate::types::zones::Zone;

    /// Minimal back face so a Room's right door exists (CR 709.5j). Only the
    /// presence of `back_face` is read by `existing_doors`.
    fn room_back_face() -> BackFaceData {
        BackFaceData {
            name: "Right Door".to_string(),
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            card_types: CardType::default(),
            mana_cost: ManaCost::default(),
            keywords: Vec::new(),
            abilities: Vec::new(),
            trigger_definitions: Default::default(),
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
            color: Vec::new(),
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: Vec::new(),
            casting_options: Vec::new(),
            layout_kind: Some(crate::types::card::LayoutKind::Split),
        }
    }

    /// Build a battlefield Room. `has_back_face` adds a synthetic back face so
    /// the right door exists (CR 709.5j).
    fn make_room(state: &mut GameState, card_id: u32, has_back_face: bool) -> ObjectId {
        let id = crate::game::zones::create_object(
            state,
            CardId(card_id as u64),
            PlayerId(0),
            format!("Room {card_id}"),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Room".to_string());
        obj.room_unlocks = Some(Default::default());
        if has_back_face {
            obj.back_face = Some(room_back_face());
        }
        id
    }

    fn make_ability(op: DoorLockOp, room: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::SetRoomDoorLock {
                op,
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(room)],
            ObjectId(900),
            PlayerId(0),
        )
    }

    #[test]
    fn unlock_single_locked_door_applies_directly() {
        let mut state = GameState::new_two_player(42);
        // Left-only Room (no back face), left door locked.
        let room = make_room(&mut state, 1, false);
        let ability = make_ability(DoorLockOp::Unlock, room);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&room).unwrap();
        assert!(obj.room_unlocks.unwrap().left_unlocked);
        assert!(!matches!(
            state.waiting_for,
            WaitingFor::ChooseRoomDoor { .. }
        ));
    }

    #[test]
    fn unlock_two_locked_doors_prompts_choice() {
        let mut state = GameState::new_two_player(42);
        let room = make_room(&mut state, 1, true);
        let ability = make_ability(DoorLockOp::Unlock, room);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseRoomDoor {
                player,
                object_id,
                options,
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*object_id, room);
                assert_eq!(options.len(), 2);
                assert!(options.iter().all(|(op, _)| *op == DoorLockOp::Unlock));
            }
            other => panic!("expected ChooseRoomDoor, got {other:?}"),
        }
        // Nothing unlocked yet — the choice is pending.
        let obj = state.objects.get(&room).unwrap();
        assert!(!obj.room_unlocks.unwrap().left_unlocked);
        assert!(!obj.room_unlocks.unwrap().right_unlocked);
    }

    #[test]
    fn lock_removes_designation() {
        let mut state = GameState::new_two_player(42);
        let room = make_room(&mut state, 1, false);
        // Left door already unlocked.
        state
            .objects
            .get_mut(&room)
            .unwrap()
            .room_unlocks
            .as_mut()
            .unwrap()
            .left_unlocked = true;
        let ability = make_ability(DoorLockOp::Lock, room);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&room).unwrap();
        assert!(!obj.room_unlocks.unwrap().left_unlocked);
    }

    #[test]
    fn no_eligible_door_is_noop() {
        let mut state = GameState::new_two_player(42);
        let room = make_room(&mut state, 1, false);
        // Left already unlocked → nothing to unlock on a left-only Room.
        state
            .objects
            .get_mut(&room)
            .unwrap()
            .room_unlocks
            .as_mut()
            .unwrap()
            .left_unlocked = true;
        let ability = make_ability(DoorLockOp::Unlock, room);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!matches!(
            state.waiting_for,
            WaitingFor::ChooseRoomDoor { .. }
        ));
        let obj = state.objects.get(&room).unwrap();
        assert!(obj.room_unlocks.unwrap().left_unlocked);
    }

    #[test]
    fn no_target_is_noop() {
        let mut state = GameState::new_two_player(42);
        let _room = make_room(&mut state, 1, false);
        let mut ability = make_ability(DoorLockOp::Unlock, ObjectId(0));
        ability.targets.clear();
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(!matches!(
            state.waiting_for,
            WaitingFor::ChooseRoomDoor { .. }
        ));
    }
}
