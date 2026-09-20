use engine::ai_support::{candidate_actions, legal_actions_for_viewer, AiDecisionContract};
use engine::game::engine::{apply_as_current, EngineError};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost,
    AdditionalCostRepeatability, Effect, EffectKind, QuantityExpr, ResolvedAbility, TargetFilter,
    TargetRef, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    CastPaymentMode, GameState, TargetEffectDetail, TargetSelectionConstraint, TargetSelectionSlot,
    WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use std::sync::Arc;

#[test]
fn commander_zone_choice_exposes_both_rule_legal_answers_and_settles_either_branch() {
    for arrival_zone in [Zone::Graveyard, Zone::Exile] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let commander = scenario
            .add_creature_to_graveyard(P0, "AI Commander Choice Contract", 2, 2)
            .id();
        scenario.with_commander(commander);
        let mut runner = scenario.build();
        runner.state_mut().format_config.command_zone = true;
        let mut events = Vec::new();
        engine::game::zones::move_to_zone(runner.state_mut(), commander, arrival_zone, &mut events);
        engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);

        let state = runner.state();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::CommanderZoneChoice {
                player: P0,
                commander_id,
                current_zone,
            } if commander_id == commander && current_zone == arrival_zone
        ));
        let contract = AiDecisionContract::issue(state, P0);
        let accept = GameAction::DecideOptionalEffect { accept: true };
        let decline = GameAction::DecideOptionalEffect { accept: false };
        assert!(contract.contains_action(state, &accept));
        assert!(contract.contains_action(state, &decline));
        assert!(contract.permits(state, P0, &accept));
        assert!(contract.permits(state, P0, &decline));
        let candidates = candidate_actions(state);
        assert_eq!(
            candidates
                .iter()
                .filter(|candidate| matches!(
                    candidate.action,
                    GameAction::DecideOptionalEffect { .. }
                ))
                .count(),
            2,
            "the engine domain must issue both commander answers"
        );
        assert_eq!(
            legal_actions_for_viewer(state, P0)
                .0
                .into_iter()
                .filter(|action| matches!(action, GameAction::DecideOptionalEffect { .. }))
                .count(),
            2
        );
        assert!(legal_actions_for_viewer(state, P1).0.is_empty());

        let mut declined = state.clone();
        apply_as_current(&mut declined, decline).expect("decline must remain reducer-legal");
        assert_eq!(declined.objects[&commander].zone, arrival_zone);
        assert!(declined.commander_declined_zone_return.contains(&commander));
        assert!(matches!(
            declined.waiting_for,
            WaitingFor::Priority { player: P0 }
        ));

        let mut accepted = state.clone();
        apply_as_current(&mut accepted, accept).expect("accept must remain reducer-legal");
        assert_eq!(accepted.objects[&commander].zone, Zone::Command);
        assert!(matches!(
            accepted.waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
    }
}

const ROUSING_REFRAIN_ORACLE: &str = "Add {R} for each card in target opponent's hand. Until end of turn, you don't lose this mana as steps and phases end. Exile Rousing Refrain with three time counters on it.";

fn rousing_refrain_target_state(payable: bool) -> (GameState, Vec<TargetRef>) {
    let p3 = PlayerId(3);
    let mut scenario = GameScenario::new_n_player(4, 7114);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [PlayerId(0), PlayerId(1), PlayerId(2)] {
        scenario.with_cards_in_hand(player, &["Opponent card"]);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(p3, "Rousing Refrain", false, ROUSING_REFRAIN_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 3,
        })
        .id();
    if payable {
        scenario.with_mana_pool(
            p3,
            (0..5)
                .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
                .collect(),
        );
    }

    let mut state = scenario.build().state().clone();
    state.active_player = p3;
    state.priority_player = p3;
    state.waiting_for = WaitingFor::Priority { player: p3 };
    apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: spell,
            card_id: CardId(spell.0),
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        },
    )
    .expect("Rousing Refrain must reach its final target prompt");

    let WaitingFor::TargetSelection {
        target_slots: _,
        selection,
        ..
    } = &state.waiting_for
    else {
        panic!("Rousing Refrain must require a target opponent");
    };
    assert_eq!(
        selection.current_slot, 0,
        "the first target must be pending"
    );
    let targets = selection.current_legal_targets.clone();
    assert_eq!(
        targets,
        vec![
            TargetRef::Player(PlayerId(0)),
            TargetRef::Player(PlayerId(1)),
            TargetRef::Player(PlayerId(2)),
        ],
        "reach guard: every opponent is a legal visible target"
    );
    (state, targets)
}

fn rousing_refrain_final_target_state(payable: bool) -> (GameState, Vec<TargetRef>) {
    let (state, targets) = rousing_refrain_target_state(payable);
    let WaitingFor::TargetSelection { target_slots, .. } = &state.waiting_for else {
        panic!("Rousing Refrain must remain in target selection");
    };
    assert_eq!(target_slots.len(), 1, "the target prompt must be final");
    (state, targets)
}

/// CR 601.2h: manual payment preserves the announced spell on the stack until
/// the caster either pays its locked total cost or cancels the cast.
fn manual_mana_payment_state(payable: bool) -> GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Manual Payment Contract Spell", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    if payable {
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            )],
        );
    }

    let mut runner = scenario.build();
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: CardId(spell.0),
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("manual cast must enter its mana-payment step");

    let state = runner.state().clone();
    assert!(
        matches!(
            &state.waiting_for,
            WaitingFor::ManaPayment {
                player: P0,
                convoke_mode: None,
            }
        ),
        "reach guard: a manual cast with a nonzero cost must pause for mana payment"
    );
    assert!(
        state
            .pending_cast
            .as_ref()
            .is_some_and(|pending| pending.object_id == spell),
        "reach guard: the real cast must retain its pending payment authority"
    );
    assert!(
        state
            .stack
            .iter()
            .any(|entry| entry.id == spell && entry.source_id == spell),
        "reach guard: the announced spell must have a live stack entry during payment"
    );
    state
}

/// CR 601.2h: a cast cannot be finalized with a partial payment, while a
/// payable manual cast may finalize from the same `WaitingFor::ManaPayment` prompt.
#[test]
fn decision_contract_validates_manual_mana_payment_finalization() {
    let unpayable = manual_mana_payment_state(false);
    let mut rejected_finalization = unpayable.clone();
    assert!(matches!(
        apply_as_current(&mut rejected_finalization, GameAction::PassPriority),
        Err(EngineError::ActionNotAllowed(message)) if message == "Cannot pay mana cost"
    ));

    let contract = AiDecisionContract::issue(&unpayable, P0);
    assert!(
        !contract.contains_action(&unpayable, &GameAction::PassPriority),
        "a ManaPayment pass must be issued only when the reducer can finalize payment"
    );
    assert!(
        contract.contains_action(&unpayable, &GameAction::CancelCast),
        "an unpaid cast must retain the reducer-approved cancellation escape"
    );
    assert!(
        contract.permits(&unpayable, P0, &GameAction::CancelCast),
        "the issued cancellation escape must be accepted at the contract boundary"
    );
    let mut canceled = unpayable.clone();
    apply_as_current(&mut canceled, GameAction::CancelCast)
        .expect("the reducer must accept cancellation of the live unpaid cast");

    let payable = manual_mana_payment_state(true);
    let payable_contract = AiDecisionContract::issue(&payable, P0);
    assert!(
        payable_contract.contains_action(&payable, &GameAction::PassPriority),
        "a payable manual cast must retain its reducer-completable payment pass"
    );
    assert!(
        payable_contract.permits(&payable, P0, &GameAction::PassPriority),
        "the payable finalization pass must be accepted at the contract boundary"
    );
    let mut finalized = payable.clone();
    apply_as_current(&mut finalized, GameAction::PassPriority)
        .expect("the reducer must accept finalization of the payable live cast");
}

#[test]
fn decision_contract_filters_final_targets_that_cannot_complete_payment() {
    let p3 = PlayerId(3);
    let (state, targets) = rousing_refrain_final_target_state(false);

    for target in &targets {
        let error = apply_as_current(
            &mut state.clone(),
            GameAction::ChooseTarget {
                target: Some(target.clone()),
            },
        )
        .expect_err("reach guard: final target selection must attempt the real payment");
        assert!(
            error.to_string().contains("Cannot pay mana cost"),
            "the unpayable final target must fail at payment, got {error}"
        );
    }

    let contract = AiDecisionContract::issue(&state, p3);
    assert_eq!(
        contract
            .candidates
            .iter()
            .map(|candidate| &candidate.action)
            .collect::<Vec<_>>(),
        vec![&GameAction::CancelCast],
        "the issued domain must contain only the reducer-completable cancellation"
    );

    let (payable, targets) = rousing_refrain_final_target_state(true);
    let payable_contract = AiDecisionContract::issue(&payable, p3);
    for target in targets {
        let action = GameAction::ChooseTarget {
            target: Some(target),
        };
        assert!(
            payable_contract.contains_action(&payable, &action),
            "a final target remains issued when the same spell can pay its cost"
        );
    }
}

#[test]
fn decision_contract_validates_a_target_before_a_dynamically_empty_optional_tail() {
    let p3 = PlayerId(3);
    let (mut state, _) = rousing_refrain_target_state(false);

    let WaitingFor::TargetSelection {
        target_slots,
        pending_cast,
        selection,
        ..
    } = &mut state.waiting_for
    else {
        panic!("Rousing Refrain must remain in target selection");
    };
    assert_eq!(
        selection.current_slot, 0,
        "the first target must be pending"
    );
    pending_cast.target_constraints = vec![TargetSelectionConstraint::DifferentTargetPlayers];
    let mut trailing_ability = ResolvedAbility::new(
        Effect::TargetOnly {
            target: TargetFilter::SpecificPlayer { id: PlayerId(0) },
        },
        Vec::new(),
        pending_cast.ability.source_id,
        p3,
    );
    trailing_ability.optional_targeting = true;
    pending_cast.ability.sub_ability = Some(Box::new(trailing_ability));
    target_slots.push(TargetSelectionSlot {
        legal_targets: vec![TargetRef::Player(PlayerId(0))],
        optional: true,
        chooser: None,
        effect_kind: EffectKind::NoOp,
        effect_detail: TargetEffectDetail::None,
    });
    assert_eq!(
        target_slots[1].legal_targets,
        vec![TargetRef::Player(PlayerId(0))],
        "the trailing slot starts with targets and only becomes empty after prior choices"
    );
    assert_eq!(
        selection.current_legal_targets,
        vec![
            TargetRef::Player(PlayerId(0)),
            TargetRef::Player(PlayerId(1)),
            TargetRef::Player(PlayerId(2)),
        ],
        "the first target remains independently legal"
    );

    let action = GameAction::ChooseTarget {
        target: Some(TargetRef::Player(PlayerId(0))),
    };
    let error = apply_as_current(&mut state.clone(), action.clone())
        .expect_err("the dynamically empty tail is auto-skipped before payment");
    assert!(
        error.to_string().contains("Cannot pay mana cost"),
        "the auto-skipped tail must still reach payment, got {error}"
    );

    let contract = AiDecisionContract::issue(&state, p3);
    assert_eq!(
        contract
            .candidates
            .iter()
            .map(|candidate| &candidate.action)
            .collect::<Vec<_>>(),
        vec![
            &GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(1))),
            },
            &GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(2))),
            },
            &GameAction::CancelCast,
        ],
        "only the target that completes through the dynamically auto-skipped tail must be excluded"
    );
}

#[test]
fn decision_contract_keeps_reducer_completable_final_activation_targets() {
    fn state_with_activation_mana(payable: bool) -> GameState {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature(P0, "Targeting activation", 1, 1)
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
                .cost(AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                }),
            )
            .id();
        if payable {
            scenario.with_mana_pool(
                P0,
                vec![ManaUnit::new(
                    ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                )],
            );
        }
        let mut state = scenario.build().state().clone();
        apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            },
        )
        .expect("activation must reach its final target prompt");
        state
    }

    let action = GameAction::ChooseTarget {
        target: Some(TargetRef::Player(P1)),
    };
    let state = state_with_activation_mana(false);
    let error = apply_as_current(&mut state.clone(), action.clone())
        .expect_err("reach guard: final activation target must attempt its unpaid mana cost");
    assert!(
        error.to_string().contains("Cannot pay mana cost"),
        "the unpayable final activation target must fail at payment, got {error}"
    );
    let contract = AiDecisionContract::issue(&state, P0);
    assert!(
        !contract.contains_action(&state, &action),
        "an unpayable final activation target must not be issued"
    );

    let state = state_with_activation_mana(true);
    let contract = AiDecisionContract::issue(&state, P0);
    assert!(
        contract.contains_action(&state, &action),
        "a reducer-completable final activation target must remain issued"
    );
}

/// Issue #7109: the decision contract must not offer an optional payment
/// whose resulting deferred target set has no legal assignment.
#[test]
fn decision_contract_filters_optional_cost_that_leaves_no_legal_targets() {
    let player = PlayerId(0);
    let mut state = GameState::new_two_player(42);
    state.phase = Phase::PreCombatMain;
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };

    let spell = create_object(
        &mut state,
        CardId(7109),
        player,
        "Kicker Target Spell".to_string(),
        Zone::Hand,
    );
    let creature = create_object(
        &mut state,
        CardId(7110),
        PlayerId(1),
        "Creature".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&creature)
        .expect("created creature must exist")
        .card_types
        .core_types
        .push(CoreType::Creature);
    {
        let spell_object = state
            .objects
            .get_mut(&spell)
            .expect("created spell must exist");
        spell_object.card_types.core_types.push(CoreType::Instant);
        spell_object.mana_cost = ManaCost::generic(0);
        spell_object.additional_cost = Some(AdditionalCost::Kicker {
            costs: vec![AbilityCost::Mana {
                cost: ManaCost::generic(1),
            }],
            repeatability: AdditionalCostRepeatability::Once,
        });
        Arc::make_mut(&mut spell_object.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Destroy {
                    target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                    cant_regenerate: false,
                },
            )
            .sub_ability(
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Destroy {
                        target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                        cant_regenerate: false,
                    },
                )
                .condition(AbilityCondition::AdditionalCostPaidInstead),
            ),
        );
    }
    state.players[0].mana_pool.add(ManaUnit::new(
        ManaType::Green,
        ObjectId(7109),
        false,
        vec![],
    ));

    apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: spell,
            card_id: CardId(7109),
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        },
    )
    .expect("the cast must reach its target-dependent kicker choice");

    assert!(
        matches!(
            &state.waiting_for,
            WaitingFor::OptionalCostChoice { pending_cast, .. }
                if pending_cast.deferred_target_selection
        ),
        "reach-guard: the production cast must defer targets until the kicker choice"
    );
    assert!(
        matches!(
            apply_as_current(
                &mut state.clone(),
                GameAction::DecideOptionalCost { pay: false },
            ),
            Err(EngineError::ActionNotAllowed(message))
                if message == "No legal targets available"
        ),
        "reach-guard: declining kicker must reproduce the rejected targetless cast"
    );

    let contract = AiDecisionContract::issue(&state, player);
    assert!(
        contract.candidates.iter().any(|candidate| {
            matches!(
                candidate.action,
                GameAction::DecideOptionalCost { pay: true }
            )
        }),
        "the target-enabling kicker payment must be issued"
    );
    assert!(
        !contract.candidates.iter().any(|candidate| {
            matches!(
                candidate.action,
                GameAction::DecideOptionalCost { pay: false }
            )
        }),
        "the targetless declining choice must not be issued"
    );
}
