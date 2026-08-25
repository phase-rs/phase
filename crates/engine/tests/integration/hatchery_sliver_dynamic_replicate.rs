//! Hatchery Sliver grants each Sliver spell Replicate for that spell's own mana cost.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AdditionalCost, AdditionalCostOrigin, AdditionalCostRepeatability,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const HATCHERY_SLIVER_ORACLE: &str = "Replicate {1}{G} (When you cast this spell, copy it for \
each time you paid its replicate cost.)\nEach Sliver spell you cast has replicate. The replicate \
cost is equal to its mana cost. (A copy of a permanent spell becomes a token.)";
const MUSCLE_SLIVER_ORACLE: &str = "All Sliver creatures get +1/+1.";

fn sliver_mana_cost() -> ManaCost {
    ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Green],
    }
}

fn add_green_mana(runner: &mut GameRunner, count: usize) {
    for _ in 0..count {
        runner.state_mut().players[P0.0 as usize]
            .mana_pool
            .add(ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]));
    }
}

fn assert_replicate_prompt(runner: &GameRunner, expected_ordinal: u32, expected_times_paid: u32) {
    let WaitingFor::OptionalCostChoice {
        cost,
        times_kicked,
        pending_cast,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected Replicate optional-cost prompt, got {:?}",
            runner.state().waiting_for
        );
    };

    assert_eq!(
        times_kicked, expected_times_paid,
        "Replicate prompt must count payments for its own keyword instance"
    );
    let instance = pending_cast
        .additional_cost_queue
        .first()
        .expect("Replicate prompt must retain its queued keyword instance");
    assert_eq!(instance.origin, AdditionalCostOrigin::Replicate);
    assert_eq!(
        instance.origin_ordinal, expected_ordinal,
        "Replicate prompt must identify the correct granted keyword instance"
    );
    let AdditionalCost::Optional {
        cost,
        repeatability,
    } = cost
    else {
        panic!("expected optional Replicate cost, got {cost:?}");
    };
    assert_eq!(repeatability, AdditionalCostRepeatability::Repeatable);
    let AbilityCost::Mana { cost } = cost else {
        panic!("expected mana Replicate cost, got {cost:?}");
    };
    assert_eq!(
        cost,
        sliver_mana_cost(),
        "Hatchery's granted Replicate must concretize to the recipient Sliver's mana cost"
    );
}

fn assert_replicate_payment_records(runner: &GameRunner, spell: ObjectId, expected: &[(u32, u32)]) {
    let payments = &runner.state().objects[&spell].additional_cost_payments;
    assert_eq!(payments.len(), expected.len());
    for (payment, (ordinal, count)) in payments.iter().zip(expected) {
        assert_eq!(payment.origin, AdditionalCostOrigin::Replicate);
        assert_eq!(payment.origin_ordinal, *ordinal);
        assert_eq!(payment.count, *count);
    }
}

fn hatchery_grants_replicate_scenario(grants: usize) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    for _ in 0..grants {
        let mut hatchery =
            scenario.add_creature_from_oracle(P0, "Hatchery Sliver", 2, 2, HATCHERY_SLIVER_ORACLE);
        hatchery
            .with_mana_cost(sliver_mana_cost())
            .with_subtypes(vec!["Sliver"]);
    }

    let muscle = scenario
        .add_creature_to_hand_from_oracle(P0, "Muscle Sliver", 1, 1, MUSCLE_SLIVER_ORACLE)
        .with_mana_cost(sliver_mana_cost())
        .with_subtypes(vec!["Sliver"])
        .id();

    (scenario.build(), muscle)
}

fn cast_creature(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the Sliver must enter the normal casting pipeline");
}

fn muscle_token_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|object| object.name == "Muscle Sliver" && object.is_token)
        .count()
}

#[test]
fn hatchery_granted_replicate_charges_the_recipient_slivers_mana_cost_once() {
    let (mut runner, muscle) = hatchery_grants_replicate_scenario(1);
    add_green_mana(&mut runner, 4); // Muscle's {1}{G}, then one granted Replicate {1}{G}.

    cast_creature(&mut runner, muscle);
    assert_replicate_prompt(&runner, 0, 0);

    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("paying the granted Replicate cost must be accepted");
    assert_replicate_prompt(&runner, 0, 1);

    runner
        .act(GameAction::DecideOptionalCost { pay: false })
        .expect("declining another Replicate payment must finish casting");

    assert_replicate_payment_records(&runner, muscle, &[(0, 1)]);
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "the recipient's base and granted Replicate costs must consume all four green mana"
    );

    runner.advance_until_stack_empty();
    assert_eq!(
        muscle_token_count(&runner),
        1,
        "one granted Replicate payment must create one Muscle Sliver token"
    );
}

#[test]
fn two_hatchery_grants_keep_replicate_payments_on_distinct_ordinals() {
    let (mut runner, muscle) = hatchery_grants_replicate_scenario(2);
    add_green_mana(&mut runner, 6); // Muscle's {1}{G}, then one {1}{G} payment for each grant.

    cast_creature(&mut runner, muscle);
    assert_replicate_prompt(&runner, 0, 0);

    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("first granted Replicate payment must be accepted");
    assert_replicate_prompt(&runner, 0, 1);

    runner
        .act(GameAction::DecideOptionalCost { pay: false })
        .expect("declining another payment for the first grant must advance the queue");
    assert_replicate_prompt(&runner, 1, 0);

    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("second granted Replicate payment must be accepted");
    assert_replicate_prompt(&runner, 1, 1);

    runner
        .act(GameAction::DecideOptionalCost { pay: false })
        .expect("declining another payment for the second grant must finish casting");

    assert_replicate_payment_records(&runner, muscle, &[(0, 1), (1, 1)]);
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "the recipient's base and two granted Replicate costs must consume all six green mana"
    );

    runner.advance_until_stack_empty();
    assert_eq!(
        muscle_token_count(&runner),
        2,
        "two independent granted Replicate payments must create two Muscle Sliver tokens"
    );
}

#[test]
fn printed_hatchery_replicate_keeps_its_fixed_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hatchery = scenario
        .add_creature_to_hand_from_oracle(P0, "Hatchery Sliver", 2, 2, HATCHERY_SLIVER_ORACLE)
        .with_mana_cost(sliver_mana_cost())
        .with_subtypes(vec!["Sliver"])
        .id();
    let mut runner = scenario.build();
    add_green_mana(&mut runner, 4); // Hatchery's {1}{G}, then printed Replicate {1}{G}.

    cast_creature(&mut runner, hatchery);
    assert_replicate_prompt(&runner, 0, 0);

    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("paying printed Replicate must be accepted");
    assert_replicate_prompt(&runner, 0, 1);

    runner
        .act(GameAction::DecideOptionalCost { pay: false })
        .expect("declining another printed Replicate payment must finish casting");

    assert_replicate_payment_records(&runner, hatchery, &[(0, 1)]);
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);

    runner.advance_until_stack_empty();
    let hatchery_tokens = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|object| object.name == "Hatchery Sliver" && object.is_token)
        .count();
    assert_eq!(
        hatchery_tokens, 1,
        "one printed Replicate payment must create one Hatchery Sliver token"
    );
}
