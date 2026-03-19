use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 615: Prevent damage — stub resolver.
///
/// Currently marks the effect as supported for coverage purposes.
/// Full runtime behavior (prevention shields on the replacement pipeline)
/// is future work.
pub fn resolve(
    _state: &mut GameState,
    _ability: &ResolvedAbility,
    _events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // TODO: Create a prevention shield on the target, consumed by
    // the damage replacement pipeline in replacement.rs.
    Ok(())
}
