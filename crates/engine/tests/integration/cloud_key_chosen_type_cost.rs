//! Reproduction + regression for issue #930: Cloud Key chosen-type cost reduction.
//!
//! Cloud Key: "As this artifact enters, choose artifact, creature, enchantment,
//! instant, or sorcery. Spells you cast of the chosen type cost {1} less to cast."
//!
//! On `main`, two coupled parser bugs make Cloud Key reduce the cost of EVERY
//! spell you cast, regardless of (or without) a chosen type:
//!   - The static `ReduceCost` filter parses to `Typed { type_filters: [Card] }`
//!     with no `FilterProp::IsChosenCardType`, so it matches all spells.
//!   - The ETB "choose artifact, creature, ..." parses to a `Labeled` choice
//!     instead of `ChoiceType::CardType`, so no `ChosenAttribute::CardType` is
//!     ever stored for `IsChosenCardType` to read.
//!
//! CR 601.2f: cost reductions apply only to spells matching the effect's filter.
//! A spell that is not of the chosen type must NOT be reduced.

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::ChoiceType;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// With Cloud Key on the battlefield and NO card type chosen, a non-artifact
/// spell (Cultivate, {2}{G}) must not receive any cost reduction.
///
/// On `main` this FAILS: Cloud Key's `ReduceCost` filter parses to all-`Card`
/// (no `IsChosenCardType`), so `display_spell_cost` reduces the generic from
/// 2 to 1 for every spell the controller casts.
#[test]
fn cloud_key_does_not_reduce_non_chosen_type_spell() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Cloud Key on the battlefield => its cost-reduction static is active.
    scenario.add_real_card(P0, "Cloud Key", Zone::Battlefield, db);
    // A {2}{G} non-artifact sorcery in hand.
    let cultivate = scenario.add_real_card(P0, "Cultivate", Zone::Hand, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let cost = engine::game::casting::display_spell_cost(runner.state(), P0, cultivate)
        .expect("Cultivate should have a displayable cost");

    let ManaCost::Cost { generic, .. } = cost else {
        panic!("expected ManaCost::Cost, got {cost:?}");
    };

    // CR 601.2f: Cloud Key reduces only spells of the CHOSEN type. With nothing
    // chosen — and Cultivate being a non-artifact sorcery regardless — the
    // generic component must stay 2. A reduced value proves the all-`Card`
    // filter / missing `IsChosenCardType` bug (issue #930).
    assert_eq!(
        generic, 2,
        "Cloud Key must NOT reduce a non-chosen-type spell (issue #930); \
         got generic={generic}, expected 2"
    );
}

/// End-to-end: casting Cloud Key surfaces a `CardType` ETB choice (issue #930
/// Bug B — the enumerated "choose artifact, creature, enchantment, instant, or
/// sorcery" must not fall to a `Labeled` choice). After choosing Artifact, an
/// artifact spell is reduced by {1} while a non-artifact spell is not (Bug A).
#[test]
fn cloud_key_reduces_only_the_chosen_card_type_after_etb_choice() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cloud_key = scenario.add_real_card(P0, "Cloud Key", Zone::Hand, db);
    let mind_stone = scenario.add_real_card(P0, "Mind Stone", Zone::Hand, db); // {2} artifact
    let cultivate = scenario.add_real_card(P0, "Cultivate", Zone::Hand, db); // {2}{G} sorcery

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // {3} generic to cast Cloud Key.
    {
        let pool = &mut runner.state_mut().players[0].mana_pool;
        for _ in 0..3 {
            pool.add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
        }
    }

    // Pool-funded cast through the canonical pipeline. Cloud Key's ETB choice
    // is a prompt the resolution driver does not auto-answer, so it halts there
    // and surfaces it via `final_waiting_for()` for the caller to inspect/drive.
    let outcome = runner.cast(cloud_key).resolve();

    // Bug B: the ETB choice must surface as a CardType choice keyed to Cloud Key.
    match outcome.final_waiting_for() {
        WaitingFor::NamedChoice {
            choice_type,
            options,
            source: Some(source),
            ..
        } => {
            assert_eq!(
                choice_type,
                &ChoiceType::card_type_from(vec![
                    CoreType::Artifact,
                    CoreType::Creature,
                    CoreType::Enchantment,
                    CoreType::Instant,
                    CoreType::Sorcery,
                ]),
                "Cloud Key ETB must preserve its exact card-type domain"
            );
            assert_eq!(
                options,
                &["Artifact", "Creature", "Enchantment", "Instant", "Sorcery"]
            );
            assert_eq!(source.prompt.identity.reference.object_id, cloud_key);
        }
        other => panic!("expected NamedChoice after Cloud Key ETB, got {other:?}"),
    }

    let waiting_before_rejection = runner.state().waiting_for.clone();
    let attributes_before_rejection = runner.state().objects[&cloud_key].chosen_attributes.clone();
    for illegal_choice in ["Land", "Planeswalker"] {
        let error = runner
            .act(GameAction::ChooseOption {
                choice: illegal_choice.to_string(),
            })
            .expect_err("an option outside Cloud Key's printed domain must be rejected");
        assert!(
            matches!(error, engine::game::EngineError::InvalidAction(_)),
            "{illegal_choice} must be an InvalidAction, got {error:?}"
        );
        assert_eq!(runner.state().waiting_for, waiting_before_rejection);
        assert_eq!(
            runner.state().objects[&cloud_key].chosen_attributes,
            attributes_before_rejection
        );
    }
    runner
        .act(GameAction::ChooseOption {
            choice: "Artifact".to_string(),
        })
        .expect("ChooseOption(Artifact) must resolve");
    assert_eq!(
        runner.state().objects[&cloud_key].chosen_card_type(),
        Some(CoreType::Artifact),
        "the chosen card type must persist on Cloud Key"
    );

    // Bug A: the artifact spell (chosen type) is reduced {2} -> {1}; the
    // non-artifact spell ({2}{G} Cultivate) is unchanged.
    let artifact_cost = engine::game::casting::display_spell_cost(runner.state(), P0, mind_stone)
        .expect("Mind Stone should have a displayable cost");
    let ManaCost::Cost {
        generic: art_generic,
        ..
    } = artifact_cost
    else {
        panic!("expected ManaCost::Cost, got {artifact_cost:?}");
    };
    assert_eq!(
        art_generic, 1,
        "an artifact spell of the chosen type must be reduced from {{2}} to {{1}}"
    );

    let other_cost = engine::game::casting::display_spell_cost(runner.state(), P0, cultivate)
        .expect("Cultivate should have a displayable cost");
    let ManaCost::Cost {
        generic: other_generic,
        ..
    } = other_cost
    else {
        panic!("expected ManaCost::Cost, got {other_cost:?}");
    };
    assert_eq!(
        other_generic, 2,
        "a non-chosen-type spell must be unchanged"
    );
}

#[test]
fn two_cloud_keys_bind_their_own_chosen_types() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first_cloud_key = scenario.add_real_card(P0, "Cloud Key", Zone::Hand, db);
    let second_cloud_key = scenario.add_real_card(P0, "Cloud Key", Zone::Hand, db);
    let mind_stone = scenario.add_real_card(P0, "Mind Stone", Zone::Hand, db);
    let cultivate = scenario.add_real_card(P0, "Cultivate", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    {
        let pool = &mut runner.state_mut().players[0].mana_pool;
        for _ in 0..6 {
            pool.add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
        }
    }

    let first = runner.cast(first_cloud_key).resolve();
    match first.final_waiting_for() {
        WaitingFor::NamedChoice {
            source: Some(source),
            options,
            ..
        } => {
            assert_eq!(source.prompt.identity.reference.object_id, first_cloud_key);
            assert_eq!(
                options,
                &["Artifact", "Creature", "Enchantment", "Instant", "Sorcery"]
            );
        }
        other => panic!("expected first Cloud Key choice, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseOption {
            choice: "Artifact".to_string(),
        })
        .expect("first Cloud Key choice resolves");

    let second = runner.cast(second_cloud_key).resolve();
    match second.final_waiting_for() {
        WaitingFor::NamedChoice {
            source: Some(source),
            options,
            ..
        } => {
            assert_eq!(source.prompt.identity.reference.object_id, second_cloud_key);
            assert_eq!(
                options,
                &["Artifact", "Creature", "Enchantment", "Instant", "Sorcery"]
            );
        }
        other => panic!("expected second Cloud Key choice, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseOption {
            choice: "Sorcery".to_string(),
        })
        .expect("second Cloud Key choice resolves");

    assert_eq!(
        runner.state().objects[&first_cloud_key].chosen_card_type(),
        Some(CoreType::Artifact),
        "the first source must retain only its Artifact choice"
    );
    assert_eq!(
        runner.state().objects[&second_cloud_key].chosen_card_type(),
        Some(CoreType::Sorcery),
        "the second source must retain only its Sorcery choice"
    );

    let ManaCost::Cost {
        generic: mind_stone_generic,
        ..
    } = engine::game::casting::display_spell_cost(runner.state(), P0, mind_stone)
        .expect("Mind Stone should have a displayable cost")
    else {
        panic!("Mind Stone must have a standard mana cost");
    };
    let ManaCost::Cost {
        generic: cultivate_generic,
        ..
    } = engine::game::casting::display_spell_cost(runner.state(), P0, cultivate)
        .expect("Cultivate should have a displayable cost")
    else {
        panic!("Cultivate must have a standard mana cost");
    };
    assert_eq!(
        mind_stone_generic, 1,
        "only the Artifact Cloud Key discounts Mind Stone"
    );
    assert_eq!(
        cultivate_generic, 1,
        "only the Sorcery Cloud Key discounts Cultivate"
    );
}

#[test]
fn archon_of_valors_reach_blocks_only_its_chosen_type() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let archon = scenario.add_real_card(P0, "Archon of Valor's Reach", Zone::Hand, db);
    let artifact = scenario.add_real_card(P0, "Mind Stone", Zone::Hand, db);
    let sorcery = scenario.add_real_card(P0, "Cultivate", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for mana_type in [
        ManaType::White,
        ManaType::White,
        ManaType::Green,
        // Leave {2}{G} after paying Archon's {3}{G}{W}, so the unchosen
        // Cultivate assertion exercises Archon's casting restriction rather
        // than failing on mana availability.
        ManaType::Green,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
    ] {
        pool.add(ManaUnit::new(mana_type, ObjectId(0), false, vec![]));
    }

    let outcome = runner.cast(archon).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::NamedChoice {
            choice_type: ChoiceType::CardType { options },
            options: prompt,
            ..
        } if options == &vec![
            CoreType::Artifact,
            CoreType::Enchantment,
            CoreType::Instant,
            CoreType::Sorcery,
            CoreType::Planeswalker,
        ] && prompt == &[
            "Artifact", "Enchantment", "Instant", "Sorcery", "Planeswalker"
        ]
    ));
    runner
        .act(GameAction::ChooseOption {
            choice: "Artifact".to_string(),
        })
        .expect("Archon's selected type persists");
    assert!(
        runner.cast(artifact).try_resolve().is_err(),
        "CR 601.3: Archon must prohibit casting its chosen Artifact type"
    );
    runner
        .cast(sorcery)
        .try_resolve()
        .expect("an adjacent unchosen spell type remains castable");
}

#[test]
fn stenn_uses_positive_exclusion_domain_and_reduces_only_selected_type() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stenn = scenario.add_real_card(P0, "Stenn, Paranoid Partisan", Zone::Hand, db);
    let artifact = scenario.add_real_card(P0, "Mind Stone", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for mana_type in [ManaType::White, ManaType::Blue] {
        pool.add(ManaUnit::new(mana_type, ObjectId(0), false, vec![]));
    }

    let outcome = runner.cast(stenn).resolve();
    match outcome.final_waiting_for() {
        WaitingFor::NamedChoice {
            choice_type,
            options,
            ..
        } => {
            assert_eq!(
                choice_type,
                &ChoiceType::card_type_from(vec![
                    CoreType::Artifact,
                    CoreType::Enchantment,
                    CoreType::Instant,
                    CoreType::Planeswalker,
                    CoreType::Sorcery,
                ])
            );
            assert_eq!(
                options,
                &[
                    "Artifact",
                    "Enchantment",
                    "Instant",
                    "Planeswalker",
                    "Sorcery"
                ]
            );
        }
        other => panic!("expected Stenn choice, got {other:?}"),
    }
    let before = runner.state().objects[&stenn].chosen_attributes.clone();
    for illegal in ["Creature", "Land"] {
        assert!(runner
            .act(GameAction::ChooseOption {
                choice: illegal.to_string(),
            })
            .is_err());
        assert_eq!(runner.state().objects[&stenn].chosen_attributes, before);
    }
    runner
        .act(GameAction::ChooseOption {
            choice: "Artifact".to_string(),
        })
        .expect("Artifact is in Stenn's exact domain");
    let ManaCost::Cost { generic, .. } =
        engine::game::casting::display_spell_cost(runner.state(), P0, artifact)
            .expect("Mind Stone has a displayable cost")
    else {
        panic!("Mind Stone must have a standard mana cost");
    };
    assert_eq!(generic, 1, "only Stenn's selected type costs {{1}} less");
}
