//! CR 904: Archenemy — the scheme deck, setting schemes in motion, and the
//! abandon state-based action.
//!
//! Schemes (CR 314) are nontraditional cards that remain in the command zone
//! throughout the game (CR 314.2), both while face down in the scheme deck and
//! while face up after being set in motion. They are not permanents and can't
//! be cast (CR 314.2).
//!
//! In the single-scheme-deck Archenemy option (CR 904.3 / CR 904.4) the engine
//! tracks one deck in [`GameState::scheme_deck`] (front = top, face down).
//! Schemes that are set in motion (CR 904.9) turn face up and live in
//! [`GameState::command_zone`]; non-ongoing schemes are turned face down and put
//! on the bottom of the scheme deck by a state-based action (CR 904.10 /
//! CR 314.6), while ongoing schemes (CR 904.11) stay face up until an ability
//! abandons them.
//!
//! This is the runtime sibling of `game::planechase`: it owns setting a scheme
//! in motion ([`set_in_motion`]), abandoning a scheme ([`abandon`]), and the
//! non-ongoing-scheme state-based action ([`check_scheme_abandon_sba`]).
//!
//! Trigger collection: scheme triggers function from the command zone because
//! `synthesize_archenemy` stamps `trigger_zones = [Zone::Command]` onto them
//! (CR 113.6b / CR 314.4 / CR 904.8). The set-in-motion and abandon
//! turn-based/state-based actions don't use the stack (CR 904.9 / CR 904.10),
//! but the resulting triggered abilities are collected the next time a player
//! would receive priority (CR 603.2 / CR 603.3). The two actions collect their
//! triggers via DIFFERENT mechanisms, and this asymmetry is deliberate:
//!
//! - [`set_in_motion`] does NOT self-collect. Every real path to it runs through
//!   `finish_enter_phase` during a `pass_priority_once_with_pipeline`, which is
//!   immediately followed by `run_post_action_pipeline`'s normal post-action
//!   event scan; that scan collects the `SchemeSetInMotion` trigger exactly once
//!   (it may place it directly on the stack rather than deferring — rules-
//!   equivalent under CR 603.2 / CR 603.3).
//! - [`abandon`] DOES self-collect (single authority), because an abandon effect
//!   turns a face-up ongoing scheme face down and removes it from the command
//!   zone. It collects the `SchemeAbandoned` trigger while the scheme is still
//!   face up in the command zone (CR 314.4 / CR 904.8), THEN turns it face down
//!   and removes it from the command zone, so the normal post-action / SBA-loop
//!   trigger scan — which only inspects objects currently in the command zone —
//!   never sees the departed scheme and cannot double-collect. No special
//!   pipeline handling is needed: this is the same leave-the-command-zone
//!   self-collect pattern `planechase::planeswalk` uses.
//!
//! The two-scheme-deck Supervillain Rumble option (CR 904.12) is deferred: this
//! module models the single-archenemy game (CR 904.2a).

use crate::types::card_type::{CoreType, Supertype};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;

/// CR 314: True when the object is a scheme card.
pub fn is_scheme_object(state: &GameState, id: ObjectId) -> bool {
    state
        .objects
        .get(&id)
        .is_some_and(|o| o.card_types.core_types.contains(&CoreType::Scheme))
}

/// CR 904.9: The top card of the archenemy's scheme deck (front = top), or
/// `None` if the scheme deck is empty.
pub fn top_scheme(state: &GameState) -> Option<ObjectId> {
    state.scheme_deck.front().copied()
}

/// CR 904.9 / CR 904.11: The schemes currently set in motion — command-zone
/// scheme cards that are face up. Returns a `Vec` because ongoing schemes
/// (CR 904.11) accumulate: more than one scheme can be face up at once, unlike
/// Planechase's single active plane.
pub fn active_schemes(state: &GameState) -> Vec<ObjectId> {
    state
        .command_zone
        .iter()
        .copied()
        .filter(|&id| {
            is_scheme_object(state, id) && state.objects.get(&id).is_some_and(|o| !o.face_down)
        })
        .collect()
}

/// CR 904.9 / CR 701.32b: Set the top scheme of the archenemy's scheme deck in
/// motion — move it off the top of the scheme deck and turn it face up in the
/// command zone.
///
/// No-op outside an Archenemy game (no archenemy designated) or when the scheme
/// deck is empty. Otherwise the top scheme is popped from `scheme_deck`, turned
/// face up, stamped with the archenemy as its controller (CR 314.5: the
/// controller of a face-up scheme is its owner — the archenemy — so its
/// "you"-scoped SetInMotion trigger resolves for the archenemy), and pushed
/// into the command zone.
///
/// CR 603.2 / CR 603.3: the `SchemeSetInMotion` event triggers the scheme's
/// "When you set this scheme in motion" ability (`SetInMotion`). The turn-based
/// action itself doesn't use the stack (CR 904.9). This function does NOT
/// self-collect the trigger: every real path that sets a scheme in motion runs
/// through `finish_enter_phase` during a `pass_priority_once_with_pipeline`,
/// which is immediately followed by `run_post_action_pipeline`'s normal
/// post-action event scan — that scan collects the `SchemeSetInMotion` trigger
/// exactly once. (The scan may place the trigger directly on the stack via
/// `process_triggers` rather than deferring it; this is rules-equivalent under
/// CR 603.2 / CR 603.3.) Self-collecting here in addition would double-collect.
pub fn set_in_motion(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let Some(archenemy) = state.archenemy else {
        return;
    };
    // CR 701.32b: move the scheme off the top of the scheme deck.
    let Some(scheme_id) = state.scheme_deck.pop_front() else {
        return;
    };
    // CR 701.32b / CR 314.5: turn it face up and stamp the archenemy as its
    // controller so its "you"-scoped triggers resolve for the archenemy.
    if let Some(obj) = state.objects.get_mut(&scheme_id) {
        obj.face_down = false;
        obj.controller = archenemy;
    }
    // CR 314.2 / CR 904.9: the scheme stays in the command zone, now face up.
    state.command_zone.push_back(scheme_id);

    // CR 904.9 / CR 701.32b: announce that the scheme was set in motion. The
    // SetInMotion trigger is collected by the normal post-action event scan in
    // `run_post_action_pipeline` (CR 603.2 / CR 603.3), which always follows the
    // `finish_enter_phase` that called this function — so this function must not
    // self-collect it here (that would double-collect).
    let event = GameEvent::SchemeSetInMotion {
        player_id: archenemy,
        scheme_id,
    };
    events.push(event);
}

/// CR 904.10 / CR 314.6: True if any scheme's triggered ability is on the stack
/// or waiting to be put on the stack (i.e. deferred but not yet on the stack).
/// While any such ability exists, the abandon state-based action does nothing.
pub fn scheme_trigger_on_stack_or_pending(state: &GameState) -> bool {
    // CR 904.10: "on the stack" — any stack entry sourced from a scheme.
    let on_stack = state
        .stack
        .iter()
        .any(|entry| is_scheme_object(state, entry.source_id));
    // CR 904.10: "waiting to be put on the stack" — any deferred trigger whose
    // pending source is a scheme.
    let pending = state
        .deferred_triggers
        .iter()
        .any(|d| is_scheme_object(state, d.pending.source_id));
    on_stack || pending
}

fn turn_face_down_to_bottom_of_scheme_deck(state: &mut GameState, scheme_id: ObjectId) {
    if let Some(obj) = state.objects.get_mut(&scheme_id) {
        obj.face_down = true;
    }
    state.command_zone.retain(|&id| id != scheme_id);
    state.scheme_deck.push_back(scheme_id);
}

/// CR 701.33a-b: Abandon a face-up ongoing scheme — turn it face down and put
/// it on the bottom of its owner's scheme deck.
///
/// CR 603.2 / CR 603.3: the `SchemeAbandoned` event triggers the scheme's "When
/// you abandon this scheme" ability (`Abandoned`). This function self-collects
/// that trigger (single authority): CR 314.4 / CR 904.8 require the trigger to
/// be collected while the scheme is still face up in the command zone, and only
/// then is the scheme turned face down and removed from the command zone. The
/// normal post-action / SBA-loop trigger scan inspects only objects currently in
/// the command zone, so once the scheme departs no later scan can re-collect
/// `SchemeAbandoned` — no pipeline filtering is required. This mirrors
/// `planechase::planeswalk`.
pub fn abandon(state: &mut GameState, scheme_id: ObjectId, events: &mut Vec<GameEvent>) {
    // CR 701.33a: only face-up ongoing schemes may be abandoned.
    if !state.objects.get(&scheme_id).is_some_and(|obj| {
        !obj.face_down && obj.card_types.supertypes.contains(&Supertype::Ongoing)
    }) {
        return;
    }

    // CR 904.7 / CR 314.5: the owner/controller of a scheme is the archenemy.
    let owner = state
        .archenemy
        .or_else(|| state.objects.get(&scheme_id).map(|o| o.controller));

    // CR 701.33b: announce that the ongoing scheme was abandoned.
    let event = GameEvent::SchemeAbandoned {
        player_id: owner.unwrap_or(state.active_player),
        scheme_id,
    };
    events.push(event.clone());

    // CR 314.4 / CR 904.8 + CR 603.2 / CR 603.3: a scheme's triggered abilities
    // may trigger only while it is face up in the command zone, so collect its
    // "when you abandon this scheme" trigger while the scheme is still face up —
    // BEFORE the face-down flip below. This self-collect is the single authority
    // for the `Abandoned` trigger because the scheme leaves the command zone
    // immediately below, and every later trigger scan only inspects objects still
    // in the command zone; no scan can re-collect `SchemeAbandoned` after this
    // point, so no pipeline filtering is required (same pattern as
    // `planechase::planeswalk`).
    crate::game::triggers::collect_triggers_into_deferred(state, &[event]);

    // CR 701.33b / CR 314.2: now turn the scheme face down and put it on the
    // bottom of its owner's scheme deck (front = top), removing it from the
    // active command-zone view.
    turn_face_down_to_bottom_of_scheme_deck(state, scheme_id);
}

/// CR 904.10 / CR 314.6: State-based action — a face-up non-ongoing scheme card
/// in the command zone, with no scheme triggered ability on the stack or
/// waiting to be put on the stack, is turned face down and put on the bottom of
/// its owner's scheme deck. This is not an abandon action: CR 701.33a allows
/// only face-up ongoing schemes to be abandoned.
///
/// Gated on an Archenemy game (`archenemy.is_some()`). Ongoing schemes
/// (CR 904.11) are exempt — they stay face up until an ability abandons them.
/// Mirrors `planechase::check_phenomenon_planeswalk_sba`: records that an action
/// was performed so the SBA fixpoint loop re-checks.
pub fn check_scheme_abandon_sba(
    state: &mut GameState,
    _events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    if state.archenemy.is_none() {
        return;
    }
    // CR 904.10: do nothing while any scheme's triggered ability is on the stack
    // or waiting to be put on the stack.
    if scheme_trigger_on_stack_or_pending(state) {
        return;
    }
    // CR 904.10 / CR 904.11: turn every face-up non-ongoing scheme face down
    // and put it on the bottom of the scheme deck; ongoing schemes are exempt.
    // Collect first to avoid borrowing `state` while mutating.
    let to_turn_down: Vec<ObjectId> = active_schemes(state)
        .into_iter()
        .filter(|&id| {
            state
                .objects
                .get(&id)
                .is_some_and(|o| !o.card_types.supertypes.contains(&Supertype::Ongoing))
        })
        .collect();
    for scheme_id in to_turn_down {
        turn_face_down_to_bottom_of_scheme_deck(state, scheme_id);
        *any_performed = true;
    }
}
