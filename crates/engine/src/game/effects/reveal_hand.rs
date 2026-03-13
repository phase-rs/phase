use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// RevealHand: reveal target player's hand, then let the caster choose a card.
///
/// Marks all cards in the target player's hand as revealed in `GameState.revealed_cards`
/// (so `filter_state_for_player` doesn't hide them), emits `CardsRevealed`, and sets
/// `WaitingFor::RevealChoice` for the caster to select a card matching the filter.
/// The sub-ability chain (exile, discard, etc.) runs via `pending_continuation`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let card_filter = match &ability.effect {
        Effect::RevealHand { card_filter, .. } => card_filter.clone(),
        _ => TargetFilter::Any,
    };

    // Find the target player from resolved targets
    let target_player = ability
        .targets
        .iter()
        .find_map(|t| match t {
            TargetRef::Player(pid) => Some(*pid),
            _ => None,
        })
        .ok_or(EffectError::MissingParam("target player".to_string()))?;

    let hand: Vec<_> = state
        .players
        .iter()
        .find(|p| p.id == target_player)
        .map(|p| p.hand.clone())
        .unwrap_or_default();

    if hand.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Reveal,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // Mark all hand cards as revealed
    for &card_id in &hand {
        state.revealed_cards.insert(card_id);
    }

    // Emit event with card names
    let card_names: Vec<String> = hand
        .iter()
        .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
        .collect();
    events.push(GameEvent::CardsRevealed {
        player: target_player,
        card_names,
    });

    state.waiting_for = WaitingFor::RevealChoice {
        player: ability.controller,
        cards: hand,
        filter: card_filter,
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Reveal,
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
    use crate::types::zones::Zone;

    fn make_reveal_ability(controller: PlayerId, target_player: PlayerId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::RevealHand {
                target: TargetFilter::Any,
                card_filter: TargetFilter::Any,
            },
            vec![TargetRef::Player(target_player)],
            ObjectId(100),
            controller,
        )
    }

    #[test]
    fn reveal_hand_sets_reveal_choice_with_opponent_hand() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bolt".to_string(),
            Zone::Hand,
        );
        let card2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Hand,
        );

        let ability = make_reveal_ability(PlayerId(0), PlayerId(1));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::RevealChoice { player, cards, .. } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards.len(), 2);
                assert!(cards.contains(&card1));
                assert!(cards.contains(&card2));
            }
            other => panic!("Expected RevealChoice, got {:?}", other),
        }
    }

    #[test]
    fn reveal_hand_marks_cards_as_revealed() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bolt".to_string(),
            Zone::Hand,
        );

        let ability = make_reveal_ability(PlayerId(0), PlayerId(1));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.revealed_cards.contains(&card1));
    }

    #[test]
    fn reveal_hand_emits_cards_revealed_event() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bolt".to_string(),
            Zone::Hand,
        );

        let ability = make_reveal_ability(PlayerId(0), PlayerId(1));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::CardsRevealed { .. })));
    }

    #[test]
    fn reveal_empty_hand_does_nothing() {
        let mut state = GameState::new_two_player(42);
        // Player 1 has no cards in hand

        let ability = make_reveal_ability(PlayerId(0), PlayerId(1));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should not set RevealChoice — no cards to choose from
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }
}
