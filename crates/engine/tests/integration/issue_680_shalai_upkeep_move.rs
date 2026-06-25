//! Issue #680 — Shalai and Hallar + Forgotten Ancient upkeep move counters.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const SHALAI_AND_HALLAR: &str = "Flying, vigilance\n\
Whenever one or more +1/+1 counters are put on a creature you control, \
Shalai and Hallar deals that much damage to target opponent.";

const FORGOTTEN_ANCIENT: &str = "Whenever a player casts a spell, you may put a \
+1/+1 counter on this creature.\n\
At the beginning of your upkeep, you may move any number of +1/+1 counters from \
this creature onto other creatures.";

fn p1p1(state: &engine::types::game_state::GameState, id: ObjectId) -> u32 {
    state
        .objects
        .get(&id)
        .unwrap()
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

#[test]
fn issue_680_upkeep_move_counters_triggers_shalai_damage_per_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    scenario.add_creature_from_oracle(P0, "Shalai and Hallar", 3, 3, SHALAI_AND_HALLAR);
    let ancient = scenario
        .add_creature_from_oracle(P0, "Forgotten Ancient", 0, 0, FORGOTTEN_ANCIENT)
        .with_plus_counters(2)
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let p1_life_before = runner.state().players[P1.0 as usize].life;

    runner.auto_advance_to_main_phase();

    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 50, "stuck at {:?}", runner.state().waiting_for);
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept upkeep move");
            }
            WaitingFor::MoveCountersDistribution { .. } => {
                runner
                    .act(GameAction::ChooseCounterMoveDistribution {
                        selections: vec![engine::types::game_state::CounterMoveChoice {
                            destination_id: bear,
                            counter_type: CounterType::Plus1Plus1,
                            count: 2,
                        }],
                    })
                    .expect("move both counters to bear");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    })
                    .expect("pick opponent for Shalai damage");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    assert_eq!(p1p1(runner.state(), ancient), 0);
    assert_eq!(p1p1(runner.state(), bear), 2);
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        p1_life_before - 2,
        "moving 2 counters onto Bear must trigger Shalai for 2 damage total"
    );
}
