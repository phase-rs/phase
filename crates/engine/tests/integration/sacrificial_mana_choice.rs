use engine::ai_support::candidate_actions;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, Effect, ManaContribution,
    ManaProduction, QuantityExpr, QuantityRef, SacrificeCost, TargetFilter, TypeFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    CastPaymentMode, GameState, ManaChoice, ManaChoiceContext, ManaChoicePrompt,
    ParkedActivationNodeConflict, PayCostKind, PendingCostMoveResume, PendingManaAbility,
    PersistedGameState, PersistedRestoreError, WaitingFor,
};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::{
    ManaColor, ManaCost, ManaSourceOutput, ManaSourcePenalty, ManaSourceQuantity, ManaType,
};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::{RulesExecutionNodeKind, RulesExecutionNodeRef};

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

fn sacrificial_mana_ability(cost: AbilityCost, color: ManaColor) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![color],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(cost)
}

fn any_one_color_sacrificial_mana_ability(cost: AbilityCost) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::AnyOneColor {
                count: QuantityExpr::Fixed { value: 1 },
                color_options: vec![ManaColor::Black, ManaColor::Red],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(cost)
}

fn begin_sacrificial_payment(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::AutoExceptSacrificialMana,
        })
        .expect("the production cast path should stop before sacrificial mana");
}

fn offered_selection(
    runner: &GameRunner,
    source: ObjectId,
) -> engine::types::mana::ManaSourceSelection {
    let WaitingFor::ManaSourceSelection { options, .. } = &runner.state().waiting_for else {
        panic!(
            "expected sacrificial mana prompt from the cast path, got {:?}",
            runner.state().waiting_for
        );
    };
    options
        .iter()
        .find(|selection| selection.source.object_id == source)
        .cloned()
        .expect("the prompt should retain the sacrificial source's exact capability")
}

fn generic_spell(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand(P0, "Sacrificial Mana Payment Witness", true)
        .with_mana_cost(ManaCost::generic(1))
        .id()
}

/// CR 601.2h + CR 602.2b + CR 605.3b: Each Petal is a separate irreversible
/// payment choice; its color is chosen only after its sacrifice cost is paid.
#[test]
fn two_real_lotus_petals_offer_distinct_nominal_one_mana_sources() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let first = scenario
        .add_artifact_from_oracle(
            P0,
            "Lotus Petal",
            "{T}, Sacrifice this artifact: Add one mana of any color.",
        )
        .id();
    let second = scenario
        .add_artifact_from_oracle(
            P0,
            "Lotus Petal",
            "{T}, Sacrifice this artifact: Add one mana of any color.",
        )
        .id();
    let mut runner = scenario.build();

    begin_sacrificial_payment(&mut runner, spell);
    let WaitingFor::ManaSourceSelection { options, .. } = &runner.state().waiting_for else {
        panic!("the real Petals must reach the sacrificial payment prompt");
    };
    assert_eq!(
        options.len(),
        2,
        "each real Petal offers one distinct deferred activation"
    );
    let serialized = serde_json::to_value(&runner.state().waiting_for).unwrap();
    let restored: WaitingFor = serde_json::from_value(serialized).unwrap();
    assert_eq!(restored, runner.state().waiting_for);
    let first_selection = offered_selection(&runner, first);
    let second_selection = offered_selection(&runner, second);
    assert_ne!(first_selection.source, second_selection.source);
    for selection in [&first_selection, &second_selection] {
        assert_eq!(
            selection.output,
            ManaSourceOutput::DeferredColorChoice {
                quantity: ManaSourceQuantity::Fixed(1)
            }
        );
        assert_eq!(selection.penalty, ManaSourcePenalty::Sacrifices);
        assert_eq!(selection.mana_type, ManaType::Colorless);
    }

    // CR 400.7: A Petal that leaves and returns is a new object. The old
    // option must fail exact live revalidation while the other Petal remains
    // an activatable choice in the same payment prompt.
    let mut stale_runner = GameRunner::from_state(runner.state().clone());
    let mut zone_events = Vec::new();
    engine::game::zones::move_to_zone(
        stale_runner.state_mut(),
        second,
        Zone::Graveyard,
        &mut zone_events,
    );
    engine::game::zones::move_to_zone(
        stale_runner.state_mut(),
        second,
        Zone::Battlefield,
        &mut zone_events,
    );
    assert_ne!(
        stale_runner.state().objects[&second].incarnation,
        second_selection.source.incarnation
    );
    assert!(stale_runner
        .act(GameAction::ActivateManaSource {
            selection: second_selection.clone()
        })
        .is_err());
    assert_eq!(
        stale_runner.state().objects[&second].zone,
        Zone::Battlefield
    );
    stale_runner
        .act(GameAction::ActivateManaSource {
            selection: first_selection,
        })
        .expect("the other Petal remains an exact live choice");

    let mut tampered = second_selection.clone();
    tampered.output = ManaSourceOutput::DeferredColorChoice {
        quantity: ManaSourceQuantity::Fixed(99),
    };
    assert!(runner
        .act(GameAction::ActivateManaSource {
            selection: tampered
        })
        .is_err());
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
    let mut wrong_ability = second_selection.clone();
    wrong_ability.ability_index = Some(99);
    assert!(runner
        .act(GameAction::ActivateManaSource {
            selection: wrong_ability
        })
        .is_err());
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);

    runner
        .act(GameAction::ActivateManaSource {
            selection: second_selection,
        })
        .expect("the selected real Petal activates");
    assert!(matches!(
        &runner.state().waiting_for,
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::SingleColor { options }, ..
        } if options.contains(&ManaType::Blue)
    ));
    assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        })
        .expect("the post-cost color choice resumes the cast");
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Blue),
        1
    );
    runner
        .act(GameAction::PassPriority)
        .expect("the chosen mana pays the spell");
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
}

/// CR 106.1a + CR 605.3b: The nominal count comes from the printed mana
/// production, while the actual chosen mana is produced after sacrifice.
#[test]
fn real_black_lotus_advertises_three_and_produces_three_after_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let lotus = scenario
        .add_artifact_from_oracle(
            P0,
            "Black Lotus",
            "{T}, Sacrifice this artifact: Add three mana of any one color.",
        )
        .id();
    let mut runner = scenario.build();

    begin_sacrificial_payment(&mut runner, spell);
    let selection = offered_selection(&runner, lotus);
    assert_eq!(
        selection.output,
        ManaSourceOutput::DeferredColorChoice {
            quantity: ManaSourceQuantity::Fixed(3)
        }
    );
    runner
        .act(GameAction::ActivateManaSource { selection })
        .expect("the real Lotus activates");
    assert!(matches!(
        &runner.state().waiting_for,
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::SingleColor { options }, ..
        } if options.contains(&ManaType::Red)
    ));
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Red),
            count: 1,
        })
        .expect("the post-cost color choice produces three mana");
    assert_eq!(runner.state().objects[&lotus].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Red),
        3
    );
    runner
        .act(GameAction::PassPriority)
        .expect("one chosen mana pays the pending spell");
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Red),
        2
    );
}

/// CR 601.2h + CR 605.3b: A count that reads the battlefield is resolved
/// after sacrificing the source, so the pre-cost descriptor stays variable.
#[test]
fn dynamic_flexible_source_does_not_snapshot_pre_sacrifice_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let source = scenario
        .add_artifact_from_oracle(P0, "Dynamic Mana Witness", "")
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyOneColor {
                        count: QuantityExpr::Ref {
                            qty: QuantityRef::ObjectCount {
                                filter: TargetFilter::Typed(
                                    TypedFilter::new(TypeFilter::Artifact)
                                        .controller(ControllerRef::You),
                                ),
                            },
                        },
                        color_options: vec![ManaColor::Blue, ManaColor::Red],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::SelfRef,
                1,
            ))),
        )
        .id();
    let other = scenario
        .add_artifact_from_oracle(P0, "Other Artifact Witness", "")
        .id();
    let mut runner = scenario.build();

    begin_sacrificial_payment(&mut runner, spell);
    let selection = offered_selection(&runner, source);
    assert_eq!(
        selection.output,
        ManaSourceOutput::DeferredColorChoice {
            quantity: ManaSourceQuantity::Variable
        }
    );
    runner
        .act(GameAction::ActivateManaSource { selection })
        .expect("the dynamic source enters post-cost choice");
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&other].zone, Zone::Battlefield);
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        })
        .expect("only the surviving artifact counts at activation");
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Blue),
        1
    );
    runner
        .act(GameAction::PassPriority)
        .expect("the mana pays the pending spell");
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
}

/// CR 605.3b: A single feasible color resolves without a choice prompt even
/// though the pre-cost source descriptor is deferred and nominally fixed.
#[test]
fn single_feasible_deferred_color_resolves_without_choose_mana_color() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let source = scenario
        .add_artifact_from_oracle(P0, "Single Color Witness", "")
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: vec![ManaColor::Red],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::SelfRef,
                1,
            ))),
        )
        .id();
    let mut runner = scenario.build();

    begin_sacrificial_payment(&mut runner, spell);
    let selection = offered_selection(&runner, source);
    assert_eq!(
        selection.output,
        ManaSourceOutput::DeferredColorChoice {
            quantity: ManaSourceQuantity::Fixed(1),
        }
    );
    runner
        .act(GameAction::ActivateManaSource { selection })
        .expect("the only legal color is produced without a color prompt");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Red),
        1
    );
    runner
        .act(GameAction::PassPriority)
        .expect("the mana pays the spell");
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
}

/// CR 106.1a + CR 605.3b: An independent-color amount uses the existing
/// AnyCombination prompt after the same nominal descriptor is selected.
#[test]
fn deferred_independent_colors_keep_the_any_combination_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let source = scenario
        .add_artifact_from_oracle(P0, "Independent Colors Witness", "")
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyCombination {
                        count: QuantityExpr::Fixed { value: 2 },
                        color_options: vec![ManaColor::Red, ManaColor::Blue],
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::SelfRef,
                1,
            ))),
        )
        .id();
    let mut runner = scenario.build();

    begin_sacrificial_payment(&mut runner, spell);
    let selection = offered_selection(&runner, source);
    assert_eq!(
        selection.output,
        ManaSourceOutput::DeferredColorChoice {
            quantity: ManaSourceQuantity::Fixed(2),
        }
    );
    runner
        .act(GameAction::ActivateManaSource { selection })
        .expect("the independent-color source enters its original prompt");
    assert!(matches!(
        &runner.state().waiting_for,
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::AnyCombination { count: 2, options }, ..
        } if options.contains(&ManaType::Red) && options.contains(&ManaType::Blue)
    ));
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::Combination(vec![ManaType::Red, ManaType::Blue]),
            count: 1,
        })
        .expect("the independent colors are produced after the cost");
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 2);
    runner
        .act(GameAction::PassPriority)
        .expect("one mana pays the spell");
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
}

/// CR 602.2b + CR 605.3b: Two printed mana abilities on one source remain
/// distinct choices even when both defer their color until after their cost.
#[test]
fn same_source_deferred_abilities_keep_their_ability_indices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let cost = AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1));
    let source = scenario
        .add_artifact_from_oracle(P0, "Two Ability Witness", "")
        .with_ability_definition(any_one_color_sacrificial_mana_ability(cost.clone()))
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 3 },
                        color_options: vec![ManaColor::Black, ManaColor::Red],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(cost),
        )
        .id();
    let mut runner = scenario.build();

    begin_sacrificial_payment(&mut runner, spell);
    let WaitingFor::ManaSourceSelection { options, .. } = &runner.state().waiting_for else {
        panic!("both indexed abilities must be offered");
    };
    let source_options: Vec<_> = options
        .iter()
        .filter(|option| option.source.object_id == source)
        .collect();
    assert_eq!(source_options.len(), 2);
    assert!(source_options
        .iter()
        .any(|option| option.ability_index == Some(0)
            && option.output
                == ManaSourceOutput::DeferredColorChoice {
                    quantity: ManaSourceQuantity::Fixed(1)
                }));
    let second = source_options
        .iter()
        .find(|option| option.ability_index == Some(1))
        .expect("the second printed ability remains distinct");
    assert_eq!(
        second.output,
        ManaSourceOutput::DeferredColorChoice {
            quantity: ManaSourceQuantity::Fixed(3),
        }
    );
    let second_selection = (**second).clone();
    runner
        .act(GameAction::ActivateManaSource {
            selection: second_selection,
        })
        .expect("the indexed second ability activates");
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Red),
            count: 1,
        })
        .expect("the second ability produces its own three mana");
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Red),
        3
    );
}

/// A self-sacrificing mana ability is offered only after the real cast pipeline
/// has exhausted non-sacrificial payment rows.
#[test]
fn self_sacrificing_mana_source_pays_a_production_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let source = scenario
        .add_creature(P0, "Blood Pet Witness", 1, 1)
        .with_ability_definition(sacrificial_mana_ability(
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ManaColor::Black,
        ))
        .id();
    let mut runner = scenario.build();

    begin_sacrificial_payment(&mut runner, spell);
    let selection = offered_selection(&runner, source);
    runner
        .act(GameAction::ActivateManaSource { selection })
        .expect("the offered self-sacrificing source should activate during payment");

    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Black),
        1,
        "the activated source's mana reaches the pending spell payment"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));

    runner
        .act(GameAction::PassPriority)
        .expect("the selected mana should pay the spell through the ordinary payment reducer");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
}

/// A frozen source selection must preserve an AnyOneColor activation's normal
/// color prompt, then resume and finish the original cast.
#[test]
fn any_one_color_self_sacrifice_selection_completes_production_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let source = scenario
        .add_creature(P0, "Gold Witness", 1, 1)
        .with_ability_definition(any_one_color_sacrificial_mana_ability(
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
        ))
        .id();
    let mut runner = scenario.build();

    begin_sacrificial_payment(&mut runner, spell);
    let selection = offered_selection(&runner, source);
    runner
        .act(GameAction::ActivateManaSource { selection })
        .expect("the frozen self-sacrifice selection should enter the color prompt");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseManaColor { .. }
    ));

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Red),
            count: 1,
        })
        .expect("choosing the source's color should resume the pending payment");
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));

    runner
        .act(GameAction::PassPriority)
        .expect("the chosen mana should finish the original cast");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
}

/// A non-sacrificial row on the same permanent remains available to the
/// automatic planner; only the irreversible row is held for explicit consent.
#[test]
fn automatic_payment_keeps_a_non_sacrificial_row_on_a_sacrificial_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let source = scenario
        .add_creature(P0, "Two-Row Mana Witness", 1, 1)
        .as_artifact()
        .with_ability_definition(sacrificial_mana_ability(AbilityCost::Tap, ManaColor::Red))
        .with_ability_definition(sacrificial_mana_ability(
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ManaColor::Black,
        ))
        .id();
    let mut runner = scenario.build();

    begin_sacrificial_payment(&mut runner, spell);

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().objects[&source].tapped);
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
}

/// The pre-activation prompt does not bypass a mana ability's own sacrifice
/// choice; selecting another artifact pays that cost while its source remains
/// on the battlefield.
#[test]
fn sacrifice_another_permanent_mana_source_resumes_the_pending_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let source = scenario
        .add_creature(P0, "Krark-Clan Ironworks Witness", 1, 1)
        .as_artifact()
        .with_ability_definition(sacrificial_mana_ability(
            AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                1,
            )),
            ManaColor::Red,
        ))
        .id();
    let sacrifice = scenario
        .add_creature(P0, "Sacrificial Artifact Witness", 1, 1)
        .as_artifact()
        .id();
    let mut runner = scenario.build();

    begin_sacrificial_payment(&mut runner, spell);
    let selection = offered_selection(&runner, source);
    runner
        .act(GameAction::ActivateManaSource { selection })
        .expect("the offered source should enter its normal interactive cost payment");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost {
            kind: PayCostKind::Sacrifice,
            ref choices,
            ..
        } if choices.contains(&sacrifice)
    ));

    runner
        .act(GameAction::SelectCards {
            cards: vec![sacrifice],
        })
        .expect("the source's ordinary sacrifice-cost reducer should accept another artifact");

    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&sacrifice].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        1,
        "the selected ability's mana remains available to the original spell"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
}

/// Returning from the safety prompt keeps the pending spell but neither spends
/// mana nor performs the irreversible source activation.
#[test]
fn back_from_sacrificial_mana_prompt_preserves_the_cast_without_cancellation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let source = scenario
        .add_creature(P0, "Back Button Witness", 1, 1)
        .with_ability_definition(sacrificial_mana_ability(
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ManaColor::Black,
        ))
        .id();
    let mut runner = scenario.build();

    begin_sacrificial_payment(&mut runner, spell);
    assert!(
        !candidate_actions(runner.state())
            .iter()
            .any(|candidate| matches!(candidate.action, GameAction::CancelCast)),
        "the real safety prompt must not synthesize a cast-cancellation action"
    );
    runner
        .act(GameAction::BackToManaPayment)
        .expect("the prompt's explicit back action should be accepted");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    assert!(runner.state().pending_cast.is_some());
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
}

/// An engine-authored selection is revalidated at the reducer boundary, so a
/// source that became ineligible cannot be activated from a stale payment prompt.
#[test]
fn stale_sacrificial_mana_selection_is_rejected_without_mutating_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let source = scenario
        .add_creature(P0, "Stale Selection Witness", 1, 1)
        .with_ability_definition(sacrificial_mana_ability(
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ManaColor::Black,
        ))
        .id();
    let mut runner = scenario.build();

    begin_sacrificial_payment(&mut runner, spell);
    let selection = offered_selection(&runner, source);
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .unwrap()
        .controller = PlayerId(1);

    assert!(
        runner
            .act(GameAction::ActivateManaSource { selection })
            .is_err(),
        "a stale source must fail before the payment reducer mutates state"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaSourceSelection { .. }
    ));
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&source].controller, PlayerId(1));
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
}

// ---------------------------------------------------------------------------
// CR 106.3 + CR 113.7 + CR 400.7 + CR 605.3b: ONE producer identity for a
// parked activation — the journal's production record and the mana the colour
// choice produces are stamped from the same resolved value.
// ---------------------------------------------------------------------------

/// The persisted JSON a save of `state` produces.
fn persisted_json(state: &GameState) -> serde_json::Value {
    serde_json::to_value(PersistedGameState::capture(state.clone())).expect("the save serializes")
}

/// Decode a save through the engine's own restore chokepoint, keeping the
/// failure as a value: a save the engine wrote must decode AND be admitted, so a
/// witness that only checked admission would miss a save its own journal rejects
/// at the decoder.
fn admitted(json: serde_json::Value) -> Result<GameState, String> {
    let persisted = serde_json::from_value::<PersistedGameState>(json)
        .map_err(|error| format!("the save must decode: {error}"))?;
    persisted
        .into_game_state()
        .map_err(|error| format!("the save must be admitted: {error:?}"))
}

/// The parked activation's `PendingManaAbility`, exactly as the save wrote it.
fn saved_pending_mut(
    json: &mut serde_json::Value,
) -> &mut serde_json::Map<String, serde_json::Value> {
    json.get_mut("state")
        .and_then(|state| state.get_mut("waiting_for"))
        .and_then(|waiting| waiting.get_mut("data"))
        .and_then(|waiting| waiting.get_mut("context"))
        .and_then(|context| context.get_mut("data"))
        .and_then(serde_json::Value::as_object_mut)
        .expect("the parked colour choice carries its pending mana ability")
}

/// Real Lotus Petal parked at its post-sacrifice colour prompt: the `{T},
/// Sacrifice` cost is paid, the artifact is a graveyard card (CR 400.7 makes
/// that a new object at the same storage id), and the activation waits on its
/// colour. Returns the announcement epoch — the incarnation of the object that
/// generated the ability.
fn petal_at_the_colour_prompt() -> (GameRunner, ObjectId, u64) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let petal = scenario
        .add_artifact_from_oracle(
            P0,
            "Lotus Petal",
            "{T}, Sacrifice this artifact: Add one mana of any color.",
        )
        .id();
    let mut runner = scenario.build();
    let announced = runner.state().objects[&petal].incarnation;
    begin_sacrificial_payment(&mut runner, spell);
    let selection = offered_selection(&runner, petal);
    runner
        .act(GameAction::ActivateManaSource { selection })
        .expect("the real Petal activates");
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::ChooseManaColor {
                choice: ManaChoicePrompt::SingleColor { options },
                ..
            } if options.contains(&ManaType::Blue)
        ),
        "the sacrificed Petal parks at its colour prompt, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&petal].zone, Zone::Graveyard);
    assert_ne!(
        runner.state().objects[&petal].incarnation,
        announced,
        "CR 400.7: the graveyard card is a new object, so a live read names the wrong one"
    );
    (runner, petal, announced)
}

/// The pool's single blue pip: the producer epoch stamped on the unit, and the
/// epoch the journal's own production record for that pip names.
fn blue_pip_producer_epochs(state: &GameState) -> (Option<u64>, Option<u64>) {
    let units: Vec<_> = state.players[P0.0 as usize]
        .mana_pool
        .units()
        .filter(|unit| unit.color == ManaType::Blue)
        .collect();
    assert_eq!(units.len(), 1, "the choice produced exactly one blue mana");
    let unit = &units[0];
    let from_node = state
        .resolved_rules_journal
        .produced_source_epoch(unit.pip_id)
        .map(|source| {
            assert_eq!(
                source.object_id, unit.source_id,
                "the journal's production record belongs to this pip's source"
            );
            source.incarnation
        });
    (unit.source_incarnation, from_node)
}

/// Answer the parked colour prompt. What the answer WROTE is asserted separately,
/// so a witness that fails on the second load shows the refusal itself.
fn choose_blue(mut runner: GameRunner) -> GameRunner {
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        })
        .expect("the restored activation's colour choice produces its mana");
    runner
}

/// Both records of the floating pip must name `announced` — the ONE value the
/// resume boundary resolves, consumed by the pip's stamp and by the journal's
/// production record alike.
fn assert_one_producer(state: &GameState, announced: u64, when: &str) {
    let (pinned, from_node) = blue_pip_producer_epochs(state);
    assert_eq!(
        pinned,
        Some(announced),
        "{when}: CR 106.3 + CR 113.7: the pip's producer is the object that generated the ability"
    );
    assert_eq!(
        from_node,
        Some(announced),
        "{when}: CR 400.7: the journal's production record names the same producer"
    );
}

/// CR 106.3 + CR 400.7: the July-22 save shape — `rules_execution_node` survived
/// (commit `d247cee56b8`), `announced_source_incarnation` did not (it arrived in
/// `ba8d407ddaf`). The restored activation must produce its mana from the object
/// that generated the ability, record the SAME epoch beside it in the journal,
/// and then survive a second save/restore.
#[test]
fn node_only_petal_prompt_round_trips_twice_with_one_producer_identity() {
    let (runner, petal, announced) = petal_at_the_colour_prompt();
    let mut json = persisted_json(runner.state());
    let pending = saved_pending_mut(&mut json);
    assert!(
        pending.contains_key("rules_execution_node"),
        "reach-guard: the historical save carries the activation's execution node"
    );
    assert!(
        pending.remove("announced_source_incarnation").is_some(),
        "reach-guard: the historical save carries no announcement epoch"
    );

    let runner = GameRunner::from_state(admitted(json).expect("the historical save restores"));
    let runner = choose_blue(runner);
    assert_one_producer(runner.state(), announced, "the restored activation wrote");
    let state = admitted(persisted_json(runner.state())).unwrap_or_else(|error| {
        panic!("the save written after the colour choice must restore again: {error}")
    });
    assert_one_producer(&state, announced, "the second load read");
    assert_eq!(state.objects[&petal].zone, Zone::Graveyard);
    assert_ne!(
        state.objects[&petal].incarnation, announced,
        "CR 400.7: the graveyard card that holds the id now is not the producer"
    );
}

/// CR 106.3 + CR 400.7: the converse save shape — the announcement epoch
/// survived but `rules_execution_node` did not. The resumed activation must not
/// re-open a node naming the live object at the storage id: that record would
/// disagree with the epoch production stamps, and the next load would reject the
/// save.
#[test]
fn announcement_only_petal_prompt_round_trips_twice_with_one_producer_identity() {
    let (runner, petal, announced) = petal_at_the_colour_prompt();
    let mut json = persisted_json(runner.state());
    let pending = saved_pending_mut(&mut json);
    assert_eq!(
        pending.get("announced_source_incarnation"),
        Some(&serde_json::json!(announced)),
        "reach-guard: the converse save carries the announcement epoch"
    );
    assert!(
        pending.remove("rules_execution_node").is_some(),
        "reach-guard: the converse save carries no execution node"
    );

    let runner = GameRunner::from_state(admitted(json).expect("the converse save restores"));
    let runner = choose_blue(runner);
    assert_one_producer(runner.state(), announced, "the resumed activation wrote");
    let state = admitted(persisted_json(runner.state())).unwrap_or_else(|error| {
        panic!("the save written after the colour choice must restore again: {error}")
    });
    assert_one_producer(&state, announced, "the second load read");
    assert_eq!(state.objects[&petal].zone, Zone::Graveyard);
    assert_ne!(
        state.objects[&petal].incarnation, announced,
        "CR 400.7: the graveyard card that holds the id now is not the producer"
    );
}

/// The intact control: a save carrying BOTH fields still restores and pays
/// exactly as it did before this change.
#[test]
fn intact_petal_prompt_restores_and_pays_from_the_announcement_epoch() {
    let (runner, petal, announced) = petal_at_the_colour_prompt();
    let mut json = persisted_json(runner.state());
    let pending = saved_pending_mut(&mut json);
    assert!(
        pending.contains_key("rules_execution_node"),
        "the intact save carries the execution node"
    );
    assert_eq!(
        pending.get("announced_source_incarnation"),
        Some(&serde_json::json!(announced)),
        "the intact save carries the announcement epoch"
    );

    let runner = GameRunner::from_state(admitted(json).expect("the intact save restores"));
    let mut runner = choose_blue(runner);
    assert_one_producer(runner.state(), announced, "the restored activation wrote");
    let state = admitted(persisted_json(runner.state())).expect("the second load restores");
    assert_one_producer(&state, announced, "the second load read");

    // CR 601.2h + CR 605.3: the control still finishes the cast it was funding.
    runner
        .act(GameAction::PassPriority)
        .expect("the chosen mana pays the spell");
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
    assert_eq!(runner.state().objects[&petal].zone, Zone::Graveyard);
}

/// CR 106.3 + CR 400.7: a pending activation that names NO producer evidence at
/// all — neither the execution node nor the announcement epoch — is refused
/// rather than priced from whatever occupies the storage id now.
#[test]
fn petal_prompt_with_no_producer_evidence_is_refused() {
    let (runner, petal, announced) = petal_at_the_colour_prompt();
    let mut json = persisted_json(runner.state());
    let pending = saved_pending_mut(&mut json);
    assert!(pending.remove("rules_execution_node").is_some());
    assert!(pending.remove("announced_source_incarnation").is_some());

    let mut runner = GameRunner::from_state(admitted(json).expect("the stripped save restores"));
    assert!(
        runner
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Blue),
                count: 1,
            })
            .is_err(),
        "a producer that no surviving record names must be refused, not guessed"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "the refusal produces no mana"
    );
    assert_eq!(runner.state().objects[&petal].zone, Zone::Graveyard);
    assert_ne!(runner.state().objects[&petal].incarnation, announced);
}

// ---------------------------------------------------------------------------
// CR 605.3b + CR 113.7 + CR 400.7: ONE settlement node for ONE activation,
// however often it parks — and a retained node that cannot be that activation's
// is refused, not read as absence.
//
// The parked activation names its producer twice (its execution node and its
// announcement epoch). The resume resolves both once, so it must record BOTH on
// the pending it is about to park again: an epoch-only pending makes the next
// resume open a SECOND `ActivatedMana` settlement node for the same activation
// and attribute the pip to it, leaving the first node settling nothing. And the
// recorded node is only evidence when the journal still holds it AND it is this
// activation's own node: a `TriggeredMana` node (CR 605.4a, a different
// occurrence) or an `ActivatedMana` node naming another object is a save
// contradicting itself, while a save that records nothing the journal still
// holds is the legacy shape recovery exists for.
// ---------------------------------------------------------------------------

/// Rest in Peace, verbatim (Scryfall, exact name).
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";
/// Leyline of the Void, verbatim (Scryfall, exact name).
const LEYLINE_OF_THE_VOID: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nIf a card would be put into an opponent's graveyard from anywhere, exile it instead.";

/// Every retained `ActivatedMana` settlement node naming this exact object
/// epoch — the nodes one activation may legitimately own at most one of.
fn activated_mana_nodes(
    state: &GameState,
    source: ObjectIncarnationRef,
) -> Vec<RulesExecutionNodeRef> {
    state
        .resolved_rules_journal
        .nodes()
        .iter()
        .filter(|node| {
            matches!(
                &node.kind,
                RulesExecutionNodeKind::ActivatedMana { source: named } if *named == source
            )
        })
        .map(|node| node.identity)
        .collect()
}

/// The mana pips this one settlement node recorded as produced — the record a
/// second settlement must not add to.
fn node_produced_pips(
    state: &GameState,
    node: RulesExecutionNodeRef,
) -> Vec<engine::types::mana::ManaPipId> {
    state
        .resolved_rules_journal
        .nodes()
        .iter()
        .find(|candidate| candidate.identity == node)
        .map(|candidate| candidate.produced_pips.clone())
        .unwrap_or_default()
}

/// Every retained `TriggeredMana` settlement node naming this exact object
/// epoch.
fn triggered_mana_nodes(
    state: &GameState,
    source: ObjectIncarnationRef,
) -> Vec<RulesExecutionNodeRef> {
    state
        .resolved_rules_journal
        .nodes()
        .iter()
        .filter(|node| {
            matches!(
                &node.kind,
                RulesExecutionNodeKind::TriggeredMana { source: named, .. } if *named == source
            )
        })
        .map(|node| node.identity)
        .collect()
}

/// The parked activation inside a save taken while its cost payment waits on a
/// replacement choice (the typed cost root, not a prompt).
fn saved_cost_pending_mut(
    json: &mut serde_json::Value,
) -> &mut serde_json::Map<String, serde_json::Value> {
    json.get_mut("state")
        .and_then(|state| state.get_mut("pending_cost_move_resume"))
        .and_then(|resume| resume.get_mut("ManaAbilityPayment"))
        .and_then(|payment| payment.get_mut("pending"))
        .and_then(serde_json::Value::as_object_mut)
        .expect("the paused mana-ability cost payment carries its pending activation")
}

/// The refusal a save draws at the restore chokepoint, kept typed so a witness
/// asserts WHICH contradiction was refused rather than only that one was.
fn admission_refusal(json: serde_json::Value) -> PersistedRestoreError {
    let persisted = serde_json::from_value::<PersistedGameState>(json)
        .expect("the save must decode before it can be judged");
    match persisted.into_game_state() {
        Ok(_) => panic!("the save must be refused at restore"),
        Err(error) => error,
    }
}

/// The `PendingManaAbility` a restored state parks at its colour prompt.
fn parked_colour_pending(state: &GameState) -> &PendingManaAbility {
    match &state.waiting_for {
        WaitingFor::ChooseManaColor {
            context: ManaChoiceContext::ManaAbility(pending),
            ..
        } => pending,
        other => panic!("expected a parked mana-ability colour prompt, got {other:?}"),
    }
}

/// Real Lotus Petal whose sacrifice cost pauses at a replacement choice: the two
/// competing graveyard replacements (Rest in Peace and Leyline of the Void) apply
/// after the ability is announced and before the ability resolves, so the
/// activation is parked in its typed cost root (CR 616.1 + CR 605.3b) rather than
/// at a prompt. Returns the announcement epoch.
fn petal_cost_paused_at_the_replacement_choice() -> (GameRunner, ObjectId, u64) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let petal = scenario
        .add_artifact_from_oracle(
            P0,
            "Lotus Petal",
            "{T}, Sacrifice this artifact: Add one mana of any color.",
        )
        .id();
    scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    scenario.add_enchantment_from_oracle(P1, "Leyline of the Void", LEYLINE_OF_THE_VOID);
    let mut runner = scenario.build();
    let announced = runner.state().objects[&petal].incarnation;
    begin_sacrificial_payment(&mut runner, spell);
    let selection = offered_selection(&runner, petal);
    runner
        .act(GameAction::ActivateManaSource { selection })
        .expect("the real Petal activates and its sacrifice cost pauses");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the sacrifice must pause at its replacement choice, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(
            runner.state().pending_cost_move_resume,
            Some(PendingCostMoveResume::ManaAbilityPayment { .. })
        ),
        "the paused cost must sit in its typed mana-ability root, got {:?}",
        runner.state().pending_cost_move_resume
    );
    assert_eq!(
        runner.state().objects[&petal].zone,
        Zone::Battlefield,
        "the sacrifice has not settled yet"
    );
    (runner, petal, announced)
}

/// CR 605.3b + CR 113.7: the cost resume binds the node it selected together with
/// the epoch, so the pending it parks at the colour prompt already carries BOTH
/// and the colour answer reuses that one node instead of opening a second.
///
/// The save is the legacy shape the epoch-only bind produced: announcement epoch
/// present, `rules_execution_node` absent. Before the fix the cost resume created
/// a node, bound only the epoch, and the colour answer opened a SECOND
/// `ActivatedMana` node for the same activation — the pip went to the second and
/// the first settled nothing.
#[test]
fn replacement_pause_round_trip_keeps_one_settlement_node_for_the_activation() {
    let (runner, petal, announced) = petal_cost_paused_at_the_replacement_choice();
    let source = ObjectIncarnationRef::of(petal, announced);
    let before = activated_mana_nodes(runner.state(), source);
    let mut json = persisted_json(runner.state());
    let pending = saved_cost_pending_mut(&mut json);
    assert_eq!(
        pending.get("announced_source_incarnation"),
        Some(&serde_json::json!(announced)),
        "reach-guard: the save carries the announcement epoch"
    );
    assert!(
        pending.remove("rules_execution_node").is_some(),
        "reach-guard: the save carries no execution node"
    );

    let mut runner = GameRunner::from_state(
        admitted(json).expect("a cost save whose node is absent is legacy, not corrupt"),
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the paused sacrifice settles through a replacement");

    let pending_at_prompt = parked_colour_pending(runner.state());
    // What the cost resume bound onto the pending it is about to park.
    let bound = pending_at_prompt.rules_execution_node;
    let bound_epoch = pending_at_prompt.announced_source_incarnation;
    let after_resume = activated_mana_nodes(runner.state(), source);

    // CR 605.3b: the colour prompt is the activation's second park. Its save must
    // carry the node, so restoring it and answering does not open another.
    let state = admitted(persisted_json(runner.state()))
        .expect("the colour-prompt save written after the resume restores");
    assert_eq!(
        parked_colour_pending(&state).rules_execution_node,
        bound,
        "the round trip keeps whatever node the cost resume bound"
    );
    let mut runner = choose_blue(GameRunner::from_state(state));
    assert_one_producer(runner.state(), announced, "the resumed activation wrote");
    let after_choice = activated_mana_nodes(runner.state(), source);
    let produced = runner
        .state()
        .resolved_rules_journal
        .produced_mana()
        .iter()
        .find(|record| record.unit.color == ManaType::Blue)
        .expect("the choice recorded its production");

    assert_eq!(
        after_choice.len(),
        before.len() + 1,
        "CR 605.3b: ONE activation settles under ONE activated-mana node however often it \
         parks, so the whole resume must open exactly one more node — {} before the resume, \
         {} after it, {} after the colour answer: {after_choice:?}",
        before.len(),
        after_resume.len(),
        after_choice.len()
    );
    assert_eq!(
        bound,
        Some(produced.producer),
        "CR 106.3 + CR 113.7: the pip is settled by the node the cost resume created and \
         bound onto the pending, not by one a later resume opened (producer {:?})",
        produced.producer
    );
    assert!(
        !before.contains(&produced.producer),
        "the node the resume bound is the one it opened, not the announcement's"
    );
    assert_eq!(
        bound_epoch,
        Some(announced),
        "CR 602.2a: the same resume binds the announcement epoch beside the node"
    );
    assert_eq!(
        runner
            .state()
            .resolved_rules_journal
            .produced_source_epoch(produced.unit.pip_id),
        Some(ObjectIncarnationRef::of(petal, announced)),
        "CR 400.7: and that node names the object that generated the ability"
    );

    // The activation still finishes the cast it was funding.
    runner
        .act(GameAction::PassPriority)
        .expect("the chosen mana pays the spell");
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
}

/// CR 605.4a + CR 113.7: a parked ACTIVATED colour prompt whose retained node is
/// a TRIGGERED-mana node for the same source and epoch settles a different
/// ability. The reference contradicts the activation, so restore refuses it
/// instead of reading an epoch out of it.
///
/// The board is the two PRINTED cards that make this shape real rather than
/// synthetic:
///
/// * **Kinnan, Bonder Prodigy** — "Whenever you tap a nonland permanent for mana,
///   add one mana of any type that permanent produced." A triggered MANA ability
///   (CR 605.1b: it triggers from the activation of an activated mana ability), so
///   it settles under a `TriggeredMana` node that names KINNAN — the object whose
///   ability triggered (CR 113.7) — and his epoch.
/// * **Enduring Vitality** — "Creatures you control have \"{T}: Add one mana of
///   any color.\"" The grant is what gives KINNAN an activated mana ability, so
///   the parked activation is announced by the same object and incarnation the
///   triggered node already names. That same-source, same-incarnation pair is the
///   whole point: only the node's KIND tells them apart.
const KINNAN_BONDER_PRODIGY: &str = "Whenever you tap a nonland permanent for mana, add one mana of any type that permanent produced.\n{5}{G}{U}: Look at the top five cards of your library. You may put a non-Human creature card from among them onto the battlefield. Put the rest on the bottom of your library in a random order.";
const ENDURING_VITALITY: &str = "Vigilance\nCreatures you control have \"{T}: Add one mana of any color.\"\nWhen Enduring Vitality dies, if it was a creature, return it to the battlefield under its owner's control. It's an enchantment. (It's not a creature.)";

#[test]
fn parked_activation_naming_a_triggered_mana_node_is_refused_at_restore() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Kinnan: the printed trigger's home, and (through Enduring Vitality's grant)
    // the source of the activation that parks. Both settle against ONE object and
    // ONE incarnation — the pair a save cannot distinguish on its own.
    let kinnan = scenario
        .add_creature_from_oracle(P0, "Kinnan, Bonder Prodigy", 2, 2, KINNAN_BONDER_PRODIGY)
        .id();
    // Enduring Vitality is an Enchantment Creature; the grant is its second line.
    let vitality = scenario
        .add_creature_from_oracle(P0, "Enduring Vitality", 3, 3, ENDURING_VITALITY)
        .id();
    // A nonland permanent to tap for mana, which is what fires Kinnan's trigger.
    let tapped = scenario
        .add_artifact_from_oracle(
            P0,
            "Lotus Petal",
            "{T}, Sacrifice this artifact: Add one mana of any color.",
        )
        .id();
    let mut runner = scenario.build();
    // Enduring Vitality is an Enchantment Creature; the scenario constructors add
    // one core type each, so state the printed pair rather than a stand-in.
    {
        let obj = runner.state_mut().objects.get_mut(&vitality).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.base_card_types = obj.card_types.clone();
    }
    let announced = runner.state().objects[&kinnan].incarnation;

    // CR 605.3a: a mana ability is activated at priority. Tapping the Petal for
    // mana is what Kinnan's trigger fires on, and the trigger belongs to KINNAN.
    runner
        .act(GameAction::ActivateAbility {
            source_id: tapped,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the Petal activates");
    let mut runner = choose_blue(runner);
    let source = ObjectIncarnationRef::of(kinnan, announced);
    let triggered = triggered_mana_nodes(runner.state(), source);
    assert_eq!(
        triggered.len(),
        1,
        "reach-guard: Kinnan's tapped-for-mana trigger settled under ONE TriggeredMana node \
         naming Kinnan's own object and epoch, got {triggered:?}"
    );

    // Kinnan's GRANTED activation (Enduring Vitality's "{T}: Add one mana of any
    // color.") parks at its colour prompt with its own activated node.
    let granted = runner.state().objects[&kinnan]
        .abilities
        .iter()
        .position(|ability| matches!(*ability.effect, Effect::Mana { .. }))
        .expect("reach-guard: Enduring Vitality granted Kinnan an activated mana ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: kinnan,
            ability_index: granted,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Kinnan's granted mana ability activates and parks at its colour choice");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ),
        "the activation parks at its colour prompt, got {:?}",
        runner.state().waiting_for
    );
    let activated = parked_colour_pending(runner.state())
        .rules_execution_node
        .expect("the parked activation carries its own activated node");
    assert_ne!(
        activated, triggered[0],
        "the triggered node is a different settlement"
    );
    assert!(
        activated_mana_nodes(runner.state(), source).contains(&activated),
        "reach-guard: the parked activation's node is an ACTIVATED node of this very object \
         and epoch — the same source and incarnation the triggered node names"
    );

    let mut json = persisted_json(runner.state());
    let pending = saved_pending_mut(&mut json);
    assert_eq!(
        pending.get("announced_source_incarnation"),
        Some(&serde_json::json!(announced)),
        "reach-guard: the save carries the epoch the triggered node also names"
    );
    pending.insert(
        "rules_execution_node".to_string(),
        serde_json::to_value(triggered[0]).expect("the node identity serializes"),
    );

    let refusal = admission_refusal(json);
    match &refusal {
        PersistedRestoreError::ContradictoryParkedActivationNode {
            source_id,
            reason: ParkedActivationNodeConflict::NotAnActivatedManaNode { node },
        } => {
            assert_eq!(*source_id, kinnan);
            assert_eq!(*node, triggered[0]);
        }
        other => panic!(
            "a triggered-mana node on an activated activation must be refused as the wrong \
             KIND, got {other:?}"
        ),
    }
}

/// CR 605.3b + CR 400.7: a retained ACTIVATED node naming a different object is
/// a reference to another activation's settlement. It was silently converted to
/// "no evidence" (the source filter dropped it) and the activation was priced
/// from its announcement epoch instead; restore refuses it now.
#[test]
fn parked_activation_naming_another_object_s_activated_node_is_refused_at_restore() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_artifact_from_oracle(
            P0,
            "Lotus Petal",
            "{T}, Sacrifice this artifact: Add one mana of any color.",
        )
        .id();
    let second = scenario
        .add_artifact_from_oracle(
            P0,
            "Lotus Petal",
            "{T}, Sacrifice this artifact: Add one mana of any color.",
        )
        .id();
    let mut runner = scenario.build();
    let first_announced = runner.state().objects[&first].incarnation;

    // The first Petal resolves completely and settles under its own node.
    runner
        .act(GameAction::ActivateAbility {
            source_id: first,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the first Petal activates");
    let mut runner = choose_blue(runner);
    let first_nodes = activated_mana_nodes(
        runner.state(),
        ObjectIncarnationRef::of(first, first_announced),
    );
    assert!(
        !first_nodes.is_empty(),
        "reach-guard: the first Petal settled under its own activated node"
    );

    // The second Petal parks at its colour prompt with its own node.
    runner
        .act(GameAction::ActivateAbility {
            source_id: second,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the second Petal activates and parks at its colour choice");
    let parked = parked_colour_pending(runner.state());
    assert_ne!(
        parked.rules_execution_node,
        Some(first_nodes[0]),
        "reach-guard: the parked activation names its own node"
    );

    let mut json = persisted_json(runner.state());
    let pending = saved_pending_mut(&mut json);
    pending.insert(
        "rules_execution_node".to_string(),
        serde_json::to_value(first_nodes[0]).expect("the node identity serializes"),
    );

    let refusal = admission_refusal(json);
    match &refusal {
        PersistedRestoreError::ContradictoryParkedActivationNode {
            source_id,
            reason: ParkedActivationNodeConflict::ForeignSource { node, source, .. },
        } => {
            assert_eq!(*source_id, second);
            assert_eq!(*node, first_nodes[0]);
            assert_eq!(source.object_id, first);
        }
        other => panic!(
            "an activated node naming another object must be refused as the wrong SOURCE, \
             not converted to absence, got {other:?}"
        ),
    }
}

/// CR 605.3b: Prismite — "{2}: Add one mana of any color." A creature that can be
/// activated TWICE in one incarnation, which is the shape no source/epoch test can
/// tell apart: activation 1 and activation 2 name the same object and the same
/// epoch, and each opens its own `ActivatedMana` node.
///
/// A save parked at activation 2's colour prompt states that occurrence beside the
/// node it belongs to. Pointing the node at activation 1's node — retained, of the
/// same source, of the same epoch — used to be accepted (the validator asked only
/// whether the node was an activated-mana node of that source), the second
/// activation re-entered the first's FINISHED node, and its settlement was written
/// into it. CR 605.3b resolves a mana ability immediately after it is activated, so
/// that node is done: the pair contradicting itself is refused.
///
/// The control is the same save untouched: it restores, produces one pip under
/// activation 2's own node, and leaves activation 1's node with exactly its own
/// pip.
const PRISMITE: &str = "{2}: Add one mana of any color.";

/// Prismite's first activation completed; the second is parked at its colour
/// prompt. Returns the runner, the source's identity, and activation 1's node.
fn prismite_second_activation_parked() -> (GameRunner, ObjectIncarnationRef, RulesExecutionNodeRef)
{
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let prismite = scenario
        .add_artifact_from_oracle(P0, "Prismite", PRISMITE)
        .id();
    // Four colorless: {2} for each activation.
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| {
                engine::types::mana::ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![])
            })
            .collect(),
    );
    let mut runner = scenario.build();
    let announced = runner.state().objects[&prismite].incarnation;
    let source = ObjectIncarnationRef::of(prismite, announced);

    let activate = |runner: &mut GameRunner| {
        runner
            .act(GameAction::ActivateAbility {
                source_id: prismite,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("Prismite activates and parks at its colour choice");
    };

    // Activation 1 completes and settles under its own node.
    activate(&mut runner);
    let mut runner = choose_blue(runner);
    let first = activated_mana_nodes(runner.state(), source);
    assert_eq!(
        first.len(),
        1,
        "reach-guard: activation 1 settled under exactly one activated-mana node, got {first:?}"
    );
    assert_eq!(
        node_produced_pips(runner.state(), first[0]).len(),
        1,
        "reach-guard: activation 1's node recorded the pip it produced"
    );

    // Activation 2 parks. It opens a SECOND node of the same source and epoch.
    activate(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ),
        "activation 2 parks at its colour prompt, got {:?}",
        runner.state().waiting_for
    );
    let parked = parked_colour_pending(runner.state());
    let second = parked
        .rules_execution_node
        .expect("activation 2 carries its own node");
    assert_eq!(
        parked
            .activated_mana_occurrence
            .map(|occurrence| occurrence.ordinal()),
        match second {
            RulesExecutionNodeRef::ActivatedMana(ordinal) => Some(ordinal),
            _ => None,
        },
        "reach-guard: the parked activation states the occurrence its own node was minted under"
    );
    let all = activated_mana_nodes(runner.state(), source);
    assert_eq!(
        all.len(),
        2,
        "reach-guard: TWO activations of one object and epoch, so two activated-mana nodes — \
         {all:?}"
    );
    assert_ne!(second, first[0], "the two activations are different nodes");
    (runner, source, first[0])
}

#[test]
fn parked_activation_pointing_at_an_earlier_activation_s_node_is_refused() {
    let (runner, source, first_node) = prismite_second_activation_parked();
    let own = parked_colour_pending(runner.state())
        .rules_execution_node
        .expect("activation 2 carries its own node");
    let RulesExecutionNodeRef::ActivatedMana(own_ordinal) = own else {
        panic!("reach-guard: the parked activation's node is an activated-mana node");
    };
    assert_eq!(
        node_produced_pips(runner.state(), first_node).len(),
        1,
        "reach-guard: activation 1's node already settled its own pip"
    );
    assert!(
        node_produced_pips(runner.state(), own).is_empty(),
        "reach-guard: activation 2's node has settled nothing yet — it is parked before production"
    );

    let mut json = persisted_json(runner.state());
    let pending = saved_pending_mut(&mut json);
    assert_eq!(
        pending.get("announced_source_incarnation"),
        Some(&serde_json::json!(source.incarnation)),
        "reach-guard: the save carries the epoch both activations share"
    );
    assert!(
        pending.contains_key("activated_mana_occurrence"),
        "reach-guard: the save states the occurrence beside the node"
    );
    pending.insert(
        "rules_execution_node".to_string(),
        serde_json::to_value(first_node).expect("the node identity serializes"),
    );

    let refusal = admission_refusal(json);
    match &refusal {
        PersistedRestoreError::ContradictoryParkedActivationNode {
            source_id,
            reason: ParkedActivationNodeConflict::ForeignOccurrence { node, occurrence },
        } => {
            assert_eq!(*source_id, source.object_id);
            assert_eq!(*node, first_node, "the save points at activation 1's node");
            assert_eq!(
                occurrence.ordinal(),
                own_ordinal,
                "and states activation 2's own occurrence — so the two disagree"
            );
        }
        other => panic!(
            "CR 605.3b: an activated node of the same source and epoch that belongs to an EARLIER \
             activation must be refused as a different occurrence, got {other:?}"
        ),
    }
}

/// The intact control for the refusal above: the save this build wrote restores,
/// produces ONE pip under activation 2's own node, and leaves activation 1's node
/// with the one pip it already had.
#[test]
fn intact_second_activation_prompt_restores_and_settles_under_its_own_node() {
    let (runner, source, first_node) = prismite_second_activation_parked();
    let own = parked_colour_pending(runner.state())
        .rules_execution_node
        .expect("activation 2 carries its own node");
    let json = persisted_json(runner.state());

    let mut runner = GameRunner::from_state(admitted(json).expect("the intact save restores"));
    assert_eq!(
        node_produced_pips(runner.state(), first_node).len(),
        1,
        "the restored save still holds activation 1's node with its own pip"
    );
    runner = choose_blue(runner);
    assert_eq!(
        node_produced_pips(runner.state(), own).len(),
        1,
        "activation 2's settlement landed in activation 2's node"
    );
    assert_eq!(
        node_produced_pips(runner.state(), first_node).len(),
        1,
        "CR 605.3b: and NOT in activation 1's finished node, which the earlier activation \
         already settled"
    );
    assert_eq!(
        activated_mana_nodes(runner.state(), source).len(),
        2,
        "two activations, two nodes: the resume opened none"
    );
}

/// CR 605.3b + CR 602.2a: the control for both refusals. A save that records NO
/// node the journal still holds is the legacy shape — the announcement epoch is
/// exact evidence — so it is recovered, produces its mana, and writes a save
/// whose one settlement node is the one recovery opened.
#[test]
fn node_absent_legacy_colour_prompt_is_recovered_with_one_settlement_node() {
    let (runner, petal, announced) = petal_at_the_colour_prompt();
    let source = ObjectIncarnationRef::of(petal, announced);
    let before = activated_mana_nodes(runner.state(), source);
    let mut json = persisted_json(runner.state());
    let pending = saved_pending_mut(&mut json);
    assert_eq!(
        pending.get("announced_source_incarnation"),
        Some(&serde_json::json!(announced)),
        "reach-guard: the legacy save carries the announcement epoch"
    );
    assert!(
        pending.remove("rules_execution_node").is_some(),
        "reach-guard: and no execution node"
    );

    let mut runner = GameRunner::from_state(
        admitted(json).expect("absence is legacy evidence, never a contradiction"),
    );
    runner = choose_blue(runner);
    assert_one_producer(runner.state(), announced, "the recovered activation wrote");
    let after = activated_mana_nodes(runner.state(), source);
    assert_eq!(
        after.len(),
        before.len() + 1,
        "the recovery opens exactly one settlement node for the activation, got {after:?} \
         against {before:?}"
    );
    let produced = runner
        .state()
        .resolved_rules_journal
        .produced_mana()
        .iter()
        .find(|record| record.unit.color == ManaType::Blue)
        .expect("the choice recorded its production");
    assert!(
        after.contains(&produced.producer),
        "the pip is settled by the node the recovery opened"
    );

    let state = admitted(persisted_json(runner.state()))
        .expect("the save written after the choice still restores");
    assert_one_producer(&state, announced, "the second load read");
    assert_eq!(
        activated_mana_nodes(&state, source),
        after,
        "and it keeps the same one node"
    );
}
