//! Issue #5339: Armory Automaton — ETB/attack attach trigger cannot be skipped.
//!
//! Oracle: "Whenever this creature enters or attacks, you may attach any number
//! of target Equipment to it."
//!
//! The controller must be able to:
//! 1. Decline the optional "you may" entirely (no attachments).
//! 2. Accept the optional but choose zero Equipment when some are legal.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ARMORY_ORACLE: &str = "Whenever this creature enters or attacks, you may attach any number of target Equipment to it.";

fn add_equipment(
    runner: &mut engine::game::scenario::GameRunner,
    name: &str,
    card_id: u64,
) -> engine::types::identifiers::ObjectId {
    let id = create_object(
        runner.state_mut(),
        CardId(card_id),
        P0,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = runner.state_mut().objects.get_mut(&id).expect("equipment");
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes.push("Equipment".to_string());
    id
}

fn setup_etb() -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let armory = scenario
        .add_creature_to_hand_from_oracle(P0, "Armory Automaton", 2, 2, ARMORY_ORACLE)
        .id();
    (scenario.build(), armory)
}

#[test]
fn declining_optional_attach_on_etb_resolves_without_stall() {
    let (mut runner, armory) = setup_etb();
    let equipment = add_equipment(&mut runner, "Bonesplitter", 99);

    runner.cast(armory).resolve();

    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("declining optional attach");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() || runner.act(GameAction::PassPriority).is_err()
                {
                    break;
                }
            }
            WaitingFor::TriggerTargetSelection { .. }
            | WaitingFor::EffectZoneChoice { .. }
            | WaitingFor::TargetSelection { .. } => {
                panic!(
                    "declining optional attach must not surface further targeting prompts, got {:?}",
                    runner.state().waiting_for
                );
            }
            _ => break,
        }
    }

    assert!(
        runner.state().stack.is_empty(),
        "stack must empty after declining optional attach"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&equipment)
            .unwrap()
            .attached_to
            .is_none(),
        "declining must leave Equipment unattached"
    );
}

#[test]
fn accepting_optional_with_zero_equipment_targets_on_etb_resolves() {
    let (mut runner, armory) = setup_etb();
    let equipment = add_equipment(&mut runner, "Skullclamp", 100);

    runner.cast(armory).resolve();

    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting optional attach");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget { target: None })
                    .expect("skipping zero Equipment targets");
            }
            WaitingFor::EffectZoneChoice { .. } => {
                runner
                    .act(GameAction::SelectCards { cards: vec![] })
                    .expect("confirming zero Equipment selection");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() || runner.act(GameAction::PassPriority).is_err()
                {
                    break;
                }
            }
            other => panic!("unexpected waiting state: {other:?}"),
        }
    }

    assert!(
        runner
            .state()
            .objects
            .get(&equipment)
            .unwrap()
            .attached_to
            .is_none(),
        "choosing zero Equipment must leave it unattached"
    );
    assert!(
        runner.state().stack.is_empty(),
        "ability must fully resolve after zero-target attach"
    );
}

#[test]
fn attack_trigger_can_be_skipped_without_stall() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let armory = scenario
        .add_creature_from_oracle(P0, "Armory Automaton", 2, 2, ARMORY_ORACLE)
        .id();
    let mut runner = scenario.build();
    let equipment = add_equipment(&mut runner, "Bonesplitter", 101);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(armory, AttackTarget::Player(P1))])
        .expect("declare attacker");

    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget { target: None })
                    .expect("skipping zero Equipment on attack trigger");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("declining attack trigger");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() || runner.act(GameAction::PassPriority).is_err()
                {
                    break;
                }
            }
            WaitingFor::EffectZoneChoice { .. } => {
                panic!(
                    "zero-target attack attach must not prompt EffectZoneChoice, got {:?}",
                    runner.state().waiting_for
                );
            }
            other => panic!("unexpected waiting state: {other:?}"),
        }
    }

    assert!(
        runner
            .state()
            .objects
            .get(&equipment)
            .unwrap()
            .attached_to
            .is_none(),
        "skipping attack attach must leave Equipment unattached"
    );
    assert!(runner.state().stack.is_empty());
}
