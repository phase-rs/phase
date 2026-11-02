use engine::ai_support::candidate_actions;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, Effect, ManaContribution,
    ManaProduction, QuantityExpr, QuantityRef, SacrificeCost, TargetFilter, TypeFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, ManaChoice, ManaChoicePrompt, PayCostKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{
    ManaColor, ManaCost, ManaSourceOutput, ManaSourcePenalty, ManaSourceQuantity, ManaType,
};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

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
