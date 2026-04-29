use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::targeting;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 609.7a: Choose a source of damage. The engine can offer source objects
/// it currently represents directly: permanents, stack objects, and command-zone
/// objects. Objects referred to only by pending effects are future work.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let source_filter = match &ability.effect {
        Effect::ChooseDamageSource { source_filter } => source_filter.clone(),
        _ => {
            return Err(EffectError::InvalidParam(
                "expected ChooseDamageSource effect".to_string(),
            ))
        }
    };

    let options = damage_source_options(state, ability, &source_filter);
    state.waiting_for = WaitingFor::DamageSourceChoice {
        player: ability.controller,
        source_filter,
        options,
    };
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });
    Ok(())
}

fn damage_source_options(
    state: &GameState,
    ability: &ResolvedAbility,
    source_filter: &crate::types::ability::TargetFilter,
) -> Vec<ObjectId> {
    let ctx = FilterContext::from_source_with_controller(ability.source_id, ability.controller);
    [Zone::Battlefield, Zone::Stack]
        .into_iter()
        .flat_map(|zone| targeting::zone_object_ids(state, zone))
        .chain(state.command_zone.iter().copied())
        .filter(|id| matches_target_filter(state, *id, source_filter, &ctx))
        .collect()
}
