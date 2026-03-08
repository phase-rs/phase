use crate::game::zones;
use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::CardId;
use crate::types::zones::Zone;

/// Create a token creature on the battlefield.
/// Reads `Name`, `Power`, `Toughness`, `Types`, `Colors` params.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let name = ability
        .params
        .get("Name")
        .cloned()
        .unwrap_or_else(|| "Token".to_string());

    let power: Option<i32> = ability
        .params
        .get("Power")
        .and_then(|v| v.parse().ok());

    let toughness: Option<i32> = ability
        .params
        .get("Toughness")
        .and_then(|v| v.parse().ok());

    // Use CardId(0) for tokens
    let obj_id = zones::create_object(
        state,
        CardId(0),
        ability.controller,
        name.clone(),
        Zone::Battlefield,
    );

    // Set power and toughness
    if let Some(obj) = state.objects.get_mut(&obj_id) {
        obj.power = power;
        obj.toughness = toughness;
    }

    events.push(GameEvent::TokenCreated {
        object_id: obj_id,
        name,
    });
    events.push(GameEvent::EffectResolved {
        api_type: ability.api_type.clone(),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;
    use std::collections::HashMap;

    #[test]
    fn token_creates_object_on_battlefield() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility {
            api_type: "Token".to_string(),
            params: HashMap::from([
                ("Name".to_string(), "Soldier".to_string()),
                ("Power".to_string(), "1".to_string()),
                ("Toughness".to_string(), "1".to_string()),
            ]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.battlefield.len(), 1);
        let obj_id = state.battlefield[0];
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.name, "Soldier");
        assert_eq!(obj.power, Some(1));
        assert_eq!(obj.toughness, Some(1));
        assert_eq!(obj.card_id, CardId(0));
    }

    #[test]
    fn token_emits_token_created_event() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility {
            api_type: "Token".to_string(),
            params: HashMap::from([("Name".to_string(), "Angel".to_string())]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(e, GameEvent::TokenCreated { name, .. } if name == "Angel")));
    }
}
