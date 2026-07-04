//! Stage 1 runtime coverage for the Rooms lock/unlock-door effect
//! (`Effect::SetRoomDoorLock`, CR 709.5f-j).
//!
//! These tests drive the real production resolution path
//! (`resolve_ability_chain` → `set_room_door_lock::resolve`) and the real
//! interactive-choice submission path (`apply_as_current` →
//! `engine_resolution_choices::handle_resolution_choice`). The effect is
//! constructed programmatically — the Oracle-text parser arm is Stage 2 and is
//! deliberately NOT exercised here.
//!
//! Each test names a revert-failing assertion in its doc comment.

use engine::game::effects::resolve_ability_chain;
use engine::game::game_object::{BackFaceData, RoomDoor};
use engine::game::triggers::process_triggers;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, DoorLockOp, Effect, QuantityExpr, ResolvedAbility,
    TargetFilter, TargetRef, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

/// Minimal back face so a Room's right door exists (CR 709.5j). Only the
/// presence of `back_face` is consulted by `room::eligible_doors`.
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
        layout_kind: Some(LayoutKind::Split),
    }
}

/// Battlefield Room controlled by P0. `(left_unlocked, right_unlocked)` set the
/// initial designations; `has_back_face` controls whether a right door exists.
fn make_room(
    state: &mut GameState,
    card_id: u32,
    has_back_face: bool,
    left_unlocked: bool,
    right_unlocked: bool,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id as u64),
        P0,
        format!("Room {card_id}"),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.card_types.subtypes.push("Room".to_string());
    obj.room_unlocks = Some(engine::game::game_object::RoomUnlockState {
        left_unlocked,
        right_unlocked,
    });
    if has_back_face {
        obj.back_face = Some(room_back_face());
    }
    id
}

fn lock_unlock_ability(op: DoorLockOp, room: ObjectId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::SetRoomDoorLock {
            op,
            target: TargetFilter::Any,
        },
        vec![TargetRef::Object(room)],
        ObjectId(900),
        P0,
    )
}

/// CR 709.5f: A left-only Room with its single locked door is unlocked directly
/// — no choice prompt, since there is exactly one eligible door.
///
/// Revert-failing assertion: `left_unlocked == true`. Reverting the resolver or
/// the `unlock_door_designation` routing leaves the door locked.
#[test]
fn unlock_single_locked_door_applies_without_prompt() {
    let mut state = GameState::new_two_player(42);
    let room = make_room(&mut state, 1, false, false, false);
    let ability = lock_unlock_ability(DoorLockOp::Unlock, room);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("resolves");

    assert!(
        state.objects[&room].room_unlocks.unwrap().left_unlocked,
        "the single locked door must be unlocked"
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::ChooseRoomDoor { .. }),
        "a single eligible door must not prompt"
    );
}

/// CR 709.5f: Two locked doors → the resolver pauses on
/// `WaitingFor::ChooseRoomDoor`; submitting `GameAction::ChooseRoomDoor` through
/// the real `apply()` boundary unlocks the chosen door and leaves the other
/// locked.
///
/// Revert-failing assertions: (1) `WaitingFor::ChooseRoomDoor` is set with both
/// doors as options; (2) after submitting the Right choice, Right is unlocked
/// and Left is still locked. Reverting the choice-prompt branch, the `handles()`
/// routing, or the choice handler breaks one of these.
#[test]
fn two_locked_doors_prompt_then_submit_choice() {
    let mut state = GameState::new_two_player(42);
    let room = make_room(&mut state, 1, true, false, false);
    let ability = lock_unlock_ability(DoorLockOp::Unlock, room);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("resolves");

    match &state.waiting_for {
        WaitingFor::ChooseRoomDoor {
            player,
            object_id,
            options,
        } => {
            assert_eq!(*player, P0);
            assert_eq!(*object_id, room);
            assert_eq!(options.len(), 2, "both locked doors must be offered");
            assert!(options.contains(&(DoorLockOp::Unlock, RoomDoor::Left)));
            assert!(options.contains(&(DoorLockOp::Unlock, RoomDoor::Right)));
        }
        other => panic!("expected ChooseRoomDoor, got {other:?}"),
    }

    // Submit the Right-door choice through the real apply() path.
    engine::game::apply_as_current(
        &mut state,
        GameAction::ChooseRoomDoor {
            object_id: room,
            op: DoorLockOp::Unlock,
            door: RoomDoor::Right,
        },
    )
    .expect("submitting a valid (op, door) choice succeeds");

    let unlocks = state.objects[&room].room_unlocks.unwrap();
    assert!(unlocks.right_unlocked, "the chosen door must be unlocked");
    assert!(
        !unlocks.left_unlocked,
        "the unchosen door must remain locked"
    );
}

/// An invalid (op, door) submission — a pair not in the prompt's `options` —
/// is rejected by the engine's choice validation (not a CR rule, an engine
/// integrity guard against spoofed payloads).
///
/// Revert-failing assertion: the call returns `Err`. Reverting the
/// `options.contains` validation would accept the illegal door.
#[test]
fn invalid_choice_is_rejected() {
    let mut state = GameState::new_two_player(42);
    let room = make_room(&mut state, 1, true, false, false);
    let ability = lock_unlock_ability(DoorLockOp::Unlock, room);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("resolves");

    // Lock is not an offered operation here (both doors are locked → Unlock-only).
    let result = engine::game::apply_as_current(
        &mut state,
        GameAction::ChooseRoomDoor {
            object_id: room,
            op: DoorLockOp::Lock,
            door: RoomDoor::Left,
        },
    );
    assert!(result.is_err(), "a non-offered (op, door) must be rejected");
}

/// CR 709.5g: Locking removes an unlocked designation. A left-only Room whose
/// single door is unlocked is locked directly (one eligible door).
///
/// Revert-failing assertion: `left_unlocked` flips true → false. Reverting the
/// `RoomUnlockState::lock` primitive or the `Lock` resolver arm leaves it true.
#[test]
fn lock_removes_unlocked_designation() {
    let mut state = GameState::new_two_player(42);
    let room = make_room(&mut state, 1, false, true, false);
    let ability = lock_unlock_ability(DoorLockOp::Lock, room);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("resolves");

    assert!(
        !state.objects[&room].room_unlocks.unwrap().left_unlocked,
        "locking must remove the unlocked designation (CR 709.5g)"
    );
}

/// CR 709.5f + CR 709.5g: A `LockOrUnlock` effect on a Room with one unlocked
/// and one locked door offers BOTH operations — a `Lock` on the unlocked half
/// and an `Unlock` on the locked half.
///
/// Revert-failing assertion: the options contain both a `Lock` and an `Unlock`
/// entry. Reverting the `LockOrUnlock` arm in `eligible_doors` collapses this to
/// a single operation.
#[test]
fn lock_or_unlock_offers_both_operations() {
    let mut state = GameState::new_two_player(42);
    // Left unlocked (lockable), Right locked (unlockable).
    let room = make_room(&mut state, 1, true, true, false);
    let ability = lock_unlock_ability(DoorLockOp::LockOrUnlock, room);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("resolves");

    match &state.waiting_for {
        WaitingFor::ChooseRoomDoor { options, .. } => {
            assert!(
                options.contains(&(DoorLockOp::Lock, RoomDoor::Left)),
                "the unlocked left half must be offered for Lock"
            );
            assert!(
                options.contains(&(DoorLockOp::Unlock, RoomDoor::Right)),
                "the locked right half must be offered for Unlock"
            );
        }
        other => panic!("expected ChooseRoomDoor, got {other:?}"),
    }
}

/// CR 709.5f + CR 609.3: A `LockOrUnlock` effect whose Room has exactly one
/// eligible door under exactly one operation auto-applies that operation — no
/// prompt — because there is no meaningful choice. Here the left-only Room is
/// already unlocked, so the only operation available is `Lock`.
///
/// Revert-failing assertion: `left_unlocked` flips true → false directly.
#[test]
fn lock_or_unlock_single_option_auto_applies() {
    let mut state = GameState::new_two_player(42);
    let room = make_room(&mut state, 1, false, true, false);
    let ability = lock_unlock_ability(DoorLockOp::LockOrUnlock, room);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("resolves");

    assert!(
        !matches!(state.waiting_for, WaitingFor::ChooseRoomDoor { .. }),
        "a single available operation must auto-apply"
    );
    assert!(
        !state.objects[&room].room_unlocks.unwrap().left_unlocked,
        "the only available operation (Lock) must apply"
    );
}

/// CR 609.3: A Room with no eligible door (left-only and already unlocked, asked
/// to Unlock) is a legal no-op.
///
/// Revert-failing assertion: no `ChooseRoomDoor` prompt and the state is
/// unchanged. Reverting the empty-eligible early return could prompt or panic.
#[test]
fn no_eligible_door_is_legal_noop() {
    let mut state = GameState::new_two_player(42);
    let room = make_room(&mut state, 1, false, true, false);
    let ability = lock_unlock_ability(DoorLockOp::Unlock, room);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("resolves");

    assert!(!matches!(
        state.waiting_for,
        WaitingFor::ChooseRoomDoor { .. }
    ));
    assert!(state.objects[&room].room_unlocks.unwrap().left_unlocked);
}

/// CR 709.5i: An effect-driven unlock that fully unlocks the Room fires a
/// "when you fully unlock this Room" trigger (CR 709.5h-i). The Room here has
/// its left door already unlocked and its right door locked, so unlocking the
/// (single eligible) right door makes it fully unlocked, emitting
/// `RoomDoorUnlocked { fully_unlocked: true }`.
///
/// Revert-failing assertion: the FullyUnlock trigger's gain-life payoff resolves
/// (life goes up by 3). Reverting the `RoomDoorUnlocked` emission in
/// `unlock_door_designation`, or routing `Unlock` through the eventless lock
/// primitive, stops the trigger from firing and life is unchanged.
#[test]
fn effect_driven_full_unlock_fires_trigger() {
    let mut state = GameState::new_two_player(42);
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    // Left already unlocked, right locked → unlocking right fully unlocks.
    let room = make_room(&mut state, 1, true, true, false);

    // Install a "when you fully unlock this Room, gain 3 life" trigger.
    let trigger = TriggerDefinition::new(TriggerMode::FullyUnlock).execute(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        },
    ));
    state
        .objects
        .get_mut(&room)
        .unwrap()
        .trigger_definitions
        .push(trigger);

    let life_before = state.players[0].life;

    let ability = lock_unlock_ability(DoorLockOp::Unlock, room);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("resolves");

    // The right door is fully unlocked deterministically (single eligible).
    assert!(state.objects[&room].room_unlocks.unwrap().right_unlocked);
    assert!(
        events.iter().any(|e| matches!(
            e,
            engine::types::events::GameEvent::RoomDoorUnlocked {
                fully_unlocked: true,
                ..
            }
        )),
        "the second-door unlock must emit a fully-unlocked event"
    );

    // Dispatch the resulting trigger and resolve it.
    process_triggers(&mut state, &events);
    let mut guard = 0;
    while !state.stack.is_empty() {
        guard += 1;
        assert!(guard < 30, "stack did not drain");
        match state.waiting_for {
            WaitingFor::Priority { .. } => {
                if engine::game::apply_as_current(&mut state, GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }

    assert_eq!(
        state.players[0].life,
        life_before + 3,
        "the FullyUnlock trigger's gain-life payoff must resolve"
    );
}
