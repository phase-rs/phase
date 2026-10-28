//! Aggregate player-scoped sacrifice-unless token creation.
//!
//! The Oracle grammar is the reusable clause printed by Acererak the Archlich:
//! each opponent decides in APNAP order, and only after every answer does the
//! source's controller create one Zombie per decline.

use engine::game::elimination::eliminate_player;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ChoiceType, ControllerRef, Effect, EffectKind,
    PlayerFilter, QuantityExpr, SubAbilityLink, TargetFilter, TargetSelectionMode,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{PersistedGameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolved_commands::ResolvedRulesCommand;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const SCOPED_UNLESS_ZOMBIE: &str =
    "For each opponent, you create a 2/2 black Zombie creature token unless they sacrifice a creature.";
const REST_IN_PEACE: &str =
    "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";
const DARKSTEEL_COLOSSUS: &str = "Trample (This creature can deal excess combat damage to the player or planeswalker it's attacking.)\nIndestructible (Damage and effects that say \"destroy\" don't destroy this creature.)\nIf Darksteel Colossus would be put into a graveyard from anywhere, reveal Darksteel Colossus and shuffle it into its owner's library instead.";

fn zombie_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            let object = &runner.state().objects[id];
            object.controller == P0 && object.is_token && object.name == "Zombie"
        })
        .count()
}

fn expect_unless(runner: &GameRunner, player: PlayerId) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::UnlessPayment { player: actual, .. } if actual == player
    ));
}

fn expect_aggregate_payment_finished(runner: &GameRunner) {
    assert!(runner.state().pending_player_scope_unless_payment.is_none());
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "terminal aggregate settlement must not retain a payer prompt"
    );
}

fn pay_sacrifice(runner: &mut GameRunner, creature: ObjectId) {
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("payer may choose to sacrifice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::WardSacrificeChoice { .. }
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![creature],
        })
        .expect("selected creature pays the unless cost");
}

fn give_p0_sorcery_window(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.phase = Phase::PreCombatMain;
    state.turn_number = 2;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
}

fn save_payment(runner: GameRunner) -> GameRunner {
    let saved = PersistedGameState::capture(runner.state().clone());
    let bytes = serde_json::to_vec(&saved).unwrap();
    let saved: PersistedGameState = serde_json::from_slice(&bytes).unwrap();
    GameRunner::from_state(
        saved
            .into_game_state()
            .expect("a genuinely produced payment restores"),
    )
}

fn payment_fixture(
    actor: Option<ControllerRef>,
    replacement: bool,
) -> (GameRunner, ObjectId, [ObjectId; 3]) {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Aggregate Actor", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let first = if replacement {
        scenario.add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE);
        scenario
            .add_creature_from_oracle(P1, "Darksteel Colossus", 11, 11, DARKSTEEL_COLOSSUS)
            .id()
    } else {
        scenario.add_creature(P1, "P1 payment", 1, 1).id()
    };
    let others = [
        scenario.add_creature(P2, "P2 payment", 1, 1).id(),
        scenario.add_creature(PlayerId(3), "P3 payment", 1, 1).id(),
    ];
    let mut runner = scenario.build();
    let object = runner.state_mut().objects.get_mut(&spell).unwrap();
    let definition = &mut std::sync::Arc::make_mut(&mut object.abilities)[0];
    definition.instruction_actor = (actor).map(Into::into);
    let mut tail = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::OriginalController,
        },
    );
    tail.instruction_actor = (Some(ControllerRef::You)).map(Into::into);
    tail.sub_link = SubAbilityLink::SequentialSibling;
    definition.sub_ability = Some(Box::new(tail));
    std::sync::Arc::make_mut(&mut object.base_abilities)[0] = definition.clone();
    give_p0_sorcery_window(&mut runner);
    (runner, spell, [first, others[0], others[1]])
}

fn assert_one_terminal_batch(
    runner: &GameRunner,
    spell: ObjectId,
    count: usize,
    expected_source_zone: Zone,
    events: &[GameEvent],
) {
    assert_eq!(zombie_count(runner), count);
    let births: Vec<_> = runner
        .state()
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|entry| match entry.command.as_ref() {
            Some(ResolvedRulesCommand::TokenCreation(command)) if command.owner == P0 => {
                Some(command.as_ref())
            }
            _ => None,
        })
        .collect();
    assert_eq!(births.len(), count, "exact journaled births");
    let created: Vec<_> = births.iter().map(|birth| birth.object.object_id).collect();
    assert_eq!(
        runner.state().last_created_token_ids,
        created,
        "the terminal instruction publishes exactly its whole token batch"
    );
    for birth in births {
        assert_eq!(runner.state().objects[&birth.object.object_id].owner, P0);
    }
    assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::EffectResolved { kind: EffectKind::Token, source_id, .. } if *source_id == spell)).count(), usize::from(count > 0));
    assert_eq!(
        runner.life(P0),
        21,
        "independent tail runs once even when every payer pays"
    );
    // CR 608.2n + CR 614.6: the spell's final graveyard delivery is still
    // replaced when the fixture has a live graveyard replacement.
    assert_eq!(runner.state().objects[&spell].zone, expected_source_zone);
    assert!(runner.state().stack.is_empty());
    expect_aggregate_payment_finished(runner);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn instruction_actor_aggregate_admits_one_bound_instruction_and_one_terminal_batch() {
    for (actor, expected) in [
        (None, P0),
        (Some(ControllerRef::You), P0),
        (Some(ControllerRef::SpecificPlayer { id: P2 }), P2),
    ] {
        for all_pay in [false, true] {
            let expected_original = actor.as_ref().map(|_| P0);
            let (mut runner, spell, creatures) = payment_fixture(actor.clone(), false);
            runner.cast(spell).resolve();
            let bound = runner
                .state()
                .pending_player_scope_unless_payment
                .as_ref()
                .unwrap()
                .pending_effect
                .clone();
            assert_eq!(bound.controller, expected);
            assert_eq!(bound.original_controller, expected_original);
            assert_eq!(bound.instruction_actor, None);
            assert_eq!(
                bound.sub_ability.as_ref().unwrap().instruction_actor,
                Some(ControllerRef::You.into()),
                "an unstarted independent instruction keeps its own actor"
            );
            assert_eq!(
                bound.scoped_player, None,
                "a payer is not a fabricated actor seat"
            );
            let mut events = Vec::new();
            for (payer, creature) in [P1, P2, PlayerId(3)].into_iter().zip(creatures) {
                expect_unless(&runner, payer);
                assert_eq!(zombie_count(&runner), 0, "no partial token batch");
                assert_eq!(
                    runner
                        .state()
                        .pending_player_scope_unless_payment
                        .as_ref()
                        .unwrap()
                        .pending_effect,
                    bound
                );
                runner = save_payment(runner);
                if all_pay {
                    events.extend(
                        runner
                            .act(GameAction::PayUnlessCost { pay: true })
                            .unwrap()
                            .events,
                    );
                    assert!(
                        matches!(&runner.state().waiting_for, WaitingFor::WardSacrificeChoice { player, pending_effect, .. } if *player == payer && pending_effect == &bound)
                    );
                    runner = save_payment(runner);
                    events.extend(
                        runner
                            .act(GameAction::SelectCards {
                                cards: vec![creature],
                            })
                            .unwrap()
                            .events,
                    );
                } else {
                    events.extend(
                        runner
                            .act(GameAction::PayUnlessCost { pay: false })
                            .unwrap()
                            .events,
                    );
                }
            }
            assert_one_terminal_batch(
                &runner,
                spell,
                if all_pay { 0 } else { 3 },
                Zone::Graveyard,
                &events,
            );
        }
    }
}

#[test]
fn instruction_actor_missing_aggregate_skips_poll_dependents_and_else_but_keeps_tail() {
    for actor in [
        ControllerRef::ChosenPlayer { index: 0 },
        ControllerRef::Opponent,
        // CR 101.3: a payer population does not establish an outer actor seat.
        ControllerRef::ScopedPlayer,
    ] {
        let (mut runner, spell, _) = payment_fixture(Some(actor), false);
        let object = runner.state_mut().objects.get_mut(&spell).unwrap();
        let root = &mut std::sync::Arc::make_mut(&mut object.abilities)[0];
        let tail = root.sub_ability.take();
        let mut dependent = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 10 },
                player: TargetFilter::OriginalController,
            },
        );
        dependent.sub_ability = tail;
        root.sub_ability = Some(Box::new(dependent));
        root.else_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 100 },
                player: TargetFilter::OriginalController,
            },
        )));
        std::sync::Arc::make_mut(&mut object.base_abilities)[0] = root.clone();
        let outcome = runner.cast(spell).resolve();
        outcome.assert_life_delta(P0, 1);
        assert!(outcome
            .state()
            .pending_player_scope_unless_payment
            .is_none());
        assert!(!matches!(
            outcome.final_waiting_for(),
            WaitingFor::UnlessPayment { .. } | WaitingFor::WardSacrificeChoice { .. }
        ));
        assert!(!outcome.events().iter().any(|event| matches!(event, GameEvent::TokenCreated { source_id, .. } if *source_id == spell)));
        outcome.assert_zone(&[spell], Zone::Graveyard);
    }
}

#[test]
fn instruction_actor_aggregate_departure_distinguishes_unanswered_selector_and_begun_payment() {
    for phase in 0..3 {
        let (mut runner, spell, creatures) =
            payment_fixture(Some(ControllerRef::SpecificPlayer { id: P2 }), phase == 2);
        runner.cast(spell).resolve();
        expect_unless(&runner, P1);
        let bound = runner
            .state()
            .pending_player_scope_unless_payment
            .as_ref()
            .unwrap()
            .pending_effect
            .clone();
        if phase > 0 {
            runner.act(GameAction::PayUnlessCost { pay: true }).unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::WardSacrificeChoice { player: P1, .. }
            ));
        }
        if phase == 2 {
            runner
                .act(GameAction::SelectCards {
                    cards: vec![creatures[0]],
                })
                .unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { player: P1, .. }
            ));
            assert!(
                matches!(&runner.state().pending_cost_move_resume, Some(engine::types::game_state::PendingCostMoveResume::WardSacrificePayment { player: P1, pending_effect, .. }) if pending_effect == &bound)
            );
        }
        runner = save_payment(runner);
        let mut events = runner
            .act(GameAction::Concede { player_id: P1 })
            .unwrap()
            .events;
        expect_unless(&runner, P2);
        assert_eq!(zombie_count(&runner), 0);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, GameEvent::PermanentSacrificed { player_id: P1, .. })),
            "departure does not fabricate a sacrifice"
        );
        assert!(runner.state().pending_cost_move_resume.is_none());
        assert!(runner.state().pending_replacement.is_none());
        assert!(!runner.state().has_active_post_replacement_drain());
        assert_eq!(
            runner
                .state()
                .pending_player_scope_unless_payment
                .as_ref()
                .unwrap()
                .pending_effect,
            bound
        );
        events.extend(
            runner
                .act(GameAction::PayUnlessCost { pay: true })
                .unwrap()
                .events,
        );
        events.extend(
            runner
                .act(GameAction::SelectCards {
                    cards: vec![creatures[1]],
                })
                .unwrap()
                .events,
        );
        expect_unless(&runner, PlayerId(3));
        events.extend(
            runner
                .act(GameAction::PayUnlessCost { pay: false })
                .unwrap()
                .events,
        );
        assert_one_terminal_batch(
            &runner,
            spell,
            if phase == 2 { 1 } else { 2 },
            if phase == 2 {
                Zone::Exile
            } else {
                Zone::Graveyard
            },
            &events,
        );
    }
}

#[test]
fn instruction_actor_aggregate_future_departure_preserves_a_living_payment_or_replacement() {
    for phase in 0..3 {
        let (mut runner, spell, creatures) =
            payment_fixture(Some(ControllerRef::SpecificPlayer { id: P2 }), phase == 2);
        runner.cast(spell).resolve();
        expect_unless(&runner, P1);
        if phase > 0 {
            runner.act(GameAction::PayUnlessCost { pay: true }).unwrap();
        }
        if phase == 2 {
            runner
                .act(GameAction::SelectCards {
                    cards: vec![creatures[0]],
                })
                .unwrap();
        }
        let prompt = runner.state().waiting_for.clone();
        let cost_move = runner.state().pending_cost_move_resume.clone();
        let bound = runner
            .state()
            .pending_player_scope_unless_payment
            .as_ref()
            .unwrap()
            .pending_effect
            .clone();
        let mut events = runner
            .act(GameAction::Concede {
                player_id: PlayerId(3),
            })
            .unwrap()
            .events;
        assert_eq!(
            runner.state().waiting_for,
            prompt,
            "another payer leaving cannot bypass this payment"
        );
        assert_eq!(runner.state().pending_cost_move_resume, cost_move);
        let pending = runner
            .state()
            .pending_player_scope_unless_payment
            .as_ref()
            .unwrap();
        assert_eq!(pending.declining_players, vec![PlayerId(3)]);
        assert_eq!(pending.remaining_players, vec![P2]);
        assert_eq!(pending.pending_effect, bound);
        assert_eq!(zombie_count(&runner), 0);
        runner = save_payment(runner);
        if phase == 0 {
            events.extend(
                runner
                    .act(GameAction::PayUnlessCost { pay: true })
                    .unwrap()
                    .events,
            );
        }
        if phase < 2 {
            events.extend(
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![creatures[0]],
                    })
                    .unwrap()
                    .events,
            );
        } else {
            let index = runner
                .state()
                .pending_replacement
                .as_ref()
                .unwrap()
                .candidates
                .iter()
                .position(|candidate| {
                    runner.state().objects[&candidate.source].name == "Rest in Peace"
                })
                .expect("the real Rest in Peace replacement remains available");
            events.extend(
                runner
                    .act(GameAction::ChooseReplacement { index })
                    .unwrap()
                    .events,
            );
        }
        assert_eq!(
            runner.state().objects[&creatures[0]].zone,
            if phase == 2 {
                Zone::Exile
            } else {
                Zone::Graveyard
            }
        );
        assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::PermanentSacrificed { player_id: P1, object_id } if *object_id == creatures[0])).count(), 1);
        expect_unless(&runner, P2);
        events.extend(
            runner
                .act(GameAction::PayUnlessCost { pay: false })
                .unwrap()
                .events,
        );
        assert_one_terminal_batch(
            &runner,
            spell,
            2,
            if phase == 2 {
                Zone::Exile
            } else {
                Zone::Graveyard
            },
            &events,
        );
        assert!(runner.state().pending_cost_move_resume.is_none());
    }
}

#[test]
fn instruction_actor_aggregate_owner_departure_abandons_each_live_payment_phase() {
    for phase in 0..3 {
        let (mut runner, spell, creatures) =
            payment_fixture(Some(ControllerRef::SpecificPlayer { id: P2 }), phase == 2);
        runner.cast(spell).resolve();
        expect_unless(&runner, P1);
        if phase > 0 {
            runner.act(GameAction::PayUnlessCost { pay: true }).unwrap();
        }
        if phase == 2 {
            runner
                .act(GameAction::SelectCards {
                    cards: vec![creatures[0]],
                })
                .unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ));
        }
        let events = runner
            .act(GameAction::Concede { player_id: P0 })
            .unwrap()
            .events;
        assert!(runner.state().pending_player_scope_unless_payment.is_none());
        assert!(runner.state().pending_cost_move_resume.is_none());
        assert!(runner.state().pending_replacement.is_none());
        assert!(!runner.state().has_active_post_replacement_drain());
        assert_eq!(zombie_count(&runner), 0);
        assert!(!events.iter().any(|event| matches!(event, GameEvent::TokenCreated { source_id, .. } | GameEvent::EffectResolved { kind: EffectKind::GainLife, source_id, .. } if *source_id == spell)));
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

#[test]
fn instruction_actor_aggregate_completed_payer_departure_keeps_its_recorded_disposition() {
    for paid in [false, true] {
        let (mut runner, spell, creatures) = payment_fixture(None, false);
        runner.cast(spell).resolve();
        expect_unless(&runner, P1);
        let mut events = runner
            .act(GameAction::PayUnlessCost { pay: paid })
            .unwrap()
            .events;
        if paid {
            events.extend(
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![creatures[0]],
                    })
                    .unwrap()
                    .events,
            );
        }
        expect_unless(&runner, P2);
        events.extend(
            runner
                .act(GameAction::Concede { player_id: P1 })
                .unwrap()
                .events,
        );
        expect_unless(&runner, P2);
        assert_eq!(
            runner
                .state()
                .pending_player_scope_unless_payment
                .as_ref()
                .unwrap()
                .declining_players,
            if paid { vec![] } else { vec![P1] }
        );
        events.extend(
            runner
                .act(GameAction::PayUnlessCost { pay: true })
                .unwrap()
                .events,
        );
        events.extend(
            runner
                .act(GameAction::SelectCards {
                    cards: vec![creatures[1]],
                })
                .unwrap()
                .events,
        );
        expect_unless(&runner, PlayerId(3));
        events.extend(
            runner
                .act(GameAction::PayUnlessCost { pay: false })
                .unwrap()
                .events,
        );
        assert_one_terminal_batch(
            &runner,
            spell,
            if paid { 1 } else { 2 },
            Zone::Graveyard,
            &events,
        );
    }
}

#[test]
fn instruction_actor_aggregate_keeps_a_real_outer_seat_or_prior_player_choice() {
    for outer_scope in [false, true] {
        let (mut runner, spell, _) = payment_fixture(
            Some(if outer_scope {
                ControllerRef::ScopedPlayer
            } else {
                ControllerRef::ChosenPlayer { index: 0 }
            }),
            false,
        );
        let scope = if outer_scope {
            let marker = engine::game::zones::create_object(
                runner.state_mut(),
                engine::types::identifiers::CardId(90_201),
                P2,
                "Synthetic aggregate scope seat".into(),
                Zone::Battlefield,
            );
            let object = runner.state_mut().objects.get_mut(&marker).unwrap();
            object.card_types.core_types = vec![engine::types::card_type::CoreType::Enchantment];
            object.base_card_types = object.card_types.clone();
            assert!(!object
                .card_types
                .core_types
                .contains(&engine::types::card_type::CoreType::Creature));
            assert!(
                object.abilities.is_empty()
                    && object.trigger_definitions.is_empty()
                    && object.replacement_definitions.is_empty()
            );
            Some(PlayerFilter::ControlsCount {
                relation: engine::types::ability::PlayerRelation::All,
                filter: TargetFilter::Typed(
                    engine::types::ability::TypedFilter::default().properties(vec![
                        engine::types::ability::FilterProp::Named {
                            name: "Synthetic aggregate scope seat".into(),
                        },
                    ]),
                ),
                comparator: engine::types::ability::Comparator::GE,
                count: Box::new(QuantityExpr::Fixed { value: 1 }),
            })
        } else {
            None
        };
        let object = runner.state_mut().objects.get_mut(&spell).unwrap();
        let mut aggregate = object.abilities[0].clone();
        aggregate.player_scope = Some(PlayerFilter::All);
        let mut outer = if outer_scope {
            let mut definition = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 0 },
                    player: TargetFilter::Controller,
                },
            );
            definition.player_scope = scope;
            definition
        } else {
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Choose {
                    choice_type: ChoiceType::player(),
                    persist: false,
                    selection: TargetSelectionMode::Chosen,
                },
            )
        };
        outer.sub_ability = Some(Box::new(aggregate));
        std::sync::Arc::make_mut(&mut object.abilities)[0] = outer.clone();
        std::sync::Arc::make_mut(&mut object.base_abilities)[0] = outer;
        let cast = runner.cast(spell);
        if outer_scope {
            cast.resolve();
        } else {
            cast.choose_option("2").resolve();
        }
        let bound = runner
            .state()
            .pending_player_scope_unless_payment
            .as_ref()
            .unwrap_or_else(|| {
                panic!(
                    "outer_scope={outer_scope}, wait={:?}",
                    runner.state().waiting_for
                )
            })
            .pending_effect
            .clone();
        assert_eq!(bound.controller, P2);
        assert_eq!(bound.original_controller, Some(P0));
        assert_eq!(bound.instruction_actor, None);
        assert_eq!(bound.scoped_player, outer_scope.then_some(P2));
        let mut events = Vec::new();
        for payer in [P0, P1, P2, PlayerId(3)] {
            expect_unless(&runner, payer);
            assert_eq!(zombie_count(&runner), 0);
            assert_eq!(
                runner
                    .state()
                    .pending_player_scope_unless_payment
                    .as_ref()
                    .unwrap()
                    .pending_effect,
                bound
            );
            events.extend(
                runner
                    .act(GameAction::PayUnlessCost { pay: false })
                    .unwrap()
                    .events,
            );
        }
        assert_one_terminal_batch(&runner, spell, 4, Zone::Graveyard, &events);
    }
}

#[test]
fn instruction_actor_aggregate_repeats_the_whole_poll_without_consuming_its_future_template() {
    for pay_first in [false, true] {
        let (mut runner, spell, creatures) = payment_fixture(Some(ControllerRef::You), false);
        let object = runner.state_mut().objects.get_mut(&spell).unwrap();
        let mut definition = object.abilities[0].clone();
        definition.repeat_for = Some(QuantityExpr::Fixed { value: 2 });
        std::sync::Arc::make_mut(&mut object.abilities)[0] = definition.clone();
        std::sync::Arc::make_mut(&mut object.base_abilities)[0] = definition;
        runner.cast(spell).resolve();
        let mut events = Vec::new();
        for process in 0..2 {
            for (payer, creature) in [P1, P2, PlayerId(3)].into_iter().zip(creatures) {
                expect_unless(&runner, payer);
                assert_eq!(
                    zombie_count(&runner),
                    if pay_first { 0 } else { process * 3 }
                );
                let pending = runner
                    .state()
                    .pending_player_scope_unless_payment
                    .as_ref()
                    .unwrap();
                assert_eq!(pending.pending_effect.instruction_actor, None);
                assert_eq!(pending.pending_effect.original_controller, Some(P0));
                if process == 0 && pay_first {
                    pay_sacrifice(&mut runner, creature);
                } else {
                    events.extend(
                        runner
                            .act(GameAction::PayUnlessCost { pay: false })
                            .unwrap()
                            .events,
                    );
                }
            }
        }
        assert_eq!(zombie_count(&runner), if pay_first { 3 } else { 6 });
        assert_eq!(runner.life(P0), 22);
        assert_eq!(
            runner.state().objects[&spell].abilities[0].instruction_actor,
            Some(ControllerRef::You.into())
        );
        assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::EffectResolved { kind: EffectKind::Token, source_id, .. } if *source_id == spell)).count(), if pay_first { 1 } else { 2 });
        expect_aggregate_payment_finished(&runner);
        assert!(runner.state().stack.is_empty());
    }
}

#[test]
fn instruction_actor_aggregate_latches_enchanted_player_before_first_payer_sacrifices_its_source() {
    let mut definition = engine::parser::oracle_effect::parse_effect_chain(
        SCOPED_UNLESS_ZOMBIE,
        AbilityKind::Activated,
    );
    definition.instruction_actor = (Some(ControllerRef::EnchantedPlayer)).map(Into::into);
    definition.player_scope = Some(PlayerFilter::All);
    definition.cost = Some(AbilityCost::Tap);
    let AbilityCost::Sacrifice(cost) = &mut definition.unless_pay.as_mut().unwrap().cost else {
        panic!("the admitted one-sacrifice cost");
    };
    cost.target = TargetFilter::Any;
    let mut tail = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::OriginalController,
        },
    );
    tail.sub_link = SubAbilityLink::SequentialSibling;
    definition.sub_ability = Some(Box::new(tail));
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Synthetic Aggregate Aura", "")
        .with_subtypes(vec!["Aura"])
        .with_ability_definition(definition)
        .id();
    for payer in [P1, P2, PlayerId(3)] {
        scenario.add_creature(payer, "Payer permanent", 1, 1);
    }
    let mut runner = scenario.build();
    engine::game::effects::attach::attach_to_player(runner.state_mut(), source, P2);
    runner.activate(source, 0).resolve();
    expect_unless(&runner, P0);
    let bound = runner
        .state()
        .pending_player_scope_unless_payment
        .as_ref()
        .unwrap()
        .pending_effect
        .clone();
    assert_eq!(bound.controller, P2);
    assert_eq!(bound.original_controller, Some(P0));
    assert_eq!(bound.instruction_actor, None);
    let mut events = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .unwrap()
        .events;
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::WardSacrificeChoice { player: P0, .. }
    ));
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: vec![source],
            })
            .unwrap()
            .events,
    );
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&source].attached_to, None);
    for payer in [P1, P2, PlayerId(3)] {
        expect_unless(&runner, payer);
        assert_eq!(
            runner
                .state()
                .pending_player_scope_unless_payment
                .as_ref()
                .unwrap()
                .pending_effect,
            bound,
            "source loss cannot re-resolve the enchanted-player actor"
        );
        events.extend(
            runner
                .act(GameAction::PayUnlessCost { pay: false })
                .unwrap()
                .events,
        );
    }
    assert_one_terminal_batch(&runner, source, 3, Zone::Graveyard, &events);
}

#[test]
fn instruction_actor_aggregate_canonical_simultaneous_departure_settles_each_admitted_unpaid_slot_once(
) {
    // This row enters the public SBA lifecycle owner after a genuine cast and
    // prompt. Single departures above use real Concede actions; there is no
    // multi-player Concede action in the engine protocol.
    for current_leaves in [false, true] {
        let (mut runner, spell, creatures) =
            payment_fixture(Some(ControllerRef::SpecificPlayer { id: P2 }), false);
        runner.cast(spell).resolve();
        expect_unless(&runner, P1);
        let bound = runner
            .state()
            .pending_player_scope_unless_payment
            .as_ref()
            .unwrap()
            .pending_effect
            .clone();
        let leaving = [if current_leaves { P1 } else { P2 }, PlayerId(3)];
        let mut events = Vec::new();
        engine::game::elimination::eliminate_players_simultaneously(
            runner.state_mut(),
            &leaving,
            &mut events,
        );
        let survivor = if current_leaves { P2 } else { P1 };
        expect_unless(&runner, survivor);
        let pending = runner
            .state()
            .pending_player_scope_unless_payment
            .as_ref()
            .unwrap();
        assert_eq!(pending.pending_effect, bound);
        assert!(pending.remaining_players.is_empty());
        assert_eq!(
            pending
                .declining_players
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>(),
            leaving.into_iter().collect()
        );
        assert_eq!(pending.declining_players.len(), 2);
        assert_eq!(zombie_count(&runner), 0);
        events.extend(
            runner
                .act(GameAction::PayUnlessCost { pay: true })
                .unwrap()
                .events,
        );
        events.extend(
            runner
                .act(GameAction::SelectCards {
                    cards: vec![creatures[usize::from(current_leaves)]],
                })
                .unwrap()
                .events,
        );
        assert_one_terminal_batch(&runner, spell, 2, Zone::Graveyard, &events);
    }
}

#[test]
fn instruction_actor_aggregate_does_not_admit_a_player_who_left_before_the_instruction() {
    let (mut runner, spell, creatures) =
        payment_fixture(Some(ControllerRef::SpecificPlayer { id: P2 }), false);
    runner
        .act(GameAction::Concede {
            player_id: PlayerId(3),
        })
        .unwrap();
    runner.cast(spell).resolve();
    expect_unless(&runner, P1);
    let pending = runner
        .state()
        .pending_player_scope_unless_payment
        .as_ref()
        .unwrap();
    assert_eq!(pending.remaining_players, vec![P2]);
    assert!(pending.declining_players.is_empty());
    let mut events = runner
        .act(GameAction::PayUnlessCost { pay: false })
        .unwrap()
        .events;
    expect_unless(&runner, P2);
    assert_eq!(zombie_count(&runner), 0);
    events.extend(
        runner
            .act(GameAction::PayUnlessCost { pay: true })
            .unwrap()
            .events,
    );
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: vec![creatures[1]],
            })
            .unwrap()
            .events,
    );
    assert_one_terminal_batch(&runner, spell, 1, Zone::Graveyard, &events);
}

#[test]
fn instruction_actor_aggregate_terminal_departure_clears_the_poll_without_resuming_tail() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut definition =
        engine::parser::oracle_effect::parse_effect_chain(SCOPED_UNLESS_ZOMBIE, AbilityKind::Spell);
    definition.instruction_actor = (Some(ControllerRef::You)).map(Into::into);
    let mut tail = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::OriginalController,
        },
    );
    tail.sub_link = SubAbilityLink::SequentialSibling;
    definition.sub_ability = Some(Box::new(tail));
    let source = scenario
        .add_spell_to_hand(P0, "Synthetic Terminal Aggregate", false)
        .with_ability_definition(definition)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.cast(source).resolve();
    expect_unless(&runner, P1);
    let events = runner
        .act(GameAction::Concede { player_id: P1 })
        .unwrap()
        .events;
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::GameOver { .. }
    ));
    assert!(runner.state().pending_player_scope_unless_payment.is_none());
    assert_eq!(runner.life(P0), 20);
    assert_eq!(zombie_count(&runner), 0);
    assert!(!events.iter().any(|event| matches!(event, GameEvent::TokenCreated { source_id, .. } | GameEvent::EffectResolved { kind: EffectKind::GainLife, source_id, .. } if *source_id == source)));
}

#[test]
fn instruction_actor_aggregate_terminal_selector_and_replacement_retire_only_the_owned_payment() {
    for replacement in [false, true] {
        for leaving in [P0, P1] {
            let (mut runner, spell, creatures) =
                payment_fixture(Some(ControllerRef::You), replacement);
            runner
                .act(GameAction::Concede {
                    player_id: PlayerId(3),
                })
                .unwrap();
            runner.act(GameAction::Concede { player_id: P2 }).unwrap();
            runner.cast(spell).resolve();
            expect_unless(&runner, P1);
            runner.act(GameAction::PayUnlessCost { pay: true }).unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::WardSacrificeChoice { player: P1, .. }
            ));
            if replacement {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![creatures[0]],
                    })
                    .unwrap();
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { player: P1, .. }
                ));
                assert!(runner.state().pending_cost_move_resume.is_some());
            }
            runner = save_payment(runner);
            let events = runner
                .act(GameAction::Concede { player_id: leaving })
                .unwrap()
                .events;
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::GameOver { .. }
            ));
            assert!(runner.state().pending_player_scope_unless_payment.is_none());
            assert!(runner.state().pending_cost_move_resume.is_none());
            assert!(runner.state().pending_replacement.is_none());
            assert!(!runner.state().has_active_post_replacement_drain());
            assert!(!runner.state().replacement_may_cost_paused);
            assert!(!events.iter().any(|event| matches!(event, GameEvent::TokenCreated { source_id, .. } | GameEvent::EffectResolved { kind: EffectKind::GainLife, source_id, .. } if *source_id == spell)));
        }
    }
}

#[test]
fn scoped_unless_sacrifice_aggregates_mixed_three_player_answers() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p2_creature = scenario.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();

    expect_unless(&runner, P1);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P1 declines");
    expect_unless(&runner, P2);
    pay_sacrifice(&mut runner, p2_creature);

    assert_eq!(
        zombie_count(&runner),
        1,
        "one decline creates one aggregate Zombie"
    );
    assert_eq!(runner.state().objects[&p2_creature].zone, Zone::Graveyard);
    expect_aggregate_payment_finished(&runner);
}

#[test]
fn scoped_unless_sacrifice_creates_one_owned_batch_for_all_declines_and_none_for_all_pay() {
    let mut all_decline = GameScenario::new_n_player(3, 42);
    let decline_spell = all_decline
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let _p1_creature = all_decline.add_creature(P1, "P1 Sacrifice", 1, 1).id();
    let _p2_creature = all_decline.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut decline_runner = all_decline.build();
    give_p0_sorcery_window(&mut decline_runner);
    decline_runner.cast(decline_spell).resolve();
    decline_runner
        .act(GameAction::PayUnlessCost { pay: false })
        .unwrap();
    decline_runner
        .act(GameAction::PayUnlessCost { pay: false })
        .unwrap();
    assert_eq!(zombie_count(&decline_runner), 2);
    expect_aggregate_payment_finished(&decline_runner);

    let mut all_pay = GameScenario::new_n_player(3, 42);
    let pay_spell = all_pay
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p1_creature = all_pay.add_creature(P1, "P1 Sacrifice", 1, 1).id();
    let p2_creature = all_pay.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut pay_runner = all_pay.build();
    give_p0_sorcery_window(&mut pay_runner);
    pay_runner.cast(pay_spell).resolve();
    pay_sacrifice(&mut pay_runner, p1_creature);
    pay_sacrifice(&mut pay_runner, p2_creature);
    assert_eq!(zombie_count(&pay_runner), 0);
}

#[test]
fn scoped_unless_sacrifice_skips_an_eliminated_pending_payer() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let _p1_creature = scenario.add_creature(P1, "P1 Sacrifice", 1, 1).id();
    let _p2_creature = scenario.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();
    expect_unless(&runner, P1);

    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("unanswered payer concedes");
    expect_unless(&runner, P2);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .unwrap();
    assert_eq!(
        zombie_count(&runner),
        2,
        "both admitted unpaid slots contribute, including the departed unanswered payer"
    );
    expect_aggregate_payment_finished(&runner);
}

#[test]
fn scoped_unless_sacrifice_settles_earlier_declines_when_final_payer_is_eliminated() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let _p2_creature = scenario.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();
    expect_unless(&runner, P1);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P1 declines before P2 leaves");
    expect_unless(&runner, P2);

    runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("final unanswered payer concedes");

    assert_eq!(
        zombie_count(&runner),
        2,
        "P1's recorded decline and P2's departed unanswered payment each settle once"
    );
    expect_aggregate_payment_finished(&runner);
}

#[test]
fn scoped_unless_sacrifice_keeps_a_departed_decliners_tokens_owed() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p2_creature = scenario.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();
    expect_unless(&runner, P1);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P1 declines before leaving");
    expect_unless(&runner, P2);

    eliminate_player(runner.state_mut(), P1, &mut Vec::new());
    expect_unless(&runner, P2);
    pay_sacrifice(&mut runner, p2_creature);

    assert_eq!(
        zombie_count(&runner),
        1,
        "P1's completed decline remains owed after P1 leaves"
    );
    expect_aggregate_payment_finished(&runner);
}

#[test]
fn scoped_unless_sacrifice_abandons_when_its_original_controller_leaves() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let _p2_creature = scenario.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();
    expect_unless(&runner, P1);

    eliminate_player(runner.state_mut(), P0, &mut Vec::new());

    assert!(runner.state().pending_player_scope_unless_payment.is_none());
    assert_eq!(zombie_count(&runner), 0);
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "the departed source controller cannot leave an aggregate prompt behind"
    );
}

#[test]
fn scoped_unless_sacrifice_resumes_after_a_graveyard_replacement_choice() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE);
    let colossus = scenario
        .add_creature_from_oracle(P1, "Darksteel Colossus", 11, 11, DARKSTEEL_COLOSSUS)
        .id();
    let _p2_creature = scenario.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();
    expect_unless(&runner, P1);
    pay_sacrifice(&mut runner, colossus);

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("replacement choice resumes the aggregate payment");
    expect_unless(&runner, P2);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .unwrap();
    assert_eq!(zombie_count(&runner), 1);
    expect_aggregate_payment_finished(&runner);
}

#[test]
fn scoped_unless_sacrifice_abandons_a_payers_replacement_choice_when_controller_leaves() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE);
    let colossus = scenario
        .add_creature_from_oracle(P1, "Darksteel Colossus", 11, 11, DARKSTEEL_COLOSSUS)
        .id();
    let _p2_creature = scenario.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();
    expect_unless(&runner, P1);
    pay_sacrifice(&mut runner, colossus);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));

    eliminate_player(runner.state_mut(), P0, &mut Vec::new());

    assert!(runner.state().pending_player_scope_unless_payment.is_none());
    assert!(runner.state().pending_replacement.is_none());
    assert!(!runner.state().replacement_may_cost_paused);
    assert!(
        !runner.state().has_active_post_replacement_drain(),
        "the abandoned aggregate must not retain a replacement continuation"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
