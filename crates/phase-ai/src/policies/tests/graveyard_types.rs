//! Unit tests for `policies::graveyard_types` — the delirium/descend progress
//! policy. No `#[cfg(test)]` in SOURCE files; tests live here.

use crate::features::graveyard_types::GRAVEYARD_TYPES_FLOOR;
use crate::features::DeckFeatures;
use crate::policies::graveyard_types::*;
use crate::policies::registry::TacticalPolicy;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

#[test]
fn activation_opts_out_below_floor() {
    let mut features = DeckFeatures::default();
    features.graveyard_types.commitment = GRAVEYARD_TYPES_FLOOR - 0.01;
    let state = GameState::default();
    assert!(GraveyardTypesPolicy
        .activation(&features, &state, PlayerId(0))
        .is_none());
}

#[test]
fn activation_opts_in_above_floor() {
    let mut features = DeckFeatures::default();
    features.graveyard_types.commitment = 0.9;
    let state = GameState::default();
    assert_eq!(
        GraveyardTypesPolicy.activation(&features, &state, PlayerId(0)),
        Some(0.9)
    );
}

/// CR 404.1: an empty graveyard has zero distinct card types.
#[test]
fn empty_graveyard_counts_zero_types() {
    let state = GameState::default();
    assert_eq!(distinct_graveyard_types(&state, PlayerId(0)), 0);
}
