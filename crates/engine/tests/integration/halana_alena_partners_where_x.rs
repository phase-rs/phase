//! Runtime regression for issue #1993 — Halana and Alena, Partners parsed its
//! beginning-of-combat trigger but resolved X as 0 counters.
//!
//! Oracle text:
//! > At the beginning of combat on your turn, put X +1/+1 counters on another
//! > target creature you control, where X is Halana and Alena's power. That
//! > creature gains haste until end of turn.
//!
//! The parser must bind the printed-name possessive to the ability source, and
//! the runtime counter effect must resolve that source power during trigger
//! resolution.

use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::phase::Phase;

const HALANA_ALENA_ORACLE: &str = "At the beginning of combat on your turn, put X +1/+1 counters \
on another target creature you control, where X is Halana and Alena's power. That creature gains \
haste until end of turn.";

/// CR 603.2b + CR 113.7 + CR 608.2c + CR 122.1: the beginning-of-combat
/// trigger uses the ability source's power for X and places that many counters
/// on the chosen other creature.
#[test]
fn halana_alena_partners_runtime_puts_source_power_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Halana and Alena, Partners", 3, 4, HALANA_ALENA_ORACLE)
        .id();
    let receiver = scenario.add_creature(P0, "Receiver", 1, 1).id();
    let mut runner = scenario.build();

    runner.pass_both_players();
    assert!(
        !runner.state().stack.is_empty(),
        "Halana and Alena trigger must be on the stack after beginning of combat"
    );
    runner.advance_until_stack_empty();

    let counters = runner
        .state()
        .objects
        .get(&receiver)
        .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0);
    assert_eq!(
        counters, 3,
        "where X is Halana and Alena's power must put three +1/+1 counters"
    );
}
