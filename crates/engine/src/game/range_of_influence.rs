//! CR 801: Limited Range of Influence Option — seat-distance predicates.
//!
//! CR 801.1: an option applicable to most multiplayer games, always used in
//! the Emperor variant (CR 809, see `game::emperor`) and often used for games
//! of five or more players. `FormatConfig::range_of_influence_for_player`
//! (types/format.rs) is the single source of a viewer's configured range;
//! every predicate in this module returns `true` unconditionally when that
//! accessor returns `None` — the default for every format that does not
//! explicitly opt in — so this module is a proven no-op for every
//! pre-existing format.
//!
//! CR 801.2: "A player's range of influence is the maximum distance from that
//! player, measured in player seats, that the player can affect." This module
//! computes that distance LIVE against the current `seat_order` on every call,
//! a deliberate simplification of CR 801.2c ("The particular players within
//! each player's range of influence are determined as each turn begins.") —
//! a player who leaves or rejoins mid-turn changes range immediately rather
//! than waiting for the next turn boundary. Flagged as a follow-up, not
//! silently papered over.
//!
//! Deferred (documented, not silently dropped — see issue tracking CR 801 for
//! the full list): CR 801.5a-c (choice-making range constraints), CR
//! 801.7/801.7a (triggered-ability range gating — no safe minimal seam
//! identified in the trigger-matching layer without a deeper follow-up), CR
//! 801.10/801.11 (mass-effect and information-gathering range scoping), CR
//! 801.12 (the "world rule"), CR 801.13a/801.13b (replacement/prevention
//! effect range scoping), CR 801.15/801.16 (draw/loop-draw propagation — no
//! "the game is a draw" effect exists yet in this engine to extend), CR
//! 801.17/801.18 (moot while the above are deferred).

use crate::types::game_state::GameState;
use crate::types::player::PlayerId;

/// CR 801.2 / CR 801.2b: the distance in player seats between `a` and `b`,
/// measured around the living seat order. `a == b` is always `0` ("A player
/// is always within their own range of influence"). A seat that has left the
/// game (CR 800.4) has no distance — returns `u8::MAX` so it can never fall
/// within a configured range, matching "a departed player is no longer part
/// of the game."
pub(crate) fn seat_distance(state: &GameState, a: PlayerId, b: PlayerId) -> u8 {
    if a == b {
        return 0;
    }
    let seat_order = &state.seat_order;
    let len = seat_order.len();
    if len == 0 {
        return u8::MAX;
    }
    let Some(a_idx) = seat_order.iter().position(|&id| id == a) else {
        return u8::MAX;
    };
    let Some(b_idx) = seat_order.iter().position(|&id| id == b) else {
        return u8::MAX;
    };
    if !super::players::is_alive(state, a) || !super::players::is_alive(state, b) {
        return u8::MAX;
    }
    // CR 801.2: distance is measured around the table, so the shorter of the
    // two directions applies.
    let forward = (b_idx + len - a_idx) % len;
    let backward = (a_idx + len - b_idx) % len;
    forward.min(backward) as u8
}

/// CR 801.2 / CR 801.2a-c: is `other` within `viewer`'s range of influence?
/// `true` unconditionally when `viewer` has no configured range (the default
/// for every pre-existing format) — the single point that makes every
/// downstream CR 801 call site a proven no-op unless a format opts in.
pub(crate) fn within_range_of_influence(
    state: &GameState,
    viewer: PlayerId,
    other: PlayerId,
) -> bool {
    state
        .format_config
        .range_of_influence_for_player(viewer)
        .is_none_or(|range| seat_distance(state, viewer, other) <= range)
}

/// CR 801.2d: "An object is within a player's range of influence if it's
/// controlled by that player or by another player within that many seats of
/// that player." Thin wrapper — an object is in range iff its controller is.
pub(crate) fn object_within_range_of_influence(
    state: &GameState,
    viewer: PlayerId,
    object_controller: PlayerId,
) -> bool {
    within_range_of_influence(state, viewer, object_controller)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::format::FormatConfig;

    fn six_player_state() -> GameState {
        GameState::new(FormatConfig::free_for_all(), 6, 42)
    }

    #[test]
    fn seat_distance_self_is_zero() {
        let state = six_player_state();
        assert_eq!(seat_distance(&state, PlayerId(2), PlayerId(2)), 0);
    }

    #[test]
    fn seat_distance_is_symmetric_and_wraps() {
        let state = six_player_state();
        // Adjacent seats (both directions around the table) are distance 1.
        assert_eq!(seat_distance(&state, PlayerId(0), PlayerId(1)), 1);
        assert_eq!(seat_distance(&state, PlayerId(1), PlayerId(0)), 1);
        assert_eq!(seat_distance(&state, PlayerId(0), PlayerId(5)), 1);
        // Opposite side of a 6-seat table is distance 3, the maximum.
        assert_eq!(seat_distance(&state, PlayerId(0), PlayerId(3)), 3);
    }

    #[test]
    fn seat_distance_unreachable_for_eliminated_player() {
        let mut state = six_player_state();
        state.players[3].is_eliminated = true;
        assert_eq!(seat_distance(&state, PlayerId(0), PlayerId(3)), u8::MAX);
    }

    /// CR 801: the single most important test in this module — when
    /// `range_of_influence_for_player` returns `None` (every pre-existing
    /// format, unconditionally), `within_range_of_influence` must return
    /// `true` for every player, no matter the distance. This is the
    /// invariant that makes every downstream CR 801 guard a proven no-op.
    #[test]
    fn within_range_of_influence_is_unconditional_true_when_unconfigured() {
        let state = six_player_state();
        assert!(state.format_config.range_of_influence.is_none());
        assert!(within_range_of_influence(&state, PlayerId(0), PlayerId(3)));
    }

    #[test]
    fn within_range_of_influence_respects_a_configured_range() {
        let mut state = six_player_state();
        state.format_config.range_of_influence = Some(1);
        assert!(within_range_of_influence(&state, PlayerId(0), PlayerId(1)));
        assert!(within_range_of_influence(&state, PlayerId(0), PlayerId(5)));
        assert!(!within_range_of_influence(&state, PlayerId(0), PlayerId(2)));
        assert!(!within_range_of_influence(&state, PlayerId(0), PlayerId(3)));
    }

    #[test]
    fn object_within_range_of_influence_mirrors_player_range() {
        let mut state = six_player_state();
        state.format_config.range_of_influence = Some(1);
        assert!(object_within_range_of_influence(
            &state,
            PlayerId(0),
            PlayerId(1)
        ));
        assert!(!object_within_range_of_influence(
            &state,
            PlayerId(0),
            PlayerId(3)
        ));
    }
}
