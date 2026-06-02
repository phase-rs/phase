use rand::Rng;

use crate::game::quantity::resolve_quantity;
use crate::types::ability::{DieRollModifier, Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

use super::resolve_ability_chain;

/// CR 706: Roll a die and execute the matching result branch.
///
/// CR 706.2: The natural roll is taken from a uniform 1..=sides distribution
/// using the game's seeded RNG; the (optional) modifier is then applied to
/// produce the *actual* result, which is what result-table branches consult
/// and what downstream effects ("where X is the result") snapshot via
/// `GameEvent::DieRolled.result`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (sides, results, modifier) = match &ability.effect {
        Effect::RollDie {
            sides,
            results,
            modifier,
        } => (*sides, results, modifier.as_ref()),
        _ => return Err(EffectError::MissingParam("RollDie".to_string())),
    };

    // CR 706.2: Roll the die using the game's seeded RNG. This is the
    // "natural result" before any modifiers.
    let natural = state.rng.random_range(1..=sides);

    // CR 706.2: Apply the (optional) modifier to produce the actual result.
    // The result is clamped to a u8-representable non-negative integer so a
    // large subtract doesn't wrap; branches with `min`/`max` already in u8
    // simply won't match when the actual result is 0.
    let actual = if let Some(m) = modifier {
        // Carry the sign as the saturating operation rather than negating the
        // resolved delta: `-resolve_quantity(..)` would panic in debug builds
        // (and wrap in release) when the quantity resolves to `i32::MIN`.
        let combined =
            match m {
                DieRollModifier::Add { value } => (natural as i32).saturating_add(
                    resolve_quantity(state, value, ability.controller, ability.source_id),
                ),
                DieRollModifier::Subtract { value } => (natural as i32).saturating_sub(
                    resolve_quantity(state, value, ability.controller, ability.source_id),
                ),
            };
        combined.clamp(0, u8::MAX as i32) as u8
    } else {
        natural
    };

    events.push(GameEvent::DieRolled {
        player_id: ability.controller,
        sides,
        result: actual,
    });

    // CR 706.2: The stored value is the actual die-roll result.
    // CR 706.4: Record the actual result so an inline "equal to the
    // result" sub_ability (no results table) reads the roll via
    // QuantityRef::EventContextAmount instead of the triggering event's amount.
    // Deliberately NOT cleared at this function's exit: for `results: []` cards
    // (Ancient Copper/Gold/Silver, Adorable Kitten) the consuming effect is the
    // RollDie's sub_ability, resolved by the outer resolve_ability_chain at depth+1
    // AFTER this returns. Clearing here would erase the value before it is read.
    state.die_result_this_resolution = Some(actual);

    // CR 706.2: Find the matching result branch and resolve its effect.
    if let Some(branch) = results.iter().find(|b| actual >= b.min && actual <= b.max) {
        let sub = ResolvedAbility::new(
            *branch.effect.effect.clone(),
            ability.targets.clone(),
            ability.source_id,
            ability.controller,
        );
        resolve_ability_chain(state, &sub, events, 0)?;
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::RollDie,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityDefinition, AbilityKind, DieResultBranch, QuantityExpr};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    #[test]
    fn roll_die_emits_event_and_resolves_branch() {
        let mut state = GameState::new_two_player(42);
        let branch = DieResultBranch {
            min: 1,
            max: 20,
            effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: crate::types::ability::TargetFilter::Controller,
                },
            )),
        };
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                sides: 20,
                results: vec![branch],
                modifier: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        // Add a card to draw
        crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            crate::types::zones::Zone::Library,
        );

        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());

        // Should have DieRolled event
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::DieRolled { sides: 20, .. })));
        // Branch covers 1-20, so it always matches — player drew a card
        assert_eq!(state.players[0].hand.len(), 1);
    }

    #[test]
    fn roll_die_no_matching_branch() {
        let mut state = GameState::new_two_player(42);
        // Branch only covers 21+ (impossible on d20), so no effect fires
        let branch = DieResultBranch {
            min: 21,
            max: 30,
            effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: crate::types::ability::TargetFilter::Controller,
                },
            )),
        };
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                sides: 20,
                results: vec![branch],
                modifier: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::DieRolled { .. })));
        assert_eq!(state.players[0].hand.len(), 0);
    }

    #[test]
    fn roll_die_without_branches() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                sides: 6,
                results: vec![],
                modifier: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        // Just emits the die rolled event with no branch resolution
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::DieRolled { sides: 6, .. })));
    }

    /// CR 706.2: "Roll a d20 and add the number of cards in your hand" — the
    /// modifier shifts the natural roll upward. We choose a generous branch
    /// covering 1..=40 so the test is RNG-deterministic regardless of seed.
    #[test]
    fn roll_die_add_modifier_shifts_result_upward() {
        let mut state = GameState::new_two_player(42);
        // Seed two cards into the controller's hand so the modifier resolves to 2.
        state.players[0]
            .hand
            .push_back(crate::types::identifiers::ObjectId(100));
        state.players[0]
            .hand
            .push_back(crate::types::identifiers::ObjectId(101));
        let branch = DieResultBranch {
            min: 1,
            max: 40,
            effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: crate::types::ability::TargetFilter::Controller,
                },
            )),
        };
        // Add a card to draw.
        crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            crate::types::zones::Zone::Library,
        );
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                sides: 20,
                results: vec![branch],
                modifier: Some(DieRollModifier::Add {
                    value: QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::HandSize {
                            player: crate::types::ability::PlayerScope::Controller,
                        },
                    },
                }),
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let result = events
            .iter()
            .find_map(|e| match e {
                GameEvent::DieRolled { result, .. } => Some(*result),
                _ => None,
            })
            .expect("DieRolled event should be present");
        // Natural roll ∈ 1..=20, modifier = +2 (two cards in hand), so actual ∈ 3..=22.
        assert!(
            (3..=22).contains(&result),
            "actual result {result} should reflect +2 modifier"
        );
    }

    /// CR 706.2: "Roll a d20 and subtract the number of cards in your hand" —
    /// the modifier shifts the natural roll downward. With many cards in
    /// hand, the actual result can be 0 or below, which clamps to 0.
    #[test]
    fn roll_die_subtract_modifier_clamps_at_zero() {
        let mut state = GameState::new_two_player(42);
        // Twenty-five cards in hand → modifier resolves to 25; any d20 roll
        // produces actual ≤ 0, which clamps to 0.
        for i in 0..25 {
            state.players[0]
                .hand
                .push_back(crate::types::identifiers::ObjectId(200 + i));
        }
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                sides: 20,
                results: vec![],
                modifier: Some(DieRollModifier::Subtract {
                    value: QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::HandSize {
                            player: crate::types::ability::PlayerScope::Controller,
                        },
                    },
                }),
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let result = events
            .iter()
            .find_map(|e| match e {
                GameEvent::DieRolled { result, .. } => Some(*result),
                _ => None,
            })
            .expect("DieRolled event should be present");
        assert_eq!(result, 0, "subtract modifier should clamp at 0");
    }

    /// CR 706.2: With a seeded RNG, rolling the same die twice from two
    /// identically-seeded states must produce the same natural result.
    /// This is foundational for replays and AI-search determinism.
    #[test]
    fn roll_die_with_seeded_rng_is_deterministic() {
        let mut state_a = GameState::new_two_player(7);
        let mut state_b = GameState::new_two_player(7);
        let ability = |state_seed: PlayerId| {
            ResolvedAbility::new(
                Effect::RollDie {
                    sides: 20,
                    results: vec![],
                    modifier: None,
                },
                vec![],
                ObjectId(1),
                state_seed,
            )
        };
        let mut ev_a = Vec::new();
        let mut ev_b = Vec::new();
        resolve(&mut state_a, &ability(PlayerId(0)), &mut ev_a).unwrap();
        resolve(&mut state_b, &ability(PlayerId(0)), &mut ev_b).unwrap();
        let r_a = ev_a
            .iter()
            .find_map(|e| match e {
                GameEvent::DieRolled { result, .. } => Some(*result),
                _ => None,
            })
            .unwrap();
        let r_b = ev_b
            .iter()
            .find_map(|e| match e {
                GameEvent::DieRolled { result, .. } => Some(*result),
                _ => None,
            })
            .unwrap();
        assert_eq!(r_a, r_b, "identically-seeded RNG must roll the same result");
    }

    /// CR 706.1: All sides in the supported set produce results in 1..=sides.
    /// This sweeps a representative slice of polyhedral dice to ensure the
    /// RNG range is correct for every die size used in Magic (d4, d6, d8,
    /// d10, d12, d20, d100).
    #[test]
    fn roll_die_produces_value_in_range_for_each_die_size() {
        for sides in [4_u8, 6, 8, 10, 12, 20, 100] {
            let mut state = GameState::new_two_player(sides as u64);
            let ability = ResolvedAbility::new(
                Effect::RollDie {
                    sides,
                    results: vec![],
                    modifier: None,
                },
                vec![],
                ObjectId(1),
                PlayerId(0),
            );
            // Roll fifty times per size; every roll must be in 1..=sides.
            for _ in 0..50 {
                let mut events = Vec::new();
                resolve(&mut state, &ability, &mut events).unwrap();
                let r = events
                    .iter()
                    .find_map(|e| match e {
                        GameEvent::DieRolled { result, .. } => Some(*result),
                        _ => None,
                    })
                    .unwrap();
                assert!((1..=sides).contains(&r), "d{sides} result {r} out of range");
            }
        }
    }

    /// CR 706.2 + CR 608.2c: After a RollDie resolves, the actual result is
    /// stamped into `state.last_effect_amount` so a follow-up sub-ability with
    /// `AbilityCondition::PreviousEffectAmount` can gate on it. This is the
    /// channel that powers "If the result is 0 or less, discard your hand"
    /// (Deck of Many Things) and analogous result-conditional riders.
    #[test]
    fn roll_die_stamps_last_effect_amount_for_chain() {
        use crate::types::ability::{AbilityCondition, Comparator};
        let mut state = GameState::new_two_player(7);
        // No modifier: actual result == natural ∈ 1..=20, always > 0.
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                sides: 20,
                results: vec![],
                modifier: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        let result = events
            .iter()
            .find_map(|e| match e {
                GameEvent::DieRolled { result, .. } => Some(*result as i32),
                _ => None,
            })
            .expect("DieRolled event must be present");
        assert_eq!(
            state.last_effect_amount,
            Some(result),
            "last_effect_amount must mirror the actual rolled result so PreviousEffectAmount conditions can read it"
        );
        // And the AbilityCondition resolver consumes that channel correctly.
        let cond = AbilityCondition::PreviousEffectAmount {
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        };
        let dummy = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "probe".into(),
                description: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        assert!(
            crate::game::effects::evaluate_condition(&cond, &state, &dummy),
            "result {result} ≥ 1, so the PreviousEffectAmount(GE, 1) condition must hold"
        );
    }

    /// CR 706.2 (Deck of Many Things, end-to-end): "Roll a d20 and subtract
    /// the number of cards in your hand. If the result is 0 or less, discard
    /// your hand." With 25 cards in hand the modifier dominates any d20 →
    /// actual clamps to 0, so the conditional Discard sub-ability MUST fire.
    #[test]
    fn roll_die_conditional_subability_fires_when_result_le_zero() {
        use crate::types::ability::{
            AbilityCondition, Comparator, DieRollModifier, PlayerScope, QuantityRef, TargetFilter,
        };
        let mut state = GameState::new_two_player(42);
        // Seed 25 real hand objects so the modifier (-25) overpowers any
        // d20 natural roll and the Discard has cards to actually move.
        for i in 0..25 {
            crate::game::zones::create_object(
                &mut state,
                crate::types::identifiers::CardId(2000 + i as u64),
                PlayerId(0),
                format!("Card {i}"),
                crate::types::zones::Zone::Hand,
            );
        }
        let hand_before = state.players[0].hand.len();
        // Conditional Discard guarded by "result ≤ 0".
        let discard = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
                target: TargetFilter::Controller,
                random: false,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
        .condition(AbilityCondition::PreviousEffectAmount {
            comparator: Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 0 },
        });
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                sides: 20,
                results: vec![],
                modifier: Some(DieRollModifier::Subtract {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::Controller,
                        },
                    },
                }),
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
        .sub_ability(discard);
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        // Roll clamped to 0; condition LE 0 holds; controller discards hand.
        assert_eq!(
            state.players[0].hand.len(),
            0,
            "result ≤ 0 should fire the guarded Discard, emptying the hand from {hand_before}"
        );
    }

    /// CR 706.2 (Deck of Many Things, end-to-end): "Roll a d20 and subtract
    /// the number of cards in your hand. If the result is 0 or less, discard
    /// your hand." With zero cards in hand the modifier is 0 → natural roll
    /// (≥ 1) wins → result ≥ 1, so the conditional Discard MUST NOT fire.
    #[test]
    fn roll_die_conditional_subability_skipped_when_result_positive() {
        use crate::types::ability::{
            AbilityCondition, AggregateFunction, Comparator, DieRollModifier, PlayerScope,
            QuantityRef, TargetFilter,
        };
        let mut state = GameState::new_two_player(7);
        // Seed two real hand objects; with 0 cards we'd test nothing — we want
        // visible objects that would have been discarded had the gate broken.
        for i in 0..2 {
            crate::game::zones::create_object(
                &mut state,
                crate::types::identifiers::CardId(3000 + i as u64),
                PlayerId(0),
                format!("Card {i}"),
                crate::types::zones::Zone::Hand,
            );
        }
        // Modifier reads opponent's hand size (which is 0) so the result
        // equals the natural d20 ≥ 1.
        let discard = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
                target: TargetFilter::Controller,
                random: false,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
        .condition(AbilityCondition::PreviousEffectAmount {
            comparator: Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 0 },
        });
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                sides: 20,
                results: vec![],
                modifier: Some(DieRollModifier::Subtract {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::Opponent {
                                aggregate: AggregateFunction::Sum,
                            },
                        },
                    },
                }),
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
        .sub_ability(discard);
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        // Result ≥ 1, so the LE 0 gate fails and the Discard is skipped.
        assert_eq!(
            state.players[0].hand.len(),
            2,
            "result ≥ 1 must not fire the guarded Discard"
        );
    }

    /// CR 706.2: After a RollDie resolves, the actual result is stamped into
    /// `state.die_result_this_resolution` so an inline "equal to
    /// the result" sub_ability (no results table) reads the roll via
    /// `QuantityRef::EventContextAmount`. The stamped value must equal the
    /// `DieRolled` event's result.
    #[test]
    fn roll_die_stamps_die_result_this_resolution() {
        let mut state = GameState::new_two_player(7);
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                sides: 20,
                results: vec![],
                modifier: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let result = events
            .iter()
            .find_map(|e| match e {
                GameEvent::DieRolled { result, .. } => Some(*result),
                _ => None,
            })
            .expect("DieRolled event must be present");
        assert_eq!(
            state.die_result_this_resolution,
            Some(result),
            "die_result_this_resolution must mirror the actual rolled result"
        );
    }

    /// CR 706.4 (issue #1602, building-block guard): "roll a d20. You create a
    /// number of Treasure tokens equal to the result." With a
    /// triggering combat-damage event of 6 already set, the inline sub_ability
    /// whose count is `EventContextAmount` must consume the ROLL, not the 6.
    /// Modeled with a Draw sub_ability (count == EventContextAmount) so we can
    /// assert exactly `result` cards were drawn.
    #[test]
    fn roll_die_subability_reads_roll_not_trigger_event() {
        use crate::types::ability::{QuantityRef, TargetFilter, TargetRef};
        let mut state = GameState::new_two_player(7);
        // Seed enough library cards that any d20 result (≤ 20) can be drawn.
        for i in 0..20 {
            crate::game::zones::create_object(
                &mut state,
                crate::types::identifiers::CardId(4000 + i as u64),
                PlayerId(0),
                format!("Card {i}"),
                crate::types::zones::Zone::Library,
            );
        }
        // The triggering event carries amount 6 (combat damage). If the cascade
        // is wrong, the sub_ability would draw 6 instead of the rolled result.
        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(1)),
            amount: 6,
            is_combat: true,
            excess: 0,
        });
        let draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                sides: 20,
                results: vec![],
                modifier: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
        .sub_ability(draw);
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        let rolled = events
            .iter()
            .find_map(|e| match e {
                GameEvent::DieRolled { result, .. } => Some(*result as usize),
                _ => None,
            })
            .expect("DieRolled event must be present");
        assert!(
            (1..=20).contains(&rolled),
            "d20 result out of range: {rolled}"
        );
        assert_eq!(
            state.players[0].hand.len(),
            rolled,
            "sub_ability must draw cards equal to the rolled result ({rolled}), not the combat damage (6)"
        );
    }
}
