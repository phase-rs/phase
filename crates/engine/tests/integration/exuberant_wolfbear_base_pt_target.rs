//! Exuberant Wolfbear's attack trigger targets a Human its controller controls
//! and sets that Human's base power and toughness to the attacker's current P/T
//! until end of turn.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

use super::rules::AttackTarget;

const WOLFBEAR_ORACLE: &str = "Whenever this creature attacks, you may change the base power and toughness of target Human you control to this creature's power and toughness until end of turn.";

fn power_toughness(runner: &GameRunner, object_id: ObjectId) -> (Option<i32>, Option<i32>) {
    let object = runner
        .state()
        .objects
        .get(&object_id)
        .expect("test permanent must remain on the battlefield");
    (object.power, object.toughness)
}

fn setup() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wolfbear = scenario
        .add_creature_from_oracle(P0, "Exuberant Wolfbear", 7, 5, WOLFBEAR_ORACLE)
        .id();
    let chosen_human = scenario
        .add_creature(P0, "Chosen Human", 1, 1)
        .with_subtypes(vec!["Human"])
        .id();
    let other_human = scenario
        .add_creature(P0, "Other Human", 1, 1)
        .with_subtypes(vec!["Human"])
        .id();
    let opponent_human = scenario
        .add_creature(P1, "Opponent Human", 1, 1)
        .with_subtypes(vec!["Human"])
        .id();

    (
        scenario.build(),
        wolfbear,
        chosen_human,
        other_human,
        opponent_human,
    )
}

fn attack_until_target_selection(runner: &mut GameRunner, wolfbear: ObjectId) {
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(wolfbear, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("Wolfbear attack declaration must succeed");

    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { source_id, .. } => {
                assert_eq!(
                    source_id,
                    Some(wolfbear),
                    "Wolfbear must own the target prompt"
                );
                return;
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            other => {
                panic!("expected Wolfbear's target selection, got waiting state {other:?}")
            }
        }
    }
    panic!("Wolfbear attack trigger did not reach target selection");
}

fn target_selected_until_optional_choice(runner: &mut GameRunner, wolfbear: ObjectId) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { source_id, .. } => {
                assert_eq!(source_id, wolfbear, "Wolfbear must own the may prompt");
                return;
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            other => panic!("expected Wolfbear's optional choice, got waiting state {other:?}"),
        }
    }
    panic!("Wolfbear attack trigger did not reach its optional choice");
}

fn finish_current_turn(runner: &mut GameRunner, turn_number: u32) {
    for _ in 0..32 {
        if runner.state().turn_number > turn_number {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("defender may declare no blockers");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected state while advancing to cleanup: {other:?}"),
        }
    }
    panic!("turn did not advance through cleanup");
}

#[test]
fn exuberant_wolfbear_accepts_only_controlled_humans_and_expires_at_cleanup() {
    let (mut runner, wolfbear, chosen_human, other_human, opponent_human) = setup();
    let current_turn = runner.state().turn_number;

    attack_until_target_selection(&mut runner, wolfbear);

    let chosen_target = match runner.state().waiting_for.clone() {
        WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        } => {
            let legal_targets = &target_slots[selection.current_slot].legal_targets;
            let legal_object_ids: Vec<_> = legal_targets
                .iter()
                .filter_map(|target| match target {
                    engine::types::ability::TargetRef::Object(id) => Some(*id),
                    engine::types::ability::TargetRef::Player(_) => None,
                })
                .collect();
            assert!(
                legal_object_ids.contains(&chosen_human) && legal_object_ids.contains(&other_human),
                "both controlled Humans must be legal targets: {legal_targets:?}"
            );
            assert!(
                !legal_object_ids.contains(&opponent_human),
                "opponent's Human must not be a legal target: {legal_targets:?}"
            );
            legal_targets
                .iter()
                .find(|target| {
                    matches!(target, engine::types::ability::TargetRef::Object(id) if *id == chosen_human)
                })
                .cloned()
                .expect("chosen controlled Human must be offered by the engine")
        }
        other => panic!("expected target selection after accepting Wolfbear: {other:?}"),
    };
    runner
        .act(GameAction::ChooseTarget {
            target: Some(chosen_target),
        })
        .expect("select the engine-offered controlled Human target");
    target_selected_until_optional_choice(&mut runner, wolfbear);
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("controller may accept Wolfbear's trigger");
    runner.advance_until_stack_empty();

    assert_eq!(
        power_toughness(&runner, chosen_human),
        (Some(7), Some(5)),
        "selected Human must take Wolfbear's 7/5 base P/T this turn"
    );
    assert_eq!(
        power_toughness(&runner, other_human),
        (Some(1), Some(1)),
        "unchosen controlled Human must stay 1/1"
    );
    assert_eq!(
        power_toughness(&runner, opponent_human),
        (Some(1), Some(1)),
        "opponent's Human must stay 1/1"
    );

    finish_current_turn(&mut runner, current_turn);
    assert_eq!(
        power_toughness(&runner, chosen_human),
        (Some(1), Some(1)),
        "until-end-of-turn base P/T must expire during cleanup"
    );
}

#[test]
fn exuberant_wolfbear_decline_leaves_controlled_human_unchanged() {
    let (mut runner, wolfbear, chosen_human, _other_human, _opponent_human) = setup();

    attack_until_target_selection(&mut runner, wolfbear);
    let chosen_target = match runner.state().waiting_for.clone() {
        WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        } => target_slots[selection.current_slot]
            .legal_targets
            .iter()
            .find(|target| {
                matches!(target, engine::types::ability::TargetRef::Object(id) if *id == chosen_human)
            })
            .cloned()
            .expect("chosen controlled Human must be offered by the engine"),
        other => panic!("expected target selection before optional choice: {other:?}"),
    };
    runner
        .act(GameAction::ChooseTarget {
            target: Some(chosen_target),
        })
        .expect("select the engine-offered controlled Human target");
    target_selected_until_optional_choice(&mut runner, wolfbear);
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("controller may decline Wolfbear's trigger");
    runner.advance_until_stack_empty();

    assert_eq!(
        power_toughness(&runner, chosen_human),
        (Some(1), Some(1)),
        "declining Wolfbear's optional trigger must leave the Human unchanged"
    );
}
