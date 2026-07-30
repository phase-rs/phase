//! Regression for issue #4921: Skullscorch's targeted player may have the source
//! deal damage instead of discarding two cards at random.
//!
//! https://github.com/phase-rs/phase/issues/4921

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const SKULLSCORCH_ORACLE: &str =
    "Target player discards two cards at random unless that player has Skullscorch deal 4 damage to them.";

fn cast_skullscorch_to_unless_prompt(
    runner: &mut engine::game::scenario::GameRunner,
    skullscorch: ObjectId,
) {
    runner
        .act(GameAction::CastSpell {
            object_id: skullscorch,
            card_id: runner.state().objects[&skullscorch].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Skullscorch");

    for _ in 0..24 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(P1)],
                    })
                    .expect("target P1");
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
        "Skullscorch must offer the targeted player an unless-payment prompt, got {:?}",
        runner.state().waiting_for
    );
}

fn setup_at_unless_prompt() -> engine::game::scenario::GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_cards_in_hand(P1, &["Hand Card A", "Hand Card B", "Hand Card C"]);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ],
    );

    let skullscorch = scenario
        .add_spell_to_hand_from_oracle(P0, "Skullscorch", true, SKULLSCORCH_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    cast_skullscorch_to_unless_prompt(&mut runner, skullscorch);
    runner
}

#[test]
fn skullscorch_declined_unless_payment_discards_two_cards() {
    let mut runner = setup_at_unless_prompt();
    let hand_before = runner.state().players[P1.0 as usize].hand.len();
    assert!(
        hand_before >= 2,
        "P1 needs at least two cards to discard, had {hand_before}"
    );

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline damage alternative");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].hand.len(),
        hand_before - 2,
        "declining the unless cost must discard two cards at random"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        20,
        "declining the unless cost must not deal damage"
    );
}

#[test]
fn skullscorch_paid_unless_payment_deals_damage_and_spares_hand() {
    let mut runner = setup_at_unless_prompt();
    let hand_before = runner.state().players[P1.0 as usize].hand.len();

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("accept damage alternative");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        16,
        "paying the unless cost must have Skullscorch deal 4 damage to the targeted player"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].hand.len(),
        hand_before,
        "paying the unless cost must prevent the random discard"
    );
}
