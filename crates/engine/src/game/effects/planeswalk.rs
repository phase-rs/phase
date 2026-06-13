//! CR 701.31 / CR 901.8: Resolver for the synthetic "planeswalking ability."
//!
//! CR 901.8 / CR 901.9c: when a player rolls the Planeswalker symbol on the
//! planar die, the planeswalking ability triggers and is put on the stack
//! (see `planechase::roll_planar_die`). On resolution, its controller — the
//! roller, CR 901.8 — planeswalks (CR 701.31).

use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 901.8 / CR 901.9c: resolve the planeswalking ability — the controller
/// (the roller, CR 901.8) planeswalks (CR 701.31).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    crate::game::planechase::planeswalk(state, ability.controller, events);
    Ok(())
}
