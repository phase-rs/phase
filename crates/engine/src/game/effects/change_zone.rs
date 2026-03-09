use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{EffectError, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// Parse a zone string to Zone enum.
fn parse_zone(s: &str) -> Result<Zone, EffectError> {
    match s {
        "Battlefield" => Ok(Zone::Battlefield),
        "Hand" => Ok(Zone::Hand),
        "Graveyard" => Ok(Zone::Graveyard),
        "Library" => Ok(Zone::Library),
        "Exile" => Ok(Zone::Exile),
        "Stack" => Ok(Zone::Stack),
        "Command" => Ok(Zone::Command),
        _ => Err(EffectError::InvalidParam(format!("unknown zone: {}", s))),
    }
}

/// Move target objects between zones.
/// Reads `Origin` and `Destination` params.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let destination = ability
        .params
        .get("Destination")
        .ok_or_else(|| EffectError::MissingParam("Destination".to_string()))?;
    let dest_zone = parse_zone(destination)?;

    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            let from_zone = state
                .objects
                .get(obj_id)
                .map(|o| o.zone)
                .unwrap_or(Zone::Battlefield);

            let proposed = ProposedEvent::ZoneChange {
                object_id: *obj_id,
                from: from_zone,
                to: dest_zone,
                cause: Some(ability.source_id),
                applied: HashSet::new(),
            };

            match replacement::replace_event(state, proposed, events) {
                ReplacementResult::Execute(event) => {
                    if let ProposedEvent::ZoneChange { object_id, to, .. } = event {
                        zones::move_to_zone(state, object_id, to, events);
                        if to == Zone::Battlefield || from_zone == Zone::Battlefield {
                            state.layers_dirty = true;
                        }
                    }
                }
                ReplacementResult::Prevented => {}
                ReplacementResult::NeedsChoice(player) => {
                    let candidate_count = state
                        .pending_replacement
                        .as_ref()
                        .map(|p| p.candidates.len())
                        .unwrap_or(0);
                    state.waiting_for = crate::types::game_state::WaitingFor::ReplacementChoice {
                        player,
                        candidate_count,
                    };
                    return Ok(());
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

/// Move all objects matching the `Valid` filter from `Origin` zone to `Destination` zone.
/// Reads `Origin`, `Destination`, and `Valid` params.
pub fn resolve_all(
    _state: &mut GameState,
    _ability: &ResolvedAbility,
    _events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    todo!("ChangeZoneAll not yet implemented")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use std::collections::HashMap;

    #[test]
    fn move_from_hand_to_battlefield() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(&mut state, CardId(1), PlayerId(0), "Card".to_string(), Zone::Hand);
        let ability = ResolvedAbility {
            api_type: "ChangeZone".to_string(),
            params: HashMap::from([
                ("Origin".to_string(), "Hand".to_string()),
                ("Destination".to_string(), "Battlefield".to_string()),
            ]),
            targets: vec![TargetRef::Object(obj_id)],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.battlefield.contains(&obj_id));
        assert!(!state.players[0].hand.contains(&obj_id));
    }

    #[test]
    fn move_to_exile() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(&mut state, CardId(1), PlayerId(0), "Card".to_string(), Zone::Battlefield);
        let ability = ResolvedAbility {
            api_type: "ChangeZone".to_string(),
            params: HashMap::from([
                ("Origin".to_string(), "Battlefield".to_string()),
                ("Destination".to_string(), "Exile".to_string()),
            ]),
            targets: vec![TargetRef::Object(obj_id)],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.exile.contains(&obj_id));
    }

    #[test]
    fn change_zone_all_bounce_opponent_creatures() {
        let mut state = GameState::new_two_player(42);
        let opp1 = create_object(&mut state, CardId(1), PlayerId(1), "Opp Bear".to_string(), Zone::Battlefield);
        state.objects.get_mut(&opp1).unwrap().card_types.core_types.push(CoreType::Creature);

        let opp2 = create_object(&mut state, CardId(2), PlayerId(1), "Opp Wolf".to_string(), Zone::Battlefield);
        state.objects.get_mut(&opp2).unwrap().card_types.core_types.push(CoreType::Creature);

        // Controller's creature should stay
        let mine = create_object(&mut state, CardId(3), PlayerId(0), "My Bear".to_string(), Zone::Battlefield);
        state.objects.get_mut(&mine).unwrap().card_types.core_types.push(CoreType::Creature);

        let ability = ResolvedAbility {
            api_type: "ChangeZoneAll".to_string(),
            params: HashMap::from([
                ("Origin".to_string(), "Battlefield".to_string()),
                ("Destination".to_string(), "Hand".to_string()),
                ("Valid".to_string(), "Creature.OppCtrl".to_string()),
            ]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        // Opponent creatures bounced to hand
        assert!(state.players[1].hand.contains(&opp1));
        assert!(state.players[1].hand.contains(&opp2));
        assert!(!state.battlefield.contains(&opp1));
        assert!(!state.battlefield.contains(&opp2));
        // Controller's creature stays
        assert!(state.battlefield.contains(&mine));
    }
}
