//! Regression for issue #4459: Decoy Gambit's unless-have-you-draw alternative
//! must draw for the spell's controller and suppress the bounce when paid.
//!
//! https://github.com/phase-rs/phase/issues/4459

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DECOY_GAMBIT_BOUNCE_ORACLE: &str =
    "Return target creature to its owner's hand unless its controller has you draw a card.";

fn add_mana(runner: &mut engine::game::scenario::GameRunner, mana: &[ManaType]) {
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

fn cast_decoy_gambit_to_unless_prompt(
    runner: &mut engine::game::scenario::GameRunner,
    gambit: ObjectId,
    creature: ObjectId,
) {
    runner
        .act(GameAction::CastSpell {
            object_id: gambit,
            card_id: runner.state().objects[&gambit].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Decoy Gambit");

    for _ in 0..24 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![engine::types::ability::TargetRef::Object(creature)],
                    })
                    .expect("target P1's creature");
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
        "Decoy Gambit must offer the creature controller an unless-payment prompt, got {:?}",
        runner.state().waiting_for
    );
}

fn setup_at_unless_prompt() -> (engine::game::scenario::GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Library Top");

    let gambit = scenario
        .add_spell_to_hand_from_oracle(P0, "Decoy Gambit", true, DECOY_GAMBIT_BOUNCE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .id();

    let creature = scenario.add_creature(P1, "Test Bear", 2, 2).id();

    let mut runner = scenario.build();
    add_mana(
        &mut runner,
        &[ManaType::Colorless, ManaType::Colorless, ManaType::Blue],
    );
    cast_decoy_gambit_to_unless_prompt(&mut runner, gambit, creature);
    (runner, gambit, creature)
}

#[test]
fn decoy_gambit_declined_unless_payment_bounces_targeted_creature() {
    let (mut runner, _gambit, creature) = setup_at_unless_prompt();
    let hand_before = runner.state().players[P1.0 as usize].hand.len();

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline draw alternative");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Hand,
        "declining the unless cost must return the targeted creature to its owner's hand"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        0,
        "declining the unless cost must not draw for the spell's controller"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].hand.len(),
        hand_before + 1,
        "declining the unless cost must put the creature in the controller's hand"
    );
}

#[test]
fn decoy_gambit_paid_unless_payment_draws_for_caster_and_spares_creature() {
    let (mut runner, _gambit, creature) = setup_at_unless_prompt();

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("accept draw alternative");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "paying the unless cost must prevent the bounce"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        1,
        "paying the unless cost must have the spell's controller draw a card"
    );
    assert!(
        runner
            .state()
            .objects
            .values()
            .any(|obj| obj.zone == Zone::Hand && obj.name == "Library Top"),
        "the drawn card must come from the caster's library"
    );
}
