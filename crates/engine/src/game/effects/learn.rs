use crate::types::ability::{
    Effect, EffectError, EffectKind, OutsideGameSourcePool, QuantityExpr, ResolvedAbility,
    TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 701.48a: Learn — "You may discard a card. If you do, draw a card.
/// If you didn't discard a card, you may reveal a Lesson card you own from
/// outside the game and put it into your hand."
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    debug_assert!(matches!(ability.effect, Effect::Learn));

    let player = ability.controller;

    let hand_cards: Vec<ObjectId> = state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.iter().copied().collect())
        .unwrap_or_default();

    if hand_cards.is_empty() {
        // CR 701.48a: with no card available to discard, the "if you didn't
        // discard a card" branch applies unconditionally — offer the Lesson
        // search from outside the game.
        let lesson_search = lesson_search_ability(ability.source_id, player);
        let _ = super::resolve_ability_chain(state, &lesson_search, events, 0);
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Learn,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    // Present the choice: rummage one card or skip.
    state.waiting_for = WaitingFor::LearnChoice { player, hand_cards };
    Ok(())
}

/// CR 701.48a: "reveal a Lesson card you own from outside the game and put it
/// into your hand" — the "if you didn't discard a card" branch, built as a
/// `SearchOutsideGame` ability so it reuses the same sideboard-access
/// machinery as Wish-class effects.
pub(crate) fn lesson_search_ability(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::SearchOutsideGame {
            filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype(
                "Lesson".to_string(),
            ))),
            count: QuantityExpr::up_to(QuantityExpr::Fixed { value: 1 }),
            reveal: true,
            destination: Zone::Hand,
            source_pool: OutsideGameSourcePool::Sideboard,
        },
        vec![],
        source_id,
        controller,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_learn_ability(source: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(Effect::Learn, vec![], source, PlayerId(0))
    }

    #[test]
    fn learn_with_empty_hand_auto_skips() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );

        let ability = make_learn_ability(source);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Learn,
                ..
            }
        )));
    }

    #[test]
    fn learn_with_cards_sets_waiting_for() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Hand Card".to_string(),
            Zone::Hand,
        );

        let ability = make_learn_ability(source);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::LearnChoice { player, hand_cards } => {
                assert_eq!(*player, PlayerId(0));
                assert!(hand_cards.contains(&card));
            }
            other => panic!("Expected LearnChoice, got {:?}", other),
        }
    }
}
