use crate::types::ability::{EffectError, ResolvedAbility, StaticDefinition, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// Effect effect: creates a temporary game effect (emblem-like).
/// In Forge, this creates an ephemeral card with static abilities, triggers, etc.
/// Simplified implementation: applies referenced StaticAbilities directly to
/// remembered/targeted objects for the current turn.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Collect static ability SVars referenced by the StaticAbilities param
    if let Some(static_keys) = ability.params.get("StaticAbilities") {
        for key in static_keys.split(',') {
            let key = key.trim();
            if let Some(svar_val) = ability.svars.get(key) {
                // Parse the static ability definition and apply to targets
                if let Ok(static_def) = crate::parser::ability::parse_static(svar_val) {
                    apply_static_to_targets(state, ability, static_def);
                }
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        api_type: ability.api_type.clone(),
        source_id: ability.source_id,
    });

    Ok(())
}

fn apply_static_to_targets(
    state: &mut GameState,
    ability: &ResolvedAbility,
    static_def: StaticDefinition,
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
