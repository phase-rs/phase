use crate::types::ability::{
    effect_variant_name, Effect, EffectError, ResolvedAbility, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// Effect effect: creates a temporary game effect (emblem-like).
/// Reads typed GenericEffect { static_abilities, duration } fields.
/// Applies referenced static abilities directly to targeted objects.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if let Effect::GenericEffect {
        static_abilities, ..
    } = &ability.effect
    {
        // Apply each static ability definition to targets
        for static_def in static_abilities {
            apply_static_to_targets(state, ability, static_def.clone());
        }
    }

    events.push(GameEvent::EffectResolved {
        api_type: effect_variant_name(&ability.effect).to_string(),
        source_id: ability.source_id,
    });

    Ok(())
}

fn apply_static_to_targets(
    state: &mut GameState,
    ability: &ResolvedAbility,
    static_def: crate::types::ability::StaticDefinition,
) {
    // Apply to targeted objects
    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            if let Some(obj) = state.objects.get_mut(obj_id) {
                // For CantBlockBy/CantBeBlocked, add the static directly
                if !obj
                    .static_definitions
                    .iter()
                    .any(|s| s.mode == static_def.mode)
                {
                    obj.static_definitions.push(static_def.clone());
                    state.layers_dirty = true;
                }
            }
        }
    }
}
