use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// Cleanup effect: clears transient card state after complex ability chains.
///
/// In Forge Java, this clears remembered objects, chosen colors/types/players,
/// imprinted cards, coin flip results, and delayed triggers. It is almost always
/// the final SubAbility in a chain (e.g., `SubAbility$ DBCleanup`).
///
/// Supported params (all optional, currently no-ops until transient state tracking
/// is implemented):
///   - `ClearRemembered` — clear remembered objects
///   - `ForgetDefined` — selectively remove entities from remembered
///   - `ClearImprinted` — clear imprinted cards
///   - `ClearTriggered` — clear delayed triggers
///   - `ClearCoinFlips` — clear coin flip results
///   - `ClearChosenCard` — clear chosen card(s)
///   - `ClearChosenPlayer` — clear chosen player
///   - `ClearChosenType` — clear chosen type(s)
///   - `ClearChosenColor` — clear chosen color(s)
///   - `ClearNamedCard` — clear named card(s)
pub fn resolve(
    _state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Cleanup is a state-clearing utility. When transient state tracking
    // (remembered, chosen, imprinted) is added to GameState/GameObject,
    // this handler will clear those fields based on the params above.

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
    fn cleanup_emits_effect_resolved() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility {
            api_type: "Cleanup".to_string(),
            params: HashMap::from([("ClearRemembered".to_string(), "True".to_string())]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(
            |e| matches!(e, GameEvent::EffectResolved { api_type, .. } if api_type == "Cleanup")
        ));
    }

    #[test]
    fn cleanup_succeeds_with_no_params() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility {
            api_type: "Cleanup".to_string(),
            params: HashMap::new(),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        assert!(resolve(&mut state, &ability, &mut events).is_ok());
    }

    #[test]
    fn cleanup_succeeds_with_multiple_clear_params() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility {
            api_type: "Cleanup".to_string(),
            params: HashMap::from([
                ("ClearRemembered".to_string(), "True".to_string()),
                ("ClearChosenPlayer".to_string(), "True".to_string()),
                ("ClearChosenCard".to_string(), "True".to_string()),
            ]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        assert!(resolve(&mut state, &ability, &mut events).is_ok());
        assert_eq!(events.len(), 1);
    }
}
