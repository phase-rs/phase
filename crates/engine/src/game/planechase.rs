//! CR 901: Planechase — the planar deck, planeswalking, the planar die, and
//! chaos resolution.
//!
//! Planes (CR 311) and phenomena (CR 312) are nontraditional cards that remain
//! in the command zone throughout the game (CR 311.2 / CR 312.2). In the
//! single-planar-deck option (CR 901.15) the engine tracks one shared deck in
//! [`GameState::planar_deck`]; the single active face-up card lives in
//! [`GameState::command_zone`], while the rest of the deck (face down) is held
//! in `planar_deck` (front = top).
//!
//! This module is the runtime score-lever: it owns planeswalking
//! ([`planeswalk`]), the planar die ([`roll_planar_die`]), chaos resolution
//! ([`chaos_ensues`]), the phenomenon encounter entry point ([`encounter`]),
//! and the phenomenon-leaves-stack state-based planeswalk
//! ([`check_phenomenon_planeswalk_sba`]).
//!
//! Trigger collection is NOT performed here. Like every other event-emitting
//! subsystem (e.g. dungeon completion in `sba::check_dungeon_completion`), these
//! functions push `GameEvent`s into the caller's event buffer; the engine loop
//! then turns those events into triggers via `collect_triggers_into_deferred`.
//! Plane/phenomenon triggers are scanned from the command zone because
//! `synthesize_planechase` stamps `trigger_zones = [Zone::Command]` onto them
//! (CR 113.6b), which makes `trigger_opts_in_to_command_zone` admit them.

use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::game::game_object::GameObject;
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// CR 901.9d / CR 706.7: The face the planar die landed on. The planar die is
/// symbolic (CR 901.3a: one Planeswalker face, one chaos face, four blank
/// faces) rather than numeric, so the engine models the outcome as the symbol,
/// not a 1-6 number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanarDieFace {
    /// CR 901.3a: the Planeswalker symbol — the roller may planeswalk.
    Planeswalk,
    /// CR 901.3a / CR 901.9b: the chaos symbol — chaos ensues.
    Chaos,
    /// CR 901.3a: one of the four blank faces — nothing happens.
    Blank,
}

/// CR 311.2 / CR 312.2: The active plane/phenomenon is the command-zone object
/// whose core type is Plane or Phenomenon. Returns its `ObjectId`, or `None`
/// outside a Planechase game.
pub fn active_plane(state: &GameState) -> Option<ObjectId> {
    state
        .command_zone
        .iter()
        .copied()
        .find(|id| is_planar_object(state, *id))
}

/// CR 311 / CR 312: True when the object is a plane or phenomenon.
fn is_planar_object(state: &GameState, id: ObjectId) -> bool {
    state.objects.get(&id).is_some_and(|o| {
        o.card_types
            .core_types
            .iter()
            .any(|ct| matches!(ct, CoreType::Plane | CoreType::Phenomenon))
    })
}

/// CR 312: True when the active card in the command zone is a phenomenon.
fn active_is_phenomenon(state: &GameState) -> bool {
    active_plane(state).is_some_and(|id| {
        state
            .objects
            .get(&id)
            .is_some_and(|o| o.card_types.core_types.contains(&CoreType::Phenomenon))
    })
}

/// CR 901.9 / CR 901.3a: Roll the planar die and resolve its outcome.
///
/// The planar die has six faces: one Planeswalker symbol, one chaos symbol, and
/// four blank faces (CR 901.3a) — a 1/1/4 distribution. We roll a d6 and map
/// `1 -> Planeswalk`, `2 -> Chaos`, `3..=6 -> Blank`. A `PlanarDieRolled` event
/// records the symbolic outcome so any "whenever a player rolls the planar die"
/// abilities can match on it.
///
/// CR 901.9c / CR 901.8: the Planeswalker symbol triggers the synthetic
/// "planeswalking ability," which is put on the stack and resolves at the next
/// priority (NOT inline) via `dispatch_synthetic_trigger`. The chaos symbol
/// (CR 901.9b) and blank faces (CR 901.9c) are resolved on the spot.
///
/// CR 901.9d / CR 706.7: rolling the planar die DOES cause any ability that
/// triggers "whenever a player rolls one or more dice" (`TriggerMode::RolledDie`,
/// matched on `GameEvent::DieRolled`) to trigger — so a sides-less, result-less
/// `DieRolled { sides: 6, result: None }` is emitted. Only effects that refer to
/// a *numerical result* of the roll ignore the planar die, because it is symbolic
/// (CR 901.3a) and has no numeric face value; those consumers guard on `None`.
pub fn roll_planar_die(
    state: &mut GameState,
    player_id: PlayerId,
    events: &mut Vec<GameEvent>,
) -> PlanarDieFace {
    // CR 901.3a: 4 blank / 1 Planeswalker / 1 chaos.
    let face = match state.rng.random_range(1..=6) {
        1 => PlanarDieFace::Planeswalk,
        2 => PlanarDieFace::Chaos,
        _ => PlanarDieFace::Blank,
    };
    events.push(GameEvent::PlanarDieRolled { player_id, face });
    // CR 901.9d / CR 706.7: rolling the planar die also fires generic "whenever a
    // player rolls one or more dice" triggers (`TriggerMode::RolledDie`, keyed on
    // `GameEvent::DieRolled`). The planar die is symbolic (CR 901.3a) with no
    // numeric face, so we emit `DieRolled { result: None }`: the `RolledDie`
    // matcher fires on it, while every numeric-result consumer (CR 706.7) guards
    // on `None` and ignores the planar roll. `sides: 6` reflects the six-faced
    // planar die (CR 901.3a) so `die_sides: Some(6)` triggers match (CR 706.7).
    events.push(GameEvent::DieRolled {
        player_id,
        sides: 6,
        result: None,
    });
    match face {
        // CR 901.9a / CR 901.9c: the Planeswalker symbol triggers the synthetic
        // "planeswalking ability" (CR 901.8), which is put on the stack and
        // resolves at the next priority — NOT inline. We dispatch it as a
        // synthetic trigger so it reaches the stack like any other triggered
        // ability (mirrors dungeon room triggers).
        PlanarDieFace::Planeswalk => queue_planeswalk_trigger(state, player_id, events),
        // CR 901.9b: the chaos symbol makes chaos ensue.
        PlanarDieFace::Chaos => chaos_ensues(state, events),
        // CR 901.9c: a blank face does nothing.
        PlanarDieFace::Blank => {}
    }
    face
}

/// CR 901.8 / CR 901.9c: queue the synthetic "planeswalking ability" so it is
/// put on the stack and resolves at the next priority (NOT inline). The ability
/// has no source (CR 901.8), so it uses a synthetic sentinel source id, and is
/// controlled by the roller (CR 901.8). Mirrors
/// `effects::venture::queue_room_trigger`.
fn queue_planeswalk_trigger(
    state: &mut GameState,
    player_id: PlayerId,
    events: &mut Vec<GameEvent>,
) {
    let source_id = planar_ability_sentinel_id(player_id);
    let pending = crate::game::triggers::PendingTrigger {
        source_id,
        // CR 901.8: controlled by the player whose roll caused the trigger.
        controller: player_id,
        condition: None,
        ability: crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Planeswalk,
            vec![],
            source_id,
            player_id,
        ),
        timestamp: 0,
        target_constraints: vec![],
        distribute: None,
        // CR 901.8: a parameterless ability with no event-context resolution.
        trigger_event: None,
        modal: None,
        mode_abilities: vec![],
        description: Some("Planeswalking ability".into()),
        may_trigger_origin: None,
        subject_match_count: None,
        die_result: None,
    };
    crate::game::triggers::dispatch_synthetic_trigger(state, pending, events);
}

/// CR 701.31b / CR 901.11: To planeswalk is to put each face-up plane/phenomenon
/// card on the bottom of its owner's planar deck face down, then move the top
/// card of the planar deck off it and turn it face up.
///
/// In the single-deck option (CR 901.15) the active card lives in the command
/// zone and the rest of the deck (face down) is held in `planar_deck` (front =
/// top). This rotates the active card to the bottom of `planar_deck` face down
/// and promotes the previous top into the command zone face up, then emits a
/// `Planeswalked { from, to }` event (CR 701.31d: `from` = the card turned face
/// down, `to` = the card turned face up).
///
/// CR 701.31b edge case — only the active card exists: if the planar deck is
/// empty, putting the departing card on the bottom makes it the sole card, so
/// moving the top card off and turning it face up turns the *same* card face up
/// again. The net effect is that the active plane stays active (a self-
/// planeswalk: `from == to`); the command zone is never left empty.
///
/// Trigger collection (CR 603.3): the `Planeswalked` event triggers both the
/// departing plane's "planeswalk away from ~" ability (`PlaneswalkedFrom`) and
/// the arriving plane's "planeswalk to / encounter ~" ability
/// (`PlaneswalkedTo`). Both abilities function from the command zone
/// (`trigger_zones = [Command]`, stamped by `synthesize_planechase`), and the
/// command-zone trigger scan only inspects objects currently in
/// `state.command_zone`. Because planeswalking removes the departing card from
/// the command zone, its leave-the-zone trigger must be collected *while it is
/// still present*. We therefore promote the arriving card, keep both endpoints
/// momentarily in the command zone, collect triggers into `deferred_triggers`
/// (CR 603.3: they reach the stack at the next priority, not immediately), then
/// finish moving the departing card out. This mirrors the deferred-trigger
/// contract used elsewhere (`engine_priority`/`engine_resolution_choices`).
pub fn planeswalk(state: &mut GameState, player_id: PlayerId, events: &mut Vec<GameEvent>) {
    let from = active_plane(state);

    // CR 701.31b: turn the departing card face down, but leave it in the command
    // zone for now so its leave-the-zone trigger can still be scanned.
    if let Some(from_id) = from {
        if let Some(obj) = state.objects.get_mut(&from_id) {
            obj.face_down = true;
        }
    }

    // CR 701.31b: move the top card of the planar deck off it and turn it face up
    // in the command zone. When the deck is empty, the departing card would be
    // the bottom-most (and only) card after rotation, hence also the new top —
    // so it turns face up again and stays active rather than emptying the zone.
    let to = state.planar_deck.pop_front().or(from);
    if let Some(to_id) = to {
        // CR 311.5 / CR 312.4: the controller of a face-up plane/phenomenon is
        // the planar controller. Stamp it onto the newly promoted card so its
        // "you"-scoped triggers resolve for the right player. Fall back to the
        // walking player before the planar controller is established.
        let new_controller = state.planar_controller.unwrap_or(player_id);
        if let Some(obj) = state.objects.get_mut(&to_id) {
            obj.face_down = false;
            obj.controller = new_controller;
        }
        // CR 701.31b self-planeswalk: when `to == from` the card never left the
        // command zone, so don't push a duplicate entry.
        if !state.command_zone.contains(&to_id) {
            state.command_zone.push_back(to_id);
        }
    }

    // CR 701.31d / CR 901.11: announce the planeswalk endpoints.
    let planeswalk_event = GameEvent::Planeswalked {
        player_id,
        from,
        to,
    };
    events.push(planeswalk_event.clone());

    // CR 603.3: collect both endpoints' triggers while the departing card is
    // still in the command zone; they are deferred to the next priority.
    crate::game::triggers::collect_triggers_into_deferred(state, &[planeswalk_event]);

    // CR 701.31b: now finish moving the departing card to the bottom of the
    // planar deck, removing it from the command zone (face down). In the
    // self-planeswalk case (`to == from`, empty deck) the departing card is the
    // card that turned face up again, so it must stay in the command zone face
    // up — skip the rotation entirely.
    if let Some(from_id) = from {
        if to != Some(from_id) {
            state.command_zone.retain(|&id| id != from_id);
            state.planar_deck.push_back(from_id);
        }
    }
}

/// CR 311.7 / CR 901.9b: Chaos ensues — the active plane's chaos-triggered
/// ability triggers. Emits a `ChaosEnsued` event keyed by the active plane so
/// its "whenever chaos ensues" trigger (and only its own) matches.
pub fn chaos_ensues(state: &mut GameState, events: &mut Vec<GameEvent>) {
    if let Some(plane_id) = active_plane(state) {
        events.push(GameEvent::ChaosEnsued { plane_id });
    }
}

/// CR 312.5: "When you encounter [this phenomenon]" means "When you move this
/// card off a planar deck and turn it face up." Encountering a phenomenon is the
/// planeswalk that turns it face up; this entry point performs that planeswalk,
/// which emits the `Planeswalked { to }` event the encounter trigger
/// (`PlaneswalkedTo`) matches.
pub fn encounter(state: &mut GameState, player_id: PlayerId, events: &mut Vec<GameEvent>) {
    // CR 312.5: encountering a phenomenon IS this planeswalk — it is the
    // turn-based/effect-driven planeswalk that turns the phenomenon face up, NOT
    // the CR 901.8 "planeswalking ability" triggered by rolling the Planeswalker
    // symbol. So it planeswalks directly (inline), never through the stack.
    planeswalk(state, player_id, events);
}

/// CR 704.6f / CR 312.7: If a phenomenon card is face up in the command zone and
/// it isn't the source of a triggered ability that has triggered but not yet
/// left the stack, its controller planeswalks. This is a state-based action.
///
/// Modeled on `sba::check_dungeon_completion`: scan the stack for any entry
/// whose `source_id` is the phenomenon; if none, planeswalk and record that an
/// action was performed (so the SBA fixpoint loop re-checks).
pub fn check_phenomenon_planeswalk_sba(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    let Some(controller) = state.planar_controller else {
        return;
    };
    if !active_is_phenomenon(state) {
        return;
    }
    let Some(phenomenon_id) = active_plane(state) else {
        return;
    };
    // CR 704.6f: do not planeswalk while the phenomenon's own triggered ability
    // is still on the stack.
    let has_ability_on_stack = state
        .stack
        .iter()
        .any(|entry| entry.source_id == phenomenon_id);
    if has_ability_on_stack {
        return;
    }
    // CR 704.6f: this is a state-based action, NOT the CR 901.8 "planeswalking
    // ability" triggered by the planar die. It planeswalks directly (inline),
    // never through the stack.
    planeswalk(state, controller, events);
    *any_performed = true;
}

/// Sentinel base for the synthetic source ObjectId of the CR 901.8
/// "planeswalking ability." Each player gets
/// `PLANAR_ABILITY_SENTINEL_BASE + player.0 as u64`.
///
/// CR 901.8: the planeswalking ability "has no source," so it cannot reuse a
/// real object id. We pick a distinct high-byte namespace from the dungeon
/// room-trigger sentinel (`dungeon::DUNGEON_SENTINEL_BASE` = `0xD0_..`) so the
/// two synthetic-source spaces never collide. Synthetic sources that are not
/// present in `state.objects` are supported by the trigger pipeline
/// (`triggers.rs` synthetic-source handling).
pub const PLANAR_ABILITY_SENTINEL_BASE: u64 = 0xD1_0000_0000;

/// CR 901.8: the synthetic ObjectId for a player's planeswalking ability,
/// controlled by the player whose planar die roll caused it to trigger.
pub fn planar_ability_sentinel_id(player: PlayerId) -> ObjectId {
    ObjectId(PLANAR_ABILITY_SENTINEL_BASE + player.0 as u64)
}

/// CR 311.5 / CR 312.4 / CR 901.6: Designate `new` as the planar controller and
/// sync the active face-up plane/phenomenon's `.controller` to match. The
/// controller of a face-up plane or phenomenon is, by rule, the planar
/// controller — keeping the object's `.controller` in lockstep means its
/// "you"-scoped abilities resolve for the correct player.
///
/// No-op outside a Planechase game (no planar controller, empty planar deck, and
/// no active plane), so non-Planechase turn/elimination paths can call this
/// unconditionally without side effects.
pub fn set_planar_controller(state: &mut GameState, new: PlayerId, _events: &mut Vec<GameEvent>) {
    if state.planar_controller.is_none()
        && state.planar_deck.is_empty()
        && active_plane(state).is_none()
    {
        return;
    }
    state.planar_controller = Some(new);
    if let Some(active_id) = active_plane(state) {
        if let Some(obj) = state.objects.get_mut(&active_id) {
            obj.controller = new;
        }
    }
}

/// CR 311.5: helper so callers can confirm an object is a plane/phenomenon
/// without importing `CoreType` (used by tests and future deck loading).
pub fn object_is_planar(obj: &GameObject) -> bool {
    obj.card_types
        .core_types
        .iter()
        .any(|ct| matches!(ct, CoreType::Plane | CoreType::Phenomenon))
}
