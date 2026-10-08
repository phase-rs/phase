//! Production-path regressions for the reducer-backed AI payment witness.

use engine::ai_support::{
    classify_payment_continuation, legal_actions, witness_payment_continuation,
    witness_payment_continuations, witness_payment_continuations_with_counters,
    PaymentContinuationBatchStatus, PaymentContinuationIndeterminate, PaymentContinuationRoot,
    PaymentContinuationState, PaymentContinuationWitnessCounters,
    PAYMENT_CONTINUATION_MAX_REDUCER_ATTEMPTS,
};
use engine::game::engine::apply_as_current_for_simulation;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaProduction, QuantityExpr,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, ManaChoice, StackEntryKind, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use std::sync::Arc;

const DRAW_ORACLE: &str = "Draw a card.";

/// Reach the live CR 601.2g–i manual-payment carrier through the normal cast
/// reducer, rather than manufacturing a pending-cast terminal state.
fn manual_spell_payment_state() -> engine::types::game_state::GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Witnessed Draw", true, DRAW_ORACLE)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, spell, false, Vec::new()))
            .collect(),
    );
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("manual cast must reach a live payment carrier");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    runner.state().clone()
}

/// Reach a real mana-ability colour prompt nested in an announced `{1}{U}`
/// payment. The one-use tap-cost producer emits exactly two mana in any ordered
/// combination of blue and red, so every colour product is reducer-legal but
/// only products containing blue complete the announced spell.
fn flexible_mana_payment_state() -> engine::types::game_state::GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Flexible Witness", true, DRAW_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();
    let source = scenario.add_creature(P0, "Flexible Source", 1, 1).id();
    let mut runner = scenario.build();
    let ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::AnyCombination {
                count: QuantityExpr::Fixed { value: 2 },
                color_options: vec![ManaColor::Blue, ManaColor::Red],
            },
            restrictions: Vec::new(),
            grants: Vec::new(),
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Tap);
    let source_object = runner.state_mut().objects.get_mut(&source).unwrap();
    Arc::make_mut(&mut source_object.abilities).push(ability);
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("manual cast reaches the live mana-payment carrier");
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("the real zero-cost mana ability is activatable while paying");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseManaColor { .. }
    ));
    runner.state().clone()
}

/// Reach a live Improvise carrier where four artifact taps and finalization are
/// required to pay the whole generic cost.
fn four_artifact_improvise_payment_state() -> engine::types::game_state::GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Improvise Witness", true, DRAW_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .with_keyword(Keyword::Improvise)
        .id();
    scenario
        .add_creature(P0, "First Artifact", 1, 1)
        .as_artifact();
    scenario
        .add_creature(P0, "Second Artifact", 1, 1)
        .as_artifact();
    scenario
        .add_creature(P0, "Third Artifact", 1, 1)
        .as_artifact();
    scenario
        .add_creature(P0, "Fourth Artifact", 1, 1)
        .as_artifact();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("manual Improvise cast reaches the live mana-payment carrier");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    runner.state().clone()
}

#[test]
fn witnessed_manual_payment_requires_real_spell_finalization() {
    let state = manual_spell_payment_state();
    let PaymentContinuationState::Affiliated(PaymentContinuationRoot::Spell {
        object_id,
        card_id,
        payer,
    }) = classify_payment_continuation(&state)
    else {
        panic!("production manual payment must classify as its announced spell root");
    };
    let placeholder = state
        .stack
        .iter()
        .find(|entry| entry.id == object_id)
        .expect("CR 601.2a announcement placeholder is present");
    assert!(matches!(
        placeholder.kind,
        StackEntryKind::Spell {
            ability: None,
            actual_mana_spent: 0,
            ..
        }
    ));
    assert!(
        !state.stack_paid_facts.contains_key(&object_id),
        "the announced placeholder has no paid facts before CR 601.2i"
    );

    let completing = legal_actions(&state)
        .into_iter()
        .find_map(|action| witness_payment_continuation(&state, &action))
        .expect("at least one reducer-legal row must complete the announced root");
    let entry = completing
        .state
        .stack
        .iter()
        .find(|entry| entry.id == object_id)
        .expect("the same announced stack entry remains after finalization");
    let StackEntryKind::Spell {
        card_id: finalized_card_id,
        actual_mana_spent,
        ..
    } = &entry.kind
    else {
        panic!("payment witness must retag the announcement as a finalized spell");
    };
    let paid = completing
        .state
        .stack_paid_facts
        .get(&object_id)
        .expect("finalization installs paid facts for the announced entry");
    assert_eq!(*finalized_card_id, card_id);
    assert_eq!(entry.controller, payer);
    assert!(
        *actual_mana_spent > 0,
        "the nonzero production cost was paid"
    );
    assert_eq!(*actual_mana_spent, paid.actual_mana_spent);
    assert!(completing.state.pending_cast.is_none());
}

#[test]
fn cancellation_is_never_a_payment_witness() {
    let state = manual_spell_payment_state();
    assert!(
        witness_payment_continuation(&state, &GameAction::CancelCast).is_none(),
        "CancelCast remains reducer-legal but cannot certify root finalization"
    );
}

#[test]
fn partial_improvise_search_retains_a_four_tap_payment_certificate() {
    let state = four_artifact_improvise_payment_state();
    let taps: Vec<_> = legal_actions(&state)
        .into_iter()
        .filter(|action| matches!(action, GameAction::TapForConvoke { .. }))
        .collect();
    assert_eq!(
        taps.len(),
        4,
        "the live Improvise carrier exposes all four artifacts"
    );

    let batch = witness_payment_continuations(&state, &taps);
    assert_eq!(
        batch.status,
        PaymentContinuationBatchStatus::Indeterminate(
            PaymentContinuationIndeterminate::PartialDirectPaymentSearch
        ),
        "direct payment roots deliberately bypass unrelated roots after retaining a proof"
    );
    assert!(
        batch.successors.iter().any(Option::is_some),
        "a four-tap Improvise path followed by finalization must retain its certificate"
    );
    assert!(
        witness_payment_continuation(&state, &taps[0]).is_some(),
        "the singleton compatibility API must preserve an indeterminate batch's positive certificate"
    );
}

#[test]
fn missing_payment_finalization_baseline_is_indeterminate_not_no_payment() {
    let mut state = manual_spell_payment_state();
    state.stack.clear();

    let batch = witness_payment_continuations(&state, &[GameAction::CancelCast]);
    assert_eq!(
        batch.status,
        PaymentContinuationBatchStatus::Indeterminate(
            PaymentContinuationIndeterminate::MissingFinalizationBaseline
        )
    );
    assert!(batch.successors.iter().all(Option::is_none));
}

#[test]
fn flexible_mana_witness_keeps_every_completing_product() {
    let state = flexible_mana_payment_state();
    let mut actions = legal_actions(&state);
    actions.sort_by(|left, right| left.cmp_stable(right));
    let expected = [
        GameAction::ChooseManaColor {
            choice: ManaChoice::Combination(vec![ManaType::Blue, ManaType::Blue]),
            count: 1,
        },
        GameAction::ChooseManaColor {
            choice: ManaChoice::Combination(vec![ManaType::Blue, ManaType::Red]),
            count: 1,
        },
        GameAction::ChooseManaColor {
            choice: ManaChoice::Combination(vec![ManaType::Red, ManaType::Blue]),
            count: 1,
        },
        GameAction::ChooseManaColor {
            choice: ManaChoice::Combination(vec![ManaType::Red, ManaType::Red]),
            count: 1,
        },
    ];
    assert_eq!(
        actions, expected,
        "the live carrier exposes all four products"
    );

    for action in &actions {
        let mut simulated = state.clone();
        apply_as_current_for_simulation(&mut simulated, action.clone())
            .expect("every generated colour product remains reducer-legal");
    }

    let batch = witness_payment_continuations(&state, &actions);
    assert_eq!(batch.status, PaymentContinuationBatchStatus::Complete);
    let accepted: Vec<_> = batch.successors.into_iter().flatten().collect();
    assert_eq!(
        accepted
            .iter()
            .map(|accepted| &accepted.action)
            .collect::<Vec<_>>(),
        expected[..3].iter().collect::<Vec<_>>(),
        "only the products that leave blue available complete the exact root"
    );
}

#[test]
fn payment_batch_over_capacity_returns_no_partial_certificate_without_applying_roots() {
    let state = flexible_mana_payment_state();
    let root = legal_actions(&state)
        .into_iter()
        .next()
        .expect("live color prompt issues a root action");
    let actions = vec![root; 65];
    let mut counters = PaymentContinuationWitnessCounters::default();
    let batch = witness_payment_continuations_with_counters(&state, &actions, &mut counters);

    assert_eq!(
        batch.status,
        PaymentContinuationBatchStatus::Indeterminate(
            PaymentContinuationIndeterminate::OverRootCapacity
        )
    );
    assert!(batch.successors.iter().all(Option::is_none));
    assert_eq!(counters.root_applies, 0);
    assert_eq!(counters.total_attempts, 0);
}

#[test]
fn payment_batch_reserves_continuation_budget_after_a_full_root_wave() {
    let state = flexible_mana_payment_state();
    let root = legal_actions(&state)
        .into_iter()
        .next()
        .expect("live color prompt issues a root action");
    let actions = vec![root; 64];
    let mut counters = PaymentContinuationWitnessCounters::default();
    let batch = witness_payment_continuations_with_counters(&state, &actions, &mut counters);

    assert_eq!(batch.status, PaymentContinuationBatchStatus::Complete);
    assert!(
        batch.successors.iter().all(Option::is_some),
        "every equivalent root needs the reducer's next payment/finalization step"
    );
    assert_eq!(counters.root_applies, 64);
    assert!(
        counters.continuation_applies >= 64,
        "the full root wave must leave enough shared budget to advance every root"
    );
    assert!(
        counters.total_attempts <= PAYMENT_CONTINUATION_MAX_REDUCER_ATTEMPTS,
        "all root and continuation work remains under the single decision bound"
    );
}
