use std::collections::HashSet;

use crate::game::game_object::CounterType;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{EffectError, ResolvedAbility, TargetRef};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::proposed_event::ProposedEvent;

/// Add a +1/+1 counter to the exploring creature via the replacement pipeline.
fn add_explore_counter(state: &mut GameState, explorer_id: ObjectId, events: &mut Vec<GameEvent>) {
    let proposed = ProposedEvent::AddCounter {
        object_id: explorer_id,
        counter_type: "P1P1".to_string(),
        count: 1,
        applied: HashSet::new(),
    };

    if let ReplacementResult::Execute(ProposedEvent::AddCounter {
        object_id,
        counter_type,
        count,
        ..
    }) = replacement::replace_event(state, proposed, events)
    {
        let ct = match counter_type.as_str() {
            "P1P1" => CounterType::Plus1Plus1,
            _ => CounterType::Generic(counter_type.clone()),
        };
        if let Some(obj) = state.objects.get_mut(&object_id) {
            let entry = obj.counters.entry(ct).or_insert(0);
            *entry += count;
            state.layers_dirty = true;
        }
        events.push(GameEvent::CounterAdded {
            object_id,
            counter_type,
            count,
        });
    }
}

/// Explore: the exploring creature reveals the top card of its controller's library.
/// - If the card is a land: put a +1/+1 counter on the creature, the card stays on top.
/// - If the card is not a land: the player chooses to put it in hand or graveyard
///   (reuses WaitingFor::DigChoice with keep_count=1).
///
/// Reads `Defined` param for the exploring creature (default: Self = source_id).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Determine the exploring creature
    let defined = ability
        .params
        .get("Defined")
        .map(|s| s.as_str())
        .unwrap_or("Self");

    let explorer_id = if defined == "Targeted" {
        ability
            .targets
            .iter()
            .find_map(|t| {
                if let TargetRef::Object(id) = t {
                    Some(*id)
                } else {
                    None
                }
            })
            .unwrap_or(ability.source_id)
    } else {
        ability.source_id
    };

    let controller = ability.controller;

    // Find the controller's library
    let player = state
        .players
        .iter()
        .find(|p| p.id == controller)
        .ok_or(EffectError::PlayerNotFound)?;

    if player.library.is_empty() {
        // Nothing to explore -- just put a +1/+1 counter (per MTG rules, explore with empty library)
        add_explore_counter(state, explorer_id, events);

        events.push(GameEvent::EffectResolved {
            api_type: ability.api_type.clone(),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // Reveal top card
    let top_card_id = player.library[0];

    // Check if it's a land
    let is_land = state
        .objects
        .get(&top_card_id)
        .map(|obj| obj.card_types.core_types.contains(&CoreType::Land))
        .unwrap_or(false);

    if is_land {
        // Land: put +1/+1 counter on exploring creature, card stays on top
        add_explore_counter(state, explorer_id, events);

        events.push(GameEvent::EffectResolved {
            api_type: ability.api_type.clone(),
            source_id: ability.source_id,
        });
    } else {
        // Nonland: player chooses hand or graveyard.
        // Reuse WaitingFor::DigChoice with keep_count=1:
        //   - selected cards go to hand (keep_count=1 means choose 1 to keep)
        //   - rest go to graveyard (but there's only 1 card, so keep=hand, don't keep=graveyard)
        state.waiting_for = WaitingFor::DigChoice {
            player: controller,
            cards: vec![top_card_id],
            keep_count: 1,
        };

        events.push(GameEvent::EffectResolved {
            api_type: ability.api_type.clone(),
            source_id: ability.source_id,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;
    use std::collections::HashMap;

    fn make_explore_ability(source_id: ObjectId) -> ResolvedAbility {
        ResolvedAbility::from_raw("Explore", HashMap::new(), vec![], source_id, PlayerId(0))
    }

    #[test]
    fn test_explore_land_on_top_adds_counter() {
        let mut state = GameState::new_two_player(42);
        let explorer = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Jadelight Ranger".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&explorer).unwrap().power = Some(2);
        state.objects.get_mut(&explorer).unwrap().toughness = Some(1);
        state
            .objects
            .get_mut(&explorer)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Put a land on top of library
        let land_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let ability = make_explore_ability(explorer);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Explorer should have a +1/+1 counter
        assert_eq!(
            state.objects[&explorer].counters[&CounterType::Plus1Plus1],
            1
        );
        // Land stays on top of library
        assert_eq!(state.players[0].library[0], land_id);
    }

    #[test]
    fn test_explore_nonland_gives_choice() {
        let mut state = GameState::new_two_player(42);
        let explorer = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Merfolk Branchwalker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&explorer)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Put a nonland on top of library
        let spell_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let ability = make_explore_ability(explorer);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Should set WaitingFor::DigChoice for the player to choose
        match &state.waiting_for {
            WaitingFor::DigChoice {
                player,
                cards,
                keep_count,
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards.len(), 1);
                assert_eq!(cards[0], spell_id);
                assert_eq!(*keep_count, 1);
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        }
    }

    #[test]
    fn test_explore_empty_library_adds_counter() {
        let mut state = GameState::new_two_player(42);
        let explorer = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Explorer".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&explorer)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        assert!(state.players[0].library.is_empty());

        let ability = make_explore_ability(explorer);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // With empty library, explorer still gets +1/+1 counter
        assert_eq!(
            state.objects[&explorer].counters[&CounterType::Plus1Plus1],
            1
        );
    }
}
