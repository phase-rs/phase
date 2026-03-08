use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{EffectError, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// Discard cards from the controller's hand.
/// Reads `NumCards` param (default 1).
/// If targets specify specific cards, discard those; otherwise discard from end of hand.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let num_cards: u32 = ability
        .params
        .get("NumCards")
        .map(|v| v.parse().unwrap_or(1))
        .unwrap_or(1);

    // Check if targets specify specific cards to discard
    let specific_targets: Vec<_> = ability
        .targets
        .iter()
        .filter_map(|t| {
            if let TargetRef::Object(obj_id) = t {
                Some(*obj_id)
            } else {
                None
            }
        })
        .collect();

    if !specific_targets.is_empty() {
        // Discard specific targeted cards
        for obj_id in specific_targets {
            let obj = state
                .objects
                .get(&obj_id)
                .ok_or(EffectError::ObjectNotFound(obj_id))?;
            if obj.zone != Zone::Hand {
                continue;
            }
            let player_id = obj.owner;

            let proposed = ProposedEvent::Discard {
                player_id,
                object_id: obj_id,
                applied: HashSet::new(),
            };

            match replacement::replace_event(state, proposed, events) {
                ReplacementResult::Execute(event) => {
                    match event {
                        ProposedEvent::Discard { player_id: pid, object_id: oid, .. } => {
                            zones::move_to_zone(state, oid, Zone::Graveyard, events);
                            events.push(GameEvent::Discarded {
                                player_id: pid,
                                object_id: oid,
                            });
                        }
                        ProposedEvent::ZoneChange { object_id: oid, to, .. } => {
                            // Replacement redirected (e.g., exile instead of graveyard)
                            zones::move_to_zone(state, oid, to, events);
                        }
                        _ => {}
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
    } else {
        // Discard from end of hand (random selection deferred)
        let player = state
            .players
            .iter()
            .find(|p| p.id == ability.controller)
            .ok_or(EffectError::PlayerNotFound)?;

        let cards_to_discard: Vec<_> = player
            .hand
            .iter()
            .rev()
            .take(num_cards as usize)
            .copied()
            .collect();

        for obj_id in cards_to_discard {
            let proposed = ProposedEvent::Discard {
                player_id: ability.controller,
                object_id: obj_id,
                applied: HashSet::new(),
            };

            match replacement::replace_event(state, proposed, events) {
                ReplacementResult::Execute(event) => {
                    match event {
                        ProposedEvent::Discard { player_id, object_id, .. } => {
                            zones::move_to_zone(state, object_id, Zone::Graveyard, events);
                            events.push(GameEvent::Discarded {
                                player_id,
                                object_id,
                            });
                        }
                        ProposedEvent::ZoneChange { object_id, to, .. } => {
                            zones::move_to_zone(state, object_id, to, events);
                        }
                        _ => {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use std::collections::HashMap;

    #[test]
    fn discard_moves_card_from_hand_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(&mut state, CardId(1), PlayerId(0), "Card".to_string(), Zone::Hand);
        let ability = ResolvedAbility {
            api_type: "DiscardCard".to_string(),
            params: HashMap::from([("NumCards".to_string(), "1".to_string())]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.players[0].hand.contains(&card));
        assert!(state.players[0].graveyard.contains(&card));
    }

    #[test]
    fn discard_specific_target() {
        let mut state = GameState::new_two_player(42);
        let c1 = create_object(&mut state, CardId(1), PlayerId(0), "Keep".to_string(), Zone::Hand);
        let c2 = create_object(&mut state, CardId(2), PlayerId(0), "Discard".to_string(), Zone::Hand);
        let ability = ResolvedAbility {
            api_type: "DiscardCard".to_string(),
            params: HashMap::new(),
            targets: vec![TargetRef::Object(c2)],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[0].hand.contains(&c1));
        assert!(!state.players[0].hand.contains(&c2));
    }

    #[test]
    fn discard_emits_discarded_event() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(&mut state, CardId(1), PlayerId(0), "Card".to_string(), Zone::Hand);
        let ability = ResolvedAbility {
            api_type: "DiscardCard".to_string(),
            params: HashMap::from([("NumCards".to_string(), "1".to_string())]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(e, GameEvent::Discarded { object_id, .. } if *object_id == card)));
    }
}
