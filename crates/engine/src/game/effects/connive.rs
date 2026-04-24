use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// CR 701.50a: Connive — draw N cards, then discard N cards. For each nonland
/// card discarded this way, put a +1/+1 counter on the conniving creature.
///
/// If the player has more cards than `count` after drawing, sets
/// `WaitingFor::ConniveDiscard` for the player to choose which cards to discard.
/// Otherwise auto-discards (0 or 1 card) and adds counters inline.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let count = match &ability.effect {
        Effect::Connive { count, .. } => *count,
        _ => 1,
    };

    // Determine conniving creature: first object target, or source
    let conniver_id = ability
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

    // Step 1: Draw `count` cards for the controller.
    // Route through replacement pipeline (Dredge, Abundance, etc.).
    let proposed = ProposedEvent::Draw {
        player_id: controller,
        count,
        applied: HashSet::new(),
    };
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if let ProposedEvent::Draw {
                player_id,
                count: draw_count,
                ..
            } = event
            {
                let player = state
                    .players
                    .iter()
                    .find(|p| p.id == player_id)
                    .ok_or(EffectError::PlayerNotFound)?;

                let cards_to_draw: Vec<_> = player
                    .library
                    .iter()
                    .take(draw_count as usize)
                    .copied()
                    .collect();

                if draw_count > 0 && cards_to_draw.len() < draw_count as usize {
                    if let Some(p) = state.players.iter_mut().find(|p| p.id == player_id) {
                        p.drew_from_empty_library = true;
                    }
                }

                for obj_id in cards_to_draw {
                    zones::move_to_zone(state, obj_id, Zone::Hand, events);
                    events.push(GameEvent::CardDrawn {
                        player_id,
                        object_id: obj_id,
                    });
                    // CR 702.94a: Connive draws count as draws for miracle tracking.
                    super::draw::record_first_draw_and_enqueue_miracle(state, player_id, obj_id);
                    if let Some(p) = state.players.iter_mut().find(|p| p.id == player_id) {
                        p.cards_drawn_this_turn = p.cards_drawn_this_turn.saturating_add(1);
                    }
                }
            }
        }
        ReplacementResult::Prevented => {
            // Draw was prevented — skip the discard step
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Connive,
                source_id: ability.source_id,
            });
            return Ok(());
        }
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            return Ok(());
        }
    }

    // Step 2: Discard `count` cards.
    let hand_cards: Vec<ObjectId> = state
        .players
        .iter()
        .find(|p| p.id == controller)
        .map(|p| p.hand.iter().copied().collect())
        .unwrap_or_default();

    let discard_count = count as usize;

    if hand_cards.is_empty() {
        // No cards to discard — skip
    } else if hand_cards.len() <= discard_count {
        // Auto-discard all cards in hand (no choice needed)
        let Some(nonland_count) =
            discard_all_and_count_nonlands(state, &hand_cards, controller, events)
        else {
            // Replacement choice interrupted the discard loop — waiting_for already set.
            return Ok(());
        };
        add_connive_counters(state, conniver_id, nonland_count, events);
    } else {
        // Player must choose which cards to discard
        state.waiting_for = WaitingFor::ConniveDiscard {
            player: controller,
            conniver_id,
            source_id: ability.source_id,
            cards: hand_cards,
            count: discard_count,
        };
        // Don't emit EffectResolved yet — it will be emitted when the choice is made
        return Ok(());
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Connive,
        source_id: ability.source_id,
    });
    Ok(())
}

/// Discard all given cards and return how many were nonland.
/// Returns `None` if a replacement effect needs a player choice (interrupts the loop).
/// Caller is responsible for setting `state.waiting_for` when `None` is returned.
pub(crate) fn discard_all_and_count_nonlands(
    state: &mut GameState,
    cards: &[ObjectId],
    player: crate::types::player::PlayerId,
    events: &mut Vec<GameEvent>,
) -> Option<u32> {
    let mut nonland_count = 0;
    for &card_id in cards {
        let is_nonland = is_nonland_card(state, card_id);
        if let super::discard::DiscardOutcome::NeedsReplacementChoice(choice_player) =
            super::discard::discard_as_cost(state, card_id, player, events)
        {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(choice_player, state);
            return None;
        }
        if is_nonland {
            nonland_count += 1;
        }
    }
    Some(nonland_count)
}

/// Check if a card is nonland (before discarding it, while it's still accessible).
fn is_nonland_card(state: &GameState, object_id: ObjectId) -> bool {
    state.objects.get(&object_id).is_some_and(|obj| {
        !obj.card_types
            .core_types
            .contains(&crate::types::card_type::CoreType::Land)
    })
}

/// Add +1/+1 counters to the conniving creature via the replacement pipeline.
/// CR 701.50c: If the creature left the battlefield, skip the counter.
pub(crate) fn add_connive_counters(
    state: &mut GameState,
    conniver_id: ObjectId,
    count: u32,
    events: &mut Vec<GameEvent>,
) {
    if count == 0 {
        return;
    }
    // CR 701.50c: Skip if the conniver has left the battlefield
    let on_battlefield = state
        .objects
        .get(&conniver_id)
        .is_some_and(|o| o.zone == Zone::Battlefield);
    if !on_battlefield {
        return;
    }

    let proposed = ProposedEvent::AddCounter {
        object_id: conniver_id,
        counter_type: CounterType::Plus1Plus1,
        count,
        applied: HashSet::new(),
    };
    if let ReplacementResult::Execute(ProposedEvent::AddCounter {
        object_id,
        counter_type,
        count: final_count,
        ..
    }) = replacement::replace_event(state, proposed, events)
    {
        if let Some(obj) = state.objects.get_mut(&object_id) {
            let entry = obj.counters.entry(counter_type.clone()).or_insert(0);
            *entry += final_count;
            state.layers_dirty = true;
        }
        events.push(GameEvent::CounterAdded {
            object_id,
            counter_type,
            count: final_count,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::TargetFilter;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;

    fn make_connive_ability(source: ObjectId, target: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Connive {
                target: TargetFilter::Any,
                count: 1,
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        )
    }

    fn add_card_to_library(
        state: &mut GameState,
        player: PlayerId,
        name: &str,
        is_land: bool,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            player,
            name.to_string(),
            Zone::Library,
        );
        if is_land {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Land);
        }
        id
    }

    fn add_card_to_hand(
        state: &mut GameState,
        player: PlayerId,
        name: &str,
        is_land: bool,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            player,
            name.to_string(),
            Zone::Hand,
        );
        if is_land {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Land);
        }
        id
    }

    #[test]
    fn connive_sets_waiting_for_when_multiple_cards_in_hand() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let conniver = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Conniver".to_string(),
            Zone::Battlefield,
        );

        // Add a card to library (will be drawn)
        add_card_to_library(&mut state, PlayerId(0), "Drawn", false);
        // Add an existing card in hand (so after draw, hand has 2 cards — choice needed)
        add_card_to_hand(&mut state, PlayerId(0), "Existing", false);

        let ability = make_connive_ability(source, conniver);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ConniveDiscard { count: 1, .. }
        ));
    }

    #[test]
    fn connive_auto_discards_single_card_nonland_adds_counter() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let conniver = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Conniver".to_string(),
            Zone::Battlefield,
        );

        // Add a nonland card to library — will be drawn, then auto-discarded
        add_card_to_library(&mut state, PlayerId(0), "Spell", false);
        // Empty hand, so after draw there's exactly 1 card → auto-discard

        let ability = make_connive_ability(source, conniver);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should have added a +1/+1 counter (nonland discarded)
        let obj = state.objects.get(&conniver).unwrap();
        assert_eq!(
            obj.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            1
        );
    }

    #[test]
    fn connive_auto_discards_land_no_counter() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let conniver = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Conniver".to_string(),
            Zone::Battlefield,
        );

        // Add a land card to library — will be drawn, then auto-discarded
        add_card_to_library(&mut state, PlayerId(0), "Forest", true);

        let ability = make_connive_ability(source, conniver);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // No counter (land discarded)
        let obj = state.objects.get(&conniver).unwrap();
        assert_eq!(
            obj.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0
        );
    }

    #[test]
    fn connive_empty_hand_after_draw_from_empty_library() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let conniver = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Conniver".to_string(),
            Zone::Battlefield,
        );

        // Empty library, empty hand — draw fails, no discard
        let ability = make_connive_ability(source, conniver);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should emit EffectResolved without panic
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Connive,
                ..
            }
        )));
    }

    #[test]
    fn connive_skips_counter_if_conniver_left_battlefield() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let conniver = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Conniver".to_string(),
            Zone::Graveyard, // Not on battlefield
        );

        add_card_to_library(&mut state, PlayerId(0), "Spell", false);

        let ability = make_connive_ability(source, conniver);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // No counter — conniver not on battlefield
        let obj = state.objects.get(&conniver).unwrap();
        assert_eq!(
            obj.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0
        );
    }
}
