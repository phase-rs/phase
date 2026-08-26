//! Regression for issue #5286: Lulu, Loyal Hollyphant end-step trigger must put
//! +1/+1 counters on each tapped creature you control and untap those same
//! creatures when a permanent you controlled left the battlefield this turn.
//!
//! https://github.com/phase-rs/phase/issues/5286

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const LULU_ORACLE: &str = "Flying\n\
At the beginning of your end step, if a permanent you controlled left the battlefield this turn, \
put a +1/+1 counter on each tapped creature you control, then untap them.\n\
Choose a Background (You can have a Background as a second commander.)";

fn drive_end_step_stack(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare attackers");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .ok();
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::End => {
                if runner.state().stack.is_empty() {
                    return;
                }
                runner.act(GameAction::PassPriority).ok();
            }
            _ if runner.state().phase == Phase::End && runner.state().stack.is_empty() => return,
            _ => runner.pass_both_players(),
        }
    }
}

#[test]
fn issue_5286_lulu_counters_and_untaps_tapped_creatures_after_revolt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let lulu = scenario
        .add_creature_from_oracle(P0, "Lulu, Loyal Hollyphant", 3, 2, LULU_ORACLE)
        .id();
    let leaver = scenario.add_creature(P0, "Leaver", 1, 1).id();
    let tapped_a = scenario.add_creature(P0, "Tapped A", 2, 2).id();
    let tapped_b = scenario.add_creature(P0, "Tapped B", 2, 2).id();
    let _untapped = scenario.add_creature(P0, "Untapped", 2, 2).id();

    let mut runner = scenario.build();

    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), leaver, Zone::Graveyard, &mut events);

    for id in [tapped_a, tapped_b] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }

    runner.advance_to_end_step();
    drive_end_step_stack(&mut runner);

    for id in [tapped_a, tapped_b] {
        let obj = runner.state().objects.get(&id).unwrap();
        assert_eq!(
            obj.counters.get(&CounterType::Plus1Plus1).copied(),
            Some(1),
            "tapped creature {id:?} should receive a +1/+1 counter"
        );
        assert!(
            !obj.tapped,
            "tapped creature {id:?} should be untapped by Lulu's trigger"
        );
    }

    let untapped_obj = runner.state().objects.get(&_untapped).unwrap();
    assert!(
        !untapped_obj.counters.contains_key(&CounterType::Plus1Plus1),
        "untapped creatures must not receive counters"
    );
    assert!(
        !untapped_obj.tapped,
        "already-untapped creature should stay untapped"
    );
    assert!(
        !runner.state().objects[&lulu].tapped,
        "Lulu herself must not be the sole beneficiary of the untap sub"
    );
}

#[test]
fn issue_5286_lulu_does_nothing_without_revolt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Lulu, Loyal Hollyphant", 3, 2, LULU_ORACLE)
        .id();
    let tapped = scenario.add_creature(P0, "Tapped Soldier", 2, 2).id();
    let _opponent_creature = scenario.add_creature(P1, "Opp Creature", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&tapped).unwrap().tapped = true;

    runner.advance_to_end_step();
    drive_end_step_stack(&mut runner);

    let obj = runner.state().objects.get(&tapped).unwrap();
    assert!(
        !obj.counters.contains_key(&CounterType::Plus1Plus1),
        "without a revolt event, Lulu must not place counters"
    );
    assert!(
        obj.tapped,
        "without a revolt event, tapped creatures stay tapped"
    );
}
