//! CR 602.2b / CR 601.2c-h — Goblin Bombardment chooses targets before its
//! sacrifice cost.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, BeholdCostAction, CounterCostSelection, Effect,
    QuantityExpr, ReplacementDefinition, SacrificeCost, TapCreaturesRequirement,
    TapCreaturesSelectionMode, TargetFilter, TargetRef, TypeFilter, TypedFilter,
    REMOVE_COUNTER_COST_X,
};
use engine::types::actions::GameAction;
use engine::types::counter::{CounterMatch, CounterType};
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

const GOBLIN_BOMBARDMENT: &str =
    "Sacrifice a creature: This enchantment deals 1 damage to any target.";

fn setup() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bombardment = scenario
        .add_creature(P0, "Goblin Bombardment", 0, 0)
        .as_enchantment()
        .from_oracle_text(GOBLIN_BOMBARDMENT)
        .id();
    let fodder = scenario.add_creature(P0, "Sacrifice Goblin", 1, 1).id();
    let target = scenario.add_creature(P1, "Target Goblin", 1, 1).id();
    let self_target = scenario.add_creature(P0, "Self-Target Goblin", 1, 1).id();
    let mut runner = scenario.build();
    for token in [fodder, target, self_target] {
        runner.state_mut().objects.get_mut(&token).unwrap().is_token = true;
    }
    (runner, bombardment, fodder, target, self_target)
}

fn activate(runner: &mut GameRunner, bombardment: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: bombardment,
            ability_index: 0,
        })
        .expect("Goblin Bombardment activation must enter target selection");
}

fn choose_target(runner: &mut GameRunner, target: ObjectId) {
    let WaitingFor::TargetSelection { target_slots, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected target selection, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(target)),
        "the selected Goblin must be targetable before the sacrifice cost"
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(target)],
        })
        .expect("selecting the target must succeed");
}

fn sacrifice(runner: &mut GameRunner, permanent: ObjectId, selected_target: ObjectId) {
    let WaitingFor::PayCost { kind, choices, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "expected sacrifice cost, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(kind, PayCostKind::Sacrifice);
    assert!(choices.contains(&permanent));
    assert!(
        runner.state().battlefield.contains(&selected_target),
        "the declared target must stay on the battlefield until the sacrifice is paid"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![permanent],
        })
        .expect("sacrificing the chosen Goblin must succeed");
}

#[test]
fn goblin_bombardment_targets_before_sacrifice_and_cancel_is_lossless() {
    let (mut runner, bombardment, fodder, target, _) = setup();
    activate(&mut runner, bombardment);
    choose_target(&mut runner, target);

    let WaitingFor::PayCost { ref kind, .. } = runner.state().waiting_for else {
        panic!("target selection must lead to the sacrifice prompt");
    };
    assert_eq!(kind, &PayCostKind::Sacrifice);
    assert!(runner.state().battlefield.contains(&target));
    runner
        .act(GameAction::CancelCast)
        .expect("the pending sacrifice payment must be cancelable");
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().battlefield.contains(&bombardment));
    assert!(runner.state().battlefield.contains(&fodder));
    assert!(runner.state().battlefield.contains(&target));
    assert!(runner.state().deferred_triggers.is_empty());

    let (mut runner, bombardment, fodder, target, _) = setup();
    activate(&mut runner, bombardment);
    choose_target(&mut runner, target);
    sacrifice(&mut runner, fodder, target);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the activation reaches the stack"
    );
    runner.advance_until_stack_empty();
    assert!(
        !runner.state().battlefield.contains(&target),
        "the untargeted sacrifice must not invalidate the selected Goblin"
    );
}

#[test]
fn goblin_bombardment_self_target_sacrifice_reaches_stack_and_fizzles() {
    let (mut runner, bombardment, fodder, _, self_target) = setup();
    activate(&mut runner, bombardment);
    choose_target(&mut runner, self_target);
    sacrifice(&mut runner, self_target, self_target);

    assert_eq!(
        runner.state().stack.len(),
        1,
        "the activation reaches the stack"
    );
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert!(
        runner.state().battlefield.contains(&fodder),
        "only the self-targeted Goblin was sacrificed"
    );
}

#[test]
fn targeted_activation_surfaces_tap_creatures_cost_after_target_selection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Targeted Tap Cost", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Any,
                    damage_source: None,
                    excess: None,
                },
            )
            .cost(AbilityCost::TapCreatures {
                requirement: TapCreaturesRequirement::count(1),
                filter: TargetFilter::Typed(TypedFilter::creature()),
            }),
        )
        .id();
    let payment = scenario.add_creature(P0, "Tap Payment", 1, 1).id();
    let target = scenario.add_creature(P1, "Tap Cost Target", 1, 1).id();
    let mut runner = scenario.build();

    activate(&mut runner, source);
    choose_target(&mut runner, target);

    let WaitingFor::PayCost { kind, choices, .. } = runner.state().waiting_for.clone() else {
        panic!("target declaration must surface the tap-creatures cost");
    };
    assert_eq!(
        kind,
        PayCostKind::TapCreatures {
            mode: TapCreaturesSelectionMode::Fixed
        }
    );
    assert!(choices.contains(&payment));

    runner
        .act(GameAction::SelectCards {
            cards: vec![payment],
        })
        .expect("paying the selected tap-creatures cost must succeed");
    assert!(runner.state().objects[&payment].tapped);
    assert_eq!(runner.state().stack.len(), 1);
}

#[test]
fn target_first_mana_payment_precedes_sacrifice_and_can_be_cancelled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Mana-Gated Bombardment", 0, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Any,
                    damage_source: None,
                    excess: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Sacrifice(SacrificeCost::count(
                        TargetFilter::Typed(TypedFilter::creature()),
                        1,
                    )),
                    // CR 107.4e: Hybrid payment is a player decision, so this
                    // keeps the activation at ManaPayment before any later
                    // non-mana cost is paid.
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            shards: vec![ManaCostShard::WhiteBlue],
                            generic: 0,
                        },
                    },
                ],
            }),
        )
        .id();
    let fodder = scenario.add_creature(P0, "Committed Fodder", 1, 1).id();
    let target = scenario.add_creature(P1, "Mana-Gated Target", 1, 1).id();
    scenario.add_creature_from_oracle(
        P0,
        "Cost Trigger Witness",
        1,
        1,
        "Whenever another creature dies, draw a card.",
    );
    let mut runner = scenario.build();

    activate(&mut runner, source);
    choose_target(&mut runner, target);

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    assert!(
        runner.state().battlefield.contains(&fodder),
        "the sacrifice cost is not paid before the mana-payment step"
    );
    assert!(runner.state().stack.is_empty());
    assert!(
        runner.state().deferred_triggers.is_empty(),
        "no death trigger exists before the sacrifice cost is paid"
    );

    runner
        .act(GameAction::CancelCast)
        .expect("an activation may be cancelled before any cost has been paid");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(
        runner.state().battlefield.contains(&fodder),
        "cancelling before payment must preserve the sacrifice fodder"
    );
    assert!(runner.state().stack.is_empty());
    assert!(
        runner.state().deferred_triggers.is_empty(),
        "rejecting cancellation must neither leak nor duplicate the local trigger"
    );
}

/// CR 601.2c + CR 601.2g-h + CR 602.2b: an activation with an ordinary mana
/// leg and a targeted symbolic remove-X-counters cost must lock its effect
/// target, pay mana, then bind X before it asks which permanent pays counters.
///
/// The separate target and counter-source objects make this discriminate the
/// post-mana continuation: surfacing the symbolic residual directly either
/// exposes the wrong count or bypasses the counter-source choice entirely.
#[test]
fn target_first_mana_then_targeted_x_counter_cost_binds_x_before_payment_choice() {
    let charge = CounterType::Generic("charge".to_string());
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Mana and Counter Battery", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::DealDamage {
                    amount: QuantityExpr::Ref {
                        qty: engine::types::ability::QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                    target: TargetFilter::Any,
                    damage_source: None,
                    excess: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::generic(1),
                    },
                    AbilityCost::RemoveCounter {
                        count: REMOVE_COUNTER_COST_X,
                        counter_type: CounterMatch::OfType(charge.clone()),
                        target: Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact))),
                        selection: CounterCostSelection::SingleObject,
                    },
                ],
            }),
        )
        .id();
    let counter_source = scenario
        .add_creature(P0, "Charge Battery", 1, 1)
        .as_artifact()
        .id();
    let target = scenario.add_creature(P1, "Damage Target", 3, 3).id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(91_001),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&counter_source)
        .expect("counter source exists")
        .counters
        .insert(charge.clone(), 2);

    activate(&mut runner, source);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseXValue { .. }
    ));
    runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("X announcement must resume target selection");
    choose_target(&mut runner, target);

    let WaitingFor::PayCost {
        kind,
        choices,
        count,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "mana payment must resume at the concrete counter-cost choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        kind,
        PayCostKind::RemoveCounter {
            counter_type: CounterMatch::OfType(charge.clone()),
            count: 2,
            selection: CounterCostSelection::SingleObject,
        }
    );
    assert_eq!(count, 1, "one counter source must be selected");
    assert!(choices.contains(&counter_source));
    assert!(
        runner.state().battlefield.contains(&target),
        "the effect target remains declared while the counter source is chosen"
    );
    assert!(runner.state().stack.is_empty());

    runner
        .act(GameAction::SelectCards {
            cards: vec![counter_source],
        })
        .expect("selecting the counter source must pay the bound X cost");
    assert_eq!(
        runner.state().objects[&counter_source]
            .counters
            .get(&charge),
        None,
        "the selected source pays exactly the announced two counters"
    );
    assert_eq!(runner.state().stack.len(), 1);
}

fn targeted_damage_cost_ability(cost: AbilityCost) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
    )
    .cost(cost)
}

#[test]
fn targeted_activation_surfaces_blight_cost_after_target_selection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Targeted Blight Cost", 1, 1)
        .with_ability_definition(targeted_damage_cost_ability(AbilityCost::Blight {
            count: 1,
        }))
        .id();
    // Keep the payment creature alive after the -1/-1 counter so this test
    // observes the paid cost rather than its later state-based-action cleanup.
    let blighted = scenario.add_creature(P0, "Blight Payment", 2, 2).id();
    let target = scenario.add_creature(P1, "Blight Cost Target", 1, 1).id();
    let mut runner = scenario.build();

    activate(&mut runner, source);
    choose_target(&mut runner, target);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::BlightChoice { .. }
    ));

    runner
        .act(GameAction::SelectCards {
            cards: vec![blighted],
        })
        .expect("paying the target-first blight cost must succeed");
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(
        runner.state().objects[&blighted]
            .counters
            .get(&engine::types::counter::CounterType::Minus1Minus1),
        Some(&1),
    );
}

#[test]
fn targeted_activation_surfaces_reveal_cost_after_target_selection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Targeted Reveal Cost", 1, 1)
        .with_ability_definition(targeted_damage_cost_ability(AbilityCost::Reveal {
            count: 1,
            filter: Some(TargetFilter::Typed(TypedFilter::creature())),
        }))
        .id();
    let revealed = scenario
        .add_creature_to_hand(P0, "Reveal Payment", 1, 1)
        .id();
    let target = scenario.add_creature(P1, "Reveal Cost Target", 1, 1).id();
    let mut runner = scenario.build();

    activate(&mut runner, source);
    choose_target(&mut runner, target);
    let WaitingFor::PayCost { kind, choices, .. } = runner.state().waiting_for.clone() else {
        panic!("target declaration must surface the reveal cost");
    };
    assert_eq!(kind, PayCostKind::Reveal);
    assert!(choices.contains(&revealed));

    runner
        .act(GameAction::SelectCards {
            cards: vec![revealed],
        })
        .expect("revealing the target-first activation payment must succeed");
    assert_eq!(runner.state().stack.len(), 1);
}

#[test]
fn targeted_activation_surfaces_behold_cost_after_target_selection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Targeted Behold Cost", 1, 1)
        .with_ability_definition(targeted_damage_cost_ability(AbilityCost::Behold {
            count: 1,
            filter: TargetFilter::Typed(TypedFilter::creature()),
            action: BeholdCostAction::ChooseOrReveal,
            type_choice: None,
        }))
        .id();
    let behold = scenario.add_creature(P0, "Behold Payment", 1, 1).id();
    let target = scenario.add_creature(P1, "Behold Cost Target", 1, 1).id();
    let mut runner = scenario.build();

    activate(&mut runner, source);
    choose_target(&mut runner, target);
    let WaitingFor::PayCost { kind, choices, .. } = runner.state().waiting_for.clone() else {
        panic!("target declaration must surface the behold cost");
    };
    assert_eq!(
        kind,
        PayCostKind::Behold {
            action: BeholdCostAction::ChooseOrReveal,
        }
    );
    assert!(choices.contains(&behold));

    runner
        .act(GameAction::SelectCards {
            cards: vec![behold],
        })
        .expect("beholding the target-first activation payment must succeed");
    assert_eq!(runner.state().stack.len(), 1);
}

fn graveyard_exile_replacement(label: &str) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: Zone::Exile,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
        .description(label.to_string())
}

#[test]
fn target_first_mill_cost_resumes_after_replacement_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Targeted Mill Cost", 1, 1)
        .with_ability_definition(targeted_damage_cost_ability(AbilityCost::Mill { count: 1 }))
        .id();
    let target = scenario.add_creature(P1, "Mill Cost Target", 1, 1).id();
    let milled = scenario.add_card_to_library_top(P0, "Mill Payment Card");
    for label in ["Mill Redirect One", "Mill Redirect Two"] {
        scenario
            .add_creature(P0, label, 0, 0)
            .as_enchantment()
            .with_replacement_definition(graveyard_exile_replacement(label));
    }
    let mut runner = scenario.build();

    activate(&mut runner, source);
    choose_target(&mut runner, target);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("choosing the mill redirect must resume the target-first activation");

    assert_eq!(runner.state().objects[&milled].zone, Zone::Exile);
    assert_eq!(runner.state().stack.len(), 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
