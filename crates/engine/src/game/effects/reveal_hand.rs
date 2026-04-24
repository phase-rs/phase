use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// CR 701.20a: RevealHand — reveal target player's hand, then let the caster choose a card.
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
    let (card_filter, count) = match &ability.effect {
        Effect::RevealHand {
            card_filter, count, ..
        } => (card_filter.clone(), count.clone()),
        _ => (TargetFilter::Any, None),
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

    let full_hand: Vec<_> = state
        .players
        .iter()
        .find(|p| p.id == target_player)
        .map(|p| p.hand.iter().copied().collect())
        .unwrap_or_default();

    // CR 701.20a: If a count is specified, reveal only that many cards.
    let hand = if let Some(count_expr) = &count {
        let n = resolve_quantity_with_targets(state, count_expr, ability).max(0) as usize;
        full_hand.into_iter().take(n).collect()
    } else {
        full_hand
    };

    if hand.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Reveal,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // CR 701.20b: Revealing a card doesn't cause it to leave the zone it's in.
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
        card_ids: hand.clone(),
        card_names,
    });

    // Filter to only eligible cards for the choice (e.g. "nonland card").
    // CR 107.3a + CR 601.2b: ability-context evaluation for dynamic thresholds.
    let eligible: Vec<_> = if matches!(card_filter, TargetFilter::Any) {
        hand
    } else {
        let ctx = FilterContext::from_ability(ability);
        hand.into_iter()
            .filter(|&id| matches_target_filter(state, id, &card_filter, &ctx))
            .collect()
    };

    if eligible.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Reveal,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    state.waiting_for = WaitingFor::RevealChoice {
        player: ability.controller,
        cards: eligible,
        filter: card_filter,
        optional: false,
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
                count: None,
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
