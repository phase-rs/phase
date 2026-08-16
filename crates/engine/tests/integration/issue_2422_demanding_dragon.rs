//! Regression for issue #2422: Demanding Dragon's ETB unless-sacrifice must
//! prompt the targeted opponent before dealing damage.
//!
//! https://github.com/phase-rs/phase/issues/2422

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DEMANDING_DRAGON_ORACLE: &str = "Flying\nWhen this creature enters, it deals 5 damage to target opponent unless that player sacrifices a creature of their choice.";

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

fn cast_demanding_dragon_to_unless_prompt() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario
        .add_creature_from_oracle(P1, "P1 Bear", 2, 2, "")
        .id();

    let dragon = scenario
        .add_creature_to_hand_from_oracle(P0, "Demanding Dragon", 5, 5, DEMANDING_DRAGON_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 3,
        })
        .id();

    let mut runner = scenario.build();
    add_mana(
        &mut runner,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Red,
            ManaType::Red,
        ],
    );

    runner
        .act(GameAction::CastSpell {
            object_id: dragon,
            card_id: runner.state().objects[&dragon].card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Demanding Dragon");

    for _ in 0..24 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("choose P1 as target opponent");
            }
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
            WaitingFor::UnlessPayment { player: P1, .. }
        ),
        "Demanding Dragon ETB must offer target-player unless-sacrifice before dealing damage, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P1)
            .unwrap()
            .life,
        20,
        "no damage should be dealt before the unless choice"
    );

    (runner, bear)
}

#[test]
fn demanding_dragon_parsed_etb_carries_unless_sacrifice_for_target_player() {
    let mut scenario = GameScenario::new();
    let dragon = scenario
        .add_creature_to_hand_from_oracle(P0, "Demanding Dragon", 5, 5, DEMANDING_DRAGON_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 3,
        })
        .id();
    let runner = scenario.build();
    let etb = runner.state().objects[&dragon]
        .trigger_definitions
        .iter_unchecked()
        .find(|t| t.definition.unless_pay.is_some())
        .expect("Demanding Dragon must have an ETB unless-pay trigger");
    let unless_pay = etb
        .definition
        .unless_pay
        .as_ref()
        .expect("ETB must carry unless_pay (#2422)");
    assert_eq!(unless_pay.payer, TargetFilter::Player);
    assert!(matches!(unless_pay.cost, AbilityCost::Sacrifice(_)));
}

#[test]
fn demanding_dragon_declined_unless_payment_deals_damage_to_targeted_opponent() {
    let (mut runner, bear) = cast_demanding_dragon_to_unless_prompt();
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline sacrifice payment");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P1)
            .unwrap()
            .life,
        15,
        "declining the target-player unless cost must allow the 5 damage"
    );
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Battlefield,
        "declining the payment must not sacrifice the targeted opponent's creature"
    );
}

#[test]
fn demanding_dragon_paid_unless_payment_sacrifices_creature_and_prevents_damage() {
    let (mut runner, bear) = cast_demanding_dragon_to_unless_prompt();
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choose to pay sacrifice cost");

    let WaitingFor::WardSacrificeChoice {
        player, permanents, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "paying Demanding Dragon's unless cost must ask P1 which creature to sacrifice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1);
    assert_eq!(permanents.as_slice(), [bear]);

    runner
        .act(GameAction::SelectCards { cards: vec![bear] })
        .expect("sacrifice P1 Bear");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P1)
            .unwrap()
            .life,
        20,
        "paying the unless cost must prevent Demanding Dragon's damage"
    );
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Graveyard,
        "paying the unless cost must sacrifice P1's chosen creature"
    );
}
