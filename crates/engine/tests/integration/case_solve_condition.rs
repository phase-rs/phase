//! CR 719.3a end-to-end: a Case whose "To solve" condition is a general
//! game-state condition (here "you have no cards in hand", as on Case of the
//! Crimson Pulse) auto-solves at the controller's end step once the condition
//! is met.
//!
//! Before this change `parse_solve_condition` decomposed only the bespoke
//! "you control no <subtype>" phrasing into `SolveCondition::ObjectCount`;
//! every other condition fell to the inert `SolveCondition::Text`, whose
//! evaluator arm returns `false`, so the Case never solved. Routing the
//! parse through the single condition authority (`parse_inner_condition`)
//! produces a `SolveCondition::Condition { condition }` that the end-step
//! evaluator hands to `layers::evaluate_condition` (CR 719.3a).
//!
//! These tests drive the real engine pipeline (build → synthesized end-step
//! `SolveCase` trigger → `TriggerCondition::SolveConditionMet` →
//! `Effect::SolveCase` → `is_solved`), not a hand-constructed expected state.
//! Reverting the parser/evaluator change makes the solve condition parse to
//! `Text` and the positive assertion (`is_solved == true`) flips to false.

use super::rules::{GameScenario, Phase, P0};
use engine::types::identifiers::ObjectId;

// CR 719.3a: "To solve" is a general game-state condition; "Solved" grants a
// dummy keyword-only ability so the line parses, but only the solve trigger
// matters for these assertions.
const CRIMSON_PULSE_LIKE: &str =
    "To solve \u{2014} You have no cards in hand.\nSolved \u{2014} {T}: Add {R}.";

fn is_solved(runner: &super::rules::GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .objects
        .get(&id)
        .expect("Case object still present")
        .case_state
        .as_ref()
        .expect("Case object carries case_state")
        .is_solved
}

fn build_case(scenario: &mut GameScenario) -> ObjectId {
    // A Case is an Enchantment with the "Case" subtype. The subtype/type must be
    // set BEFORE `from_oracle_text`, because the parse + synthesis pipeline gates
    // both the solve-condition decomposition and the auto-solve trigger on the
    // "Case" subtype.
    let mut builder = scenario.add_creature(P0, "Test Case", 0, 0);
    builder
        .as_enchantment()
        .with_subtypes(vec!["Case"])
        .from_oracle_text(CRIMSON_PULSE_LIKE);
    builder.id()
}

#[test]
fn case_auto_solves_at_end_step_when_condition_met() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P0 has an empty hand by default — the "you have no cards in hand"
    // condition is met.
    let case = build_case(&mut scenario);

    let mut runner = scenario.build();
    assert!(
        !is_solved(&runner, case),
        "Case starts unsolved before its end step"
    );

    // Advance to (and through) P0's end step; the synthesized auto-solve trigger
    // fires, its `SolveConditionMet` predicate evaluates the hand-size condition
    // via `layers::evaluate_condition`, and `Effect::SolveCase` flips `is_solved`.
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert!(
        is_solved(&runner, case),
        "Case must auto-solve at end step when 'you have no cards in hand' is met (CR 719.3a)"
    );
}

#[test]
fn case_does_not_solve_when_condition_unmet() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Give P0 a card in hand so the "you have no cards in hand" condition is NOT met.
    scenario.add_card_to_hand(P0, "Filler Card");
    let case = build_case(&mut scenario);

    let mut runner = scenario.build();
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert!(
        !is_solved(&runner, case),
        "Case must NOT solve while the solve condition is unmet (CR 719.3a — condition gate)"
    );
}
