//! Shared `activation()` builders for migrated policies.
//!
//! Most policies' historical scaling collapsed `archetype_scale * turn_phase_mult`
//! at the registry level. This helper applies that composition uniformly so
//! per-policy `activation()` bodies stay one expression long.

use engine::types::game_state::GameState;

use crate::deck_profile::DeckArchetype;
use crate::features::DeckFeatures;

/// Compose `archetype_scale * turn_phase_mult(turn_number)` into the single
/// activation knob, returning `Some(product)`.
pub fn arch_times_turn(
    features: &DeckFeatures,
    state: &GameState,
    arch_scale: fn(DeckArchetype) -> f64,
) -> Option<f32> {
    let arch = arch_scale(features.archetype);
    let turn = features.strategy.turn_phase_mult(state.turn_number);
    Some((arch * turn) as f32)
}

/// `turn_phase_mult` only — the archetype dimension was unused historically.
pub fn turn_only(features: &DeckFeatures, state: &GameState) -> Option<f32> {
    Some(features.strategy.turn_phase_mult(state.turn_number) as f32)
}
