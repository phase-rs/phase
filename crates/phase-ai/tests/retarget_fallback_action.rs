//! CR 115.7 — `phase_ai::search::fallback_action` must answer a parked
//! `RetargetChoice` with a submission the reducer accepts.
//!
//! The fallback is the AI's last resort when scoring produces nothing, so a
//! fallback that proposes an illegal submission is a freeze by a second route.
//! Both rows drive `fallback_action` itself — the seam these rows exist to
//! carry. That is an ENTRY POINT, not an isolation: `fallback_action` ends in
//! `gate(action)`, which filters through `contract.contains_action`, and the
//! contract builds its candidates from the same engine enumerator that reaches
//! `retarget_actions` (`ai_support::context`'s `AiDecisionContract::issue` ->
//! `candidate_actions_for_semantic_owner_with_probe`). Its `issued` helper reads
//! `contract.candidates` even more directly. So these rows assert a
//! CO-DEPENDENCE property — the fallback answers the prompt only when the
//! generator and the fallback agree — and reverting EITHER side alone yields
//! `None` and fails the `is_some()` guard. They do not isolate the fallback from
//! the generator, and must not be cited as if they did.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    mana_multi_role, ControllerRef, Effect, ManaProduction, ManaTargetRole, QuantityExpr,
    ResolvedAbility, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    CastingVariant, RetargetScope, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use phase_ai::config::{create_config, AiDifficulty, Platform};

const LIGHTNING_BOLT_ORACLE: &str = "Lightning Bolt deals 3 damage to any target.";

/// `fallback_action` gates its result on the decision contract, which is exact
/// set membership against the contract's own candidates — so the contract MUST
/// be issued for the prompt's own player, or the row degrades into asserting
/// nothing.
fn fallback_for_prompt(runner: &GameRunner) -> Option<GameAction> {
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let contract = engine::ai_support::AiDecisionContract::issue(runner.state(), P0);
    phase_ai::search::fallback_action(runner.state(), &config, &contract)
}

/// Row 2b — CR 115.7a: the fallback must propose ANOTHER legal target. At base
/// it returns `current_targets`, which `apply_retarget` rejects whenever the
/// current target has dropped out of the pool.
#[test]
fn fallback_retarget_action_is_legal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let alternative = scenario.add_creature(P0, "Goblin", 1, 1).id();
    scenario.add_creature(P0, "Bystander", 1, 1);
    let victim = scenario.add_creature(P1, "Bear", 2, 2).id();

    let mut runner = scenario.build();

    // The same fixture shape as the engine-side row: a single-target spell on
    // the stack whose current target is deliberately absent from the pool.
    let parsed = parse_oracle_text(
        LIGHTNING_BOLT_ORACLE,
        "Lightning Bolt",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let bolt_id = create_object(
        runner.state_mut(),
        CardId(77),
        P1,
        "Lightning Bolt".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&bolt_id)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Instant];
    let bolt_ability = ResolvedAbility::new(
        parsed.abilities[0].effect.as_ref().clone(),
        vec![TargetRef::Object(victim)],
        bolt_id,
        P1,
    );
    runner.state_mut().stack.push_back(StackEntry {
        id: bolt_id,
        source_id: bolt_id,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(77),
            ability: Some(Box::new(bolt_ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    // A directly parked prompt: no pending cast, so `fallback_action`'s
    // cancel-cast escape cannot fire before the `RetargetChoice` arm.
    runner.state_mut().waiting_for = WaitingFor::RetargetChoice {
        player: P0,
        stack_entry_index: 0,
        scope: RetargetScope::Single,
        current_targets: vec![TargetRef::Object(victim)],
        legal_new_targets: vec![TargetRef::Object(alternative)],
    };

    let action = fallback_for_prompt(&runner);

    // Positive reach-guard, both halves: `Some` alone would also be satisfied by
    // a `CancelCast` returned from an escape that ran before the arm.
    assert!(action.is_some(), "reach guard: the fallback must answer");
    assert!(
        matches!(action, Some(GameAction::RetargetSpell { .. })),
        "reach guard: the RetargetChoice arm must be the one that answered, got {action:?}"
    );

    // Discriminating: at base the action is `RetargetSpell { [victim] }`, which
    // the reducer rejects with "chosen target not in legal alternatives".
    runner
        .act(action.unwrap())
        .expect("the fallback's action must be accepted by the reducer");
}

/// Row 2f — CR 115.7a, at the `phase-ai` seam: a flat pool member legal only for
/// another slot must never be proposed, because `apply_retarget` re-checks each
/// changed submission against its own slot filter.
#[test]
fn fallback_multi_role_retarget_action_is_slot_legal() {
    let mut runner = GameScenario::new().build();

    let role = ManaTargetRole::Both {
        // Slot 0 (recipient, surfaced first): only an opponent of P0, i.e. P1.
        recipient: TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
        // Slot 1 (count source): any player.
        count_source: TargetFilter::Player,
    };
    let source = create_object(
        runner.state_mut(),
        CardId(901),
        P0,
        "Multi-Role Mana Source".to_string(),
        Zone::Battlefield,
    );
    let entry_id = create_object(
        runner.state_mut(),
        CardId(901),
        P0,
        "Multi-Role Mana Ability".to_string(),
        Zone::Stack,
    );

    // Slot 0 holds P1, not P0 — so proposing `[P0]` would be a genuine change
    // into a slot it is illegal for, rather than an exempt non-change.
    let current_targets = vec![TargetRef::Player(P1), TargetRef::Player(P0)];
    let ability = ResolvedAbility::new(
        Effect::Mana {
            produced: ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 1 },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: Some(role),
        },
        current_targets.clone(),
        source,
        P0,
    );
    runner.state_mut().stack.push_back(StackEntry {
        id: entry_id,
        source_id: source,
        controller: P0,
        kind: StackEntryKind::ActivatedAbility {
            source_id: source,
            ability: Box::new(ability),
        },
    });

    // Both structural reach-guards: without the entry, `retarget_actions`'
    // `is_none_or` passes every candidate unfiltered and `apply_retarget` skips
    // its per-slot stage, so this row would be vacuous in both directions.
    assert!(
        runner.state().stack[0].ability().is_some(),
        "reach guard: stack index 0 must carry the ability under test"
    );
    assert!(
        mana_multi_role(&runner.state().stack[0].ability().unwrap().effect).is_some(),
        "reach guard: the node must be inside the per-slot admitted class"
    );

    runner.state_mut().waiting_for = WaitingFor::RetargetChoice {
        player: P0,
        stack_entry_index: 0,
        scope: RetargetScope::Single,
        current_targets,
        legal_new_targets: vec![TargetRef::Player(P0), TargetRef::Player(P1)],
    };

    let action = fallback_for_prompt(&runner);

    assert!(action.is_some(), "reach guard: the fallback must answer");
    assert!(
        matches!(action, Some(GameAction::RetargetSpell { .. })),
        "reach guard: the RetargetChoice arm must be the one that answered, got {action:?}"
    );

    // Discriminating: `[P0]` is in the flat pool but legal only for slot 1, so
    // it must never be the proposal; only the slot-0-legal `[P1]` may be.
    assert_eq!(
        action,
        Some(GameAction::RetargetSpell {
            new_targets: vec![TargetRef::Player(P1)],
        }),
        "CR 115.7a: the fallback must not propose a pool member legal only for \
         another slot"
    );

    // Discriminating: at base the action is `[P1, P0]` (length 2), which the
    // `Single` arm rejects outright.
    runner
        .act(action.unwrap())
        .expect("the fallback's action must be accepted by the reducer");
}
