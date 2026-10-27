use crate::game::game_object::RoomDoor;
use crate::types::ability::DoorLockOp;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::mana::SpecialAction;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::casting::{self, SpecialActionManaPayment};
use super::engine::{EngineError, PriorityAnnouncementFacadeAccess, PriorityPrincipal};

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

/// CR 709.5 + CR 709.5a: does this type line have the two shared halves? The
/// engine marker is the `Room` subtype, which CR 709.5a puts on both halves.
/// Single authority, so the current-form and printed-form readers below cannot
/// drift apart — they differ only in WHICH type line they hand it.
fn has_shared_type_line(types: &crate::types::card_type::CardType) -> bool {
    types.subtypes.iter().any(|s| s == "Room")
}

/// The object IS a Room right now — its current, post-copy card types. Every
/// battlefield question (name, door existence, lock/unlock eligibility) asks
/// this one, because those all describe what the permanent is.
pub(crate) fn is_room(obj: &crate::game::game_object::GameObject) -> bool {
    has_shared_type_line(&obj.card_types)
}

/// CR 709.5d: the unlocked designation an entering permanent is given — "if
/// its left half was cast as a spell … if its right half was cast as a spell".
/// `None` is that rule's last sentence: neither half was cast, so it enters
/// with neither designation.
///
/// Read from the object's PRINTED form (`base_card_types`), not its current
/// one. "Its left half" is a half of THIS card; a card without a shared type
/// line has none, and no half of it can have been cast. An enter-as-copy
/// replacement (Copy Enchantment, Mirrormade) makes such a spell into a Room
/// permanent DURING entry — asking its post-copy form would answer a different
/// question and hand it a designation although nothing of its own was cast.
/// The printed form survives the copy (`apply_copiable_values` writes
/// `card_types`, never `base_card_types`), so both entry seams — the ordinary
/// resolution tail and the replacement-choice resume — can ask this same
/// question at the point they need it.
pub(crate) fn cast_half_designation(
    obj: &crate::game::game_object::GameObject,
) -> Option<RoomDoor> {
    has_shared_type_line(&obj.base_card_types).then(|| live_face_door(obj))
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
    if obj.zone != Zone::Battlefield || !is_room(obj) {
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
    if obj.zone != Zone::Battlefield || !is_room(obj) {
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

/// CR 116.2m + CR 709.5e: the non-payment preconditions for unlocking `door`,
/// returning that door's unlock cost after CR 116.2m/CR 118.7a reductions.
///
/// Kept separate from payment so a forged direct action fails before
/// auto-tapping any mana source. The cost is read once here and latched into
/// the continuation: a resumed payment must not re-derive it from a board that
/// changed while a prompt was pending.
fn validate_unlock_room_door(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    door: RoomDoor,
) -> Result<crate::types::mana::ManaCost, EngineError> {
    if state.active_player != player
        || !matches!(
            state.phase,
            crate::types::phase::Phase::PreCombatMain | crate::types::phase::Phase::PostCombatMain
        )
        || !state.stack.is_empty()
    {
        return Err(EngineError::ActionNotAllowed(
            "Room doors can be unlocked only as a main-phase special action with an empty stack"
                .to_string(),
        ));
    }

    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Room not found".to_string()))?;
    if obj.controller != player || obj.zone != Zone::Battlefield {
        return Err(EngineError::ActionNotAllowed(
            "Only the controller of a battlefield Room can unlock it".to_string(),
        ));
    }
    if !is_room(obj) {
        return Err(EngineError::ActionNotAllowed(
            "Object is not a Room".to_string(),
        ));
    }
    if obj.room_unlocks.unwrap_or_default().is_unlocked(door) {
        return Err(EngineError::ActionNotAllowed(
            "That door is already unlocked".to_string(),
        ));
    }
    // CR 709.5e + CR 707.2: the unlock cost is the locked HALF's mana cost,
    // read from the EFFECTIVE halves (printed order) — the copied snapshot
    // when a copy effect applies, else the object's own printed halves
    // (whose orientation `own_room_halves` resolves through `live_face_door`,
    // the same CR 709.5d mapping as before).
    let halves = effective_room_halves(obj);
    let cost = match door {
        RoomDoor::Left => halves.left.mana_cost.clone(),
        RoomDoor::Right => halves
            .right
            .as_ref()
            .map(|half| half.mana_cost.clone())
            .ok_or_else(|| {
                EngineError::ActionNotAllowed("Room has no second door face".to_string())
            })?,
    };

    // CR 116.2m + CR 118.7a: Reduce the door's generic unlock cost by the
    // player's active `ReduceActionCost { action: UnlockDoor }` statics
    // (Inquisitive Glimmer — "Unlock costs you pay cost {1} less") before
    // payment. Single authority shared with the plot path.
    Ok(casting::apply_special_action_cost_reduction(
        state,
        player,
        SpecialAction::UnlockDoor,
        cost,
    ))
}

/// CR 116.2m + CR 709.5e + CR 605.3b + CR 616.1: pay the latched unlock cost
/// through the Room-unlock continuation.
///
/// `PaymentContext::SpecialAction(UnlockDoor)` is what makes spend-restricted
/// mana ("only to … unlock doors", Smoky Lounge) eligible here and
/// spell/activation-restricted mana correctly ineligible. A paused mana-source
/// cost move or a pre-spend echo choice leaves the door LOCKED and surfaces
/// that prompt; only a completed payment commits the designation.
fn pay_room_unlock_cost(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    door: RoomDoor,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<SpecialActionManaPayment, EngineError> {
    let resume = crate::types::game_state::ManaAbilityResume::RoomUnlock {
        player,
        object_id,
        door,
        cost: cost.clone(),
    };
    casting::pay_special_action_mana_cost_with_resume(
        state,
        player,
        Some(object_id),
        cost,
        SpecialAction::UnlockDoor,
        Some(&resume),
        events,
    )
}

/// CR 116.2m: initiate the unlock special action. Preflight (timing, control,
/// locked door, cost) happens before automatic mana activation, so an invalid
/// or unaffordable action cannot mutate mana, waiting state, or door designations.
pub(crate) fn handle_unlock_room_door(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    door: RoomDoor,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let cost = validate_unlock_room_door(state, player, object_id, door)?;
    match pay_room_unlock_cost(state, player, object_id, door, &cost, events)? {
        SpecialActionManaPayment::Paid => Ok(finish_paid_room_unlock(
            state, player, object_id, door, events,
        )),
        SpecialActionManaPayment::Paused => Ok(state.waiting_for.clone()),
    }
}

/// CR 116.2m + CR 605.3b + CR 616.1: resume an unlock whose cost move or
/// pre-spend echo choice has been answered. `cost` was latched at announcement
/// and is not re-derived; a second pause keeps the same typed root.
pub(crate) fn resume_room_unlock_payment(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    door: RoomDoor,
    cost: crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    match pay_room_unlock_cost(state, player, object_id, door, &cost, events)? {
        SpecialActionManaPayment::Paid => Ok(finish_paid_room_unlock(
            state, player, object_id, door, events,
        )),
        SpecialActionManaPayment::Paused => Ok(state.waiting_for.clone()),
    }
}

/// CR 116.2m + CR 709.5c: commit the unlocked designation only after full
/// payment succeeds. Returning `WaitingFor::Priority` is what lets the ordinary
/// reducer epilogue converge the special action at settled priority.
pub(crate) fn finish_paid_room_unlock(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    door: RoomDoor,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    unlock_door_designation(state, object_id, player, door, events);
    WaitingFor::Priority { player }
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
    if obj.zone != Zone::Battlefield || !is_room(obj) {
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
    if obj.zone != Zone::Battlefield || !is_room(obj) {
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
