use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 722: Become the monarch.
///
/// CR 722.2: When a player becomes the monarch, the current monarch ceases to be
/// the monarch. Only one player can be the monarch at a time.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let player_id = ability.controller;
    state.monarch = Some(player_id);
    events.push(GameEvent::MonarchChanged { player_id });
    Ok(())
}
