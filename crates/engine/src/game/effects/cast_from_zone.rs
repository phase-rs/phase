use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 601.2a + CR 118.9: Stub handler for casting a card from a zone.
/// Currently a no-op — cards appear as "supported" in coverage but the
/// effect does nothing at runtime. Full implementation is future work.
pub fn resolve(
    _state: &mut GameState,
    _ability: &ResolvedAbility,
    _events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    Ok(())
}
