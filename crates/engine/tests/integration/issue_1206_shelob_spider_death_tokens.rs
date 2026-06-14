//! Issue #1206 — Shelob, Child of Ungoliant must not trigger without spider damage.
//!
//! End-to-end regression (spider damage → creature dies → Food copy token) lives in
//! `engine::game::triggers::tests::shelob_spider_damage_death_trigger_end_to_end`.

use engine::game::scenario::{GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::game_state::WaitingFor;

const SHELOB_DEATH_TRIGGER: &str = "Whenever another creature dealt damage this turn by a Spider you controlled dies, create a token that's a copy of that creature, except it's a Food artifact with \"{2}, {T}, Sacrifice ~: You gain 3 life,\" and it loses all other card types.";

fn drain_to_priority(runner: &mut engine::game::scenario::GameRunner) {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 256,
            "drain exceeded bound; waiting_for = {:?}",
            runner.state().waiting_for
        );
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                if runner
                    .act(engine::types::actions::GameAction::PassPriority)
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

fn destroy_with_lethal_damage(
    runner: &mut engine::game::scenario::GameRunner,
    object_id: engine::types::identifiers::ObjectId,
) {
    runner
        .state_mut()
        .objects
        .get_mut(&object_id)
        .unwrap()
        .damage_marked = 99;

    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_to_priority(runner);
}

#[test]
fn shelob_does_not_trigger_without_spider_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Shelob, Child of Ungoliant", 4, 4, SHELOB_DEATH_TRIGGER);
    let victim_id = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    let stack_before = runner.state().stack.len();

    destroy_with_lethal_damage(&mut runner, victim_id);

    assert_eq!(
        runner.state().stack.len(),
        stack_before,
        "Shelob must not trigger when the dying creature was not damaged by a Spider"
    );
}
