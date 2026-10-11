//! Synthetic typed selected sacrifice cost regressions.

use engine::ai_support::legal_actions_full;
use engine::game::engine::apply;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, Effect,
    QuantityExpr, ReplacementDefinition, ReplacementMode, SacrificeCost, TargetFilter, TypedFilter,
};
use engine::types::actions::{GameAction, ResolutionOptionalPaymentChoice};
use engine::types::game_state::{
    ActivationResidual, CostResume, PersistedGameState, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::{CostPaymentProhibition, ProhibitionScope, StaticMode};
use engine::types::zones::{EtbTapState, Zone};

fn fixture(stolen: bool, replacement: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let one = AbilityCost::Sacrifice(SacrificeCost::count(
        TypedFilter::permanent()
            .controller(ControllerRef::You)
            .into(),
        1,
    ));
    let source = scenario
        .add_enchantment_from_oracle(if stolen { P1 } else { P0 }, "Selected source", "")
        .controlled_by(P0)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::generic(1),
                    },
                    one.clone(),
                    one,
                ],
            }),
        )
        .id();
    let other = scenario
        .add_enchantment_from_oracle(P0, "Other permanent", "")
        .id();
    if replacement {
        scenario
            .add_creature(P1, "Optional graveyard redirect", 1, 1)
            .with_replacement_definition(
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
                    )),
            );
    }
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(9999), false, vec![]); 4],
    );
    (scenario.build(), source, other)
}

fn restore(runner: GameRunner) -> GameRunner {
    let before = serde_json::to_value(runner.state()).unwrap();
    let encoded =
        serde_json::to_string(&PersistedGameState::capture(runner.state().clone())).unwrap();
    let decoded: PersistedGameState = serde_json::from_str(&encoded).unwrap();
    let state = decoded.into_game_state().expect("checked payment restore");
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
    GameRunner::from_state(state)
}

fn choices(runner: &GameRunner, source: ObjectId) -> Vec<ObjectId> {
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
            "real selected sacrifice prompt: {:?}",
            runner.state().waiting_for
        )
    };
    assert_eq!((*player, *min_count, *count), (P0, 1, 1));
    assert_eq!(
        (spell.object_id, spell.activation_ability_index),
        (source, Some(0))
    );
    assert_eq!(
        (spell.ability.source_id, spell.ability.controller),
        (source, P0)
    );
    // The public fixed-mana route currently surfaces None; direct ManaLeg is tested inline.
    assert!(matches!(
        spell.activation_residual,
        ActivationResidual::None
    ));
    choices.clone()
}

#[test]
fn public_selected_two_count_cross_product_pays_once() {
    // CR 109.5 + CR 113.8: The activator controls the activated ability after source exit.
    // CR 601.2h + CR 602.2b: All selected cost components are paid before stack entry.
    for stolen in [false, true] {
        for source_first in [false, true] {
            let (mut runner, source, other) = fixture(stolen, false);
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                })
                .unwrap();
            let first_choices = choices(&runner, source);
            assert_eq!(first_choices.len(), 2);
            assert!(first_choices.contains(&source) && first_choices.contains(&other));
            let (first, second) = if source_first {
                (source, other)
            } else {
                (other, source)
            };
            assert!(legal_actions_full(runner.state())
                .0
                .contains(&GameAction::SelectCards { cards: vec![first] }));
            runner
                .act(GameAction::SelectCards { cards: vec![first] })
                .unwrap();
            assert_eq!(choices(&runner, source), vec![second]);
            let WaitingFor::PayCost {
                resume: CostResume::Spell { spell },
                ..
            } = &runner.state().waiting_for
            else {
                unreachable!()
            };
            let paid = &spell.ability.cost_paid_objects;
            assert_eq!(paid.len(), 1);
            assert_eq!(
                (
                    paid[0].snapshot().unwrap().object_id,
                    paid[0].snapshot().unwrap().lki.controller
                ),
                (first, P0)
            );
            if first == source {
                assert_eq!(
                    runner.state().objects[&source].controller,
                    if stolen { P1 } else { P0 }
                );
            }
            assert_eq!(runner.state().players[0].mana_pool.total(), 3);
            assert!(legal_actions_full(runner.state())
                .0
                .contains(&GameAction::SelectCards {
                    cards: vec![second]
                }));
            runner
                .act(GameAction::SelectCards {
                    cards: vec![second],
                })
                .unwrap();
            assert_eq!(runner.state().stack.len(), 1);
            let entry = runner.state().stack.back().unwrap();
            assert_eq!((entry.source_id, entry.controller), (source, P0));
            let StackEntryKind::ActivatedAbility { ability, .. } = &entry.kind else {
                panic!("activated entry")
            };
            assert_eq!(
                (ability.source_id, ability.controller, ability.ability_index),
                (source, P0, Some(0))
            );
            assert_eq!(
                ability
                    .cost_paid_objects
                    .iter()
                    .map(|record| record.object_id())
                    .collect::<Vec<_>>(),
                vec![first, second]
            );
            assert!(ability.cost_paid_objects.iter().all(|record| record
                .snapshot()
                .unwrap()
                .lki
                .controller
                == P0));
            // CR 701.21a: Sacrificed permanents move to their owners' graveyards.
            assert!(runner.state().players[if stolen { 1 } else { 0 }]
                .graveyard
                .contains(&source));
            assert!(runner.state().players[0].graveyard.contains(&other));
            runner.resolve_top();
            assert!(runner.state().stack.is_empty());
            assert_eq!(
                (
                    runner.state().players[0].life,
                    runner.state().players[1].life
                ),
                (21, 20)
            );
            assert_eq!(runner.state().players[0].mana_pool.total(), 3);
        }
    }
}

#[test]
fn public_two_count_checked_restore_at_each_prompt() {
    for restore_first in [false, true] {
        let (mut runner, source, other) = fixture(true, false);
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .unwrap();
        if restore_first {
            runner = restore(runner);
        }
        assert!(choices(&runner, source).contains(&source));
        runner
            .act(GameAction::SelectCards {
                cards: vec![source],
            })
            .unwrap();
        assert_eq!(choices(&runner, source), vec![other]);
        runner = restore(runner);
        runner
            .act(GameAction::SelectCards { cards: vec![other] })
            .unwrap();
        assert_eq!(runner.state().stack.len(), 1);
        runner.resolve_top();
        assert_eq!(runner.state().players[0].life, 21);
        assert!(runner.state().stack.is_empty());
    }
}

#[test]
fn public_replacement_pause_and_checked_restore_keep_selected_actor() {
    for restore_pause in [false, true] {
        let (mut runner, source, other) = fixture(true, true);
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .unwrap();
        runner
            .act(GameAction::SelectCards {
                cards: vec![source],
            })
            .unwrap();
        let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
            panic!("real replacement pause")
        };
        let accept = candidates
            .iter()
            .position(|candidate| candidate.description == "Accept")
            .unwrap();
        assert!(runner.state().pending_cost_move_resume.is_some());
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        if restore_pause {
            runner = restore(runner);
        }
        runner
            .act(GameAction::ChooseReplacement { index: accept })
            .unwrap();
        assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
        assert_eq!(runner.state().objects[&source].controller, P1);
        assert!(runner.state().pending_cost_move_resume.is_none());
        assert_eq!(choices(&runner, source), vec![other]);
        runner
            .act(GameAction::SelectCards { cards: vec![other] })
            .unwrap();
        let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
            panic!("second replacement pause")
        };
        let decline = candidates
            .iter()
            .position(|candidate| candidate.description == "Decline")
            .unwrap();
        runner
            .act(GameAction::ChooseReplacement { index: decline })
            .unwrap();
        assert_eq!(runner.state().stack.len(), 1);
        runner.resolve_top();
        assert_eq!(runner.state().players[0].life, 21);
        assert_eq!(runner.state().objects[&other].zone, Zone::Graveyard);
        assert!(runner.state().stack.is_empty());
    }
}

#[test]
fn public_invalid_responses_preserve_full_state_then_valid_twin_pays() {
    let (mut runner, source, other) = fixture(true, false);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .unwrap();
    assert_eq!(choices(&runner, source).len(), 2);
    for (actor, cards) in [
        (P1, vec![source]),
        (P0, vec![]),
        (P0, vec![source, other]),
        (P0, vec![source, source]),
        (P0, vec![ObjectId(999999)]),
    ] {
        let before = serde_json::to_value(runner.state()).unwrap();
        assert!(apply(runner.state_mut(), actor, GameAction::SelectCards { cards }).is_err());
        assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
    }
    runner
        .state_mut()
        .objects
        .get_mut(&other)
        .unwrap()
        .controller = P1;
    runner
        .state_mut()
        .objects
        .get_mut(&other)
        .unwrap()
        .base_controller = Some(P1);
    let before = serde_json::to_value(runner.state()).unwrap();
    assert!(runner
        .act(GameAction::SelectCards { cards: vec![other] })
        .is_err());
    assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
    runner
        .state_mut()
        .objects
        .get_mut(&other)
        .unwrap()
        .controller = P0;
    runner
        .state_mut()
        .objects
        .get_mut(&other)
        .unwrap()
        .base_controller = Some(P0);
    runner
        .act(GameAction::SelectCards {
            cards: vec![source],
        })
        .unwrap();
    assert_eq!(choices(&runner, source), vec![other]);
    runner
        .act(GameAction::SelectCards { cards: vec![other] })
        .unwrap();
    runner.resolve_top();
    assert_eq!(runner.state().players[0].life, 21);
}

#[test]
fn public_optional_normalized_cost_uses_payer_without_rebinding_payoff() {
    // CR 118.12a: The instructed player pays the optional cost.
    // CR 109.5: This cost context does not rebind the spell's payoff controller.
    for stolen_fodder in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mut payment = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PayCost {
                cost: AbilityCost::OneOf {
                    costs: vec![AbilityCost::Sacrifice(SacrificeCost::count(
                        TypedFilter::permanent()
                            .controller(ControllerRef::You)
                            .into(),
                        1,
                    ))],
                },
                scale: None,
                payer: TargetFilter::Opponent,
            },
        );
        payment.optional = true;
        let mut payoff = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
        );
        payoff.condition = Some(AbilityCondition::effect_performed());
        payment.sub_ability = Some(Box::new(payoff));
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic third-party payment", false)
            .with_mana_cost(ManaCost::zero())
            .with_ability_definition(payment)
            .id();
        let fodder = scenario
            .add_creature(if stolen_fodder { P0 } else { P1 }, "Payer fodder", 1, 1)
            .controlled_by(P1)
            .id();
        let own = scenario
            .add_creature(P0, "Source-controller fodder", 1, 1)
            .id();
        let mut runner = scenario.build();
        runner.cast(spell).commit();
        runner.resolve_top();
        if let WaitingFor::OptionalEffectChoice { player, .. } = runner.state().waiting_for {
            apply(
                runner.state_mut(),
                player,
                GameAction::DecideOptionalEffect { accept: true },
            )
            .unwrap();
        }
        let WaitingFor::ResolutionOptionalPaymentChoice { player, .. } =
            &runner.state().waiting_for
        else {
            panic!("real third-party payment menu")
        };
        assert_eq!(*player, P1);
        assert!(runner.state().resolving_stack_entry.is_some());
        apply(
            runner.state_mut(),
            P1,
            GameAction::ChooseResolutionOptionalPaymentBranch {
                choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
            },
        )
        .unwrap();
        let WaitingFor::PayCost {
            player,
            choices,
            min_count,
            count,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("payer sacrifice")
        };
        assert_eq!((*player, *min_count, *count), (P1, 1, 1));
        assert!(choices.contains(&fodder));
        assert!(!choices.contains(&own));
        let before = serde_json::to_value(runner.state()).unwrap();
        assert!(apply(
            runner.state_mut(),
            P0,
            GameAction::SelectCards {
                cards: vec![fodder]
            }
        )
        .is_err());
        assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
        assert!(legal_actions_full(runner.state())
            .0
            .contains(&GameAction::SelectCards {
                cards: vec![fodder]
            }));
        apply(
            runner.state_mut(),
            P1,
            GameAction::SelectCards {
                cards: vec![fodder],
            },
        )
        .unwrap();
        assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert_eq!(
            (
                runner.state().players[0].life,
                runner.state().players[1].life
            ),
            (23, 20)
        );
        assert!(runner.state().resolving_stack_entry.is_none());
    }
}

#[test]
fn public_zero_and_positive_count_bounds_preserve_selected_payments() {
    // CR 601.2h: A selected zero cost is complete without moving a permanent.
    for count in [0, 1, 2] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_enchantment_from_oracle(P1, "Bounded source", "")
            .controlled_by(P0)
            .with_ability_definition(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                )
                .cost(AbilityCost::Sacrifice(SacrificeCost::count(
                    TypedFilter::permanent()
                        .controller(ControllerRef::You)
                        .into(),
                    count,
                ))),
            )
            .id();
        let other = scenario.add_creature(P0, "Bounded fodder", 1, 1).id();
        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .unwrap();
        let WaitingFor::PayCost {
            choices,
            min_count,
            count: upper,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("real bounded Count prompt")
        };
        assert_eq!((*min_count, *upper), (count as usize, count as usize));
        assert!(choices.contains(&source) && choices.contains(&other));
        if count > 0 {
            let before = serde_json::to_value(runner.state()).unwrap();
            assert!(runner
                .act(GameAction::SelectCards { cards: vec![] })
                .is_err());
            assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
        }
        let selected = match count {
            0 => vec![],
            1 => vec![source],
            2 => vec![source, other],
            _ => unreachable!(),
        };
        assert!(legal_actions_full(runner.state())
            .0
            .contains(&GameAction::SelectCards {
                cards: selected.clone()
            }));
        runner
            .act(GameAction::SelectCards { cards: selected })
            .unwrap();
        assert_eq!(runner.state().stack.len(), 1);
        runner.resolve_top();
        assert_eq!(runner.state().players[0].life, 21);
        assert_eq!(
            runner.state().objects[&other].zone,
            if count == 2 {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            }
        );
    }
}

#[test]
fn x_mana_sacrifice_keeps_payer_after_source_exits() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let one = AbilityCost::Sacrifice(SacrificeCost::count(
        TypedFilter::permanent()
            .controller(ControllerRef::You)
            .into(),
        1,
    ));
    let source = scenario
        .add_enchantment_from_oracle(P1, "X source", "")
        .controlled_by(P0)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            shards: vec![engine::types::mana::ManaCostShard::X],
                            generic: 0,
                        },
                    },
                    one.clone(),
                    one,
                ],
            }),
        )
        .id();
    let other = scenario
        .add_enchantment_from_oracle(P0, "Other fodder", "")
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(9999), false, vec![]); 4],
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
    let root = runner.state().pending_cast.as_ref().unwrap();
    assert!(matches!(
        root.activation_residual,
        ActivationResidual::XMana
    ));
    assert!(root.activation_cost.is_some());
    assert_eq!(runner.state().objects[&other].zone, Zone::Battlefield);
    runner.act(GameAction::ChooseX { value: 1 }).unwrap();
    assert_eq!(runner.state().players[0].mana_pool.total(), 3);
    let WaitingFor::PayCost {
        player,
        choices,
        min_count,
        count,
        resume: CostResume::Spell { spell },
        ..
    } = &runner.state().waiting_for
    else {
        panic!("first X sacrifice prompt: {:?}", runner.state().waiting_for);
    };
    assert_eq!((*player, *min_count, *count), (P0, 1, 1));
    assert!(choices.contains(&source) && choices.contains(&other));
    assert!(matches!(
        spell.activation_residual,
        ActivationResidual::XMana
    ));
    assert_eq!(spell.ability.chosen_x, Some(1));
    assert_eq!(
        (spell.object_id, spell.activation_ability_index),
        (source, Some(0))
    );
    assert_eq!(
        (spell.ability.source_id, spell.ability.controller),
        (source, P0)
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![source],
        })
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
            "second X sacrifice prompt: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((*player, *min_count, *count), (P0, 1, 1));
    assert_eq!(choices, &vec![other]);
    assert!(matches!(
        spell.activation_residual,
        ActivationResidual::XMana
    ));
    assert_eq!(spell.ability.chosen_x, Some(1));
    assert_eq!(
        (spell.object_id, spell.activation_ability_index),
        (source, Some(0))
    );
    assert_eq!(
        (spell.ability.source_id, spell.ability.controller),
        (source, P0)
    );
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&source].owner, P1);
    assert!(runner.state().players[1].graveyard.contains(&source));
    runner
        .act(GameAction::SelectCards { cards: vec![other] })
        .unwrap();
    let entry = runner.state().stack.back().unwrap();
    assert_eq!((entry.source_id, entry.controller), (source, P0));
    let ability = entry.ability().unwrap();
    assert_eq!(
        (ability.source_id, ability.controller, ability.ability_index),
        (source, P0, Some(0))
    );
    assert_eq!(ability.chosen_x, Some(1));
    let paid: Vec<_> = ability
        .cost_paid_objects
        .iter()
        .map(|record| record.snapshot().unwrap().object_id)
        .collect();
    assert_eq!(paid, vec![source, other]);
    assert!(runner.state().players[0].graveyard.contains(&other));
    assert_eq!(runner.state().players[0].mana_pool.total(), 3);
    runner.resolve_top();
    assert_eq!(runner.state().players[0].life, 21);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
#[test]
fn distinct_sources_and_ability_indices_keep_separate_selected_payments() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let one = AbilityCost::Sacrifice(SacrificeCost::count(
        TypedFilter::creature()
            .controller(ControllerRef::You)
            .into(),
        1,
    ));
    let mut sources = vec![];
    for owner in [P1, P0] {
        sources.push(
            scenario
                .add_enchantment_from_oracle(owner, "Independent source", "")
                .controlled_by(P0)
                .with_ability_definition(AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 99 },
                        player: TargetFilter::Controller,
                    },
                ))
                .with_ability_definition(
                    AbilityDefinition::new(
                        AbilityKind::Activated,
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                            player: TargetFilter::Controller,
                        },
                    )
                    .cost(AbilityCost::Composite {
                        costs: vec![one.clone(), one.clone()],
                    }),
                )
                .id(),
        );
    }
    let fodder: Vec<_> = (0..4)
        .map(|_| scenario.add_creature(P0, "Separate fodder", 1, 1).id())
        .collect();
    let mut runner = scenario.build();
    for (source, pair) in sources.iter().zip(fodder.chunks_exact(2)) {
        runner
            .act(GameAction::ActivateAbility {
                source_id: *source,
                ability_index: 1,
            })
            .unwrap();
        for id in pair {
            let WaitingFor::PayCost {
                choices,
                resume: CostResume::Spell { spell },
                ..
            } = &runner.state().waiting_for
            else {
                panic!("selected source and index prompt")
            };
            assert_eq!(
                (
                    spell.object_id,
                    spell.activation_ability_index,
                    spell.ability.controller
                ),
                (*source, Some(1), P0)
            );
            assert!(choices.contains(id));
            runner
                .act(GameAction::SelectCards { cards: vec![*id] })
                .unwrap();
        }
        assert_eq!(runner.state().objects[source].zone, Zone::Battlefield);
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().stack[0].source_id, *source);
        runner.resolve_top();
    }
    assert_eq!(runner.state().players[0].life, 22);
    assert!(runner.state().stack.is_empty());
}

#[test]
fn public_cost_prohibition_excludes_only_forbidden_permanents() {
    // CR 118.3 + CR 701.21a: A payer must select a permanent they can sacrifice.
    for nonland_prohibition in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_enchantment_from_oracle(P1, "Prohibited cost source", "")
            .controlled_by(P0)
            .with_ability_definition(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                )
                .cost(AbilityCost::Sacrifice(SacrificeCost::count(
                    TypedFilter::permanent()
                        .controller(ControllerRef::You)
                        .into(),
                    1,
                ))),
            )
            .id();
        let creature = scenario.add_creature(P0, "Forbidden creature", 1, 1).id();
        let land = scenario.add_land_from_oracle(P0, "Legal land", "").id();
        scenario
            .add_enchantment_from_oracle(P1, "Cost prohibition", "")
            .with_static(StaticMode::CantPayCost {
                who: ProhibitionScope::AllPlayers,
                cost: CostPaymentProhibition::Sacrifice {
                    filter: if nonland_prohibition {
                        TargetFilter::Not {
                            filter: Box::new(TypedFilter::land().into()),
                        }
                    } else {
                        TypedFilter::creature().into()
                    },
                },
            });
        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .unwrap();
        let WaitingFor::PayCost {
            player,
            choices,
            min_count,
            count,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("real prohibited sacrifice selection")
        };
        assert_eq!((*player, *min_count, *count), (P0, 1, 1));
        assert!(!choices.contains(&creature));
        assert!(choices.contains(&land));
        assert_eq!(choices.contains(&source), !nonland_prohibition);
        let before = serde_json::to_value(runner.state()).unwrap();
        assert!(runner
            .act(GameAction::SelectCards {
                cards: vec![creature]
            })
            .is_err());
        assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
        assert!(legal_actions_full(runner.state())
            .0
            .contains(&GameAction::SelectCards { cards: vec![land] }));
        runner
            .act(GameAction::SelectCards { cards: vec![land] })
            .unwrap();
        assert_eq!(runner.state().objects[&land].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&creature].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        assert_eq!(runner.state().stack.len(), 1);
        runner.resolve_top();
        assert_eq!(runner.state().players[0].life, 21);
    }
}
