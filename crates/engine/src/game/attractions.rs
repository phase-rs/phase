//! CR 717 + CR 701.51 + CR 701.52: Unfinity Attraction deck, open, and visit.
//!
//! Attractions live in a supplementary deck (command zone) tracked per player via
//! `Player::attraction_deck`. Opening moves the top card to the battlefield; rolling
//! to visit is a turn-based action at the beginning of the active player's precombat
//! main phase when they control an Attraction.

use crate::types::ability::{EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::effects::roll_die;
use super::game_object::GameObject;
use super::zones;
use crate::types::ability::EffectError;

/// CR 717.1: Default lit numbers when card data omits variant lights (1 and 6 are always lit).
pub fn default_attraction_lights() -> Vec<u8> {
    vec![1, 6]
}

pub fn is_attraction_card(obj: &GameObject) -> bool {
    obj.in_attraction_deck
        || !obj.attraction_lights.is_empty()
        || obj
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Attraction"))
}

pub fn is_attraction_permanent(obj: &GameObject) -> bool {
    obj.zone == Zone::Battlefield && is_attraction_card(obj)
}

/// CR 701.51b: Put the top card of the controller's Attraction deck onto the battlefield.
pub fn open_attractions(
    state: &mut GameState,
    player: PlayerId,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    for _ in 0..count {
        let Some(object_id) = state
            .players
            .iter_mut()
            .find(|p| p.id == player)
            .and_then(|p| p.attraction_deck.pop_front())
        else {
            // CR 609.3: If the player has fewer Attractions than requested, open
            // as many as possible and ignore the impossible remainder.
            break;
        };
        zones::move_to_zone(state, object_id, Zone::Battlefield, events);
        if let Some(obj) = state.objects.get_mut(&object_id) {
            obj.in_attraction_deck = false;
        }
        events.push(GameEvent::AttractionOpened {
            player_id: player,
            object_id,
        });
    }
    Ok(())
}

/// CR 701.52a: Roll a d6 and visit each controlled Attraction whose lights include the result.
pub fn roll_to_visit_attractions(
    state: &mut GameState,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) {
    if !controls_attraction(state, player) {
        return;
    }
    let roll = roll_die::roll_die(state, player, 6, events);
    events.push(GameEvent::AttractionsRolledToVisit {
        player_id: player,
        roll,
    });
    let visited = visited_attraction_ids(state, player, roll);
    for attraction_id in visited {
        events.push(GameEvent::AttractionVisited {
            player_id: player,
            roll,
            attraction_id,
        });
    }
    super::triggers::process_triggers(state, events);
}

/// CR 703.4g + CR 717.4: Turn-based action at the beginning of the precombat main phase.
pub fn perform_roll_to_visit_turn_based_action(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let player = state.active_player;
    roll_to_visit_attractions(state, player, events);
}

fn controls_attraction(state: &GameState, player: PlayerId) -> bool {
    state.battlefield.iter().any(|id| {
        state
            .objects
            .get(id)
            // CR 702.26b: a phased-out permanent is treated as though it does not exist.
            .is_some_and(|o| {
                o.controller == player && o.is_phased_in() && is_attraction_permanent(o)
            })
    })
}

fn visited_attraction_ids(state: &GameState, player: PlayerId, roll: u8) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .filter_map(|id| {
            let obj = state.objects.get(id)?;
            // CR 702.26b: a phased-out permanent is treated as though it does not exist.
            if obj.controller != player || !obj.is_phased_in() || !is_attraction_permanent(obj) {
                return None;
            }
            if obj.attraction_lights.contains(&roll) {
                Some(*id)
            } else {
                None
            }
        })
        .collect()
}

pub fn resolve_open(
    state: &mut GameState,
    ability: &ResolvedAbility,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    open_attractions(state, ability.controller, count, events)?;
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::OpenAttractions,
        source_id: ability.source_id,
    });
    Ok(())
}

pub fn resolve_roll_to_visit(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    roll_to_visit_attractions(state, ability.controller, events);
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::RollToVisitAttractions,
        source_id: ability.source_id,
    });
    Ok(())
}
