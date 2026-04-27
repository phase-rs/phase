use std::collections::HashSet;

use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// Outcome of a discard attempt routed through the replacement pipeline.
pub(crate) enum DiscardOutcome {
    /// Discard completed (normally or via replacement redirect).
    Complete,
    /// A replacement effect requires player choice before discard can proceed.
    /// Callers must handle this by surfacing the replacement choice to the player.
    NeedsReplacementChoice(PlayerId),
}

/// CR 701.9a: To discard a card, move it from owner's hand to their graveyard.
/// If targets specify specific cards, discard those; otherwise discard from end of hand.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 701.9b + CR 608.2d: Peel `UpTo` from the count expression to derive
    // the upper-bound expression and the may-pick-fewer flag. Plain
    // `QuantityExpr` means a mandatory count; wrapped in `UpTo` means the
    // player may discard 0..=count.
    let (num_cards, up_to, unless_filter) = match &ability.effect {
        Effect::DiscardCard { count, .. } => (*count, false, None),
        Effect::Discard {
            count,
            unless_filter,
            ..
        } => {
            let (inner, up_to) = count.peel_up_to();
            (
                // CR 107.1b: Use ability context so X resolves against the caster's chosen value.
                resolve_quantity_with_targets(state, inner, ability) as u32,
                up_to,
                unless_filter.clone(),
            )
        }
        _ => (1, false, None),
    };

    // Check if targets specify specific cards to discard
    let specific_targets: Vec<_> = ability
        .targets
        .iter()
        .filter_map(|t| {
            if let TargetRef::Object(obj_id) = t {
                Some(*obj_id)
            } else {
                None
            }
        })
        .collect();

    if !specific_targets.is_empty() {
        // Discard specific targeted cards
        for obj_id in specific_targets {
            let obj = state
                .objects
                .get(&obj_id)
                .ok_or(EffectError::ObjectNotFound(obj_id))?;
            if obj.zone != Zone::Hand {
                continue;
            }
            let player_id = obj.owner;

            let proposed = ProposedEvent::Discard {
                player_id,
                object_id: obj_id,
                applied: HashSet::new(),
            };

            match replacement::replace_event(state, proposed, events) {
                ReplacementResult::Execute(event) => {
                    match event {
                        ProposedEvent::Discard {
                            player_id: pid,
                            object_id: oid,
                            ..
                        } => {
                            zones::move_to_zone(state, oid, Zone::Graveyard, events);
                            crate::game::restrictions::record_discard(state, pid);
                            events.push(GameEvent::Discarded {
                                player_id: pid,
                                object_id: oid,
                            });
                        }
                        ProposedEvent::ZoneChange {
                            object_id: oid, to, ..
                        } => {
                            // Replacement redirected (e.g., Madness → exile instead of graveyard).
                            zones::move_to_zone(state, oid, to, events);
                            // CR 702.35: The card was still discarded — record and emit event
                            // so "whenever you discard" triggers fire.
                            crate::game::restrictions::record_discard(state, player_id);
                            events.push(GameEvent::Discarded {
                                player_id,
                                object_id: oid,
                            });
                        }
                        _ => {}
                    }
                }
                ReplacementResult::Prevented => {}
                ReplacementResult::NeedsChoice(player) => {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    return Ok(());
                }
            }
        }
    } else {
        // CR 701.9a: Find discard player — first TargetRef::Player, or default to controller.
        let discard_player = ability.target_player();

        // CR 701.9b: Player chooses which card(s) to discard (not "at random").
        let hand_cards: Vec<ObjectId> = state
            .players
            .iter()
            .find(|p| p.id == discard_player)
            .ok_or(EffectError::PlayerNotFound)?
            .hand
            .iter()
            .copied()
            .collect();

        // CR 701.9b: For "up to N" discards, present the full N to the player.
        // The available cards list naturally constrains actual selection.
        let count = if up_to {
            num_cards as usize
        } else {
            (num_cards as usize).min(hand_cards.len())
        };
        if count == 0 && !up_to {
            // CR 608.2c: Effect resolved as no-op (empty hand) — veto downstream IfYouDo.
            state.cost_payment_failed_flag = true;
        } else if hand_cards.is_empty() {
            // up_to=true with empty hand — choosing 0 is the only option, skip interaction.
        } else if !up_to && hand_cards.len() <= count {
            // Forced discard — no choice needed, discard all eligible cards.
            // When up_to=true, always present the choice (player may discard fewer).
            for obj_id in &hand_cards {
                if let DiscardOutcome::NeedsReplacementChoice(player) =
                    discard_as_cost(state, *obj_id, discard_player, events)
                {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    // Known limitation: EffectResolved is not emitted when replacement
                    // choice interrupts forced-discard (same systemic gap as sacrifice).
                    return Ok(());
                }
            }
        } else if count > 0 || up_to {
            // CR 701.9b: Player chooses — present interactive selection.
            state.waiting_for = crate::types::game_state::WaitingFor::DiscardChoice {
                player: discard_player,
                count,
                cards: hand_cards,
                source_id: ability.source_id,
                effect_kind: EffectKind::from(&ability.effect),
                up_to,
                unless_filter,
            };
            // EffectResolved is emitted by the engine handler after the player chooses.
            return Ok(());
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 207.2c + CR 118.12a: Discard a card as part of an ability cost (Channel).
/// Routes through the replacement pipeline so Madness (CR 702.35) etc. can intercept.
pub(crate) fn discard_as_cost(
    state: &mut GameState,
    object_id: ObjectId,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> DiscardOutcome {
    let proposed = ProposedEvent::Discard {
        player_id: player,
        object_id,
        applied: HashSet::new(),
    };
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => match event {
            ProposedEvent::Discard {
                player_id: pid,
                object_id: oid,
                ..
            } => {
                zones::move_to_zone(state, oid, Zone::Graveyard, events);
                crate::game::restrictions::record_discard(state, pid);
                events.push(GameEvent::Discarded {
                    player_id: pid,
                    object_id: oid,
                });
            }
            ProposedEvent::ZoneChange {
                object_id: oid, to, ..
            } => {
                // CR 614.1c: Replacement redirected destination (e.g., Madness → exile).
                // CR 702.35: The card was still discarded — record and emit event
                // so "whenever you discard" triggers fire.
                zones::move_to_zone(state, oid, to, events);
                crate::game::restrictions::record_discard(state, player);
                events.push(GameEvent::Discarded {
                    player_id: player,
                    object_id: oid,
                });
            }
            _ => {}
        },
        ReplacementResult::Prevented => {
            // CR 614.1a: If the discard is prevented, the cost was not fully paid.
            // This is extremely rare during cost payment. The card stays in hand.
        }
        ReplacementResult::NeedsChoice(choice_player) => {
            return DiscardOutcome::NeedsReplacementChoice(choice_player);
        }
    }
    DiscardOutcome::Complete
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ReplacementDefinition, TargetFilter,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;

    #[test]
    fn discard_moves_card_from_hand_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );
        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.players[0].hand.contains(&card));
        assert!(state.players[0].graveyard.contains(&card));
    }

    #[test]
    fn discard_specific_target() {
        let mut state = GameState::new_two_player(42);
        let c1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Keep".to_string(),
            Zone::Hand,
        );
        let c2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Discard".to_string(),
            Zone::Hand,
        );
        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(c2)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[0].hand.contains(&c1));
        assert!(!state.players[0].hand.contains(&c2));
    }

    #[test]
    fn discard_replacement_can_exile_card_and_still_emit_discarded() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Madness Spell".to_string(),
            Zone::Hand,
        );
        let mut replacement = ReplacementDefinition::new(ReplacementEvent::Discard);
        replacement.valid_card = Some(TargetFilter::SelfRef);
        replacement.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
        )));
        state
            .objects
            .get_mut(&card)
            .unwrap()
            .replacement_definitions
            .push(replacement);

        let mut events = Vec::new();
        let outcome = discard_as_cost(&mut state, card, PlayerId(0), &mut events);

        assert!(matches!(outcome, DiscardOutcome::Complete));
        assert!(state.exile.contains(&card));
        assert!(!state.players[0].graveyard.contains(&card));
        assert!(events.iter().any(
            |event| matches!(event, GameEvent::Discarded { object_id, .. } if *object_id == card)
        ));
    }

    #[test]
    fn discard_emits_discarded_event() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );
        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::Discarded { object_id, .. } if *object_id == card)));
    }

    #[test]
    fn discard_as_cost_moves_to_graveyard_and_records() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Channel Card".to_string(),
            Zone::Hand,
        );
        let mut events = Vec::new();

        discard_as_cost(&mut state, card, PlayerId(0), &mut events);

        // Card moved hand → graveyard
        assert!(!state.players[0].hand.contains(&card));
        assert!(state.players[0].graveyard.contains(&card));
        // Discarded event emitted
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::Discarded { object_id, .. } if *object_id == card)));
        // Restriction tracking updated
        assert!(state
            .players_who_discarded_card_this_turn
            .contains(&PlayerId(0)));
    }

    #[test]
    fn non_targeted_discard_creates_waiting_for() {
        use crate::types::ability::QuantityExpr;
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        let c1 = create_object(&mut state, CardId(1), PlayerId(0), "A".into(), Zone::Hand);
        let c2 = create_object(&mut state, CardId(2), PlayerId(0), "B".into(), Zone::Hand);
        let c3 = create_object(&mut state, CardId(3), PlayerId(0), "C".into(), Zone::Hand);

        let ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                random: false,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DiscardChoice {
                player,
                count,
                cards,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 1);
                assert!(cards.contains(&c1));
                assert!(cards.contains(&c2));
                assert!(cards.contains(&c3));
            }
            other => panic!("Expected DiscardChoice, got {:?}", other),
        }
    }

    #[test]
    fn non_targeted_discard_auto_when_hand_equals_count() {
        use crate::types::ability::QuantityExpr;
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        let c1 = create_object(&mut state, CardId(1), PlayerId(0), "A".into(), Zone::Hand);
        let c2 = create_object(&mut state, CardId(2), PlayerId(0), "B".into(), Zone::Hand);

        let ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                random: false,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Should auto-discard without WaitingFor
        assert!(
            !matches!(state.waiting_for, WaitingFor::DiscardChoice { .. }),
            "Should not create DiscardChoice when hand == count"
        );
        assert!(!state.players[0].hand.contains(&c1));
        assert!(!state.players[0].hand.contains(&c2));
    }

    #[test]
    fn non_targeted_discard_noop_when_hand_empty() {
        use crate::types::ability::QuantityExpr;
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        // No cards in hand

        let ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                random: false,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !matches!(state.waiting_for, WaitingFor::DiscardChoice { .. }),
            "Should not create DiscardChoice when hand is empty"
        );
    }

    #[test]
    fn non_targeted_discard_multiple_creates_waiting_for() {
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        // Create 5 cards in hand
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Hand,
            );
        }
        assert_eq!(state.players[0].hand.len(), 5);

        // Non-targeted discard of 2 → interactive choice
        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 2,
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DiscardChoice {
                player,
                count,
                cards,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 2);
                assert_eq!(cards.len(), 5);
            }
            other => panic!("Expected DiscardChoice, got {:?}", other),
        }
        // Hand unchanged until player selects
        assert_eq!(state.players[0].hand.len(), 5);
    }

    #[test]
    fn opponent_discard_targets_opponent_hand() {
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        // Give player 1 (opponent) 3 cards
        let _c1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opp A".into(),
            Zone::Hand,
        );
        let _c2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opp B".into(),
            Zone::Hand,
        );
        let _c3 = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opp C".into(),
            Zone::Hand,
        );
        // Give player 0 (controller) 1 card
        create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Mine".into(),
            Zone::Hand,
        );

        // "Target opponent discards a card" — controller is P0, target is P1
        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Opponent (P1) should see the discard choice, not controller (P0)
        match &state.waiting_for {
            WaitingFor::DiscardChoice {
                player,
                count,
                cards,
                ..
            } => {
                assert_eq!(*player, PlayerId(1), "Opponent should make the choice");
                assert_eq!(*count, 1);
                assert_eq!(
                    cards.len(),
                    3,
                    "Should show opponent's 3 cards, not controller's 1"
                );
            }
            other => panic!("Expected DiscardChoice, got {:?}", other),
        }
    }

    #[test]
    fn opponent_discard_auto_when_one_card() {
        let mut state = GameState::new_two_player(42);
        // Opponent has exactly 1 card — should auto-discard without choice
        let opp_card = create_object(&mut state, CardId(1), PlayerId(1), "Opp".into(), Zone::Hand);
        // Controller has cards too (should not be affected)
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mine".into(),
            Zone::Hand,
        );

        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Opponent's card should be discarded
        assert!(!state.players[1].hand.contains(&opp_card));
        assert!(state.players[1].graveyard.contains(&opp_card));
        // Controller's hand unchanged
        assert_eq!(state.players[0].hand.len(), 1);
    }

    #[test]
    fn target_player_defaults_to_controller() {
        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        assert_eq!(ability.target_player(), PlayerId(0));
    }

    #[test]
    fn target_player_extracts_from_mixed_targets() {
        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            vec![
                TargetRef::Object(ObjectId(50)),
                TargetRef::Player(PlayerId(1)),
            ],
            ObjectId(100),
            PlayerId(0),
        );
        assert_eq!(ability.target_player(), PlayerId(1));
    }

    #[test]
    fn discard_as_cost_returns_complete() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );
        let mut events = Vec::new();

        let outcome = discard_as_cost(&mut state, card, PlayerId(0), &mut events);

        assert!(matches!(outcome, DiscardOutcome::Complete));
        assert!(!state.players[0].hand.contains(&card));
        assert!(state.players[0].graveyard.contains(&card));
    }

    #[test]
    fn up_to_discard_presents_choice_even_when_hand_small() {
        use crate::types::ability::QuantityExpr;
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        // Only 1 card in hand, but "discard up to 2" should still present a choice
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Hand,
        );

        let ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::up_to(QuantityExpr::Fixed { value: 2 }),
                target: TargetFilter::Any,
                random: false,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.9b: up_to=true must present choice even when hand ≤ count
        match &state.waiting_for {
            WaitingFor::DiscardChoice {
                up_to,
                count,
                cards,
                ..
            } => {
                assert!(*up_to);
                // CR 701.9b: up_to presents uncapped count (2), not min(2, hand=1)
                assert_eq!(*count, 2);
                assert_eq!(cards.len(), 1);
            }
            other => panic!(
                "Expected DiscardChoice with up_to, got {:?}",
                std::mem::discriminant(other)
            ),
        }
    }

    #[test]
    fn up_to_discard_allows_zero_selection() {
        use crate::game::engine::apply_as_current;
        use crate::types::actions::GameAction;
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("Card {i}"),
                Zone::Hand,
            );
        }

        // Set up a DiscardChoice with up_to=true
        state.waiting_for = WaitingFor::DiscardChoice {
            player: PlayerId(0),
            count: 2,
            cards: state.players[0].hand.iter().copied().collect::<Vec<_>>(),
            source_id: ObjectId(100),
            effect_kind: crate::types::ability::EffectKind::Discard,
            up_to: true,
            unless_filter: None,
        };

        // Select zero cards — should succeed with up_to=true
        let result = apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] });
        assert!(
            result.is_ok(),
            "Zero selection should succeed for up_to discard"
        );
    }

    #[test]
    fn empty_hand_discard_sets_cost_payment_failed_flag() {
        use crate::types::ability::QuantityExpr;

        let mut state = GameState::new_two_player(42);
        // No cards in hand — discard should set veto flag

        let ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                random: false,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 608.2c: No-op discard vetoes downstream IfYouDo conditions
        assert!(
            state.cost_payment_failed_flag,
            "cost_payment_failed_flag should be set when discard count is 0 (empty hand)"
        );
    }

    #[test]
    fn empty_hand_up_to_discard_does_not_set_failed_flag() {
        use crate::types::ability::QuantityExpr;

        let mut state = GameState::new_two_player(42);
        // No cards in hand, but up_to=true — choosing 0 is valid success

        let ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::up_to(QuantityExpr::Fixed { value: 2 }),
                target: TargetFilter::Any,
                random: false,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // up_to=true with empty hand is not a failure — it's a valid 0 selection
        assert!(
            !state.cost_payment_failed_flag,
            "cost_payment_failed_flag should NOT be set for up_to discard with empty hand"
        );
    }
}
