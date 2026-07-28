//! CR 602.2b / CR 601.2c-h — target-bearing sacrifice activations announce
//! their target before paying the sacrifice cost.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MOGG_FANATIC: &str = "Sacrifice this creature: It deals 1 damage to any target.";

fn setup() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fanatic = scenario
        .add_creature_from_oracle(P0, "Mogg Fanatic", 1, 1, MOGG_FANATIC)
        .id();
    let target = scenario.add_creature(P1, "Goblin Token", 1, 1).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&target)
        .unwrap()
        .is_token = true;
    (runner, fanatic, target)
}

fn activate(runner: &mut GameRunner, fanatic: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: fanatic,
            ability_index: 0,
        })
        .expect("Mogg Fanatic activation must enter target selection");
}

fn choose_target(runner: &mut GameRunner, target: ObjectId) {
    let WaitingFor::TargetSelection { target_slots, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected target selection, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(target)),
        "the Goblin token must be targetable before Mogg Fanatic is sacrificed"
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(target)],
        })
        .expect("selecting the target must succeed");
}

fn sacrifice_fanatic(runner: &mut GameRunner, fanatic: ObjectId) {
    let WaitingFor::PayCost { kind, choices, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "expected sacrifice cost, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(kind, PayCostKind::Sacrifice);
    assert!(choices.contains(&fanatic));
    runner
        .act(GameAction::SelectCards {
            cards: vec![fanatic],
        })
        .expect("sacrificing Mogg Fanatic must succeed");
}

#[test]
fn mogg_fanatic_target_selection_precedes_sacrifice_and_cancel_is_lossless() {
    let (mut runner, fanatic, target) = setup();
    activate(&mut runner, fanatic);

    assert!(
        runner.state().battlefield.contains(&fanatic),
        "the sacrifice cost cannot be paid before target selection"
    );
    choose_target(&mut runner, target);
    sacrifice_fanatic(&mut runner, fanatic);

    assert_eq!(runner.state().objects[&fanatic].zone, Zone::Graveyard);
    runner.advance_until_stack_empty();
    assert!(
        !runner.state().battlefield.contains(&target),
        "the announced Goblin token must take the activation's damage"
    );

    let (mut runner, fanatic, target) = setup();
    activate(&mut runner, fanatic);
    runner
        .act(GameAction::CancelCast)
        .expect("target selection must be cancelable");
    assert!(runner.state().battlefield.contains(&fanatic));
    assert!(runner.state().battlefield.contains(&target));
    assert!(runner.state().stack.is_empty());
}

#[test]
fn mogg_fanatic_self_target_fizzles_after_its_sacrifice_cost() {
    let (mut runner, fanatic, _target) = setup();
    activate(&mut runner, fanatic);
    choose_target(&mut runner, fanatic);
    sacrifice_fanatic(&mut runner, fanatic);

    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&fanatic].zone, Zone::Graveyard);
    assert!(
        runner.state().stack.is_empty(),
        "the ability must leave the stack after its only target became illegal"
    );
}
