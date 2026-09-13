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
            // CR 701.48a: Learn was templated in 2021, long after the M10 zone
            // change, so "outside the game" means the sideboard in EVERY
            // format — including one that reverts the pre-M10 Wish boundary.
            // It shares the pool with the Wish cycle but not its era, which is
            // exactly why the era is carried rather than read off the pool.
            reach: crate::types::ability::OutsideGameReach::CurrentRules,
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

    /// CR 701.48a + CR 400.11: **Learn keeps the modern boundary even in a
    /// format that reverts it for the Wish cycle.**
    ///
    /// Learn declares `OutsideGameSourcePool::Sideboard`, exactly as a Wish
    /// does, so widening by pool alone would let a 2021 mechanic pull an owned
    /// face-up exiled Lesson — a reach the pre-M10 rule never gave it, for a
    /// card that did not exist under that rule. `OutsideGameReach` is what
    /// keeps them apart, and this is the regression that says so.
    #[test]
    fn a_legacy_wish_format_does_not_widen_learn_to_exile() {
        let mut state = GameState::new_two_player(42);
        state.format_config = crate::types::format::FormatConfig::for_custom_rules(
            &crate::types::custom_format::test_rules_with_legacy(
                crate::types::custom_format::LegacyRuleSet {
                    wish_scope:
                        crate::types::custom_format::WishOutsideGameScope::PreM10ReachesExile,
                    ..crate::types::custom_format::LegacyRuleSet::default()
                },
            ),
        );

        // An owned, face-up, matching Lesson sitting in exile: everything the
        // widened pool would offer, if the widening applied here.
        let lesson = create_object(
            &mut state,
            CardId(7),
            PlayerId(0),
            "Environmental Sciences".to_string(),
            Zone::Exile,
        );
        if let Some(obj) = state.objects.get_mut(&lesson) {
            obj.card_types = crate::types::card_type::CardType {
                core_types: vec![crate::types::card_type::CoreType::Sorcery],
                subtypes: vec!["Lesson".to_string()],
                ..Default::default()
            };
            obj.face_down = false;
        }
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );

        let ability = lesson_search_ability(source, PlayerId(0));
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let offered_exile = match &state.waiting_for {
            WaitingFor::OutsideGameChoice { choices, .. } => choices.iter().any(|choice| {
                matches!(
                    &choice.source,
                    crate::types::game_state::OutsideGameChoiceSource::FaceUpExile { object_id }
                        if *object_id == lesson
                )
            }),
            // No sideboard is registered, so with exile correctly excluded the
            // pool is empty and no choice is raised at all.
            _ => false,
        };
        assert!(
            !offered_exile,
            "Learn is post-M10 templating: the pre-M10 Wish scope must not reach \
             its exile, got {:?}",
            state.waiting_for
        );

        // Paired control on the SAME state: a Wish-class search over the same
        // exiled card IS widened, so the assertion above is about Learn's era
        // and not about the format failing to declare the scope.
        let wish = ResolvedAbility::new(
            Effect::SearchOutsideGame {
                filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype(
                    "Lesson".to_string(),
                ))),
                count: QuantityExpr::up_to(QuantityExpr::Fixed { value: 1 }),
                reveal: true,
                destination: Zone::Hand,
                source_pool: OutsideGameSourcePool::Sideboard,
                reach: crate::types::ability::OutsideGameReach::WishCycle,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &wish, &mut events, 0).unwrap();
        assert!(
            matches!(
                &state.waiting_for,
                WaitingFor::OutsideGameChoice { choices, .. }
                    if choices.iter().any(|c| matches!(
                        &c.source,
                        crate::types::game_state::OutsideGameChoiceSource::FaceUpExile { object_id }
                            if *object_id == lesson
                    ))
            ),
            "a Wish-class search in this same format must reach the exiled card, \
             got {:?}",
            state.waiting_for
        );
    }
}
