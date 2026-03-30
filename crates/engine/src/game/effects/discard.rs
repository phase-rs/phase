use std::collections::HashSet;

use crate::game::quantity::resolve_quantity;
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
    let (num_cards, unless_filter) = match &ability.effect {
        Effect::DiscardCard { count, .. } => (*count, None),
        Effect::Discard {
            count,
            unless_filter,
            ..
        } => (
            resolve_quantity(state, count, ability.controller, ability.source_id) as u32,
            unless_filter.clone(),
        ),
        _ => (1, None),
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
            .to_vec();

        let count = (num_cards as usize).min(hand_cards.len());
        if count == 0 {
            // Nothing to discard — skip.
        } else if hand_cards.len() <= count {
            // Forced discard — no choice needed, discard all eligible cards.
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
        } else {
            // CR 701.9b: Player chooses — present interactive selection.
            state.waiting_for = crate::types::game_state::WaitingFor::DiscardChoice {
                player: discard_player,
                count,
                cards: hand_cards,
                source_id: ability.source_id,
                effect_kind: EffectKind::from(&ability.effect),
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
    use crate::types::ability::TargetFilter;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

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
}
