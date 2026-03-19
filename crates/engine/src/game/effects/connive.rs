use crate::types::ability::{EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 702.162a: Connive — draw a card, then discard a card; if a nonland card
/// is discarded, put a +1/+1 counter on the conniving creature.
///
/// Stub: emits EffectResolved event. Full game logic is a follow-up.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let _ = state;
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Connive,
        source_id: ability.source_id,
    });
    Ok(())
}
