//! Regression for issue #864: Phyrexian Dreadnought's ETB unless-sacrifice must
//! accept creatures whose total power is at least 12.
//!
//! https://github.com/phase-rs/phase/issues/864

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{AbilityCost, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PHYREXIAN_DREADNOUGHT_ORACLE: &str = "Trample\nWhen this creature enters, sacrifice it unless you sacrifice any number of creatures with total power 12 or greater.";

fn add_mana(runner: &mut GameRunner, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

fn cast_dreadnought_to_unless_prompt(
    fodder_power: (i32, i32),
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fodder_a = scenario
        .add_creature(P0, "Fodder A", fodder_power.0, fodder_power.0)
        .id();
    let fodder_b = scenario
        .add_creature(P0, "Fodder B", fodder_power.1, fodder_power.1)
        .id();

    let dreadnought = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Phyrexian Dreadnought",
            12,
            12,
            PHYREXIAN_DREADNOUGHT_ORACLE,
        )
        .with_mana_cost(ManaCost::generic(1))
        .id();

    let mut runner = scenario.build();
    add_mana(&mut runner, &[ManaType::Colorless]);

    runner
        .act(GameAction::CastSpell {
            object_id: dreadnought,
            card_id: runner.state().objects[&dreadnought].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Phyrexian Dreadnought");

    for _ in 0..24 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected pre-resolution prompt: {other:?}"),
        }
    }

    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player: P0, .. }
        ),
        "Phyrexian Dreadnought ETB must offer unless-sacrifice, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&dreadnought].zone,
        Zone::Battlefield,
        "Dreadnought should remain on the battlefield until the unless choice resolves"
    );

    (runner, dreadnought, fodder_a, fodder_b)
}

#[test]
fn phyrexian_dreadnought_parsed_etb_carries_power_threshold_unless_cost() {
    let mut scenario = GameScenario::new();
    let dreadnought = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Phyrexian Dreadnought",
            12,
            12,
            PHYREXIAN_DREADNOUGHT_ORACLE,
        )
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let runner = scenario.build();
    let etb = runner.state().objects[&dreadnought]
        .trigger_definitions
        .iter_unchecked()
        .find(|t| t.definition.unless_pay.is_some())
        .expect("Phyrexian Dreadnought must have an ETB unless-pay trigger");
    let unless_pay = etb
        .definition
        .unless_pay
        .as_ref()
        .expect("ETB must carry unless_pay (#864)");
    assert_eq!(unless_pay.payer, TargetFilter::Controller);
    assert!(matches!(
        &unless_pay.cost,
        AbilityCost::Sacrifice(cost)
            if matches!(
                cost.requirement,
                engine::types::ability::SacrificeRequirement::Aggregate {
                    stat: engine::types::ability::SacrificeAggregateStat::TotalPower,
                    comparator: engine::types::ability::Comparator::GE,
                    value: 12,
                }
            )
    ));
}

#[test]
fn phyrexian_dreadnought_paid_unless_payment_sacrifices_fodder_and_survives() {
    let (mut runner, dreadnought, fodder_a, fodder_b) = cast_dreadnought_to_unless_prompt((6, 6));

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choose to pay sacrifice cost");

    let WaitingFor::WardSacrificeChoice {
        player,
        permanents,
        min_total_power,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "paying Phyrexian Dreadnought's unless cost must ask which creatures to sacrifice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(min_total_power, &Some(12));
    assert!(permanents.contains(&fodder_a));
    assert!(permanents.contains(&fodder_b));

    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder_a, fodder_b],
        })
        .expect("sacrifice 12 total power of creatures");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&dreadnought].zone,
        Zone::Battlefield,
        "paying the unless cost must keep Phyrexian Dreadnought on the battlefield"
    );
    assert_eq!(runner.state().objects[&fodder_a].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&fodder_b].zone, Zone::Graveyard);
}

#[test]
fn phyrexian_dreadnought_declined_unless_payment_sacrifices_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dreadnought = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Phyrexian Dreadnought",
            12,
            12,
            PHYREXIAN_DREADNOUGHT_ORACLE,
        )
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, &[ManaType::Colorless]);

    runner
        .act(GameAction::CastSpell {
            object_id: dreadnought,
            card_id: runner.state().objects[&dreadnought].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Phyrexian Dreadnought");

    for _ in 0..24 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected pre-resolution prompt: {other:?}"),
        }
    }
    runner.advance_until_stack_empty();

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline sacrifice payment");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&dreadnought].zone,
        Zone::Graveyard,
        "declining the unless cost must sacrifice Phyrexian Dreadnought"
    );
}
