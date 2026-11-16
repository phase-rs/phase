//! Illuminated Folio — activated ability with a reveal cost requiring two cards
//! that share a color.
//!
//! Oracle text (verified via Scryfall): "{1}, {T}, Reveal two cards from your
//! hand that share a color: Draw a card."
//!
//! This test validates that:
//! 1. The parser correctly reads the count (2) and shared-quality constraint (color).
//! 2. The activation prompts for exactly two cards.
//! 3. The two revealed cards must share at least one color.
//! 4. Revealing two cards of different colors is rejected.
//! 5. Revealing only one card is rejected.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::EngineError;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, QuantityRef, SharedQuality,
    TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const ILLUMINATED_FOLIO_ORACLE: &str =
    "{1}, {T}, Reveal two cards from your hand that share a color: Draw a card.";

fn setup_scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Add cards to P0's library so they can draw
    scenario.with_library_top(
        P0,
        &[
            "Library Card 1",
            "Library Card 2",
            "Library Card 3",
            "Library Card 4",
        ],
    );
    scenario
}

fn activate_and_pay(
    runner: &mut GameRunner,
    folio: ObjectId,
    chosen_cards: &[ObjectId],
) -> Result<(), engine::game::EngineError> {
    runner
        .act(GameAction::ActivateAbility {
            source_id: folio,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating Illuminated Folio must be accepted");

    // Drive the pay-cost prompt - handle all cost payment steps
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ManaPayment { .. } => {
                return Err(engine::game::EngineError::InvalidAction(
                    "hit ManaPayment".to_string(),
                ));
            }
            WaitingFor::PayCost {
                kind: PayCostKind::TapCreatures { .. },
                choices,
                ..
            } => {
                // Tap the folio itself
                if choices.contains(&folio) {
                    runner
                        .act(GameAction::SelectCards { cards: vec![folio] })
                        .map_err(|e| {
                            engine::game::EngineError::InvalidAction(format!(
                                "tap payment failed: {e:?}"
                            ))
                        })?;
                } else {
                    return Err(engine::game::EngineError::InvalidAction(
                        "folio not in tap choices".to_string(),
                    ));
                }
            }
            WaitingFor::PayCost {
                kind: PayCostKind::Reveal { .. },
                choices,
                count,
                ..
            } => {
                assert_eq!(
                    count, 2,
                    "Illuminated Folio must require exactly 2 cards to reveal"
                );
                // Verify the eligible choices include our test cards
                assert!(
                    choices.len() >= chosen_cards.len(),
                    "all test cards must be eligible choices"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: chosen_cards.to_vec(),
                    })
                    .map_err(|e| {
                        engine::game::EngineError::InvalidAction(format!(
                            "reveal selection failed: {e:?}"
                        ))
                    })?;
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return Ok(());
                }
                // Pass priority to let the ability resolve
                if runner.act(GameAction::PassPriority).is_err() {
                    // If we can't pass (e.g., it's opponent's turn), just wait
                    return Ok(());
                }
            }
            WaitingFor::GameOver { winner } => {
                return Err(engine::game::EngineError::InvalidAction(format!(
                    "game over during cost payment: winner={:?}",
                    winner
                )));
            }
            other => {
                return Err(engine::game::EngineError::InvalidAction(format!(
                    "unexpected prompt: {other:?}"
                )));
            }
        }
    }
    Err(engine::game::EngineError::InvalidAction(
        "activation pipeline did not settle".to_string(),
    ))
}

#[test]
fn illuminated_folio_reveal_two_blue_cards_succeeds() {
    let mut scenario = setup_scenario();

    // Add Illuminated Folio to battlefield using alternative method
    let folio = scenario
        .add_creature(P0, "Illuminated Folio", 0, 0)
        .as_artifact()
        .from_oracle_text(ILLUMINATED_FOLIO_ORACLE)
        .id();

    // Two blue cards in hand (share blue)
    let blue1 = scenario
        .add_creature_to_hand(P0, "Blue Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();
    let blue2 = scenario
        .add_creature_to_hand(P0, "Blue Card 2", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .id();
    // A red card in hand (different color)
    let _red1 = scenario
        .add_creature_to_hand(P0, "Red Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();

    // Give P0 enough mana to pay {1}
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(91_001),
            false,
            vec![],
        )],
    );

    let mut runner = scenario.build();

    // Activate and pay with two blue cards (should succeed)
    activate_and_pay(&mut runner, folio, &[blue1, blue2]).expect("two blue cards must be valid");

    // Verify the ability resolved - P0 should have drawn a card
    // Started with 3 cards in hand, revealed 2 (still in hand), drew 1 = 4 cards in hand
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        4,
        "player should have drawn a card"
    );
}

#[test]
fn illuminated_folio_reveal_blue_and_red_fails() {
    let mut scenario = setup_scenario();

    let folio = scenario
        .add_creature(P0, "Illuminated Folio", 0, 0)
        .as_artifact()
        .from_oracle_text(ILLUMINATED_FOLIO_ORACLE)
        .id();

    // One blue, one red - different colors
    let blue1 = scenario
        .add_creature_to_hand(P0, "Blue Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();
    let red1 = scenario
        .add_creature_to_hand(P0, "Red Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();

    // Two blue cards keep a legal pair in hand, so the cost is payable
    // and the shared-color check is what rejects the chosen pair.
    scenario
        .add_creature_to_hand(P0, "Blue Card 2", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        });
    scenario
        .add_creature_to_hand(P0, "Blue Card 3", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        });

    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(91_001),
            false,
            vec![],
        )],
    );

    let mut runner = scenario.build();

    // Activate and try to pay with blue + red (should fail - no shared color)
    let result = activate_and_pay(&mut runner, folio, &[blue1, red1]);
    match result {
        Err(EngineError::InvalidAction(msg)) => assert!(
            msg.contains("must share"),
            "rejection must come from the shared-color check, got: {msg}"
        ),
        other => panic!("expected shared-color rejection, got {other:?}"),
    }
}

/// CR 701.20a: Revealing only one card when two are required must be rejected.
/// The reveal prompt must be reached and the selection must fail.
#[test]
fn illuminated_folio_reveal_only_one_card_fails() {
    let mut scenario = setup_scenario();

    let folio = scenario
        .add_creature(P0, "Illuminated Folio", 0, 0)
        .as_artifact()
        .from_oracle_text(ILLUMINATED_FOLIO_ORACLE)
        .id();

    // Two blue cards in hand
    let blue1 = scenario
        .add_creature_to_hand(P0, "Blue Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();
    let _blue2 = scenario
        .add_creature_to_hand(P0, "Blue Card 2", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .id();

    // A second blue card keeps a legal pair in hand, so the cost is payable
    // and the card-count check is what rejects the single card.
    scenario
        .add_creature_to_hand(P0, "Blue Card 3", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        });

    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(91_001),
            false,
            vec![],
        )],
    );

    let mut runner = scenario.build();

    // Try to activate with only one card (should fail - need exactly 2)
    runner
        .act(GameAction::ActivateAbility {
            source_id: folio,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating Illuminated Folio must be accepted");

    // Drive the pay-cost prompt but only select 1 card
    let mut reached_reveal = false;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("mana payment should be passable");
            }
            WaitingFor::PayCost {
                kind: PayCostKind::TapCreatures { .. },
                choices,
                ..
            } => {
                if choices.contains(&folio) {
                    runner
                        .act(GameAction::SelectCards { cards: vec![folio] })
                        .expect("tap payment must succeed");
                }
            }
            WaitingFor::PayCost {
                kind: PayCostKind::Reveal { .. },
                count,
                ..
            } => {
                reached_reveal = true;
                assert_eq!(count, 2);
                // Select only 1 card - should fail
                let result = runner.act(GameAction::SelectCards { cards: vec![blue1] });
                assert!(
                    result.is_err(),
                    "revealing only 1 card when 2 are required must be rejected"
                );
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must succeed");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(
        reached_reveal,
        "activation pipeline must reach reveal prompt"
    );
}

#[test]
fn illuminated_folio_reveal_multicolor_and_blue_succeeds() {
    let mut scenario = setup_scenario();

    let folio = scenario
        .add_creature(P0, "Illuminated Folio", 0, 0)
        .as_artifact()
        .from_oracle_text(ILLUMINATED_FOLIO_ORACLE)
        .id();

    // Multicolor card (blue + red)
    let multi = scenario
        .add_creature_to_hand(P0, "Multicolor Card", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Red],
            generic: 1,
        })
        .id();
    // Blue card
    let blue1 = scenario
        .add_creature_to_hand(P0, "Blue Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();

    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(91_001),
            false,
            vec![],
        )],
    );

    let mut runner = scenario.build();

    // Activate and pay with multicolor + blue (share blue - should succeed)
    activate_and_pay(&mut runner, folio, &[multi, blue1]).expect("multicolor + blue share blue");

    // Verify the ability resolved
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        3, // Started with 2, revealed 2 (still in hand), drew 1 = 3 cards in hand
        "player should have drawn a card"
    );
}

#[test]
fn illuminated_folio_reveal_colorless_cards_fails() {
    let mut scenario = setup_scenario();

    let folio = scenario
        .add_creature(P0, "Illuminated Folio", 0, 0)
        .as_artifact()
        .from_oracle_text(ILLUMINATED_FOLIO_ORACLE)
        .id();

    // Two colorless cards (no colors to share)
    let colorless1 = scenario
        .add_creature_to_hand(P0, "Colorless Card 1", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let colorless2 = scenario
        .add_creature_to_hand(P0, "Colorless Card 2", 3, 3)
        .with_mana_cost(ManaCost::generic(3))
        .id();

    // Two blue cards keep a legal pair in hand, so the cost is payable
    // and the shared-color check is what rejects the chosen pair.
    scenario
        .add_creature_to_hand(P0, "Blue Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        });
    scenario
        .add_creature_to_hand(P0, "Blue Card 2", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        });

    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(91_001),
            false,
            vec![],
        )],
    );

    let mut runner = scenario.build();

    // Activate and try to pay with two colorless cards (should fail - no shared color)
    let result = activate_and_pay(&mut runner, folio, &[colorless1, colorless2]);
    match result {
        Err(EngineError::InvalidAction(msg)) => assert!(
            msg.contains("must share"),
            "rejection must come from the shared-color check, got: {msg}"
        ),
        other => panic!("expected shared-color rejection, got {other:?}"),
    }
}

/// CR 601.2b + CR 701.20a: With enough cards in hand but no two sharing a
/// color, the reveal cost is unpayable, so the Folio cannot be activated. The
/// same board plus a second blue card activates (positive guard).
#[test]
fn illuminated_folio_unpayable_without_a_pair_sharing_a_color() {
    let build = |with_second_blue: bool| {
        let mut scenario = setup_scenario();
        let folio = scenario
            .add_creature(P0, "Illuminated Folio", 0, 0)
            .as_artifact()
            .from_oracle_text(ILLUMINATED_FOLIO_ORACLE)
            .id();
        let mut colored = vec![
            ("Blue Card 1", ManaCostShard::Blue),
            ("Red Card 1", ManaCostShard::Red),
            ("Green Card 1", ManaCostShard::Green),
        ];
        if with_second_blue {
            colored.push(("Blue Card 2", ManaCostShard::Blue));
        }
        for (name, shard) in colored {
            scenario
                .add_creature_to_hand(P0, name, 2, 2)
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![shard],
                    generic: 1,
                });
        }
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(
                ManaType::Colorless,
                ObjectId(91_001),
                false,
                vec![],
            )],
        );
        (scenario.build(), folio)
    };
    let activate = |runner: &mut GameRunner, folio| {
        runner.act(GameAction::ActivateAbility {
            source_id: folio,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
    };

    let (mut runner, folio) = build(false);
    assert!(
        activate(&mut runner, folio).is_err(),
        "no two cards in hand share a color, so the reveal cost is unpayable"
    );
    assert!(runner.state().stack.is_empty());

    let (mut runner, folio) = build(true);
    activate(&mut runner, folio).expect("two blue cards make the reveal cost payable");
}

// =============================================================================
// Discriminating tests for shared-quality reveal continuation routes
// =============================================================================
// NOTE: The following tests manually construct the reveal cost with shared_quality
// because the Oracle parser does not yet support these patterns. No printed cards
// currently use these patterns. When parser support is added, these tests can use
// the parser path directly.

/// CR 601.2b + CR 701.20a: A spell with an additional reveal cost that requires
/// shared quality. The constraint travels with the spell's reveal prompt.
///
/// Oracle: "As an additional cost to cast this spell, reveal two cards from your
/// hand that share a color. Draw a card."
#[test]
fn spell_reveal_shared_color_constraint_travels_with_prompt() {
    use engine::types::ability::{AbilityCost, AdditionalCost, SharedQuality};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two blue cards - can pay
    let blue1 = scenario
        .add_creature_to_hand(P0, "Blue Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();
    let blue2 = scenario
        .add_creature_to_hand(P0, "Blue Card 2", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .id();

    // A red card - can't pay with blue + red
    let red1 = scenario
        .add_creature_to_hand(P0, "Red Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();

    // Library for drawing
    scenario.with_library_top(P0, &["Library Card 1", "Library Card 2"]);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(90_001),
            false,
            vec![],
        )],
    );

    // Add a spell with an additional cost: reveal two cards that share a color
    let mut builder = scenario.add_spell_to_hand(P0, "Spell Reveal Shared Color", false);
    builder.with_mana_cost(ManaCost::generic(0));
    let spell = builder.id();

    let mut runner = scenario.build();

    // Manually inject the additional cost into the pending cast
    let spell_obj = runner.state_mut().objects.get_mut(&spell).unwrap();
    spell_obj.additional_cost = Some(AdditionalCost::Required(AbilityCost::Reveal {
        count: engine::types::ability::QuantityExpr::Fixed { value: 2 },
        filter: Some(engine::types::ability::TargetFilter::Typed(
            engine::types::ability::TypedFilter::card(),
        )),
        shared_quality: Some(SharedQuality::Color),
    }));

    // Add the spell effect: "Draw a card"
    let spell_abilities = std::sync::Arc::make_mut(&mut spell_obj.abilities);
    spell_abilities.push(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    ));

    let card_id = runner.state().objects[&spell].card_id;

    // Cast and pay with two blue cards - should succeed
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting must be accepted");

    // Drive the pay-cost prompt
    let mut reached_reveal = false;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind:
                    PayCostKind::Reveal {
                        shared_quality: Some(quality),
                        ..
                    },
                choices,
                count,
                ..
            } => {
                reached_reveal = true;
                assert_eq!(count, 2);
                assert!(matches!(
                    quality,
                    engine::types::ability::SharedQuality::Color
                ));
                assert!(
                    choices.contains(&blue1) && choices.contains(&blue2) && choices.contains(&red1)
                );
                // Pay with two blue cards - shares color
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![blue1, blue2],
                    })
                    .expect("two blue cards must be accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must succeed");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(reached_reveal, "must reach reveal prompt");

    // Spell should have resolved, drawing a card
    // Started with 4 cards in hand (3 creatures + spell), spell leaves hand,
    // revealed 2 stay in hand, drew 1 = 4 cards in hand
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        4,
        "3 cards remain after casting; revealed cards stay in hand; drew 1 = 4"
    );
    assert!(runner.state().stack.is_empty());
}

/// CR 601.2b + CR 701.20a: Spell reveal with shared quality - wrong pair rejected.
#[test]
fn spell_reveal_shared_color_rejects_different_colors() {
    use engine::types::ability::{AbilityCost, AdditionalCost, SharedQuality};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // One blue, one red - can't share color
    let blue1 = scenario
        .add_creature_to_hand(P0, "Blue Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();
    let red1 = scenario
        .add_creature_to_hand(P0, "Red Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();

    // Keep a legal pair in hand so the cost is payable
    scenario
        .add_creature_to_hand(P0, "Blue Card 2", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        });
    scenario
        .add_creature_to_hand(P0, "Blue Card 3", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        });

    scenario.with_library_top(P0, &["Library Card 1", "Library Card 2"]);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(90_001),
            false,
            vec![],
        )],
    );

    let mut builder = scenario.add_spell_to_hand(P0, "Spell Reveal Shared Color", false);
    builder.with_mana_cost(ManaCost::generic(0));
    let spell = builder.id();

    let mut runner = scenario.build();

    let spell_obj = runner.state_mut().objects.get_mut(&spell).unwrap();
    spell_obj.additional_cost = Some(AdditionalCost::Required(AbilityCost::Reveal {
        count: engine::types::ability::QuantityExpr::Fixed { value: 2 },
        filter: Some(engine::types::ability::TargetFilter::Typed(
            engine::types::ability::TypedFilter::card(),
        )),
        shared_quality: Some(SharedQuality::Color),
    }));

    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting must be accepted");

    // Try to pay with blue + red - should fail
    let mut reached_reveal = false;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Reveal { .. },
                ..
            } => {
                reached_reveal = true;
                let result = runner.act(GameAction::SelectCards {
                    cards: vec![blue1, red1],
                });
                assert!(
                    result.is_err(),
                    "revealing different colors must be rejected"
                );
                match result {
                    Err(EngineError::InvalidAction(msg)) => assert!(
                        msg.contains("must share"),
                        "rejection must be shared-color, got: {msg}"
                    ),
                    _ => panic!("expected InvalidAction"),
                }
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must succeed");
            }
            _ => {}
        }
    }
    assert!(reached_reveal, "must reach reveal prompt");
}

/// CR 702.37e + CR 701.20a: A turn-face-up (morph/disguise) cost with a reveal
/// leg requiring shared quality. The constraint is validated at payment time.
///
/// This test manually constructs a face-down creature with a Morph keyword
/// whose turn-face-up cost is "Reveal two cards from your hand that share a color".
#[test]
fn turn_face_up_reveal_shared_color_constraint_enforced() {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::ability::{AbilityCost, SharedQuality};
    use engine::types::identifiers::ObjectId;
    use engine::types::keywords::{Keyword, MorphCost};
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    use engine::types::phase::Phase;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two blue cards - can pay
    let blue1 = scenario
        .add_creature_to_hand(P0, "Blue Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();
    let blue2 = scenario
        .add_creature_to_hand(P0, "Blue Card 2", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .id();

    // A red card
    let red1 = scenario
        .add_creature_to_hand(P0, "Red Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();

    // Add a creature with a simple Morph keyword (parser supports this)
    // Use a pure mana cost Morph which the parser recognizes, then manually
    // replace with the reveal cost on the back face.
    let ambusher = scenario
        .add_creature_to_hand(P0, "Morph Ambusher", 2, 2)
        .from_oracle_text_with_keywords(&["Morph"], "Morph {2}{U}")
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(90_001), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(90_002), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(90_003), false, vec![]),
        ],
    );

    let mut runner = scenario.build();

    // Manually add Morph keyword since parser doesn't handle parameterized Morph in keyword-only line
    {
        let obj = runner.state_mut().objects.get_mut(&ambusher).unwrap();
        use engine::types::keywords::{Keyword, MorphCost};
        use engine::types::mana::{ManaCost, ManaCostShard};
        let morph_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 2,
        };
        obj.keywords
            .push(Keyword::Morph(MorphCost::Mana(morph_cost.clone())));
        obj.base_keywords
            .push(Keyword::Morph(MorphCost::Mana(morph_cost)));
    }

    let card_id = runner.state().objects[&ambusher].card_id;

    // Cast face down
    runner
        .act(GameAction::PlayFaceDown {
            object_id: ambusher,
            card_id,
        })
        .expect("cast face down");
    assert!(runner.state().objects[&ambusher].face_down);

    // Manually replace the back face's Morph cost with one that requires
    // revealing two cards that share a color.
    {
        let obj = runner.state_mut().objects.get_mut(&ambusher).unwrap();
        let back_face = obj
            .back_face
            .as_mut()
            .expect("face-down card must have back_face");
        // Find and replace the Morph keyword (match any Morph variant)
        for kw in &mut back_face.keywords {
            if let Keyword::Morph(_) = kw {
                *kw = Keyword::Morph(MorphCost::NonMana(AbilityCost::Reveal {
                    count: engine::types::ability::QuantityExpr::Fixed { value: 2 },
                    filter: Some(engine::types::ability::TargetFilter::Typed(
                        engine::types::ability::TypedFilter::card(),
                    )),
                    shared_quality: Some(SharedQuality::Color),
                }));
                break;
            }
        }
    }

    // Turn face up - this should prompt for the reveal cost with shared quality
    runner
        .act(GameAction::TurnFaceUp {
            object_id: ambusher,
            x: 0,
        })
        .expect("turn face up");

    // Drive the pay-cost prompt
    let mut reached_reveal = false;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind:
                    PayCostKind::Reveal {
                        shared_quality: Some(quality),
                        ..
                    },
                choices,
                count,
                ..
            } => {
                reached_reveal = true;
                assert_eq!(count, 2);
                assert!(matches!(
                    quality,
                    engine::types::ability::SharedQuality::Color
                ));
                assert!(
                    choices.contains(&blue1) && choices.contains(&blue2) && choices.contains(&red1)
                );

                // Pay with two blue cards - shares color
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![blue1, blue2],
                    })
                    .expect("two blue cards must be accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must succeed");
            }
            _ => {}
        }
    }
    assert!(reached_reveal, "must reach reveal prompt");
    assert!(
        !runner.state().objects[&ambusher].face_down,
        "must be face up after paying"
    );
}

/// CR 702.37e + CR 701.20a: Turn face up reveal - wrong pair rejected.
///
/// This test manually constructs a face-down creature with a Morph keyword
/// whose turn-face-up cost is "Reveal two cards from your hand that share a color".
#[test]
fn turn_face_up_reveal_shared_color_rejects_different_colors() {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::ability::{AbilityCost, SharedQuality};
    use engine::types::identifiers::ObjectId;
    use engine::types::keywords::{Keyword, MorphCost};
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    use engine::types::phase::Phase;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // One blue, one red - can't share color
    let blue1 = scenario
        .add_creature_to_hand(P0, "Blue Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();
    let red1 = scenario
        .add_creature_to_hand(P0, "Red Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();

    // Keep a legal pair in hand so the cost is payable
    scenario
        .add_creature_to_hand(P0, "Blue Card 2", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        });
    scenario
        .add_creature_to_hand(P0, "Blue Card 3", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        });

    // Add a creature with a simple Morph keyword (parser supports this)
    // Use a pure mana cost Morph which the parser recognizes, then manually
    // replace with the reveal cost on the back face.
    let ambusher = scenario
        .add_creature_to_hand(P0, "Morph Ambusher", 2, 2)
        .from_oracle_text_with_keywords(&["Morph"], "Morph {2}{U}")
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(90_001), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(90_002), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(90_003), false, vec![]),
        ],
    );

    let mut runner = scenario.build();

    // Manually add Morph keyword since parser doesn't handle parameterized Morph in keyword-only line
    {
        let obj = runner.state_mut().objects.get_mut(&ambusher).unwrap();
        use engine::types::keywords::{Keyword, MorphCost};
        use engine::types::mana::{ManaCost, ManaCostShard};
        let morph_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 2,
        };
        obj.keywords
            .push(Keyword::Morph(MorphCost::Mana(morph_cost.clone())));
        obj.base_keywords
            .push(Keyword::Morph(MorphCost::Mana(morph_cost)));
    }

    let card_id = runner.state().objects[&ambusher].card_id;

    // Cast face down
    runner
        .act(GameAction::PlayFaceDown {
            object_id: ambusher,
            card_id,
        })
        .expect("cast face down");
    assert!(runner.state().objects[&ambusher].face_down);

    // Manually replace the back face's Morph cost with one that requires
    // revealing two cards that share a color.
    {
        let obj = runner.state_mut().objects.get_mut(&ambusher).unwrap();
        let back_face = obj
            .back_face
            .as_mut()
            .expect("face-down card must have back_face");
        // Find and replace the Morph keyword (match any Morph variant)
        for kw in &mut back_face.keywords {
            if let Keyword::Morph(_) = kw {
                *kw = Keyword::Morph(MorphCost::NonMana(AbilityCost::Reveal {
                    count: engine::types::ability::QuantityExpr::Fixed { value: 2 },
                    filter: Some(engine::types::ability::TargetFilter::Typed(
                        engine::types::ability::TypedFilter::card(),
                    )),
                    shared_quality: Some(SharedQuality::Color),
                }));
                break;
            }
        }
    }

    // Turn face up - this should prompt for the reveal cost with shared quality
    runner
        .act(GameAction::TurnFaceUp {
            object_id: ambusher,
            x: 0,
        })
        .expect("turn face up");

    // Try to pay with blue + red - should fail
    let mut reached_reveal = false;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Reveal { .. },
                ..
            } => {
                reached_reveal = true;
                let result = runner.act(GameAction::SelectCards {
                    cards: vec![blue1, red1],
                });
                assert!(
                    result.is_err(),
                    "revealing different colors must be rejected"
                );
                match result {
                    Err(EngineError::InvalidAction(msg)) => assert!(
                        msg.contains("must share"),
                        "rejection must be shared-color, got: {msg}"
                    ),
                    _ => panic!("expected InvalidAction"),
                }
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must succeed");
            }
            _ => {}
        }
    }
    assert!(reached_reveal, "must reach reveal prompt");
}

/// CR 601.2h + CR 701.20a: Multiple Reveal legs in a single cost, each with
/// its own shared-quality constraint.
///
/// Oracle: "{1}, {T}, Reveal a card from your hand that shares a color with a
/// card in your graveyard, then reveal a card from your hand that shares a
/// creature type with a creature you control: Draw two cards."
const _MULTI_LEG_REVEAL_ORACLE: &str = "{1}, {T}, Reveal a card from your hand that shares a color with a card in your graveyard, then reveal a card from your hand that shares a creature type with a creature you control: Draw two cards.";

#[test]
fn single_leg_reveal_shared_quality_parsing() {
    // Verify parse_reveal_from_hand_cost_body can produce shared_quality
    use engine::parser::oracle_cost::parse_oracle_cost;
    use engine::types::ability::{AbilityCost, SharedQuality};

    let cost = parse_oracle_cost("Reveal two cards from your hand that share a color");
    assert!(matches!(
        cost,
        AbilityCost::Reveal {
            shared_quality: Some(SharedQuality::Color),
            ..
        }
    ));

    let cost = parse_oracle_cost("Reveal two cards from your hand that share a creature type");
    assert!(matches!(
        cost,
        AbilityCost::Reveal {
            shared_quality: Some(SharedQuality::CreatureType),
            ..
        }
    ));
}

/// CR 701.20a + CR 107.3a: Reveal X cards where X is announced and the cards
/// must share a quality. X is bounded by the largest qualifying group.
///
#[test]
fn reveal_x_shared_color_x_bounded_by_largest_group() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Hand: 2 blue, 2 red, 1 green = 5 cards
    // Largest group sharing a color = 2 (blue or red)
    // So X can be at most 2

    let blue1 = scenario
        .add_creature_to_hand(P0, "Blue Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();
    let blue2 = scenario
        .add_creature_to_hand(P0, "Blue Card 2", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .id();
    let _red1 = scenario
        .add_creature_to_hand(P0, "Red Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();
    let _red2 = scenario
        .add_creature_to_hand(P0, "Red Card 2", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        })
        .id();
    let _green1 = scenario
        .add_creature_to_hand(P0, "Green Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();

    scenario.with_library_top(
        P0,
        &[
            "Library Card 1",
            "Library Card 2",
            "Library Card 3",
            "Library Card 4",
            "Library Card 5",
        ],
    );
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(90_001), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(90_002), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(90_003), false, vec![]),
        ],
    );

    // Add an artifact with an activated ability that has a reveal X cost
    let mut builder = scenario.add_creature(P0, "X Reveal Folio", 0, 0);
    builder.as_artifact();
    // We need to manually inject the ability since parser doesn't support X in reveal count with shared quality
    let folio = builder.id();

    let mut runner = scenario.build();

    // Manually inject the activated ability with a variable X reveal cost
    let folio_obj = runner.state_mut().objects.get_mut(&folio).unwrap();
    let abilities = std::sync::Arc::make_mut(&mut folio_obj.abilities);
    let x_quantity = QuantityExpr::Ref {
        qty: QuantityRef::Variable {
            name: "X".to_string(),
        },
    };
    abilities.push(
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: x_quantity.clone(),
                target: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Reveal {
                    count: x_quantity,
                    filter: Some(TargetFilter::Typed(TypedFilter::card())),
                    shared_quality: Some(SharedQuality::Color),
                },
            ],
        }),
    );

    // Try to activate with X=3 - should fail because no 3 cards share a color
    runner
        .act(GameAction::ActivateAbility {
            source_id: folio,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation accepted, then X announcement");

    // Must be at ChooseXValue prompt
    let WaitingFor::ChooseXValue { .. } = runner.state().waiting_for else {
        panic!(
            "expected ChooseXValue, got {:?}",
            runner.state().waiting_for
        );
    };

    // Try X=3 - should be rejected (max is 2)
    let result = runner.act(GameAction::ChooseX { value: 3 });
    assert!(
        result.is_err(),
        "X=3 should be rejected - no 3 cards share a color"
    );
    assert!(
        runner.state().stack.is_empty(),
        "ability should not go on stack"
    );

    // Still at ChooseXValue - choose X=2 instead (no need to re-activate)
    let WaitingFor::ChooseXValue { .. } = runner.state().waiting_for else {
        panic!(
            "expected ChooseXValue after X=3 rejection, got {:?}",
            runner.state().waiting_for
        );
    };

    // Choose X=2 - should succeed
    runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("X=2 should be accepted");

    // Drive the pay-cost prompt
    let mut reached_reveal = false;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind:
                    PayCostKind::Reveal {
                        shared_quality: Some(quality),
                        ..
                    },
                choices: _,
                count,
                ..
            } => {
                reached_reveal = true;
                assert_eq!(count, 2);
                assert!(matches!(
                    quality,
                    engine::types::ability::SharedQuality::Color
                ));

                // Pay with two blue cards
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![blue1, blue2],
                    })
                    .expect("two blue cards must be accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must succeed");
            }
            _ => {}
        }
    }
    assert!(reached_reveal, "must reach reveal prompt");

    // Should have drawn 2 cards
    // Started with 5, revealed 2 (stay in hand per CR 701.20b), drew 2 = 7
    assert_eq!(runner.state().players[P0.0 as usize].hand.len(), 7);
}

#[test]
fn reveal_x_shared_color_x_zero_reveals_none() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _blue1 = scenario
        .add_creature_to_hand(P0, "Blue Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();

    scenario.with_library_top(P0, &["Library Card 1"]);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(90_001),
            false,
            vec![],
        )],
    );

    // Add an artifact with an activated ability that has a reveal X cost
    let mut builder = scenario.add_creature(P0, "X Reveal Folio", 0, 0);
    builder.as_artifact();
    // We need to manually inject the ability since parser doesn't support X in reveal count with shared quality
    let folio = builder.id();

    let mut runner = scenario.build();

    // Manually inject the activated ability with a variable X reveal cost
    let folio_obj = runner.state_mut().objects.get_mut(&folio).unwrap();
    let abilities = std::sync::Arc::make_mut(&mut folio_obj.abilities);
    let x_quantity = QuantityExpr::Ref {
        qty: QuantityRef::Variable {
            name: "X".to_string(),
        },
    };
    abilities.push(
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: x_quantity.clone(),
                target: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Reveal {
                    count: x_quantity,
                    filter: Some(TargetFilter::Typed(TypedFilter::card())),
                    shared_quality: Some(SharedQuality::Color),
                },
            ],
        }),
    );

    // Activate with X=0 - should succeed without revealing
    runner
        .act(GameAction::ActivateAbility {
            source_id: folio,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation accepted, then X announcement");

    let WaitingFor::ChooseXValue { .. } = runner.state().waiting_for else {
        panic!(
            "expected ChooseXValue, got {:?}",
            runner.state().waiting_for
        );
    };

    // Choose X=0
    runner
        .act(GameAction::ChooseX { value: 0 })
        .expect("X=0 should be accepted");

    // Should not reach reveal prompt, ability resolves immediately
    runner.advance_until_stack_empty();

    // Drew 0 cards, hand unchanged
    assert_eq!(runner.state().players[P0.0 as usize].hand.len(), 1);
}

// =============================================================================
// Focused tests for shared_quality consumers in casting_costs and morph
// These test the payability and payment validation directly by constructing
// the cost state, bypassing the Oracle parser.
// =============================================================================

/// CR 601.2b + CR 701.20a: Spell additional cost with shared-quality reveal.
/// Tests the payability gate and payment validation for a spell's additional
/// reveal cost by constructing the cost directly.
#[test]
fn spell_additional_cost_reveal_shared_color_payability_and_payment() {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::ability::{AbilityCost, SharedQuality};
    use engine::types::actions::GameAction;
    use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    // Hand: two blue cards (share color), one red card
    let blue1 = scenario
        .add_creature_to_hand(P0, "Blue Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();
    let blue2 = scenario
        .add_creature_to_hand(P0, "Blue Card 2", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .id();
    let red1 = scenario
        .add_creature_to_hand(P0, "Red Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();

    // Library for drawing
    scenario.with_library_top(P0, &["Library Card 1", "Library Card 2"]);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(90_001),
            false,
            vec![],
        )],
    );

    // Add a spell with an additional cost: reveal two cards that share a color
    let mut builder = scenario.add_spell_to_hand(P0, "Spell With Reveal Cost", false);
    builder.with_mana_cost(ManaCost::generic(1));
    // Manually construct the additional cost with shared_quality
    let spell_id = builder.id();

    let mut runner = scenario.build();

    // Manually inject the additional cost into the pending cast
    // This simulates what the parser would produce for "As an additional cost to cast this spell, reveal two cards from your hand that share a color"
    let spell_obj = runner.state_mut().objects.get_mut(&spell_id).unwrap();
    spell_obj.additional_cost = Some(engine::types::ability::AdditionalCost::Required(
        AbilityCost::Reveal {
            count: engine::types::ability::QuantityExpr::Fixed { value: 2 },
            filter: Some(engine::types::ability::TargetFilter::Typed(
                engine::types::ability::TypedFilter::card(),
            )),
            shared_quality: Some(SharedQuality::Color),
        },
    ));

    // Cast the spell
    let card_id = runner.state().objects[&spell_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting must be accepted");

    // Drive the pay-cost prompt for the additional cost
    let mut reached_reveal = false;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind:
                    PayCostKind::Reveal {
                        shared_quality: Some(quality),
                        ..
                    },
                choices,
                count,
                ..
            } => {
                reached_reveal = true;
                assert_eq!(count, 2);
                assert!(matches!(quality, SharedQuality::Color));
                assert!(
                    choices.contains(&blue1) && choices.contains(&blue2) && choices.contains(&red1)
                );

                // Pay with two blue cards - shares color (should succeed)
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![blue1, blue2],
                    })
                    .expect("two blue cards must be accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must succeed");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(reached_reveal, "must reach reveal prompt");

    // Spell should have resolved (no effect, so no card drawn)
    // Started with 4 cards in hand (3 creatures + spell), spell leaves hand,
    // revealed 2 stay in hand, no draw = 3 cards in hand
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        3,
        "3 cards remain after casting; revealed cards stay in hand; no draw effect = 3"
    );
    assert!(runner.state().stack.is_empty());
}

/// CR 601.2b + CR 701.20a: Spell additional cost with shared-quality reveal - wrong pair rejected.
#[test]
fn spell_additional_cost_reveal_shared_color_rejects_different_colors() {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::ability::{AbilityCost, SharedQuality};
    use engine::types::actions::GameAction;
    use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    // Hand: one blue, one red (don't share color), plus two more blue to keep cost payable
    let blue1 = scenario
        .add_creature_to_hand(P0, "Blue Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();
    let red1 = scenario
        .add_creature_to_hand(P0, "Red Card 1", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();
    scenario
        .add_creature_to_hand(P0, "Blue Card 2", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        });
    scenario
        .add_creature_to_hand(P0, "Blue Card 3", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        });

    scenario.with_library_top(P0, &["Library Card 1", "Library Card 2"]);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(90_001),
            false,
            vec![],
        )],
    );

    let mut builder = scenario.add_spell_to_hand(P0, "Spell With Reveal Cost", false);
    builder.with_mana_cost(ManaCost::generic(1));
    let spell_id = builder.id();

    let mut runner = scenario.build();

    let spell_obj = runner.state_mut().objects.get_mut(&spell_id).unwrap();
    spell_obj.additional_cost = Some(engine::types::ability::AdditionalCost::Required(
        AbilityCost::Reveal {
            count: engine::types::ability::QuantityExpr::Fixed { value: 2 },
            filter: Some(engine::types::ability::TargetFilter::Typed(
                engine::types::ability::TypedFilter::card(),
            )),
            shared_quality: Some(SharedQuality::Color),
        },
    ));

    let card_id = runner.state().objects[&spell_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting must be accepted");

    // Try to pay with blue + red - should fail
    let mut reached_reveal = false;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Reveal { .. },
                ..
            } => {
                reached_reveal = true;
                let result = runner.act(GameAction::SelectCards {
                    cards: vec![blue1, red1],
                });
                assert!(
                    result.is_err(),
                    "revealing different colors must be rejected"
                );
                match result {
                    Err(engine::game::EngineError::InvalidAction(msg)) => assert!(
                        msg.contains("must share"),
                        "rejection must be shared-color, got: {msg}"
                    ),
                    _ => panic!("expected InvalidAction"),
                }
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must succeed");
            }
            _ => {}
        }
    }
    assert!(reached_reveal, "must reach reveal prompt");
}
