use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CLOUD_EX_SOLDIER: &str = "Whenever ~ attacks, draw a card for each equipped attacking creature you control. Then if ~ has power 7 or greater, create two Treasure tokens.";

fn treasure_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.controller == P0
                && obj.zone == Zone::Battlefield
                && obj.is_token
                && obj.name == "Treasure"
        })
        .count()
}

fn cloud_board(
    power: i32,
    coattacker_power: Option<i32>,
) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cloud = scenario
        .add_creature_from_oracle(P0, "Cloud, Ex-SOLDIER", power, 4, CLOUD_EX_SOLDIER)
        .id();
    let coattacker =
        coattacker_power.map(|power| scenario.add_creature(P0, "High-Power Ally", power, 4).id());
    (scenario.build(), cloud, coattacker)
}

fn attack_cloud(runner: &mut GameRunner, cloud: ObjectId, coattacker: Option<ObjectId>) {
    runner.advance_to_combat();
    let mut attacks = vec![(cloud, AttackTarget::Player(P1))];
    if let Some(coattacker) = coattacker {
        attacks.push((coattacker, AttackTarget::Player(P1)));
    }
    runner
        .declare_attackers(&attacks)
        .expect("declaring Cloud as an attacker must succeed");
}

fn set_power(runner: &mut GameRunner, object: ObjectId, power: i32) {
    let obj = runner
        .state_mut()
        .objects
        .get_mut(&object)
        .expect("object exists");
    obj.base_power = Some(power);
    obj.power = Some(power);
}

#[test]
fn cloud_live_power_at_resolution_creates_two_treasures() {
    let (mut runner, cloud, _) = cloud_board(6, None);
    attack_cloud(&mut runner, cloud, None);
    set_power(&mut runner, cloud, 7);

    runner.advance_until_stack_empty();

    assert_eq!(
        treasure_count(&runner),
        2,
        "Cloud's then-if gate must read source live power at resolution"
    );
}

#[test]
fn cloud_live_power_below_threshold_creates_no_treasures() {
    let (mut runner, cloud, _) = cloud_board(7, None);
    attack_cloud(&mut runner, cloud, None);
    set_power(&mut runner, cloud, 6);

    runner.advance_until_stack_empty();

    assert_eq!(
        treasure_count(&runner),
        0,
        "Cloud attacked with 7 power, but the resolution-time source power was below the gate"
    );
}

#[test]
fn cloud_ignores_high_power_coattacker_for_source_condition() {
    let (mut runner, cloud, coattacker) = cloud_board(3, Some(9));
    attack_cloud(&mut runner, cloud, coattacker);

    runner.advance_until_stack_empty();

    assert_eq!(
        treasure_count(&runner),
        0,
        "a hostile high-power coattacker must not satisfy Cloud's source-power condition"
    );
}
