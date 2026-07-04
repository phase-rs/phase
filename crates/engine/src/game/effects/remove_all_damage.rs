use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::zones::Zone;

/// CR 120.6 + CR 120.3: "Remove all damage from [creature]" clears the marked
/// damage early (before the cleanup step at which it would otherwise wear off,
/// CR 514.2). Unlike `Regenerate`, it installs no shield and does not tap or
/// remove from combat — it only zeroes `damage_marked` (and the deathtouch
/// flag, mirroring the umbra/regeneration heal at replacement.rs).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // "remove all damage from ~" with no explicit target heals the source.
    let use_self = match &ability.effect {
        Effect::RemoveAllDamage { target } => {
            matches!(target, TargetFilter::None | TargetFilter::SelfRef)
                || (matches!(target, TargetFilter::Any) && ability.targets.is_empty())
        }
        _ => false,
    };

    let targets: Vec<_> = if use_self {
        vec![ability.source_id]
    } else {
        ability
            .targets
            .iter()
            .filter_map(|t| match t {
                TargetRef::Object(id) => Some(*id),
                _ => None,
            })
            .collect()
    };

    for obj_id in targets {
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            if obj.zone == Zone::Battlefield {
                heal_marked_damage(obj);
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 120.6 + CR 120.3: Clear all marked damage (and the deathtouch flag) from a
/// creature. Single authority shared by the standalone `RemoveAllDamage`
/// resolver and the Wolverine `dealt_damage_applier` heal, mirroring the
/// umbra-armor / regeneration inline heal.
pub(crate) fn heal_marked_damage(obj: &mut crate::game::game_object::GameObject) {
    obj.damage_marked = 0;
    obj.dealt_deathtouch_damage = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;

    #[test]
    fn remove_all_damage_clears_marked_damage_on_source() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Wolverine".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.damage_marked = 3;
            obj.dealt_deathtouch_damage = true;
        }

        let ability = ResolvedAbility::new(
            Effect::RemoveAllDamage {
                target: TargetFilter::SelfRef,
            },
            Vec::new(),
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(obj.damage_marked, 0, "marked damage must be cleared");
        assert!(
            !obj.dealt_deathtouch_damage,
            "deathtouch flag must be cleared"
        );
    }
}
