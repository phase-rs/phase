use crate::game::zones;
use crate::types::ability::{EffectError, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::zones::Zone;

/// Bounce: return target permanent(s) to their owner's hand.
/// Reads `Defined` param for target selection (Targeted, Self, etc.).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Determine targets: from ability.targets, or Self (source_id)
    let defined = ability
        .params
        .get("Defined")
        .map(|s| s.as_str())
        .unwrap_or("Targeted");

    let targets: Vec<_> = if defined == "Self" {
        vec![ability.source_id]
    } else {
        ability
            .targets
            .iter()
            .filter_map(|t| {
                if let TargetRef::Object(id) = t {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect()
    };

    for obj_id in targets {
        // Only bounce permanents on the battlefield
        let is_on_battlefield = state
            .objects
            .get(&obj_id)
            .map(|o| o.zone == Zone::Battlefield)
            .unwrap_or(false);

        if is_on_battlefield {
            zones::move_to_zone(state, obj_id, Zone::Hand, events);
        }
    }

    events.push(GameEvent::EffectResolved {
        api_type: ability.api_type.clone(),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use std::collections::HashMap;

    #[test]
    fn test_bounce_moves_permanent_to_hand() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "Bounce".to_string(),
                params: std::collections::HashMap::new(),
            },
            api_type: "Bounce".to_string(),
            params: HashMap::new(),
            targets: vec![TargetRef::Object(obj_id)],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[1].hand.contains(&obj_id));
    }

    #[test]
    fn test_bounce_self() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Ninja".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "Bounce".to_string(),
                params: std::collections::HashMap::new(),
            },
            api_type: "Bounce".to_string(),
            params: HashMap::from([("Defined".to_string(), "Self".to_string())]),
            targets: vec![],
            source_id: obj_id,
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[0].hand.contains(&obj_id));
    }

    #[test]
    fn test_bounce_emits_zone_changed() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "Bounce".to_string(),
                params: std::collections::HashMap::new(),
            },
            api_type: "Bounce".to_string(),
            params: HashMap::new(),
            targets: vec![TargetRef::Object(obj_id)],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::ZoneChanged {
                from: Zone::Battlefield,
                to: Zone::Hand,
                ..
            }
        )));
    }
}
