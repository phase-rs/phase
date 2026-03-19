use crate::types::ability::{EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// Stub resolver for ChooseFromZone — choose card(s) from a zone.
///
/// Building block for impulse draw, cascade, hideaway, and similar
/// exile-then-select patterns. Full implementation requires tracked-set
/// infrastructure to know which cards were exiled by the parent effect.
pub fn resolve(
    _state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // TODO: Present WaitingFor for card selection from exiled set.
    // Needs tracked-set infrastructure to know which cards were exiled by parent.
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChooseFromZone,
        source_id: ability.source_id,
    });
    Ok(())
}
