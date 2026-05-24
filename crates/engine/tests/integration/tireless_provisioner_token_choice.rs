//! Integration test for Tireless Provisioner's landfall token choice.
//!
//! CR 111.2 + CR 608.2d: "create a Food token or a Treasure token" is a
//! shared-verb choice clause. The controller chooses one of the two token types
//! at resolution. The engine must present `WaitingFor::ChooseOneOfBranch` with
//! two branches (Food / Treasure) and create the selected token.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TIRELESS_PROVISIONER_ORACLE: &str =
    "Landfall — Whenever a land you control enters, create a Food token or a Treasure token.";

fn count_tokens_by_subtype(runner: &engine::game::scenario::GameRunner, subtype: &str) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.is_token
                && o.zone == Zone::Battlefield
                && o.card_types.subtypes.iter().any(|s| s == subtype)
        })
        .count()
}

/// Drive the trigger stack until we hit a ChooseOneOfBranch or empty-stack
/// Priority. Returns the waiting state variant name for assertion.
fn advance_to_choice_or_priority(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..50 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        match &runner.state().waiting_for {
            WaitingFor::ChooseOneOfBranch { .. } => return,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
        }
    }
    panic!(
        "did not reach ChooseOneOfBranch or clean Priority within iteration limit — stuck at {:?}",
        runner.state().waiting_for
    );
}

/// After choosing a branch, resolve the remaining stack to clean Priority.
fn resolve_remaining(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..50 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
        }
    }
    panic!(
        "did not reach clean Priority — stuck at {:?}",
        runner.state().waiting_for
    );
}

// CR 608.2d: Choosing Food creates a Food token, not a Treasure token.
#[test]
fn tireless_provisioner_landfall_prompts_food_or_treasure_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(
        P0,
        "Tireless Provisioner",
        3,
        2,
        TIRELESS_PROVISIONER_ORACLE,
    );

    let forest_id = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&forest_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest_id,
            card_id,
        })
        .expect("should play Forest");

    advance_to_choice_or_priority(&mut runner);

    // The engine must present a ChooseOneOfBranch with exactly two branches.
    match &runner.state().waiting_for {
        WaitingFor::ChooseOneOfBranch {
            branches,
            branch_descriptions,
            ..
        } => {
            assert_eq!(branches.len(), 2, "should have Food and Treasure branches");
            let descs: Vec<&str> = branch_descriptions.iter().map(|s| s.as_str()).collect();
            assert!(
                descs.iter().any(|d| d.contains("Food")),
                "one branch should mention Food, got: {descs:?}"
            );
            assert!(
                descs.iter().any(|d| d.contains("Treasure")),
                "one branch should mention Treasure, got: {descs:?}"
            );
        }
        other => panic!("expected ChooseOneOfBranch, got: {other:?}"),
    }

    // Choose Food (branch 0).
    runner
        .act(GameAction::ChooseBranch { index: 0 })
        .expect("choose Food branch");

    resolve_remaining(&mut runner);

    assert_eq!(
        count_tokens_by_subtype(&runner, "Food"),
        1,
        "choosing branch 0 should create exactly one Food token"
    );
    assert_eq!(
        count_tokens_by_subtype(&runner, "Treasure"),
        0,
        "choosing Food should not create a Treasure token"
    );
}

// CR 608.2d: Choosing Treasure creates a Treasure token, not a Food token.
#[test]
fn tireless_provisioner_landfall_treasure_branch_creates_treasure() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(
        P0,
        "Tireless Provisioner",
        3,
        2,
        TIRELESS_PROVISIONER_ORACLE,
    );

    let forest_id = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&forest_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest_id,
            card_id,
        })
        .expect("should play Forest");

    advance_to_choice_or_priority(&mut runner);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch { .. }
        ),
        "expected ChooseOneOfBranch"
    );

    // Choose Treasure (branch 1).
    runner
        .act(GameAction::ChooseBranch { index: 1 })
        .expect("choose Treasure branch");

    resolve_remaining(&mut runner);

    assert_eq!(
        count_tokens_by_subtype(&runner, "Treasure"),
        1,
        "choosing branch 1 should create exactly one Treasure token"
    );
    assert_eq!(
        count_tokens_by_subtype(&runner, "Food"),
        0,
        "choosing Treasure should not create a Food token"
    );
}
