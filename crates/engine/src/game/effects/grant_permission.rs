use crate::types::ability::{CastingPermission, Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::{BendingType, GameEvent};
use crate::types::game_state::GameState;

/// Grant a CastingPermission to the target object.
/// Building block for Airbending, Foretell, Suspend, and similar "cast from exile" mechanics.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let permission = match &ability.effect {
        Effect::GrantCastingPermission { permission, .. } => permission.clone(),
        _ => return Err(EffectError::MissingParam("permission".to_string())),
    };

    if ability.targets.is_empty() {
        // Untargeted: grant permission to the ability's source object (self-referencing).
        if let Some(obj) = state.objects.get_mut(&ability.source_id) {
            obj.casting_permissions.push(permission.clone());
        }
    } else {
        for target in &ability.targets {
            if let crate::types::ability::TargetRef::Object(obj_id) = target {
                if let Some(obj) = state.objects.get_mut(obj_id) {
                    obj.casting_permissions.push(permission.clone());
                }
            }
        }
    }

    // Emit bending event if this is an airbending permission (generic {2} from exile)
    if matches!(permission, CastingPermission::ExileWithAltCost { .. }) {
        events.push(GameEvent::Airbend {
            source_id: ability.source_id,
            controller: ability.controller,
        });
        // Track bending type for Avatar Aang
        if let Some(player) = state
            .players
            .iter_mut()
            .find(|p| p.id == ability.controller)
        {
            player.bending_types_this_turn.insert(BendingType::Air);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}
