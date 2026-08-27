//! Public-resolution regressions for Equipment attachment continuations.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost,
    AdditionalCostRepeatability, ChoiceType, ControllerRef, Effect, MultiTargetSpec, QuantityExpr,
    TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
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

const PSYCHIC_PAPER_ORACLE: &str = "As this Equipment becomes attached to a creature, choose a creature card name and a creature type.\nEquipped creature has ward {1}, it can't be blocked, and its name and creature type are the last chosen name and creature type.\nEquip {2}";

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

/// CR 601.2b/c + CR 608.2c: paid AdditionalCostPaidInstead attachments select
/// their two roles while casting, then carry those exact bindings to the root
/// effect that replaces the base spell on resolution.
fn paid_instead_attach_definition() -> AbilityDefinition {
    AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp).sub_ability(
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Attach {
                attachment: TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact)
                        .subtype("Equipment".to_string())
                        .controller(ControllerRef::You),
                ),
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        )
        .condition(AbilityCondition::AdditionalCostPaidInstead),
    )
}

fn paid_instead_attach_fixture() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let equipment = scenario
        .add_artifact_from_oracle(P0, "Paid Instead Equipment", "")
        .with_subtypes(vec!["Equipment"])
        .id();
    let host = scenario.add_creature(P0, "Paid Instead Host", 2, 2).id();
    scenario.add_basic_land(P0, ManaColor::Green);
    let spell = scenario
        .add_spell_to_hand(P0, "Paid Instead Attach", false)
        .with_mana_cost(ManaCost::zero())
        .with_additional_cost(AdditionalCost::Kicker {
            costs: vec![AbilityCost::Mana {
                cost: ManaCost::generic(1),
            }],
            repeatability: AdditionalCostRepeatability::Once,
        })
        .with_ability_definition(paid_instead_attach_definition())
        .id();
    (scenario.build(), spell, equipment, host)
}

fn cast_paid_instead_attach_to_target_selection(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast must begin");

    for _ in 0..4 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .expect("kicker cost must be paid");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("zero mana payment must complete");
            }
            WaitingFor::TargetSelection { .. } => return,
            ref other => panic!("expected paid attachment target selection, got {other:?}"),
        }
    }
    panic!("paid attachment cast did not reach target selection");
}

fn assert_paid_attachment_bindings_and_resolve(
    runner: &mut GameRunner,
    equipment: ObjectId,
    host: ObjectId,
) {
    let ability = runner
        .state()
        .stack
        .back()
        .and_then(|entry| entry.ability())
        .expect("paid spell must be on the stack");
    assert_ne!(
        ability.context.attach_target_bindings,
        Default::default(),
        "the AdditionalCostPaidInstead root must retain the child Attach role bindings"
    );

    for _ in 0..4 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("paid attachment spell must resolve");
    }
    assert_eq!(
        runner.state().objects[&equipment].attached_to,
        Some(AttachTarget::Object(host)),
        "the paid override must resolve the chosen Equipment and host roles"
    );
}

#[test]
fn paid_instead_attach_select_targets_preserves_role_bindings_to_resolution() {
    let (mut runner, spell, equipment, host) = paid_instead_attach_fixture();
    cast_paid_instead_attach_to_target_selection(&mut runner, spell);
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(equipment), TargetRef::Object(host)],
        })
        .expect("bulk target submission must complete the paid attachment cast");
    assert_paid_attachment_bindings_and_resolve(&mut runner, equipment, host);
}

#[test]
fn paid_instead_attach_choose_target_preserves_role_bindings_to_resolution() {
    let (mut runner, spell, equipment, host) = paid_instead_attach_fixture();
    cast_paid_instead_attach_to_target_selection(&mut runner, spell);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(equipment)),
        })
        .expect("first role target must be accepted");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(host)),
        })
        .expect("second role target must complete the paid attachment cast");
    assert_paid_attachment_bindings_and_resolve(&mut runner, equipment, host);
}

#[test]
fn attached_replacement_between_bound_attachments_preserves_remaining_attachment_and_tail() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Attachment Host", 2, 2).id();
    let first = scenario
        .add_artifact_from_oracle(P0, "Psychic Paper", PSYCHIC_PAPER_ORACLE)
        .with_subtypes(vec!["Equipment"])
        .id();
    let second = scenario
        .add_artifact_from_oracle(P0, "Second Equipment", "")
        .with_subtypes(vec!["Equipment"])
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Attach Both", false)
        .with_mana_cost(ManaCost::zero())
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Attach {
                    attachment: TargetFilter::Any,
                    target: TargetFilter::Any,
                },
            )
            .multi_target(MultiTargetSpec::fixed(2, 2))
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )),
        )
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_card_names = vec!["Llanowar Elves".to_string()].into();
    let life_before = runner.life(P0);

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("attach-both spell must begin casting");
    runner
        .act(GameAction::SelectTargets {
            targets: vec![
                TargetRef::Object(first),
                TargetRef::Object(second),
                TargetRef::Object(host),
            ],
        })
        .expect("both attachment roles and their host must be selectable");

    let mut saw_attached_replacement = false;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("attachment spell must keep resolving");
            }
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::CardName,
                ..
            } => {
                runner
                    .act(GameAction::ChooseOption {
                        choice: "Llanowar Elves".to_string(),
                    })
                    .expect("first attachment replacement name choice must be accepted");
                saw_attached_replacement = true;
            }
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::CreatureType { .. },
                ..
            } => {
                runner
                    .act(GameAction::ChooseOption {
                        choice: "Zombie".to_string(),
                    })
                    .expect("first attachment replacement type choice must be accepted");
            }
            other => panic!("unexpected attach-both prompt: {other:?}"),
        }
    }

    assert!(
        saw_attached_replacement,
        "the first bound attachment must pause through its Attached replacement"
    );
    for attachment in [first, second] {
        assert_eq!(
            runner.state().objects[&attachment].attached_to,
            Some(AttachTarget::Object(host)),
            "each bound attachment must resolve after the first replacement pause"
        );
    }
    assert_eq!(
        runner.life(P0),
        life_before + 1,
        "the enclosing Attach tail must resume after both attachments"
    );
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn bound_attachments_with_a_synchronous_prefix_do_not_replay_the_final_attachment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Attachment Host", 2, 2).id();
    let first = scenario
        .add_artifact_from_oracle(P0, "First Equipment", "")
        .with_subtypes(vec!["Equipment"])
        .id();
    let second = scenario
        .add_artifact_from_oracle(P0, "Psychic Paper", PSYCHIC_PAPER_ORACLE)
        .with_subtypes(vec!["Equipment"])
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Attach Both", false)
        .with_mana_cost(ManaCost::zero())
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Attach {
                    attachment: TargetFilter::Any,
                    target: TargetFilter::Any,
                },
            )
            .multi_target(MultiTargetSpec::fixed(2, 2))
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )),
        )
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_card_names = vec!["Llanowar Elves".to_string()].into();
    let life_before = runner.life(P0);
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("attach-both spell must begin casting");
    runner
        .act(GameAction::SelectTargets {
            targets: vec![
                TargetRef::Object(first),
                TargetRef::Object(second),
                TargetRef::Object(host),
            ],
        })
        .expect("both attachment roles and their host must be selectable");

    let mut card_name_choices = 0;
    let mut creature_type_choices = 0;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("attachment spell must keep resolving");
            }
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::CardName,
                ..
            } => {
                runner
                    .act(GameAction::ChooseOption {
                        choice: "Llanowar Elves".to_string(),
                    })
                    .expect("the second attachment's card-name choice must resolve");
                card_name_choices += 1;
            }
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::CreatureType { .. },
                ..
            } => {
                runner
                    .act(GameAction::ChooseOption {
                        choice: "Zombie".to_string(),
                    })
                    .expect("the second attachment's creature-type choice must resolve");
                creature_type_choices += 1;
            }
            other => panic!("unexpected attachment prompt: {other:?}"),
        };
    }

    for attachment in [first, second] {
        assert_eq!(
            runner.state().objects[&attachment].attached_to,
            Some(AttachTarget::Object(host)),
            "each ordinary selected Equipment must attach"
        );
    }
    assert_eq!(
        runner.life(P0),
        life_before + 1,
        "the trailing effect resolves once"
    );
    assert_eq!(
        card_name_choices, 1,
        "the paused second attachment must not replay its card-name replacement"
    );
    assert_eq!(
        creature_type_choices, 1,
        "the paused second attachment must not replay its creature-type replacement"
    );
}

#[test]
fn singleton_bound_attachment_replacement_preserves_trailing_effect_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario
        .add_creature(P0, "Singleton Attachment Host", 2, 2)
        .id();
    let equipment = scenario
        .add_artifact_from_oracle(P0, "Singleton Psychic Paper", PSYCHIC_PAPER_ORACLE)
        .with_subtypes(vec!["Equipment"])
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Attach Once", false)
        .with_mana_cost(ManaCost::zero())
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Attach {
                    attachment: TargetFilter::Any,
                    target: TargetFilter::Any,
                },
            )
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )),
        )
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_card_names = vec!["Llanowar Elves".to_string()].into();
    let life_before = runner.life(P0);

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("singleton Attach spell must begin casting");
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(equipment), TargetRef::Object(host)],
        })
        .expect("the attachment and host must be selectable");

    let mut saw_attached_replacement = false;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("singleton attachment spell must keep resolving");
            }
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::CardName,
                ..
            } => {
                runner
                    .act(GameAction::ChooseOption {
                        choice: "Llanowar Elves".to_string(),
                    })
                    .expect("attachment replacement name choice must be accepted");
                saw_attached_replacement = true;
            }
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::CreatureType { .. },
                ..
            } => {
                runner
                    .act(GameAction::ChooseOption {
                        choice: "Zombie".to_string(),
                    })
                    .expect("attachment replacement type choice must be accepted");
            }
            other => panic!("unexpected singleton Attach prompt: {other:?}"),
        }
    }

    assert!(
        saw_attached_replacement,
        "the bound attachment must pause through its Attached replacement"
    );
    assert_eq!(
        runner.state().objects[&equipment].attached_to,
        Some(AttachTarget::Object(host)),
        "the bound Equipment must attach after its replacement pause"
    );
    assert_eq!(
        runner.life(P0),
        life_before + 1,
        "the trailing effect must resolve exactly once"
    );
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
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
fn gilgamesh_direct_equipment_choice_completes_to_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let samurai = scenario
        .add_creature(P0, "Test Samurai", 2, 2)
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
    let mut saw_attachment_choice = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
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
                assert_eq!(
                    cards.len(),
                    2,
                    "the direct attachment choice must be scoped to the two Equipment moved by this Dig"
                );
                assert!(cards.contains(&dug_equipment));
                assert!(cards.contains(&other_dug_equipment));
                assert!(
                    !cards.contains(&preexisting_equipment),
                    "a matching Equipment that was already on the battlefield is not 'one of them'"
                );
                let resolved = runner
                    .act(GameAction::SelectCards {
                        cards: vec![other_dug_equipment],
                    })
                    .expect("selecting the moved Equipment must be accepted");
                assert!(matches!(resolved.waiting_for, WaitingFor::Priority { .. }));
                saw_attachment_choice = true;
                assert!(saw_dig_choice, "Gilgamesh must surface the DigChoice");
                assert!(
                    saw_optional_attach,
                    "a kept Equipment entering from Gilgamesh's Dig must open the optional attachment"
                );
                assert!(
                    saw_attachment_choice,
                    "the two moved Equipment must produce a second, candidate-scoped attachment choice"
                );
                assert_eq!(
                    runner.state().objects[&other_dug_equipment].attached_to,
                    Some(AttachTarget::Object(samurai)),
                    "the selected Equipment from the Dig attaches to the selected Samurai"
                );
                assert_eq!(runner.state().objects[&dug_equipment].attached_to, None);
                assert_eq!(
                    runner.state().objects[&preexisting_equipment].attached_to,
                    None,
                    "a preexisting matching Equipment must not be attached by Gilgamesh's event-scoped choice"
                );
                return;
            }
            other => panic!("unexpected Gilgamesh resolution prompt: {other:?}"),
        }
    }

    assert!(saw_dig_choice, "Gilgamesh must surface the DigChoice");
    assert!(
        saw_optional_attach,
        "a kept Equipment entering from Gilgamesh's Dig must open the optional attachment"
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
fn gilgamesh_host_choice_then_singleton_equipment_completes_to_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let samurai = scenario
        .add_creature(P0, "Only Samurai", 2, 2)
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
    let psychic_paper_template = scenario
        .add_artifact_from_oracle(P0, "Psychic Paper Template", PSYCHIC_PAPER_ORACLE)
        .with_subtypes(vec!["Equipment"])
        .id();
    let equipment = scenario.add_card_to_library_top(P0, "Only Dug Equipment");
    let _rest = scenario.add_card_to_library_top(P0, "Library Rest");
    let mut runner = scenario.build();
    let psychic_paper_replacements = runner.state().objects[&psychic_paper_template]
        .replacement_definitions
        .clone();
    let psychic_paper_base_replacements = runner.state().objects[&psychic_paper_template]
        .base_replacement_definitions
        .clone();
    let equipment_object = runner
        .state_mut()
        .objects
        .get_mut(&equipment)
        .expect("dug Equipment exists");
    equipment_object
        .card_types
        .core_types
        .push(CoreType::Artifact);
    equipment_object
        .card_types
        .subtypes
        .push("Equipment".to_string());
    equipment_object.base_card_types = equipment_object.card_types.clone();
    equipment_object.replacement_definitions = psychic_paper_replacements;
    equipment_object.base_replacement_definitions = psychic_paper_base_replacements;
    runner.state_mut().all_card_names = vec!["Llanowar Elves".to_string()].into();

    cast_for_free(&mut runner, gilgamesh);
    let mut saw_dig_choice = false;
    let mut saw_optional_attach = false;
    let mut saw_host_choice = false;
    let mut saw_attached_replacement = false;
    for _ in 0..48 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must be accepted");
            }
            WaitingFor::DigChoice { .. } => {
                assert!(
                    !saw_host_choice,
                    "the singleton attachment must not reopen the Dig selection"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![equipment],
                    })
                    .expect("selecting the singleton Equipment must be accepted");
                saw_dig_choice = true;
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                assert!(
                    !saw_host_choice,
                    "the singleton attachment must not reopen an optional selection"
                );
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting Gilgamesh's optional attachment must work");
                saw_optional_attach = true;
            }
            WaitingFor::EffectZoneChoice { cards, .. } => {
                assert!(
                    !saw_host_choice,
                    "only the Samurai host choice is interactive"
                );
                assert!(cards.contains(&samurai));
                assert!(cards.contains(&other_samurai));
                let resolved = runner
                    .act(GameAction::SelectCards {
                        cards: vec![samurai],
                    })
                    .expect("selecting the Samurai host must consume the prompt");
                assert!(matches!(
                    resolved.waiting_for,
                    WaitingFor::NamedChoice { .. }
                ));
                saw_host_choice = true;
                assert!(saw_dig_choice, "Gilgamesh must surface the DigChoice");
                assert!(
                    saw_host_choice,
                    "multiple Samurai must require the host choice"
                );
                assert!(
                    saw_optional_attach,
                    "a kept Equipment entering from Gilgamesh's Dig must open the optional attachment"
                );
            }
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::CardName,
                ..
            } => {
                runner
                    .act(GameAction::ChooseOption {
                        choice: "Llanowar Elves".to_string(),
                    })
                    .expect("attached replacement card-name choice must be accepted");
                saw_attached_replacement = true;
            }
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::CreatureType { .. },
                ..
            } => {
                let resolved = runner
                    .act(GameAction::ChooseOption {
                        choice: "Zombie".to_string(),
                    })
                    .expect("attached replacement creature-type choice must be accepted");
                assert!(matches!(resolved.waiting_for, WaitingFor::Priority { .. }));
                assert!(
                    saw_attached_replacement,
                    "the singleton attachment must reach its replacement prompt"
                );
                assert_eq!(
                    runner.state().objects[&equipment].attached_to,
                    Some(AttachTarget::Object(samurai)),
                    "the singleton forwarded attachment must resume through its enclosing continuation"
                );
                return;
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                panic!("Gilgamesh's non-targeted Samurai choice must not be stack targeting")
            }
            other => panic!("unexpected Gilgamesh singleton prompt: {other:?}"),
        }
    }

    assert!(saw_dig_choice, "Gilgamesh must surface the DigChoice");
    assert!(
        saw_optional_attach,
        "a kept Equipment entering from Gilgamesh's Dig must open the optional attachment"
    );
    assert!(
        saw_host_choice,
        "multiple Samurai must require the host choice"
    );
    assert!(
        saw_attached_replacement,
        "the singleton forwarded attachment must enter the attached-event replacement path"
    );
    assert_eq!(
        runner.state().objects[&equipment].attached_to,
        Some(AttachTarget::Object(samurai)),
        "the singleton Equipment must attach after its host choice without retaining a stale prompt"
    );
}

#[test]
fn gilgamesh_host_then_equipment_choice_preserves_event_scoped_candidates() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let samurai = scenario
        .add_creature(P0, "Chosen Samurai", 2, 2)
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
    let first_equipment = scenario.add_card_to_library_top(P0, "First Dug Equipment");
    let second_equipment = scenario.add_card_to_library_top(P0, "Second Dug Equipment");
    let _rest = scenario.add_card_to_library_top(P0, "Library Rest");
    let mut runner = scenario.build();
    for equipment_id in [first_equipment, second_equipment] {
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
    for _ in 0..48 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must be accepted");
            }
            WaitingFor::DigChoice { .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![first_equipment, second_equipment],
                    })
                    .expect("selecting the two dug Equipment must be accepted");
                saw_dig_choice = true;
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting Gilgamesh's optional attachment must work");
                saw_optional_attach = true;
            }
            WaitingFor::EffectZoneChoice { cards, .. } => {
                assert!(saw_dig_choice, "Gilgamesh must surface the DigChoice");
                assert!(
                    saw_optional_attach,
                    "a kept Equipment entering from Gilgamesh's Dig must open the optional attachment"
                );
                assert_eq!(
                    cards.len(),
                    2,
                    "the first choice must offer both Samurai hosts"
                );
                assert!(cards.contains(&samurai));
                assert!(cards.contains(&other_samurai));

                let after_host = runner
                    .act(GameAction::SelectCards {
                        cards: vec![samurai],
                    })
                    .expect("selecting the Samurai host must be accepted");
                let WaitingFor::EffectZoneChoice {
                    cards: equipment_cards,
                    effect_kind,
                    ..
                } = &after_host.waiting_for
                else {
                    panic!(
                        "selecting one of multiple Samurai must open the Equipment choice before resolution continues"
                    );
                };
                assert_eq!(*effect_kind, engine::types::ability::EffectKind::Attach);
                assert_eq!(
                    equipment_cards.len(),
                    2,
                    "the second choice must contain exactly the Equipment moved by this Dig"
                );
                assert!(equipment_cards.contains(&first_equipment));
                assert!(equipment_cards.contains(&second_equipment));

                let resolved = runner
                    .act(GameAction::SelectCards {
                        cards: vec![second_equipment],
                    })
                    .expect("selecting the event-scoped Equipment must be accepted");
                assert!(matches!(resolved.waiting_for, WaitingFor::Priority { .. }));
                assert_eq!(
                    runner.state().objects[&second_equipment].attached_to,
                    Some(AttachTarget::Object(samurai)),
                    "the selected moved Equipment attaches to the selected Samurai"
                );
                assert_eq!(runner.state().objects[&first_equipment].attached_to, None);
                return;
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                panic!("Gilgamesh's non-targeted choices must not be stack targeting")
            }
            other => panic!("unexpected Gilgamesh combined prompt: {other:?}"),
        }
    }

    panic!("Gilgamesh's combined host and Equipment choices did not resolve");
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
