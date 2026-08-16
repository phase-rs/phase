//! Issue #5159: "Whenever a creature you control attacks alone, investigate"
//! (Agent 13, Sharon Carter) must fire only when exactly one creature attacks.
//!
//! https://github.com/phase-rs/phase/issues/5159

use engine::types::phase::Phase;

use super::rules::{run_combat, GameScenario, P0};

const ORACLE: &str = "Whenever a creature you control attacks alone, investigate.";

fn count_clues_on_battlefield(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|obj| obj.name.eq_ignore_ascii_case("Clue"))
        })
        .count()
}

#[test]
fn issue_5159_solo_attack_investigates_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let agent = scenario
        .add_creature_from_oracle(P0, "Agent 13, Sharon Carter", 2, 2, ORACLE)
        .id();
    let mut runner = scenario.build();

    run_combat(&mut runner, vec![agent], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        count_clues_on_battlefield(&runner),
        1,
        "solo attack must investigate exactly once"
    );
}

#[test]
fn issue_5159_multi_attack_does_not_investigate() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let agent = scenario
        .add_creature_from_oracle(P0, "Agent 13, Sharon Carter", 2, 2, ORACLE)
        .id();
    let other = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();
    let mut runner = scenario.build();

    run_combat(&mut runner, vec![agent, other], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        count_clues_on_battlefield(&runner),
        0,
        "multi-creature attack must not trigger attacks-alone investigate"
    );
}
