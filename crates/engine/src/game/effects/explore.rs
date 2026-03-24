use std::collections::HashSet;

use crate::game::game_object::CounterType;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::proposed_event::ProposedEvent;

/// Add a +1/+1 counter to the exploring creature via the replacement pipeline.
fn add_explore_counter(state: &mut GameState, explorer_id: ObjectId, events: &mut Vec<GameEvent>) {
    let proposed = ProposedEvent::AddCounter {
        object_id: explorer_id,
        counter_type: CounterType::Plus1Plus1,
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
        if let Some(obj) = state.objects.get_mut(&object_id) {
            let entry = obj.counters.entry(counter_type.clone()).or_insert(0);
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

/// CR 701.44a: Explore — reveal the top card of the exploring creature's controller's library.
/// - If the card is a land: put it into that player's hand (no counter).
/// - If the card is not a land: put a +1/+1 counter on the creature, then the player
///   chooses to put the card back on top or into their graveyard
///   (reuses WaitingFor::DigChoice with keep_count=1).
///
/// The exploring creature defaults to the ability's source_id.
/// If the ability has a targeted object, that creature explores instead.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Determine the exploring creature
    let explorer_id = ability
        .targets
        .iter()
        .find_map(|t| {
            if let TargetRef::Object(id) = t {
                Some(*id)
            } else {
                None
            }
        })
        .unwrap_or(ability.source_id);

    let controller = ability.controller;

    // Find the controller's library
    let player = state
        .players
        .iter()
        .find(|p| p.id == controller)
        .ok_or(EffectError::PlayerNotFound)?;

    if player.library.is_empty() {
        // CR 701.44a: Explore with empty library — just put a +1/+1 counter.
        add_explore_counter(state, explorer_id, events);

        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // Reveal top card
    let top_card_id = player.library[0];
    let revealed_name = state
        .objects
        .get(&top_card_id)
        .map(|o| o.name.clone())
        .unwrap_or_default();
    events.push(GameEvent::CardsRevealed {
        player: controller,
        card_ids: vec![top_card_id],
        card_names: vec![revealed_name],
    });

    // Check if it's a land
    let is_land = state
        .objects
        .get(&top_card_id)
        .map(|obj| obj.card_types.core_types.contains(&CoreType::Land))
        .unwrap_or(false);

    if is_land {
        // CR 701.44a: Land revealed — put the card into the player's hand. No counter.
        if let Some(player) = state.players.iter_mut().find(|p| p.id == controller) {
            player.library.retain(|id| *id != top_card_id);
            player.hand.push(top_card_id);
        }
        if let Some(obj) = state.objects.get_mut(&top_card_id) {
            obj.zone = crate::types::zones::Zone::Hand;
        }

        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
    } else {
        // CR 701.44a: Nonland revealed — put a +1/+1 counter on the creature,
        // then player chooses to put the card back on top or into graveyard.
        add_explore_counter(state, explorer_id, events);

        // Reuse WaitingFor::DigChoice with keep_count=1:
        //   - selected cards go to hand (keep_count=1 means choose 1 to keep)
        //   - rest go to graveyard (but there's only 1 card, so keep=hand, don't keep=graveyard)
        state.waiting_for = WaitingFor::DigChoice {
            player: controller,
            selectable_cards: vec![top_card_id],
            cards: vec![top_card_id],
            keep_count: 1,
            up_to: false,
            kept_destination: None,
            rest_destination: None,
            source_id: Some(ability.source_id),
        };

        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::Effect;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_explore_ability(source_id: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(Effect::Explore, vec![], source_id, PlayerId(0))
    }

    #[test]
    fn test_explore_land_goes_to_hand() {
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

        // CR 701.44a: Land revealed — no counter on explorer
        assert!(!state.objects[&explorer]
            .counters
            .contains_key(&CounterType::Plus1Plus1));
        // Land moved to hand
        assert!(state.players[0].hand.contains(&land_id));
        // Land removed from library
        assert!(!state.players[0].library.contains(&land_id));
    }

    #[test]
    fn test_explore_nonland_adds_counter_and_gives_choice() {
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

        // CR 701.44a: Nonland revealed — explorer gets +1/+1 counter
        assert_eq!(
            state.objects[&explorer].counters[&CounterType::Plus1Plus1],
            1
        );

        // Player chooses to put card back on top or into graveyard
        match &state.waiting_for {
            WaitingFor::DigChoice {
                player,
                cards,
                keep_count,
                ..
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
