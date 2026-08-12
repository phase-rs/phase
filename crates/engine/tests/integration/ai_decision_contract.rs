use engine::ai_support::AiDecisionContract;
use engine::game::engine::{apply_as_current, EngineError};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost,
    AdditionalCostRepeatability, Effect, QuantityExpr, TargetFilter, TargetRef, TypeFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use std::sync::Arc;

const ROUSING_REFRAIN_ORACLE: &str = "Add {R} for each card in target opponent's hand. Until end of turn, you don't lose this mana as steps and phases end. Exile Rousing Refrain with three time counters on it.";

fn rousing_refrain_final_target_state(payable: bool) -> (GameState, Vec<TargetRef>) {
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
        target_slots,
        selection,
        ..
    } = &state.waiting_for
    else {
        panic!("Rousing Refrain must require a target opponent");
    };
    assert_eq!(target_slots.len(), 1, "the target prompt must be final");
    assert_eq!(selection.current_slot, 0, "the only target must be pending");
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
