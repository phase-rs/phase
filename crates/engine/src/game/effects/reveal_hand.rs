use rand::seq::SliceRandom;

use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ParentTargetMissingReason, ResolvedAbility, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// CR 701.20a / CR 701.20e: RevealHand — reveal or privately look at a target
/// player's hand, then optionally let the caster choose a card.
///
/// Public reveals (`reveal: true`) mark cards in `GameState.revealed_cards` so
/// `filter_state_for_viewer` shows them to all players. Private looks (`reveal:
/// false`) record `private_look_ids` / `private_look_player` so only the ability
/// controller can see the hand. When a post-reveal card choice is required,
/// `WaitingFor::RevealChoice` is opened for the caster.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (card_filter, count, random, choice_optional, is_reveal, target) = match &ability.effect {
        Effect::RevealHand {
            card_filter,
            count,
            selection,
            choice_optional,
            reveal,
            target,
            ..
        } => (
            card_filter.clone(),
            count.clone(),
            selection.is_random(),
            *choice_optional,
            *reveal,
            target.clone(),
        ),
        _ => (
            TargetFilter::Any,
            None,
            false,
            false,
            true,
            TargetFilter::Any,
        ),
    };

    // Find the target player from resolved targets. Targeted RevealHand
    // (Thoughtseize, Duress) builds a real `TargetRef::Player` slot — keep that
    // fast path unchanged (zero regression).
    let target_player = ability
        .targets
        .iter()
        .find_map(|t| match t {
            TargetRef::Player(pid) => Some(*pid),
            _ => None,
        })
        // CR 608.2d + CR 608.2c: "an opponent" is a choice the controller
        // announces while resolving the effect. For the as-enters look-at-hand
        // class (Anointed Peacekeeper, Sorcerous Spyglass), the parser's as-enters
        // composition (`parse_as_enters_choose` →
        // `front_opponent_choice_for_nontargeted_look`) now FRONTS an explicit
        // `Choose(Opponent)` step and rebinds this look's player filter to
        // `ControllerRef::ChosenPlayer { index: 0 }` (CR 608.2c "that player").
        // So by the time control reaches here, `collect_player_targets` resolves
        // that filter to exactly the single chosen opponent — a 1-element vec — and
        // `.first()` is EXACT, not a multiplayer simplification (CR 608.2d
        // satisfied by the fronted choice, not by picking the first opponent here).
        // Any residual non-fronted `Typed`/`Controller` opponent filter that still
        // reaches this arm (a hand-look shape outside the as-enters seam) falls
        // back to the first opponent, exact in two-player. `TargetFilter::Any`
        // (RevealAll/RevealPartial) hits `collect_player_targets`' empty arm → still
        // `MissingParam`, so those reveals are unchanged.
        .or_else(|| {
            crate::game::ability_utils::collect_player_targets(state, ability, &target)
                .first()
                .copied()
        })
        .ok_or(EffectError::MissingParam("target player".to_string()))?;

    let full_hand: Vec<_> = state
        .players
        .iter()
        .find(|p| p.id == target_player)
        .map(|p| p.hand.iter().copied().collect())
        .unwrap_or_default();

    let mut hand = full_hand;
    if random {
        hand.shuffle(&mut state.rng);
    }
    // CR 701.20a: If a count is specified, reveal only that many cards.
    if let Some(count_expr) = &count {
        let n = resolve_quantity_with_targets(state, count_expr, ability).max(0) as usize;
        hand.truncate(n);
    }

    if hand.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Reveal,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    // CR 701.20b: Revealing a card doesn't cause it to leave the zone it's in.
    if is_reveal {
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
    } else {
        // CR 701.20e: "Look at" privately shows the hand to the ability controller.
        state.private_look_ids = hand.clone();
        state.private_look_player = Some(ability.controller);
    }

    let needs_reveal_choice = choice_optional || !matches!(card_filter, TargetFilter::None);
    if !needs_reveal_choice {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Reveal,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

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
        if !is_reveal {
            state.private_look_ids.clear();
            state.private_look_player = None;
        }
        // CR 608.2c + issue #4950 (Thoughtseize): a choice WAS required
        // (`needs_reveal_choice`) but the eligible set came up empty — there
        // is nothing to choose. Signal this so an immediately-chained
        // `ParentTarget`-bound sub-ability (e.g. Thoughtseize's
        // `DiscardCard{target: ParentTarget}`) can resolve to a hard no-op
        // instead of falling back to a whole-hand action. Consumed by
        // `apply_parent_chain_context` at the very next parent->child
        // hand-off — see `GameState::last_parent_target_missing_reason`.
        state.last_parent_target_missing_reason = Some(ParentTargetMissingReason::RevealHandChoice);
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Reveal,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    state.waiting_for = WaitingFor::RevealChoice {
        player: ability.controller,
        cards: eligible,
        filter: card_filter,
        optional: choice_optional,
        decline_runs_continuation: false,
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Reveal,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::visibility::filter_state_for_viewer;
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
                selection: crate::types::ability::CardSelectionMode::Chosen,
                choice_optional: false,
                reveal: true,
            },
            vec![TargetRef::Player(target_player)],
            ObjectId(100),
            controller,
        )
    }

    /// CR 701.20e + CR 608.2d: A non-targeted "look at an opponent's hand"
    /// (Anointed Peacekeeper) carries a controller-scoped `Typed(Opponent)`
    /// target and an Object source slot (no explicit player target). The
    /// resolver must fall back to `collect_player_targets` and privately look at
    /// the OPPONENT's hand — never the controller's, even when both are
    /// non-empty (multi-authority hostile fixture). Reverting the `.or_else`
    /// fallback makes this return `MissingParam` and the assertions below fail.
    #[test]
    fn look_at_an_opponents_hand_falls_back_to_opponent_controller_choice() {
        use crate::types::ability::{ControllerRef, TypedFilter};

        let mut state = GameState::new_two_player(42);
        // Controller (PlayerId(0)) also holds a card — must NOT be looked at.
        let own_card = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "My Secret".to_string(),
            Zone::Hand,
        );
        let opp_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Their Secret".to_string(),
            Zone::Hand,
        );

        let ability = ResolvedAbility::new(
            Effect::RevealHand {
                target: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
                card_filter: TargetFilter::None,
                count: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                choice_optional: false,
                reveal: false,
            },
            // Source object slot only — no explicit player target.
            vec![TargetRef::Object(ObjectId(100))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).expect("opponent-hand look should resolve");

        assert_eq!(
            state.private_look_ids,
            vec![opp_card],
            "must privately look at the opponent's hand"
        );
        assert!(
            !state.private_look_ids.contains(&own_card),
            "must never look at the controller's own hand"
        );
        assert_eq!(
            state.private_look_player,
            Some(PlayerId(0)),
            "the looker is the ability controller"
        );
    }

    /// An explicit `TargetRef::Player` slot (Thoughtseize-style) still wins the
    /// fast path even when the effect also carries a `Typed(Opponent)` filter
    /// that would resolve to a different player — zero regression for targeted
    /// RevealHand.
    #[test]
    fn explicit_player_target_wins_over_controller_filter() {
        use crate::types::ability::{ControllerRef, TypedFilter};

        let mut state = GameState::new_two_player(42);
        let controller_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Controller Card".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Card".to_string(),
            Zone::Hand,
        );

        let ability = ResolvedAbility::new(
            Effect::RevealHand {
                // Filter would resolve to the opponent (PlayerId(1))…
                target: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
                card_filter: TargetFilter::None,
                count: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                choice_optional: false,
                reveal: false,
            },
            // …but the explicit player target is the controller itself.
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.private_look_ids,
            vec![controller_card],
            "explicit player target must win over the filter fallback"
        );
    }

    /// R2: `TargetFilter::Any` (RevealAll/RevealPartial shapes) with no explicit
    /// player target hits `collect_player_targets`' empty arm, so the resolver
    /// still errors `MissingParam` — the fallback is strictly additive.
    #[test]
    fn any_target_without_player_slot_still_missing_param() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::RevealHand {
                target: TargetFilter::Any,
                card_filter: TargetFilter::Any,
                count: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                choice_optional: false,
                reveal: true,
            },
            vec![TargetRef::Object(ObjectId(100))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(
            matches!(result, Err(EffectError::MissingParam(_))),
            "Any-target reveal without a player slot must still be MissingParam, got {result:?}"
        );
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

    #[test]
    fn random_count_reveal_limits_choice_to_one_card() {
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
        let card3 = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Island".to_string(),
            Zone::Hand,
        );

        let ability = ResolvedAbility::new(
            Effect::RevealHand {
                target: TargetFilter::Any,
                card_filter: TargetFilter::Any,
                count: Some(crate::types::ability::QuantityExpr::Fixed { value: 1 }),
                selection: crate::types::ability::CardSelectionMode::Random,
                choice_optional: false,
                reveal: true,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::RevealChoice { cards, .. } => {
                assert_eq!(cards.len(), 1);
                assert!([card1, card2, card3].contains(&cards[0]));
            }
            other => panic!("Expected RevealChoice, got {:?}", other),
        }
        assert_eq!(state.revealed_cards.len(), 1);
    }

    /// CR 701.20a + CR 107.1: A compound `Offset` reveal count (Klaw — "one plus
    /// the number of creature cards in your graveyard") resolves to inner+1 and
    /// truncates the revealed set to exactly that many cards. The controller
    /// (PlayerId(0)) has 2 creature cards in graveyard, so the count resolves to
    /// 2 + 1 = 3; the 5-card hand must be truncated to 3 revealed cards. Before
    /// the parser fix this count was dropped (None) at synthesis, so the whole
    /// hand would have been revealed — this asserts the Offset both resolves and
    /// is consumed by the resolver.
    #[test]
    fn reveal_hand_offset_count_truncates_to_inner_plus_one() {
        use crate::types::ability::{CountScope, QuantityExpr, QuantityRef, TypeFilter, ZoneRef};
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);

        // Controller (PlayerId(0)) graveyard: 2 creature cards.
        for cid in 10..12 {
            let gy = create_object(
                &mut state,
                CardId(cid),
                PlayerId(0),
                "Dead Bear".to_string(),
                Zone::Graveyard,
            );
            state
                .objects
                .get_mut(&gy)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        // Target (PlayerId(1)) hand: 5 cards (M > N+1 = 3).
        for cid in 1..6 {
            create_object(
                &mut state,
                CardId(cid),
                PlayerId(1),
                format!("Card {cid}"),
                Zone::Hand,
            );
        }

        let ability = ResolvedAbility::new(
            Effect::RevealHand {
                target: TargetFilter::Player,
                card_filter: TargetFilter::Any,
                count: Some(QuantityExpr::Offset {
                    offset: 1,
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::ZoneCardCount {
                            zone: ZoneRef::Graveyard,
                            card_types: vec![TypeFilter::Creature],
                            filter: None,
                            scope: CountScope::Controller,
                        },
                    }),
                }),
                selection: crate::types::ability::CardSelectionMode::Chosen,
                choice_optional: false,
                reveal: true,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // 2 creatures in graveyard + 1 = 3 cards revealed (not the full 5).
        assert_eq!(
            state.revealed_cards.len(),
            3,
            "Offset count must resolve to inner(2) + 1 = 3 revealed cards"
        );
        match &state.waiting_for {
            WaitingFor::RevealChoice { cards, .. } => {
                assert_eq!(
                    cards.len(),
                    3,
                    "choice set is limited to the 3 revealed cards"
                );
            }
            other => panic!("Expected RevealChoice, got {other:?}"),
        }
    }

    #[test]
    fn look_at_hand_is_private_to_looker_and_skips_reveal_choice() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Secret Bolt".to_string(),
            Zone::Hand,
        );

        let ability = ResolvedAbility::new(
            Effect::RevealHand {
                target: TargetFilter::Player,
                card_filter: TargetFilter::None,
                count: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                choice_optional: false,
                reveal: false,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !state.revealed_cards.contains(&card1),
            "private look must not publish cards via revealed_cards"
        );
        assert_eq!(state.private_look_ids, vec![card1]);
        assert_eq!(state.private_look_player, Some(PlayerId(0)));
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::CardsRevealed { .. })),
            "private look must not emit CardsRevealed"
        );

        let looker_view = filter_state_for_viewer(&state, PlayerId(0));
        assert_eq!(
            looker_view.objects[&card1].name, "Secret Bolt",
            "looker must see the privately looked-at hand card"
        );

        let other_player_view = filter_state_for_viewer(&state, PlayerId(1));
        assert_eq!(
            other_player_view.objects[&card1].name, "Secret Bolt",
            "hand owner always sees their own hand"
        );
    }

    #[test]
    fn look_at_own_hand_does_not_leak_to_opponent() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Probe Secret".to_string(),
            Zone::Hand,
        );

        let ability = ResolvedAbility::new(
            Effect::RevealHand {
                target: TargetFilter::Player,
                card_filter: TargetFilter::None,
                count: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                choice_optional: false,
                reveal: false,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(1),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let opponent_view = filter_state_for_viewer(&state, PlayerId(0));
        assert_eq!(
            opponent_view.objects[&card1].name, "Hidden Card",
            "Gitaxian Probe self-target look must not leak hand contents to the opponent"
        );
    }

    #[test]
    fn optional_reveal_hand_choice_decline_skips_continuation() {
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::types::ability::{AbilityKind, QuantityExpr};
        use crate::types::actions::GameAction;
        use crate::types::game_state::PendingContinuation;

        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Duress Target".to_string(),
            Zone::Hand,
        );
        state.revealed_cards.insert(card);
        state.waiting_for = WaitingFor::RevealChoice {
            player: PlayerId(0),
            cards: vec![card],
            filter: TargetFilter::Any,
            optional: true,
            decline_runs_continuation: false,
        };
        let mut continuation = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        continuation.kind = AbilityKind::Spell;
        state.pending_continuation = Some(PendingContinuation::new(Box::new(continuation), &state));

        let mut events = Vec::new();
        handle_resolution_choice(
            &mut state,
            WaitingFor::RevealChoice {
                player: PlayerId(0),
                cards: vec![card],
                filter: TargetFilter::Any,
                optional: true,
                decline_runs_continuation: false,
            },
            GameAction::SelectCards { cards: vec![] },
            &mut events,
        )
        .expect("optional reveal-hand choice decline should succeed");

        assert_eq!(state.players[0].life, 20);
        assert!(
            state.pending_continuation.is_none(),
            "declining the optional card choice should skip the follow-up"
        );
        assert!(!state.revealed_cards.contains(&card));
    }
}
