//! Regression test for GitHub issue #1531 — Terra, Herald of Hope routes the
//! "you may pay {2}" combat-damage trigger to the wrong player.
//!
//! Oracle text (combat-damage half):
//!   "Whenever Terra deals combat damage to a player, you may pay {2}.
//!    When you do, return target creature card with power 3 or less from your
//!    graveyard to the battlefield tapped."
//!
//! Bug: the "you may pay {2}" optional decision was offered to the *damaged*
//! player (the trigger's target) instead of to Terra's controller (the "you"
//! of the ability).
//!
//! CR 109.5: "you" in a triggered ability refers to the controller of the
//! ability when it triggered.
//! CR 603.12: "When you do, …" is a reflexive triggered ability — the outer
//! "you may pay {2}" is the optional decision under test here.
//!
//! Companion parser test in `parser/oracle_trigger.rs`
//! (`trigger_you_may_pay_remains_controller`, landed via PR #1769) pins
//! `payer: TargetFilter::Controller`. This test pins the runtime: the
//! `WaitingFor::OptionalEffectChoice` surfaced by Terra's combat-damage trigger
//! prompts Terra's controller, not the damaged opponent.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

use super::rules::AttackTarget;

/// Verified against Scryfall printing of Terra, Herald of Hope. Only the
/// combat-damage trigger is needed for this regression; the Trance upkeep
/// trigger is irrelevant to the routing under test and is omitted to keep the
/// setup focused (the engine has no special-cased text matching — any other
/// trigger would simply add noise on the stack).
const TERRA_COMBAT_TRIGGER: &str = "Whenever Terra deals combat damage to a player, \
    you may pay {2}. When you do, return target creature card with power 3 or less \
    from your graveyard to the battlefield tapped.";

/// CR 109.5 + CR 603.12: Terra (controlled by P1) deals combat damage to P0;
/// the "you may pay {2}" prompt MUST be offered to P1 (Terra's controller),
/// not P0 (the damaged player / trigger target).
#[test]
fn terra_combat_damage_optional_pay_prompts_terras_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P1 controls Terra. Power 3 keeps the test simple — Terra dealing 3
    // combat damage to P0 satisfies the trigger condition (CR 510.1b +
    // CR 603.2 keyed on `DamageDone`/CombatOnly).
    let terra = scenario
        .add_creature(P1, "Terra, Herald of Hope", 3, 3)
        .from_oracle_text(TERRA_COMBAT_TRIGGER)
        .id();

    // Seed P1's graveyard with a creature card so the reflexive sub-clause
    // ("return target creature card with power 3 or less from your graveyard")
    // has a legal target. This keeps the outer optional prompt from being
    // pre-empted by a no-legal-targets short-circuit (CR 608.2b governs
    // target legality at resolution).
    let _graveyard_bear = scenario
        .add_creature_to_graveyard(P1, "Graveyard Bear", 2, 2)
        .id();

    // P0 controls a vanilla blocker presence so the scenario has both players
    // on the battlefield; not strictly required but mirrors the issue
    // reproducer where P0 is a normal opponent.
    let _temmet = scenario.add_creature(P0, "Vanilla Blocker", 1, 1).id();

    let mut runner = scenario.build();

    // Make P1 the active player so P1 can declare attackers this turn.
    // Scenario default is P0 active; swap to P1 to match the bug scenario
    // (P1 controls Terra and is the attacker).
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    // Verify the parsed trigger really did land on Terra (precondition — if
    // the parser regressed there'd be no trigger to fire and the test would
    // be vacuous).
    let trigger_count = runner
        .state()
        .objects
        .get(&terra)
        .unwrap()
        .trigger_definitions
        .len();
    assert!(
        trigger_count >= 1,
        "Terra must have its combat-damage trigger parsed (precondition); \
         got {trigger_count} triggers"
    );

    // Drive combat: P1 declares Terra as an attacker against P0. We can't
    // reuse `run_combat` because that helper hard-codes the defender to P1;
    // here P0 is the defender. Inline the same flow (CR 508 → CR 509 →
    // CR 510) but with P0 as the attack target.
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(terra, AttackTarget::Player(P0))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .expect("DeclareBlockers should succeed (P0 declines to block)");
    }
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }

    // Combat damage resolves (CR 510.2) → Terra's trigger goes on the stack
    // (CR 603.2). Pass priority through the post-damage main-phase rounds
    // until either the optional prompt surfaces (the assertion target) or
    // we exhaust a safety budget.
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 64,
            "exhausted advance budget before reaching OptionalEffectChoice; \
             final waiting_for = {:?}",
            runner.state().waiting_for
        );
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice {
                player, source_id, ..
            } => {
                // CR 109.5: the "you" of Terra's combat-damage trigger is
                // Terra's controller (P1), not the damaged player (P0).
                assert_eq!(
                    *source_id, terra,
                    "the optional prompt must be sourced from Terra"
                );
                assert_eq!(
                    *player, P1,
                    "CR 109.5: 'you may pay {{2}}' must prompt Terra's controller (P1), \
                     NOT the damaged player (P0). prompted = {player:?}"
                );
                return;
            }
            WaitingFor::AssignCombatDamage { .. } => {
                // Single attacker, no blockers — nothing to assign; engine
                // should auto-progress, but if we land here just pass.
                let _ = runner.act(GameAction::PassPriority);
            }
            WaitingFor::Priority { .. } => {
                let _ = runner.act(GameAction::PassPriority);
            }
            other => panic!(
                "unexpected waiting_for while driving Terra's combat-damage trigger: {other:?}"
            ),
        }
    }
}
