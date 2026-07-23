//! Unit tests for `policies::poison` — the CR 104.3d poison-clock policy.
//! No `#[cfg(test)]` in SOURCE files; tests live here.

use crate::features::poison::{LETHAL_POISON, POISON_CLOCK_FLOOR};
use crate::features::DeckFeatures;
use crate::policies::registry::TacticalPolicy;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::policies::poison::*;

/// CR 104.3d: ten or more poison counters loses the game, so the ninth
/// counter is the lethal setup and the eighth is not.
#[test]
fn reaches_lethal_matches_cr_104_3d_boundary() {
    assert_eq!(LETHAL_POISON, 10);
    assert!(reaches_lethal(9), "9 + 1 == 10 is lethal");
    assert!(!reaches_lethal(8), "8 + 1 == 9 is not yet lethal");
    assert!(reaches_lethal(u32::MAX), "saturating add must not wrap");
}

#[test]
fn activation_opts_out_below_floor() {
    let mut features = DeckFeatures::default();
    features.poison.commitment = POISON_CLOCK_FLOOR - 0.01;
    let state = GameState::default();
    assert!(PoisonClockPolicy
        .activation(&features, &state, PlayerId(0))
        .is_none());
}

#[test]
fn activation_opts_in_above_floor() {
    let mut features = DeckFeatures::default();
    features.poison.commitment = 0.9;
    let state = GameState::default();
    assert_eq!(
        PoisonClockPolicy.activation(&features, &state, PlayerId(0)),
        Some(0.9)
    );
}

#[test]
fn most_poisoned_opponent_ignores_the_ai_itself() {
    let mut state = GameState::default();
    while state.players.len() < 2 {
        state.players.push(Default::default());
    }
    state.players[0].poison_counters = 7;
    state.players[1].poison_counters = 3;
    // The AI's own 7 poison must not be read as pressure it is applying.
    assert_eq!(most_poisoned_opponent(&state, PlayerId(0)), 3);
    assert_eq!(most_poisoned_opponent(&state, PlayerId(1)), 7);
}
