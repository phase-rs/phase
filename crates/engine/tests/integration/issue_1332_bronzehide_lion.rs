//! Integration test for GitHub issue #1332: Bronzehide Lion dies with no legal
//! enchant targets must resolve cleanly (CR 303.4g → graveyard) without hanging.

use engine::game::scenario::{GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::zones::Zone;

const BRONZEHIDE_LION_ORACLE: &str =
    "{G}{W}: This creature gains indestructible until end of turn.\n\
When this creature dies, return it to the battlefield. It's an Aura enchantment with enchant \
creature you control and \"{G}{W}: Enchanted creature gains indestructible until end of turn,\" \
and it loses all other abilities.";

fn drain_to_priority(runner: &mut engine::game::scenario::GameRunner) {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 256,
            "drain exceeded safety bound; waiting_for = {:?}, stack = {}",
            runner.state().waiting_for,
            runner.state().stack.len()
        );
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
}

#[test]
fn bronzehide_lion_dies_with_no_creature_you_control_returns_then_graveyards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    let lion_id = scenario
        .add_creature_from_oracle(P0, "Bronzehide Lion", 3, 3, BRONZEHIDE_LION_ORACLE)
        .id();

    let mut runner = scenario.build();

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), lion_id, Zone::Graveyard, &mut events);
    process_triggers(runner.state_mut(), &events);

    assert_eq!(runner.state().stack.len(), 1);

    drain_to_priority(&mut runner);

    let lion = &runner.state().objects[&lion_id];
    assert_eq!(lion.zone, Zone::Graveyard);
    assert!(!lion.base_trigger_definitions.is_empty());
    assert!(runner.state().stack.is_empty());
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::ReturnAsAuraTarget { .. }
    ));
}
