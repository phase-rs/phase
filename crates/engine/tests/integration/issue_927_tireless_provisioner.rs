//! Regression pins for issue #927: Tireless Provisioner's landfall trigger must
//! prompt the controller to choose Food or Treasure, not auto-resolve a branch.
//!
//! Note: these tests pin already-correct engine behavior — they were verified
//! to pass against the pre-fix `origin/main` engine (the parse, prompt, and
//! branch routing all held on base). The discriminating coverage for this PR's
//! engine deltas (empty-chooser fail-loud guard, token-name branch-label
//! fallback) lives in the `choose_one_of.rs` unit tests, which fail against
//! the base logic.

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{Effect, PlayerFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;
use crate::support::shared_card_export_json as load_export;

const TIRELESS_PROVISIONER_ORACLE: &str =
    "Landfall — Whenever a land you control enters, create a Food token or a Treasure token.";

fn food_tokens_on_battlefield(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|o| o.is_token && o.card_types.subtypes.iter().any(|s| s == "Food"))
        })
        .count()
}

fn treasure_tokens_on_battlefield(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner.state().objects.get(id).is_some_and(|o| {
                o.is_token && o.card_types.subtypes.iter().any(|s| s == "Treasure")
            })
        })
        .count()
}

#[test]
fn tireless_provisioner_export_execute_is_choose_one_of() {
    let Some(export) = load_export() else {
        return;
    };
    let card = export
        .get("tireless provisioner")
        .expect("Tireless Provisioner must be in card-data export");
    let triggers = card
        .get("triggers")
        .and_then(|v| v.as_array())
        .expect("triggers array");
    let execute = triggers[0]
        .get("execute")
        .and_then(|v| v.get("effect"))
        .expect("landfall execute effect");
    assert_eq!(
        execute.get("type").and_then(|v| v.as_str()),
        Some("ChooseOneOf"),
        "shipped card-data must encode Food/Treasure as ChooseOneOf, not a token chain"
    );
    let branches = execute
        .get("branches")
        .and_then(|v| v.as_array())
        .expect("ChooseOneOf branches");
    assert_eq!(branches.len(), 2);
    assert_eq!(
        branches[0]
            .get("effect")
            .and_then(|e| e.get("name"))
            .and_then(|n| n.as_str()),
        Some("Food")
    );
    assert_eq!(
        branches[1]
            .get("effect")
            .and_then(|e| e.get("name"))
            .and_then(|n| n.as_str()),
        Some("Treasure")
    );
}

#[test]
fn tireless_provisioner_card_data_landfall_prompts_before_token() {
    let Some(db) = load_db() else {
        return;
    };
    if db.get_face_by_name("Tireless Provisioner").is_none() {
        eprintln!("skipping: Tireless Provisioner not in integration fixture — run scripts/gen-test-fixture.py");
        return;
    }

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Tireless Provisioner", Zone::Battlefield, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Hand, db);
    let mut runner = scenario.build();

    // Production path: WASM rehydrates every object from the export after deck load.
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id: runner.state().objects[&forest].card_id,
        })
        .expect("play Forest");
    runner.pass_both_players();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch { .. }
        ),
        "export-backed Tireless Provisioner must pause on Food/Treasure choice, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(food_tokens_on_battlefield(&runner), 0);
    assert_eq!(treasure_tokens_on_battlefield(&runner), 0);
}

#[test]
fn tireless_provisioner_landfall_parses_as_choose_one_of() {
    let mut scenario = GameScenario::new();
    let id = scenario
        .add_creature_from_oracle(
            P0,
            "Tireless Provisioner",
            3,
            2,
            TIRELESS_PROVISIONER_ORACLE,
        )
        .id();
    let runner = scenario.build();
    let obj = &runner.state().objects[&id];

    let trigger = (0..obj.trigger_definitions.len())
        .filter_map(|i| obj.trigger_definitions.get(i))
        .find(|t| t.definition.destination == Some(Zone::Battlefield))
        .expect("landfall trigger");
    let execute = trigger.definition.execute.as_ref().expect("execute");
    match &*execute.effect {
        Effect::ChooseOneOf { chooser, branches } => {
            assert_eq!(*chooser, PlayerFilter::Controller);
            assert_eq!(branches.len(), 2);
            assert!(matches!(&*branches[0].effect, Effect::Token { name, .. } if name == "Food"));
            assert!(
                matches!(&*branches[1].effect, Effect::Token { name, .. } if name == "Treasure")
            );
        }
        other => panic!("expected ChooseOneOf execute, got {other:?}"),
    }
}

#[test]
fn tireless_provisioner_landfall_prompts_food_or_treasure_before_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "Tireless Provisioner",
        3,
        2,
        TIRELESS_PROVISIONER_ORACLE,
    );
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();

    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id: runner.state().objects[&forest].card_id,
        })
        .expect("play Forest");

    // Landfall trigger should be on the stack after the land enters.
    assert!(
        !runner.state().stack.is_empty(),
        "landfall trigger should be on the stack after playing a land"
    );

    // Resolve the trigger — must pause on branch choice, not create a token yet.
    runner.pass_both_players();

    match &runner.state().waiting_for {
        WaitingFor::ChooseOneOfBranch {
            player,
            branches,
            branch_descriptions,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(branches.len(), 2);
            assert_eq!(branch_descriptions.len(), 2);
            assert!(branch_descriptions[0].contains("Food"));
            assert!(branch_descriptions[1].contains("Treasure"));
        }
        other => panic!(
            "expected ChooseOneOfBranch after landfall resolves, got {other:?}; \
             food={}, treasure={}",
            food_tokens_on_battlefield(&runner),
            treasure_tokens_on_battlefield(&runner),
        ),
    }

    assert_eq!(food_tokens_on_battlefield(&runner), 0);
    assert_eq!(treasure_tokens_on_battlefield(&runner), 0);
}

#[test]
fn tireless_provisioner_food_branch_creates_only_food() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "Tireless Provisioner",
        3,
        2,
        TIRELESS_PROVISIONER_ORACLE,
    );
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();

    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id: runner.state().objects[&forest].card_id,
        })
        .expect("play Forest");
    runner.pass_both_players();

    let food_index = match &runner.state().waiting_for {
        WaitingFor::ChooseOneOfBranch { branches, .. } => branches
            .iter()
            .position(|b| matches!(&*b.effect, Effect::Token { name, .. } if name == "Food"))
            .expect("Food branch"),
        other => panic!("expected ChooseOneOfBranch, got {other:?}"),
    };

    runner
        .act(GameAction::ChooseBranch { index: food_index })
        .expect("choose Food");

    assert_eq!(food_tokens_on_battlefield(&runner), 1);
    assert_eq!(treasure_tokens_on_battlefield(&runner), 0);
    assert!(
        runner.state().stack.is_empty(),
        "stack should be empty after branch resolves"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "should return to priority after choice"
    );
}

#[test]
fn tireless_provisioner_treasure_branch_creates_only_treasure() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "Tireless Provisioner",
        3,
        2,
        TIRELESS_PROVISIONER_ORACLE,
    );
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();

    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id: runner.state().objects[&forest].card_id,
        })
        .expect("play Forest");
    runner.pass_both_players();

    let treasure_index = match &runner.state().waiting_for {
        WaitingFor::ChooseOneOfBranch { branches, .. } => branches
            .iter()
            .position(|b| matches!(&*b.effect, Effect::Token { name, .. } if name == "Treasure"))
            .expect("Treasure branch"),
        other => panic!("expected ChooseOneOfBranch, got {other:?}"),
    };

    runner
        .act(GameAction::ChooseBranch {
            index: treasure_index,
        })
        .expect("choose Treasure");

    assert_eq!(food_tokens_on_battlefield(&runner), 0);
    assert_eq!(treasure_tokens_on_battlefield(&runner), 1);
    assert_eq!(
        runner
            .state()
            .objects
            .values()
            .filter(|o| o.is_token && o.zone == Zone::Battlefield)
            .count(),
        1
    );
}
