use crate::types::ability::{EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 719.2: Resolve the "solve" action — set the source Case's case_state.is_solved = true.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let source_id = ability.source_id;
    if let Some(obj) = state.objects.get_mut(&source_id) {
        if let Some(ref mut cs) = obj.case_state {
            // CR 719.3b: Solved is a designation a permanent can have. Once a
            // permanent becomes solved, it stays solved until it leaves the battlefield.
            if !cs.is_solved {
                cs.is_solved = true;
                events.push(GameEvent::CaseSolved {
                    object_id: source_id,
                });
                crate::game::layers::mark_layers_full(state);
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::SolveCase,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}
