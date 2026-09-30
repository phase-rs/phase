//! Initial acceptance of ordinary selected sacrifice costs.
//! Payment abilities are synthetic typed fixtures; Control Magic uses its pinned Oracle text.

use engine::ai_support::legal_actions_full;
use engine::game::casting::find_eligible_sacrifice_targets;
use engine::game::effects::attach::attach_to;
use engine::game::engine::{apply, EngineError};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost, ControllerRef, Effect,
    QuantityExpr, ReplacementDefinition, ReplacementMode, SacrificeCost, TargetFilter, TypeFilter,
    TypedFilter,
};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::events::GameEvent;
use engine::types::game_state::{
    ActivationResidual, CastPaymentMode, CastingVariant, CostResume, PersistedGameState,
    SpellCostSource, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{EmergeCost, Keyword};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

fn sacrifice(filter: TargetFilter, count: u32) -> AbilityCost {
    AbilityCost::Sacrifice(SacrificeCost::count(filter, count))
}

fn payoff(cost: AbilityCost) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )
    .cost(cost)
}

fn restore(runner: GameRunner) -> GameRunner {
    let waiting = serde_json::to_value(&runner.state().waiting_for).unwrap();
    let cursor = serde_json::to_value(&runner.state().pending_cost_move_resume).unwrap();
    let saved = serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
    let restored = GameRunner::from_state(
        serde_json::from_value::<PersistedGameState>(saved)
            .unwrap()
            .into_game_state()
            .unwrap(),
    );
    assert_eq!(
        serde_json::to_value(&restored.state().waiting_for).unwrap(),
        waiting
    );
    assert_eq!(
        serde_json::to_value(&restored.state().pending_cost_move_resume).unwrap(),
        cursor
    );
    restored
}

fn prompt(runner: &GameRunner, bounds: (usize, usize)) -> Vec<ObjectId> {
    let WaitingFor::PayCost {
        player,
        kind,
        choices,
        min_count,
        count,
        resume,
    } = &runner.state().waiting_for
    else {
        panic!("actual sacrifice prompt: {:?}", runner.state().waiting_for);
    };
    assert_eq!(*player, P0);
    assert!(matches!(
        kind,
        engine::types::game_state::PayCostKind::Sacrifice
    ));
    assert_eq!((*min_count, *count), bounds);
    assert!(matches!(
        resume,
        CostResume::Spell { .. } | CostResume::SpellCost { .. }
    ));
    choices.clone()
}

fn refuse_unchanged(runner: &mut GameRunner, player: PlayerId, cards: Vec<ObjectId>) {
    let before = serde_json::to_value(runner.state()).unwrap();
    assert!(apply(
        runner.state_mut(),
        player,
        GameAction::SelectCards { cards }
    )
    .is_err());
    assert_eq!(
        serde_json::to_value(runner.state()).unwrap(),
        before,
        "full action rollback"
    );
}

fn finish_payoff(runner: &mut GameRunner, source: ObjectId, paid: &[ObjectId]) {
    assert_eq!(runner.state().stack.len(), 1);
    let entry = runner.state().stack.back().unwrap();
    assert_eq!(entry.source_id, source);
    assert_eq!(entry.controller, P0);
    let ability = entry.ability().expect("payment payoff ability");
    assert_eq!(ability.source_id, source);
    assert_eq!(ability.controller, P0);
    assert_eq!(
        ability.ability_index,
        if matches!(entry.kind, StackEntryKind::ActivatedAbility { .. }) {
            Some(0)
        } else {
            None
        }
    );
    assert_eq!(ability.cost_paid_objects.len(), paid.len());
    for id in paid {
        assert_eq!(runner.state().objects[id].zone, Zone::Graveyard);
    }
    runner.resolve_top();
    assert_eq!(runner.state().players[0].life, 21);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn public_concession_refuses_stale_sacrifice_before_any_payment() {
    for concede in [false, true] {
        for saved in [false, true] {
            let p2 = PlayerId(2);
            let mut scenario = GameScenario::new_n_player(3, 42);
            scenario.at_phase(Phase::PreCombatMain);
            let filter: TargetFilter = TypedFilter::creature()
                .controller(ControllerRef::You)
                .into();
            let source = scenario
                .add_enchantment_from_oracle(P0, "Typed sacrifice source", "")
                .with_ability_definition(payoff(sacrifice(filter.clone(), 1)))
                .id();
            let fodder = scenario.add_creature(p2, "Third-seat creature", 2, 2).id();
            let aura = scenario
                .add_enchantment_from_oracle(
                    P1,
                    "Control Magic",
                    "Enchant creature\nYou control enchanted creature.",
                )
                .controlled_by(P0)
                .with_subtypes(vec!["Aura"])
                .with_keyword(Keyword::Enchant(TypedFilter::creature().into()))
                .id();
            let mut runner = scenario.build();
            attach_to(runner.state_mut(), aura, fodder);
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                })
                .unwrap();
            assert!(prompt(&runner, (1, 1)).contains(&fodder));
            assert_eq!(runner.state().objects[&fodder].controller, P0);
            assert!(!prompt(&runner, (1, 1)).contains(&source));
            let before = serde_json::to_value(runner.state()).unwrap();
            assert!(matches!(
                apply(runner.state_mut(), P0, GameAction::SelectCards { cards: vec![source] }),
                Err(EngineError::InvalidAction(ref text))
                    if text == "Selected permanent not eligible for sacrifice"
            ));
            assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
            if concede {
                // CR 104.3a + CR 800.4a: Concession removes the owned Aura during the payment prompt.
                apply(
                    runner.state_mut(),
                    P1,
                    GameAction::Concede { player_id: P1 },
                )
                .unwrap();
                assert_ne!(runner.state().objects[&aura].zone, Zone::Battlefield);
                assert_eq!(runner.state().objects[&fodder].controller, p2);
                assert!(prompt(&runner, (1, 1)).contains(&fodder));
                assert!(!find_eligible_sacrifice_targets(
                    runner.state(),
                    P0,
                    source,
                    None,
                    &filter
                )
                .contains(&fodder));
            } else {
                assert!(
                    find_eligible_sacrifice_targets(runner.state(), P0, source, None, &filter)
                        .contains(&fodder)
                );
            }
            if saved {
                runner = restore(runner);
            }
            if concede {
                let before = serde_json::to_value(runner.state()).unwrap();
                let result = apply(
                    runner.state_mut(),
                    P0,
                    GameAction::SelectCards {
                        cards: vec![fodder],
                    },
                );
                assert!(
                    matches!(result, Err(EngineError::ActionNotAllowed(ref text)) if text == "Selected permanent no longer eligible for sacrifice")
                );
                assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
            } else {
                assert!(legal_actions_full(runner.state()).0.iter().any(|action| matches!(action, GameAction::SelectCards { cards } if cards == &vec![fodder])));
                let result = apply(
                    runner.state_mut(),
                    P0,
                    GameAction::SelectCards {
                        cards: vec![fodder],
                    },
                )
                .unwrap();
                // CR 701.21a: The payer sacrifices its controlled permanent into the owner's graveyard.
                assert!(runner.state().players[2].graveyard.contains(&fodder));
                assert!(result.events.iter().any(|event| matches!(event, GameEvent::PermanentSacrificed { object_id, player_id } if *object_id == fodder && *player_id == P0)));
                assert!(result.events.iter().any(|event| matches!(event, GameEvent::ZoneChanged { object_id, record, .. } if *object_id == fodder && record.controller == P0)));
                finish_payoff(&mut runner, source, &[fodder]);
            }
        }
    }
}

#[test]
fn unselected_advertised_control_loss_keeps_lawful_full_selection() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let filter: TargetFilter = TypedFilter::creature().into();
    let source = scenario
        .add_enchantment_from_oracle(P0, "Source", "")
        .with_ability_definition(payoff(sacrifice(filter.clone(), 1)))
        .id();
    let retained = scenario.add_creature(P0, "Retained", 2, 2).id();
    let lost = scenario.add_creature(p2, "Lost", 2, 2).id();
    let aura = scenario
        .add_enchantment_from_oracle(
            P1,
            "Control Magic",
            "Enchant creature\nYou control enchanted creature.",
        )
        .controlled_by(P0)
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(filter.clone()))
        .id();
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, lost);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .unwrap();
    let advertised = prompt(&runner, (1, 1));
    assert!(advertised.contains(&retained) && advertised.contains(&lost));
    apply(
        runner.state_mut(),
        P1,
        GameAction::Concede { player_id: P1 },
    )
    .unwrap();
    assert_eq!(prompt(&runner, (1, 1)), advertised);
    let live = find_eligible_sacrifice_targets(runner.state(), P0, source, None, &filter);
    assert!(live.contains(&retained) && !live.contains(&lost));
    let before = serde_json::to_value(runner.state()).unwrap();
    assert!(matches!(
        apply(runner.state_mut(), P0, GameAction::SelectCards { cards: vec![lost] }),
        Err(EngineError::ActionNotAllowed(ref text))
            if text == "Selected permanent no longer eligible for sacrifice"
    ));
    assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
    runner
        .act(GameAction::SelectCards {
            cards: vec![retained],
        })
        .unwrap();
    finish_payoff(&mut runner, source, &[retained]);
    assert_eq!(runner.state().objects[&lost].zone, Zone::Battlefield);
}

#[test]
fn selected_sacrifice_preserves_bound_zero_fixed_and_variable_counts() {
    for count in [0, 1, 2, u32::MAX] {
        for selected in 0..=2usize {
            if count != u32::MAX && selected != count as usize {
                continue;
            }
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let source = scenario
                .add_enchantment_from_oracle(P0, "Source", "")
                .with_ability_definition(payoff(sacrifice(TypedFilter::creature().into(), count)))
                .id();
            let a = scenario.add_creature(P0, "A", 2, 2).id();
            let b = scenario.add_creature(P0, "B", 2, 2).id();
            let mut runner = scenario.build();
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                })
                .unwrap();
            let bounds = if count == u32::MAX {
                (0, 2)
            } else {
                (count as usize, count as usize)
            };
            assert_eq!(prompt(&runner, bounds), vec![a, b]);
            if count > 0 && count != u32::MAX {
                refuse_unchanged(&mut runner, P0, vec![]);
                if count == 2 {
                    refuse_unchanged(&mut runner, P0, vec![a]);
                    refuse_unchanged(&mut runner, P0, vec![a, a]);
                } else {
                    refuse_unchanged(&mut runner, P0, vec![a, b]);
                }
            }
            refuse_unchanged(&mut runner, P1, vec![a, b][..selected].to_vec());
            if selected > 0 {
                refuse_unchanged(&mut runner, P0, vec![source; selected]);
            }
            let chosen = vec![a, b][..selected].to_vec();
            assert!(legal_actions_full(runner.state()).0.iter().any(
                |action| matches!(action, GameAction::SelectCards { cards } if cards == &chosen)
            ));
            runner = restore(runner);
            runner
                .act(GameAction::SelectCards {
                    cards: chosen.clone(),
                })
                .unwrap();
            if count == u32::MAX {
                assert_eq!(
                    runner
                        .state()
                        .stack
                        .back()
                        .unwrap()
                        .ability()
                        .unwrap()
                        .chosen_x,
                    Some(selected as u32)
                );
            }
            finish_payoff(&mut runner, source, &chosen);
        }
    }
    // CR 118.3: Insufficient fixed resources are rejected before a prompt exists.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Insufficient", "")
        .with_ability_definition(payoff(sacrifice(TypedFilter::creature().into(), 2)))
        .id();
    scenario.add_creature(P0, "Only one", 2, 2);
    let mut runner = scenario.build();
    let before = serde_json::to_value(runner.state()).unwrap();
    assert!(runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0
        })
        .is_err());
    assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
}

#[test]
fn current_sacrifice_leaf_and_selected_oneof_pay_in_order() {
    for reverse in [false, true] {
        for nested in [false, true] {
            for oneof in [false, true] {
                let creature_cost = sacrifice(TypedFilter::creature().into(), 1);
                let artifact_cost = sacrifice(TypedFilter::new(TypeFilter::Artifact).into(), 1);
                let (first, second) = if reverse {
                    (artifact_cost.clone(), creature_cost.clone())
                } else {
                    (creature_cost.clone(), artifact_cost.clone())
                };
                let selected = AbilityCost::Composite {
                    costs: vec![
                        if nested {
                            AbilityCost::Composite {
                                costs: vec![first.clone()],
                            }
                        } else {
                            first.clone()
                        },
                        second.clone(),
                    ],
                };
                let cost = if oneof {
                    AbilityCost::OneOf {
                        costs: vec![selected, sacrifice(TypedFilter::permanent().into(), 1)],
                    }
                } else {
                    selected
                };
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let source = scenario
                    .add_enchantment_from_oracle(P0, "Source", "")
                    .with_ability_definition(payoff(cost))
                    .id();
                let creature = scenario.add_creature(P0, "Creature", 2, 2).id();
                let artifact = scenario.add_artifact_from_oracle(P0, "Artifact", "").id();
                let mut runner = scenario.build();
                runner
                    .act(GameAction::ActivateAbility {
                        source_id: source,
                        ability_index: 0,
                    })
                    .unwrap();
                if oneof {
                    runner
                        .act(GameAction::ChooseActivationCostBranch { index: 0 })
                        .unwrap();
                }
                let (first_id, second_id) = if reverse {
                    (artifact, creature)
                } else {
                    (creature, artifact)
                };
                assert_eq!(prompt(&runner, (1, 1)), vec![first_id]);
                refuse_unchanged(&mut runner, P0, vec![second_id]);
                runner = restore(runner);
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![first_id],
                    })
                    .unwrap();
                assert_eq!(prompt(&runner, (1, 1)), vec![second_id]);
                runner = restore(runner);
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![second_id],
                    })
                    .unwrap();
                finish_payoff(&mut runner, source, &[first_id, second_id]);
            }
        }
    }
}

#[test]
fn real_spell_additional_cost_and_keyword_costs_pay_exact_reductions() {
    for kind in ["other", "offering", "emerge", "emerge_artifact"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        for _ in 0..5 {
            scenario.add_basic_land(P0, ManaColor::White);
        }
        let fodder = scenario
            .add_creature(P0, "Fodder", 2, 2)
            .as_artifact()
            .as_creature()
            .with_subtypes(vec!["Spirit"])
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Green],
            })
            .id();
        let mut builder = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Synthetic typed spell",
            false,
            "You gain 1 life.",
        );
        builder.with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::White],
        });
        match kind {
            "other" => {
                builder.with_additional_cost(AdditionalCost::Required(sacrifice(
                    TypedFilter::creature().subtype("Spirit".into()).into(),
                    1,
                )));
            }
            "offering" => {
                builder.with_keyword(Keyword::Offering("Spirit".into()));
            }
            "emerge" => {
                builder.with_keyword(Keyword::Emerge(EmergeCost::creature(ManaCost::Cost {
                    generic: 3,
                    shards: vec![ManaCostShard::White],
                })));
            }
            "emerge_artifact" => {
                builder.with_keyword(Keyword::Emerge(EmergeCost::from_quality(
                    ManaCost::Cost {
                        generic: 3,
                        shards: vec![ManaCostShard::White],
                    },
                    TypedFilter::new(TypeFilter::Artifact).into(),
                )));
            }
            _ => unreachable!(),
        }
        let spell = builder.id();
        let mut runner = scenario.build();
        assert!(runner.state().objects[&fodder]
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Creature));
        assert!(runner.state().objects[&fodder]
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Artifact));
        assert!(find_eligible_sacrifice_targets(
            runner.state(),
            P0,
            spell,
            None,
            &TypedFilter::creature().subtype("Spirit".into()).into()
        )
        .contains(&fodder));
        let lands: Vec<_> = runner
            .state()
            .battlefield
            .iter()
            .copied()
            .filter(|id| {
                runner.state().objects[id]
                    .card_types
                    .core_types
                    .contains(&engine::types::card_type::CoreType::Land)
            })
            .collect();
        // CR 702.48c + CR 702.119a: The selected keyword cost determines the reduction.
        let outcome = if kind.starts_with("emerge") {
            runner
                .cast(spell)
                .casting_variant(CastingVariant::Emerge)
                .alternative_cast(AlternativeCastDecision::Alternative)
                .sacrifice_with(&[fodder])
                .resolve()
        } else {
            runner
                .cast(spell)
                .accept_optional()
                .sacrifice_with(&[fodder])
                .resolve()
        };
        outcome.assert_life_delta(P0, 1);
        outcome.assert_zone(&[fodder, spell], Zone::Graveyard);
        assert_eq!(outcome.state().players[0].mana_pool.total(), 0);
        let tapped = lands
            .iter()
            .filter(|id| outcome.state().objects[id].tapped)
            .count();
        assert_eq!(tapped, if kind == "other" { 4 } else { 2 }, "{kind}");
        assert!(outcome.state().stack.is_empty());
    }
}

// CR 118.3a + CR 601.2h + CR 602.2b: Mana and each selected sacrifice pay once.
#[test]
fn normal_and_replacement_sacrifice_settle_once_and_survive_checked_restore() {
    for replacement in [false, true] {
        for saved in [false, true] {
            for accept in [false, true] {
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let source = scenario
                    .add_enchantment_from_oracle(P0, "Source", "")
                    .with_ability_definition(payoff(AbilityCost::Composite {
                        costs: vec![
                            AbilityCost::Mana {
                                cost: ManaCost::generic(1),
                            },
                            sacrifice(TypedFilter::creature().into(), 2),
                        ],
                    }))
                    .id();
                let a = scenario.add_creature(P0, "A", 2, 2).id();
                let b = scenario.add_creature(P0, "B", 2, 2).id();
                if replacement {
                    scenario
                        .add_enchantment_from_oracle(P0, "Optional graveyard redirect", "")
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
                let mut runner = scenario.build();
                let mut events = runner
                    .act(GameAction::ActivateAbility {
                        source_id: source,
                        ability_index: 0,
                    })
                    .unwrap()
                    .events;
                assert_eq!(prompt(&runner, (2, 2)), vec![a, b]);
                refuse_unchanged(&mut runner, P0, vec![a, a]);
                if saved {
                    runner = restore(runner);
                }
                events.extend(
                    runner
                        .act(GameAction::SelectCards { cards: vec![a, b] })
                        .unwrap()
                        .events,
                );
                if replacement {
                    for _ in 0..2 {
                        let WaitingFor::ReplacementChoice { candidates, .. } =
                            &runner.state().waiting_for
                        else {
                            panic!("real replacement pause");
                        };
                        let index = candidates
                            .iter()
                            .position(|c| {
                                c.description == if accept { "Accept" } else { "Decline" }
                            })
                            .unwrap();
                        assert!(legal_actions_full(runner.state()).0.iter().any(|action| matches!(action, GameAction::ChooseReplacement { index: offered } if *offered == index)));
                        if saved {
                            runner = restore(runner);
                        }
                        events.extend(
                            runner
                                .act(GameAction::ChooseReplacement { index })
                                .unwrap()
                                .events,
                        );
                    }
                }
                assert_eq!(runner.state().stack.len(), 1);
                let ability = runner.state().stack.back().unwrap().ability().unwrap();
                assert_eq!(ability.source_id, source);
                assert_eq!(ability.controller, P0);
                assert_eq!(ability.ability_index, Some(0));
                assert_eq!(
                    ability
                        .cost_paid_objects
                        .iter()
                        .map(|record| record.object_id())
                        .collect::<Vec<_>>(),
                    vec![a, b]
                );
                for record in &ability.cost_paid_objects {
                    let engine::types::ability::CostPaidObjectRecord::Captured(snapshot) = record
                    else {
                        panic!("actual sacrifice snapshot");
                    };
                    assert_eq!(snapshot.lki.controller, P0);
                }
                assert_eq!(runner.state().players[0].mana_pool.total(), 3);
                for id in [a, b] {
                    assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::PermanentSacrificed { object_id, player_id } if *object_id == id && *player_id == P0)).count(), 1);
                    assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to, record } if *object_id == id && *to == if replacement && accept { Zone::Exile } else { Zone::Graveyard } && record.controller == P0)).count(), 1);
                }
                assert!(
                    matches!(runner.state().stack.back().unwrap().kind, StackEntryKind::ActivatedAbility { source_id, .. } if source_id == source)
                );
                assert_eq!(
                    runner.state().objects[&a].zone,
                    if replacement && accept {
                        Zone::Exile
                    } else {
                        Zone::Graveyard
                    }
                );
                assert_eq!(
                    runner.state().objects[&b].zone,
                    if replacement && accept {
                        Zone::Exile
                    } else {
                        Zone::Graveyard
                    }
                );
                runner.resolve_top();
                assert_eq!(runner.state().players[0].life, 21);
                assert!(runner.state().stack.is_empty());
                assert_eq!(runner.state().players[0].mana_pool.total(), 3);
            }
        }
    }
}

// CR 601.2h + CR 602.2b: Fixed mana and two distinct sacrifice components pay once.
#[test]
fn fixed_mana_source_loss_identity_positive_controls() {
    for (stolen, source_first) in [(false, false), (false, true), (true, false)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let component = sacrifice(
            TypedFilter::permanent()
                .controller(ControllerRef::You)
                .into(),
            1,
        );
        let source = scenario
            .add_enchantment_from_oracle(if stolen { P1 } else { P0 }, "Source", "")
            .controlled_by(P0)
            .with_ability_definition(payoff(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::generic(1),
                    },
                    component.clone(),
                    component,
                ],
            }))
            .id();
        let other = scenario.add_enchantment_from_oracle(P0, "Other", "").id();
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
        let advertised = prompt(&runner, (1, 1));
        assert!(advertised.contains(&source) && advertised.contains(&other));
        let (first, second) = if source_first {
            (source, other)
        } else {
            (other, source)
        };
        runner
            .act(GameAction::SelectCards { cards: vec![first] })
            .unwrap();
        assert!(prompt(&runner, (1, 1)).contains(&second));
        runner
            .act(GameAction::SelectCards {
                cards: vec![second],
            })
            .unwrap();
        assert_eq!(runner.state().players[0].mana_pool.total(), 3);
        assert!(runner.state().players[if stolen { 1 } else { 0 }]
            .graveyard
            .contains(&source));
        finish_payoff(&mut runner, source, &[source, other]);
    }
}

#[test]
fn x_mana_activation_pays_both_selected_sacrifices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let component = sacrifice(
        TypedFilter::permanent()
            .controller(ControllerRef::You)
            .into(),
        1,
    );
    let source = scenario
        .add_enchantment_from_oracle(P1, "Original X guard source", "")
        .controlled_by(P0)
        .with_ability_definition(payoff(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        generic: 0,
                        shards: vec![ManaCostShard::X],
                    },
                },
                component.clone(),
                component,
            ],
        }))
        .id();
    let other = scenario.add_enchantment_from_oracle(P0, "Other", "").id();
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
    assert!(matches!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .unwrap()
            .activation_residual,
        ActivationResidual::XMana
    ));
    runner.act(GameAction::ChooseX { value: 1 }).unwrap();
    assert_eq!(runner.state().players[0].mana_pool.total(), 3);
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    let first_choices = prompt(&runner, (1, 1));
    let WaitingFor::PayCost {
        resume: CostResume::Spell { spell },
        ..
    } = &runner.state().waiting_for
    else {
        unreachable!("prompt verified the Spell cost root")
    };
    assert_eq!(
        (spell.object_id, spell.activation_ability_index),
        (source, Some(0))
    );
    assert_eq!(
        (
            spell.ability.source_id,
            spell.ability.controller,
            spell.ability.chosen_x
        ),
        (source, P0, Some(1))
    );
    assert!(matches!(
        spell.activation_residual,
        ActivationResidual::XMana
    ));
    assert!(first_choices.contains(&source) && first_choices.contains(&other));
    runner
        .act(GameAction::SelectCards {
            cards: vec![source],
        })
        .unwrap();
    assert_eq!(prompt(&runner, (1, 1)), vec![other]);
    assert!(runner.state().players[1].graveyard.contains(&source));
    assert_eq!(runner.state().objects[&source].owner, P1);
    let WaitingFor::PayCost {
        resume: CostResume::Spell { spell },
        ..
    } = &runner.state().waiting_for
    else {
        unreachable!("prompt verified the Spell cost root")
    };
    assert_eq!(
        (spell.object_id, spell.activation_ability_index),
        (source, Some(0))
    );
    assert_eq!(
        (
            spell.ability.source_id,
            spell.ability.controller,
            spell.ability.chosen_x
        ),
        (source, P0, Some(1))
    );
    assert!(matches!(
        spell.activation_residual,
        ActivationResidual::XMana
    ));
    runner
        .act(GameAction::SelectCards { cards: vec![other] })
        .unwrap();
    let ability = runner.state().stack.back().unwrap().ability().unwrap();
    assert_eq!(ability.chosen_x, Some(1));
    let paid: Vec<_> = ability
        .cost_paid_objects
        .iter()
        .map(|record| record.snapshot().unwrap().object_id)
        .collect();
    assert_eq!(paid, vec![source, other]);
    assert!(runner.state().players[0].graveyard.contains(&other));
    assert_eq!(runner.state().players[0].mana_pool.total(), 3);
    finish_payoff(&mut runner, source, &[source, other]);
}
#[test]
fn selected_keyword_sacrifice_revalidates_its_current_quality() {
    for kind in ["other", "offering", "emerge", "emerge_artifact"] {
        for eligible in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            for _ in 0..5 {
                scenario.add_basic_land(P0, ManaColor::White);
            }
            let fodder = scenario
                .add_creature(P0, "Quality fodder", 2, 2)
                .as_artifact()
                .as_creature()
                .with_subtypes(vec!["Spirit"])
                .with_mana_cost(ManaCost::Cost {
                    generic: 1,
                    shards: vec![ManaCostShard::Green],
                })
                .id();
            let mut builder = scenario.add_spell_to_hand_from_oracle(
                P0,
                "Synthetic typed quality spell",
                false,
                "You gain 1 life.",
            );
            let cost = ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::White],
            };
            builder.with_mana_cost(cost.clone());
            match kind {
                "other" => {
                    builder.with_additional_cost(AdditionalCost::Required(sacrifice(
                        TypedFilter::creature().subtype("Spirit".into()).into(),
                        1,
                    )));
                }
                "offering" => {
                    builder.with_keyword(Keyword::Offering("Spirit".into()));
                }
                "emerge" => {
                    builder.with_keyword(Keyword::Emerge(EmergeCost::creature(cost.clone())));
                }
                "emerge_artifact" => {
                    builder.with_keyword(Keyword::Emerge(EmergeCost::from_quality(
                        cost,
                        TypedFilter::new(TypeFilter::Artifact).into(),
                    )));
                }
                _ => unreachable!(),
            }
            let spell = builder.id();
            let mut runner = scenario.build();
            assert!(runner.state().objects[&fodder]
                .card_types
                .core_types
                .contains(&engine::types::card_type::CoreType::Creature));
            assert!(runner.state().objects[&fodder]
                .card_types
                .core_types
                .contains(&engine::types::card_type::CoreType::Artifact));
            assert!(find_eligible_sacrifice_targets(
                runner.state(),
                P0,
                spell,
                None,
                &TypedFilter::creature().subtype("Spirit".into()).into()
            )
            .contains(&fodder));
            let card_id = runner.state().objects[&spell].card_id;
            runner
                .act(GameAction::CastSpell {
                    object_id: spell,
                    card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap();
            if kind == "offering" {
                runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .unwrap();
            }
            if kind.starts_with("emerge") {
                match &runner.state().waiting_for {
                    WaitingFor::CastingVariantChoice { options, .. } => {
                        let index = options
                            .iter()
                            .position(|option| option.variant == CastingVariant::Emerge)
                            .unwrap();
                        runner
                            .act(GameAction::ChooseCastingVariant { index })
                            .unwrap();
                    }
                    WaitingFor::AlternativeCastChoice { .. } => {
                        runner
                            .act(GameAction::ChooseAlternativeCast {
                                choice: AlternativeCastDecision::Alternative,
                            })
                            .unwrap();
                    }
                    other => panic!("real emerge choice: {other:?}"),
                }
            }
            assert!(prompt(&runner, (1, 1)).contains(&fodder));
            let WaitingFor::PayCost {
                resume: CostResume::SpellCost { cost, source, .. },
                ..
            } = &runner.state().waiting_for
            else {
                panic!("concrete spell cost");
            };
            assert_eq!(
                *source,
                match kind {
                    "offering" => SpellCostSource::Offering,
                    "emerge" | "emerge_artifact" => SpellCostSource::Emerge,
                    _ => SpellCostSource::Other,
                }
            );
            let AbilityCost::Sacrifice(selected) = cost.as_ref() else {
                panic!("selected Count cost");
            };
            let filter = selected.target.clone();
            // Synthetic hostile live-quality change, after the genuine producer advertised it.
            if !eligible {
                let obj = runner.state_mut().objects.get_mut(&fodder).unwrap();
                if matches!(kind, "other" | "offering") {
                    obj.card_types.subtypes.clear();
                } else if kind == "emerge" {
                    obj.card_types.core_types = vec![engine::types::card_type::CoreType::Artifact];
                } else {
                    obj.card_types.core_types = vec![engine::types::card_type::CoreType::Creature];
                }
                obj.base_card_types = obj.card_types.clone();
            }
            assert_eq!(
                find_eligible_sacrifice_targets(runner.state(), P0, spell, None, &filter)
                    .contains(&fodder),
                eligible
            );
            if eligible {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![fodder],
                    })
                    .unwrap();
                runner.resolve_top();
                assert_eq!(runner.state().players[0].life, 21);
                assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
                assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
                assert!(runner.state().stack.is_empty());
            } else {
                let before = serde_json::to_value(runner.state()).unwrap();
                assert!(
                    matches!(apply(runner.state_mut(), P0, GameAction::SelectCards { cards: vec![fodder] }),
                    Err(EngineError::ActionNotAllowed(ref text)) if text == "Selected permanent no longer eligible for sacrifice")
                );
                assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
            }
        }
    }
}

#[test]
fn additional_variable_sacrifice_keeps_announced_bound_after_control_loss() {
    for concede in [false, true] {
        let p2 = PlayerId(2);
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let filter: TargetFilter = TypedFilter::creature().into();
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Typed announced X sacrifice",
                false,
                "You gain 1 life.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::X],
            })
            .with_additional_cost(AdditionalCost::Required(sacrifice(
                filter.clone(),
                u32::MAX,
            )))
            .id();
        let retained = scenario.add_creature(P0, "Retained", 2, 2).id();
        let lost = scenario.add_creature(p2, "Lost", 2, 2).id();
        let aura = scenario
            .add_enchantment_from_oracle(
                P1,
                "Control Magic",
                "Enchant creature\nYou control enchanted creature.",
            )
            .controlled_by(P0)
            .with_subtypes(vec!["Aura"])
            .with_keyword(Keyword::Enchant(filter.clone()))
            .id();
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Green, ObjectId(9999), false, vec![]); 4],
        );
        let mut runner = scenario.build();
        attach_to(runner.state_mut(), aura, lost);
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseXValue { .. }
        ));
        runner.act(GameAction::ChooseX { value: 2 }).unwrap();
        let advertised = prompt(&runner, (2, 2));
        assert!(advertised.contains(&retained) && advertised.contains(&lost));
        if concede {
            apply(
                runner.state_mut(),
                P1,
                GameAction::Concede { player_id: P1 },
            )
            .unwrap();
            assert_eq!(prompt(&runner, (2, 2)), advertised);
            let live = find_eligible_sacrifice_targets(runner.state(), P0, spell, None, &filter);
            assert!(live.contains(&retained) && !live.contains(&lost));
            refuse_unchanged(&mut runner, P0, vec![retained]);
            let before = serde_json::to_value(runner.state()).unwrap();
            assert!(
                matches!(apply(runner.state_mut(), P0, GameAction::SelectCards { cards: vec![retained, lost] }),
                Err(EngineError::ActionNotAllowed(ref text)) if text == "Selected permanent no longer eligible for sacrifice")
            );
            assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
        } else {
            runner
                .act(GameAction::SelectCards {
                    cards: vec![retained, lost],
                })
                .unwrap();
            assert_eq!(runner.state().players[0].mana_pool.total(), 2);
            assert_eq!(
                runner
                    .state()
                    .stack
                    .back()
                    .unwrap()
                    .ability()
                    .unwrap()
                    .chosen_x,
                Some(2)
            );
            finish_payoff(&mut runner, spell, &[retained, lost]);
        }
    }
}
