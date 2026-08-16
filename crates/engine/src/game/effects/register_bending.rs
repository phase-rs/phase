use crate::game::bending;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let kind = match &ability.effect {
        Effect::RegisterBending { kind } => *kind,
        _ => return Err(EffectError::MissingParam("RegisterBending".to_string())),
    };

    bending::record_bending(state, events, kind, ability.source_id, ability.controller);
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::RegisterBending,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}
