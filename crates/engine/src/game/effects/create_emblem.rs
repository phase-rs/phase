use crate::game::zones::create_object;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::CardId;
use crate::types::zones::Zone;

/// CR 114.1: Create an emblem in the command zone with the given static abilities.
/// Emblems are not permanents — they cannot be destroyed, exiled, bounced, or sacrificed.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let statics = match &ability.effect {
        Effect::CreateEmblem { statics } => statics,
        _ => return Err(EffectError::MissingParam("CreateEmblem".into())),
    };

    // CR 114.1: Create emblem in command zone owned by the ability's controller
    let emblem_id = create_object(
        state,
        CardId(0),
        ability.controller,
        "Emblem".to_string(),
        Zone::Command,
    );
    let obj = state.objects.get_mut(&emblem_id).unwrap();
    obj.is_emblem = true;
    obj.static_definitions = statics.clone();
    obj.base_static_definitions = statics.clone();

    state.layers_dirty = true;
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        ContinuousModification, ControllerRef, StaticDefinition, TargetFilter, TypedFilter,
    };
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;
    use crate::types::statics::StaticMode;

    fn ninja_pump_static() -> StaticDefinition {
        StaticDefinition {
            mode: StaticMode::Continuous,
            affected: Some(TargetFilter::Typed(TypedFilter {
                card_type: None,
                subtype: Some("Ninja".to_string()),
                controller: Some(ControllerRef::You),
                properties: vec![],
            })),
            modifications: vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            characteristic_defining: false,
            description: None,
        }
    }

    #[test]
    fn create_emblem_creates_object_in_command_zone() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::CreateEmblem {
                statics: vec![ninja_pump_static()],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Emblem should be in command zone
        assert_eq!(state.command_zone.len(), 1);
        let emblem_id = state.command_zone[0];
        let emblem = state.objects.get(&emblem_id).unwrap();
        assert!(emblem.is_emblem);
        assert_eq!(emblem.zone, Zone::Command);
        assert_eq!(emblem.controller, PlayerId(0));
        assert_eq!(emblem.static_definitions.len(), 1);
        assert_eq!(emblem.base_static_definitions.len(), 1);
    }

    #[test]
    fn create_emblem_marks_layers_dirty() {
        let mut state = GameState::new_two_player(42);
        state.layers_dirty = false;
        let ability = ResolvedAbility::new(
            Effect::CreateEmblem {
                statics: vec![ninja_pump_static()],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.layers_dirty);
    }

    /// Helper: create an emblem and return its ObjectId
    fn create_test_emblem(state: &mut GameState) -> ObjectId {
        let ability = ResolvedAbility::new(
            Effect::CreateEmblem {
                statics: vec![ninja_pump_static()],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(state, &ability, &mut events).unwrap();
        state.command_zone[0]
    }

    #[test]
    fn destroy_targeting_emblem_is_noop() {
        let mut state = GameState::new_two_player(42);
        let emblem_id = create_test_emblem(&mut state);

        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![crate::types::ability::TargetRef::Object(emblem_id)],
            ObjectId(200),
            PlayerId(1),
        );
        let mut events = Vec::new();
        super::super::destroy::resolve(&mut state, &ability, &mut events).unwrap();

        // Emblem still exists in command zone
        assert!(state.command_zone.contains(&emblem_id));
        assert!(state.objects.contains_key(&emblem_id));
    }

    #[test]
    fn change_zone_exile_targeting_emblem_is_noop() {
        let mut state = GameState::new_two_player(42);
        let emblem_id = create_test_emblem(&mut state);

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Command),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
            },
            vec![crate::types::ability::TargetRef::Object(emblem_id)],
            ObjectId(200),
            PlayerId(1),
        );
        let mut events = Vec::new();
        super::super::change_zone::resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.command_zone.contains(&emblem_id));
        assert_eq!(state.objects[&emblem_id].zone, Zone::Command);
    }

    #[test]
    fn bounce_targeting_emblem_is_noop() {
        let mut state = GameState::new_two_player(42);
        let emblem_id = create_test_emblem(&mut state);

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
            },
            vec![crate::types::ability::TargetRef::Object(emblem_id)],
            ObjectId(200),
            PlayerId(1),
        );
        let mut events = Vec::new();
        super::super::bounce::resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.command_zone.contains(&emblem_id));
    }

    #[test]
    fn sacrifice_targeting_emblem_is_noop() {
        let mut state = GameState::new_two_player(42);
        let emblem_id = create_test_emblem(&mut state);

        let ability = ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Any,
            },
            vec![crate::types::ability::TargetRef::Object(emblem_id)],
            ObjectId(200),
            PlayerId(1),
        );
        let mut events = Vec::new();
        super::super::sacrifice::resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.command_zone.contains(&emblem_id));
    }
}
