//! Scythecat Cub doubles the counters placed by its second landfall resolution.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

const SCYTHECAT_CUB_ORACLE: &str = "Trample\n\
Landfall — Whenever a land you control enters, put a +1/+1 counter on target creature you control. If this is the second time this ability has resolved this turn, double the number of +1/+1 counters on that creature instead.";

fn resolve_cub_landfall(
    runner: &mut engine::game::scenario::GameRunner,
    land_id: engine::types::identifiers::ObjectId,
    target_id: engine::types::identifiers::ObjectId,
) {
    let card_id = runner.state().objects[&land_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("land should be playable");

    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { target_slots, .. } => {
            assert!(
                target_slots[0]
                    .legal_targets
                    .iter()
                    .any(|target| matches!(target, TargetRef::Object(id) if *id == target_id)),
                "Lotus Cobra must be a legal Scythecat Cub target"
            );
        }
        other => panic!("expected Scythecat Cub target selection, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target_id)),
        })
        .expect("Lotus Cobra should be selectable");
    runner.advance_until_stack_empty();
}

#[test]
fn scythecat_cub_doubles_counters_on_second_landfall_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let cub_id = scenario
        .add_creature_from_oracle(P0, "Scythecat Cub", 2, 2, SCYTHECAT_CUB_ORACLE)
        .id();
    let cobra_id = scenario.add_creature(P0, "Lotus Cobra", 2, 1).id();
    let first_land_id = scenario.add_land_to_hand(P0, "First Land").id();
    let second_land_id = scenario.add_land_to_hand(P0, "Second Land").id();

    let mut runner = scenario.build();
    runner.state_mut().max_lands_per_turn = 2;

    resolve_cub_landfall(&mut runner, first_land_id, cobra_id);
    assert_eq!(
        runner.state().objects[&cobra_id]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(1),
        "the first landfall resolution should place one +1/+1 counter"
    );

    resolve_cub_landfall(&mut runner, second_land_id, cobra_id);
    assert_eq!(
        runner.state().objects[&cobra_id]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(2),
        "the second landfall resolution should double Lotus Cobra's counter"
    );
    assert_eq!(
        runner
            .state()
            .ability_resolutions_this_turn
            .get(&(cub_id, 0)),
        Some(&2),
        "both landfall triggers should share Cub's printed ability index"
    );
}
