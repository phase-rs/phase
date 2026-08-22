use crate::game::game_object::RoomDoor;
use crate::types::ability::DoorLockOp;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::engine::{PriorityAnnouncementFacadeAccess, PriorityPrincipal};

/// An engine-authored Room-door unlock announcement for Priority preflight.
/// The Room identity and locked door stay private to the Room authority until
/// the facade reconstructs the ordinary special-action primer.
pub(in crate::game) struct PriorityUnlockRoomDoorAnnouncement {
    object_id: ObjectId,
    door: RoomDoor,
}

impl PriorityUnlockRoomDoorAnnouncement {
    fn new(object_id: ObjectId, door: RoomDoor) -> Self {
        Self { object_id, door }
    }

    pub(in crate::game) fn object_id(
        &self,
        _access: &PriorityAnnouncementFacadeAccess,
    ) -> ObjectId {
        self.object_id
    }

    pub(in crate::game) fn door(&self, _access: &PriorityAnnouncementFacadeAccess) -> RoomDoor {
        self.door
    }
}

/// Enumerates the active holder's locked Room doors during the normal
/// main-phase, empty-stack special-action window.
pub(in crate::game) fn priority_unlock_room_door_announcements(
    state: &GameState,
    principal: &PriorityPrincipal,
) -> Vec<PriorityUnlockRoomDoorAnnouncement> {
    let player = principal.semantic_holder();
    if state.active_player != player
        || !state.stack.is_empty()
        || !matches!(
            state.phase,
            crate::types::phase::Phase::PreCombatMain | crate::types::phase::Phase::PostCombatMain
        )
    {
        return Vec::new();
    }
    state
        .battlefield
        .iter()
        .copied()
        .filter(|object_id| {
            state
                .objects
                .get(object_id)
                .is_some_and(|object| object.controller == player)
        })
        .flat_map(|object_id| {
            eligible_doors(state, object_id, DoorLockOp::Unlock)
                .into_iter()
                .map(move |(_, door)| PriorityUnlockRoomDoorAnnouncement::new(object_id, door))
        })
        .collect()
}

/// CR 709.5j + CR 709.5d: The door (printed half) the object's LIVE face is.
/// `modal_back_face` records that the right (second printed) half was cast —
/// the same mapping CR 709.5d entry-unlocking uses. Single authority: the
/// unlock-cost lookup and the door-text install both resolve through this.
pub fn live_face_door(obj: &crate::game::game_object::GameObject) -> RoomDoor {
    if obj.modal_back_face {
        RoomDoor::Right
    } else {
        RoomDoor::Left
    }
}

/// CR 709.5 + CR 709.5c: On the battlefield a locked half doesn't have its
/// rules text, so a door-stamped definition (trigger or static) functions only
/// while its half is unlocked — and stops again when that half is re-locked
/// (CR 709.5g). `None` means the definition is not Room-half text: no gating.
/// Off the battlefield there are no lock designations and both halves' text
/// exists, so the gate applies only there. Single authority shared by the
/// trigger iterator and every statics gather.
pub(crate) fn door_text_functions(
    obj: &crate::game::game_object::GameObject,
    door: Option<RoomDoor>,
) -> bool {
    let Some(door) = door else {
        return true;
    };
    if obj.zone != Zone::Battlefield {
        return true;
    }
    obj.room_unlocks.unwrap_or_default().is_unlocked(door)
}

/// CR 709.5b + CR 707.2: the halves the object's OWN printed form provides, in
/// printed order. Engine representation: the live face's identity sits in the
/// `base_*` fields and the other printed half in the `back_face` slot;
/// `live_face_door` (the single orientation authority) maps the two slots back
/// to printed order. Room-ness is the CALLER's gate — every consumer
/// (`effective_room_halves` behind the handlers' live-type checks, and the
/// copiable snapshot behind its own base-type check) verifies the object is a
/// Room before asking; deriving unconditionally keeps this a pure projection.
pub(crate) fn own_room_halves(
    obj: &crate::game::game_object::GameObject,
) -> crate::types::ability::RoomCopiableHalves {
    use crate::types::ability::{RoomCopiableHalves, RoomHalfIdentity};
    let live = RoomHalfIdentity {
        name: obj.base_name.clone(),
        mana_cost: obj.base_mana_cost.clone(),
    };
    let back = obj.back_face.as_ref().map(|back| RoomHalfIdentity {
        name: back.name.clone(),
        mana_cost: back.mana_cost.clone(),
    });
    match live_face_door(obj) {
        RoomDoor::Left => RoomCopiableHalves {
            left: live,
            right: back,
        },
        RoomDoor::Right => match back {
            Some(back) => RoomCopiableHalves {
                left: back,
                right: Some(live),
            },
            // A right-cast orientation implies a printed left half; fall back
            // defensively to the live half alone rather than inventing one.
            None => RoomCopiableHalves {
                left: live,
                right: None,
            },
        },
    }
}

/// CR 707.2 + CR 613.1a: the halves the object EFFECTIVELY has — the copied
/// snapshot when a Layer-1a copy effect applied one (set by
/// `apply_copiable_values`, cleared by the Step-1 seed, so it expires with the
/// copy), else the object's own printed halves. Single authority for every
/// per-half question: the door-gated name, door unlock costs, and which doors
/// exist.
pub(crate) fn effective_room_halves(
    obj: &crate::game::game_object::GameObject,
) -> crate::types::ability::RoomCopiableHalves {
    obj.copied_room_halves
        .clone()
        .unwrap_or_else(|| own_room_halves(obj))
}

/// CR 709.5: on the battlefield a locked half doesn't have its NAME. A Room
/// permanent's name is therefore the printed-order combination of its
/// unlocked halves — both → "Left // Right", one → that half alone, neither →
/// no name at all (CR 709.5d: an uncast Room enters fully locked). `None` for
/// every object the rule doesn't reach (not a Room by its CURRENT, post-copy
/// card types; not on the battlefield): callers keep their layer-derived name.
///
/// CR 707.2: the halves come from `effective_room_halves`, so a copy shows the
/// COPIED halves through its own designations (designations are status,
/// CR 709.5c — they are never copied and they survive copy expiry).
pub(crate) fn door_gated_battlefield_name(
    obj: &crate::game::game_object::GameObject,
) -> Option<String> {
    if obj.zone != Zone::Battlefield || !obj.card_types.subtypes.iter().any(|s| s == "Room") {
        return None;
    }
    let halves = effective_room_halves(obj);
    let unlocks = obj.room_unlocks.unwrap_or_default();
    let left = unlocks
        .is_unlocked(RoomDoor::Left)
        .then_some(halves.left.name.as_str());
    let right = halves
        .right
        .as_ref()
        .filter(|_| unlocks.is_unlocked(RoomDoor::Right))
        .map(|half| half.name.as_str());
    Some(match (left, right) {
        (Some(left), Some(right)) => format!("{left} // {right}"),
        (Some(half), None) | (None, Some(half)) => half.to_string(),
        (None, None) => String::new(),
    })
}

/// CR 709.5j: A "door" is a half of a Room permanent. A Room has a left door
/// always and a right door only if it has a back face (the second half of the
/// split card). Returns the doors that actually exist for `object_id`.
fn existing_doors(state: &GameState, object_id: ObjectId) -> Vec<RoomDoor> {
    match state.objects.get(&object_id) {
        // CR 709.5j + CR 707.2: the right door is the second half's — read from
        // the EFFECTIVE halves so a copy of a two-halved Room has both doors
        // and a copy of a single-halved Room only the left one.
        Some(obj) => {
            if effective_room_halves(obj).right.is_some() {
                vec![RoomDoor::Left, RoomDoor::Right]
            } else {
                vec![RoomDoor::Left]
            }
        }
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
        // CR 709.5 + CR 613: a gained designation changes the door-gated name,
        // a layer-derived characteristic — re-derive (mirror of transform.rs).
        crate::game::layers::mark_layers_full(state);
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
    let changed = room_state.lock(door);
    if changed {
        // CR 709.5g + CR 613: a lost designation changes the door-gated name —
        // re-derive layered characteristics (mirror of `unlock_door_designation`).
        crate::game::layers::mark_layers_full(state);
    }
    changed
}
