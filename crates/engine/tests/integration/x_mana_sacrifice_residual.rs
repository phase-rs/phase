//! Public activated ability payment for a concrete sacrifice count after X mana.
//! These abilities are synthetic typed fixtures, not printed-card coverage.

use std::panic::{catch_unwind, AssertUnwindSafe};

use engine::ai_support::legal_actions_full;
use engine::game::engine::{apply, EngineError};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Comparator, ControllerRef, CounterCostSelection,
    Effect, ManaContribution, ManaProduction, ModalChoice, QuantityExpr, QuantityRef,
    ReplacementDefinition, ReplacementMode, SacrificeAggregateStat, SacrificeCost,
    SacrificeRequirement, TapCreaturesRequirement, TargetFilter, TargetRef, TypeFilter,
    TypedFilter, REMOVE_COUNTER_COST_X,
};
use engine::types::actions::GameAction;
use engine::types::counter::{CounterMatch, CounterType};
use engine::types::game_state::{
    ActivationResidual, CastPaymentMode, CostResume, PersistedGameState, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

fn x_mana() -> AbilityCost {
    AbilityCost::Mana {
        cost: ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::X],
        },
    }
}

fn count_creatures(count: u32) -> AbilityCost {
    AbilityCost::Sacrifice(SacrificeCost::count(
        TypedFilter::creature()
            .controller(ControllerRef::You)
            .into(),
        count,
    ))
}

fn count_lands(count: u32) -> AbilityCost {
    AbilityCost::Sacrifice(SacrificeCost::count(
        TypedFilter::land().controller(ControllerRef::You).into(),
        count,
    ))
}

fn draw_x(cost: AbilityCost) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::Variable { name: "X".into() },
            },
            target: TargetFilter::Controller,
        },
    )
    .cost(cost)
}

fn damage_x(cost: AbilityCost) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::DealDamage {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::Variable { name: "X".into() },
            },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
    )
    .cost(cost)
}

fn optional_graveyard_exile_redirect() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .mode(ReplacementMode::Optional { decline: None })
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                enters_modified_if: None,
                face_down_profile: None,
            },
        ))
}

fn choose_x_and_finish_mana(runner: &mut GameRunner, x: u32) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseXValue { .. }
    ));
    let pending = runner.state().pending_cast.as_ref().unwrap();
    assert!(matches!(
        pending.activation_residual,
        ActivationResidual::XMana
    ));
    runner.act(GameAction::ChooseX { value: x }).unwrap();
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner.act(GameAction::PassPriority).unwrap();
    }
}

fn assert_x_guard_refuses(runner: &mut GameRunner, label: &str) {
    let result = catch_unwind(AssertUnwindSafe(|| {
        runner.act(GameAction::ChooseX { value: 1 })
    }));
    match result {
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied());
            assert_eq!(
                message,
                Some("non-self sacrifice/exile cost unhandled"),
                "{label}"
            );
        }
        Ok(Err(EngineError::ActionNotAllowed(message))) => {
            assert_eq!(
                message, "non-self sacrifice/exile cost unhandled",
                "{label}"
            );
        }
        Ok(other) => panic!("{label} unexpectedly proceeded: {other:?}"),
    }
}

#[test]
fn x_mana_count_two_pays_once_and_draws_announced_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "X count source", "")
        .with_ability_definition(draw_x(AbilityCost::Composite {
            costs: vec![x_mana(), count_creatures(2)],
        }))
        .id();
    let first = scenario.add_creature(P0, "First fodder", 1, 1).id();
    let second = scenario.add_creature(P0, "Second fodder", 1, 1).id();
    let third = scenario.add_creature(P0, "Unused fodder", 1, 1).id();
    let opposing = scenario.add_creature(P1, "Opponent creature", 1, 1).id();
    scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
    );
    let mut runner = scenario.build();
    let hand_before = runner.state().players[0].hand.len();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .unwrap();
    choose_x_and_finish_mana(&mut runner, 4);
    let WaitingFor::PayCost {
        player,
        choices,
        min_count,
        count,
        resume: CostResume::Spell { spell },
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected concrete Count2 sacrifice prompt: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((*player, *min_count, *count), (P0, 2, 2));
    assert_eq!(choices.len(), 3);
    assert!(choices.contains(&first) && choices.contains(&second) && choices.contains(&third));
    assert!(!choices.contains(&opposing));
    assert!(matches!(
        spell.activation_residual,
        ActivationResidual::XMana
    ));
    assert_eq!(spell.ability.chosen_x, Some(4));
    for (actor, cards) in [
        (P1, vec![first, second]),
        (P0, vec![]),
        (P0, vec![first]),
        (P0, vec![first, second, third]),
        (P0, vec![first, first]),
        (P0, vec![first, opposing]),
    ] {
        let mut invalid = GameRunner::from_state(runner.state().clone());
        let before = serde_json::to_value(invalid.state()).unwrap();
        assert!(apply(
            invalid.state_mut(),
            actor,
            GameAction::SelectCards { cards }
        )
        .is_err());
        assert_eq!(serde_json::to_value(invalid.state()).unwrap(), before);
    }
    let selected = vec![first, second];
    assert!(legal_actions_full(runner.state())
        .0
        .contains(&GameAction::SelectCards {
            cards: selected.clone()
        }));
    runner
        .act(GameAction::SelectCards { cards: selected })
        .unwrap();
    assert_eq!(runner.state().stack.len(), 1);
    let entry = runner.state().stack.back().unwrap();
    assert_eq!((entry.source_id, entry.controller), (source, P0));
    let ability = entry.ability().unwrap();
    assert_eq!(ability.chosen_x, Some(4));
    let paid: Vec<_> = ability
        .cost_paid_objects
        .iter()
        .map(|record| record.snapshot().unwrap().object_id)
        .collect();
    assert_eq!(paid, vec![first, second]);
    assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&third].zone, Zone::Battlefield);
    runner.resolve_top();
    assert_eq!(runner.state().players[0].hand.len(), hand_before + 4);
}

#[test]
fn x_mana_count_zero_acknowledgment_keeps_announced_x() {
    for x in [0, 4] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_enchantment_from_oracle(P0, "Zero count source", "")
            .with_ability_definition(draw_x(AbilityCost::Composite {
                costs: vec![x_mana(), count_creatures(0)],
            }))
            .id();
        scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
        );
        let mut runner = scenario.build();
        let hand_before = runner.state().players[0].hand.len();
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .unwrap();
        choose_x_and_finish_mana(&mut runner, x);
        let WaitingFor::PayCost {
            player,
            choices,
            min_count,
            count,
            resume: CostResume::Spell { spell },
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "expected Count0 acknowledgment: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!((*player, *min_count, *count), (P0, 0, 0));
        assert!(choices.is_empty());
        assert!(matches!(
            spell.activation_residual,
            ActivationResidual::XMana
        ));
        assert_eq!(spell.ability.chosen_x, Some(x));
        runner
            .act(GameAction::SelectCards { cards: vec![] })
            .unwrap();
        assert_eq!(runner.state().stack.len(), 1);
        let ability = runner.state().stack.back().unwrap().ability().unwrap();
        assert_eq!(ability.chosen_x, Some(x));
        assert!(ability.cost_paid_objects.is_empty());
        runner.resolve_top();
        assert_eq!(
            runner.state().players[0].hand.len(),
            hand_before + x as usize
        );
    }
}

#[test]
fn x_mana_repeated_count_keeps_the_payer_after_source_sacrifice() {
    for owner in [P0, P1] {
        for source_first in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let one = AbilityCost::Sacrifice(SacrificeCost::count(
                TypedFilter::permanent()
                    .controller(ControllerRef::You)
                    .into(),
                1,
            ));
            let source = scenario
                .add_enchantment_from_oracle(owner, "Source selectable", "")
                .controlled_by(P0)
                .with_ability_definition(draw_x(AbilityCost::Composite {
                    costs: vec![x_mana(), one.clone(), one],
                }))
                .id();
            let other = scenario.add_enchantment_from_oracle(P0, "Other", "").id();
            let opponent = scenario
                .add_enchantment_from_oracle(P1, "Opponent", "")
                .id();
            scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
            scenario.with_mana_pool(
                P0,
                vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
            );
            let mut runner = scenario.build();
            let hand_before = runner.state().players[0].hand.len();
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                })
                .unwrap();
            choose_x_and_finish_mana(&mut runner, 4);
            let (first, second) = if source_first {
                (source, other)
            } else {
                (other, source)
            };
            for selected in [first, second] {
                let WaitingFor::PayCost {
                    player,
                    choices,
                    min_count,
                    count,
                    resume: CostResume::Spell { spell },
                    ..
                } = &runner.state().waiting_for
                else {
                    panic!(
                        "expected repeated Count1 prompt: {:?}",
                        runner.state().waiting_for
                    );
                };
                assert_eq!((*player, *min_count, *count), (P0, 1, 1));
                assert!(choices.contains(&selected));
                assert!(!choices.contains(&opponent));
                assert!(matches!(
                    spell.activation_residual,
                    ActivationResidual::XMana
                ));
                assert_eq!(spell.ability.chosen_x, Some(4));
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![selected],
                    })
                    .unwrap();
            }
            let ability = runner.state().stack.back().unwrap().ability().unwrap();
            let paid: Vec<_> = ability
                .cost_paid_objects
                .iter()
                .map(|record| record.snapshot().unwrap().object_id)
                .collect();
            assert_eq!(paid, vec![first, second]);
            assert_eq!(ability.chosen_x, Some(4));
            assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
            assert_eq!(runner.state().objects[&other].zone, Zone::Graveyard);
            runner.resolve_top();
            assert_eq!(runner.state().players[0].hand.len(), hand_before + 4);
        }
    }
}

#[test]
fn x_mana_targeted_count_keeps_target_and_records_crime_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Targeted X source", "")
        .with_ability_definition(damage_x(AbilityCost::Composite {
            costs: vec![x_mana(), count_creatures(1)],
        }))
        .id();
    let fodder = scenario.add_creature(P0, "Cost fodder", 1, 1).id();
    let first = scenario.add_creature(P1, "Untargeted", 5, 5).id();
    let second = scenario.add_creature(P1, "Chosen target", 5, 5).id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
    );
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .unwrap();
    assert!(matches!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .unwrap()
            .activation_residual,
        ActivationResidual::XMana
    ));
    runner.act(GameAction::ChooseX { value: 4 }).unwrap();
    let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for else {
        panic!(
            "target declaration must precede cost payment: {:?}",
            runner.state().waiting_for
        );
    };
    assert!(matches!(
        pending_cast.activation_residual,
        ActivationResidual::XMana
    ));
    assert_eq!(pending_cast.ability.chosen_x, Some(4));
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(second)],
        })
        .unwrap();
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner.act(GameAction::PassPriority).unwrap();
    }
    let WaitingFor::PayCost {
        player,
        choices,
        min_count,
        count,
        resume: CostResume::Spell { spell },
        ..
    } = &runner.state().waiting_for
    else {
        panic!("targeted Count1 prompt: {:?}", runner.state().waiting_for);
    };
    assert_eq!((*player, *min_count, *count), (P0, 1, 1));
    assert_eq!(choices, &vec![fodder]);
    assert!(matches!(
        spell.activation_residual,
        ActivationResidual::XMana
    ));
    assert_eq!(spell.ability.chosen_x, Some(4));
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .unwrap();
    assert_eq!(runner.state().stack.len(), 1);
    let ability = runner.state().stack.back().unwrap().ability().unwrap();
    assert_eq!(ability.chosen_x, Some(4));
    assert_eq!(ability.targets, vec![TargetRef::Object(second)]);
    // CR 700.13: Targeting an opponent's permanent commits a crime as the
    // activated ability is put on the stack.
    assert_eq!(runner.state().players[0].crimes_committed_this_turn, 1);
    runner.resolve_top();
    assert_eq!(runner.state().objects[&first].damage_marked, 0);
    assert_eq!(runner.state().objects[&second].damage_marked, 4);
    assert_eq!(runner.state().players[0].crimes_committed_this_turn, 1);
}

#[test]
fn x_mana_count_plus_tap_refuses_before_any_sacrifice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Mixed cost source", "")
        .with_ability_definition(draw_x(AbilityCost::Composite {
            costs: vec![
                x_mana(),
                AbilityCost::Tap,
                AbilityCost::Sacrifice(SacrificeCost::count(
                    TypedFilter::permanent()
                        .controller(ControllerRef::You)
                        .into(),
                    1,
                )),
            ],
        }))
        .id();
    let other = scenario.add_enchantment_from_oracle(P0, "Other", "").id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
    );
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .unwrap();
    assert!(matches!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .unwrap()
            .activation_residual,
        ActivationResidual::XMana
    ));
    let result = catch_unwind(AssertUnwindSafe(|| {
        runner.act(GameAction::ChooseX { value: 1 })
    }));
    match result {
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied());
            assert_eq!(message, Some("non-self sacrifice/exile cost unhandled"));
        }
        Ok(Err(EngineError::ActionNotAllowed(message))) => {
            assert_eq!(message, "non-self sacrifice/exile cost unhandled");
        }
        Ok(other) => panic!("mixed Tap suffix unexpectedly proceeded: {other:?}"),
    }
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&other].zone, Zone::Battlefield);
    assert!(!runner.state().objects[&source].tapped);
}

#[test]
fn x_mana_replacement_pause_restores_payer_and_announced_x() {
    for accept_first in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let one = AbilityCost::Sacrifice(SacrificeCost::count(
            TypedFilter::permanent()
                .controller(ControllerRef::You)
                .into(),
            1,
        ));
        let source = scenario
            .add_enchantment_from_oracle(P1, "Replaced source", "")
            .controlled_by(P0)
            .with_ability_definition(draw_x(AbilityCost::Composite {
                costs: vec![x_mana(), one.clone(), one],
            }))
            .id();
        let other = scenario.add_enchantment_from_oracle(P0, "Other", "").id();
        scenario
            .add_creature(P1, "Optional graveyard redirect", 1, 1)
            .with_replacement_definition(optional_graveyard_exile_redirect());
        scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
        );
        let mut runner = scenario.build();
        let hand_before = runner.state().players[0].hand.len();
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .unwrap();
        choose_x_and_finish_mana(&mut runner, 4);
        let WaitingFor::PayCost { choices, .. } = &runner.state().waiting_for else {
            panic!("first Count1 prompt: {:?}", runner.state().waiting_for);
        };
        assert!(choices.contains(&source));
        runner
            .act(GameAction::SelectCards {
                cards: vec![source],
            })
            .unwrap();
        let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
            panic!("source sacrifice must pause for optional replacement");
        };
        let choice = candidates
            .iter()
            .position(|candidate| {
                candidate.description == if accept_first { "Accept" } else { "Decline" }
            })
            .unwrap();
        assert!(runner.state().pending_cost_move_resume.is_some());
        // CR 614.6: Accepting the optional replacement substitutes exile for
        // the graveyard move. The saved cost cursor resumes under the payer.
        let encoded =
            serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
        let restored = serde_json::from_value::<PersistedGameState>(encoded)
            .unwrap()
            .into_game_state()
            .unwrap();
        runner = GameRunner::from_state(restored);
        runner
            .act(GameAction::ChooseReplacement { index: choice })
            .unwrap();
        assert_eq!(
            runner.state().objects[&source].zone,
            if accept_first {
                Zone::Exile
            } else {
                Zone::Graveyard
            }
        );
        let WaitingFor::PayCost {
            player,
            choices,
            min_count,
            count,
            resume: CostResume::Spell { spell },
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "second Count1 prompt after source exit: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!((*player, *min_count, *count), (P0, 1, 1));
        assert_eq!(choices, &vec![other]);
        assert!(matches!(
            spell.activation_residual,
            ActivationResidual::XMana
        ));
        assert_eq!(spell.ability.chosen_x, Some(4));
        runner
            .act(GameAction::SelectCards { cards: vec![other] })
            .unwrap();
        let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
            panic!("second sacrifice must pause for optional replacement");
        };
        let decline = candidates
            .iter()
            .position(|candidate| candidate.description == "Decline")
            .unwrap();
        runner
            .act(GameAction::ChooseReplacement { index: decline })
            .unwrap();
        assert_eq!(runner.state().objects[&other].zone, Zone::Graveyard);
        let entry = runner.state().stack.back().unwrap();
        assert_eq!((entry.source_id, entry.controller), (source, P0));
        let ability = entry.ability().unwrap();
        assert_eq!(ability.chosen_x, Some(4));
        let paid: Vec<_> = ability
            .cost_paid_objects
            .iter()
            .map(|record| record.snapshot().unwrap().object_id)
            .collect();
        assert_eq!(paid, vec![source, other]);
        runner.resolve_top();
        assert_eq!(runner.state().players[0].hand.len(), hand_before + 4);
    }
}

#[test]
fn x_mana_unsupported_siblings_refuse_before_count_payment() {
    for (label, unsupported) in [
        (
            "zero life",
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 0 },
            },
        ),
        ("unbounded sacrifice", count_creatures(u32::MAX)),
        (
            "graveyard exile",
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Graveyard),
                filter: None,
            },
        ),
        (
            "unresolved choice",
            AbilityCost::OneOf {
                costs: vec![count_creatures(1)],
            },
        ),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_enchantment_from_oracle(P0, label, "")
            .with_ability_definition(draw_x(AbilityCost::Composite {
                costs: vec![x_mana(), count_creatures(1), unsupported],
            }))
            .id();
        let fodder = scenario.add_creature(P0, "Count fodder", 1, 1).id();
        scenario.add_creature(P0, "Second fodder", 1, 1);
        scenario.add_land_to_graveyard(P0, "Exile candidate");
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
        );
        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .unwrap();
        assert!(matches!(
            runner
                .state()
                .pending_cast
                .as_ref()
                .unwrap()
                .activation_residual,
            ActivationResidual::XMana
        ));
        assert_x_guard_refuses(&mut runner, label);
        assert!(runner.state().stack.is_empty(), "{label}");
        assert_eq!(
            runner.state().objects[&source].zone,
            Zone::Battlefield,
            "{label}"
        );
        assert_eq!(
            runner.state().objects[&fodder].zone,
            Zone::Battlefield,
            "{label}"
        );
    }
}

#[test]
fn x_mana_nested_zero_and_positive_counts_each_pay_once() {
    for zero_first in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let suffix = if zero_first {
            vec![count_creatures(0), count_creatures(1)]
        } else {
            vec![count_creatures(1), count_creatures(0)]
        };
        let source = scenario
            .add_enchantment_from_oracle(P0, "Nested X source", "")
            .with_ability_definition(draw_x(AbilityCost::Composite {
                costs: vec![x_mana(), AbilityCost::Composite { costs: suffix }],
            }))
            .id();
        let fodder = scenario.add_creature(P0, "Nested fodder", 1, 1).id();
        scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
        );
        let mut runner = scenario.build();
        let hand_before = runner.state().players[0].hand.len();
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .unwrap();
        choose_x_and_finish_mana(&mut runner, 4);
        for count_expected in if zero_first { [0, 1] } else { [1, 0] } {
            let WaitingFor::PayCost {
                player,
                choices,
                min_count,
                count,
                resume: CostResume::Spell { spell },
                ..
            } = &runner.state().waiting_for
            else {
                panic!("nested Count prompt: {:?}", runner.state().waiting_for);
            };
            assert_eq!(*player, P0);
            assert_eq!((*min_count, *count), (count_expected, count_expected));
            assert!(matches!(
                spell.activation_residual,
                ActivationResidual::XMana
            ));
            assert_eq!(spell.ability.chosen_x, Some(4));
            let selected = if count_expected == 0 {
                vec![]
            } else {
                assert_eq!(choices, &vec![fodder]);
                vec![fodder]
            };
            runner
                .act(GameAction::SelectCards { cards: selected })
                .unwrap();
        }
        let ability = runner.state().stack.back().unwrap().ability().unwrap();
        assert_eq!(ability.chosen_x, Some(4));
        assert_eq!(
            ability
                .cost_paid_objects
                .iter()
                .map(|record| record.snapshot().unwrap().object_id)
                .collect::<Vec<_>>(),
            vec![fodder]
        );
        assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
        runner.resolve_top();
        assert_eq!(runner.state().players[0].hand.len(), hand_before + 4);
    }
}

#[test]
fn x_mana_count_two_after_costed_mana_source_pays_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Outer X source", "")
        .with_ability_definition(draw_x(AbilityCost::Composite {
            costs: vec![
                x_mana(),
                AbilityCost::Sacrifice(SacrificeCost::count(
                    TypedFilter::land().controller(ControllerRef::You).into(),
                    2,
                )),
            ],
        }))
        .id();
    let mana_source = scenario
        .add_land_from_oracle(P0, "Costed double-green source", "")
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green, ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
                ],
            }),
        )
        .id();
    let lands: Vec<_> = (0..4)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Green))
        .collect();
    scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
    let mut runner = scenario.build();
    for land in lands.iter().skip(2) {
        runner.state_mut().objects.get_mut(land).unwrap().tapped = true;
    }
    let hand_before = runner.state().players[0].hand.len();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .unwrap();
    assert!(matches!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .unwrap()
            .activation_residual,
        ActivationResidual::XMana
    ));
    runner
        .state_mut()
        .pending_cast
        .as_mut()
        .unwrap()
        .payment_mode = CastPaymentMode::Manual;
    runner.act(GameAction::ChooseX { value: 4 }).unwrap();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { player: P0, .. }
        ),
        "expected manual mana window, got {:?}",
        runner.state().waiting_for
    );
    // CR 601.2g + CR 602.2b + CR 605.3a: Activate an available mana
    // ability inside the outer activation's mana-payment window.
    let (_, _, grouped) = legal_actions_full(runner.state());
    let action = grouped
        .get(&mana_source)
        .into_iter()
        .flatten()
        .find(|action| {
            matches!(
                action,
                GameAction::ActivateManaSource { .. } | GameAction::TapLandForMana { .. }
            )
        })
        .cloned()
        .expect("engine-authored costed mana source");
    runner.act(action).unwrap();
    assert_eq!(runner.state().objects[&mana_source].zone, Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    for land in lands.iter().take(2) {
        let (_, _, grouped) = legal_actions_full(runner.state());
        let action = grouped
            .get(land)
            .into_iter()
            .flatten()
            .find(|action| matches!(action, GameAction::TapLandForMana { .. }))
            .cloned()
            .expect("engine-authored basic-land mana source");
        runner.act(action).unwrap();
    }
    assert_eq!(runner.state().players[0].mana_pool.total(), 4);
    runner.act(GameAction::PassPriority).unwrap();
    let WaitingFor::PayCost {
        player,
        choices,
        min_count,
        count,
        resume: CostResume::Spell { spell },
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "outer Count2 sacrifice prompt: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((*player, *min_count, *count), (P0, 2, 2));
    assert_eq!(choices.len(), 4);
    assert!(matches!(
        spell.activation_residual,
        ActivationResidual::XMana
    ));
    assert_eq!(spell.ability.chosen_x, Some(4));
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    let selected = lands[2..].to_vec();
    assert!(legal_actions_full(runner.state())
        .0
        .contains(&GameAction::SelectCards {
            cards: selected.clone()
        }));
    runner
        .act(GameAction::SelectCards {
            cards: selected.clone(),
        })
        .unwrap();
    let ability = runner.state().stack.back().unwrap().ability().unwrap();
    assert_eq!(ability.chosen_x, Some(4));
    assert_eq!(
        ability
            .cost_paid_objects
            .iter()
            .map(|record| record.snapshot().unwrap().object_id)
            .collect::<Vec<_>>(),
        selected
    );
    assert!(lands[..2]
        .iter()
        .all(|id| runner.state().objects[id].zone == Zone::Battlefield));
    runner.resolve_top();
    assert_eq!(runner.state().players[0].hand.len(), hand_before + 4);
}

#[test]
fn x_mana_count_preserves_announced_modal_choice() {
    for with_x in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let cost = if with_x {
            AbilityCost::Composite {
                costs: vec![x_mana(), count_creatures(1)],
            }
        } else {
            count_creatures(1)
        };
        let source = scenario
            .add_enchantment_from_oracle(P0, "Modal Count source", "")
            .with_ability_definition(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                )
                .cost(cost)
                .with_modal(
                    ModalChoice {
                        min_choices: 1,
                        max_choices: 1,
                        mode_count: 2,
                        mode_descriptions: vec!["One life".into(), "Four life".into()],
                        ..ModalChoice::default()
                    },
                    vec![
                        AbilityDefinition::new(
                            AbilityKind::Activated,
                            Effect::GainLife {
                                amount: QuantityExpr::Fixed { value: 1 },
                                player: TargetFilter::Controller,
                            },
                        ),
                        AbilityDefinition::new(
                            AbilityKind::Activated,
                            Effect::GainLife {
                                amount: QuantityExpr::Fixed { value: 4 },
                                player: TargetFilter::Controller,
                            },
                        ),
                    ],
                ),
            )
            .id();
        let fodder = scenario.add_creature(P0, "Modal fodder", 1, 1).id();
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
        );
        let mut runner = scenario.build();
        let life_before = runner.state().players[0].life;
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .unwrap();
        let mut selected_mode = false;
        let mut selected_x = false;
        for _ in 0..3 {
            match &runner.state().waiting_for {
                WaitingFor::AbilityModeChoice { .. } => {
                    // CR 601.2b + CR 602.2b: The announced mode belongs to
                    // this activation through its later cost payment.
                    runner
                        .act(GameAction::SelectModes { indices: vec![1] })
                        .unwrap();
                    selected_mode = true;
                }
                WaitingFor::ChooseXValue { .. } => {
                    assert!(with_x);
                    runner.act(GameAction::ChooseX { value: 4 }).unwrap();
                    selected_x = true;
                }
                WaitingFor::ManaPayment { .. } => {
                    runner.act(GameAction::PassPriority).unwrap();
                }
                WaitingFor::PayCost { .. } => break,
                waiting => panic!("modal activation did not reach Count payment: {waiting:?}"),
            }
        }
        assert!(selected_mode);
        assert_eq!(selected_x, with_x);
        let WaitingFor::PayCost {
            player,
            choices,
            min_count,
            count,
            resume: CostResume::Spell { spell },
            ..
        } = &runner.state().waiting_for
        else {
            panic!("modal Count payment: {:?}", runner.state().waiting_for);
        };
        assert_eq!((*player, *min_count, *count), (P0, 1, 1));
        assert_eq!(choices, &vec![fodder]);
        assert_eq!(spell.ability.chosen_x, with_x.then_some(4));
        assert_eq!(spell.ability.selected_mode_labels, vec!["Four life"]);
        runner
            .act(GameAction::SelectCards {
                cards: vec![fodder],
            })
            .unwrap();
        let ability = runner.state().stack.back().unwrap().ability().unwrap();
        assert_eq!(ability.chosen_x, with_x.then_some(4));
        assert_eq!(ability.selected_mode_labels, vec!["Four life"]);
        assert_eq!(
            ability.cost_paid_objects[0].snapshot().unwrap().object_id,
            fodder
        );
        runner.resolve_top();
        assert_eq!(runner.state().players[0].life, life_before + 4);
    }
}

#[test]
fn x_mana_pure_unresolved_one_of_keeps_its_existing_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Pure choice X source", "")
        .with_ability_definition(draw_x(AbilityCost::Composite {
            costs: vec![
                x_mana(),
                AbilityCost::OneOf {
                    costs: vec![count_creatures(1)],
                },
            ],
        }))
        .id();
    let fodder = scenario.add_creature(P0, "Pure choice fodder", 1, 1).id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
    );
    scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .unwrap();
    choose_x_and_finish_mana(&mut runner, 4);
    let WaitingFor::ActivationCostOneOfChoice {
        player,
        costs,
        pending_cast,
    } = &runner.state().waiting_for
    else {
        panic!("pure OneOf choice: {:?}", runner.state().waiting_for);
    };
    assert_eq!(*player, P0);
    assert_eq!(costs, &vec![count_creatures(1)]);
    assert_eq!(pending_cast.ability.chosen_x, Some(4));
    assert!(matches!(
        pending_cast.activation_residual,
        ActivationResidual::None
    ));
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Battlefield);
    assert!(runner.state().stack.is_empty());
    let hand_before = runner.state().players[0].hand.len();
    runner
        .act(GameAction::ChooseActivationCostBranch { index: 0 })
        .unwrap();
    let WaitingFor::PayCost {
        player,
        choices,
        min_count,
        count,
        resume: CostResume::Spell { spell },
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "selected OneOf Count payment: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((*player, *min_count, *count), (P0, 1, 1));
    assert_eq!(choices, &vec![fodder]);
    assert_eq!(spell.ability.chosen_x, Some(4));
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .unwrap();
    let ability = runner.state().stack.back().unwrap().ability().unwrap();
    assert_eq!(ability.chosen_x, Some(4));
    assert_eq!(
        ability.cost_paid_objects[0].snapshot().unwrap().object_id,
        fodder
    );
    runner.resolve_top();
    assert_eq!(runner.state().players[0].hand.len(), hand_before + 4);
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
}

#[test]
fn x_mana_count_two_at_zero_still_pays_two_and_ignores_cast_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Count2 zero X source", "")
        .with_ability_definition(draw_x(AbilityCost::Composite {
            costs: vec![x_mana(), count_creatures(2)],
        }))
        .id();
    let first = scenario.add_creature(P0, "First zero X fodder", 1, 1).id();
    let second = scenario.add_creature(P0, "Second zero X fodder", 1, 1).id();
    scenario.with_library_top(P0, &["A", "B"]);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .unwrap()
        .cost_x_paid = Some(9);
    let hand_before = runner.state().players[0].hand.len();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .unwrap();
    choose_x_and_finish_mana(&mut runner, 0);
    let WaitingFor::PayCost {
        player,
        choices,
        min_count,
        count,
        resume: CostResume::Spell { spell },
        ..
    } = &runner.state().waiting_for
    else {
        panic!("Count2 at X0: {:?}", runner.state().waiting_for);
    };
    assert_eq!((*player, *min_count, *count), (P0, 2, 2));
    assert_eq!(choices, &vec![first, second]);
    assert_eq!(spell.ability.chosen_x, Some(0));
    assert_eq!(runner.state().objects[&source].cost_x_paid, Some(9));
    runner
        .act(GameAction::SelectCards {
            cards: vec![first, second],
        })
        .unwrap();
    let ability = runner.state().stack.back().unwrap().ability().unwrap();
    assert_eq!(ability.chosen_x, Some(0));
    assert_eq!(
        ability
            .cost_paid_objects
            .iter()
            .map(|record| record.snapshot().unwrap().object_id)
            .collect::<Vec<_>>(),
        vec![first, second]
    );
    runner.resolve_top();
    assert_eq!(runner.state().players[0].hand.len(), hand_before);
    assert_eq!(runner.state().objects[&source].cost_x_paid, Some(9));
}

#[test]
fn x_mana_two_activations_on_one_source_keep_independent_announcements() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Repeated zero Count source", "")
        .with_ability_definition(draw_x(AbilityCost::Composite {
            costs: vec![x_mana(), count_creatures(0)],
        }))
        .id();
    scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
    );
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .unwrap()
        .cost_x_paid = Some(9);
    let hand_before = runner.state().players[0].hand.len();
    for (x, expected_hand) in [(4, hand_before + 4), (0, hand_before + 4)] {
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .unwrap();
        choose_x_and_finish_mana(&mut runner, x);
        let WaitingFor::PayCost {
            player,
            min_count,
            count,
            resume: CostResume::Spell { spell },
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "successive Count0 payment: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!((*player, *min_count, *count), (P0, 0, 0));
        assert_eq!((spell.object_id, spell.ability.chosen_x), (source, Some(x)));
        assert_eq!(runner.state().objects[&source].cost_x_paid, Some(9));
        runner
            .act(GameAction::SelectCards { cards: vec![] })
            .unwrap();
        assert_eq!(
            runner
                .state()
                .stack
                .back()
                .unwrap()
                .ability()
                .unwrap()
                .chosen_x,
            Some(x)
        );
        runner.resolve_top();
        assert_eq!(runner.state().players[0].hand.len(), expected_hand);
        assert!(runner.state().stack.is_empty());
    }
}

#[test]
fn x_mana_distinct_creature_and_land_counts_resume_in_both_orders() {
    for reverse in [false, true] {
        for nested in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let mut suffix = vec![count_creatures(1), count_lands(1)];
            if reverse {
                suffix.reverse();
            }
            let cost = if nested {
                AbilityCost::Composite {
                    costs: vec![x_mana(), AbilityCost::Composite { costs: suffix }],
                }
            } else {
                let mut costs = vec![x_mana()];
                costs.extend(suffix);
                AbilityCost::Composite { costs }
            };
            let source = scenario
                .add_enchantment_from_oracle(P0, "Distinct Count X source", "")
                .with_ability_definition(draw_x(cost))
                .id();
            let creature = scenario.add_creature(P0, "Only creature", 1, 1).id();
            let land = scenario.add_basic_land(P0, ManaColor::Green);
            scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
            scenario.with_mana_pool(
                P0,
                vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
            );
            let mut runner = scenario.build();
            let hand_before = runner.state().players[0].hand.len();
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                })
                .unwrap();
            choose_x_and_finish_mana(&mut runner, 4);
            let expected = if reverse {
                [land, creature]
            } else {
                [creature, land]
            };
            for id in expected {
                let WaitingFor::PayCost {
                    player,
                    choices,
                    min_count,
                    count,
                    resume: CostResume::Spell { spell },
                    ..
                } = &runner.state().waiting_for
                else {
                    panic!("distinct Count payment: {:?}", runner.state().waiting_for);
                };
                assert_eq!((*player, *min_count, *count), (P0, 1, 1));
                assert_eq!(choices, &vec![id]);
                assert_eq!(
                    (spell.object_id, spell.activation_ability_index),
                    (source, Some(0))
                );
                assert_eq!(spell.ability.chosen_x, Some(4));
                assert!(matches!(
                    spell.activation_residual,
                    ActivationResidual::XMana
                ));
                runner
                    .act(GameAction::SelectCards { cards: vec![id] })
                    .unwrap();
                assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
            }
            let ability = runner.state().stack.back().unwrap().ability().unwrap();
            assert_eq!(ability.chosen_x, Some(4));
            assert_eq!(
                ability
                    .cost_paid_objects
                    .iter()
                    .map(|record| record.snapshot().unwrap().object_id)
                    .collect::<Vec<_>>(),
                expected
            );
            runner.resolve_top();
            assert_eq!(runner.state().players[0].hand.len(), hand_before + 4);
            assert!(runner.state().stack.is_empty());
        }
    }
}

#[test]
fn x_mana_target_and_crime_survive_optional_replacement_restore() {
    for accept in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_enchantment_from_oracle(P0, "Replaced targeted X source", "")
            .with_ability_definition(damage_x(AbilityCost::Composite {
                costs: vec![x_mana(), count_creatures(1)],
            }))
            .id();
        let fodder = scenario
            .add_creature(P0, "Replaced payment fodder", 1, 1)
            .id();
        let first = scenario
            .add_creature(P1, "Untargeted replacement control", 5, 5)
            .id();
        let second = scenario
            .add_creature(P1, "Targeted replacement control", 5, 5)
            .id();
        scenario
            .add_creature(P1, "Optional graveyard redirect", 1, 1)
            .with_replacement_definition(optional_graveyard_exile_redirect());
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
        );
        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseXValue { .. }
        ));
        runner.act(GameAction::ChooseX { value: 4 }).unwrap();
        let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for else {
            panic!(
                "targeted replacement declaration: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(pending_cast.ability.chosen_x, Some(4));
        assert!(matches!(
            pending_cast.activation_residual,
            ActivationResidual::XMana
        ));
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(second)],
            })
            .unwrap();
        if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            runner.act(GameAction::PassPriority).unwrap();
        }
        let WaitingFor::PayCost {
            player,
            choices,
            min_count,
            count,
            resume: CostResume::Spell { spell },
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "targeted replacement cost: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!((*player, *min_count, *count), (P0, 1, 1));
        assert_eq!(choices, &vec![fodder]);
        assert_eq!(spell.ability.chosen_x, Some(4));
        assert_eq!(spell.ability.targets, vec![TargetRef::Object(second)]);
        assert!(spell.crime_candidate);
        runner
            .act(GameAction::SelectCards {
                cards: vec![fodder],
            })
            .unwrap();
        let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
            panic!("optional replacement must pause the targeted payment");
        };
        let choice = candidates
            .iter()
            .position(|candidate| {
                candidate.description == if accept { "Accept" } else { "Decline" }
            })
            .unwrap();
        let encoded =
            serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
        let restored = serde_json::from_value::<PersistedGameState>(encoded)
            .unwrap()
            .into_game_state()
            .unwrap();
        runner = GameRunner::from_state(restored);
        runner
            .act(GameAction::ChooseReplacement { index: choice })
            .unwrap();
        assert_eq!(
            runner.state().objects[&fodder].zone,
            if accept { Zone::Exile } else { Zone::Graveyard }
        );
        let entry = runner.state().stack.back().unwrap();
        assert_eq!((entry.source_id, entry.controller), (source, P0));
        let ability = entry.ability().unwrap();
        assert_eq!(ability.chosen_x, Some(4));
        assert_eq!(ability.targets, vec![TargetRef::Object(second)]);
        assert_eq!(
            ability.cost_paid_objects[0].snapshot().unwrap().object_id,
            fodder
        );
        assert_eq!(runner.state().players[0].crimes_committed_this_turn, 1);
        runner.resolve_top();
        assert_eq!(runner.state().objects[&first].damage_marked, 0);
        assert_eq!(runner.state().objects[&second].damage_marked, 4);
        assert_eq!(runner.state().players[0].crimes_committed_this_turn, 1);
    }
}

#[test]
fn x_mana_mixed_requirement_life_tap_and_exile_shapes_refuse_atomically() {
    let creature: TargetFilter = TypedFilter::creature()
        .controller(ControllerRef::You)
        .into();
    let cases = vec![
        (
            "fixed life zero",
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 0 },
            },
            1,
        ),
        (
            "fixed life one",
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 1 },
            },
            1,
        ),
        ("unbounded sacrifice", count_creatures(u32::MAX), 1),
        (
            "aggregate sacrifice",
            AbilityCost::Sacrifice(SacrificeCost::new(
                creature.clone(),
                SacrificeRequirement::Aggregate {
                    stat: SacrificeAggregateStat::TotalPower,
                    comparator: Comparator::GE,
                    value: 1,
                },
            )),
            1,
        ),
        ("tap after Count0", AbilityCost::Tap, 0),
        ("tap after Count1", AbilityCost::Tap, 1),
        (
            "hand exile",
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Hand),
                filter: None,
            },
            1,
        ),
        (
            "graveyard exile",
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Graveyard),
                filter: None,
            },
            1,
        ),
        (
            "battlefield exile",
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Battlefield),
                filter: Some(creature.clone()),
            },
            1,
        ),
    ];
    for (label, unsupported, count) in cases {
        for reversed in [false, true] {
            for nested in [false, true] {
                let mut suffix = vec![count_creatures(count), unsupported.clone()];
                if reversed {
                    suffix.reverse();
                }
                let cost = if nested {
                    AbilityCost::Composite {
                        costs: vec![x_mana(), AbilityCost::Composite { costs: suffix }],
                    }
                } else {
                    let mut costs = vec![x_mana()];
                    costs.extend(suffix);
                    AbilityCost::Composite { costs }
                };
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let source = scenario
                    .add_enchantment_from_oracle(P0, label, "")
                    .with_ability_definition(draw_x(cost))
                    .id();
                let fodder = scenario.add_creature(P0, "Count fodder", 1, 1).id();
                let second = scenario.add_creature(P0, "Second fodder", 1, 1).id();
                let hand = scenario.add_card_to_hand(P0, "Hand exile candidate");
                let graveyard = scenario
                    .add_land_to_graveyard(P0, "Graveyard exile candidate")
                    .id();
                scenario.with_mana_pool(
                    P0,
                    vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
                );
                let mut runner = scenario.build();
                runner
                    .act(GameAction::ActivateAbility {
                        source_id: source,
                        ability_index: 0,
                    })
                    .unwrap();
                assert!(
                    matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { .. }),
                    "{label}/{reversed}/{nested}"
                );
                assert!(matches!(
                    runner
                        .state()
                        .pending_cast
                        .as_ref()
                        .unwrap()
                        .activation_residual,
                    ActivationResidual::XMana
                ));
                assert_x_guard_refuses(&mut runner, &format!("{label}/{reversed}/{nested}"));
                assert!(
                    runner.state().stack.is_empty(),
                    "{label}/{reversed}/{nested}"
                );
                assert_eq!(runner.state().players[0].life, 20);
                assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
                assert!(!runner.state().objects[&source].tapped);
                for id in [fodder, second] {
                    assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
                    assert!(!runner.state().objects[&id].tapped);
                }
                assert_eq!(runner.state().objects[&hand].zone, Zone::Hand);
                assert_eq!(runner.state().objects[&graveyard].zone, Zone::Graveyard);
            }
        }
    }
}

#[test]
fn x_mana_targeted_tap_creatures_sibling_refuses_before_payment() {
    run_targeted_tap_creatures_sibling(true);
}

#[test]
fn x_mana_targeted_tap_creatures_sibling_refuses_after_slot_choice() {
    run_targeted_tap_creatures_sibling(false);
}

fn run_targeted_tap_creatures_sibling(bulk: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Targeted mixed TapCreatures", "")
        .with_ability_definition(damage_x(AbilityCost::Composite {
            costs: vec![
                x_mana(),
                count_creatures(1),
                AbilityCost::TapCreatures {
                    requirement: TapCreaturesRequirement::Count { count: 1 },
                    filter: TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .into(),
                },
            ],
        }))
        .id();
    let fodder = scenario.add_creature(P0, "Untapped fodder", 1, 1).id();
    let target = scenario.add_creature(P1, "Opponent target", 5, 5).id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
    );
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .unwrap();
    runner.act(GameAction::ChooseX { value: 1 }).unwrap();
    let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for else {
        panic!(
            "target declaration must precede costs: {:?}",
            runner.state().waiting_for
        );
    };
    assert!(matches!(
        pending_cast.activation_residual,
        ActivationResidual::XMana
    ));
    assert_eq!(pending_cast.ability.chosen_x, Some(1));
    assert!(matches!(
        pending_cast.activation_cost.as_ref(),
        Some(AbilityCost::Composite { costs })
            if costs.iter().any(|cost| matches!(cost, AbilityCost::Sacrifice(_)))
                && costs.iter().any(|cost| matches!(cost, AbilityCost::TapCreatures { .. }))
    ));
    let action = if bulk {
        GameAction::SelectTargets {
            targets: vec![TargetRef::Object(target)],
        }
    } else {
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        }
    };
    let result = catch_unwind(AssertUnwindSafe(|| runner.act(action)));
    match result {
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied());
            assert_eq!(message, Some("non-self sacrifice/exile cost unhandled"));
        }
        Ok(Err(EngineError::ActionNotAllowed(message))) => {
            assert_eq!(message, "non-self sacrifice/exile cost unhandled");
        }
        Ok(other) => panic!("targeted TapCreatures unexpectedly proceeded: {other:?}"),
    }
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Battlefield);
    assert!(!runner.state().objects[&fodder].tapped);
    assert_eq!(runner.state().objects[&target].damage_marked, 0);
}

#[test]
fn x_mana_modal_symbolic_counter_target_x0_refuses() {
    run_modal_symbolic_counter_sibling(true, 0);
}

#[test]
fn x_mana_modal_symbolic_counter_target_x1_refuses() {
    run_modal_symbolic_counter_sibling(true, 1);
}

#[test]
fn x_mana_modal_symbolic_counter_no_target_x0_refuses() {
    run_modal_symbolic_counter_sibling(false, 0);
}

#[test]
fn x_mana_modal_symbolic_counter_no_target_x1_refuses() {
    run_modal_symbolic_counter_sibling(false, 1);
}

fn run_modal_symbolic_counter_sibling(targeted: bool, x: u32) {
    let charge = CounterType::Generic("charge".into());
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cost = AbilityCost::Composite {
        costs: vec![
            x_mana(),
            count_creatures(1),
            AbilityCost::RemoveCounter {
                count: REMOVE_COUNTER_COST_X,
                counter_type: CounterMatch::OfType(charge.clone()),
                target: Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact))),
                selection: CounterCostSelection::SingleObject,
            },
        ],
    };
    let mode = if targeted {
        damage_x(cost.clone())
    } else {
        draw_x(cost.clone())
    };
    let source = scenario
        .add_enchantment_from_oracle(P0, "Modal symbolic cost", "")
        .with_ability_definition(mode.clone().with_modal(
            ModalChoice {
                min_choices: 1,
                max_choices: 1,
                mode_count: 2,
                mode_descriptions: vec!["Other mode".into(), "Chosen mode".into()],
                ..ModalChoice::default()
            },
            vec![
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                ),
                mode,
            ],
        ))
        .id();
    let fodder = scenario.add_creature(P0, "Cost fodder", 1, 1).id();
    let battery = scenario
        .add_creature(P0, "Counter battery", 1, 1)
        .as_artifact()
        .id();
    let victim = scenario.add_creature(P1, "Chosen victim", 5, 5).id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
    );
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&battery)
        .unwrap()
        .counters
        .insert(charge.clone(), 2);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::AbilityModeChoice { .. }
    ));
    runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .unwrap();
    let WaitingFor::ChooseXValue { pending_cast, .. } = &runner.state().waiting_for else {
        panic!(
            "modal X must be announced: {:?}",
            runner.state().waiting_for
        );
    };
    assert!(matches!(
        pending_cast.activation_residual,
        ActivationResidual::XMana
    ));
    assert_eq!(pending_cast.object_id, source);
    assert_eq!(pending_cast.chosen_modes, vec![1]);
    let result = catch_unwind(AssertUnwindSafe(|| {
        runner.act(GameAction::ChooseX { value: x })
    }));
    if targeted {
        let Ok(Ok(_)) = result else {
            panic!("targeted modal X must reach target selection: {result:?}");
        };
        let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for else {
            panic!(
                "targeted modal X must declare target: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(pending_cast.ability.chosen_x, Some(x));
        assert!(matches!(
            pending_cast.activation_residual,
            ActivationResidual::XMana
        ));
        assert_eq!(pending_cast.chosen_modes, vec![1]);
        let result = catch_unwind(AssertUnwindSafe(|| {
            runner.act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(victim)],
            })
        }));
        assert_modal_sibling_refusal(result, targeted, x);
    } else {
        assert_modal_sibling_refusal(result, targeted, x);
    }
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&battery].counters.get(&charge),
        Some(&2)
    );
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&victim].damage_marked, 0);
}

fn assert_modal_sibling_refusal(
    result: Result<
        Result<engine::types::game_state::ActionResult, EngineError>,
        Box<dyn std::any::Any + Send>,
    >,
    targeted: bool,
    x: u32,
) {
    match result {
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied());
            assert_eq!(message, Some("non-self sacrifice/exile cost unhandled"), "targeted={targeted} x={x}");
        }
        Ok(Err(EngineError::ActionNotAllowed(message))) => {
            assert_eq!(message, "non-self sacrifice/exile cost unhandled", "targeted={targeted} x={x}");
        }
        Ok(other) => panic!("modal symbolic sibling reached payment before validation targeted={targeted} x={x}: {other:?}"),
    }
}

#[test]
fn x_mana_modal_count_only_pays_after_mode_x_and_optional_target() {
    for targeted in [false, true] {
        for x in [0, 1] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let cost = AbilityCost::Composite {
                costs: vec![x_mana(), count_creatures(1)],
            };
            let chosen_mode = if targeted {
                damage_x(cost.clone())
            } else {
                draw_x(cost.clone())
            };
            let source = scenario
                .add_enchantment_from_oracle(P0, "Modal Count only", "")
                .with_ability_definition(chosen_mode.clone().with_modal(
                    ModalChoice {
                        min_choices: 1,
                        max_choices: 1,
                        mode_count: 2,
                        mode_descriptions: vec!["Other mode".into(), "Chosen mode".into()],
                        ..ModalChoice::default()
                    },
                    vec![
                        AbilityDefinition::new(
                            AbilityKind::Activated,
                            Effect::GainLife {
                                amount: QuantityExpr::Fixed { value: 1 },
                                player: TargetFilter::Controller,
                            },
                        ),
                        chosen_mode,
                    ],
                ))
                .id();
            let fodder = scenario.add_creature(P0, "Modal Count fodder", 1, 1).id();
            let victim = scenario.add_creature(P1, "Modal Count victim", 5, 5).id();
            scenario.with_mana_pool(
                P0,
                vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
            );
            if !targeted {
                scenario.with_library_top(P0, &["Draw A", "Draw B"]);
            }
            let mut runner = scenario.build();
            let hand_before = runner.state().players[0].hand.len();
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                })
                .unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::AbilityModeChoice { .. }
            ));
            runner
                .act(GameAction::SelectModes { indices: vec![1] })
                .unwrap();
            let WaitingFor::ChooseXValue { pending_cast, .. } = &runner.state().waiting_for else {
                panic!(
                    "modal Count-only X announcement: {:?}",
                    runner.state().waiting_for
                );
            };
            assert!(matches!(
                pending_cast.activation_residual,
                ActivationResidual::XMana
            ));
            assert_eq!(pending_cast.chosen_modes, vec![1]);
            runner.act(GameAction::ChooseX { value: x }).unwrap();
            if targeted {
                let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for
                else {
                    panic!(
                        "modal Count-only target declaration: {:?}",
                        runner.state().waiting_for
                    );
                };
                assert_eq!(pending_cast.ability.chosen_x, Some(x));
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(victim)],
                    })
                    .unwrap();
            }
            if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
                runner.act(GameAction::PassPriority).unwrap();
            }
            let WaitingFor::PayCost {
                player,
                choices,
                min_count,
                count,
                resume: CostResume::Spell { spell },
                ..
            } = &runner.state().waiting_for
            else {
                panic!("modal Count-only payment: {:?}", runner.state().waiting_for);
            };
            assert_eq!((*player, *min_count, *count), (P0, 1, 1));
            assert_eq!(choices, &vec![fodder]);
            assert_eq!(spell.object_id, source);
            assert_eq!(spell.activation_ability_index, Some(0));
            assert!(matches!(
                spell.activation_residual,
                ActivationResidual::XMana
            ));
            assert_eq!(spell.ability.chosen_x, Some(x));
            // CR 601.2b/c + CR 602.2b: The chosen mode and announced X are
            // bound before targets and remain on the selected ability.
            assert_eq!(spell.ability.selected_mode_labels, vec!["Chosen mode"]);
            runner
                .act(GameAction::SelectCards {
                    cards: vec![fodder],
                })
                .unwrap();
            assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
            assert_eq!(runner.state().stack.len(), 1);
            let ability = runner.state().stack.back().unwrap().ability().unwrap();
            assert_eq!(ability.chosen_x, Some(x));
            assert_eq!(ability.selected_mode_labels, vec!["Chosen mode"]);
            assert_eq!(
                ability.cost_paid_objects[0].snapshot().unwrap().object_id,
                fodder
            );
            if targeted {
                assert_eq!(ability.targets, vec![TargetRef::Object(victim)]);
            }
            runner.resolve_top();
            if targeted {
                assert_eq!(runner.state().objects[&victim].damage_marked, x);
                assert_eq!(runner.state().players[0].crimes_committed_this_turn, 1);
            } else {
                assert_eq!(
                    runner.state().players[0].hand.len(),
                    hand_before + x as usize
                );
            }
        }
    }
}
