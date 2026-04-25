use crate::game::zones::create_object;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::CardId;
use crate::types::zones::Zone;
use std::sync::Arc;

/// CR 114.1 + CR 114.4: Create an emblem in the command zone with the given
/// abilities (statics and triggers). Emblems are not permanents — they cannot
/// be destroyed, exiled, bounced, or sacrificed. Per CR 114.4, both static
/// and triggered abilities function from the command zone.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (statics, triggers) = match &ability.effect {
        Effect::CreateEmblem { statics, triggers } => (statics, triggers),
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
    // CR 114.5: An emblem is neither a card nor a permanent. Emblem isn't a
    // card type. Setting `is_emblem` BEFORE installing ability definitions is
    // load-bearing: `functioning_abilities::object_functions` uses this flag
    // to admit command-zone objects, so the first trigger/static scan after
    // creation sees the emblem's abilities.
    obj.is_emblem = true;
    obj.static_definitions = statics.clone().into();
    obj.base_static_definitions = Arc::new(statics.clone());
    // CR 113.1c + CR 114.4: Install triggered abilities on the emblem so
    // `active_trigger_definitions` yields them during command-zone scans.
    obj.trigger_definitions = triggers.clone().into();
    obj.base_trigger_definitions = Arc::new(triggers.clone());

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
                type_filters: vec![crate::types::ability::TypeFilter::Subtype(
                    "Ninja".to_string(),
                )],
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
            active_zones: vec![],
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
                triggers: Vec::new(),
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
                triggers: Vec::new(),
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
                triggers: Vec::new(),
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
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
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
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                up_to: false,
            },
            vec![crate::types::ability::TargetRef::Object(emblem_id)],
            ObjectId(200),
            PlayerId(1),
        );
        let mut events = Vec::new();
        super::super::sacrifice::resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.command_zone.contains(&emblem_id));
    }

    #[test]
    fn create_emblem_installs_triggered_abilities_on_command_zone_emblem() {
        // CR 113.1c + CR 114.4: An emblem-hosted triggered ability must be
        // installed as a `TriggerDefinition` on the emblem object, with both
        // the live and base stores populated so clones and layer resets
        // preserve the trigger.
        use crate::types::triggers::TriggerMode;
        let mut state = GameState::new_two_player(42);
        let trig = crate::types::ability::TriggerDefinition::new(TriggerMode::SpellCast)
            .trigger_zones(vec![Zone::Command]);
        let ability = ResolvedAbility::new(
            Effect::CreateEmblem {
                statics: Vec::new(),
                triggers: vec![trig.clone()],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let emblem_id = state.command_zone[0];
        let emblem = state.objects.get(&emblem_id).unwrap();
        assert!(emblem.is_emblem);
        assert_eq!(emblem.trigger_definitions.len(), 1);
        assert_eq!(emblem.base_trigger_definitions.len(), 1);
        // CR 114.4 gate: `active_trigger_definitions` must yield the trigger
        // because `is_emblem` is set.
        let count =
            crate::game::functioning_abilities::active_trigger_definitions(&state, emblem).count();
        assert_eq!(count, 1, "command-zone emblem trigger must be active");
    }
}
