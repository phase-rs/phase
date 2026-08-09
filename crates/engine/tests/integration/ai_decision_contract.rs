use engine::ai_support::AiDecisionContract;
use engine::game::engine::{apply_as_current, EngineError};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost,
    AdditionalCostRepeatability, Effect, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use std::sync::Arc;

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
