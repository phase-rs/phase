use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 725: Become the monarch.
///
/// CR 725.3: Only one player can be the monarch at a time. As a player becomes
/// the monarch, the current monarch ceases to be the monarch.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 725.1: The monarch is a designation a player can have.
    let player_id = ability.controller;
    state.monarch = Some(player_id);
    events.push(GameEvent::MonarchChanged { player_id });
    Ok(())
}
