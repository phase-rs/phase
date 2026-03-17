//! Dynamic quantity resolution for QuantityExpr values.
//!
//! Evaluates QuantityRef variants (ObjectCount, PlayerCount, CountersOnSelf, etc.)
//! against the current game state at resolution time. Used by effect resolvers
//! to support "for each [X]" patterns on Draw, DealDamage, GainLife, LoseLife, Mill.

use crate::game::filter::matches_target_filter_controlled;
use crate::game::game_object::parse_counter_type;
use crate::types::ability::{PlayerFilter, QuantityExpr, QuantityRef, ResolvedAbility, TargetRef};
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// Resolve a QuantityExpr to a concrete integer value.
///
/// `controller` is the player who controls the ability (used for relative filters).
/// `source_id` is the object that owns the ability (used for CountersOnSelf, filter matching).
pub fn resolve_quantity(
    state: &GameState,
    expr: &QuantityExpr,
    controller: PlayerId,
    source_id: ObjectId,
) -> i32 {
    match expr {
        QuantityExpr::Fixed { value } => *value,
        QuantityExpr::Ref { qty } => resolve_ref(state, qty, controller, source_id, &[]),
    }
}

/// Resolve a QuantityExpr with access to the ability's targets.
///
/// Required for TargetPower which needs to look up the targeted permanent.
pub fn resolve_quantity_with_targets(
    state: &GameState,
    expr: &QuantityExpr,
    ability: &ResolvedAbility,
) -> i32 {
    match expr {
        QuantityExpr::Fixed { value } => *value,
        QuantityExpr::Ref { qty } => resolve_ref(
            state,
            qty,
            ability.controller,
            ability.source_id,
            &ability.targets,
        ),
    }
}

fn resolve_ref(
    state: &GameState,
    qty: &QuantityRef,
    controller: PlayerId,
    source_id: ObjectId,
    targets: &[TargetRef],
) -> i32 {
    let player = state.players.iter().find(|p| p.id == controller);
    match qty {
        QuantityRef::HandSize => player.map_or(0, |p| p.hand.len() as i32),
        QuantityRef::LifeTotal => player.map_or(0, |p| p.life),
        QuantityRef::GraveyardSize => player.map_or(0, |p| p.graveyard.len() as i32),
        QuantityRef::LifeAboveStarting => {
            player.map_or(0, |p| p.life - state.format_config.starting_life)
        }
        QuantityRef::ObjectCount { filter } => state
            .battlefield
            .iter()
            .filter(|&&id| {
                matches_target_filter_controlled(state, id, filter, source_id, controller)
            })
            .count() as i32,
        QuantityRef::PlayerCount { filter } => resolve_player_count(state, filter, controller),
        QuantityRef::CountersOnSelf { counter_type } => state
            .objects
            .get(&source_id)
            .map(|obj| {
                let ct = parse_counter_type(counter_type);
                obj.counters.get(&ct).copied().unwrap_or(0) as i32
            })
            .unwrap_or(0),
        QuantityRef::Variable { .. } => {
            // Variable amounts (X) are resolved during mana payment, not here.
            // Default to 0 for unresolved variables.
            0
        }
        QuantityRef::TargetPower => {
            // Find the first object target and return its power.
            targets
                .iter()
                .find_map(|t| {
                    if let TargetRef::Object(id) = t {
                        state.objects.get(id)
                    } else {
                        None
                    }
                })
                .and_then(|obj| obj.power)
                .unwrap_or(0)
        }
    }
}

/// Count players matching a PlayerFilter relative to the controller.
fn resolve_player_count(state: &GameState, filter: &PlayerFilter, controller: PlayerId) -> i32 {
    state
        .players
        .iter()
        .filter(|p| {
            !p.is_eliminated
                && match filter {
                    PlayerFilter::Opponent => p.id != controller,
                    PlayerFilter::OpponentLostLife => {
                        p.id != controller && p.life_lost_this_turn > 0
                    }
                    PlayerFilter::OpponentGainedLife => {
                        p.id != controller && p.life_gained_this_turn > 0
                    }
                    PlayerFilter::All => true,
                }
        })
        .count() as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::CounterType;
    use crate::game::zones::create_object;
    use crate::types::ability::{ControllerRef, TargetFilter, TypedFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::zones::Zone;

    #[test]
    fn resolve_quantity_fixed_returns_value() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Fixed { value: 3 };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 3);
    }

    #[test]
    fn resolve_quantity_hand_size() {
        let mut state = GameState::new_two_player(42);
        for i in 0..4 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {i}"),
                Zone::Hand,
            );
        }
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::HandSize,
        };
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(99)),
            4
        );
    }

    #[test]
    fn resolve_quantity_object_count_creatures_you_control() {
        let mut state = GameState::new_two_player(42);
        // Add 3 creatures for player 0
        for i in 0..3 {
            let id = create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Creature {i}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }
        // Add 1 creature for player 1 (should not count)
        let opp = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Opp Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        };
        // Source is controlled by player 0
        let source = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 3);
    }

    #[test]
    fn resolve_quantity_player_count_opponent_lost_life() {
        let mut state = GameState::new_two_player(42);
        // Opponent (player 1) lost life this turn
        state.players[1].life_lost_this_turn = 3;

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentLostLife,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 1);
    }

    #[test]
    fn resolve_quantity_player_count_opponent_lost_life_none_lost() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentLostLife,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 0);
    }

    #[test]
    fn resolve_quantity_player_count_opponent() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::Opponent,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 1);
    }

    #[test]
    fn resolve_quantity_counters_on_self() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Planeswalker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .counters
            .insert(CounterType::Loyalty, 4);

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::CountersOnSelf {
                counter_type: "loyalty".to_string(),
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 4);
    }

    #[test]
    fn resolve_quantity_player_filter_opponent_gained_life() {
        let mut state = GameState::new_two_player(42);
        state.players[1].life_gained_this_turn = 5;

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentGainedLife,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 1);
    }

    #[test]
    fn resolve_quantity_player_filter_all() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::All,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 2);
    }
}
