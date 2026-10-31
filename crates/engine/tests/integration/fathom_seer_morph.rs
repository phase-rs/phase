//! CR 702.37e + CR 116.2b + CR 118.1 + CR 118.3 + CR 118.11 + CR 614.6 + CR 616.1:
//! Non-mana morph cost integration tests.
//!
//! Fathom Seer:
//! "Morph—Return two Islands you control to their owner's hand.
//! When this creature is turned face up, draw two cards."

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::game_state::{AlternativeCastKeyword, CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

#[test]
fn fathom_seer_cast_face_down_and_turn_face_up_paying_islands() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // 2 Islands on battlefield controlled by P0
    let island1 = scenario.add_basic_land(P0, ManaColor::Blue);
    let island2 = scenario.add_basic_land(P0, ManaColor::Blue);

    // Fathom Seer in P0 hand
    let seer = scenario
        .add_creature_to_hand(P0, "Fathom Seer", 1, 3)
        .from_oracle_text(
            "Morph—Return two Islands you control to their owner's hand.\nWhen this creature is turned face up, draw two cards.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        })
        .id();

    // Give P0 3 colorless mana to cast face-down
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                seer,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                seer,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                seer,
                false,
                vec![],
            ),
        ],
    );

    // Put some cards in P0's library so drawing succeeds
    for _ in 0..5 {
        scenario.add_card_to_library_top(P0, "Library Card");
    }

    let mut runner = scenario.build();

    // 1. Cast face down for {3}
    let card_id = runner.state().objects[&seer].card_id;
    let res = runner.act(GameAction::CastSpell {
        object_id: seer,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    assert!(
        res.is_ok(),
        "face-down cast should succeed: {:?}",
        res.err()
    );
    // CR 601.2b / CR 702.37c: Since both normal cast and face-down cast are
    // affordable, the engine surfaces WaitingFor::AlternativeCastChoice with FaceDown.
    match &runner.state().waiting_for {
        WaitingFor::AlternativeCastChoice { keyword, .. } => {
            assert_eq!(*keyword, AlternativeCastKeyword::FaceDown);
            runner
                .act(GameAction::ChooseAlternativeCast {
                    choice: AlternativeCastDecision::Alternative,
                })
                .expect("choosing face down alternative cast should succeed");
        }
        other => panic!("expected AlternativeCastChoice(FaceDown), got {other:?}"),
    }

    // Resolve the face-down creature spell on the stack
    runner.resolve_top();

    // Verify it is on the battlefield and face down
    assert_eq!(runner.state().objects[&seer].zone, Zone::Battlefield);
    assert!(runner.state().objects[&seer].face_down);
    assert_eq!(runner.state().objects[&seer].power, Some(2));
    assert_eq!(runner.state().objects[&seer].toughness, Some(2));

    // 2. Turn face up
    let res = runner.act(GameAction::TurnFaceUp {
        object_id: seer,
        x: 0,
    });
    assert!(
        res.is_ok(),
        "turn face up action should succeed: {:?}",
        res.err()
    );

    // Should be waiting for PayCost::ReturnToHand
    match &runner.state().waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::ReturnToHand,
            choices,
            count,
            ..
        } => {
            assert_eq!(*count, 2);
            assert!(choices.contains(&island1));
            assert!(choices.contains(&island2));
        }
        other => panic!("expected WaitingFor::PayCost(ReturnToHand), got {other:?}"),
    }

    // 3. Select the 2 Islands to return
    let res = runner.act(GameAction::SelectCards {
        cards: vec![island1, island2],
    });
    assert!(
        res.is_ok(),
        "selecting cards to pay cost should succeed: {:?}",
        res.err()
    );

    // Verify Islands returned to hand
    assert_eq!(runner.state().objects[&island1].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&island2].zone, Zone::Hand);

    // Verify Fathom Seer is face up
    assert!(!runner.state().objects[&seer].face_down);
    assert_eq!(runner.state().objects[&seer].name, "Fathom Seer");
    assert_eq!(runner.state().objects[&seer].power, Some(1));
    assert_eq!(runner.state().objects[&seer].toughness, Some(3));

    // Resolve the turn face-up trigger ("draw two cards")
    let hand_before = runner.state().players[0].hand.len();
    runner.resolve_top();
    let hand_after = runner.state().players[0].hand.len();
    assert_eq!(
        hand_after - hand_before,
        2,
        "must have drawn two cards from turn face up trigger"
    );
    assert!(
        runner.state().stack.is_empty(),
        "stack must be empty after resolving trigger"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "state must be at Priority boundary"
    );
}

// CR 118.1 + CR 118.3 + CR 702.37e: Multi-object cost payment requires distinct IDs.
#[test]
fn fathom_seer_duplicate_island_selection_rejected() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let island1 = scenario.add_basic_land(P0, ManaColor::Blue);
    let island2 = scenario.add_basic_land(P0, ManaColor::Blue);
    let seer = scenario
        .add_creature_to_hand(P0, "Fathom Seer", 1, 3)
        .from_oracle_text(
            "Morph—Return two Islands you control to their owner's hand.\nWhen this creature is turned face up, draw two cards.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                seer,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                seer,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                seer,
                false,
                vec![],
            ),
        ],
    );
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&seer].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: seer,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast face down");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("choose face down");
    }
    runner.resolve_top();
    assert!(runner.state().objects[&seer].face_down);

    runner
        .act(GameAction::TurnFaceUp {
            object_id: seer,
            x: 0,
        })
        .expect("turn face up");

    // Guard exact prompt before duplicate selection (N3). CR 118.3: the prompt
    // publishes the exact minimum the handler enforces.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::ReturnToHand,
                count: 2,
                min_count: 2,
                ..
            }
        ),
        "expected WaitingFor::PayCost(ReturnToHand, count=2), got {:?}",
        runner.state().waiting_for
    );

    // Supplying duplicate ID [island1, island1] must be rejected
    let res = runner.act(GameAction::SelectCards {
        cards: vec![island1, island1],
    });
    assert!(res.is_err(), "must reject duplicate island IDs");
    // Board and face-down state must remain unchanged
    assert!(runner.state().objects[&seer].face_down);
    assert_eq!(runner.state().objects[&island1].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&island2].zone, Zone::Battlefield);
}

// CR 702.37e + CR 118.3: Insufficient resources to pay turn-face-up cost
#[test]
fn fathom_seer_insufficient_islands_cannot_turn_face_up() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let island1 = scenario.add_basic_land(P0, ManaColor::Blue);
    let seer = scenario
        .add_creature_to_hand(P0, "Fathom Seer", 1, 3)
        .from_oracle_text(
            "Morph—Return two Islands you control to their owner's hand.\nWhen this creature is turned face up, draw two cards.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                seer,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                seer,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                seer,
                false,
                vec![],
            ),
        ],
    );
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&seer].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: seer,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast face down");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("choose face down");
    }
    runner.resolve_top();
    assert!(runner.state().objects[&seer].face_down);

    // Attempting to turn face up must be rejected
    let res = runner.act(GameAction::TurnFaceUp {
        object_id: seer,
        x: 0,
    });
    assert!(
        res.is_err(),
        "must reject turn face up when lacking required Islands"
    );
    assert!(runner.state().objects[&seer].face_down);
    assert_eq!(runner.state().objects[&island1].zone, Zone::Battlefield);
}

// CR 702.37e + CR 118.11 + CR 701.9: Discard as turn-face-up cost
#[test]
fn gathan_raiders_turn_face_up_discard_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let discard_card = scenario.add_card_to_hand(P0, "Mountain");
    let raiders = scenario
        .add_creature_to_hand(P0, "Gathan Raiders", 3, 3)
        .from_oracle_text("Morph—Discard a card.")
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                raiders,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                raiders,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                raiders,
                false,
                vec![],
            ),
        ],
    );
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&raiders].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: raiders,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast face down");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("choose face down");
    }
    runner.resolve_top();
    assert!(runner.state().objects[&raiders].face_down);

    runner
        .act(GameAction::TurnFaceUp {
            object_id: raiders,
            x: 0,
        })
        .expect("turn face up");
    match &runner.state().waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::Discard,
            count,
            choices,
            ..
        } => {
            assert_eq!(*count, 1);
            assert!(choices.contains(&discard_card));
        }
        other => panic!("expected PayCost(Discard), got {other:?}"),
    }
    let res = runner.act(GameAction::SelectCards {
        cards: vec![discard_card],
    });
    assert!(res.is_ok(), "discarding card as cost succeeds: {res:?}");
    assert_eq!(runner.state().objects[&discard_card].zone, Zone::Graveyard);
    assert!(!runner.state().objects[&raiders].face_down);
    assert_eq!(runner.state().objects[&raiders].power, Some(3));
}

// CR 702.37e + CR 701.20a + CR 701.20b: Reveal as turn-face-up cost
#[test]
fn ruthless_ripper_turn_face_up_reveal_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let black_card = scenario
        .add_creature_to_hand(P0, "Black Creature", 1, 1)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    let ripper = scenario
        .add_creature_to_hand(P0, "Ruthless Ripper", 1, 1)
        .from_oracle_text("Deathtouch\nMorph—Reveal a black card in your hand.")
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                ripper,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                ripper,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                ripper,
                false,
                vec![],
            ),
        ],
    );
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&ripper].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: ripper,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast face down");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("choose face down");
    }
    runner.resolve_top();
    assert!(runner.state().objects[&ripper].face_down);

    runner
        .act(GameAction::TurnFaceUp {
            object_id: ripper,
            x: 0,
        })
        .expect("turn face up");
    match &runner.state().waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::Reveal { .. },
            count,
            choices,
            ..
        } => {
            assert_eq!(*count, 1);
            assert!(choices.contains(&black_card));
        }
        other => panic!("expected PayCost(Reveal), got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![black_card],
        })
        .expect("reveal card");
    assert!(!runner.state().objects[&ripper].face_down);
    assert_eq!(runner.state().objects[&black_card].zone, Zone::Hand);
}

// CR 702.37e + CR 701.21a: Sacrifice as turn-face-up cost
#[test]
fn skirk_volcanist_turn_face_up_sacrifice_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mountain1 = scenario.add_basic_land(P0, ManaColor::Red);
    let mountain2 = scenario.add_basic_land(P0, ManaColor::Red);
    let volcanist = scenario
        .add_creature_to_hand(P0, "Skirk Volcanist", 3, 1)
        .from_oracle_text("Morph—Sacrifice two Mountains.")
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
        ],
    );
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&volcanist].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: volcanist,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast face down");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("choose face down");
    }
    runner.resolve_top();
    assert!(runner.state().objects[&volcanist].face_down);

    runner
        .act(GameAction::TurnFaceUp {
            object_id: volcanist,
            x: 0,
        })
        .expect("turn face up");
    match &runner.state().waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::Sacrifice,
            count,
            choices,
            ..
        } => {
            assert_eq!(*count, 2);
            assert!(choices.contains(&mountain1));
            assert!(choices.contains(&mountain2));
        }
        other => panic!("expected PayCost(Sacrifice), got {other:?}"),
    }
    let res = runner
        .act(GameAction::SelectCards {
            cards: vec![mountain1, mountain2],
        })
        .expect("sacrifice mountains");
    assert!(!runner.state().objects[&volcanist].face_down);
    assert_eq!(runner.state().objects[&mountain1].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&mountain2].zone, Zone::Graveyard);

    let sac_events: Vec<_> = res
        .events
        .iter()
        .filter(|e| {
            matches!(
                e,
                engine::types::events::GameEvent::PermanentSacrificed { .. }
            )
        })
        .collect();
    assert_eq!(
        sac_events.len(),
        2,
        "must emit PermanentSacrificed for both Mountains"
    );
}

// CR 702.37e + CR 119: Pay life as turn-face-up cost
#[test]
fn zombie_cutthroat_turn_face_up_pay_life_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cutthroat = scenario
        .add_creature_to_hand(P0, "Zombie Cutthroat", 3, 4)
        .from_oracle_text("Morph—Pay 5 life.")
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                cutthroat,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                cutthroat,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                cutthroat,
                false,
                vec![],
            ),
        ],
    );
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&cutthroat].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: cutthroat,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast face down");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("choose face down");
    }
    runner.resolve_top();
    assert!(runner.state().objects[&cutthroat].face_down);
    assert_eq!(runner.state().players[0].life, 20);

    runner
        .act(GameAction::TurnFaceUp {
            object_id: cutthroat,
            x: 0,
        })
        .expect("turn face up");
    assert!(!runner.state().objects[&cutthroat].face_down);
    assert_eq!(runner.state().players[0].life, 15);
}

// CR 702.37e + F5: Serialization round-trip for MorphCost and CostResume::TurnFaceUp
#[test]
fn morph_cost_and_cost_resume_turn_face_up_serialization_round_trip() {
    use engine::types::ability::{AbilityCost, QuantityExpr, TurnUpCostSource};
    use engine::types::game_state::CostResume;
    use engine::types::identifiers::ObjectId;
    use engine::types::keywords::MorphCost;

    let morph_cost = MorphCost::NonMana(AbilityCost::ReturnToHand {
        count: 2,
        filter: None,
        from_zone: None,
    });
    let json = serde_json::to_string(&morph_cost).expect("serialize morph_cost");
    let deserialized: MorphCost = serde_json::from_str(&json).expect("deserialize morph_cost");
    assert_eq!(morph_cost, deserialized);

    let cost_resume = CostResume::TurnFaceUp {
        source: engine::types::identifiers::ObjectIncarnationRef::of(ObjectId(42), 1),
        cost_source: TurnUpCostSource::Morph,
        remaining_cost: Some(Box::new(AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 5 },
        })),
    };
    let json = serde_json::to_string(&cost_resume).expect("serialize cost_resume");
    let deserialized: CostResume = serde_json::from_str(&json).expect("deserialize cost_resume");
    assert_eq!(cost_resume, deserialized);

    // CR 702.37e: Legacy wire compatibility with bare object_id
    let legacy_json = r#"{"type":"TurnFaceUp","object_id":42,"cost_source":"Morph"}"#;
    let from_legacy: CostResume =
        serde_json::from_str(legacy_json).expect("deserialize legacy cost_resume");
    assert!(
        matches!(from_legacy, CostResume::TurnFaceUp { source, cost_source: TurnUpCostSource::Morph, .. } if source.object_id == ObjectId(42))
    );
}

// F3 + F5: Coverage check for MorphCost support
#[test]
fn morph_cost_unsupported_coverage_check() {
    use engine::types::ability::AbilityCost;
    use engine::types::keywords::{is_ability_cost_supported_for_morph, MorphCost};

    let supported = MorphCost::NonMana(AbilityCost::ReturnToHand {
        count: 2,
        filter: None,
        from_zone: None,
    });
    assert!(supported.is_supported());

    let unsupported = MorphCost::NonMana(AbilityCost::Tap);
    assert!(!unsupported.is_supported());
    assert!(!is_ability_cost_supported_for_morph(&AbilityCost::Tap));

    // CR 702.37e: Composite is unsupported for morph (C2/C5)
    let composite = MorphCost::NonMana(AbilityCost::Composite {
        costs: vec![AbilityCost::Tap],
    });
    assert!(!composite.is_supported());
    assert!(!is_ability_cost_supported_for_morph(
        &AbilityCost::Composite {
            costs: vec![AbilityCost::Tap],
        }
    ));
}

// CR 702.37e + CR 701.21a + CR 614.6 + CR 616.1: Sacrifice turn-face-up cost with replacement choice
#[test]
fn skirk_volcanist_turn_face_up_sacrifice_replacement_pause() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, ReplacementMode, TargetFilter,
    };
    use engine::types::replacements::ReplacementEvent;
    use engine::types::zones::EtbTapState;
    use engine::types::ReplacementDefinition;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mountain1 = scenario.add_basic_land(P0, ManaColor::Red);
    let mountain2 = scenario.add_basic_land(P0, ManaColor::Red);
    let volcanist = scenario
        .add_creature_to_hand(P0, "Skirk Volcanist", 3, 1)
        .from_oracle_text("Morph—Sacrifice two Mountains.")
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    // Add optional replacement redirect for graveyard moves
    scenario
        .add_creature(engine::game::scenario::P1, "Redirector", 1, 1)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .destination_zone(Zone::Graveyard)
                .mode(ReplacementMode::Optional { decline: None })
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        origin: None,
                        destination: Zone::Exile,
                        target: TargetFilter::SelfRef,
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: EtbTapState::Unspecified,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                        conditional_enter_with_counters: vec![],
                        enters_modified_if: None,
                        face_down_profile: None,
                    },
                )),
        );
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
        ],
    );
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&volcanist].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: volcanist,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast face down");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("choose face down");
    }
    runner.resolve_top();

    runner
        .act(GameAction::TurnFaceUp {
            object_id: volcanist,
            x: 0,
        })
        .expect("turn face up");

    runner
        .act(GameAction::SelectCards {
            cards: vec![mountain1, mountain2],
        })
        .expect("select mountains");

    // Pauses for optional replacement on first mountain
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "expected ReplacementChoice pause, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_cost_move_resume.is_some(),
        "must retain PendingCostMoveResume across replacement pause"
    );

    // Accept replacement (exiles instead of graveyard)
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("choose replacement");

    // May pause again for second mountain, or complete
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("choose replacement");
    }

    // CR 614.6: Both sacrificed mountains were moved to exile instead of graveyard.
    assert_eq!(runner.state().objects[&mountain1].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&mountain2].zone, Zone::Exile);

    // Must be face up now
    assert!(!runner.state().objects[&volcanist].face_down);
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "pending_cost_move_resume must be cleared"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "must complete at Priority boundary, got {:?}",
        runner.state().waiting_for
    );
}

// CR 702.37e + CR 605.3b + CR 616.1: Cost move root resumes at common PriorityBoundary after child prompt drains
#[test]
fn turn_face_up_cost_move_resumes_after_mandatory_child_drains() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ChoiceType, ControllerRef, Effect, TargetSelectionMode,
    };
    use engine::types::replacements::ReplacementEvent;
    use engine::types::{ReplacementDefinition, TargetFilter};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let island1 = scenario.add_basic_land(P0, ManaColor::Blue);
    let island2 = scenario.add_basic_land(P0, ManaColor::Blue);
    let seer = scenario
        .add_creature_to_hand(P0, "Fathom Seer", 1, 3)
        .from_oracle_text("Morph—Return two Islands you control to their owner's hand.")
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                seer,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                seer,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                seer,
                false,
                vec![],
            ),
        ],
    );
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&seer].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: seer,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast face down");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("choose face down");
    }
    runner.resolve_top();
    assert!(runner.state().objects[&seer].face_down);

    // Install delivery-child replacement on island1 (moved to hand -> choose odd or even)
    let delivery_repl = ReplacementDefinition::new(ReplacementEvent::Moved)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Choose {
                choice_type: ChoiceType::OddOrEven,
                chooser: ControllerRef::You,
                persist: false,
                selection: TargetSelectionMode::Chosen,
            },
        ))
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(Zone::Hand);

    std::sync::Arc::make_mut(
        &mut runner
            .state_mut()
            .objects
            .get_mut(&island1)
            .unwrap()
            .base_replacement_definitions,
    )
    .push(delivery_repl.clone());
    runner
        .state_mut()
        .objects
        .get_mut(&island1)
        .unwrap()
        .replacement_definitions
        .push(delivery_repl);

    // Public payment flow: dispatch GameAction::TurnFaceUp
    runner
        .act(GameAction::TurnFaceUp {
            object_id: seer,
            x: 0,
        })
        .expect("turn face up");

    // Select the two Islands to return to hand
    runner
        .act(GameAction::SelectCards {
            cards: vec![island1, island2],
        })
        .expect("select islands");

    // Intermediate state: island1 was moved to hand, but delivery replacement paused for NamedChoice
    assert_eq!(runner.state().objects[&island1].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&island2].zone, Zone::Battlefield);
    assert!(
        runner.state().objects[&seer].face_down,
        "must remain face down while paused"
    );
    assert!(
        runner.state().pending_cost_move_resume.is_some(),
        "must hold pending_cost_move_resume while child prompt is open"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::OddOrEven,
                ..
            }
        ),
        "expected NamedChoice child prompt, got {:?}",
        runner.state().waiting_for
    );

    // Submitting the choice drains the child prompt, reaches PriorityBoundary,
    // resumes TurnFaceUpCostMove, finishes moving island2, and flips Fathom Seer.
    let res = runner.act(GameAction::ChooseOption {
        choice: "Odd".to_string(),
    });
    assert!(res.is_ok(), "choice action should succeed: {:?}", res.err());

    assert_eq!(runner.state().objects[&island2].zone, Zone::Hand);
    assert!(
        !runner.state().objects[&seer].face_down,
        "Fathom Seer must be flipped face up"
    );
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "pending_cost_move_resume must be cleared"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "state must settle at Priority boundary, got {:?}",
        runner.state().waiting_for
    );
}

// CR 702.37e + CR 614.6 + CR 616.1: Deferred life cost depth pre-capture ensures child tail completes before flip
#[test]
fn zombie_cutthroat_deferred_life_cost_child_drain_before_flip() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ChooseOneOfKind, Effect, PlayerFilter, QuantityExpr,
        QuantityModification,
    };
    use engine::types::replacements::ReplacementEvent;
    use engine::types::{ReplacementDefinition, TargetFilter};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cutthroat = scenario
        .add_creature_to_hand(P0, "Zombie Cutthroat", 3, 4)
        .from_oracle_text("Morph—Pay 5 life.")
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                cutthroat,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                cutthroat,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                cutthroat,
                false,
                vec![],
            ),
        ],
    );
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&cutthroat].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: cutthroat,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast face down");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("choose face down");
    }
    runner.resolve_top();
    assert!(runner.state().objects[&cutthroat].face_down);
    assert_eq!(runner.state().players[0].life, 20);

    // Install interactive LoseLife substitution rider on cutthroat:
    // When controller loses life, prompt ChooseOneOfBranch with a branch that gains 1 life
    let choice = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChooseOneOf {
            chooser: PlayerFilter::Controller,
            branches: vec![AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )],
            kind: ChooseOneOfKind::Ordinary,
        },
    );
    let life_repl = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::Plus { value: 0 })
        .execute(choice);

    std::sync::Arc::make_mut(
        &mut runner
            .state_mut()
            .objects
            .get_mut(&cutthroat)
            .unwrap()
            .base_replacement_definitions,
    )
    .push(life_repl.clone());
    runner
        .state_mut()
        .objects
        .get_mut(&cutthroat)
        .unwrap()
        .replacement_definitions
        .push(life_repl);

    // Public payment flow: dispatch GameAction::TurnFaceUp
    runner
        .act(GameAction::TurnFaceUp {
            object_id: cutthroat,
            x: 0,
        })
        .expect("turn face up");

    // Intermediate state:
    // 5 life was deducted immediately by the engine (20 -> 15)
    assert_eq!(runner.state().players[0].life, 15);
    // Creature is still face down
    assert!(
        runner.state().objects[&cutthroat].face_down,
        "must remain face down during pause"
    );
    // Deferred life root is parked
    assert!(
        runner.state().pending_deferred_life_cost_resume.is_some(),
        "must park pending_deferred_life_cost_resume"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch { .. }
        ),
        "expected ChooseOneOfBranch prompt, got {:?}",
        runner.state().waiting_for
    );

    // Submitting the child choice executes the branch (+1 life), drains back to
    // resolution depth 0, resumes DeferredLifeCostResume::TurnFaceUp, and flips Zombie Cutthroat.
    let res = runner.act(GameAction::ChooseBranch { index: 0 });
    assert!(res.is_ok(), "branch action should succeed: {:?}", res.err());

    // Creature is now turned face up
    assert!(!runner.state().objects[&cutthroat].face_down);
    // Life was deducted (20 - 5 = 15) then gained 1 from branch (16)
    assert_eq!(runner.state().players[0].life, 16);
    assert!(
        runner.state().pending_deferred_life_cost_resume.is_none(),
        "deferred life root must be cleared"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "must be at Priority, got {:?}",
        runner.state().waiting_for
    );
}

// CR 702.37e + CR 601.2h + CR 614.6: Preserve ordinary delivery children across sacrifice cost payment
#[test]
fn turn_face_up_sacrifice_cost_preserves_delivery_children_until_drain() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ChoiceType, ControllerRef, Effect, ReplacementDefinition,
        TargetFilter, TargetRef, TargetSelectionMode,
    };
    use engine::types::game_state::PendingCostMoveResume;
    use engine::types::replacements::ReplacementEvent;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mountain1 = scenario.add_basic_land(P0, ManaColor::Red);
    let mountain2 = scenario.add_basic_land(P0, ManaColor::Red);
    let mayhem_devil = scenario
        .add_creature(P0, "Mayhem Devil", 3, 3)
        .from_oracle_text(
            "Whenever a player sacrifices a permanent, Mayhem Devil deals 1 damage to any target.",
        )
        .id();
    let volcanist = scenario
        .add_creature_to_hand(P0, "Skirk Volcanist", 3, 1)
        .from_oracle_text("Morph—Sacrifice two Mountains.")
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
        ],
    );
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&volcanist].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: volcanist,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast face down");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("choose face down");
    }
    runner.resolve_top();
    assert!(runner.state().objects[&volcanist].face_down);

    // Install delivery-child replacement effect on each Mountain (moved to graveyard -> choose odd or even)
    let delivery_repl = ReplacementDefinition::new(ReplacementEvent::Moved)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Choose {
                choice_type: ChoiceType::OddOrEven,
                chooser: ControllerRef::You,
                persist: false,
                selection: TargetSelectionMode::Chosen,
            },
        ))
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(Zone::Graveyard);

    std::sync::Arc::make_mut(
        &mut runner
            .state_mut()
            .objects
            .get_mut(&mountain1)
            .unwrap()
            .base_replacement_definitions,
    )
    .push(delivery_repl.clone());
    runner
        .state_mut()
        .objects
        .get_mut(&mountain1)
        .unwrap()
        .replacement_definitions
        .push(delivery_repl.clone());

    std::sync::Arc::make_mut(
        &mut runner
            .state_mut()
            .objects
            .get_mut(&mountain2)
            .unwrap()
            .base_replacement_definitions,
    )
    .push(delivery_repl.clone());
    runner
        .state_mut()
        .objects
        .get_mut(&mountain2)
        .unwrap()
        .replacement_definitions
        .push(delivery_repl);

    // Public TurnFaceUp special action
    let res = runner.act(GameAction::TurnFaceUp {
        object_id: volcanist,
        x: 0,
    });
    assert!(res.is_ok(), "TurnFaceUp must succeed: {:?}", res.err());
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ),
        "must be WaitingFor::PayCost(Sacrifice), got {:?}",
        runner.state().waiting_for
    );

    // Submit SelectCards with the two Mountains
    let res1 = runner.act(GameAction::SelectCards {
        cards: vec![mountain1, mountain2],
    });
    assert!(res1.is_ok(), "SelectCards must succeed: {:?}", res1.err());
    let res1 = res1.unwrap();
    // CR 701.21a + CR 603.2: First mountain moved to graveyard; its delivery tail
    // paused on an ordinary child prompt. PermanentSacrificed must be emitted so
    // sacrifice observers trigger.
    let sac_events1: Vec<_> = res1
        .events
        .iter()
        .filter(|e| {
            matches!(
                e,
                engine::types::events::GameEvent::PermanentSacrificed { .. }
            )
        })
        .collect();
    assert_eq!(
        sac_events1.len(),
        1,
        "must emit exactly one PermanentSacrificed when first mountain moves, got {:?}",
        sac_events1
    );
    assert_eq!(
        sac_events1[0],
        &engine::types::events::GameEvent::PermanentSacrificed {
            object_id: mountain1,
            player_id: P0,
        }
    );

    // First mountain moved to graveyard; its delivery tail paused on NamedChoice (not clobbered by replacement_choice)
    assert_eq!(runner.state().objects[&mountain1].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&mountain2].zone, Zone::Battlefield);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::OddOrEven,
                ..
            }
        ),
        "must be on first Mountain's NamedChoice prompt, got {:?}",
        runner.state().waiting_for
    );
    // CR 603.2c + CR 603.10a: both Mountains leave together, so the first
    // sacrifice's events wait in the payment ledger instead of being collected
    // as their own event across the initial pause.
    assert!(
        runner.state().deferred_triggers.is_empty(),
        "no observer may be collected for a partial departure group"
    );
    assert!(
        matches!(
            &runner.state().pending_cost_move_resume,
            Some(PendingCostMoveResume::TurnFaceUpCostMove {
                deferred_cost_events,
                ..
            }) if !deferred_cost_events.is_empty()
        ),
        "first sacrifice's events must be retained in the payment ledger"
    );
    assert!(
        runner.state().objects[&volcanist].face_down,
        "permanent must remain face down while payment is paused"
    );
    assert!(
        matches!(
            runner.state().pending_cost_move_resume,
            Some(PendingCostMoveResume::TurnFaceUpCostMove {
                paused_at_index: 0,
                ..
            })
        ),
        "must have TurnFaceUpCostMove paused at index 0"
    );

    // Answer first child prompt
    let res2 = runner.act(GameAction::ChooseOption {
        choice: "Odd".to_string(),
    });
    assert!(
        res2.is_ok(),
        "first ChooseOption must succeed: {:?}",
        res2.err()
    );
    let res2 = res2.unwrap();
    // CR 701.21a + CR 603.2: Second mountain moved to graveyard; its delivery tail
    // paused on the second child prompt. Exactly one PermanentSacrificed must be emitted.
    let sac_events2: Vec<_> = res2
        .events
        .iter()
        .filter(|e| {
            matches!(
                e,
                engine::types::events::GameEvent::PermanentSacrificed { .. }
            )
        })
        .collect();
    assert_eq!(
        sac_events2.len(),
        1,
        "must emit exactly one PermanentSacrificed when second mountain moves, got {:?}",
        sac_events2
    );
    assert_eq!(
        sac_events2[0],
        &engine::types::events::GameEvent::PermanentSacrificed {
            object_id: mountain2,
            player_id: P0,
        }
    );

    // Second mountain moved to graveyard; its delivery tail paused on second NamedChoice
    assert_eq!(runner.state().objects[&mountain2].zone, Zone::Graveyard);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::OddOrEven,
                ..
            }
        ),
        "must be on second Mountain's NamedChoice prompt, got {:?}",
        runner.state().waiting_for
    );
    // CR 603.2c + CR 603.10a: the second delivery tail still holds the payment
    // open, so both sacrifices' events stay in the ledger until it settles.
    assert!(
        runner.state().deferred_triggers.is_empty(),
        "no observer may be collected before the payment's group settles"
    );
    assert!(
        matches!(
            &runner.state().pending_cost_move_resume,
            Some(PendingCostMoveResume::TurnFaceUpCostMove {
                deferred_cost_events,
                ..
            }) if deferred_cost_events.iter().filter(|e| matches!(
                e,
                engine::types::events::GameEvent::PermanentSacrificed { .. }
            )).count() == 2
        ),
        "both sacrifices' events must be retained in the payment ledger"
    );
    assert!(
        runner.state().objects[&volcanist].face_down,
        "permanent must remain face down while second payment is paused"
    );
    assert!(
        matches!(
            runner.state().pending_cost_move_resume,
            Some(PendingCostMoveResume::TurnFaceUpCostMove {
                paused_at_index: 1,
                ..
            })
        ),
        "must have TurnFaceUpCostMove paused at index 1"
    );

    // Answer second child prompt
    let res3 = runner.act(GameAction::ChooseOption {
        choice: "Even".to_string(),
    });
    assert!(
        res3.is_ok(),
        "second ChooseOption must succeed: {:?}",
        res3.err()
    );
    let res3 = res3.unwrap();
    let sac_events3: Vec<_> = res3
        .events
        .iter()
        .filter(|e| {
            matches!(
                e,
                engine::types::events::GameEvent::PermanentSacrificed { .. }
            )
        })
        .collect();
    assert_eq!(
        sac_events3.len(),
        0,
        "no sacrifice events on final child drain, got {:?}",
        sac_events3
    );

    // Exactly one flip: permanent is turned face up
    assert!(
        !runner.state().objects[&volcanist].face_down,
        "Skirk Volcanist must be turned face up after all costs and children finish"
    );
    // Roots cleared
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "pending_cost_move_resume must be cleared"
    );

    // CR 603.2 + CR 603.3b: exactly two eventual triggered results after both child answers.
    // The first Mayhem Devil trigger prompts for target selection:
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "must prompt for first Mayhem Devil trigger target, got {:?}",
        runner.state().waiting_for
    );
    let target_res1 = runner.act(GameAction::SelectTargets {
        targets: vec![TargetRef::Player(P1)],
    });
    assert!(
        target_res1.is_ok(),
        "first Mayhem Devil SelectTargets must succeed: {:?}",
        target_res1.err()
    );

    // Second Mayhem Devil trigger prompts for target selection:
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "must prompt for second Mayhem Devil trigger target, got {:?}",
        runner.state().waiting_for
    );
    let target_res2 = runner.act(GameAction::SelectTargets {
        targets: vec![TargetRef::Player(P1)],
    });
    assert!(
        target_res2.is_ok(),
        "second Mayhem Devil SelectTargets must succeed: {:?}",
        target_res2.err()
    );

    // Exactly two eventual triggered results are now on the stack
    assert_eq!(
        runner.state().stack.len(),
        2,
        "must have exactly two Mayhem Devil triggers on the stack"
    );
    for entry in &runner.state().stack {
        assert_eq!(
            entry.source_id, mayhem_devil,
            "stack entry source must be Mayhem Devil"
        );
    }

    // At Priority
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "state must settle at Priority, got {:?}",
        runner.state().waiting_for
    );
}

// CR 702.37e + CR 603.2 + CR 603.3b: Paired unpaused control: Skirk Volcanist sacrificing
// two Mountains without delivery-child replacement effects immediately triggers Mayhem Devil twice.
#[test]
fn turn_face_up_sacrifice_cost_unpaused_control_preserves_triggers_on_stack() {
    use engine::types::ability::TargetRef;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mountain1 = scenario.add_basic_land(P0, ManaColor::Red);
    let mountain2 = scenario.add_basic_land(P0, ManaColor::Red);
    let mayhem_devil = scenario
        .add_creature(P0, "Mayhem Devil", 3, 3)
        .from_oracle_text(
            "Whenever a player sacrifices a permanent, Mayhem Devil deals 1 damage to any target.",
        )
        .id();
    let volcanist = scenario
        .add_creature_to_hand(P0, "Skirk Volcanist", 3, 1)
        .from_oracle_text("Morph—Sacrifice two Mountains.")
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
        ],
    );
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&volcanist].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: volcanist,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast face down");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("choose face down");
    }
    runner.resolve_top();
    assert!(runner.state().objects[&volcanist].face_down);

    // Public TurnFaceUp special action
    let res = runner.act(GameAction::TurnFaceUp {
        object_id: volcanist,
        x: 0,
    });
    assert!(res.is_ok(), "TurnFaceUp must succeed: {:?}", res.err());
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ),
        "must be WaitingFor::PayCost(Sacrifice), got {:?}",
        runner.state().waiting_for
    );

    // Submit SelectCards with the two Mountains (no replacement pause)
    let res1 = runner.act(GameAction::SelectCards {
        cards: vec![mountain1, mountain2],
    });
    assert!(res1.is_ok(), "SelectCards must succeed: {:?}", res1.err());

    // Both Mountains are sacrificed to graveyard
    assert_eq!(runner.state().objects[&mountain1].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&mountain2].zone, Zone::Graveyard);

    // Skirk Volcanist is turned face up
    assert!(
        !runner.state().objects[&volcanist].face_down,
        "Skirk Volcanist must be turned face up"
    );

    // CR 603.2 + CR 603.3b: Both Mayhem Devil triggers are put onto the stack.
    // Target prompt 1:
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "must prompt for first Mayhem Devil trigger target, got {:?}",
        runner.state().waiting_for
    );
    let target_res1 = runner.act(GameAction::SelectTargets {
        targets: vec![TargetRef::Player(P1)],
    });
    assert!(
        target_res1.is_ok(),
        "first Mayhem Devil SelectTargets must succeed: {:?}",
        target_res1.err()
    );

    // Target prompt 2:
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "must prompt for second Mayhem Devil trigger target, got {:?}",
        runner.state().waiting_for
    );
    let target_res2 = runner.act(GameAction::SelectTargets {
        targets: vec![TargetRef::Player(P1)],
    });
    assert!(
        target_res2.is_ok(),
        "second Mayhem Devil SelectTargets must succeed: {:?}",
        target_res2.err()
    );

    // Exactly two Mayhem Devil triggers on the stack
    assert_eq!(
        runner.state().stack.len(),
        2,
        "must have exactly two Mayhem Devil triggers on the stack"
    );
    for entry in &runner.state().stack {
        assert_eq!(
            entry.source_id, mayhem_devil,
            "stack entry source must be Mayhem Devil"
        );
    }

    // State settles at Priority
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "state must settle at Priority, got {:?}",
        runner.state().waiting_for
    );
}

fn cast_face_down(
    runner: &mut engine::game::scenario::GameRunner,
    object_id: engine::types::ObjectId,
) {
    let card_id = runner.state().objects[&object_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast face down");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("choose face down");
    }
    runner.resolve_top();
    assert!(runner.state().objects[&object_id].face_down);
}

// CR 116.2b + CR 601.2h + CR 602.2b + CR 702.37e: Turning face up is a special
// action, not casting a spell or activating an ability. Prohibitions on paying
// life or sacrificing permanents to cast spells or activate abilities (such as
// Yasharn, Implacable Earth) do not prevent turning face down creatures face up.
#[test]
fn turn_face_up_life_and_sacrifice_costs_permitted_under_yasharn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Yasharn on battlefield controlled by P1:
    // "Players can't pay life or sacrifice nonland permanents to cast spells or activate abilities."
    scenario.add_creature_from_oracle(
        P1,
        "Yasharn, Implacable Earth",
        4,
        4,
        "When Yasharn enters, search your library for a basic Forest card and a basic Plains card, reveal those cards, put them into your hand, then shuffle.\nPlayers can't pay life or sacrifice nonland permanents to cast spells or activate abilities.",
    );

    // Zombie Cutthroat in hand
    let cutthroat = scenario
        .add_creature_to_hand(P0, "Zombie Cutthroat", 3, 4)
        .from_oracle_text("Morph—Pay 5 life.")
        .id();

    // Skirk Volcanist in hand: "Morph—Sacrifice two Mountains."
    let volcanist = scenario
        .add_creature_to_hand(P0, "Skirk Volcanist", 3, 1)
        .from_oracle_text("Morph—Sacrifice two Mountains.")
        .id();

    // Two Mountains on battlefield for P0
    let mountain1 = scenario.add_basic_land(P0, ManaColor::Red);
    let mountain2 = scenario.add_basic_land(P0, ManaColor::Red);

    // 6 colorless mana for P0 to cast cutthroat and volcanist face down
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                cutthroat,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                cutthroat,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                cutthroat,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
        ],
    );

    let mut runner = scenario.build();

    cast_face_down(&mut runner, cutthroat);
    cast_face_down(&mut runner, volcanist);

    // 1. Turning Zombie Cutthroat face up by paying 5 life is legal under Yasharn
    let res = runner.act(GameAction::TurnFaceUp {
        object_id: cutthroat,
        x: 0,
    });
    assert!(
        res.is_ok(),
        "turn face up paying life under Yasharn must succeed: {:?}",
        res.err()
    );
    assert!(!runner.state().objects[&cutthroat].face_down);
    assert_eq!(runner.state().players[P0.0 as usize].life, 15);

    // 2. Turning Skirk Volcanist face up by sacrificing two Mountains is legal under Yasharn
    let res = runner.act(GameAction::TurnFaceUp {
        object_id: volcanist,
        x: 0,
    });
    assert!(
        res.is_ok(),
        "turn face up initiating sacrifice must succeed: {:?}",
        res.err()
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost {
            kind: PayCostKind::Sacrifice,
            ..
        }
    ));

    let res = runner.act(GameAction::SelectCards {
        cards: vec![mountain1, mountain2],
    });
    assert!(
        res.is_ok(),
        "sacrificing mountains under Yasharn must succeed: {:?}",
        res.err()
    );
    assert!(!runner.state().objects[&volcanist].face_down);
    assert_eq!(runner.state().objects[&mountain1].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&mountain2].zone, Zone::Graveyard);
}

// CR 702.37e: MorphCost::NonMana(AbilityCost::Mana { cost }) normalizes to
// TurnFaceUpCost::Mana in turn_face_up_prepare, routing through the standard
// turn-face-up mana payment pipeline.
#[test]
fn turn_face_up_normalizes_non_mana_mana_cost_and_pays_via_special_action() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let morph_creature = scenario
        .add_creature_to_hand(P0, "Mana Morph Creature", 2, 2)
        .from_oracle_text("Morph—Pay 5 life.")
        .id();

    // 3 colorless mana to cast face down, plus 2 colorless to turn face up
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                morph_creature,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                morph_creature,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                morph_creature,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                morph_creature,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                morph_creature,
                false,
                vec![],
            ),
        ],
    );

    let mut runner = scenario.build();

    cast_face_down(&mut runner, morph_creature);

    // Directly attach MorphCost::NonMana(AbilityCost::Mana { cost: {2} }) to verify normalization
    let two_mana = ManaCost::Cost {
        generic: 2,
        shards: vec![],
    };
    let obj = runner.state_mut().objects.get_mut(&morph_creature).unwrap();
    let back_face = obj.back_face.as_mut().expect("back_face exists");
    back_face.keywords = vec![engine::types::keywords::Keyword::Morph(
        engine::types::keywords::MorphCost::NonMana(engine::types::ability::AbilityCost::Mana {
            cost: two_mana,
        }),
    )];

    let res = runner.act(GameAction::TurnFaceUp {
        object_id: morph_creature,
        x: 0,
    });
    assert!(
        res.is_ok(),
        "TurnFaceUp with normalized NonMana(AbilityCost::Mana) must succeed: {:?}",
        res.err()
    );
    assert!(
        !runner.state().objects[&morph_creature].face_down,
        "permanent must be face up after paying normalized mana cost"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "mana pool must be consumed"
    );
}

// CR 400.2: Hand candidate sets for Reveal and Discard morph payments leak private
// hand contents (which cards and how many match the required property). The viewer
// filter redacts choices to empty for viewers without private access, preserving public
// required-count metadata.
#[test]
fn turn_face_up_reveal_and_discard_redacts_private_choices_for_opponent_and_spectator() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Ruthless Ripper: "Morph—Reveal a black card in your hand."
    let ripper = scenario
        .add_creature_to_hand(P0, "Ruthless Ripper", 1, 1)
        .from_oracle_text(
            "Deathtouch\nMorph—Reveal a black card in your hand.\nWhen this creature is turned face up, target player loses 2 life.",
        )
        .id();

    // P0 hand: 1 black creature, 1 white card
    let black_card = scenario
        .add_creature_to_hand(P0, "Black Card", 1, 1)
        .with_color(vec![ManaColor::Black])
        .id();
    let _white_card = scenario
        .add_creature_to_hand(P0, "White Card", 1, 1)
        .with_color(vec![ManaColor::White])
        .id();

    // 3 colorless mana to cast face down
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                ripper,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                ripper,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                ripper,
                false,
                vec![],
            ),
        ],
    );

    let mut runner = scenario.build();

    cast_face_down(&mut runner, ripper);

    // P0 initiates TurnFaceUp
    let res = runner.act(GameAction::TurnFaceUp {
        object_id: ripper,
        x: 0,
    });
    assert!(
        res.is_ok(),
        "initiating reveal payment must succeed: {:?}",
        res.err()
    );

    // In raw game state, choices contain black_card but not white_card
    let raw_waiting = &runner.state().waiting_for;
    assert!(
        matches!(
            raw_waiting,
            WaitingFor::PayCost {
                player: P0,
                kind: PayCostKind::Reveal { .. },
                count: 1,
                ..
            }
        ),
        "raw state must be waiting for PayCost::Reveal, got {:?}",
        raw_waiting
    );
    if let WaitingFor::PayCost { choices, count, .. } = raw_waiting {
        assert_eq!(*count, 1);
        assert_eq!(choices, &vec![black_card]);
    }

    // Filter for actor (P0) -> choices are preserved
    let p0_view = engine::game::visibility::filter_state_for_viewer(runner.state(), P0);
    if let WaitingFor::PayCost { choices, count, .. } = &p0_view.waiting_for {
        assert_eq!(*count, 1, "P0 must see required count");
        assert_eq!(
            choices,
            &vec![black_card],
            "P0 must see their eligible black card"
        );
    } else {
        panic!("P0 view must be WaitingFor::PayCost");
    }

    // Filter for opponent (P1) -> choices are redacted to empty, count is preserved
    let p1_view = engine::game::visibility::filter_state_for_viewer(runner.state(), P1);
    if let WaitingFor::PayCost { choices, count, .. } = &p1_view.waiting_for {
        assert_eq!(*count, 1, "P1 must see public required count");
        assert!(
            choices.is_empty(),
            "P1 must not see private candidate IDs or set cardinality, got {:?}",
            choices
        );
    } else {
        panic!("P1 view must be WaitingFor::PayCost");
    }

    // Filter for spectator (unseated) -> choices are redacted to empty, count is preserved
    let spectator_view = engine::game::visibility::filter_state_for_unseated_viewer(runner.state());
    if let WaitingFor::PayCost { choices, count, .. } = &spectator_view.waiting_for {
        assert_eq!(*count, 1, "Spectator must see public required count");
        assert!(
            choices.is_empty(),
            "Spectator must not see private candidate IDs or set cardinality, got {:?}",
            choices
        );
    } else {
        panic!("Spectator view must be WaitingFor::PayCost");
    }

    // P0 selects the black card to complete the payment
    let res = runner.act(GameAction::SelectCards {
        cards: vec![black_card],
    });
    assert!(res.is_ok(), "SelectCards must succeed: {:?}", res.err());
    let res = res.unwrap();

    // Verify public CardsRevealed event is emitted with the selected card
    let revealed = res.events.iter().any(|e| {
        matches!(
            e,
            engine::types::events::GameEvent::CardsRevealed {
                card_ids,
                card_names,
                ..
            } if card_ids.contains(&black_card) && card_names.contains(&"Black Card".to_string())
        )
    });
    assert!(
        revealed,
        "public CardsRevealed event must be emitted for the chosen card"
    );
    assert!(
        !runner.state().objects[&ripper].face_down,
        "ripper must be turned face up"
    );
}

// CR 701.21a + CR 702.37e + CR 116.2b: Turning face up is a special action exempt from
// cast/activation sacrifice prohibitions (Yasharn), but universal "can't be sacrificed"
// restrictions (Assault Suit) must exclude the object from morph sacrifice candidates.
#[test]
fn turn_face_up_sacrifice_cost_excludes_cant_be_sacrificed_permanent_under_yasharn() {
    use engine::types::statics::StaticMode;
    use engine::types::{StaticDefinition, TargetFilter};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Yasharn on battlefield controlled by P1:
    // "Players can't pay life or sacrifice nonland permanents to cast spells or activate abilities."
    scenario.add_creature_from_oracle(
        P1,
        "Yasharn, Implacable Earth",
        4,
        4,
        "When Yasharn enters, search your library for a basic Forest card and a basic Plains card, reveal those cards, put them into your hand, then shuffle.\nPlayers can't pay life or sacrifice nonland permanents to cast spells or activate abilities.",
    );

    // Gift of Doom in P0's hand: "Morph—Sacrifice another creature."
    let gift = scenario
        .add_creature_to_hand(P0, "Gift of Doom", 1, 1)
        .from_oracle_text("Morph—Sacrifice another creature.")
        .id();

    // Creature equipped with Assault Suit: "can't be sacrificed"
    let assault_creature = scenario.add_creature(P0, "Assault Bear", 2, 2).id();

    // Ordinary creature: can be sacrificed to pay the special action cost
    let ordinary_creature = scenario.add_creature(P0, "Ordinary Bear", 2, 2).id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                gift,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                gift,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                gift,
                false,
                vec![],
            ),
        ],
    );

    let mut runner = scenario.build();

    // Add CantBeSacrificed static to assault_creature
    runner
        .state_mut()
        .objects
        .get_mut(&assault_creature)
        .unwrap()
        .static_definitions
        .push(
            StaticDefinition::new(StaticMode::Other("CantBeSacrificed".to_string()))
                .affected(TargetFilter::SelfRef),
        );

    cast_face_down(&mut runner, gift);

    // P0 initiates TurnFaceUp
    let res = runner.act(GameAction::TurnFaceUp {
        object_id: gift,
        x: 0,
    });
    assert!(
        res.is_ok(),
        "initiating morph sacrifice under Yasharn must succeed: {:?}",
        res.err()
    );

    // Prompt must offer ordinary_creature, but EXCLUDE assault_creature
    let waiting = &runner.state().waiting_for;
    assert!(
        matches!(
            waiting,
            WaitingFor::PayCost {
                player: P0,
                kind: PayCostKind::Sacrifice,
                count: 1,
                ..
            }
        ),
        "expected WaitingFor::PayCost::Sacrifice, got {:?}",
        waiting
    );
    if let WaitingFor::PayCost { choices, count, .. } = waiting {
        assert_eq!(*count, 1);
        assert!(
            choices.contains(&ordinary_creature),
            "ordinary creature must be an eligible sacrifice choice under Yasharn"
        );
        assert!(
            !choices.contains(&assault_creature),
            "CR 701.21a: CantBeSacrificed permanent must be excluded from sacrifice choices"
        );
        assert!(
            !choices.contains(&gift),
            "face-down morph source must not sacrifice itself"
        );
    }

    // Complete the payment by sacrificing ordinary_creature
    let res = runner.act(GameAction::SelectCards {
        cards: vec![ordinary_creature],
    });
    assert!(
        res.is_ok(),
        "SelectCards for ordinary_creature must succeed: {:?}",
        res.err()
    );

    assert_eq!(
        runner.state().objects[&ordinary_creature].zone,
        Zone::Graveyard
    );
    assert_eq!(
        runner.state().objects[&assault_creature].zone,
        Zone::Battlefield
    );
    assert!(
        !runner.state().objects[&gift].face_down,
        "Gift of Doom must be turned face up"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

// CR 702.37e + CR 118.1: Morph cost shapes must reject aggregate sacrifice requirements
// and random/source-card discards across offer, payment, and coverage.
#[test]
fn turn_face_up_rejects_unsupported_aggregate_and_random_discard_costs() {
    use engine::game::EngineError;
    use engine::types::ability::{
        CardSelectionMode, Comparator, DiscardSelfScope, QuantityExpr, SacrificeAggregateStat,
        SacrificeCost, SacrificeRequirement,
    };
    use engine::types::keywords::{is_ability_cost_supported_for_morph, Keyword};
    use engine::types::{AbilityCost, TargetFilter};

    // 1. Aggregate sacrifice requirement
    let aggregate_sac_cost = AbilityCost::Sacrifice(SacrificeCost {
        target: TargetFilter::Any,
        requirement: SacrificeRequirement::Aggregate {
            stat: SacrificeAggregateStat::TotalPower,
            comparator: Comparator::GE,
            value: 4,
        },
    });
    assert!(
        !is_ability_cost_supported_for_morph(&aggregate_sac_cost),
        "aggregate sacrifice must be unsupported for morph"
    );

    // 2. Random from-hand discard
    let random_discard_cost = AbilityCost::Discard {
        count: QuantityExpr::Fixed { value: 1 },
        filter: None,
        selection: CardSelectionMode::Random,
        self_scope: DiscardSelfScope::FromHand,
    };
    assert!(
        !is_ability_cost_supported_for_morph(&random_discard_cost),
        "random discard must be unsupported for morph"
    );

    // 3. Source-card discard
    let source_discard_cost = AbilityCost::Discard {
        count: QuantityExpr::Fixed { value: 1 },
        filter: None,
        selection: CardSelectionMode::Chosen,
        self_scope: DiscardSelfScope::SourceCard,
    };
    assert!(
        !is_ability_cost_supported_for_morph(&source_discard_cost),
        "source-card discard must be unsupported for morph"
    );

    // Verify public payment rejection: GameAction::TurnFaceUp with unsupported morph payload
    // A) Aggregate sacrifice
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let agg_morph = scenario
        .add_creature_to_hand(P0, "Aggregate Morph", 2, 2)
        .with_keyword(Keyword::morph(aggregate_sac_cost))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                agg_morph,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                agg_morph,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                agg_morph,
                false,
                vec![],
            ),
        ],
    );
    let mut runner = scenario.build();
    cast_face_down(&mut runner, agg_morph);

    let res = runner.act(GameAction::TurnFaceUp {
        object_id: agg_morph,
        x: 0,
    });
    assert!(
        matches!(res, Err(EngineError::InvalidAction(_))),
        "TurnFaceUp with aggregate sacrifice must fail with InvalidAction, got {:?}",
        res
    );
    assert!(
        runner.state().objects[&agg_morph].face_down,
        "must remain face down"
    );

    // B) Random discard
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rand_morph = scenario
        .add_creature_to_hand(P0, "Random Discard Morph", 2, 2)
        .with_keyword(Keyword::morph(random_discard_cost))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                rand_morph,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                rand_morph,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                rand_morph,
                false,
                vec![],
            ),
        ],
    );
    let mut runner = scenario.build();
    cast_face_down(&mut runner, rand_morph);

    let res = runner.act(GameAction::TurnFaceUp {
        object_id: rand_morph,
        x: 0,
    });
    assert!(
        matches!(res, Err(EngineError::InvalidAction(_))),
        "TurnFaceUp with random discard must fail with InvalidAction, got {:?}",
        res
    );
    assert!(
        runner.state().objects[&rand_morph].face_down,
        "must remain face down"
    );

    // C) Source-card discard
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let src_morph = scenario
        .add_creature_to_hand(P0, "Source Discard Morph", 2, 2)
        .with_keyword(Keyword::morph(source_discard_cost))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                src_morph,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                src_morph,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                src_morph,
                false,
                vec![],
            ),
        ],
    );
    let mut runner = scenario.build();
    cast_face_down(&mut runner, src_morph);

    let res = runner.act(GameAction::TurnFaceUp {
        object_id: src_morph,
        x: 0,
    });
    assert!(
        matches!(res, Err(EngineError::InvalidAction(_))),
        "TurnFaceUp with source-card discard must fail with InvalidAction, got {:?}",
        res
    );
    assert!(
        runner.state().objects[&src_morph].face_down,
        "must remain face down"
    );
}

// CR 702.37e + CR 400.7 + CR 101.3 + CR 708.9: Settle a paid morph action when
// its original permanent leaves the battlefield during payment.
//
// Raven Guild Initiate with Maskwood Nexus:
// Initiate says "Morph—Return a Bird you control to its owner's hand."
// Nexus says "Creatures you control are every creature type."
// The face-down Initiate is an eligible Bird and may pay its morph cost by returning
// itself to hand. Upon leaving the battlefield it becomes a new object (CR 400.7) and
// reveals its identity (CR 708.9). The instruction to turn the departed permanent face up
// is impossible and thus ignored (CR 101.3). Cost payment finishes cleanly without flipping
// and restores priority.
#[test]
fn raven_guild_initiate_self_return_with_maskwood_nexus_settles_without_flip() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_artifact_from_oracle(
        P0,
        "Maskwood Nexus",
        "Creatures you control are every creature type. The same is true for creature spells you control and creature cards you own that aren't on the battlefield.",
    );

    let initiate = scenario
        .add_creature_to_hand(P0, "Raven Guild Initiate", 1, 4)
        .from_oracle_text("Morph—Return a Bird you control to its owner's hand.")
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Blue],
        })
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                initiate,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                initiate,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                initiate,
                false,
                vec![],
            ),
        ],
    );

    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec![
        "Bird".to_string(),
        "Human".to_string(),
        "Wizard".to_string(),
    ];
    engine::game::layers::evaluate_layers(runner.state_mut());
    cast_face_down(&mut runner, initiate);

    // Turn face up
    let res = runner.act(GameAction::TurnFaceUp {
        object_id: initiate,
        x: 0,
    });
    assert!(res.is_ok(), "turn face up should succeed: {:?}", res.err());

    // Prompted for PayCost::ReturnToHand including the face-down initiate itself
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::ReturnToHand,
                choices,
                count: 1,
                ..
            } if choices.contains(&initiate)
        ),
        "must prompt for ReturnToHand including initiate itself (via Maskwood Nexus), got {:?}",
        runner.state().waiting_for
    );

    // CR 702.37e + CR 400.7 + CR 101.3: Pay by returning the Initiate itself to hand.
    let outcome = runner
        .act(GameAction::SelectCards {
            cards: vec![initiate],
        })
        .expect("selecting initiate as payment should succeed");

    // Costs paid: Initiate is now in Hand
    assert_eq!(runner.state().objects[&initiate].zone, Zone::Hand);
    // CR 708.9: Restored face_down to false upon leaving the battlefield
    assert!(!runner.state().objects[&initiate].face_down);

    // No TurnedFaceUp event emitted
    assert!(
        !outcome
            .events
            .iter()
            .any(|e| matches!(e, engine::types::events::GameEvent::TurnedFaceUp { .. })),
        "TurnedFaceUp must NOT be emitted when original permanent left the battlefield during payment"
    );

    // Cleared roots
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(runner.state().pending_deferred_life_cost_resume.is_none());

    // Restored priority to P0
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
}

#[test]
fn raven_guild_initiate_other_bird_payment_control_turns_face_up() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_artifact_from_oracle(
        P0,
        "Maskwood Nexus",
        "Creatures you control are every creature type. The same is true for creature spells you control and creature cards you own that aren't on the battlefield.",
    );

    let other_bird = scenario.add_creature(P0, "Storm Crow", 1, 2).id();

    let initiate = scenario
        .add_creature_to_hand(P0, "Raven Guild Initiate", 1, 4)
        .from_oracle_text("Morph—Return a Bird you control to its owner's hand.")
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Blue],
        })
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                initiate,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                initiate,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                initiate,
                false,
                vec![],
            ),
        ],
    );

    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec![
        "Bird".to_string(),
        "Human".to_string(),
        "Wizard".to_string(),
    ];
    engine::game::layers::evaluate_layers(runner.state_mut());
    cast_face_down(&mut runner, initiate);

    let res = runner.act(GameAction::TurnFaceUp {
        object_id: initiate,
        x: 0,
    });
    assert!(res.is_ok(), "turn face up should succeed: {:?}", res.err());

    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::ReturnToHand,
                choices,
                count: 1,
                ..
            } if choices.contains(&other_bird) && choices.contains(&initiate)
        ),
        "must prompt for ReturnToHand with both other_bird and initiate eligible, got {:?}",
        runner.state().waiting_for
    );

    let outcome = runner
        .act(GameAction::SelectCards {
            cards: vec![other_bird],
        })
        .expect("selecting other_bird as payment should succeed");

    // other_bird moved to Hand
    assert_eq!(runner.state().objects[&other_bird].zone, Zone::Hand);

    // initiate remains on Battlefield and is turned face up
    assert_eq!(runner.state().objects[&initiate].zone, Zone::Battlefield);
    assert!(!runner.state().objects[&initiate].face_down);
    assert_eq!(
        runner.state().objects[&initiate].name,
        "Raven Guild Initiate"
    );

    // TurnedFaceUp event WAS emitted for initiate
    assert!(
        outcome
            .events
            .iter()
            .any(|e| matches!(e, engine::types::events::GameEvent::TurnedFaceUp { object_id, .. } if *object_id == initiate)),
        "TurnedFaceUp MUST be emitted for initiate"
    );

    // Cleared roots and restored priority
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(runner.state().pending_deferred_life_cost_resume.is_none());
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
}

const SAFFI_ERIKSDOTTER_ORACLE: &str =
    "Sacrifice Saffi Eriksdotter: When target creature is put into your graveyard this turn, return that card to the battlefield.";

// CR 702.37e + CR 603.2 + CR 603.7a-c: Preserve delayed-observer triggers across morph cost pauses.
// When Saffi Eriksdotter's delayed return ability is active on a creature sacrificed to pay Gift of Doom's
// morph cost, an interactive delivery-child replacement pause preserves the delayed trigger in deferred_triggers,
// and upon completion the delayed ability triggers, reaches the stack, and returns the creature to the battlefield.
#[test]
fn turn_face_up_sacrifice_cost_preserves_delayed_triggers_across_child_pause() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ChoiceType, ControllerRef, Effect, ReplacementDefinition,
        TargetFilter, TargetSelectionMode,
    };
    use engine::types::game_state::PendingCostMoveResume;
    use engine::types::replacements::ReplacementEvent;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let saffi = scenario
        .add_creature_from_oracle(P0, "Saffi Eriksdotter", 2, 2, SAFFI_ERIKSDOTTER_ORACLE)
        .id();
    let victim = scenario.add_creature(P0, "Doomed Bear", 2, 2).id();
    let gift = scenario
        .add_creature_to_hand(P0, "Gift of Doom", 1, 1)
        .from_oracle_text("Morph—Sacrifice another creature.")
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Black],
        })
        .id();

    // Colorless mana to cast Gift of Doom face-down
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                gift,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                gift,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                gift,
                false,
                vec![],
            ),
        ],
    );

    let mut runner = scenario.build();

    // Cast Gift of Doom face down
    let card_id = runner.state().objects[&gift].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: gift,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast face down");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("choose face down");
    }
    runner.resolve_top();
    assert!(runner.state().objects[&gift].face_down);

    // Activate Saffi targeting victim and resolve it
    let activation = runner.activate(saffi, 0).target_object(victim).resolve();
    assert_eq!(
        activation.zone_of(saffi),
        Zone::Graveyard,
        "reach-guard: activating Saffi sacrifices her"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: Saffi's activation must install exactly one delayed trigger"
    );

    // Install delivery-child replacement effect on victim (moved to graveyard -> choose odd or even)
    let delivery_repl = ReplacementDefinition::new(ReplacementEvent::Moved)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Choose {
                choice_type: ChoiceType::OddOrEven,
                chooser: ControllerRef::You,
                persist: false,
                selection: TargetSelectionMode::Chosen,
            },
        ))
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(Zone::Graveyard);

    std::sync::Arc::make_mut(
        &mut runner
            .state_mut()
            .objects
            .get_mut(&victim)
            .unwrap()
            .base_replacement_definitions,
    )
    .push(delivery_repl.clone());
    runner
        .state_mut()
        .objects
        .get_mut(&victim)
        .unwrap()
        .replacement_definitions
        .push(delivery_repl);

    // Public TurnFaceUp special action for Gift of Doom
    let res = runner.act(GameAction::TurnFaceUp {
        object_id: gift,
        x: 0,
    });
    assert!(res.is_ok(), "TurnFaceUp must succeed: {:?}", res.err());
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ),
        "must be WaitingFor::PayCost(Sacrifice), got {:?}",
        runner.state().waiting_for
    );

    // Submit SelectCards sacrificing the victim
    let res1 = runner.act(GameAction::SelectCards {
        cards: vec![victim],
    });
    assert!(res1.is_ok(), "SelectCards must succeed: {:?}", res1.err());

    // Victim moved to graveyard; delivery tail paused on NamedChoice
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::OddOrEven,
                ..
            }
        ),
        "must be on victim's NamedChoice prompt, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().objects[&gift].face_down,
        "Gift of Doom must remain face down while payment is paused"
    );
    assert!(
        matches!(
            runner.state().pending_cost_move_resume,
            Some(PendingCostMoveResume::TurnFaceUpCostMove {
                paused_at_index: 0,
                ..
            })
        ),
        "must have TurnFaceUpCostMove paused at index 0"
    );

    // CR 603.2 + CR 603.7c: Saffi's delayed return trigger must be retained in deferred_triggers
    assert_eq!(
        runner.state().deferred_triggers.len(),
        1,
        "must retain delayed return trigger in deferred_triggers across cost pause"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        0,
        "one-shot delayed trigger has fired and is no longer pending in delayed_triggers"
    );

    // Answer the child prompt
    let res2 = runner.act(GameAction::ChooseOption {
        choice: "Odd".to_string(),
    });
    assert!(res2.is_ok(), "ChooseOption must succeed: {:?}", res2.err());

    // Flip committed: Gift of Doom is face up
    assert!(
        !runner.state().objects[&gift].face_down,
        "Gift of Doom must be turned face up after cost and child finish"
    );
    // Cost root cleared
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "pending_cost_move_resume must be cleared"
    );

    // Saffi's delayed trigger has drained to the stack
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Saffi's delayed return trigger must be on the stack"
    );

    // Resolve Saffi's delayed return trigger
    runner.resolve_top();

    // Victim returned to battlefield!
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "victim must be returned to the battlefield by Saffi's delayed trigger"
    );

    // At Priority
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "state must settle at Priority, got {:?}",
        runner.state().waiting_for
    );
}

// CR 702.37e + CR 603.2 + CR 603.7a-c: Paired unpaused control: Gift of Doom sacrificing
// a Saffi-protected creature without delivery-child replacement effects immediately triggers
// the delayed return ability.
#[test]
fn turn_face_up_sacrifice_cost_unpaused_control_preserves_delayed_triggers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let saffi = scenario
        .add_creature_from_oracle(P0, "Saffi Eriksdotter", 2, 2, SAFFI_ERIKSDOTTER_ORACLE)
        .id();
    let victim = scenario.add_creature(P0, "Doomed Bear", 2, 2).id();
    let gift = scenario
        .add_creature_to_hand(P0, "Gift of Doom", 1, 1)
        .from_oracle_text("Morph—Sacrifice another creature.")
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Black],
        })
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                gift,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                gift,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                gift,
                false,
                vec![],
            ),
        ],
    );

    let mut runner = scenario.build();

    let card_id = runner.state().objects[&gift].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: gift,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast face down");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("choose face down");
    }
    runner.resolve_top();
    assert!(runner.state().objects[&gift].face_down);

    let activation = runner.activate(saffi, 0).target_object(victim).resolve();
    assert_eq!(activation.zone_of(saffi), Zone::Graveyard);
    assert_eq!(runner.state().delayed_triggers.len(), 1);

    let res = runner.act(GameAction::TurnFaceUp {
        object_id: gift,
        x: 0,
    });
    assert!(res.is_ok(), "TurnFaceUp must succeed");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost {
            kind: PayCostKind::Sacrifice,
            ..
        }
    ));

    let res1 = runner.act(GameAction::SelectCards {
        cards: vec![victim],
    });
    assert!(res1.is_ok(), "SelectCards must succeed");

    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert!(!runner.state().objects[&gift].face_down);
    assert!(runner.state().pending_cost_move_resume.is_none());

    assert_eq!(
        runner.state().stack.len(),
        1,
        "Saffi's delayed return trigger must be on the stack"
    );

    runner.resolve_top();

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "victim must be returned to the battlefield"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

// CR 702.37e + CR 603.2 + CR 603.3b: Preserving delivered event prefix across resumed morph payment
// that pauses again. When paying Skirk Volcanist's morph cost (Sacrifice two Mountains) with Mayhem Devil
// on the battlefield, the first sacrifice pauses for an optional replacement redirect (Graveyard -> Exile).
// Accepting the first replacement delivers the first sacrifice and resumes payment, which moves the second
// Mountain and pauses for the second replacement redirect.
// At this intermediate prompt, the first sacrifice must ALREADY have a retained Mayhem Devil observer.
// Completing both sacrifices must produce exactly two Mayhem Devil triggers.
#[test]
fn turn_face_up_sacrifice_preserves_delivered_observer_prefix_across_second_replacement_pause() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, ReplacementMode, TargetFilter, TargetRef,
    };
    use engine::types::game_state::PendingCostMoveResume;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::zones::EtbTapState;
    use engine::types::ReplacementDefinition;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mountain1 = scenario.add_basic_land(P0, ManaColor::Red);
    let mountain2 = scenario.add_basic_land(P0, ManaColor::Red);
    let mayhem_devil = scenario
        .add_creature(P0, "Mayhem Devil", 3, 3)
        .from_oracle_text(
            "Whenever a player sacrifices a permanent, Mayhem Devil deals 1 damage to any target.",
        )
        .id();
    let volcanist = scenario
        .add_creature_to_hand(P0, "Skirk Volcanist", 3, 1)
        .from_oracle_text("Morph—Sacrifice two Mountains.")
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Red],
        })
        .id();

    // Optional replacement redirect for graveyard moves (exile instead)
    scenario
        .add_creature(P1, "Redirector", 1, 1)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .destination_zone(Zone::Graveyard)
                .mode(ReplacementMode::Optional { decline: None })
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        origin: None,
                        destination: Zone::Exile,
                        target: TargetFilter::SelfRef,
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: EtbTapState::Unspecified,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                        conditional_enter_with_counters: vec![],
                        enters_modified_if: None,
                        face_down_profile: None,
                    },
                )),
        );

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
        ],
    );

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&volcanist].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: volcanist,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast face down");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("choose face down");
    }
    runner.resolve_top();
    assert!(runner.state().objects[&volcanist].face_down);

    runner
        .act(GameAction::TurnFaceUp {
            object_id: volcanist,
            x: 0,
        })
        .expect("turn face up");

    runner
        .act(GameAction::SelectCards {
            cards: vec![mountain1, mountain2],
        })
        .expect("select mountains");

    // Pauses for optional replacement on first mountain
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "expected ReplacementChoice pause on first mountain, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().deferred_triggers.len(), 0);

    // Accept replacement for first mountain (exiles instead of graveyard)
    let _res1 = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("choose replacement for first mountain");

    // First mountain moved to exile; its sacrifice event delivered before second mountain paused.
    assert_eq!(runner.state().objects[&mountain1].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&mountain2].zone, Zone::Battlefield);

    // Second mountain must pause on ReplacementChoice
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "expected second ReplacementChoice pause on second mountain, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(
            runner.state().pending_cost_move_resume,
            Some(PendingCostMoveResume::TurnFaceUpCostMove {
                paused_at_index: 1,
                ..
            })
        ),
        "TurnFaceUpCostMove must be paused at index 1"
    );

    // CR 603.2c + CR 603.10a discriminator: both Mountains leave together, so the
    // first sacrifice's events are withheld in the payment ledger (not collected
    // as their own event) until the second sacrifice completes the group.
    assert!(
        runner.state().deferred_triggers.is_empty(),
        "no observer may be collected for a partial departure group"
    );
    assert!(
        matches!(
            &runner.state().pending_cost_move_resume,
            Some(PendingCostMoveResume::TurnFaceUpCostMove {
                deferred_cost_events,
                ..
            }) if !deferred_cost_events.is_empty()
        ),
        "first sacrifice's events must be retained in the payment ledger across the second pause"
    );

    // Accept replacement for second mountain (exiles instead of graveyard)
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("choose replacement for second mountain");

    assert_eq!(runner.state().objects[&mountain2].zone, Zone::Exile);
    assert!(!runner.state().objects[&volcanist].face_down);
    assert!(runner.state().pending_cost_move_resume.is_none());

    // CR 603.2 + CR 603.3b: Completing both sacrifices must produce exactly two Mayhem Devil triggers
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "must prompt for first Mayhem Devil trigger target, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P1)],
        })
        .expect("select target for first Mayhem Devil trigger");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "must prompt for second Mayhem Devil trigger target, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P1)],
        })
        .expect("select target for second Mayhem Devil trigger");

    assert_eq!(
        runner.state().stack.len(),
        2,
        "must have exactly two Mayhem Devil triggers on the stack"
    );
    for entry in &runner.state().stack {
        assert_eq!(entry.source_id, mayhem_devil);
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

// CR 702.37e + CR 603.2 + CR 603.7a-c: Preserving delivered delayed-observer trigger prefix
// across resumed morph payment that pauses again.
// When Saffi Eriksdotter's delayed return ability is active on a creature sacrificed to pay Skirk Volcanist's
// morph cost (Sacrifice two Mountains), the first creature (a Mountain creature) pauses for an optional
// replacement redirect (Graveyard -> Exile). The player declines the redirect (index 1), so the creature is
// put into the graveyard, firing Saffi's delayed trigger.
// Payment resumes and moves the second Mountain creature, pausing for the second replacement redirect.
// At this intermediate prompt, Saffi's delayed return trigger must ALREADY be retained in deferred_triggers.
// Answering the second replacement prompt completes payment and flips the permanent, and Saffi's delayed
// trigger drains to the stack and returns the first creature to the battlefield.
#[test]
fn turn_face_up_sacrifice_preserves_delivered_delayed_trigger_prefix_across_second_replacement_pause(
) {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, ReplacementMode, TargetFilter,
    };
    use engine::types::game_state::PendingCostMoveResume;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::zones::EtbTapState;
    use engine::types::ReplacementDefinition;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let saffi = scenario
        .add_creature_from_oracle(P0, "Saffi Eriksdotter", 2, 2, SAFFI_ERIKSDOTTER_ORACLE)
        .id();

    // Two Mountain creatures eligible for Skirk Volcanist's "Sacrifice two Mountains" cost
    let mountain_creature1 = scenario
        .add_creature(P0, "Mountain Dryad 1", 1, 1)
        .with_subtypes(vec!["Mountain"])
        .id();
    let mountain_creature2 = scenario
        .add_creature(P0, "Mountain Dryad 2", 1, 1)
        .with_subtypes(vec!["Mountain"])
        .id();

    let volcanist = scenario
        .add_creature_to_hand(P0, "Skirk Volcanist", 3, 1)
        .from_oracle_text("Morph—Sacrifice two Mountains.")
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Red],
        })
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
            ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                volcanist,
                false,
                vec![],
            ),
        ],
    );

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&volcanist].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: volcanist,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast face down");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("choose face down");
    }
    runner.resolve_top();
    assert!(runner.state().objects[&volcanist].face_down);

    // Activate Saffi targeting mountain_creature1 and resolve it
    let activation = runner
        .activate(saffi, 0)
        .target_object(mountain_creature1)
        .resolve();
    assert_eq!(
        activation.zone_of(saffi),
        Zone::Graveyard,
        "reach-guard: activating Saffi sacrifices her"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: Saffi's activation must install exactly one delayed trigger"
    );

    // Install optional replacement redirect on both Mountain creatures (Graveyard -> Exile)
    let repl = ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .mode(ReplacementMode::Optional { decline: None })
        .valid_card(TargetFilter::SelfRef)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                enters_modified_if: None,
                face_down_profile: None,
            },
        ));

    std::sync::Arc::make_mut(
        &mut runner
            .state_mut()
            .objects
            .get_mut(&mountain_creature1)
            .unwrap()
            .base_replacement_definitions,
    )
    .push(repl.clone());
    runner
        .state_mut()
        .objects
        .get_mut(&mountain_creature1)
        .unwrap()
        .replacement_definitions
        .push(repl.clone());

    std::sync::Arc::make_mut(
        &mut runner
            .state_mut()
            .objects
            .get_mut(&mountain_creature2)
            .unwrap()
            .base_replacement_definitions,
    )
    .push(repl.clone());
    runner
        .state_mut()
        .objects
        .get_mut(&mountain_creature2)
        .unwrap()
        .replacement_definitions
        .push(repl);

    // Turn Skirk Volcanist face up
    runner
        .act(GameAction::TurnFaceUp {
            object_id: volcanist,
            x: 0,
        })
        .expect("turn face up");

    runner
        .act(GameAction::SelectCards {
            cards: vec![mountain_creature1, mountain_creature2],
        })
        .expect("select mountain creatures");

    // First mountain creature pauses on optional ReplacementChoice
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "expected ReplacementChoice pause on first mountain creature, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().deferred_triggers.len(), 0);

    // Decline replacement for first mountain creature (index 1: decline, moves to Graveyard)
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline replacement for first mountain creature");

    assert_eq!(
        runner.state().objects[&mountain_creature1].zone,
        Zone::Graveyard
    );
    assert_eq!(
        runner.state().objects[&mountain_creature2].zone,
        Zone::Battlefield
    );

    // Second mountain creature must pause on ReplacementChoice
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "expected second ReplacementChoice pause on second mountain creature, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(
            runner.state().pending_cost_move_resume,
            Some(PendingCostMoveResume::TurnFaceUpCostMove {
                paused_at_index: 1,
                ..
            })
        ),
        "TurnFaceUpCostMove must be paused at index 1"
    );

    // CR 603.2c + CR 603.7c + CR 603.10a discriminator: the first death is withheld
    // in the payment ledger until the departure group completes, so Saffi's delayed
    // trigger is not collected from a partial group.
    assert!(
        runner.state().deferred_triggers.is_empty(),
        "no delayed trigger may be collected for a partial departure group"
    );
    assert!(
        matches!(
            &runner.state().pending_cost_move_resume,
            Some(PendingCostMoveResume::TurnFaceUpCostMove {
                deferred_cost_events,
                ..
            }) if !deferred_cost_events.is_empty()
        ),
        "the first death must be retained in the payment ledger across the second pause"
    );

    // Accept replacement for second mountain creature (exiles it)
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("choose replacement for second mountain creature");

    assert_eq!(
        runner.state().objects[&mountain_creature2].zone,
        Zone::Exile
    );
    assert!(!runner.state().objects[&volcanist].face_down);
    assert!(runner.state().pending_cost_move_resume.is_none());

    // Saffi's delayed return trigger has drained to the stack
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Saffi's delayed return trigger must be on the stack"
    );

    // Resolve Saffi's delayed return trigger
    runner.resolve_top();

    // mountain_creature1 is returned to the battlefield
    assert_eq!(
        runner.state().objects[&mountain_creature1].zone,
        Zone::Battlefield,
        "mountain_creature1 must be returned to the battlefield by Saffi's delayed trigger"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// Skirk Volcanist ("Morph—Sacrifice two Mountains") with two Mountain
/// creatures: Blood Artist and a Mountain Goblin, watched by an opponent's
/// Chainsaw ("Whenever one or more creatures die, put a rev counter on this
/// Equipment."). `optional_graveyard_redirect` adds an optional Graveyard
/// replacement so each sacrifice pauses for a choice.
fn blood_artist_co_departure_runner(
    optional_graveyard_redirect: bool,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::ObjectId,
    engine::types::ObjectId,
    engine::types::ObjectId,
    engine::types::ObjectId,
) {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, ReplacementMode, TargetFilter,
    };
    use engine::types::replacements::ReplacementEvent;
    use engine::types::zones::EtbTapState;
    use engine::types::ReplacementDefinition;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let blood_artist = scenario
        .add_creature(P0, "Blood Artist", 0, 1)
        .from_oracle_text(
            "Whenever Blood Artist or another creature dies, target player loses 1 life and you gain 1 life.",
        )
        .with_subtypes(vec!["Mountain"])
        .id();
    let goblin = scenario
        .add_creature(P0, "Mountain Goblin", 1, 1)
        .with_subtypes(vec!["Mountain"])
        .id();
    // CR 603.2c: a batch observer controlled by the opponent, so its trigger
    // never shares an ordering prompt with Blood Artist's.
    let chainsaw = scenario
        .add_artifact_from_oracle(
            P1,
            "Chainsaw",
            "Whenever one or more creatures die, put a rev counter on this Equipment.",
        )
        .id();
    let volcanist = scenario
        .add_creature_to_hand(P0, "Skirk Volcanist", 3, 1)
        .from_oracle_text("Morph—Sacrifice two Mountains.")
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    if optional_graveyard_redirect {
        scenario
            .add_creature(P1, "Redirector", 1, 1)
            .with_replacement_definition(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .destination_zone(Zone::Graveyard)
                    .mode(ReplacementMode::Optional { decline: None })
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChangeZone {
                            origin: None,
                            destination: Zone::Exile,
                            target: TargetFilter::SelfRef,
                            owner_library: false,
                            enter_transformed: false,
                            enters_under: None,
                            enter_tapped: EtbTapState::Unspecified,
                            enters_attacking: false,
                            up_to: false,
                            enter_with_counters: vec![],
                            conditional_enter_with_counters: vec![],
                            enters_modified_if: None,
                            face_down_profile: None,
                        },
                    )),
            );
    }
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| {
                ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    volcanist,
                    false,
                    vec![],
                )
            })
            .collect(),
    );
    let mut runner = scenario.build();
    cast_face_down(&mut runner, volcanist);
    runner
        .act(GameAction::TurnFaceUp {
            object_id: volcanist,
            x: 0,
        })
        .expect("turn face up");
    (runner, volcanist, blood_artist, goblin, chainsaw)
}

/// Answer every Blood Artist target prompt, then count the stacked triggers
/// whose source is `source`.
fn co_departure_trigger_count(
    runner: &mut engine::game::scenario::GameRunner,
    source: engine::types::ObjectId,
) -> usize {
    while matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("select Blood Artist target");
    }
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == source)
        .count()
}

// CR 603.10a + CR 702.37e: A look-back observer paid as part of the same
// "Sacrifice two Mountains" cost leaves the battlefield simultaneously with the
// other paid permanent, so Blood Artist sees both deaths.
#[test]
fn turn_face_up_sacrifice_cost_co_departing_observer_sees_both_deaths() {
    let (mut runner, volcanist, blood_artist, goblin, chainsaw) =
        blood_artist_co_departure_runner(false);
    runner
        .act(GameAction::SelectCards {
            cards: vec![goblin, blood_artist],
        })
        .expect("sacrifice both Mountains");
    assert!(!runner.state().objects[&volcanist].face_down);
    assert_eq!(runner.state().objects[&goblin].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&blood_artist].zone, Zone::Graveyard);
    assert_eq!(
        co_departure_trigger_count(&mut runner, blood_artist),
        2,
        "Blood Artist must trigger for its own death and the co-departing creature"
    );
    // CR 603.2c: both deaths are one event, so Chainsaw triggers exactly once.
    assert_eq!(
        co_departure_trigger_count(&mut runner, chainsaw),
        1,
        "Chainsaw must trigger once for the single simultaneous death event"
    );
}

// CR 603.10a + CR 614.6 + CR 616.1: The same simultaneous departure survives a
// replacement-choice pause between the two sacrifices.
#[test]
fn turn_face_up_sacrifice_cost_co_departing_observer_survives_replacement_pause() {
    use engine::types::game_state::PendingCostMoveResume;

    let (mut runner, volcanist, blood_artist, goblin, chainsaw) =
        blood_artist_co_departure_runner(true);
    runner
        .act(GameAction::SelectCards {
            cards: vec![blood_artist, goblin],
        })
        .expect("sacrifice both Mountains");
    let mut pauses = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        pauses += 1;
        if pauses == 2 {
            // CR 603.2c + CR 603.10a: the first death waits in the payment's
            // ledger rather than being collected as its own event.
            assert_eq!(runner.state().objects[&blood_artist].zone, Zone::Graveyard);
            assert!(
                runner.state().deferred_triggers.is_empty(),
                "no trigger may be collected for a partial departure group"
            );
            assert!(
                matches!(
                    &runner.state().pending_cost_move_resume,
                    Some(PendingCostMoveResume::TurnFaceUpCostMove {
                        deferred_cost_events,
                        ..
                    }) if !deferred_cost_events.is_empty()
                ),
                "the first sacrifice's events must be withheld in the payment ledger"
            );
        }
        // Decline the optional exile redirect so both creatures die.
        runner
            .act(GameAction::ChooseReplacement { index: 1 })
            .expect("decline replacement");
    }
    assert_eq!(pauses, 2, "each sacrifice must pause for the replacement");
    assert!(!runner.state().objects[&volcanist].face_down);
    assert_eq!(runner.state().objects[&goblin].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&blood_artist].zone, Zone::Graveyard);
    assert_eq!(
        co_departure_trigger_count(&mut runner, blood_artist),
        2,
        "Blood Artist must trigger for its own death and the co-departing creature"
    );
    // CR 603.2c: the pause splits the payment across actions, not the event;
    // Chainsaw triggers exactly once.
    assert_eq!(
        co_departure_trigger_count(&mut runner, chainsaw),
        1,
        "Chainsaw must trigger once for the single simultaneous death event"
    );
}

// CR 702.37e: Turning a permanent face up only requires paying its morph cost;
// nothing excludes the face-down permanent itself from paying it. Under Ashaya,
// Soul of the Wild + Prismatic Omen the face-down Skirk Volcanist is itself a
// Mountain, so "Sacrifice two Mountains" (no "another") may sacrifice it. The
// permanent leaves during payment, so the action settles without a flip.
#[test]
fn skirk_volcanist_may_sacrifice_itself_for_its_morph_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "Ashaya, Soul of the Wild",
        0,
        0,
        "Ashaya, Soul of the Wild's power and toughness are each equal to the number of lands you control.\nNontoken creatures you control are Forest lands in addition to their other types.",
    );
    scenario.add_enchantment_from_oracle(
        P0,
        "Prismatic Omen",
        "Lands you control are every basic land type in addition to their other types.",
    );
    let goblin = scenario.add_creature(P0, "Goblin", 1, 1).id();
    let volcanist = scenario
        .add_creature_to_hand(P0, "Skirk Volcanist", 3, 1)
        .from_oracle_text("Morph—Sacrifice two Mountains.")
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| {
                ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    volcanist,
                    false,
                    vec![],
                )
            })
            .collect(),
    );
    let mut runner = scenario.build();
    cast_face_down(&mut runner, volcanist);

    runner
        .act(GameAction::TurnFaceUp {
            object_id: volcanist,
            x: 0,
        })
        .expect("turn face up must be offered");
    match &runner.state().waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::Sacrifice,
            choices,
            count: 2,
            ..
        } => {
            assert!(
                choices.contains(&volcanist),
                "the face-down Mountain must be able to pay its own morph cost"
            );
            assert!(choices.contains(&goblin));
        }
        other => panic!("expected PayCost(Sacrifice) for two Mountains, got {other:?}"),
    }

    let outcome = runner
        .act(GameAction::SelectCards {
            cards: vec![volcanist, goblin],
        })
        .expect("sacrificing the face-down permanent itself is a legal payment");
    assert_eq!(runner.state().objects[&goblin].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&volcanist].zone, Zone::Graveyard);
    assert!(
        outcome.events.iter().any(|e| matches!(
            e,
            engine::types::events::GameEvent::PermanentSacrificed { object_id, .. }
                if *object_id == volcanist
        )),
        "the face-down permanent must be sacrificed as part of the payment"
    );
    assert!(
        !outcome
            .events
            .iter()
            .any(|e| matches!(e, engine::types::events::GameEvent::TurnedFaceUp { .. })),
        "a permanent that left during payment is not turned face up"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
}
