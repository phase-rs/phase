//! Dyadrine, Synthesis Amalgam — attack-triggered non-targeted counter choice.
//!
//! CR 508.2 + CR 608.2c: declaring an attack puts the trigger on the stack;
//! accepting its `may` instruction then chooses exactly two eligible creatures
//! during resolution, removes counters from both, draws, and creates the Robot.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const DYADRINE_ORACLE: &str = "Whenever you attack, you may remove a +1/+1 counter from each of two creatures you control. If you do, draw a card and create a 2/2 colorless Robot artifact creature token.";

#[test]
fn dyadrine_attack_acceptance_selects_creatures_then_removes_draws_and_creates_robot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dyadrine = scenario
        .add_creature_from_oracle(P0, "Dyadrine, Synthesis Amalgam", 2, 2, DYADRINE_ORACLE)
        .id();
    let first = {
        let mut builder = scenario.add_creature(P0, "First Counter Bearer", 1, 1);
        builder.with_plus_counters(1);
        builder.id()
    };
    let second = {
        let mut builder = scenario.add_creature(P0, "Second Counter Bearer", 1, 1);
        builder.with_plus_counters(1);
        builder.id()
    };
    let third = {
        let mut builder = scenario.add_creature(P0, "Third Counter Bearer", 1, 1);
        builder.with_plus_counters(1);
        builder.id()
    };
    scenario.with_library_top(P0, &["Dyadrine Draw"]);
    let mut runner = scenario.build();
    let hand_before = runner.state().players[0].hand.len();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(dyadrine, AttackTarget::Player(P1))])
        .expect("Dyadrine attacks");

    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order Dyadrine's attack trigger");
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                assert_eq!(player, P0, "the trigger controller chooses the may action");
                break;
            }
            WaitingFor::Priority { .. } => {
                runner.pass_both_players();
            }
            other => panic!("unexpected state before Dyadrine's may choice: {other:?}"),
        }
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "Dyadrine's counter-removal instruction must be offered as a may choice"
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept Dyadrine's counter-removal instruction");
    let WaitingFor::ChooseObjectsSelection {
        eligible, min, max, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "accepting Dyadrine must prompt a non-targeted creature selection, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        (min, max),
        (2, Some(2)),
        "must choose exactly two creatures"
    );
    assert_eq!(
        eligible,
        vec![
            TargetRef::Object(first),
            TargetRef::Object(second),
            TargetRef::Object(third),
        ],
        "only creatures carrying removable +1/+1 counters are selectable"
    );

    assert!(
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(first)],
            })
            .is_err(),
        "an exact-two selection must reject a one-creature submission"
    );

    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(first), TargetRef::Object(second)],
        })
        .expect("select the two counter-bearing creatures");
    runner.advance_until_stack_empty();

    for id in [first, second] {
        assert_eq!(
            runner.state().objects[&id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or_default(),
            0,
            "each selected creature loses its +1/+1 counter"
        );
    }
    assert_eq!(
        runner.state().objects[&third]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or_default(),
        1,
        "the eligible but unselected third creature retains its counter"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 1,
        "the accepted and completed removal draws one card"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .values()
            .filter(|object| {
                object.is_token
                    && object.zone == Zone::Battlefield
                    && object.controller == P0
                    && object
                        .card_types
                        .subtypes
                        .iter()
                        .any(|subtype| subtype == "Robot")
            })
            .count(),
        1,
        "the completed removal creates one Robot token"
    );
}
