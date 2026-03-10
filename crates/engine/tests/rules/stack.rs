#![allow(unused_imports)]
use super::*;

use std::collections::HashMap;

use engine::types::ability::{DamageAmount, Effect, TargetRef, TargetSpec};
use engine::types::game_state::StackEntryKind;
use engine::types::identifiers::{CardId, ObjectId};

/// CR 405.5: Stack resolves LIFO -- last spell cast resolves first.
///
/// Cast two bolts targeting different players. The second bolt should resolve
/// before the first when both players pass priority.
#[test]
fn stack_resolves_lifo() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two bolts in hand (targeting: bolt with "Any" auto-targets if only 1 legal target,
    // but with 2 players + potentially creatures, we need target selection.
    // However, if there are no creatures, only 2 players -> need SelectTargets)
    let bolt1_id = scenario.add_bolt_to_hand(P0);
    let bolt2_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();

    // Cast bolt 1
    let bolt1_card_id = runner.state().objects[&bolt1_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            card_id: bolt1_card_id,
            targets: vec![],
        })
        .expect("cast bolt 1 should succeed");

    // Bolt targeting "Any" with 2 players = need target selection
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("select target for bolt 1");
    }

    // Cast bolt 2 (P0 should still have priority after casting bolt 1)
    let bolt2_card_id = runner.state().objects[&bolt2_id].card_id;
    let result2 = runner
        .act(GameAction::CastSpell {
            card_id: bolt2_card_id,
            targets: vec![],
        })
        .expect("cast bolt 2 should succeed");

    if matches!(result2.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P0)],
            })
            .expect("select target for bolt 2");
    }

    // Both bolts on stack. Stack should have 2 entries.
    assert_eq!(
        runner.state().stack.len(),
        2,
        "Two bolts should be on the stack"
    );

    // The last cast (bolt 2) should be on top of stack
    let top = runner.state().stack.last().unwrap();
    assert_eq!(
        top.source_id, bolt2_id,
        "Bolt 2 (last cast) should be on top of stack"
    );

    // Resolve top (bolt 2) -- both players pass priority
    runner.resolve_top();

    // After resolving bolt 2 (targeted P0), bolt 1 should still be on stack
    assert_eq!(
        runner.state().stack.len(),
        1,
        "After resolving bolt 2, bolt 1 should remain"
    );

    // The life change from bolt 2 (targeted P0) should be applied first
    assert_eq!(
        runner.state().players[0].life, 17,
        "P0 should have lost 3 life from bolt 2"
    );
    assert_eq!(
        runner.state().players[1].life, 20,
        "P1 should still be at 20 (bolt 1 hasn't resolved yet)"
    );
}

/// CR 117.3a: Active player gets priority after a spell resolves.
///
/// After resolving a spell from the stack, the active player should receive priority.
#[test]
fn active_player_gets_priority_after_resolve() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // One creature so bolt has a single auto-target
    let bear_id = scenario.add_creature(P1, "Bear", 2, 2).id();
    let bolt_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();

    // Cast bolt (single creature = auto-target)
    let bolt_card_id = runner.state().objects[&bolt_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            card_id: bolt_card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    // If there's target selection needed (2 players + 1 creature = 3 targets)
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(bear_id)],
            })
            .expect("select target");
    }

    // Resolve the bolt
    runner.resolve_top();

    // Active player should have priority
    let state = runner.state();
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::Priority { player } if player == state.active_player
        ),
        "After resolving, active player should have priority. Got: {:?}",
        state.waiting_for
    );
}

/// CR 405.1: Stack is empty after all spells resolve.
///
/// Cast a spell, resolve it, and verify the stack is empty.
#[test]
fn empty_stack_after_all_spells_resolve() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bear_id = scenario.add_creature(P1, "Bear", 2, 2).id();
    let bolt_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();

    let bolt_card_id = runner.state().objects[&bolt_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            card_id: bolt_card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(bear_id)],
            })
            .expect("select target");
    }

    // Resolve all
    runner.resolve_top();

    assert!(
        runner.state().stack.is_empty(),
        "Stack should be empty after all spells resolve"
    );
}

/// CR 608.2b: Instant resolves with effect -- target loses life/takes damage.
///
/// Cast Lightning Bolt targeting a player. After resolution, that player
/// should have lost 3 life.
#[test]
fn instant_resolves_with_damage_effect() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Add a single creature so there's only one valid target category;
    // but with "Any" targeting, multiple targets exist.
    // Instead, let's just add a bolt and handle target selection.
    let bolt_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();

    let bolt_card_id = runner.state().objects[&bolt_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            card_id: bolt_card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    // Target P1
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("select target");
    }

    // Resolve
    runner.resolve_top();

    assert_eq!(
        runner.state().players[1].life, 17,
        "P1 should have lost 3 life from Lightning Bolt"
    );
    assert!(
        runner.state().stack.is_empty(),
        "Stack should be empty after resolution"
    );
}

/// CR 117.4: Both players must pass priority in succession for top of stack to resolve.
///
/// After casting a spell, only the active player (P0) has passed. P1 must also pass
/// before the spell resolves.
#[test]
fn both_players_must_pass_for_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bolt_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();

    let bolt_card_id = runner.state().objects[&bolt_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            card_id: bolt_card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    // Handle target selection if needed
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("select target");
    }

    // Bolt is on the stack. P0 should have priority (as active player after casting).
    // Pass priority from P0 -> P1 gets priority
    let after_pass = runner
        .act(GameAction::PassPriority)
        .expect("P0 pass should succeed");

    // After P0 passes, P1 should have priority (spell NOT yet resolved)
    assert!(
        matches!(after_pass.waiting_for, WaitingFor::Priority { player } if player == P1),
        "After P0 passes, P1 should have priority. Got: {:?}",
        after_pass.waiting_for
    );

    // Stack should still have the bolt (not yet resolved)
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Bolt should still be on stack after only P0 passes"
    );

    // Now P1 passes -- both have passed, top of stack resolves
    let _after_resolve = runner
        .act(GameAction::PassPriority)
        .expect("P1 pass should succeed");

    // Stack should be empty now (bolt resolved)
    assert!(
        runner.state().stack.is_empty(),
        "Stack should be empty after both players pass"
    );

    // Bolt's effect should have been applied
    assert_eq!(
        runner.state().players[1].life, 17,
        "P1 should have lost 3 life from resolved bolt"
    );
}
