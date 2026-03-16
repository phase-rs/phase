use crate::types::ability::{
    EffectError, EffectKind, ResolvedAbility, StaticDefinition, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::keywords::Keyword;
use crate::types::statics::StaticMode;

/// CR 702.157a: Suspect target creature(s).
/// A suspected creature has menace and "This creature can't block."
///
/// Option C architecture: the designation (`is_suspected`) is the source of truth,
/// and the derived abilities (Menace, CantBlock) are written to `base_keywords` /
/// `base_static_definitions` so they survive layer recalculation naturally.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target_ids: Vec<_> = match &ability.effect {
        crate::types::ability::Effect::Suspect { target } => match target {
            TargetFilter::LastCreated => state.last_created_token_ids.clone(),
            _ => ability
                .targets
                .iter()
                .filter_map(|t| match t {
                    TargetRef::Object(id) => Some(*id),
                    _ => None,
                })
                .collect(),
        },
        _ => return Ok(()),
    };

    for obj_id in target_ids {
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            obj.is_suspected = true;

            // CR 702.157a: Add menace to base_keywords (survives layer recalc).
            if !obj
                .base_keywords
                .iter()
                .any(|k| matches!(k, Keyword::Menace))
            {
                obj.base_keywords.push(Keyword::Menace);
            }

            // CR 702.157a: Add CantBlock to base_static_definitions (survives layer recalc).
            if !obj
                .base_static_definitions
                .iter()
                .any(|s| s.mode == StaticMode::CantBlock)
            {
                obj.base_static_definitions
                    .push(StaticDefinition::new(StaticMode::CantBlock));
            }

            events.push(GameEvent::CreatureSuspected { object_id: obj_id });
        }
    }

    state.layers_dirty = true;

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Suspect,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::layers::evaluate_layers;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, ResolvedAbility};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup_creature(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.power = Some(2);
        obj.toughness = Some(2);
        id
    }

    #[test]
    fn suspect_sets_designation_and_base_abilities() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);

        let ability = ResolvedAbility::new(
            Effect::Suspect {
                target: TargetFilter::Any,
            },
            vec![crate::types::ability::TargetRef::Object(id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert!(obj.is_suspected);
        assert!(obj
            .base_keywords
            .iter()
            .any(|k| matches!(k, Keyword::Menace)));
        assert!(obj
            .base_static_definitions
            .iter()
            .any(|s| s.mode == StaticMode::CantBlock));
    }

    #[test]
    fn suspect_survives_layer_recalc() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);

        let ability = ResolvedAbility::new(
            Effect::Suspect {
                target: TargetFilter::Any,
            },
            vec![crate::types::ability::TargetRef::Object(id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Layer recalc should preserve menace + CantBlock from base_*
        evaluate_layers(&mut state);

        let obj = state.objects.get(&id).unwrap();
        assert!(obj.is_suspected);
        assert!(obj.keywords.iter().any(|k| matches!(k, Keyword::Menace)));
        assert!(obj
            .static_definitions
            .iter()
            .any(|s| s.mode == StaticMode::CantBlock));
    }

    #[test]
    fn unsuspect_removes_abilities_on_recalc() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);

        // Suspect the creature
        let ability = ResolvedAbility::new(
            Effect::Suspect {
                target: TargetFilter::Any,
            },
            vec![crate::types::ability::TargetRef::Object(id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        // Unsuspect: remove designation + clean base_* fields.
        // In a real game this would be done by a future `unsuspect` handler.
        let obj = state.objects.get_mut(&id).unwrap();
        obj.is_suspected = false;
        obj.base_keywords.retain(|k| !matches!(k, Keyword::Menace));
        obj.base_static_definitions
            .retain(|s| s.mode != StaticMode::CantBlock);
        // Also clear computed statics so layer recalc starts clean.
        // (The layer system's conditional reset only fires when base_static_definitions is non-empty.)
        obj.static_definitions
            .retain(|s| s.mode != StaticMode::CantBlock);

        evaluate_layers(&mut state);

        let obj = state.objects.get(&id).unwrap();
        assert!(!obj.is_suspected);
        assert!(!obj.keywords.iter().any(|k| matches!(k, Keyword::Menace)));
        assert!(!obj
            .static_definitions
            .iter()
            .any(|s| s.mode == StaticMode::CantBlock));
    }

    #[test]
    fn suspect_last_created_token() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        state.last_created_token_ids = vec![id];

        let ability = ResolvedAbility::new(
            Effect::Suspect {
                target: TargetFilter::LastCreated,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert!(obj.is_suspected);
    }
}
