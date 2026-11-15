//! Dwarven Armorer's real printed activation chooses one of two P/T counter
//! branches after its target and costs have been committed.

use crate::support::shared_card_db;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DWARVEN_ARMORER: &str = "Dwarven Armorer";

fn counter_count(runner: &GameRunner, object: ObjectId, counter: CounterType) -> u32 {
    runner
        .state()
        .objects
        .get(&object)
        .and_then(|card| card.counters.get(&counter).copied())
        .unwrap_or(0)
}

fn activate_armorer_branch(choice_index: usize) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let armorer = scenario.add_real_card(P0, DWARVEN_ARMORER, Zone::Battlefield, db);
    let discard = scenario.add_real_card(P0, "Mountain", Zone::Hand, db);
    let target = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );

    let mut runner = scenario.build();
    runner
        .activate(armorer, 0)
        .target_object(target)
        .pay_with(&[discard])
        .resolve();

    match &runner.state().waiting_for {
        WaitingFor::ChooseOneOfBranch { branches, .. } => assert_eq!(
            branches.len(),
            2,
            "Dwarven Armorer must offer both printed counter branches"
        ),
        other => panic!("expected Dwarven Armorer counter choice, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseBranch {
            index: choice_index,
        })
        .expect("choosing Dwarven Armorer's counter branch must succeed");
    runner.advance_until_stack_empty();

    (runner, armorer, discard, target)
}

#[test]
fn dwarven_armorer_plus_zero_plus_one_branch_pays_costs_and_applies_only_that_counter() {
    let (runner, armorer, discard, target) = activate_armorer_branch(0);

    assert_eq!(
        counter_count(
            &runner,
            target,
            CounterType::PowerToughness {
                power: 0,
                toughness: 1,
            },
        ),
        1
    );
    assert_eq!(
        counter_count(
            &runner,
            target,
            CounterType::PowerToughness {
                power: 1,
                toughness: 0,
            },
        ),
        0
    );
    assert!(runner.state().objects[&armorer].tapped);
    assert_eq!(runner.state().objects[&discard].zone, Zone::Graveyard);
}

#[test]
fn dwarven_armorer_plus_one_plus_zero_branch_pays_costs_and_applies_only_that_counter() {
    let (runner, armorer, discard, target) = activate_armorer_branch(1);

    assert_eq!(
        counter_count(
            &runner,
            target,
            CounterType::PowerToughness {
                power: 1,
                toughness: 0,
            },
        ),
        1
    );
    assert_eq!(
        counter_count(
            &runner,
            target,
            CounterType::PowerToughness {
                power: 0,
                toughness: 1,
            },
        ),
        0
    );
    assert!(runner.state().objects[&armorer].tapped);
    assert_eq!(runner.state().objects[&discard].zone, Zone::Graveyard);
}
