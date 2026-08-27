//! Public-resolution regressions for Equipment attachment continuations.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const SOKKA_AND_SUKI_ORACLE: &str = "Whenever Sokka and Suki or another Ally you control enters, \
attach up to one target Equipment you control to that creature.\n\
Whenever an Equipment you control enters, create a 1/1 white Ally creature token.";

const GILGAMESH_ORACLE: &str =
    "Whenever Gilgamesh enters or attacks, look at the top six cards of your library. \
You may put any number of Equipment cards from among them onto the battlefield. Put the rest on \
the bottom of your library in a random order. When you put one or more Equipment onto the \
battlefield this way, you may attach one of them to a Samurai you control.";

const HAMMER_OF_NAZAHN_ORACLE: &str =
    "Whenever Hammer of Nazahn or another Equipment you control enters, \
you may attach that Equipment to target creature you control.\n\
Equipped creature gets +2/+0 and has indestructible.\n\
Equip {4}";

fn cast_for_free(runner: &mut GameRunner, object_id: ObjectId) {
    let card_id = runner.state().objects[&object_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("free cast must be accepted");
}

fn choose_trigger_target(runner: &mut GameRunner, target: ObjectId) {
    runner
        .act(GameAction::SelectTargets {
            targets: vec![engine::types::ability::TargetRef::Object(target)],
        })
        .expect("trigger target selection must be accepted");
}

#[test]
fn sokka_and_suki_event_context_attaches_the_selected_equipment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _sokka = scenario
        .add_creature_from_oracle(P0, "Sokka and Suki", 3, 3, SOKKA_AND_SUKI_ORACLE)
        .with_subtypes(vec!["Human", "Warrior", "Ally"])
        .id();
    let swordsmans_steel = scenario
        .add_artifact_from_oracle(P0, "Swordsman's Steel", "")
        .with_subtypes(vec!["Equipment"])
        .id();
    let other_equipment = scenario
        .add_artifact_from_oracle(P0, "Second Equipment", "")
        .with_subtypes(vec!["Equipment"])
        .id();
    let entering_ally = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Ally", 1, 1, "")
        .with_subtypes(vec!["Ally"])
        .with_mana_cost(ManaCost::default())
        .id();
    let mut runner = scenario.build();

    cast_for_free(&mut runner, entering_ally);
    let mut saw_attachment_target = false;
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if saw_attachment_target && runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must be accepted");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                choose_trigger_target(&mut runner, swordsmans_steel);
                saw_attachment_target = true;
            }
            WaitingFor::EffectZoneChoice { .. } => {
                panic!(
                    "Sokka's explicit target Equipment must be selected while placing the trigger on the stack"
                );
            }
            other => panic!("unexpected Sokka and Suki resolution prompt: {other:?}"),
        }
    }

    assert!(
        saw_attachment_target,
        "Sokka's trigger must require selecting one of the two legal Equipment targets"
    );
    assert_eq!(
        runner.state().objects[&swordsmans_steel].attached_to,
        Some(AttachTarget::Object(entering_ally)),
        "the selected Equipment attaches to the Ally carried by the trigger event"
    );
    assert_eq!(
        runner.state().objects[&other_equipment].attached_to,
        None,
        "the unselected Equipment must remain unattached"
    );
}

#[test]
fn gilgamesh_dig_kept_equipment_reaches_optional_samurai_attachment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let samurai = scenario
        .add_creature(P0, "Test Samurai", 2, 2)
        .with_subtypes(vec!["Samurai"])
        .id();
    let other_samurai = scenario
        .add_creature(P0, "Other Samurai", 2, 2)
        .with_subtypes(vec!["Samurai"])
        .id();
    let gilgamesh = scenario
        .add_creature_to_hand_from_oracle(P0, "Gilgamesh, Master-at-Arms", 3, 3, GILGAMESH_ORACLE)
        .with_subtypes(vec!["Human", "Warrior"])
        .with_mana_cost(ManaCost::default())
        .id();
    let preexisting_equipment = scenario
        .add_artifact_from_oracle(P0, "Preexisting Equipment", "")
        .with_subtypes(vec!["Equipment"])
        .id();
    let dug_equipment = scenario.add_card_to_library_top(P0, "Swordsman's Steel");
    let other_dug_equipment = scenario.add_card_to_library_top(P0, "Second Dug Equipment");
    let _rest = scenario.add_card_to_library_top(P0, "Library Rest");
    let mut runner = scenario.build();
    for equipment_id in [dug_equipment, other_dug_equipment] {
        let equipment = runner
            .state_mut()
            .objects
            .get_mut(&equipment_id)
            .expect("dug Equipment exists");
        equipment.card_types.core_types.push(CoreType::Artifact);
        equipment.card_types.subtypes.push("Equipment".to_string());
        equipment.base_card_types = equipment.card_types.clone();
    }

    cast_for_free(&mut runner, gilgamesh);
    let mut saw_dig_choice = false;
    let mut saw_optional_attach = false;
    let mut saw_host_choice = false;
    let mut saw_attachment_choice = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if saw_attachment_choice && runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must be accepted");
            }
            WaitingFor::DigChoice { cards, .. } => {
                assert!(
                    cards.contains(&dug_equipment),
                    "Dig must offer the Equipment"
                );
                assert!(
                    cards.contains(&other_dug_equipment),
                    "Dig must offer every moved Equipment candidate"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![dug_equipment, other_dug_equipment],
                    })
                    .expect("selecting the dug Equipment candidates must be accepted");
                saw_dig_choice = true;
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting Gilgamesh's optional attachment must work");
                saw_optional_attach = true;
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                panic!(
                    "Gilgamesh says 'a Samurai', not 'target Samurai'; its Samurai choice must wait for resolution"
                );
            }
            WaitingFor::EffectZoneChoice { cards, .. } => {
                if !saw_host_choice {
                    assert!(
                        cards.contains(&samurai),
                        "Gilgamesh's resolution-time host choice must offer the chosen Samurai"
                    );
                    assert!(
                        cards.contains(&other_samurai),
                        "Gilgamesh's host choice must offer every legal Samurai"
                    );
                    runner
                        .act(GameAction::SelectCards {
                            cards: vec![samurai],
                        })
                        .expect("selecting the Samurai host must be accepted");
                    saw_host_choice = true;
                } else {
                    assert_eq!(
                        cards.len(),
                        2,
                        "the attachment choice must be scoped to the two Equipment moved by this Dig"
                    );
                    assert!(cards.contains(&dug_equipment));
                    assert!(cards.contains(&other_dug_equipment));
                    assert!(
                        !cards.contains(&preexisting_equipment),
                        "a matching Equipment that was already on the battlefield is not 'one of them'"
                    );
                    runner
                        .act(GameAction::SelectCards {
                            cards: vec![other_dug_equipment],
                        })
                        .expect("selecting the moved Equipment must be accepted");
                    saw_attachment_choice = true;
                }
            }
            other => panic!("unexpected Gilgamesh resolution prompt: {other:?}"),
        }
    }

    assert!(saw_dig_choice, "Gilgamesh must surface the DigChoice");
    assert!(
        saw_optional_attach,
        "a kept Equipment entering from Gilgamesh's Dig must open the optional attachment"
    );
    assert!(
        saw_host_choice,
        "the optional attachment must first prompt for a Samurai host"
    );
    assert_eq!(
        runner.state().objects[&other_dug_equipment].attached_to,
        Some(AttachTarget::Object(samurai)),
        "the selected Equipment from the Dig attaches to the selected Samurai"
    );
    assert!(
        saw_attachment_choice,
        "the two moved Equipment must produce a second, candidate-scoped attachment choice"
    );
    assert_eq!(runner.state().objects[&dug_equipment].attached_to, None);
    assert_eq!(
        runner.state().objects[&preexisting_equipment].attached_to,
        None,
        "a preexisting matching Equipment must not be attached by Gilgamesh's event-scoped choice"
    );
}

#[test]
fn hammer_of_nazahn_parent_target_etb_remains_attached() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P0, "Test Creature", 2, 2).id();
    let hammer = scenario
        .add_artifact_to_hand_from_oracle(P0, "Hammer of Nazahn", HAMMER_OF_NAZAHN_ORACLE)
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(ManaCost::default())
        .id();
    let mut runner = scenario.build();

    cast_for_free(&mut runner, hammer);
    let mut saw_optional_attach = false;
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if saw_optional_attach && runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must be accepted");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                choose_trigger_target(&mut runner, creature)
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting Hammer's attachment must work");
                saw_optional_attach = true;
            }
            other => panic!("unexpected Hammer of Nazahn resolution prompt: {other:?}"),
        }
    }

    assert!(
        saw_optional_attach,
        "Hammer's ETB must offer its optional attachment"
    );
    assert_eq!(
        runner.state().objects[&hammer].attached_to,
        Some(AttachTarget::Object(creature)),
        "Hammer's ParentTarget event helper must still attach the entering Equipment"
    );
}
