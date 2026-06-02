use crate::types::ability::{Effect, EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

use super::super::attractions;

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    match &ability.effect {
        Effect::OpenAttractions { count } => {
            attractions::resolve_open(state, ability, *count, events)
        }
        Effect::RollToVisitAttractions => {
            attractions::resolve_roll_to_visit(state, ability, events)
        }
        _ => Err(EffectError::MissingParam("attraction effect".to_string())),
    }
}
