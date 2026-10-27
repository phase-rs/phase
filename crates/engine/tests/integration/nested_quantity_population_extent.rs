//! Complete relative populations at their ordinary modal spell consumer.
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const COLLISION: &str = "Choose one —\n• Collision Course deals X damage to target creature, where X is the number of permanents you control that are creatures and/or Vehicles.\n• Destroy target artifact.";

// Pinned MTGJSON 2026-10-04, Oracle aceea999-90ab-472c-86a7-48af1542cbcf.
const VESTIGE: &str = "When this creature leaves the battlefield, draw a card, then you may put a permanent card with mana value less than or equal to the number of lands you control from your hand onto the battlefield tapped.\nWarp {4} (You may cast this card from your hand for its warp cost. Exile this creature at the beginning of the next end step, then you may cast it from exile on a later turn.)";

/// CR 608.2c + CR 608.2d + CR 608.2h: a labeled typed synthetic spell pauses
/// before its optional exact selection, then resumes with the live population.
/// Genuine current and historical parent continuations must make the same choice.
#[test]
fn private_exact_selection_public_insufficiency_enough_zero_up_to_and_mandatory_restore() {
    use engine::game::derived_views::ClientGameStateRef;
    use engine::game::scenario::GameRunner;
    use engine::types::ability::{
        AbilityCondition, AbilityDefinition, AbilityKind, Comparator, ControllerRef, Effect,
        EffectOutcomeSignal, FilterProp, MultiTargetSpec, QuantityExpr, QuantityRef,
        SubAbilityLink, TargetChoiceTiming, TargetFilter, TypeFilter, TypedFilter,
    };
    use engine::types::zones::EtbTapState;
    for actor in [P0, P1] {
        // 0 exact2/one, 1 exact2/three, 2 exact0, 3 up-to2,
        // 4 mandatory2/one, 5 mandatory2/three, 6 live-land exact2/one,
        // 7 live-land exact2/three. All begin with a paid mandatory Draw.
        for branch in 0..8 {
            let enough = matches!(branch, 1 | 3 | 5 | 7);
            let mandatory = matches!(branch, 4 | 5);
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.add_land_from_oracle(actor, "Live exact count land one", "");
            scenario.add_land_from_oracle(actor, "Live exact count land two", "");
            let independent = scenario
                .add_spell_to_library_top(actor, "Private independent tail draw", true)
                .id();
            let drawn = scenario
                .add_spell_to_library_top(actor, "Private preceding mandatory draw", true)
                .with_mana_cost(ManaCost::generic(1))
                .id();
            let first = scenario
                .add_artifact_to_hand_from_oracle(actor, "Private first legal permanent", "")
                .with_mana_cost(ManaCost::generic(1))
                .id();
            let second = scenario
                .add_artifact_to_hand_from_oracle(actor, "Private nondefault legal permanent", "")
                .with_mana_cost(ManaCost::generic(if enough { 1 } else { 5 }))
                .id();
            let foreign_actor = if actor == P0 { P1 } else { P0 };
            let foreign = scenario
                .add_artifact_to_hand_from_oracle(foreign_actor, "Foreign private decoy", "")
                .id();
            scenario.with_mana_pool(
                actor,
                vec![ManaUnit::new(
                    ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                )],
            );
            let mut tail = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            );
            tail.sub_link = SubAbilityLink::SequentialSibling;
            let gain = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 5 },
                    player: TargetFilter::Controller,
                },
            )
            .condition(AbilityCondition::EffectOutcome {
                signal: EffectOutcomeSignal::OptionalEffectPerformed,
            })
            .sub_ability(tail.clone());
            let mut put = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: Some(Zone::Hand),
                    destination: Zone::Battlefield,
                    target: TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Permanent)
                            .controller(ControllerRef::You)
                            .properties(vec![
                                FilterProp::InZone { zone: Zone::Hand },
                                FilterProp::Cmc {
                                    comparator: Comparator::LE,
                                    value: QuantityExpr::Fixed { value: 2 },
                                },
                            ]),
                    ),
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: Some(ControllerRef::You),
                    enter_tapped: EtbTapState::Tapped,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            )
            .sub_ability(if mandatory { tail } else { gain });
            put.optional = !mandatory;
            put.target_choice_timing = TargetChoiceTiming::Resolution;
            let exact = if branch >= 6 {
                QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(
                            TypedFilter::new(TypeFilter::Land).controller(ControllerRef::You),
                        ),
                    },
                }
            } else {
                QuantityExpr::Fixed {
                    value: if branch == 2 { 0 } else { 2 },
                }
            };
            put.multi_target = Some(if branch == 3 {
                MultiTargetSpec::up_to(exact)
            } else {
                MultiTargetSpec::exact(exact)
            });
            let draw = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )
            .sub_ability(put);
            let root = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Scry {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )
            .sub_ability(draw);
            let spell = scenario
                .add_spell_to_hand_from_oracle(
                    actor,
                    "Synthetic typed private exact selection",
                    true,
                    "",
                )
                .with_mana_cost(ManaCost::generic(1))
                .with_ability_definition(root)
                .id();
            let mut runner = scenario.build();
            runner.state_mut().active_player = actor;
            runner.state_mut().priority_player = actor;
            runner.state_mut().waiting_for = WaitingFor::Priority { player: actor };
            if enough {
                let object = runner.state_mut().objects.get_mut(&drawn).unwrap();
                object.card_types.core_types = vec![engine::types::card_type::CoreType::Artifact];
                object.base_card_types = object.card_types.clone();
            }
            // A public cast pays the cost; two real priority passes reach Scry,
            // a genuine pending parent boundary before the now-immediate child.
            runner.cast(spell).commit();
            runner.act(GameAction::PassPriority).unwrap();
            runner.act(GameAction::PassPriority).unwrap();
            assert!(
                matches!(&runner.state().waiting_for, WaitingFor::ScryChoice { player, cards } if *player == actor && cards == &vec![drawn])
            );
            assert_eq!(runner.state().players[actor.0 as usize].library.len(), 2);
            assert_eq!(
                runner.state().players[actor.0 as usize].mana_pool.total(),
                0
            );
            assert!(!runner.state().resolution_stack.is_empty());
            for restored in warp_restore_forms(runner.state()) {
                let mut resumed = GameRunner::from_state(restored);
                resumed
                    .act(GameAction::SelectCards { cards: vec![drawn] })
                    .unwrap();
                assert_eq!(resumed.state().objects[&drawn].zone, Zone::Hand);
                let impossible = matches!(branch, 0 | 6);
                if impossible || (mandatory && !enough) {
                    assert!(
                        matches!(resumed.state().waiting_for, WaitingFor::Priority { .. }),
                        "branch {branch}: {:?}",
                        resumed.state().waiting_for
                    );
                } else {
                    if !mandatory {
                        assert!(
                            matches!(resumed.state().waiting_for, WaitingFor::OptionalEffectChoice { player, source_id, .. } if player == actor && source_id == spell)
                        );
                        let accept = engine::ai_support::legal_actions(resumed.state())
                            .into_iter()
                            .find(|action| {
                                matches!(action, GameAction::DecideOptionalEffect { accept: true })
                            })
                            .unwrap();
                        resumed.act(accept).unwrap();
                    }
                    if branch != 2 {
                        let WaitingFor::EffectZoneChoice {
                            player,
                            cards,
                            min_count,
                            count,
                            ..
                        } = &resumed.state().waiting_for
                        else {
                            panic!("real exact/up-to selection branch {branch}")
                        };
                        assert_eq!(*player, actor);
                        assert_eq!((*min_count, *count), (if branch == 3 { 0 } else { 2 }, 2));
                        assert_eq!(
                            cards
                                .iter()
                                .copied()
                                .collect::<std::collections::BTreeSet<_>>(),
                            [first, second, drawn].into_iter().collect()
                        );
                        // CR 402.3: BOTH real envelopes hide private names and
                        // selectable identities from opponents and unseated viewers.
                        let before = serde_json::to_value(resumed.state()).unwrap();
                        for viewer in [Some(actor), Some(foreign_actor), None] {
                            let projected = match viewer {
                                Some(player) => engine::game::visibility::filter_state_for_viewer(
                                    resumed.state(),
                                    player,
                                ),
                                None => engine::game::visibility::filter_state_for_unseated_viewer(
                                    resumed.state(),
                                ),
                            };
                            let direct = serde_json::to_value(ClientGameStateRef::wrap(
                                resumed.state(),
                                viewer,
                            ))
                            .unwrap();
                            let filtered = serde_json::to_value(ClientGameStateRef::wrap_filtered(
                                resumed.state(),
                                &projected,
                                viewer,
                            ))
                            .unwrap();
                            assert_eq!(direct, filtered);
                            let WaitingFor::EffectZoneChoice {
                                cards: projected_cards,
                                ..
                            } = &projected.waiting_for
                            else {
                                panic!("projected selection")
                            };
                            assert_eq!(
                                projected_cards,
                                &if viewer == Some(actor) {
                                    cards.clone()
                                } else {
                                    vec![ObjectId(0); cards.len()]
                                }
                            );
                            if viewer != Some(actor) {
                                assert!(!direct
                                    .to_string()
                                    .contains("Private preceding mandatory draw"));
                                assert_eq!(
                                    projected.objects[&drawn].card_id,
                                    engine::types::identifiers::CardId(0)
                                );
                                assert!(projected.objects[&drawn].abilities.is_empty());
                            }
                        }
                        assert_eq!(serde_json::to_value(resumed.state()).unwrap(), before);
                        let selected = if branch == 3 {
                            vec![]
                        } else {
                            vec![second, drawn]
                        };
                        let action = engine::ai_support::legal_actions(resumed.state()).into_iter().find(|action| matches!(action, GameAction::SelectCards { cards } if cards.iter().copied().collect::<std::collections::BTreeSet<_>>() == selected.iter().copied().collect())).unwrap();
                        resumed.act(action).unwrap();
                    }
                }
                resumed.advance_until_stack_empty();
                let moved = enough && branch != 3;
                assert_eq!(
                    resumed.state().objects[&drawn].zone,
                    if moved { Zone::Battlefield } else { Zone::Hand }
                );
                if moved {
                    assert!(resumed.state().objects[&drawn].tapped);
                    assert_eq!(resumed.state().objects[&drawn].controller, actor);
                }
                assert_eq!(resumed.state().objects[&first].zone, Zone::Hand);
                assert_eq!(resumed.state().objects[&foreign].zone, Zone::Hand);
                assert_eq!(resumed.state().objects[&independent].zone, Zone::Hand);
                assert_eq!(resumed.state().players[actor.0 as usize].library.len(), 0);
                assert_eq!(
                    resumed.state().players[actor.0 as usize].life,
                    if !mandatory && !impossible { 25 } else { 20 }
                );
                assert!(matches!(
                    resumed.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                assert!(resumed.state().active_optional_effect_frame().is_none());
                assert!(
                    resumed.state().stack.is_empty()
                        && resumed.state().deferred_triggers.is_empty()
                        && resumed.state().pending_trigger.is_none()
                        && resumed.state().pending_trigger_order.is_none()
                        && resumed.state().pending_trigger_entry.is_none()
                        && resumed.state().pending_trigger_event_batch.is_empty()
                        && resumed.state().resolution_stack.is_empty()
                );
                let completed = serde_json::to_value(resumed.state()).unwrap();
                for viewer in [Some(actor), Some(foreign_actor), None] {
                    let projected = match viewer {
                        Some(player) => engine::game::visibility::filter_state_for_viewer(
                            resumed.state(),
                            player,
                        ),
                        None => engine::game::visibility::filter_state_for_unseated_viewer(
                            resumed.state(),
                        ),
                    };
                    let direct =
                        serde_json::to_value(ClientGameStateRef::wrap(resumed.state(), viewer))
                            .unwrap();
                    assert_eq!(
                        direct,
                        serde_json::to_value(ClientGameStateRef::wrap_filtered(
                            resumed.state(),
                            &projected,
                            viewer
                        ))
                        .unwrap()
                    );
                    assert!(matches!(projected.waiting_for, WaitingFor::Priority { .. }));
                    if viewer != Some(actor) && !moved {
                        assert_eq!(
                            projected.objects[&drawn].card_id,
                            engine::types::identifiers::CardId(0)
                        );
                        assert!(!direct
                            .to_string()
                            .contains("Private preceding mandatory draw"));
                    }
                }
                assert_eq!(serde_json::to_value(resumed.state()).unwrap(), completed);
                resumed.advance_until_stack_empty();
                assert_eq!(
                    resumed.state().players[actor.0 as usize].library.len(),
                    0,
                    "independent tail once"
                );
            }
        }
    }
}

/// CR 109.5 + CR 608.2d: the scoped player's Hand supplies the required
/// population, independently of the controller/source owner and their cards.
#[test]
fn private_exact_selection_public_scoped_owner_and_controller_are_distinct() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, Effect, FilterProp, MultiTargetSpec,
        PlayerFilter, QuantityExpr, SubAbilityLink, TargetChoiceTiming, TargetFilter, TypeFilter,
        TypedFilter,
    };
    use engine::types::zones::EtbTapState;
    for controller in [P0, P1] {
        let selected_owner = if controller == P0 { P1 } else { P0 };
        for enough in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.with_library_top(
                controller,
                &["Scoped first Draw", "Scoped independent Draw", "Reserve"],
            );
            let first = scenario
                .add_artifact_to_hand_from_oracle(selected_owner, "Scoped selected first", "")
                .id();
            let second = scenario
                .add_spell_to_hand_from_oracle(selected_owner, "Scoped selected second", true, "")
                .id();
            for index in 0..3 {
                scenario.add_land_to_hand(controller, &format!("Controller decoy {index}"));
            }
            let mut tail = AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            );
            tail.sub_link = SubAbilityLink::SequentialSibling;
            let mut put = AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::ChangeZone {
                    origin: Some(Zone::Hand),
                    destination: Zone::Battlefield,
                    target: TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Permanent)
                            .controller(ControllerRef::ScopedPlayer)
                            .properties(vec![FilterProp::InZone { zone: Zone::Hand }]),
                    ),
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: Some(ControllerRef::ScopedPlayer),
                    enter_tapped: EtbTapState::Tapped,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            )
            .sub_ability(tail);
            put.optional = true;
            put.player_scope = Some(PlayerFilter::Opponent);
            put.target_choice_timing = TargetChoiceTiming::Resolution;
            put.multi_target = Some(MultiTargetSpec::exact(QuantityExpr::Fixed { value: 2 }));
            let root = AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )
            .sub_ability(put);
            let source = scenario
                .add_creature(controller, "Synthetic scoped-selection producer", 2, 2)
                .with_ability_definition(root)
                .id();
            let mut runner = scenario.build();
            // Initial battlefield fixture: owner, current controller and scoped
            // selected owner are separate authorities, without a casting shortcut.
            runner.state_mut().objects.get_mut(&source).unwrap().owner = selected_owner;
            if enough {
                let object = runner.state_mut().objects.get_mut(&second).unwrap();
                object.card_types.core_types = vec![engine::types::card_type::CoreType::Artifact];
                object.base_card_types = object.card_types.clone();
            }
            runner.state_mut().active_player = controller;
            runner.state_mut().priority_player = controller;
            runner.state_mut().waiting_for = WaitingFor::Priority { player: controller };
            let before = runner.state().players[controller.0 as usize].library.len();
            runner.activate(source, 0).stop_at_optional().resolve();
            if enough {
                assert!(
                    matches!(runner.state().waiting_for, WaitingFor::OptionalEffectChoice { player, source_id, .. } if player == selected_owner && source_id == source)
                );
                assert_eq!(
                    runner.state().players[controller.0 as usize].library.len(),
                    before - 1
                );
                let frame = runner.state().active_optional_effect_frame().unwrap();
                assert_eq!(frame.ability.controller, selected_owner);
                assert_eq!(frame.ability.original_controller, Some(controller));
                assert_eq!(frame.ability.scoped_player, Some(selected_owner));
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .unwrap();
                let WaitingFor::EffectZoneChoice {
                    player,
                    cards,
                    count: 2,
                    min_count: 2,
                    ..
                } = &runner.state().waiting_for
                else {
                    panic!("scoped exact2")
                };
                assert_eq!(*player, selected_owner);
                assert_eq!(
                    cards
                        .iter()
                        .copied()
                        .collect::<std::collections::BTreeSet<_>>(),
                    [first, second].into_iter().collect()
                );
                let action = engine::ai_support::legal_actions(runner.state()).into_iter().find(|action| matches!(action, GameAction::SelectCards { cards } if cards.iter().copied().collect::<std::collections::BTreeSet<_>>() == [first, second].into_iter().collect())).unwrap();
                runner.act(action).unwrap();
            }
            runner.advance_until_stack_empty();
            for id in [first, second] {
                assert_eq!(
                    runner.state().objects[&id].zone,
                    if enough {
                        Zone::Battlefield
                    } else {
                        Zone::Hand
                    }
                );
                if enough {
                    assert_eq!(runner.state().objects[&id].controller, selected_owner);
                    assert!(runner.state().objects[&id].tapped);
                }
            }
            assert_eq!(
                runner.state().players[controller.0 as usize].library.len(),
                before - 2
            );
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(
                runner.state().stack.is_empty()
                    && runner.state().deferred_triggers.is_empty()
                    && runner.state().pending_trigger.is_none()
                    && runner.state().pending_trigger_order.is_none()
                    && runner.state().pending_trigger_entry.is_none()
                    && runner.state().pending_trigger_event_batch.is_empty()
                    && runner.state().resolution_stack.is_empty()
            );
        }
    }
}

/// CR 608.2c: a synthetic complete grammar control proves the independent
/// post-choice Draw survives accept and decline exactly once.
#[test]
fn nested_optional_put_resumes_independent_draw_once_after_accept_or_decline() {
    for actor in [P0, P1] {
        for accept in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let source = scenario.add_spell_to_hand_from_oracle(actor, "Synthetic nested optional with independent tail", true,
                "Draw a card, then you may put a permanent card with mana value less than or equal to the number of lands you control from your hand onto the battlefield tapped. Draw a card.")
                .with_mana_cost(ManaCost::generic(1)).id();
            scenario.add_land_from_oracle(actor, "Battlefield land one", "");
            scenario.add_land_from_oracle(actor, "Battlefield land two", "");
            let default = scenario.add_land_to_hand(actor, "Initial Hand land").id();
            scenario.with_library_top(actor, &["Independent tail draw", "Reserve"]);
            let drawn = scenario
                .add_spell_to_library_top(actor, "Newly drawn independent permanent", false)
                .as_artifact()
                .with_mana_cost(ManaCost::generic(2))
                .id();
            scenario.with_mana_pool(
                actor,
                vec![ManaUnit::new(
                    ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                )],
            );
            let mut runner = scenario.build();
            runner.state_mut().active_player = actor;
            runner.state_mut().priority_player = actor;
            runner.state_mut().waiting_for = WaitingFor::Priority { player: actor };
            let before = runner.state().players[actor.0 as usize].library.len();
            let outcome = runner.cast(source).commit().stop_at_optional().resolve();
            assert!(
                matches!(outcome.final_waiting_for(), WaitingFor::OptionalEffectChoice { player, source_id, .. } if *player == actor && *source_id == source)
            );
            assert_eq!(
                runner.state().players[actor.0 as usize].library.len(),
                before - 1
            );
            runner
                .act(GameAction::DecideOptionalEffect { accept })
                .unwrap();
            if accept {
                let WaitingFor::EffectZoneChoice { player, cards, .. } =
                    &runner.state().waiting_for
                else {
                    panic!("complete synthetic continuation selection")
                };
                assert_eq!(*player, actor);
                assert_eq!(
                    cards
                        .iter()
                        .copied()
                        .collect::<std::collections::BTreeSet<_>>(),
                    [default, drawn].into_iter().collect()
                );
                let action = engine::ai_support::legal_actions(runner.state()).into_iter().find(|action| matches!(action, GameAction::SelectCards { cards } if cards == &vec![drawn])).unwrap();
                runner.act(action).unwrap();
                assert_eq!(runner.state().objects[&drawn].zone, Zone::Battlefield);
                assert!(runner.state().objects[&drawn].tapped);
            } else {
                assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
            }
            runner.advance_until_stack_empty();
            assert_eq!(
                runner.state().players[actor.0 as usize].library.len(),
                before - 2
            );
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(
                runner.state().stack.is_empty()
                    && runner.state().deferred_triggers.is_empty()
                    && runner.state().pending_trigger.is_none()
                    && runner.state().pending_trigger_order.is_none()
                    && runner.state().pending_trigger_entry.is_none()
                    && runner.state().pending_trigger_event_batch.is_empty()
                    && runner.state().resolution_stack.is_empty()
            );
        }
    }
}

/// CR 109.5 + CR 113.7a + CR 400.7 + CR 608.2h: two independent departed
/// sources retain their trigger controller and old birth while the land count is live.
#[test]
fn vestige_two_sources_new_birth_control_changes_live_lands_and_retired_reply() {
    use engine::types::identifiers::ObjectIncarnationRef;
    for actor in [P0, P1] {
        let owner = if actor == P0 { P1 } else { P0 };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mut sources = vec![];
        let mut steals = vec![];
        let mut bounces = vec![];
        for _ in 0..2 {
            sources.push(
                scenario
                    .add_creature_to_hand_from_oracle(owner, "Anticausal Vestige", 7, 5, VESTIGE)
                    .with_mana_cost(ManaCost::generic(6))
                    .from_oracle_text_with_keywords(&["Warp"], VESTIGE)
                    .id(),
            );
            steals.push(
                scenario
                    .add_spell_to_hand_from_oracle(
                        actor,
                        "Synthetic paid source control",
                        true,
                        "Gain control of target creature.",
                    )
                    .with_mana_cost(ManaCost::generic(1))
                    .id(),
            );
            bounces.push(
                scenario
                    .add_spell_to_hand_from_oracle(
                        owner,
                        "Synthetic paid source bounce",
                        true,
                        "Return target creature to its owner's hand.",
                    )
                    .with_mana_cost(ManaCost::generic(1))
                    .id(),
            );
        }
        for player in [P0, P1] {
            scenario.add_enchantment_from_oracle(
                player,
                "Synthetic flash authority",
                "You may cast creature spells as though they had flash.",
            );
            scenario.with_mana_pool(
                player,
                (0..30)
                    .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
                    .collect(),
            );
        }
        let first_land = scenario
            .add_land_from_oracle(actor, "Live land one", "")
            .id();
        scenario.add_land_from_oracle(actor, "Live land two", "");
        scenario.add_land_from_oracle(actor, "Live land three", "");
        let destroy_land = scenario
            .add_spell_to_hand_from_oracle(
                owner,
                "Synthetic paid live population change",
                true,
                "Destroy target land.",
            )
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let zero = scenario
            .add_land_to_hand(actor, "Private actor Hand land zero")
            .id();
        let high = scenario
            .add_artifact_to_hand_from_oracle(
                actor,
                "Private actor MV3 excluded after live change",
                "",
            )
            .with_mana_cost(ManaCost::generic(3))
            .id();
        let first_draw = scenario
            .add_spell_to_library_top(actor, "Private second occurrence reserve", false)
            .as_artifact()
            .with_mana_cost(ManaCost::generic(2))
            .id();
        let second_draw = scenario
            .add_spell_to_library_top(actor, "Private first occurrence reserve", false)
            .as_artifact()
            .with_mana_cost(ManaCost::generic(2))
            .id();
        let foreign = scenario
            .add_artifact_to_hand_from_oracle(owner, "Private source owner Hand decoy", "")
            .with_mana_cost(ManaCost::generic(2))
            .id();
        let mut runner = scenario.build();
        runner.state_mut().active_player = owner;
        runner.state_mut().priority_player = owner;
        runner.state_mut().waiting_for = WaitingFor::Priority { player: owner };
        let mut captured = vec![];
        for i in 0..2 {
            let _ = runner.cast(sources[i]).commit();
            runner.resolve_top();
            let _ = runner.cast(steals[i]).target_object(sources[i]).commit();
            runner.resolve_top();
            assert_eq!(runner.state().objects[&sources[i]].controller, actor);
            captured.push(ObjectIncarnationRef::from_object(
                &runner.state().objects[&sources[i]],
            ));
        }
        for i in 0..2 {
            let _ = runner.cast(bounces[i]).target_object(sources[i]).commit();
            runner.act(GameAction::PassPriority).unwrap();
            runner.act(GameAction::PassPriority).unwrap();
            assert_eq!(runner.state().objects[&sources[i]].zone, Zone::Hand);
        }
        // Public recast before either LTB resolves: current source controller
        // and birth now differ from its captured departed controller/birth.
        let _ = runner.cast(sources[0]).commit();
        runner.resolve_top();
        assert_eq!(runner.state().objects[&sources[0]].controller, owner);
        assert!(runner.state().objects[&sources[0]].incarnation > captured[0].incarnation);
        let _ = runner.cast(destroy_land).target_object(first_land).commit();
        runner.resolve_top();
        assert_eq!(runner.state().objects[&first_land].zone, Zone::Graveyard);
        let triggers: Vec<_> = runner
            .state()
            .stack
            .iter()
            .filter(|entry| sources.contains(&entry.source_id))
            .collect();
        assert_eq!(triggers.len(), 2);
        for trigger in triggers {
            let i = sources
                .iter()
                .position(|id| *id == trigger.source_id)
                .unwrap();
            assert_eq!(trigger.controller, actor);
            assert_eq!(
                trigger
                    .ability()
                    .unwrap()
                    .trigger_source
                    .as_ref()
                    .unwrap()
                    .identity
                    .reference,
                captured[i]
            );
        }
        let mut retired_reply: Option<GameAction> = None;
        for (occurrence, newly_drawn) in [second_draw, first_draw].into_iter().enumerate() {
            runner.advance_until_stack_empty();
            let WaitingFor::OptionalEffectChoice {
                player, source_id, ..
            } = runner.state().waiting_for
            else {
                panic!("each captured LTB draws before its own optional child")
            };
            assert_eq!(player, actor);
            assert_eq!(source_id, sources[1 - occurrence]);
            assert_eq!(runner.state().objects[&newly_drawn].zone, Zone::Hand);
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .unwrap();
            let WaitingFor::EffectZoneChoice {
                player,
                cards,
                source_id: selected_source,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("each occurrence owns a fresh bounded Hand choice")
            };
            assert_eq!((*player, *selected_source), (actor, source_id));
            assert_eq!(
                cards
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>(),
                [zero, newly_drawn].into_iter().collect()
            );
            assert!(!cards.contains(&high) && !cards.contains(&foreign));
            if let Some(old) = &retired_reply {
                assert!(
                    runner.act(old.clone()).is_err(),
                    "old selected object is no longer in this occurrence's legal Hand set"
                );
            }
            let response = engine::ai_support::legal_actions(runner.state()).into_iter().find(|action| matches!(action, GameAction::SelectCards { cards } if cards == &vec![newly_drawn])).unwrap();
            retired_reply = Some(response.clone());
            runner.act(response).unwrap();
            let selected = &runner.state().objects[&newly_drawn];
            assert_eq!(
                (selected.zone, selected.controller, selected.tapped),
                (Zone::Battlefield, actor, true)
            );
        }
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&sources[0]].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&sources[0]].controller, owner);
        assert_eq!(runner.state().objects[&sources[1]].zone, Zone::Hand);
        assert_eq!(runner.state().objects[&high].zone, Zone::Hand);
        assert_eq!(runner.state().objects[&foreign].zone, Zone::Hand);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(
            runner.state().stack.is_empty()
                && runner.state().deferred_triggers.is_empty()
                && runner.state().pending_trigger.is_none()
                && runner.state().pending_trigger_order.is_none()
                && runner.state().pending_trigger_entry.is_none()
                && runner.state().pending_trigger_event_batch.is_empty()
                && runner.state().resolution_stack.is_empty()
        );
    }
}

/// CR 608.2g: genuine targeted and own-graveyard siblings still cast a complete
/// spell during resolution. These synthetic controls do not claim Spectral support.
#[test]
fn refused_spectral_window_preserves_public_cast_siblings_draw_and_flying_body() {
    use engine::types::ability::{Effect, QuantityExpr, TargetFilter};
    use engine::types::game_state::CastOfferKind;
    use engine::types::keywords::Keyword;
    const CAST: &str = "You may cast an instant or sorcery spell with mana value less than or equal to the number of Spirits you control from a graveyard without paying its mana cost. If that spell would be put into a graveyard, exile it instead.";
    const SPECTRAL: &str = "Flying\nWhen this creature enters, you may cast an instant or sorcery spell with mana value less than or equal to the number of Spirits you control from a graveyard without paying its mana cost. If that spell would be put into a graveyard, exile it instead.";
    for actor in [P0, P1] {
        let other = if actor == P0 { P1 } else { P0 };
        for targeted in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let text = if targeted {
                "You may cast target instant card from your graveyard without paying its mana cost."
            } else {
                "You may cast up to one instant spell from your graveyard without paying its mana cost."
            };
            let parent = scenario
                .add_spell_to_hand_from_oracle(
                    actor,
                    "Synthetic supported graveyard cast",
                    true,
                    text,
                )
                .with_mana_cost(ManaCost::generic(1))
                .id();
            let child = scenario
                .add_spell_to_graveyard(actor, "Distinct complete graveyard Draw", true)
                .from_oracle_text("Draw a card.")
                .with_mana_cost(ManaCost::generic(2))
                .id();
            let foreign = scenario
                .add_spell_to_graveyard(other, "Foreign graveyard decoy", true)
                .from_oracle_text("Draw a card.")
                .with_mana_cost(ManaCost::generic(2))
                .id();
            scenario.with_library_top(actor, &["New child draw", "Reserve one", "Reserve two"]);
            scenario.with_mana_pool(
                actor,
                vec![ManaUnit::new(
                    ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                )],
            );
            let mut runner = scenario.build();
            runner.state_mut().active_player = actor;
            runner.state_mut().priority_player = actor;
            runner.state_mut().waiting_for = WaitingFor::Priority { player: actor };
            if targeted {
                let _ = runner.cast(parent).target_object(child).commit();
            } else {
                let _ = runner.cast(parent).commit();
            }
            runner.resolve_top();
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::OptionalEffectChoice { player, .. } if player == actor)
            );
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .unwrap();
            if !targeted {
                assert!(
                    matches!(runner.state().waiting_for, WaitingFor::CastOffer { player, kind: CastOfferKind::FreeCastWindow { .. } } if player == actor)
                );
                let choose = engine::ai_support::legal_actions(runner.state()).into_iter().find(|action| matches!(action, GameAction::FreeCastWindowChoice { selection: Some(selected) } if *selected == child)).expect("real own-GY offered child");
                assert!(!engine::ai_support::legal_actions(runner.state()).iter().any(|action| matches!(action, GameAction::FreeCastWindowChoice { selection: Some(selected) } if *selected == foreign)));
                runner.act(choose).unwrap();
            }
            assert_eq!(runner.state().objects[&child].zone, Zone::Stack);
            let before = runner.state().players[actor.0 as usize].library.len();
            runner.advance_until_stack_empty();
            assert_eq!(
                runner.state().players[actor.0 as usize].library.len(),
                before - 1
            );
            assert_eq!(runner.state().objects[&child].zone, Zone::Graveyard);
            assert_eq!(runner.state().objects[&foreign].zone, Zone::Graveyard);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(
                runner.state().stack.is_empty()
                    && runner.state().deferred_triggers.is_empty()
                    && runner.state().pending_trigger.is_none()
                    && runner.state().pending_trigger_order.is_none()
                    && runner.state().pending_trigger_entry.is_none()
                    && runner.state().pending_trigger_event_batch.is_empty()
                    && runner.state().resolution_stack.is_empty()
            );
        }
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        // Full synthetic source, explicitly distinct from the named card.
        let text = format!("{CAST} Draw a card.");
        let parsed = engine::parser::oracle::parse_oracle_text(
            &text,
            "Synthetic refused operation plus independent Draw",
            &[],
            &["Instant".into()],
            &[],
        );
        assert_eq!(
            *parsed.abilities[0].effect,
            Effect::unimplemented("untargeted_dynamic_graveyard_cast_with_rider", CAST)
        );
        assert_eq!(
            *parsed.abilities[0].sub_ability.as_ref().unwrap().effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller
            }
        );
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                actor,
                "Synthetic refused operation plus independent Draw",
                true,
                &text,
            )
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let spectral = scenario
            .add_creature_to_hand_from_oracle(actor, "Spectral Arcanist", 3, 2, SPECTRAL)
            .with_mana_cost(ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::Blue],
            })
            .with_subtypes(vec!["Spirit", "Wizard"])
            .from_oracle_text_with_keywords(&["Flying"], SPECTRAL)
            .id();
        scenario.with_library_top(actor, &["Independent Draw", "Reserve one", "Reserve two"]);
        scenario.with_mana_pool(
            actor,
            (0..7)
                .map(|index| {
                    ManaUnit::new(
                        if index == 0 {
                            ManaType::Blue
                        } else {
                            ManaType::Colorless
                        },
                        ObjectId(0),
                        false,
                        vec![],
                    )
                })
                .collect(),
        );
        let mut runner = scenario.build();
        runner.state_mut().active_player = actor;
        runner.state_mut().priority_player = actor;
        runner.state_mut().waiting_for = WaitingFor::Priority { player: actor };
        runner
            .cast(spell)
            .decline_optional()
            .resolve()
            .assert_hand_drawn(actor, 1);
        runner.cast(spectral).decline_optional().resolve();
        let body = &runner.state().objects[&spectral];
        assert_eq!(
            (body.zone, body.power, body.toughness),
            (Zone::Battlefield, Some(3), Some(2))
        );
        assert!(body.keywords.contains(&Keyword::Flying));
        assert!(
            body.card_types.subtypes.contains(&"Spirit".into())
                && body.card_types.subtypes.contains(&"Wizard".into())
        );
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(
            runner.state().stack.is_empty()
                && runner.state().deferred_triggers.is_empty()
                && runner.state().pending_trigger.is_none()
                && runner.state().pending_trigger_order.is_none()
                && runner.state().pending_trigger_entry.is_none()
                && runner.state().pending_trigger_event_batch.is_empty()
                && runner.state().resolution_stack.is_empty()
        );
    }
}

/// CR 109.2 + CR 202.3e + CR 608.2c + CR 608.2h: the live BF count bounds
/// a resolution-time Hand choice after the mandatory draw, including Hand X=0.
#[test]
fn vestige_paid_departure_and_paid_warp_select_new_draw_at_live_battlefield_ceiling() {
    use engine::game::engine::apply;
    use engine::game::scenario::GameRunner;
    use engine::game::turn_control::authorized_submitter;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, EffectScope, ReplacementDefinition, TapStateChange,
        TargetFilter,
    };
    use engine::types::actions::AlternativeCastDecision;
    use engine::types::game_state::{ActivePlayerControl, CastingVariant};
    use engine::types::identifiers::ObjectIncarnationRef;
    use engine::types::replacements::ReplacementEvent;
    for actor in [P0, P1] {
        let other = if actor == P0 { P1 } else { P0 };
        for warped in [false, true] {
            for lands in [2, 3] {
                // multi / decline / no legal permanent / sole immediate /
                // one entry modifier / ambiguous two-modifier entry.
                for branch in 0..6 {
                    if lands == 3 && branch != 0 {
                        continue;
                    }
                    let controlled = branch == 0 && lands == 2;
                    let submitter = if controlled { other } else { actor };
                    let mut scenario = GameScenario::new();
                    scenario.at_phase(Phase::PreCombatMain);
                    let source = scenario
                        .add_creature_to_hand_from_oracle(
                            actor,
                            "Anticausal Vestige",
                            7,
                            5,
                            VESTIGE,
                        )
                        .with_mana_cost(ManaCost::generic(6))
                        .with_subtypes(vec!["Eldrazi"])
                        .from_oracle_text_with_keywords(&["Warp"], VESTIGE)
                        .id();
                    for _ in 0..lands {
                        scenario.add_land_from_oracle(actor, "Controlled battlefield land", "");
                    }
                    let grave_land = scenario
                        .add_spell_to_graveyard(actor, "Unrelated graveyard land", false)
                        .as_land()
                        .id();
                    let foreign = scenario
                        .add_creature_to_hand(other, "Foreign hand decoy", 2, 2)
                        .with_mana_cost(ManaCost::generic(2))
                        .id();
                    let offhand = scenario
                        .add_creature_to_graveyard(actor, "Offhand decoy", 2, 2)
                        .with_mana_cost(ManaCost::generic(2))
                        .id();
                    let high = scenario
                        .add_creature_to_hand(actor, "Above live ceiling", 4, 4)
                        .with_mana_cost(ManaCost::generic(lands + 1))
                        .id();
                    let nonpermanent = scenario
                        .add_spell_to_hand_from_oracle(
                            actor,
                            "Hand instant decoy",
                            true,
                            "Draw a card.",
                        )
                        .with_mana_cost(ManaCost::generic(1))
                        .id();
                    let mut expected = vec![];
                    if branch != 2 && branch != 3 {
                        expected.push(
                            scenario
                                .add_land_to_hand(actor, "Private Hand land zero")
                                .id(),
                        );
                        expected.push(
                            scenario
                                .add_artifact_to_hand_from_oracle(
                                    actor,
                                    "Private Hand artifact",
                                    "",
                                )
                                .with_mana_cost(ManaCost::generic(lands))
                                .id(),
                        );
                        expected.push(
                            scenario
                                .add_creature_to_hand(actor, "Private Hand X permanent", 1, 1)
                                .with_mana_cost(ManaCost::Cost {
                                    generic: 0,
                                    shards: vec![ManaCostShard::X],
                                })
                                .id(),
                        );
                    }
                    scenario
                        .with_library_top(actor, &["Private reserve one", "Private reserve two"]);
                    let drawn = scenario
                        .add_spell_to_library_top(
                            actor,
                            "Private newly drawn bounded creature",
                            false,
                        )
                        .with_mana_cost(ManaCost::generic(lands))
                        .as_creature()
                        .id();
                    if branch != 2 {
                        expected.push(drawn);
                    }
                    let remove = scenario
                        .add_spell_to_hand_from_oracle(
                            actor,
                            "Synthetic paid departure",
                            true,
                            "Destroy target creature.",
                        )
                        .with_mana_cost(ManaCost::generic(1))
                        .id();
                    let mut modifiers = vec![];
                    for i in 0..if branch == 4 {
                        1
                    } else if branch == 5 {
                        2
                    } else {
                        0
                    } {
                        let replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
                            .valid_card(TargetFilter::Named {
                                name: "Private newly drawn bounded creature".into(),
                            })
                            .destination_zone(Zone::Battlefield)
                            .execute(AbilityDefinition::new(
                                AbilityKind::Spell,
                                Effect::SetTapState {
                                    target: TargetFilter::SelfRef,
                                    scope: EffectScope::Single,
                                    state: TapStateChange::Tap,
                                },
                            ));
                        modifiers.push(
                            scenario
                                .add_creature(other, &format!("Synthetic entry modifier {i}"), 0, 1)
                                .with_replacement_definition(replacement)
                                .id(),
                        );
                    }
                    scenario.with_mana_pool(
                        actor,
                        (0..20)
                            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
                            .collect(),
                    );
                    let mut runner = scenario.build();
                    {
                        let state = runner.state_mut();
                        state.active_player = actor;
                        state.priority_player = submitter;
                        state.waiting_for = WaitingFor::Priority { player: actor };
                        if controlled {
                            state.turn_decision_controller = Some(submitter);
                            state.turn_decision_control_timestamp = Some(1);
                            state.active_full_turn_control = Some(ActivePlayerControl {
                                controller: submitter,
                                timestamp: 1,
                            });
                        }
                        let object = state.objects.get_mut(&drawn).unwrap();
                        if branch == 2 {
                            object.card_types.core_types =
                                vec![engine::types::card_type::CoreType::Sorcery];
                            object.base_card_types = object.card_types.clone();
                        } else {
                            object.power = Some(2);
                            object.base_power = Some(2);
                            object.toughness = Some(2);
                            object.base_toughness = Some(2);
                        }
                    }
                    if warped {
                        pay_warp_through_public_actions(&mut runner, actor, source);
                    } else {
                        let cast = engine::ai_support::legal_actions(runner.state()).into_iter().find(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == source)).unwrap();
                        runner.act(cast).unwrap();
                        runner
                            .act(GameAction::ChooseAlternativeCast {
                                choice: AlternativeCastDecision::Normal,
                            })
                            .unwrap();
                        assert!(matches!(
                            runner.state().stack.last().unwrap().kind,
                            StackEntryKind::Spell {
                                casting_variant: CastingVariant::Normal,
                                actual_mana_spent: 6,
                                ..
                            }
                        ));
                        runner.advance_until_stack_empty();
                    }
                    let captured =
                        ObjectIncarnationRef::from_object(&runner.state().objects[&source]);
                    assert_eq!(
                        (
                            runner.state().objects[&source].power,
                            runner.state().objects[&source].toughness
                        ),
                        (Some(7), Some(5))
                    );
                    let before_draw = runner.state().players[actor.0 as usize].library.len();
                    if warped {
                        assert_eq!(runner.state().delayed_triggers.len(), 1);
                        assert_eq!(
                            runner.state().delayed_triggers[0]
                                .ability
                                .trigger_source
                                .as_ref()
                                .unwrap()
                                .identity
                                .reference,
                            captured
                        );
                        runner.advance_to_end_step();
                        runner.advance_until_stack_empty();
                        assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
                        assert!(runner.state().objects[&source].casting_permissions.iter().any(|permission| matches!(permission, engine::types::ability::CastingPermission::WarpExile { castable_after_turn } if *castable_after_turn == runner.state().turn_number)));
                    } else {
                        let _ = runner.cast(remove).target_object(source).commit();
                        runner.act(GameAction::PassPriority).unwrap();
                        runner.act(GameAction::PassPriority).unwrap();
                        let trigger = runner
                            .state()
                            .stack
                            .iter()
                            .find(|entry| entry.source_id == source)
                            .expect("real queued LTB");
                        assert_eq!(trigger.controller, actor);
                        assert_eq!(
                            trigger
                                .ability()
                                .unwrap()
                                .trigger_source
                                .as_ref()
                                .unwrap()
                                .identity
                                .reference,
                            captured
                        );
                        runner.advance_until_stack_empty();
                        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
                    }
                    assert_eq!(
                        runner.state().objects[&source].incarnation,
                        captured.incarnation + 1
                    );
                    assert_eq!(
                        runner.state().players[actor.0 as usize].library.len(),
                        before_draw - 1
                    );
                    assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
                    if branch == 2 {
                        assert!(matches!(
                            runner.state().waiting_for,
                            WaitingFor::Priority { .. }
                        ));
                        assert!(
                            runner.state().stack.is_empty()
                                && runner.state().deferred_triggers.is_empty()
                                && runner.state().pending_trigger.is_none()
                                && runner.state().pending_trigger_order.is_none()
                                && runner.state().pending_trigger_entry.is_none()
                                && runner.state().pending_trigger_event_batch.is_empty()
                                && runner.state().resolution_stack.is_empty()
                        );
                        continue;
                    }
                    assert!(
                        matches!(runner.state().waiting_for, WaitingFor::OptionalEffectChoice { player, source_id, .. } if player == actor && source_id == source)
                    );
                    assert_eq!(
                        runner
                            .state()
                            .active_optional_effect_frame()
                            .unwrap()
                            .ability
                            .trigger_source
                            .as_ref()
                            .unwrap()
                            .identity
                            .reference,
                        captured
                    );
                    assert_warp_draw_privacy(runner.state(), actor, drawn, source);
                    for optional_state in warp_restore_forms(runner.state()) {
                        let mut optional = GameRunner::from_state(optional_state);
                        let response = engine::ai_support::legal_actions(optional.state()).into_iter().find(|action| matches!(action, GameAction::DecideOptionalEffect { accept } if *accept == (branch != 1))).unwrap();
                        assert_eq!(optional.state().waiting_for.acting_player(), Some(actor));
                        assert_eq!(authorized_submitter(optional.state()), Some(submitter));
                        assert!(apply(
                            optional.state_mut(),
                            if submitter == actor { other } else { actor },
                            response.clone()
                        )
                        .is_err());
                        apply(optional.state_mut(), submitter, response).unwrap();
                        if branch == 1 {
                            optional.advance_until_stack_empty();
                            assert_eq!(optional.state().objects[&drawn].zone, Zone::Hand);
                            assert_warp_draw_privacy(optional.state(), actor, drawn, source);
                            assert!(matches!(
                                optional.state().waiting_for,
                                WaitingFor::Priority { .. }
                            ));
                            assert!(
                                optional.state().stack.is_empty()
                                    && optional.state().deferred_triggers.is_empty()
                                    && optional.state().pending_trigger.is_none()
                                    && optional.state().pending_trigger_order.is_none()
                                    && optional.state().pending_trigger_entry.is_none()
                                    && optional.state().pending_trigger_event_batch.is_empty()
                                    && optional.state().resolution_stack.is_empty()
                            );
                            continue;
                        }
                        if branch == 3 {
                            assert_eq!(optional.state().objects[&drawn].zone, Zone::Battlefield);
                            assert!(optional.state().objects[&drawn].tapped);
                            assert!(matches!(
                                optional.state().waiting_for,
                                WaitingFor::Priority { .. }
                            ));
                            assert!(
                                optional.state().stack.is_empty()
                                    && optional.state().deferred_triggers.is_empty()
                                    && optional.state().pending_trigger.is_none()
                                    && optional.state().pending_trigger_order.is_none()
                                    && optional.state().pending_trigger_entry.is_none()
                                    && optional.state().pending_trigger_event_batch.is_empty()
                                    && optional.state().resolution_stack.is_empty()
                            );
                            continue;
                        }
                        let WaitingFor::EffectZoneChoice {
                            player,
                            cards,
                            count,
                            min_count,
                            up_to,
                            source_id,
                            zone,
                            destination,
                            enter_tapped,
                            ..
                        } = &optional.state().waiting_for
                        else {
                            panic!("actual bounded Hand selection")
                        };
                        assert_eq!(
                            (
                                *player,
                                *source_id,
                                *count,
                                *min_count,
                                *up_to,
                                *zone,
                                *destination
                            ),
                            (
                                actor,
                                source,
                                1,
                                0,
                                false,
                                Zone::Hand,
                                Some(Zone::Battlefield)
                            )
                        );
                        assert!(enter_tapped.is_tapped());
                        assert_eq!(
                            cards
                                .iter()
                                .copied()
                                .collect::<std::collections::BTreeSet<_>>(),
                            expected.iter().copied().collect()
                        );
                        assert_ne!(
                            cards[0], drawn,
                            "newly drawn legal card is a nondefault choice"
                        );
                        assert_warp_draw_privacy(optional.state(), actor, drawn, source);
                        for selection_state in warp_restore_forms(optional.state()) {
                            let mut selection = GameRunner::from_state(selection_state);
                            for illegal in [high, nonpermanent, foreign, offhand, grave_land] {
                                assert!(selection
                                    .act(GameAction::SelectCards {
                                        cards: vec![illegal]
                                    })
                                    .is_err());
                            }
                            let choose = engine::ai_support::legal_actions(selection.state()).into_iter().find(|action| matches!(action, GameAction::SelectCards { cards } if cards == &vec![drawn])).expect("actual AI-issued nondefault newly drawn selection");
                            assert!(apply(
                                selection.state_mut(),
                                if submitter == actor { other } else { actor },
                                choose.clone()
                            )
                            .is_err());
                            let birth = selection.state().objects[&drawn].incarnation;
                            let delivered =
                                apply(selection.state_mut(), submitter, choose.clone()).unwrap();
                            // CR 614.12 + CR 614.12a: prospective entry modifiers
                            // choose before the selected permanent actually arrives.
                            if branch == 5 {
                                assert!(
                                    matches!(
                                        selection.state().waiting_for,
                                        WaitingFor::ReplacementChoice {
                                            candidate_count: 2,
                                            ..
                                        }
                                    ),
                                    "two actual entry modifiers require a real ordering choice"
                                );
                            }
                            let arrivals = if matches!(
                                selection.state().waiting_for,
                                WaitingFor::ReplacementChoice { .. }
                            ) {
                                warp_restore_forms(selection.state())
                            } else {
                                vec![selection.state().clone()]
                            };
                            for arrival in arrivals {
                                let mut selection = GameRunner::from_state(arrival);
                                let mut events = delivered.events.clone();
                                assert_warp_draw_privacy(
                                    selection.state(),
                                    actor,
                                    expected[0],
                                    source,
                                );
                                for _ in 0..3 {
                                    if !matches!(
                                        selection.state().waiting_for,
                                        WaitingFor::ReplacementChoice { .. }
                                    ) {
                                        break;
                                    }
                                    let pending =
                                        selection.state().pending_replacement.as_ref().unwrap();
                                    assert!(pending
                                        .candidates
                                        .iter()
                                        .all(|id| modifiers.contains(&id.source)));
                                    let index = pending.candidates.len() - 1;
                                    let selected_replacement_source =
                                        pending.candidates[index].source;
                                    let response = engine::ai_support::legal_actions(selection.state()).into_iter().find(|action| matches!(action, GameAction::ChooseReplacement { index: candidate } if *candidate == index)).unwrap();
                                    let applied = selection.act(response).unwrap();
                                    assert!(applied.events.iter().any(|event| matches!(event, engine::types::events::GameEvent::ReplacementApplied { source_id, .. } if *source_id == selected_replacement_source)));
                                    events.extend(applied.events);
                                }
                                selection.advance_until_stack_empty();
                                // CR 110.2a: the instructed player controls the entrant.
                                let selected = &selection.state().objects[&drawn];
                                assert_eq!(
                                    (
                                        selected.zone,
                                        selected.controller,
                                        selected.incarnation,
                                        selected.tapped
                                    ),
                                    (Zone::Battlefield, actor, birth + 1, true)
                                );
                                assert_eq!(events.iter().filter(|event| matches!(event, engine::types::events::GameEvent::ZoneChanged { object_id, from: Some(Zone::Hand), to: Zone::Battlefield, .. } if *object_id == drawn)).count(), 1);
                                assert_eq!(
                                    selection.state().players[actor.0 as usize].library.len(),
                                    before_draw - 1,
                                    "no duplicate mandatory draw"
                                );
                                assert!(
                                    selection.act(choose.clone()).is_err(),
                                    "retired selection cannot be replayed"
                                );
                                assert_eq!(selection.state().objects[&high].zone, Zone::Hand);
                                assert_eq!(selection.state().objects[&foreign].zone, Zone::Hand);
                                assert_eq!(
                                    selection.state().objects[&offhand].zone,
                                    Zone::Graveyard
                                );
                                assert_warp_draw_privacy(
                                    selection.state(),
                                    actor,
                                    expected[0],
                                    source,
                                );
                                assert!(matches!(
                                    selection.state().waiting_for,
                                    WaitingFor::Priority { .. }
                                ));
                                assert!(
                                    selection.state().stack.is_empty()
                                        && selection.state().deferred_triggers.is_empty()
                                        && selection.state().pending_trigger.is_none()
                                        && selection.state().pending_trigger_order.is_none()
                                        && selection.state().pending_trigger_entry.is_none()
                                        && selection.state().pending_trigger_event_batch.is_empty()
                                        && selection.state().resolution_stack.is_empty()
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

fn pay_warp_through_public_actions(
    runner: &mut engine::game::scenario::GameRunner,
    caster: engine::types::player::PlayerId,
    source: ObjectId,
) {
    use engine::types::actions::AlternativeCastDecision;
    use engine::types::game_state::{AlternativeCastKeyword, CastingVariant};
    if runner.state().waiting_for.acting_player() != Some(caster) {
        runner.act(GameAction::PassPriority).unwrap();
    }
    let before = runner.state().players[caster.0 as usize].mana_pool.total();
    let cast = engine::ai_support::legal_actions(runner.state()).into_iter()
        .find(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == source)).expect("AI-issued legal hand cast");
    runner.act(cast).unwrap();
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::AlternativeCastChoice { player, object_id, keyword: AlternativeCastKeyword::Warp, normal_cost, alternative_cost, .. }
        if *player == caster && *object_id == source && *normal_cost == ManaCost::generic(6) && *alternative_cost == Some(ManaCost::generic(4)))
    );
    let choose = engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .find(|action| {
            matches!(
                action,
                GameAction::ChooseAlternativeCast {
                    choice: AlternativeCastDecision::Alternative
                }
            )
        })
        .expect("AI-issued Warp response");
    runner.act(choose).unwrap();
    assert!(matches!(
        runner.state().stack.last().unwrap().kind,
        StackEntryKind::Spell {
            casting_variant: CastingVariant::Warp,
            actual_mana_spent: 4,
            ..
        }
    ));
    assert_eq!(
        runner.state().players[caster.0 as usize].mana_pool.total(),
        before - 4
    );
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
}

fn warp_restore_forms(
    state: &engine::types::game_state::GameState,
) -> Vec<engine::types::game_state::GameState> {
    use engine::types::game_state::PersistedGameState;
    use engine::types::resolution::{ResolutionFrame, ResolutionStateWire};
    fn remove_old_draw_fields(value: &mut serde_json::Value, version: u64) {
        match value {
            serde_json::Value::Object(fields) => {
                if version < 4 {
                    fields.remove("delivery_owner");
                }
                fields.remove("dealer");
                for child in fields.values_mut() {
                    remove_old_draw_fields(child, version);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    remove_old_draw_fields(item, version);
                }
            }
            _ => {}
        }
    }
    let mut restored = vec![
        state.clone(),
        serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap(),
        PersistedGameState::capture(state.clone())
            .into_game_state()
            .unwrap(),
    ];
    // Historical v1 has independent owner slots, not a typed frame array.
    let mut v1 = serde_json::to_value(state).unwrap();
    v1.as_object_mut().unwrap().remove("resolution_stack");
    v1["resolution_state_version"] = serde_json::json!(1);
    for frame in state.resolution_stack.iter() {
        match frame {
            ResolutionFrame::AbilityContinuation(frame) => {
                assert!(v1.get("pending_continuation").is_none());
                v1["pending_continuation"] = serde_json::to_value(&frame.pending).unwrap();
                if let Some(context) = &frame.choose_zone_trigger_context {
                    v1["pending_choose_zone_trigger_context"] =
                        serde_json::to_value(context).unwrap();
                }
            }
            ResolutionFrame::ChangeZone(frame) => {
                if let Some(pending) = &frame.pending {
                    assert!(
                        pending.remaining.is_empty(),
                        "single-source Warp has no entry-modified remaining members to downgrade"
                    );
                    v1["pending_change_zone_iteration"] = serde_json::to_value(pending).unwrap();
                }
                if let Some(snapshot) = &frame.devour_eligible_snapshot {
                    v1["devour_eligible_snapshot"] = serde_json::to_value(snapshot).unwrap();
                }
            }
            ResolutionFrame::PostReplacement(frame) => {
                v1["post_replacement_drains"] = serde_json::to_value(frame).unwrap();
            }
            ResolutionFrame::OptionalEffect(frame) => {
                assert!(
                    frame.trigger_events.len() <= 1,
                    "v1 has only the singular LTB event slot"
                );
                assert!(
                    frame.return_result_occurrence.is_none(),
                    "v1 cannot represent instruction-result history"
                );
                v1["pending_optional_effect"] = serde_json::to_value(&frame.ability).unwrap();
                v1["pending_optional_trigger_event"] =
                    serde_json::to_value(&frame.trigger_event).unwrap();
                v1["pending_optional_trigger_match_count"] =
                    serde_json::to_value(frame.trigger_match_count).unwrap();
            }
            other => {
                panic!("fixture must explicitly encode its actual historical owner: {other:?}")
            }
        }
    }
    remove_old_draw_fields(&mut v1, 1);
    restored.push(
        serde_json::from_value::<ResolutionStateWire>(v1)
            .unwrap()
            .into_game_state(),
    );
    for version in [2, 3, 4] {
        let mut wire =
            serde_json::to_value(ResolutionStateWire::from_game_state(state.clone())).unwrap();
        wire["resolution_state_version"] = serde_json::json!(version);
        remove_old_draw_fields(&mut wire, version);
        restored.push(
            serde_json::from_value::<ResolutionStateWire>(wire)
                .unwrap()
                .into_game_state(),
        );
    }
    restored
}

/// CR 400.7j + CR 608.2c + CR 701.3a: a return of another object keeps its
/// result and original producer through replacement and legacy restoration.
#[test]
fn nonself_battlefield_result_restores_ungrouped_grant_and_attach_once() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, AttachSelection, ContinuousModification, ControllerRef,
        Effect, QuantityExpr, ReplacementDefinition, ReplacementMode, StaticDefinition,
        SubAbilityLink, TargetFilter,
    };
    use engine::types::identifiers::ObjectIncarnationRef;
    use engine::types::keywords::Keyword;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::zones::EtbTapState;

    // Honest synthetic composition: public activation returns Equipment, then
    // optionally grants it haste or attaches it to the producing creature.
    for caster in [P0, P1] {
        for attach in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let returned = scenario
                .add_artifact_from_oracle(caster, "Synthetic returned Equipment", "")
                .with_subtypes(vec!["Equipment"])
                .id();
            let mut draw = AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            );
            draw.sub_link = SubAbilityLink::SequentialSibling;
            let mut rider = AbilityDefinition::new(
                AbilityKind::Database,
                if attach {
                    Effect::Attach {
                        attachment: TargetFilter::SelfRef,
                        target: TargetFilter::ParentTarget,
                        selection: AttachSelection::Targeted,
                    }
                } else {
                    Effect::GenericEffect {
                        static_abilities: vec![StaticDefinition::continuous()
                            .affected(TargetFilter::SelfRef)
                            .modifications(vec![ContinuousModification::AddKeyword {
                                keyword: Keyword::Haste,
                            }])],
                        duration: None,
                        target: Some(TargetFilter::SelfRef),
                        end_cost: None,
                    }
                },
            )
            .sub_ability(draw);
            rider.optional = true;
            let mut root = AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    target: TargetFilter::SpecificObject { id: returned },
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: Some(ControllerRef::You),
                    enter_tapped: EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            )
            .sub_ability(rider);
            root.forward_result = true;
            let producer = scenario
                .add_creature(caster, "Synthetic returning producer", 2, 2)
                .with_ability_definition(root)
                .id();
            scenario
                .add_enchantment_from_oracle(caster, "Synthetic optional entry replacement", "")
                .with_replacement_definition(
                    ReplacementDefinition::new(ReplacementEvent::Moved)
                        .valid_card(TargetFilter::SpecificObject { id: returned })
                        .destination_zone(Zone::Battlefield)
                        .mode(ReplacementMode::Optional { decline: None }),
                );
            scenario.with_library_top(caster, &["Independent return sibling Draw", "Reserve"]);
            let mut runner = scenario.build();
            engine::game::zones::move_to_zone(
                runner.state_mut(),
                returned,
                Zone::Graveyard,
                &mut Vec::new(),
            );
            runner.state_mut().active_player = caster;
            runner.state_mut().priority_player = caster;
            runner.state_mut().waiting_for = WaitingFor::Priority { player: caster };
            runner.activate(producer, 0).stop_at_optional().resolve();
            assert!(matches!(runner.state().waiting_for,
                WaitingFor::ReplacementChoice { player, candidate_count: 2, .. } if player == caster));
            let delivery = runner
                .state()
                .pending_zone_change_delivery_from_replacement()
                .unwrap();
            assert_eq!(delivery.member.object_id, returned);
            assert_ne!(producer, returned);
            assert!(delivery.terminal_completion.is_none() && delivery.delivery_events.is_empty());

            for grouped in [true, false] {
                let mut paused = runner.state().clone();
                if !grouped {
                    // Encode the supported old ungrouped owner using this real
                    // pending replacement's captured member; invent no prompt,
                    // completion or delivery event. v1 uses independent slots.
                    paused.take_active_change_zone_frame().unwrap().unwrap();
                    let marker = paused
                        .active_ability_continuation_frame_mut()
                        .unwrap()
                        .pending
                        .chain
                        .context
                        .pending_forwarded_zone_result
                        .as_mut()
                        .unwrap();
                    marker.group = None;
                    marker.selected = Some(vec![delivery.member]);
                }
                let marker = paused
                    .active_ability_continuation()
                    .unwrap()
                    .chain
                    .context
                    .pending_forwarded_zone_result
                    .as_ref()
                    .unwrap();
                assert_eq!(marker.producer, producer);
                if !grouped {
                    assert_eq!(marker.selected.as_deref(), Some(&[delivery.member][..]));
                    assert!(marker.group.is_none());
                }
                for restored in warp_restore_forms(&paused) {
                    let mut resumed = engine::game::scenario::GameRunner::from_state(restored);
                    resumed
                        .act(GameAction::ChooseReplacement { index: 1 })
                        .unwrap();
                    resumed.advance_until_stack_empty();
                    assert!(matches!(resumed.state().waiting_for,
                        WaitingFor::OptionalEffectChoice { player, source_id, .. }
                        if player == caster && source_id == returned));
                    let child = &resumed
                        .state()
                        .active_optional_effect_frame()
                        .unwrap()
                        .ability;
                    assert!(child.context.pending_forwarded_zone_result.is_none());
                    let result = child.context.forwarded_result_context.as_deref().unwrap();
                    let pin =
                        ObjectIncarnationRef::from_object(&resumed.state().objects[&returned]);
                    assert_eq!(
                        Some(pin.incarnation),
                        delivery.member.incarnation.checked_add(1)
                    );
                    assert_eq!(result.targets, vec![TargetRef::Object(returned)]);
                    assert_eq!(result.object_incarnations, vec![pin]);
                    assert!(pin.is_current(resumed.state()));
                    if attach {
                        assert_eq!(
                            child.targets,
                            vec![TargetRef::Object(producer)],
                            "Attach retains the original producer as its host"
                        );
                    }
                    resumed
                        .act(GameAction::DecideOptionalEffect { accept: true })
                        .unwrap();
                    resumed.advance_until_stack_empty();
                    assert_eq!(resumed.state().objects[&returned].zone, Zone::Battlefield);
                    if attach {
                        assert_eq!(
                            resumed.state().objects[&returned].attached_to,
                            Some(engine::game::game_object::AttachTarget::Object(producer))
                        );
                    } else {
                        assert_eq!(
                            resumed
                                .state()
                                .transient_continuous_effects
                                .iter()
                                .filter(|effect| effect.affected
                                    == (TargetFilter::SpecificObject { id: returned })
                                    && effect.modifications.contains(
                                        &ContinuousModification::AddKeyword {
                                            keyword: Keyword::Haste,
                                        }
                                    ))
                                .count(),
                            1
                        );
                    }
                    assert_eq!(resumed.state().players[caster.0 as usize].hand.len(), 1);
                    resumed.advance_until_stack_empty();
                    assert_eq!(
                        resumed.state().players[caster.0 as usize].hand.len(),
                        1,
                        "independent sibling and returned rider complete exactly once"
                    );
                    assert!(matches!(
                        resumed.state().waiting_for,
                        WaitingFor::Priority { .. }
                    ));
                    assert!(
                        resumed.state().stack.is_empty()
                            && resumed.state().deferred_triggers.is_empty()
                            && resumed.state().pending_trigger.is_none()
                            && resumed.state().pending_trigger_order.is_none()
                            && resumed.state().pending_trigger_entry.is_none()
                            && resumed.state().pending_trigger_event_batch.is_empty()
                            && resumed.state().resolution_stack.is_empty()
                    );
                }
            }
        }
    }
}

/// CR 400.2: inspect the actual Hand object and printed-card identifier in
/// both client envelopes, while preserving the authoritative private state.
fn assert_warp_draw_privacy(
    state: &engine::types::game_state::GameState,
    owner: engine::types::player::PlayerId,
    drawn: ObjectId,
    public_exile: ObjectId,
) {
    use engine::game::derived_views::ClientGameStateRef;
    let before = serde_json::to_value(state).unwrap();
    let actual = &state.objects[&drawn];
    assert_eq!(actual.zone, Zone::Hand);
    assert_ne!(actual.card_id, engine::types::identifiers::CardId(0));
    for viewer in state
        .players
        .iter()
        .map(|player| Some(player.id))
        .chain(std::iter::once(None))
    {
        let filtered = match viewer {
            Some(player) => engine::game::visibility::filter_state_for_viewer(state, player),
            None => engine::game::visibility::filter_state_for_unseated_viewer(state),
        };
        let sees_private =
            viewer == Some(owner) || (viewer.is_some() && viewer == state.turn_decision_controller);
        let object = &filtered.objects[&drawn];
        assert_eq!(
            object.id, drawn,
            "opaque object identity is retained independently of its printed face"
        );
        if sees_private {
            assert_eq!(
                (object.name.as_str(), object.card_id),
                (actual.name.as_str(), actual.card_id)
            );
        } else {
            assert_eq!(object.name, "Hidden Card");
            assert_eq!(object.card_id, engine::types::identifiers::CardId(0));
            assert!(object.abilities.is_empty() && object.keywords.is_empty());
        }
        assert_eq!(filtered.objects[&public_exile].name, "Anticausal Vestige");
        let direct = serde_json::to_value(ClientGameStateRef::wrap(state, viewer)).unwrap();
        let prefiltered =
            serde_json::to_value(ClientGameStateRef::wrap_filtered(state, &filtered, viewer))
                .unwrap();
        assert_eq!(direct, prefiltered);
        let payload = &direct["state"]["objects"][drawn.0.to_string()];
        assert_eq!(payload["name"], serde_json::json!(object.name));
        assert_eq!(payload["card_id"], serde_json::json!(object.card_id));
        // CR 402.3: a private Hand choice cannot expose another player's card identities.
        if let WaitingFor::EffectZoneChoice {
            player,
            cards,
            zone: Zone::Hand,
            ..
        } = &state.waiting_for
        {
            assert_eq!(*player, owner);
            let WaitingFor::EffectZoneChoice {
                cards: projected, ..
            } = &filtered.waiting_for
            else {
                panic!("private selection retains its public prompt shape")
            };
            assert_eq!(
                projected,
                &if sees_private {
                    cards.clone()
                } else {
                    vec![ObjectId(0); cards.len()]
                }
            );
            for envelope in [&direct, &prefiltered] {
                assert_eq!(
                    envelope["state"]["waiting_for"]["cards"],
                    serde_json::to_value(projected).unwrap()
                );
            }
        }
        if !sees_private {
            assert!(
                !direct.to_string().contains(&actual.name),
                "private newly drawn object must not escape through another envelope field"
            );
        }
    }
    assert_eq!(
        serde_json::to_value(state).unwrap(),
        before,
        "projection cannot alter current/in-flight/completed authority"
    );
}

/// CR 702.185a + CR 608.2c: both installation routes retain their exact paid source and continuation.
#[test]
fn warp_actual_entry_and_exile_replacements_restore_and_keep_actor_submitter_private_draw() {
    use engine::game::engine::apply;
    use engine::game::turn_control::authorized_submitter;
    use engine::types::ability::{CastingPermission, ReplacementDefinition};
    use engine::types::actions::AlternativeCastDecision;
    use engine::types::game_state::{ActivePlayerControl, CastingVariant};
    use engine::types::identifiers::ObjectIncarnationRef;
    use engine::types::player::PlayerId;
    use engine::types::replacements::ReplacementEvent;
    for caster in [P0, P1] {
        let submitter = if caster == P0 { P1 } else { P0 };
        let mut scenario = GameScenario::new_n_player(3, 714);
        scenario.at_phase(Phase::PreCombatMain);
        let vestige = scenario
            .add_creature_to_hand_from_oracle(caster, "Anticausal Vestige", 7, 5, VESTIGE)
            .with_mana_cost(ManaCost::generic(6))
            .with_subtypes(vec!["Eldrazi"])
            .from_oracle_text_with_keywords(&["Warp"], VESTIGE)
            .id();
        let hand_land = scenario
            .add_land_to_hand(caster, "Available decline permanent")
            .id();
        let mut guard_sources = Vec::new();
        for destination in [Zone::Battlefield, Zone::Exile] {
            for ordinal in 0..2 {
                guard_sources.push(
                    scenario
                        .add_enchantment_from_oracle(
                            caster,
                            "Synthetic exact replacement boundary",
                            "",
                        )
                        .with_replacement_definition(
                            ReplacementDefinition::new(ReplacementEvent::Moved)
                                .valid_card(engine::types::ability::TargetFilter::SpecificObject {
                                    id: vestige,
                                })
                                .destination_zone(destination)
                                .description(format!(
                                    "{destination:?} selected boundary {ordinal}"
                                )),
                        )
                        .id(),
                );
            }
        }
        for player in [P0, P1, PlayerId(2)] {
            scenario.with_library_top(
                player,
                &[
                    "Private newly drawn independent card",
                    "Private reserve one",
                    "Private reserve two",
                ],
            );
        }
        scenario.with_mana_pool(
            caster,
            (0..6)
                .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.active_player = caster;
            state.turn_decision_controller = Some(submitter);
            state.turn_decision_control_timestamp = Some(1);
            state.active_full_turn_control = Some(ActivePlayerControl {
                controller: submitter,
                timestamp: 1,
            });
            state.priority_player = submitter;
            state.waiting_for = WaitingFor::Priority { player: caster };
        }
        let cast = engine::ai_support::legal_actions(runner.state()).into_iter().find(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == vestige)).unwrap();
        assert_eq!(runner.state().waiting_for.acting_player(), Some(caster));
        assert_eq!(authorized_submitter(runner.state()), Some(submitter));
        assert!(apply(runner.state_mut(), caster, cast.clone()).is_err());
        apply(runner.state_mut(), submitter, cast).unwrap();
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::AlternativeCastChoice { player, object_id, .. } if player == caster && object_id == vestige)
        );
        let choose = engine::ai_support::legal_actions(runner.state())
            .into_iter()
            .find(|action| {
                matches!(
                    action,
                    GameAction::ChooseAlternativeCast {
                        choice: AlternativeCastDecision::Alternative
                    }
                )
            })
            .unwrap();
        assert!(apply(runner.state_mut(), caster, choose.clone()).is_err());
        apply(runner.state_mut(), submitter, choose).unwrap();
        assert!(matches!(
            runner.state().stack.last().unwrap().kind,
            StackEntryKind::Spell {
                casting_variant: CastingVariant::Warp,
                actual_mana_spent: 4,
                ..
            }
        ));
        runner.resolve_top();
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::ReplacementChoice { player, candidate_count: 2, .. } if player == caster)
        );
        assert!(
            runner.state().delayed_triggers.is_empty(),
            "NeedsChoice has not entered the paid spell yet"
        );
        for (form_index, entered) in warp_restore_forms(runner.state()).into_iter().enumerate() {
            let mut runner = engine::game::scenario::GameRunner::from_state(entered);
            let pending = runner.state().pending_replacement.as_ref().unwrap();
            assert_eq!(pending.candidates.len(), 2);
            assert!(pending
                .candidates
                .iter()
                .all(|rid| guard_sources[..2].contains(&rid.source)));
            let response = engine::ai_support::legal_actions(runner.state())
                .into_iter()
                .find(|action| matches!(action, GameAction::ChooseReplacement { index: 1 }))
                .unwrap();
            assert_eq!(runner.state().waiting_for.acting_player(), Some(caster));
            assert_eq!(authorized_submitter(runner.state()), Some(submitter));
            assert!(apply(runner.state_mut(), caster, response.clone()).is_err());
            apply(runner.state_mut(), submitter, response).unwrap();
            runner.advance_until_stack_empty();
            let captured = ObjectIncarnationRef::from_object(&runner.state().objects[&vestige]);
            assert_eq!(
                (
                    runner.state().objects[&vestige].zone,
                    runner.state().objects[&vestige].power,
                    runner.state().objects[&vestige].toughness
                ),
                (Zone::Battlefield, Some(7), Some(5))
            );
            assert_eq!(
                runner.state().delayed_triggers.len(),
                1,
                "NeedsChoice installs exactly the same captured delayed producer"
            );
            assert_eq!(
                runner.state().delayed_triggers[0]
                    .ability
                    .trigger_source
                    .as_ref()
                    .unwrap()
                    .identity
                    .reference,
                captured
            );
            for delayed in warp_restore_forms(runner.state())
                .into_iter()
                .skip(form_index)
                .take(1)
            {
                let mut runner = engine::game::scenario::GameRunner::from_state(delayed);
                let hand_before = runner.state().players[caster.0 as usize].hand.clone();
                runner.advance_to_end_step();
                runner.advance_until_stack_empty();
                assert!(
                    matches!(runner.state().waiting_for, WaitingFor::ReplacementChoice { player, candidate_count: 2, .. } if player == caster)
                );
                assert!(runner.state().objects[&vestige]
                    .casting_permissions
                    .is_empty());
                for paused in warp_restore_forms(runner.state())
                    .into_iter()
                    .skip(form_index)
                    .take(1)
                {
                    let mut runner = engine::game::scenario::GameRunner::from_state(paused);
                    let pending = runner.state().pending_replacement.as_ref().unwrap();
                    assert!(pending
                        .candidates
                        .iter()
                        .all(|rid| guard_sources[2..].contains(&rid.source)));
                    let response = engine::ai_support::legal_actions(runner.state())
                        .into_iter()
                        .find(|action| matches!(action, GameAction::ChooseReplacement { index: 1 }))
                        .unwrap();
                    assert_eq!(runner.state().waiting_for.acting_player(), Some(caster));
                    assert_eq!(authorized_submitter(runner.state()), Some(submitter));
                    assert!(apply(runner.state_mut(), caster, response.clone()).is_err());
                    apply(runner.state_mut(), submitter, response).unwrap();
                    runner.advance_until_stack_empty();
                    assert_eq!(runner.state().objects[&vestige].zone, Zone::Exile);
                    assert_eq!(
                        runner.state().objects[&vestige].incarnation,
                        captured.incarnation + 1
                    );
                    assert_eq!(runner.state().objects[&vestige].casting_permissions.iter().filter(|permission| matches!(permission, CastingPermission::WarpExile { castable_after_turn } if *castable_after_turn == runner.state().turn_number)).count(), 1);
                    let drawn = *runner.state().players[caster.0 as usize]
                        .hand
                        .iter()
                        .find(|id| !hand_before.contains(id))
                        .unwrap();
                    assert_eq!(
                        runner.state().players[caster.0 as usize].hand.len(),
                        hand_before.len() + 1
                    );
                    assert!(
                        matches!(runner.state().waiting_for, WaitingFor::OptionalEffectChoice { player, source_id, .. } if player == caster && source_id == vestige)
                    );
                    assert_warp_draw_privacy(runner.state(), caster, drawn, vestige);
                    let response = engine::ai_support::legal_actions(runner.state())
                        .into_iter()
                        .find(|action| {
                            matches!(action, GameAction::DecideOptionalEffect { accept: false })
                        })
                        .unwrap();
                    assert!(apply(runner.state_mut(), caster, response.clone()).is_err());
                    apply(runner.state_mut(), submitter, response).unwrap();
                    runner.advance_until_stack_empty();
                    assert_eq!(runner.state().objects[&hand_land].zone, Zone::Hand);
                    assert_warp_draw_privacy(runner.state(), caster, drawn, vestige);
                    assert!(matches!(
                        runner.state().waiting_for,
                        WaitingFor::Priority { .. }
                    ));
                    assert!(
                        runner.state().stack.is_empty()
                            && runner.state().deferred_triggers.is_empty()
                            && runner.state().pending_trigger.is_none()
                            && runner.state().pending_trigger_order.is_none()
                            && runner.state().pending_trigger_entry.is_none()
                            && runner.state().pending_trigger_event_batch.is_empty()
                            && runner.state().resolution_stack.is_empty()
                    );
                    assert_eq!(
                        runner.state().objects[&vestige].casting_permissions.len(),
                        1,
                        "completed parent drains and grants once"
                    );
                }
            }
        }
    }
}

/// CR 603.7c + CR 400.7j + CR 702.185a: the delayed operation must
/// earn its own right; a different operation's Exile successor cannot do so.
#[test]
fn paid_warp_stale_births_do_not_exile_or_grant_beside_real_remaining_source() {
    use engine::types::ability::CastingPermission;
    use engine::types::actions::AlternativeCastDecision;
    use engine::types::game_state::{CastingVariant, GameState, PersistedGameState};
    for caster in [P0, P1] {
        for change in ["bounce", "blink", "normal recast", "unrelated exile"] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let stale = scenario
                .add_creature_to_hand_from_oracle(caster, "Anticausal Vestige", 7, 5, VESTIGE)
                .with_mana_cost(ManaCost::generic(6))
                .with_subtypes(vec!["Eldrazi"])
                .from_oracle_text_with_keywords(&["Warp"], VESTIGE)
                .id();
            let remaining = scenario
                .add_creature_to_hand_from_oracle(caster, "Anticausal Vestige", 7, 5, VESTIGE)
                .with_mana_cost(ManaCost::generic(6))
                .with_subtypes(vec!["Eldrazi"])
                .from_oracle_text_with_keywords(&["Warp"], VESTIGE)
                .id();
            let operation = scenario.add_spell_to_hand_from_oracle(caster, "Synthetic birth-changing operation", true,
                match change {
                    "blink" => "Exile target creature, then return it to the battlefield under its owner's control.",
                    "unrelated exile" => "Exile target creature.",
                    _ => "Return target creature to its owner's hand.",
                }).with_mana_cost(ManaCost::generic(1)).id();
            for player in [P0, P1] {
                scenario.with_library_top(
                    player,
                    &[
                        "Private draw A",
                        "Private draw B",
                        "Private draw C",
                        "Private draw D",
                        "Private draw E",
                        "Private draw F",
                    ],
                );
            }
            scenario.with_mana_pool(
                caster,
                (0..30)
                    .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
                    .collect(),
            );
            let mut runner = scenario.build();
            runner.state_mut().active_player = caster;
            runner.state_mut().priority_player = caster;
            runner.state_mut().waiting_for = WaitingFor::Priority { player: caster };
            pay_warp_through_public_actions(&mut runner, caster, stale);
            pay_warp_through_public_actions(&mut runner, caster, remaining);
            let original = runner.state().objects[&stale].incarnation;
            assert_eq!(runner.state().delayed_triggers.len(), 2);
            runner.cast(operation).target_object(stale).resolve();
            runner.advance_until_stack_empty();
            assert!(
                runner.state().objects[&stale].incarnation > original,
                "the public operation changes the captured birth"
            );
            if change == "normal recast" {
                if runner.state().priority_player != caster {
                    runner.act(GameAction::PassPriority).unwrap();
                }
                let cast = engine::ai_support::legal_actions(runner.state()).into_iter()
                    .find(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == stale)).unwrap();
                runner.act(cast).unwrap();
                assert!(
                    matches!(runner.state().waiting_for, WaitingFor::AlternativeCastChoice { object_id, .. } if object_id == stale)
                );
                runner
                    .act(GameAction::ChooseAlternativeCast {
                        choice: AlternativeCastDecision::Normal,
                    })
                    .unwrap();
                assert!(matches!(
                    runner.state().stack.last().unwrap().kind,
                    StackEntryKind::Spell {
                        casting_variant: CastingVariant::Normal,
                        actual_mana_spent: 6,
                        ..
                    }
                ));
                runner.advance_until_stack_empty();
                assert_eq!(runner.state().objects[&stale].zone, Zone::Battlefield);
                assert_eq!(
                    runner.state().delayed_triggers.len(),
                    2,
                    "normal recast adds no Warp instruction"
                );
            }
            let unchanged_zone = runner.state().objects[&stale].zone;
            let unchanged_birth = runner.state().objects[&stale].incarnation;
            let raw: GameState =
                serde_json::from_value(serde_json::to_value(runner.state()).unwrap()).unwrap();
            let trusted = PersistedGameState::capture(runner.state().clone())
                .into_game_state()
                .unwrap();
            for restored in [runner.state().clone(), raw, trusted] {
                let mut runner = engine::game::scenario::GameRunner::from_state(restored);
                runner.advance_to_end_step();
                runner.advance_until_stack_empty();
                assert_eq!(
                    (
                        runner.state().objects[&stale].zone,
                        runner.state().objects[&stale].incarnation
                    ),
                    (unchanged_zone, unchanged_birth),
                    "old delayed instruction cannot exile another birth: {change}"
                );
                assert!(
                    !runner.state().objects[&stale]
                        .casting_permissions
                        .iter()
                        .any(|permission| matches!(
                            permission,
                            CastingPermission::WarpExile { .. }
                        )),
                    "a stale instruction cannot grant: {change}"
                );
                assert_eq!(
                    runner.state().objects[&remaining].zone,
                    Zone::Exile,
                    "paid remaining source is the positive delivery reach guard"
                );
                assert_eq!(runner.state().objects[&remaining].casting_permissions.iter().filter(|permission| matches!(permission, CastingPermission::WarpExile { castable_after_turn } if *castable_after_turn == runner.state().turn_number)).count(), 1, "only the actually exiled source earns once");
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                assert!(
                    runner.state().stack.is_empty()
                        && runner.state().deferred_triggers.is_empty()
                        && runner.state().pending_trigger.is_none()
                        && runner.state().pending_trigger_order.is_none()
                        && runner.state().pending_trigger_entry.is_none()
                        && runner.state().pending_trigger_event_batch.is_empty()
                        && runner.state().resolution_stack.is_empty()
                );
            }
        }
    }
}

/// CR 702.185a + CR 118.9c: distinct selected costs and the card owner's later right remain independent.
#[test]
fn paid_warp_two_sources_keep_selected_cost_and_owner_after_control_changes() {
    use engine::types::ability::CastingPermission;
    use engine::types::actions::AlternativeCastDecision;
    use engine::types::game_state::{AlternativeCastKeyword, CastingVariant};
    for owner in [P0, P1] {
        let thief = if owner == P0 { P1 } else { P0 };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let vestige = scenario
            .add_creature_to_hand_from_oracle(owner, "Anticausal Vestige", 7, 5, VESTIGE)
            .with_mana_cost(ManaCost::generic(6))
            .with_subtypes(vec!["Eldrazi"])
            .from_oracle_text_with_keywords(&["Warp"], VESTIGE)
            .id();
        let distinct = scenario
            .add_creature_to_hand(owner, "Synthetic distinct Warp cost source", 3, 3)
            .with_mana_cost(ManaCost::generic(8))
            .from_oracle_text_with_keywords(&["Warp"], "Warp {2}")
            .id();
        let steal = scenario
            .add_spell_to_hand_from_oracle(
                thief,
                "Synthetic control operation",
                true,
                "Gain control of target creature.",
            )
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let twin = scenario
            .add_creature_to_hand(thief, "Nonowner legal funded twin", 2, 2)
            .with_mana_cost(ManaCost::generic(1))
            .id();
        for player in [P0, P1] {
            scenario.add_enchantment_from_oracle(
                player,
                "Synthetic creature timing authority",
                "You may cast creature spells as though they had flash.",
            );
            scenario.with_library_top(
                player,
                &[
                    "Private draw one",
                    "Private draw two",
                    "Private draw three",
                    "Private draw four",
                ],
            );
            scenario.with_mana_pool(
                player,
                (0..20)
                    .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
                    .collect(),
            );
        }
        let mut runner = scenario.build();
        runner.state_mut().active_player = owner;
        runner.state_mut().priority_player = owner;
        runner.state_mut().waiting_for = WaitingFor::Priority { player: owner };
        pay_warp_through_public_actions(&mut runner, owner, vestige);
        let cast = engine::ai_support::legal_actions(runner.state()).into_iter().find(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == distinct)).unwrap();
        runner.act(cast).unwrap();
        assert!(
            matches!(&runner.state().waiting_for, WaitingFor::AlternativeCastChoice { object_id, keyword: AlternativeCastKeyword::Warp, normal_cost, alternative_cost, .. } if *object_id == distinct && *normal_cost == ManaCost::generic(8) && *alternative_cost == Some(ManaCost::generic(2)))
        );
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .unwrap();
        assert!(matches!(
            runner.state().stack.last().unwrap().kind,
            StackEntryKind::Spell {
                casting_variant: CastingVariant::Warp,
                actual_mana_spent: 2,
                ..
            }
        ));
        let paid_pool = runner.state().players[owner.0 as usize].mana_pool.total();
        assert!(
            runner
                .act(GameAction::ChooseAlternativeCast {
                    choice: AlternativeCastDecision::Alternative
                })
                .is_err(),
            "an old cost answer cannot pay or create another source"
        );
        assert_eq!(
            runner.state().players[owner.0 as usize].mana_pool.total(),
            paid_pool
        );
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().delayed_triggers.len(), 2);
        runner.act(GameAction::PassPriority).unwrap();
        assert_eq!(runner.state().priority_player, thief);
        runner.cast(steal).target_object(vestige).resolve();
        assert_eq!(
            (
                runner.state().objects[&vestige].controller,
                runner.state().objects[&vestige].owner
            ),
            (thief, owner)
        );
        runner.advance_to_end_step();
        runner.advance_until_stack_empty();
        for source in [vestige, distinct] {
            assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
            assert_eq!(
                runner.state().objects[&source]
                    .casting_permissions
                    .iter()
                    .filter(|permission| matches!(permission, CastingPermission::WarpExile { .. }))
                    .count(),
                1
            );
        }
        runner.advance_to_upkeep();
        runner.advance_to_phase(Phase::PreCombatMain);
        assert_eq!(runner.state().active_player, thief);
        for _ in 0..10 {
            runner.state_mut().players[thief.0 as usize]
                .mana_pool
                .add(ManaUnit::new(
                    ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                ));
        }
        runner.cast(twin).commit();
        assert!(matches!(
            runner.state().stack.last().unwrap().kind,
            StackEntryKind::Spell {
                actual_mana_spent: 1,
                ..
            }
        ));
        runner.advance_until_stack_empty();
        if runner.state().priority_player != thief {
            runner.act(GameAction::PassPriority).unwrap();
        }
        let card_id = runner.state().objects[&vestige].card_id;
        assert!(
            runner
                .act(GameAction::CastSpell {
                    object_id: vestige,
                    card_id,
                    targets: Vec::new(),
                    payment_mode: CastPaymentMode::Auto
                })
                .is_err(),
            "funded former controller cannot take the owner's Warp right"
        );
        runner.advance_to_upkeep();
        runner.advance_to_phase(Phase::PreCombatMain);
        assert_eq!(runner.state().active_player, owner);
        for _ in 0..14 {
            runner.state_mut().players[owner.0 as usize]
                .mana_pool
                .add(ManaUnit::new(
                    ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                ));
        }
        for (source, cost) in [(vestige, 6), (distinct, 8)] {
            let before = runner.state().players[owner.0 as usize].mana_pool.total();
            let action = engine::ai_support::legal_actions(runner.state()).into_iter().find(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == source)).unwrap();
            runner.act(action).unwrap();
            assert!(
                matches!(runner.state().stack.last().unwrap().kind, StackEntryKind::Spell { casting_variant: CastingVariant::Normal, actual_mana_spent, .. } if actual_mana_spent == cost)
            );
            assert_eq!(
                runner.state().players[owner.0 as usize].mana_pool.total(),
                before - usize::try_from(cost).unwrap()
            );
            runner.advance_until_stack_empty();
            assert!(runner.state().objects[&source]
                .casting_permissions
                .is_empty());
        }
        assert!(runner.state().delayed_triggers.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(
            runner.state().stack.is_empty()
                && runner.state().deferred_triggers.is_empty()
                && runner.state().pending_trigger.is_none()
                && runner.state().pending_trigger_order.is_none()
                && runner.state().pending_trigger_entry.is_none()
                && runner.state().pending_trigger_event_batch.is_empty()
                && runner.state().resolution_stack.is_empty()
        );
    }
}

/// CR 614.6 + CR 702.185a: an instruction redirected away from Exile earns no Warp permission.
#[test]
fn paid_warp_redirected_or_no_move_parent_earns_nothing_beside_actual_exile() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, CastingPermission, Effect, ReplacementDefinition,
        ReplacementMode, TargetFilter,
    };
    use engine::types::replacements::ReplacementEvent;
    for caster in [P0, P1] {
        for destination in [Zone::Battlefield, Zone::Graveyard] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let source = scenario
                .add_creature_to_hand_from_oracle(caster, "Anticausal Vestige", 7, 5, VESTIGE)
                .with_mana_cost(ManaCost::generic(6))
                .from_oracle_text_with_keywords(&["Warp"], VESTIGE)
                .id();
            let positive = scenario
                .add_creature_to_hand_from_oracle(caster, "Anticausal Vestige", 7, 5, VESTIGE)
                .with_mana_cost(ManaCost::generic(6))
                .from_oracle_text_with_keywords(&["Warp"], VESTIGE)
                .id();
            let guard = scenario
                .add_enchantment_from_oracle(caster, "Synthetic selected Exile replacement", "")
                .with_replacement_definition(
                    ReplacementDefinition::new(ReplacementEvent::Moved)
                        .valid_card(TargetFilter::SpecificObject { id: source })
                        .destination_zone(Zone::Exile)
                        .mode(ReplacementMode::Optional { decline: None })
                        .execute(AbilityDefinition::new(
                            AbilityKind::Database,
                            Effect::ChangeZone {
                                origin: None,
                                destination,
                                target: TargetFilter::SelfRef,
                                owner_library: false,
                                enter_transformed: false,
                                enters_under: None,
                                enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                                enters_attacking: false,
                                up_to: false,
                                enter_with_counters: Vec::new(),
                                conditional_enter_with_counters: Vec::new(),
                                face_down_profile: None,
                                enters_modified_if: None,
                            },
                        )),
                )
                .id();
            for player in [P0, P1] {
                scenario.with_library_top(
                    player,
                    &[
                        "Private independent draw one",
                        "Private independent draw two",
                        "Reserve",
                    ],
                );
            }
            scenario.with_mana_pool(
                caster,
                (0..12)
                    .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
                    .collect(),
            );
            let mut runner = scenario.build();
            runner.state_mut().active_player = caster;
            runner.state_mut().priority_player = caster;
            runner.state_mut().waiting_for = WaitingFor::Priority { player: caster };
            pay_warp_through_public_actions(&mut runner, caster, source);
            pay_warp_through_public_actions(&mut runner, caster, positive);
            runner.advance_to_end_step();
            runner.advance_until_stack_empty();
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::ReplacementChoice { player, .. } if player == caster)
            );
            let paused = runner.state().pending_replacement.as_ref().unwrap();
            assert!(paused.is_optional);
            assert_eq!(paused.candidates[0].source, guard);
            for restored in warp_restore_forms(runner.state()) {
                let mut runner = engine::game::scenario::GameRunner::from_state(restored);
                let accept = engine::ai_support::legal_actions(runner.state())
                    .into_iter()
                    .find(|action| matches!(action, GameAction::ChooseReplacement { index: 0 }))
                    .unwrap();
                runner.act(accept).unwrap();
                runner.advance_until_stack_empty();
                assert_eq!(runner.state().objects[&source].zone, destination);
                assert!(
                    !runner.state().objects[&source]
                        .casting_permissions
                        .iter()
                        .any(|permission| matches!(
                            permission,
                            CastingPermission::WarpExile { .. }
                        )),
                    "redirected or Remained parent cannot earn Warp"
                );
                assert_eq!(runner.state().objects[&positive].zone, Zone::Exile);
                assert_eq!(
                    runner.state().objects[&positive]
                        .casting_permissions
                        .iter()
                        .filter(|permission| matches!(
                            permission,
                            CastingPermission::WarpExile { .. }
                        ))
                        .count(),
                    1
                );
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                assert!(
                    runner.state().stack.is_empty()
                        && runner.state().deferred_triggers.is_empty()
                        && runner.state().pending_trigger.is_none()
                        && runner.state().pending_trigger_order.is_none()
                        && runner.state().pending_trigger_entry.is_none()
                        && runner.state().pending_trigger_event_batch.is_empty()
                        && runner.state().resolution_stack.is_empty()
                );
            }
        }
    }
}

/// CR 608.2c + CR 614.6: a completed delivery retains its result while its replacement child finishes.
#[test]
fn paid_warp_completed_delivery_retains_its_result_through_actual_post_replacement_child() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, CastingPermission, Effect, QuantityExpr,
        ReplacementDefinition, ReplacementMode, TargetFilter,
    };
    use engine::types::game_state::ZoneMoveCompletion;
    use engine::types::replacements::ReplacementEvent;
    for caster in [P0, P1] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_to_hand_from_oracle(caster, "Anticausal Vestige", 7, 5, VESTIGE)
            .with_mana_cost(ManaCost::generic(6))
            .from_oracle_text_with_keywords(&["Warp"], VESTIGE)
            .id();
        scenario
            .add_enchantment_from_oracle(caster, "Synthetic bounded replacement child", "")
            .with_replacement_definition(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .valid_card(TargetFilter::SpecificObject { id: source })
                    .destination_zone(Zone::Exile)
                    .mode(ReplacementMode::Optional { decline: None })
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Scry {
                            count: QuantityExpr::Fixed { value: 2 },
                            target: TargetFilter::Controller,
                        },
                    )),
            );
        for player in [P0, P1] {
            scenario.with_library_top(
                player,
                &[
                    "Private result-tail first",
                    "Private result-tail second",
                    "Private result-tail reserve",
                    "Private result-tail last",
                ],
            );
        }
        scenario.with_mana_pool(
            caster,
            (0..6)
                .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        runner.state_mut().active_player = caster;
        runner.state_mut().priority_player = caster;
        runner.state_mut().waiting_for = WaitingFor::Priority { player: caster };
        pay_warp_through_public_actions(&mut runner, caster, source);
        runner.advance_to_end_step();
        runner.advance_until_stack_empty();
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::ReplacementChoice { player, .. } if player == caster)
        );
        let original = runner
            .state()
            .pending_zone_change_delivery_from_replacement()
            .unwrap();
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .unwrap();
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::ScryChoice { player, ref cards } if player == caster && cards.len() == 2)
        );
        assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
        let retained = runner
            .state()
            .resolution_stack
            .active_change_zone_or_post_replacement_child()
            .unwrap()
            .pending
            .as_ref()
            .unwrap()
            .paused_current
            .as_ref()
            .unwrap();
        assert_eq!(retained.member, original.member);
        assert_eq!(retained.expected_event, original.expected_event);
        assert_eq!(
            retained.terminal_completion,
            Some(ZoneMoveCompletion::Moved)
        );
        assert!(
            !retained.delivery_events.is_empty(),
            "completed parent retains its own bounded terminal events before child retirement"
        );
        for restored in warp_restore_forms(runner.state()) {
            let mut runner = engine::game::scenario::GameRunner::from_state(restored);
            let WaitingFor::ScryChoice { cards, .. } = &runner.state().waiting_for else {
                panic!("historical shape retains actual child prompt")
            };
            let mut reversed = cards.clone();
            reversed.reverse();
            runner
                .act(GameAction::SelectCards { cards: reversed })
                .unwrap();
            runner.advance_until_stack_empty();
            assert_eq!(
                runner.state().objects[&source]
                    .casting_permissions
                    .iter()
                    .filter(|permission| matches!(permission, CastingPermission::WarpExile { .. }))
                    .count(),
                1
            );
            assert_eq!(
                runner.state().players[caster.0 as usize].hand.len(),
                1,
                "real independent LTB Draw completes after the child"
            );
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(
                runner.state().stack.is_empty()
                    && runner.state().deferred_triggers.is_empty()
                    && runner.state().pending_trigger.is_none()
                    && runner.state().pending_trigger_order.is_none()
                    && runner.state().pending_trigger_entry.is_none()
                    && runner.state().pending_trigger_event_batch.is_empty()
                    && runner.state().resolution_stack.is_empty()
            );
        }
    }
}

/// CR 513.2 + CR 702.185a: a Warp installed after End began exiles on
/// the following turn and the owner must wait until THAT exile turn ends.
/// CR 702.8a + CR 106.4: functioning Flash and fresh normal-cost funding
/// prevent timing or phase-drained mana from masking the turn restriction.
#[test]
fn vestige_late_end_warp_uses_actual_exile_turn_then_owner_pays_normal_six() {
    use engine::types::ability::CastingPermission;
    use engine::types::actions::AlternativeCastDecision;
    use engine::types::game_state::{AlternativeCastKeyword, CastingVariant};

    for owner in [P0, P1] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let flash = scenario
            .add_enchantment_from_oracle(
                owner,
                "Synthetic creature timing authority",
                "You may cast creature spells as though they had flash.",
            )
            .id();
        let vestige = scenario
            .add_creature_to_hand_from_oracle(owner, "Anticausal Vestige", 7, 5, VESTIGE)
            .with_mana_cost(ManaCost::generic(6))
            .with_subtypes(vec!["Eldrazi"])
            .from_oracle_text_with_keywords(&["Warp"], VESTIGE)
            .id();
        let twin = scenario
            .add_creature_to_hand(owner, "Normal creature timing twin", 7, 5)
            .with_mana_cost(ManaCost::generic(6))
            .id();
        for player in [P0, P1] {
            scenario.with_library_top(
                player,
                &[
                    "First private draw",
                    "Second private draw",
                    "Third private draw",
                    "Fourth private draw",
                    "Fifth private draw",
                ],
            );
        }
        let mut runner = scenario.build();
        runner.state_mut().active_player = owner;
        runner.state_mut().priority_player = owner;
        runner.state_mut().waiting_for = WaitingFor::Priority { player: owner };
        assert_eq!(runner.state().objects[&flash].static_definitions.len(), 1);
        let timing = &runner.state().objects[&flash].static_definitions[0];
        assert!(matches!(
            timing.mode,
            engine::types::statics::StaticMode::CastWithKeyword {
                keyword: engine::types::keywords::Keyword::Flash
            }
        ));
        assert!(
            matches!(&timing.affected, Some(engine::types::ability::TargetFilter::Typed(filter)) if filter.type_filters.contains(&engine::types::ability::TypeFilter::Creature) && filter.controller == Some(engine::types::ability::ControllerRef::You))
        );
        runner.advance_to_end_step();
        assert_eq!(runner.state().phase, Phase::End);
        assert!(runner.state().stack.is_empty());
        let cast_turn = runner.state().turn_number;
        for _ in 0..6 {
            runner.state_mut().players[owner.0 as usize]
                .mana_pool
                .add(ManaUnit::new(
                    ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                ));
        }
        if runner.state().priority_player != owner {
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(runner.state().priority_player, owner);
        let cast = engine::ai_support::legal_actions(runner.state()).into_iter().find(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == vestige)).expect("Flash permits the late-End hand cast");
        runner.act(cast).unwrap();
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::AlternativeCastChoice { player, object_id, keyword: AlternativeCastKeyword::Warp, .. } if player == owner && object_id == vestige)
        );
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .unwrap();
        assert!(matches!(
            runner.state().stack.last().unwrap().kind,
            StackEntryKind::Spell {
                casting_variant: CastingVariant::Warp,
                actual_mana_spent: 4,
                ..
            }
        ));
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&vestige].zone, Zone::Battlefield);
        assert_eq!(runner.state().delayed_triggers.len(), 1);
        runner.advance_to_upkeep();
        assert!(runner.state().turn_number > cast_turn);
        assert_eq!(runner.state().objects[&vestige].zone, Zone::Battlefield);
        runner.advance_to_end_step();
        assert_eq!(runner.state().phase, Phase::End);
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&vestige].zone, Zone::Exile);
        let exile_turn = runner.state().turn_number;
        assert!(exile_turn > cast_turn);
        assert_eq!(runner.state().objects[&vestige].casting_permissions.iter().filter(|permission| matches!(permission, CastingPermission::WarpExile { castable_after_turn } if *castable_after_turn == exile_turn)).count(), 1);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        // Fund after all crossings, and actually cast the legal normal twin
        // at the same End-step window before attempting the restricted card.
        for _ in 0..12 {
            runner.state_mut().players[owner.0 as usize]
                .mana_pool
                .add(ManaUnit::new(
                    ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                ));
        }
        if runner.state().priority_player != owner {
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(runner.state().priority_player, owner);
        let legal_twin = engine::ai_support::legal_actions(runner.state()).into_iter().find(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == twin)).expect("functioning Flash plus normal six funding at the exact test window");
        runner.act(legal_twin).unwrap();
        assert!(matches!(
            runner.state().stack.last().unwrap().kind,
            StackEntryKind::Spell {
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 6,
                ..
            }
        ));
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&twin].zone, Zone::Battlefield);
        if runner.state().priority_player != owner {
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(runner.state().priority_player, owner);
        assert_eq!(
            runner.state().players[owner.0 as usize].mana_pool.total(),
            6
        );
        assert!(!engine::ai_support::legal_actions(runner.state()).iter().any(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == vestige)));
        let card_id = runner.state().objects[&vestige].card_id;
        assert!(runner
            .act(GameAction::CastSpell {
                object_id: vestige,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto
            })
            .is_err());
        assert_eq!(runner.state().objects[&vestige].zone, Zone::Exile);
        assert_eq!(
            runner.state().players[owner.0 as usize].mana_pool.total(),
            6
        );
        runner.advance_to_upkeep();
        assert!(runner.state().turn_number > exile_turn);
        runner.advance_to_phase(Phase::PreCombatMain);
        assert_eq!(runner.state().active_player, owner);
        assert_eq!(runner.state().phase, Phase::PreCombatMain);
        for _ in 0..6 {
            runner.state_mut().players[owner.0 as usize]
                .mana_pool
                .add(ManaUnit::new(
                    ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                ));
        }
        let later_cast = engine::ai_support::legal_actions(runner.state()).into_iter().find(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == vestige)).expect("owner later-turn normal cast");
        runner.act(later_cast).unwrap();
        assert!(matches!(
            runner.state().stack.last().unwrap().kind,
            StackEntryKind::Spell {
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 6,
                ..
            }
        ));
        assert_eq!(
            runner.state().players[owner.0 as usize].mana_pool.total(),
            0
        );
        runner.advance_until_stack_empty();
        let object = &runner.state().objects[&vestige];
        assert_eq!(
            (object.zone, object.power, object.toughness),
            (Zone::Battlefield, Some(7), Some(5))
        );
        assert!(object.casting_permissions.is_empty());
        assert!(runner.state().delayed_triggers.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(
            runner.state().stack.is_empty()
                && runner.state().deferred_triggers.is_empty()
                && runner.state().pending_trigger.is_none()
                && runner.state().pending_trigger_order.is_none()
                && runner.state().pending_trigger_entry.is_none()
                && runner.state().pending_trigger_event_batch.is_empty()
                && runner.state().resolution_stack.is_empty()
        );
    }
}

/// CR 702.185a + CR 118.9c: the paid alternative is four, the printed
/// cost is six, and only the alternative creates the delayed exile.
/// CR 113.7a + CR 603.6c: the independent LTB instruction still draws.
#[test]
fn vestige_paid_warp_and_normal_keep_cost_body_delayed_identity_and_independent_draw() {
    use engine::game::scenario::GameRunner;
    use engine::types::ability::CastingPermission;
    use engine::types::actions::AlternativeCastDecision;
    use engine::types::game_state::{AlternativeCastKeyword, CastingVariant, PersistedGameState};
    use engine::types::identifiers::ObjectIncarnationRef;
    use engine::types::keywords::Keyword;

    for caster in [P0, P1] {
        for warped in [false, true] {
            for decline in [false, true] {
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let vestige = scenario
                    .add_creature_to_hand_from_oracle(caster, "Anticausal Vestige", 7, 5, VESTIGE)
                    .with_mana_cost(ManaCost::generic(6))
                    .with_subtypes(vec!["Eldrazi"])
                    .from_oracle_text_with_keywords(&["Warp"], VESTIGE)
                    .id();
                // Decline has an available MV0 permanent; the empty branch
                // contains only nonpermanents. Neither claims the new ceiling.
                let hand_land =
                    decline.then(|| scenario.add_land_to_hand(caster, "Hand land").id());
                for player in [P0, P1] {
                    scenario.with_library_top(player, &["Private draw", "Reserve", "Reserve 2"]);
                }
                scenario.with_mana_pool(
                    caster,
                    (0..6)
                        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
                        .collect(),
                );
                let mut runner = scenario.build();
                // Initial fixture turn; all casting and resolution prompts below
                // are produced by public actions.
                runner.state_mut().active_player = caster;
                runner.state_mut().priority_player = caster;
                runner.state_mut().waiting_for = WaitingFor::Priority { player: caster };
                assert!(runner.state().objects[&vestige]
                    .keywords
                    .contains(&Keyword::Warp(ManaCost::generic(4))));
                let cast = engine::ai_support::legal_actions(runner.state()).into_iter().find(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == vestige)).expect("actual AI-issued paid cast");
                runner.act(cast).unwrap();
                let WaitingFor::AlternativeCastChoice {
                    player,
                    object_id,
                    keyword,
                    normal_cost,
                    alternative_cost,
                    ..
                } = &runner.state().waiting_for
                else {
                    panic!("both affordable costs must be offered")
                };
                assert_eq!(
                    (*player, *object_id, *keyword),
                    (caster, vestige, AlternativeCastKeyword::Warp)
                );
                assert_eq!(*normal_cost, ManaCost::generic(6));
                assert_eq!(*alternative_cost, Some(ManaCost::generic(4)));
                let choice = if warped {
                    AlternativeCastDecision::Alternative
                } else {
                    AlternativeCastDecision::Normal
                };
                let choose = engine::ai_support::legal_actions(runner.state()).into_iter().find(|action| matches!(action, GameAction::ChooseAlternativeCast { choice: selected } if *selected == choice)).expect("actual AI-issued cost response");
                runner.act(choose).unwrap();
                assert!(
                    matches!(runner.state().stack.last().unwrap().kind, StackEntryKind::Spell { casting_variant, actual_mana_spent, .. } if casting_variant == if warped { CastingVariant::Warp } else { CastingVariant::Normal } && actual_mana_spent == if warped { 4 } else { 6 })
                );
                assert_eq!(
                    runner.state().players[caster.0 as usize].mana_pool.total(),
                    if warped { 2 } else { 0 }
                );
                runner.advance_until_stack_empty();
                let permanent = &runner.state().objects[&vestige];
                assert_eq!(
                    (permanent.zone, permanent.power, permanent.toughness),
                    (Zone::Battlefield, Some(7), Some(5))
                );
                let captured = ObjectIncarnationRef::from_object(permanent);
                assert_eq!(runner.state().delayed_triggers.len(), usize::from(warped));
                if warped {
                    let delayed = &runner.state().delayed_triggers[0];
                    assert_eq!((delayed.source_id, delayed.controller), (vestige, caster));
                    assert_eq!(
                        delayed
                            .ability
                            .trigger_source
                            .as_ref()
                            .unwrap()
                            .identity
                            .reference,
                        captured
                    );
                }
                let current =
                    serde_json::from_value(serde_json::to_value(runner.state()).unwrap()).unwrap();
                let trusted = PersistedGameState::capture(runner.state().clone())
                    .into_game_state()
                    .unwrap();
                let mut forms = vec![runner.state().clone(), current, trusted];
                forms.extend(warp_restore_forms(runner.state()).into_iter().skip(3));
                for restored in forms {
                    let mut runner = GameRunner::from_state(restored);
                    let before = runner.state().players[caster.0 as usize].hand.len();
                    let previous_hand = runner.state().players[caster.0 as usize].hand.clone();
                    runner.advance_to_end_step();
                    assert_eq!(runner.state().phase, Phase::End);
                    runner.advance_until_stack_empty();
                    if warped {
                        assert_eq!(runner.state().objects[&vestige].zone, Zone::Exile);
                        assert_eq!(
                            runner.state().objects[&vestige].incarnation,
                            captured.incarnation + 1
                        );
                        assert_eq!(
                            runner.state().players[caster.0 as usize].hand.len(),
                            before + 1
                        );
                        assert!(runner.state().objects[&vestige].casting_permissions.iter().any(|permission| matches!(permission, CastingPermission::WarpExile { castable_after_turn } if *castable_after_turn == runner.state().turn_number)));
                        let drawn = *runner.state().players[caster.0 as usize]
                            .hand
                            .iter()
                            .find(|id| !previous_hand.contains(id))
                            .expect(
                                "mandatory independent Draw publishes an actual new Hand object",
                            );
                        assert_warp_draw_privacy(runner.state(), caster, drawn, vestige);
                        if decline {
                            let WaitingFor::OptionalEffectChoice {
                                player, source_id, ..
                            } = runner.state().waiting_for
                            else {
                                panic!("available optional after mandatory draw")
                            };
                            assert_eq!((player, source_id), (caster, vestige));
                            let response = engine::ai_support::legal_actions(runner.state())
                                .into_iter()
                                .find(|action| {
                                    matches!(
                                        action,
                                        GameAction::DecideOptionalEffect { accept: false }
                                    )
                                })
                                .expect("actual AI-issued decline");
                            runner.act(response).unwrap();
                            runner.advance_until_stack_empty();
                            assert_warp_draw_privacy(runner.state(), caster, drawn, vestige);
                            assert_eq!(
                                runner.state().objects[&hand_land.unwrap()].zone,
                                Zone::Hand
                            );
                        }
                    } else {
                        assert_eq!(runner.state().objects[&vestige].zone, Zone::Battlefield);
                        assert_eq!(runner.state().players[caster.0 as usize].hand.len(), before);
                        assert!(runner.state().objects[&vestige]
                            .casting_permissions
                            .is_empty());
                    }
                    assert!(matches!(
                        runner.state().waiting_for,
                        WaitingFor::Priority { .. }
                    ));
                    assert!(
                        runner.state().stack.is_empty()
                            && runner.state().deferred_triggers.is_empty()
                            && runner.state().pending_trigger.is_none()
                            && runner.state().pending_trigger_order.is_none()
                            && runner.state().pending_trigger_entry.is_none()
                            && runner.state().pending_trigger_event_batch.is_empty()
                            && runner.state().resolution_stack.is_empty()
                    );
                }
            }
        }
    }
}

/// CR 601.2b + CR 601.2c + CR 601.2h: both paid modes bind their own
/// announced target. CR 205.2b + CR 608.2h: count distinct live permanents.
#[test]
fn collision_both_paid_modes_keep_whole_population_and_announced_target() {
    for caster in [P0, P1] {
        let other = if caster == P0 { P1 } else { P0 };
        for mode in [0, 1] {
            for change in [0, 1, 2, 3] {
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let spell = scenario
                    .add_spell_to_hand_from_oracle(caster, "Collision Course", false, COLLISION)
                    .with_mana_cost(ManaCost::Cost {
                        generic: 1,
                        shards: vec![ManaCostShard::White],
                    })
                    .id();
                let bounce = scenario
                    .add_spell_to_hand_from_oracle(
                        caster,
                        "Disperse",
                        true,
                        "Return target nonland permanent to its owner's hand.",
                    )
                    .with_mana_cost(ManaCost::Cost {
                        generic: 1,
                        shards: vec![ManaCostShard::Blue],
                    })
                    .id();
                let ordinary = scenario
                    .add_creature(other, "Ordinary creature", 1, 20)
                    .controlled_by(caster)
                    .id();
                let control = scenario
                    .add_spell_to_hand_from_oracle(
                        other,
                        "Synthetic Vehicle control",
                        true,
                        "Gain control of target artifact.",
                    )
                    .with_mana_cost(ManaCost::Cost {
                        generic: 0,
                        shards: vec![ManaCostShard::Blue],
                    })
                    .id();
                scenario.with_mana_pool(
                    other,
                    vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
                );
                let vehicle = scenario
                    .add_artifact_from_oracle(caster, "Noncreature Vehicle", "")
                    .with_subtypes(vec!["Vehicle"])
                    .id();
                let overlap = scenario
                    .add_creature(caster, "Creature Vehicle", 1, 20)
                    .as_artifact_creature()
                    .with_subtypes(vec!["Vehicle"])
                    .id();
                scenario.add_enchantment_from_oracle(caster, "Nonmatching own permanent", "");
                let other_creature = scenario
                    .add_creature(other, "First creature option", 1, 20)
                    .id();
                let selected_creature = scenario
                    .add_creature(other, "Second creature option", 1, 20)
                    .id();
                let other_artifact = scenario
                    .add_artifact_from_oracle(other, "First artifact option", "")
                    .id();
                let selected_artifact = scenario
                    .add_artifact_from_oracle(other, "Second artifact option", "")
                    .id();
                let foreign = scenario
                    .add_artifact_from_oracle(caster, "Foreign Vehicle", "")
                    .with_subtypes(vec!["Vehicle"])
                    .controlled_by(other)
                    .id();
                scenario.with_mana_pool(
                    caster,
                    [
                        ManaType::White,
                        ManaType::Colorless,
                        ManaType::Blue,
                        ManaType::Colorless,
                    ]
                    .into_iter()
                    .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
                    .collect(),
                );
                let mut runner = scenario.build();
                runner.state_mut().active_player = caster;
                runner.state_mut().priority_player = caster;
                runner.state_mut().waiting_for = WaitingFor::Priority { player: caster };
                let card_id = runner.state().objects[&spell].card_id;
                runner
                    .act(GameAction::CastSpell {
                        object_id: spell,
                        card_id,
                        targets: vec![],
                        payment_mode: CastPaymentMode::Auto,
                    })
                    .unwrap();
                let WaitingFor::ModeChoice {
                    player,
                    modal,
                    pending_cast,
                    unavailable_modes,
                } = &runner.state().waiting_for
                else {
                    panic!("actual Collision mode announcement")
                };
                assert_eq!(*player, caster);
                assert_eq!(
                    (modal.min_choices, modal.max_choices, modal.mode_count),
                    (1, 1, 2)
                );
                assert_eq!(pending_cast.object_id, spell);
                assert!(unavailable_modes.is_empty());
                runner
                    .act(GameAction::SelectModes {
                        indices: vec![mode],
                    })
                    .unwrap();
                let WaitingFor::TargetSelection {
                    player,
                    pending_cast,
                    target_slots,
                    ..
                } = &runner.state().waiting_for
                else {
                    panic!("actual selected-mode targeting")
                };
                assert_eq!(*player, caster);
                assert_eq!(pending_cast.object_id, spell);
                assert_eq!(target_slots.len(), 1);
                let target = if mode == 0 {
                    selected_creature
                } else {
                    selected_artifact
                };
                let mut expected_options = if mode == 0 {
                    vec![ordinary, overlap, other_creature, selected_creature]
                } else {
                    vec![vehicle, overlap, other_artifact, selected_artifact]
                };
                if mode == 1 {
                    expected_options.push(foreign);
                }
                let mut options = target_slots[0]
                    .legal_targets
                    .iter()
                    .map(|target| match target {
                        TargetRef::Object(id) => *id,
                        _ => panic!("object-only mode"),
                    })
                    .collect::<Vec<_>>();
                options.sort();
                expected_options.sort();
                assert_eq!(options, expected_options);
                assert_ne!(
                    target_slots[0].legal_targets.first(),
                    Some(&TargetRef::Object(target))
                );
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .unwrap();
                let entry = runner.state().stack.last().unwrap();
                assert_eq!(
                    (entry.id, entry.source_id, entry.controller),
                    (spell, spell, caster)
                );
                assert_eq!(
                    entry.ability().unwrap().targets,
                    vec![TargetRef::Object(target)]
                );
                assert!(matches!(
                    entry.kind,
                    StackEntryKind::Spell {
                        actual_mana_spent: 2,
                        ..
                    }
                ));
                assert_eq!(
                    runner.state().players[caster.0 as usize].mana_pool.total(),
                    2
                );
                if change == 3 {
                    runner.act(GameAction::PassPriority).unwrap();
                    assert_eq!(runner.state().priority_player, other);
                    runner.cast(control).target_object(vehicle).commit();
                    runner.resolve_top();
                    assert_eq!(runner.state().objects[&vehicle].zone, Zone::Battlefield);
                    assert_eq!(runner.state().objects[&vehicle].controller, other);
                    assert_eq!(runner.state().objects[&vehicle].owner, caster);
                } else if change != 0 {
                    // Public paid bounce changes the population or removes the selected target.
                    let selected = if change == 1 { vehicle } else { target };
                    runner.cast(bounce).target_object(selected).commit();
                    runner.resolve_top();
                    assert_eq!(runner.state().objects[&selected].zone, Zone::Hand);
                }
                let raw = serde_json::to_value(runner.state()).unwrap();
                let current: engine::types::game_state::GameState =
                    serde_json::from_value(raw.clone()).unwrap();
                let trusted =
                    engine::types::game_state::PersistedGameState::capture(runner.state().clone());
                let mut legacy = raw;
                legacy.as_object_mut().unwrap().remove("resolution_stack");
                legacy["resolution_state_version"] = serde_json::json!(1);
                let legacy =
                    serde_json::from_value::<engine::types::resolution::ResolutionStateWire>(
                        legacy,
                    )
                    .unwrap()
                    .into_game_state();
                for restored in [current, trusted.into_game_state().unwrap(), legacy] {
                    let mut runner = engine::game::scenario::GameRunner::from_state(restored);
                    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
                    runner.advance_until_stack_empty();
                    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
                    if mode == 0 {
                        assert_eq!(
                            runner.state().objects[&selected_creature].damage_marked,
                            if change == 2 {
                                0
                            } else if change == 1 || change == 3 {
                                2
                            } else {
                                3
                            }
                        );
                        assert_eq!(runner.state().objects[&other_creature].damage_marked, 0);
                    } else {
                        assert_eq!(
                            runner.state().objects[&selected_artifact].zone,
                            if change == 2 {
                                Zone::Hand
                            } else {
                                Zone::Graveyard
                            }
                        );
                        assert_eq!(
                            runner.state().objects[&other_artifact].zone,
                            Zone::Battlefield
                        );
                    }
                    assert!(matches!(
                        runner.state().waiting_for,
                        WaitingFor::Priority { .. }
                    ));
                    assert!(
                        runner.state().stack.is_empty()
                            && runner.state().deferred_triggers.is_empty()
                            && runner.state().pending_trigger.is_none()
                            && runner.state().pending_trigger_order.is_none()
                            && runner.state().pending_trigger_entry.is_none()
                            && runner.state().pending_trigger_event_batch.is_empty()
                            && runner.state().resolution_stack.is_empty()
                    );
                }
            }
        }
    }
}
