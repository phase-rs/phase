//! The bounded-witness budget for one `targeted_exchange_verdict` call.
//!
//! R1 is the row that proves the clone-free precondition actually elides work:
//! a root whose source carries no adverse-exchange effect shape must cost zero
//! candidate enumerations and zero reducer replays. P1 and P2 are its permanent
//! positive reach-guards — without them the zero could be a blanket-disabled
//! preview rather than a precondition that fired.

#[cfg(feature = "test-support")]
use engine::ai_support::{
    targeted_exchange_verdict_with_budget, validated_candidate_actions_for_semantic_owner,
    CandidateAction, TargetedExchangeBudget, TargetedExchangeVerdict,
};
#[cfg(feature = "test-support")]
use engine::game::layers::{evaluate_layers, flush_layers};
#[cfg(feature = "test-support")]
use engine::game::scenario::{GameScenario, P0, P1};
#[cfg(feature = "test-support")]
use engine::types::actions::GameAction;
#[cfg(feature = "test-support")]
use engine::types::game_state::GameState;
#[cfg(feature = "test-support")]
use engine::types::identifiers::ObjectId;
#[cfg(feature = "test-support")]
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
#[cfg(feature = "test-support")]
use engine::types::phase::Phase;

/// Verbatim Oracle text, copied from `self_destruct_target_power.rs:18-19`.
#[cfg(feature = "test-support")]
const SELF_DESTRUCT_ORACLE: &str =
    "Target creature you control deals X damage to any other target and X damage to itself, where X is its power.";

/// Verbatim Oracle text for a root carrying no adverse-exchange shape.
#[cfg(feature = "test-support")]
const LIGHTNING_BOLT_ORACLE: &str = "Lightning Bolt deals 3 damage to any target.";

/// Verbatim Oracle text for the Fight class (Prey Upon).
#[cfg(feature = "test-support")]
const PREY_UPON_ORACLE: &str =
    "Target creature you control fights target creature you don't control.";

/// Build a two-player pre-combat main phase board, put `oracle` into P0's hand
/// as a one-red-mana sorcery, and hand back the state plus the spell's id.
#[cfg(feature = "test-support")]
fn board(
    name: &str,
    oracle: &str,
    ai_pt: (i32, i32),
    enemy_pt: (i32, i32),
) -> (GameState, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_vanilla(P0, ai_pt.0, ai_pt.1);
    scenario.add_vanilla(P1, enemy_pt.0, enemy_pt.1);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, false, oracle)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|player| player.id == P0)
        .expect("P0 exists")
        .mana_pool
        .add(ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));
    let mut state = runner.state().clone();
    // R1's third reach-guard (and every row's precondition): the guard's first
    // rail is `layers_dirty.is_dirty() => return true`, which SPENDS the budget.
    // A `GameScenario` that puts permanents onto the battlefield leaves an
    // `EnteredObjects` mark, so flush before measuring.
    flush_layers(&mut state);
    (state, spell)
}

/// Recover the engine-issued root cast candidate for `spell`, exactly as
/// `search.rs` does.
#[cfg(feature = "test-support")]
fn root_candidate(state: &GameState, spell: ObjectId) -> CandidateAction {
    validated_candidate_actions_for_semantic_owner(state, P0)
        .into_iter()
        .find(|candidate| {
            matches!(candidate.action, GameAction::CastSpell { object_id, .. } if object_id == spell)
        })
        .expect("the engine must issue the root cast candidate")
}

/// Assert the three reach-guards R1 depends on, so a zero budget can never be
/// vacuous: `replay_exact_candidate` bails on a missing semantic owner or actor
/// before spending anything, and a dirty lattice spends the budget through the
/// guard's own fall-open rail.
#[cfg(feature = "test-support")]
fn assert_reach_guards(state: &GameState, root: &CandidateAction) {
    assert!(
        root.metadata.semantic_owner.is_some(),
        "reach guard: without a semantic owner `replay_exact_candidate` early-returns and the zero budget is vacuous"
    );
    assert!(
        root.metadata.actor.is_some(),
        "reach guard: without an actor `replay_exact_candidate` early-returns and the zero budget is vacuous"
    );
    assert!(
        !state.layers_dirty.is_dirty(),
        "reach guard: a dirty lattice makes the guard fall open and SPEND the budget — assumption A must hold in this fixture"
    );
}

/// R1 — a root with no adverse-exchange shape costs zero replay and zero
/// enumeration. This is the row that measures the fix.
#[cfg(feature = "test-support")]
#[test]
fn budget_is_zero_for_a_root_without_an_adverse_exchange_shape() {
    let (state, spell) = board("Lightning Bolt", LIGHTNING_BOLT_ORACLE, (2, 2), (3, 3));
    let root = root_candidate(&state, spell);
    assert_reach_guards(&state, &root);

    let (verdict, budget) = targeted_exchange_verdict_with_budget(&state, &root);
    assert_eq!(verdict, TargetedExchangeVerdict::Indeterminate);
    assert_eq!(
        budget,
        TargetedExchangeBudget::default(),
        "the clone-free precondition must elide BOTH the candidate enumeration and the reducer replay for a root that provably cannot be rejected"
    );
}

/// P1 — positive reach-guard for R1: the guard did not blanket-disable the
/// preview. Target-sourced self damage, 2/2 source against a 3/3 recipient.
#[cfg(feature = "test-support")]
#[test]
fn budget_is_spent_and_verdict_rejects_for_a_target_sourced_self_damage_root() {
    let (state, spell) = board("Self-Destruct", SELF_DESTRUCT_ORACLE, (2, 2), (3, 3));
    let root = root_candidate(&state, spell);
    assert_reach_guards(&state, &root);

    let (verdict, budget) = targeted_exchange_verdict_with_budget(&state, &root);
    assert_eq!(
        verdict,
        TargetedExchangeVerdict::Reject,
        "the 2/2 source dies to the 3/3 recipient's damage while the recipient survives"
    );
    assert!(
        budget.candidate_enumerations >= 1,
        "the preview must still run its candidate enumeration for a shape-bearing root"
    );
    assert!(
        budget.replay_clone_applies >= 1,
        "the preview must still clone-and-apply for a shape-bearing root"
    );
    assert!(
        budget.preview_clone_resolves >= 1,
        "the preview must still resolve the bound exchange for a shape-bearing root"
    );
    assert!(budget.nodes >= 1, "C2: the node cap must still be charged");
    assert!(
        budget.branches <= 16,
        "C2: the branch cap must still bound exploration"
    );
}

/// P2 — positive reach-guard, Fight class. AI 2/2 against an enemy 3/3.
#[cfg(feature = "test-support")]
#[test]
fn budget_is_spent_for_a_fight_root() {
    let (state, spell) = board("Prey Upon", PREY_UPON_ORACLE, (2, 2), (3, 3));
    let root = root_candidate(&state, spell);
    assert_reach_guards(&state, &root);

    let (verdict, budget) = targeted_exchange_verdict_with_budget(&state, &root);
    assert_eq!(
        verdict,
        TargetedExchangeVerdict::Reject,
        "the AI's 2/2 dies to the 3/3 it fights while the 3/3 survives"
    );
    assert!(
        budget.replay_clone_applies >= 1,
        "the Fight arm of the leaf shape test must let the preview run"
    );
    assert!(budget.nodes >= 1, "C2: the node cap must still be charged");
}
