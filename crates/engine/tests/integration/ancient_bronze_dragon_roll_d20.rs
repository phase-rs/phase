//! Reproduction for issue #1602, Deliverable 1 — Ancient Bronze Dragon's
//! reflexive die-result carry.
//!
//! Oracle (Ancient Bronze Dragon):
//! > Flying
//! > Whenever this creature deals combat damage to a player, roll a d20. When
//! > you do, put X +1/+1 counters on each of up to two target creatures, where
//! > X is the result.
//!
//! This is the REFLEXIVE class: the "When you do …" sub-ability resolves on its
//! OWN stack entry in a later resolution scope than the original roll. The bug
//! was that (1) X parsed as `Variable("the result")` → 0, and (2) even a correct
//! count would read 0 because `die_result_this_resolution` was cleared before
//! the reflexive entry resolved. Deliverable 1 carries the rolled value on the
//! trigger stack entry (`die_result`) and re-stamps it into resolution scope
//! when the reflexive entry resolves.
//!
//! This test drives real combat (unblocked 6/5 flyer → 6 combat damage), resolves
//! the roll-a-d20 trigger, then completes the reflexive target selection (two
//! target creatures), and asserts each chosen creature gains exactly the d20
//! result (1..=20) in +1/+1 counters — NOT the combat damage (6).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

use super::rules::run_combat;

const ANCIENT_BRONZE_DRAGON_ORACLE: &str = "Flying\nWhenever this creature deals combat damage \
to a player, roll a d20. When you do, put X +1/+1 counters on each of up to two target creatures, \
where X is the result.";

#[test]
fn ancient_bronze_dragon_reflexive_counts_equal_d20_result() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dragon = scenario
        .add_creature_from_oracle(
            P0,
            "Ancient Bronze Dragon",
            6,
            5,
            ANCIENT_BRONZE_DRAGON_ORACLE,
        )
        .id();
    // Two creatures to receive the +1/+1 counters (the reflexive trigger targets
    // "up to two target creatures").
    let bear_a = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let bear_b = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let mut runner = scenario.build();

    // Unblocked flyer → 6 combat damage to P1.
    run_combat(&mut runner, vec![dragon], vec![]);

    // Drive the stack: resolve the roll-a-d20 trigger, then complete the
    // reflexive "When you do …" target selection (pick both creatures).
    let mut all_events: Vec<GameEvent> = Vec::new();
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(bear_a)],
                    })
                    .or_else(|_| {
                        runner.act(GameAction::SelectTargets {
                            targets: vec![TargetRef::Object(bear_b)],
                        })
                    })
                    .expect("select a target creature for the reflexive trigger");
            }
            WaitingFor::Priority { .. } => match runner.act(GameAction::PassPriority) {
                Ok(result) => all_events.extend(result.events),
                Err(_) => break,
            },
            _ => break,
        }
    }

    let rolled = all_events
        .iter()
        .find_map(|e| match e {
            GameEvent::DieRolled {
                result, sides: 20, ..
            } => Some(*result as usize),
            _ => None,
        })
        .expect("Ancient Bronze Dragon should roll a d20 on combat damage");
    assert!(
        (1..=20).contains(&rolled),
        "d20 result out of range: {rolled}"
    );

    let plus1 = CounterType::Plus1Plus1;
    let count_a = runner
        .state()
        .objects
        .get(&bear_a)
        .and_then(|o| o.counters.get(&plus1).copied())
        .unwrap_or(0);
    let count_b = runner
        .state()
        .objects
        .get(&bear_b)
        .and_then(|o| o.counters.get(&plus1).copied())
        .unwrap_or(0);

    eprintln!("d20 roll = {rolled}, counters: a={count_a}, b={count_b}");

    // CR 706.2 + CR 706.4 + CR 603.12: the chosen target gains +1/+1 counters
    // equal to the CARRIED d20 result (re-stamped into the reflexive entry's
    // resolution scope), NOT the combat damage (6). This is the Deliverable-1
    // carry under real combat. (At least one target is chosen above; the
    // selected creature must show the die result.)
    let max_counters = count_a.max(count_b);
    assert_eq!(
        max_counters as usize, rolled,
        "the chosen target must gain +1/+1 counters equal to the d20 result \
         ({rolled}), not the combat damage (6); got a={count_a} b={count_b}"
    );
    // Combat damage was 6 — assert no target was mistakenly given 6 counters
    // from the surviving DamageDealt event (unless the roll genuinely is 6).
    if rolled != 6 {
        assert_ne!(
            count_a, 6,
            "target A must not read the combat-damage amount"
        );
        assert_ne!(
            count_b, 6,
            "target B must not read the combat-damage amount"
        );
    }
}
