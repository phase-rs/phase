use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;

const SIFT_THROUGH_SANDS_ORACLE: &str =
    "Draw two cards, then discard a card.\nIf you've cast a spell named Peer Through Depths and a spell named Reach Through Mists this turn, you may search your library for a card named The Unspeakable, put it onto the battlefield, then shuffle.";

#[test]
fn sift_through_sands_draws_two_then_discards_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Sift Through Sands", true, SIFT_THROUGH_SANDS_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.add_card_to_library_top(P0, "Card 1");
    scenario.add_card_to_library_top(P0, "Card 2");
    scenario.add_card_to_library_top(P0, "Card 3");
    let mut runner = scenario.build();

    let committed = runner.cast(spell).commit();
    let outcome = committed.resolve();

    println!("Events during Sift Through Sands resolution:");
    for (i, ev) in outcome.events().iter().enumerate() {
        println!("  Event {}: {:?}", i, ev);
    }
    println!("Final waiting for: {:?}", outcome.final_waiting_for());

    let chosen_card = match outcome.final_waiting_for() {
        WaitingFor::DiscardChoice { cards, count, .. } => {
            assert_eq!(*count, 1, "Must ask to discard 1 card");
            assert_eq!(cards.len(), 2, "Hand must contain the 2 drawn cards");
            cards[0]
        }
        other => panic!("Expected WaitingFor::DiscardChoice, got {:?}", other),
    };

    let outcome2 = runner
        .act(engine::types::actions::GameAction::SelectCards {
            cards: vec![chosen_card],
        })
        .unwrap();
    println!("Outcome 2 waiting for: {:?}", outcome2.waiting_for);
    println!(
        "Final P0 hand after discard: {:?}",
        runner.state().players[P0.0 as usize].hand
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        1,
        "Hand must have 1 card after drawing 2 and discarding 1"
    );
}

const SIFT_THROUGH_SANDS_USER_ORACLE: &str =
    "Draw two cards, then discard a card.\n\nIf you’ve cast a spell named Peer Through Depths and a spell named Reach Through Mists this turn, you may search your library for a card named The Unspeakable, put it onto the battlefield, then shuffle.";

const SIFT_THROUGH_SANDS_SINGLE_NL_CURLY: &str =
    "Draw two cards, then discard a card.\nIf you’ve cast a spell named Peer Through Depths and a spell named Reach Through Mists this turn, you may search your library for a card named The Unspeakable, put it onto the battlefield, then shuffle.";

fn all_effects(
    parsed: &engine::parser::oracle::ParsedAbilities,
) -> Vec<engine::types::ability::Effect> {
    fn walk(
        def: &engine::types::ability::AbilityDefinition,
        out: &mut Vec<engine::types::ability::Effect>,
    ) {
        out.push((*def.effect).clone());
        if let Some(sub) = def.sub_ability.as_deref() {
            walk(sub, out);
        }
    }
    let mut out = Vec::new();
    for ability in &parsed.abilities {
        walk(ability, &mut out);
    }
    for trigger in &parsed.triggers {
        if let Some(execute) = trigger.execute.as_deref() {
            walk(execute, &mut out);
        }
    }
    out
}

fn assert_no_unimplemented(parsed: &engine::parser::oracle::ParsedAbilities) {
    for effect in all_effects(parsed) {
        assert!(
            !matches!(effect, engine::types::ability::Effect::Unimplemented { .. }),
            "no clause may parse to Effect::Unimplemented, found {effect:?}"
        );
    }
}

fn assert_has_named_spell_cast_condition(
    conditions: &[engine::types::ability::AbilityCondition],
    expected_name: &str,
) {
    assert!(
        conditions.iter().any(|c| match c {
            engine::types::ability::AbilityCondition::QuantityCheck {
                lhs:
                    engine::types::ability::QuantityExpr::Ref {
                        qty:
                            engine::types::ability::QuantityRef::SpellsCastThisTurn {
                                scope: engine::types::ability::CountScope::Controller,
                                filter: Some(engine::types::ability::TargetFilter::Typed(typed)),
                            },
                    },
                comparator: engine::types::ability::Comparator::GE,
                rhs: engine::types::ability::QuantityExpr::Fixed { value: 1 },
            } => typed.properties.iter().any(|p| match p {
                engine::types::ability::FilterProp::Named { name } => {
                    name.eq_ignore_ascii_case(expected_name)
                }
                _ => false,
            }),
            _ => false,
        }),
        "Expected condition with CountScope::Controller for spell named {expected_name:?}, got {conditions:?}"
    );
}

fn assert_sift_through_sands_single_nl_ast(parsed: &engine::parser::oracle::ParsedAbilities) {
    assert_eq!(
        parsed.abilities.len(),
        1,
        "Single newline must parse as 1 chained ability"
    );
    assert_no_unimplemented(parsed);

    let main = &parsed.abilities[0];
    assert!(
        matches!(
            main.effect.as_ref(),
            engine::types::ability::Effect::Draw {
                count: engine::types::ability::QuantityExpr::Fixed { value: 2 },
                target: engine::types::ability::TargetFilter::Controller,
            }
        ),
        "Main effect must be Draw 2 for controller, got {:?}",
        main.effect
    );

    let discard_sub = main
        .sub_ability
        .as_ref()
        .expect("Draw 2 must chain to Discard 1");
    assert!(
        matches!(
            discard_sub.effect.as_ref(),
            engine::types::ability::Effect::Discard {
                count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
                ..
            }
        ),
        "First sub-ability must be Discard 1 for controller, got {:?}",
        discard_sub.effect
    );

    let search_sub = discard_sub
        .sub_ability
        .as_ref()
        .expect("Discard must chain to SearchLibrary");
    assert!(
        search_sub.optional,
        "SearchLibrary must be marked optional ('you may')"
    );
    assert!(
        matches!(
            search_sub.effect.as_ref(),
            engine::types::ability::Effect::SearchLibrary {
                count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                reveal: false,
                ..
            }
        ),
        "SearchLibrary effect mismatch, got {:?}",
        search_sub.effect
    );
    let conditions = match search_sub.condition.as_ref() {
        Some(engine::types::ability::AbilityCondition::And { conditions }) => {
            assert_eq!(
                conditions.len(),
                2,
                "SearchLibrary must have compound And condition for 2 prior spells"
            );
            conditions
        }
        other => panic!(
            "SearchLibrary must have compound And condition for prior spells, got {:?}",
            other
        ),
    };
    assert_has_named_spell_cast_condition(conditions, "peer through depths");
    assert_has_named_spell_cast_condition(conditions, "reach through mists");
}

fn assert_sift_through_sands_user_oracle_ast(parsed: &engine::parser::oracle::ParsedAbilities) {
    assert_eq!(
        parsed.abilities.len(),
        2,
        "Double newline must parse as 2 sequential abilities"
    );
    assert_no_unimplemented(parsed);

    let ability0 = &parsed.abilities[0];
    assert!(
        matches!(
            ability0.effect.as_ref(),
            engine::types::ability::Effect::Draw {
                count: engine::types::ability::QuantityExpr::Fixed { value: 2 },
                target: engine::types::ability::TargetFilter::Controller,
            }
        ),
        "Ability 0 effect must be Draw 2 for controller, got {:?}",
        ability0.effect
    );

    let discard_sub = ability0
        .sub_ability
        .as_ref()
        .expect("Ability 0 must chain to Discard 1");
    assert!(
        matches!(
            discard_sub.effect.as_ref(),
            engine::types::ability::Effect::Discard {
                count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
                ..
            }
        ),
        "Ability 0 sub-ability must be Discard 1 for controller, got {:?}",
        discard_sub.effect
    );

    let ability1 = &parsed.abilities[1];
    assert!(
        ability1.optional,
        "Ability 1 (SearchLibrary) must be marked optional ('you may')"
    );
    assert!(
        matches!(
            ability1.effect.as_ref(),
            engine::types::ability::Effect::SearchLibrary {
                count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                reveal: false,
                ..
            }
        ),
        "Ability 1 SearchLibrary effect mismatch, got {:?}",
        ability1.effect
    );
    let conditions = match ability1.condition.as_ref() {
        Some(engine::types::ability::AbilityCondition::And { conditions }) => {
            assert_eq!(
                conditions.len(),
                2,
                "Ability 1 must have compound And condition for 2 prior spells"
            );
            conditions
        }
        other => panic!(
            "Ability 1 must have compound And condition for prior spells, got {:?}",
            other
        ),
    };
    assert_has_named_spell_cast_condition(conditions, "peer through depths");
    assert_has_named_spell_cast_condition(conditions, "reach through mists");
}

#[test]
fn sift_through_sands_single_nl_curly_inspect() {
    let parsed = engine::parser::parse_oracle_text(
        SIFT_THROUGH_SANDS_SINGLE_NL_CURLY,
        "Sift Through Sands",
        &[],
        &["Instant".to_string()],
        &["Arcane".to_string()],
    );
    assert_sift_through_sands_single_nl_ast(&parsed);
}

#[test]
fn sift_through_sands_parses_user_oracle() {
    let parsed = engine::parser::parse_oracle_text(
        SIFT_THROUGH_SANDS_USER_ORACLE,
        "Sift Through Sands",
        &[],
        &["Instant".to_string()],
        &["Arcane".to_string()],
    );
    assert_sift_through_sands_user_oracle_ast(&parsed);
}

#[test]
fn sift_through_sands_user_oracle_draws_two() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Sift Through Sands",
            true,
            SIFT_THROUGH_SANDS_USER_ORACLE,
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.add_card_to_library_top(P0, "Card 1");
    scenario.add_card_to_library_top(P0, "Card 2");
    scenario.add_card_to_library_top(P0, "Card 3");
    let mut runner = scenario.build();

    let committed = runner.cast(spell).commit();
    let outcome = committed.resolve();

    println!(
        "User oracle final waiting for: {:?}",
        outcome.final_waiting_for()
    );
    println!(
        "User oracle P0 hand: {:?}",
        runner.state().players[P0.0 as usize].hand
    );

    match outcome.final_waiting_for() {
        WaitingFor::DiscardChoice { cards, count, .. } => {
            assert_eq!(*count, 1, "Must ask to discard 1 card");
            assert_eq!(cards.len(), 2, "Hand must contain the 2 drawn cards");
        }
        other => panic!("Expected WaitingFor::DiscardChoice, got {:?}", other),
    }
}

#[test]
fn sift_through_sands_real_card_from_db_draws_two() {
    let db = crate::support::shared_card_db()
        .expect("shared_card_db fixture must be available for real card tests");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sift_id = scenario.add_real_card(
        P0,
        "Sift Through Sands",
        engine::types::zones::Zone::Hand,
        db,
    );
    scenario.add_real_card(P0, "Island", engine::types::zones::Zone::Library, db);
    scenario.add_real_card(P0, "Island", engine::types::zones::Zone::Library, db);
    scenario.add_real_card(P0, "Island", engine::types::zones::Zone::Library, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().debug_mode = true;

    // Give P0 plenty of mana to cast Sift Through Sands (1UU)
    let mana = vec![
        ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
    ];
    for u in mana {
        runner.state_mut().players[P0.0 as usize].mana_pool.add(u);
    }

    let initial_hand_len = runner.state().players[P0.0 as usize].hand.len();
    assert_eq!(
        initial_hand_len, 1,
        "P0 should only have Sift Through Sands in hand"
    );

    let committed = runner.cast(sift_id).commit();
    let outcome = committed.resolve();

    println!(
        "Real card outcome waiting for: {:?}",
        outcome.final_waiting_for()
    );
    println!(
        "Real card P0 hand: {:?}",
        runner.state().players[P0.0 as usize].hand
    );

    match outcome.final_waiting_for() {
        WaitingFor::DiscardChoice { cards, count, .. } => {
            assert_eq!(*count, 1, "Must ask to discard 1 card");
            assert_eq!(cards.len(), 2, "Hand must contain the 2 drawn cards");
        }
        other => panic!("Expected WaitingFor::DiscardChoice, got {:?}", other),
    }
}

#[test]
fn sift_through_sands_without_prior_spells_does_not_search() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Sift Through Sands",
            true,
            SIFT_THROUGH_SANDS_USER_ORACLE,
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.add_card_to_library_top(P0, "The Unspeakable");
    scenario.add_card_to_library_top(P0, "Card 2");
    scenario.add_card_to_library_top(P0, "Card 1");
    let mut runner = scenario.build();

    let committed = runner.cast(spell).commit();
    let outcome = committed.resolve();

    let discard_card = match outcome.final_waiting_for() {
        WaitingFor::DiscardChoice { cards, .. } => cards[0],
        other => panic!("Expected WaitingFor::DiscardChoice, got {:?}", other),
    };

    let outcome2 = runner
        .act(engine::types::actions::GameAction::SelectCards {
            cards: vec![discard_card],
        })
        .unwrap();

    // Because Peer Through Depths and Reach Through Mists were not cast,
    // the search ability must not trigger/resolve. It should return directly to Priority.
    println!("Without prior spells outcome: {:?}", outcome2.waiting_for);
    assert!(
        matches!(outcome2.waiting_for, WaitingFor::Priority { .. }),
        "Expected WaitingFor::Priority, got {:?}",
        outcome2.waiting_for
    );
    // The Unspeakable should still be in the library, not on the battlefield
    let unspeakable_obj = runner
        .state()
        .objects
        .values()
        .find(|obj| obj.name.eq_ignore_ascii_case("The Unspeakable"))
        .expect("The Unspeakable should exist");
    assert_eq!(
        unspeakable_obj.zone,
        engine::types::zones::Zone::Library,
        "The Unspeakable must remain in the library"
    );
}

#[test]
fn sift_through_sands_with_prior_spells_searches_the_unspeakable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reach = scenario
        .add_spell_to_hand_from_oracle(P0, "Reach Through Mists", true, "Draw a card.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let peer = scenario
        .add_spell_to_hand_from_oracle(P0, "Peer Through Depths", true, "Draw a card.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let sift = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Sift Through Sands",
            true,
            SIFT_THROUGH_SANDS_USER_ORACLE,
        )
        .with_mana_cost(ManaCost::zero())
        .id();

    // Populate library: The Unspeakable deep in library, then filler cards on top
    let unspeakable_id = scenario.add_card_to_library_top(P0, "The Unspeakable");
    scenario.add_card_to_library_top(P0, "Card 1");
    scenario.add_card_to_library_top(P0, "Card 2");
    scenario.add_card_to_library_top(P0, "Card 3");
    scenario.add_card_to_library_top(P0, "Card 4");
    scenario.add_card_to_library_top(P0, "Card 5");
    scenario.add_card_to_library_top(P0, "Card 6");
    scenario.add_card_to_library_top(P0, "Card 7");

    let mut runner = scenario.build();

    // 1. Cast and resolve Reach Through Mists
    let r1 = runner.cast(reach).commit().resolve();
    assert!(
        matches!(r1.final_waiting_for(), WaitingFor::Priority { .. }),
        "Reach Through Mists should resolve cleanly to Priority"
    );

    // 2. Cast and resolve Peer Through Depths
    let r2 = runner.cast(peer).commit().resolve();
    assert!(
        matches!(r2.final_waiting_for(), WaitingFor::Priority { .. }),
        "Peer Through Depths should resolve cleanly to Priority"
    );

    // 3. Cast Sift Through Sands
    let r3 = runner.cast(sift).commit().resolve();
    println!("After Sift Through Sands: {:?}", r3.final_waiting_for());

    // Hand must contain the 2 drawn cards and prompt DiscardChoice
    let discard_card = match r3.final_waiting_for() {
        WaitingFor::DiscardChoice { cards, count, .. } => {
            assert_eq!(*count, 1);
            cards[0]
        }
        other => panic!("Expected DiscardChoice, got {:?}", other),
    };

    // Act to discard 1 card
    let r4 = runner
        .act(engine::types::actions::GameAction::SelectCards {
            cards: vec![discard_card],
        })
        .unwrap();

    println!("After discard: {:?}", r4.waiting_for);
    // Now both Reach Through Mists and Peer Through Depths were cast this turn!
    // The search ability is an optional effect ("you may search your library...") and
    // must strictly prompt the player with WaitingFor::OptionalEffectChoice.
    assert!(
        matches!(r4.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
        "Expected WaitingFor::OptionalEffectChoice, got {:?}",
        r4.waiting_for
    );
    let r5 = runner
        .act(engine::types::actions::GameAction::DecideOptionalEffect { accept: true })
        .unwrap();

    match r5.waiting_for {
        WaitingFor::SearchChoice { cards, .. } => {
            assert!(
                cards.contains(&unspeakable_id),
                "Library search should find The Unspeakable"
            );
            let r6 = runner
                .act(engine::types::actions::GameAction::SelectCards {
                    cards: vec![unspeakable_id],
                })
                .unwrap();
            assert!(
                matches!(r6.waiting_for, WaitingFor::Priority { .. }),
                "Expected priority after searching, got {:?}",
                r6.waiting_for
            );
        }
        other => panic!("Expected SearchChoice, got {:?}", other),
    }

    // Verify The Unspeakable is now on the battlefield!
    assert_eq!(
        runner.state().objects[&unspeakable_id].zone,
        engine::types::zones::Zone::Battlefield,
        "The Unspeakable must be on the battlefield!"
    );
}

#[test]
fn sift_through_sands_after_only_peer_through_depths_does_not_search() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let peer = scenario
        .add_spell_to_hand_from_oracle(P0, "Peer Through Depths", true, "Draw a card.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let sift = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Sift Through Sands",
            true,
            SIFT_THROUGH_SANDS_USER_ORACLE,
        )
        .with_mana_cost(ManaCost::zero())
        .id();

    let unspeakable_id = scenario.add_card_to_library_top(P0, "The Unspeakable");
    scenario.add_card_to_library_top(P0, "Card 5");
    scenario.add_card_to_library_top(P0, "Card 4");
    scenario.add_card_to_library_top(P0, "Card 3");
    scenario.add_card_to_library_top(P0, "Card 2");
    scenario.add_card_to_library_top(P0, "Card 1");

    let mut runner = scenario.build();

    // 1. Cast and resolve Peer Through Depths (Reach Through Mists is NOT cast)
    let r1 = runner.cast(peer).commit().resolve();
    assert!(
        matches!(r1.final_waiting_for(), WaitingFor::Priority { .. }),
        "Peer Through Depths should resolve cleanly to Priority"
    );

    // 2. Cast and resolve Sift Through Sands
    let r2 = runner.cast(sift).commit().resolve();

    let discard_card = match r2.final_waiting_for() {
        WaitingFor::DiscardChoice { cards, count, .. } => {
            assert_eq!(*count, 1, "Must ask to discard 1 card");
            assert_eq!(
                cards.len(),
                3,
                "Hand must contain 1 drawn from Peer + 2 drawn from Sift"
            );
            cards[0]
        }
        other => panic!("Expected WaitingFor::DiscardChoice, got {:?}", other),
    };

    let r3 = runner
        .act(engine::types::actions::GameAction::SelectCards {
            cards: vec![discard_card],
        })
        .unwrap();

    // Only Peer Through Depths was cast, so Reach Through Mists was NOT.
    // The conditional search ability must NOT trigger or prompt.
    assert!(
        matches!(r3.waiting_for, WaitingFor::Priority { .. }),
        "Expected WaitingFor::Priority without optional search prompt, got {:?}",
        r3.waiting_for
    );

    // The Unspeakable must remain in the library
    assert_eq!(
        runner.state().objects[&unspeakable_id].zone,
        engine::types::zones::Zone::Library,
        "The Unspeakable must remain in the library when Reach Through Mists was not cast"
    );
}

#[test]
fn sift_through_sands_after_only_reach_through_mists_does_not_search() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reach = scenario
        .add_spell_to_hand_from_oracle(P0, "Reach Through Mists", true, "Draw a card.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let sift = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Sift Through Sands",
            true,
            SIFT_THROUGH_SANDS_USER_ORACLE,
        )
        .with_mana_cost(ManaCost::zero())
        .id();

    let unspeakable_id = scenario.add_card_to_library_top(P0, "The Unspeakable");
    scenario.add_card_to_library_top(P0, "Card 5");
    scenario.add_card_to_library_top(P0, "Card 4");
    scenario.add_card_to_library_top(P0, "Card 3");
    scenario.add_card_to_library_top(P0, "Card 2");
    scenario.add_card_to_library_top(P0, "Card 1");

    let mut runner = scenario.build();

    // 1. Cast and resolve Reach Through Mists (Peer Through Depths is NOT cast)
    let r1 = runner.cast(reach).commit().resolve();
    assert!(
        matches!(r1.final_waiting_for(), WaitingFor::Priority { .. }),
        "Reach Through Mists should resolve cleanly to Priority"
    );

    // 2. Cast and resolve Sift Through Sands
    let r2 = runner.cast(sift).commit().resolve();

    let discard_card = match r2.final_waiting_for() {
        WaitingFor::DiscardChoice { cards, count, .. } => {
            assert_eq!(*count, 1, "Must ask to discard 1 card");
            assert_eq!(
                cards.len(),
                3,
                "Hand must contain 1 drawn from Reach + 2 drawn from Sift"
            );
            cards[0]
        }
        other => panic!("Expected WaitingFor::DiscardChoice, got {:?}", other),
    };

    let r3 = runner
        .act(engine::types::actions::GameAction::SelectCards {
            cards: vec![discard_card],
        })
        .unwrap();

    // Only Reach Through Mists was cast, so Peer Through Depths was NOT.
    // The conditional search ability must NOT trigger or prompt.
    assert!(
        matches!(r3.waiting_for, WaitingFor::Priority { .. }),
        "Expected WaitingFor::Priority without optional search prompt, got {:?}",
        r3.waiting_for
    );

    // The Unspeakable must remain in the library
    assert_eq!(
        runner.state().objects[&unspeakable_id].zone,
        engine::types::zones::Zone::Library,
        "The Unspeakable must remain in the library when Peer Through Depths was not cast"
    );
}

#[test]
fn sift_through_sands_split_player_prerequisites_does_not_search() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let peer = scenario
        .add_spell_to_hand_from_oracle(P1, "Peer Through Depths", true, "Draw a card.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let reach = scenario
        .add_spell_to_hand_from_oracle(P0, "Reach Through Mists", true, "Draw a card.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let sift = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Sift Through Sands",
            true,
            SIFT_THROUGH_SANDS_USER_ORACLE,
        )
        .with_mana_cost(ManaCost::zero())
        .id();

    let unspeakable_id = scenario.add_card_to_library_top(P0, "The Unspeakable");
    scenario.add_card_to_library_top(P0, "Card 5");
    scenario.add_card_to_library_top(P0, "Card 4");
    scenario.add_card_to_library_top(P0, "Card 3");
    scenario.add_card_to_library_top(P0, "Card 2");
    scenario.add_card_to_library_top(P0, "Card 1");

    scenario.add_card_to_library_top(P1, "P1 Card 1");

    let mut runner = scenario.build();

    // 1. P0 passes priority so P1 receives priority to cast Peer Through Depths (Instant)
    let pass = runner
        .act(engine::types::actions::GameAction::PassPriority)
        .unwrap();
    assert!(
        matches!(pass.waiting_for, WaitingFor::Priority { player } if player == P1),
        "Priority must pass to P1, got {:?}",
        pass.waiting_for
    );

    // P1 casts and resolves Peer Through Depths
    let r1 = runner.cast(peer).commit().resolve();
    assert!(
        matches!(r1.final_waiting_for(), WaitingFor::Priority { player } if *player == P0),
        "Priority should return to active player P0 after Peer Through Depths resolves"
    );

    // 2. P0 casts and resolves Reach Through Mists
    let r2 = runner.cast(reach).commit().resolve();
    assert!(
        matches!(r2.final_waiting_for(), WaitingFor::Priority { .. }),
        "Reach Through Mists cast by P0 should resolve cleanly to Priority"
    );

    // 3. P0 casts and resolves Sift Through Sands
    let r3 = runner.cast(sift).commit().resolve();

    let discard_card = match r3.final_waiting_for() {
        WaitingFor::DiscardChoice { cards, count, .. } => {
            assert_eq!(*count, 1, "Must ask to discard 1 card");
            assert_eq!(
                cards.len(),
                3,
                "P0 hand must contain 1 drawn from Reach + 2 drawn from Sift"
            );
            cards[0]
        }
        other => panic!("Expected WaitingFor::DiscardChoice, got {:?}", other),
    };

    let r4 = runner
        .act(engine::types::actions::GameAction::SelectCards {
            cards: vec![discard_card],
        })
        .unwrap();

    // P1 cast Peer Through Depths and P0 cast Reach Through Mists this turn.
    // P0's "you've cast" condition requires P0 to have cast BOTH spells.
    // The conditional search ability must NOT trigger or prompt P0 to search.
    assert!(
        matches!(r4.waiting_for, WaitingFor::Priority { .. }),
        "Expected WaitingFor::Priority without optional search prompt when opponent cast a prerequisite, got {:?}",
        r4.waiting_for
    );

    // The Unspeakable must remain in P0's library
    assert_eq!(
        runner.state().objects[&unspeakable_id].zone,
        engine::types::zones::Zone::Library,
        "The Unspeakable must remain in P0's library when an opponent cast one of the prerequisites"
    );
}
