//! Regression: same-controller combat-damage triggers must surface their
//! CR 603.3b ordering prompt instead of being stranded in
//! `pending_trigger_order` (the turn-18 Coastal Piracy hang).
//!
//! Bug: when 2+ triggers controlled by the same player fired simultaneously in
//! the combat damage step, `combat_damage::resolve_combat_damage` ran
//! `process_triggers`, which populated `state.pending_trigger_order` and set
//! `state.waiting_for` to `WaitingFor::OrderTriggers`. The `Phase::CombatDamage`
//! arm of `auto_advance` then checked `!state.stack.is_empty()` — but the
//! queued triggers live in `pending_trigger_order`, NOT on the stack — found an
//! empty stack, and called `advance_phase`, silently discarding the ordering
//! prompt. The triggers never reached the stack and the game hung waiting for an
//! ordering choice it had thrown away.
//!
//! Fix (Edit 1, `turns.rs`): the `CombatDamage` arm now surfaces a live
//! `WaitingFor::OrderTriggers` prompt (mirroring `finish_declare_attackers` in
//! `engine_combat.rs`) before the `!state.stack.is_empty()` guard can advance
//! past it.
//!
//! Fix (Edit 2, `turns.rs::process_phase_triggers`): the ordering prompt is now
//! reconstructed from the AUTHORITATIVE `pending_trigger_order` state rather than
//! cloned from a possibly-stale `state.waiting_for`.
//!
//! CR references (verified against `docs/MagicCompRules.txt`):
//!   - CR 603.3b: when multiple abilities trigger before a player would receive
//!     priority, each player orders their own simultaneous triggers (the
//!     OrderTriggers prompt) before they go on the stack.
//!   - CR 510.1b / CR 510.2: an unblocked creature deals combat damage equal to
//!     its power to the player it is attacking, in the combat damage step.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

use super::rules::run_combat;

/// "Whenever this creature deals combat damage to a player, you may draw a
/// card." — a per-creature combat-damage trigger. Two creatures with the same
/// controller carrying this trigger fire two simultaneous, same-controller
/// triggers when both connect, which is the exact CR 603.3b ordering case.
const DRAW_ON_COMBAT_DAMAGE: &str =
    "Whenever this creature deals combat damage to a player, you may draw a card.";

/// Mandatory variant (no "you may") for the first-strike re-entry test. A
/// per-creature combat-damage trigger with NO optional choice: it resolves with
/// no `OptionalEffectChoice` prompt, so the two ordered triggers drain fully off
/// the stack and the test can reach the empty-stack priority pass that resumes
/// the CR 510.4 regular sub-step. The optional `DRAW_ON_COMBAT_DAMAGE` would
/// surface a draw-or-not prompt mid-resolution that the stack-drain helper cannot
/// satisfy, stalling before the re-entry under test.
const DRAW_ON_COMBAT_DAMAGE_MANDATORY: &str =
    "Whenever this creature deals combat damage to a player, draw a card.";

/// Primary regression — two same-controller creatures with combat-damage
/// triggers attack unblocked, both deal combat damage to P1, and the resulting
/// two simultaneous same-controller triggers MUST surface a CR 603.3b
/// `WaitingFor::OrderTriggers` prompt for P0 instead of hanging.
#[test]
fn two_same_controller_combat_damage_triggers_surface_order_prompt_then_progress() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two attackers, both P0-controlled, each with the draw-on-combat-damage
    // trigger. Distinct names so the harness can disambiguate.
    let bear_a = scenario
        .add_creature_from_oracle(P0, "Tide Raider A", 2, 2, DRAW_ON_COMBAT_DAMAGE)
        .id();
    let bear_b = scenario
        .add_creature_from_oracle(P0, "Tide Raider B", 2, 2, DRAW_ON_COMBAT_DAMAGE)
        .id();

    // Stock P0's library so the optional draws (if taken later) are observable
    // and never trigger a draw-from-empty loss.
    for name in ["Lib 1", "Lib 2", "Lib 3", "Lib 4"] {
        scenario.add_card_to_library_top(P0, name);
    }

    let mut runner = scenario.build();
    let life_before_p1 = runner.life(P1);

    // Both creatures attack P1 unblocked → 2 simultaneous, same-controller
    // combat-damage triggers fire inside `resolve_combat_damage`.
    run_combat(&mut runner, vec![bear_a, bear_b], vec![]);

    // CR 510.1b / CR 510.2: both 2/2s connected — P1 took 4 combat damage. This
    // proves combat actually reached the damage step (not skipped).
    assert_eq!(
        runner.life(P1),
        life_before_p1 - 4,
        "CR 510.1b: two unblocked 2/2s deal 4 combat damage to P1"
    );

    // THE FIX: the two same-controller combat-damage triggers surface a
    // CR 603.3b ordering prompt for P0 — they are NOT stranded in
    // `pending_trigger_order` with the phase advanced past them (the hang).
    match &runner.state().waiting_for {
        WaitingFor::OrderTriggers { player, triggers } => {
            assert_eq!(
                *player, P0,
                "CR 603.3b: P0 controls both combat-damage triggers and must order them"
            );
            assert_eq!(
                triggers.len(),
                2,
                "CR 603.3b: both same-controller combat-damage triggers are awaiting ordering"
            );
        }
        other => panic!(
            "expected CR 603.3b OrderTriggers prompt after combat damage, got {other:?} \
             (the turn-18 hang: triggers stranded in pending_trigger_order, phase advanced)"
        ),
    }

    // The ordering pass is genuinely live in the authoritative source.
    assert!(
        runner.state().pending_trigger_order.is_some(),
        "the ordering pass must be live in pending_trigger_order while the prompt is up"
    );

    // Drain the prompt by submitting an order, then resolve the two triggers.
    // The turn MUST progress out of combat — not loop back to DeclareAttackers.
    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("submitting the CR 603.3b trigger order should succeed");
    runner.advance_until_stack_empty();

    assert!(
        runner.state().pending_trigger_order.is_none(),
        "after ordering and resolving, the ordering pass must be cleared"
    );
    assert_ne!(
        runner.state().phase,
        Phase::DeclareAttackers,
        "the turn must progress past combat after the triggers resolve — not re-enter \
         DeclareAttackers (a symptom of the stranded-prompt hang)"
    );
}

/// Shape / recovery pin (Edit 2) — the surfaced CR 603.3b prompt is rebuilt
/// from the AUTHORITATIVE `pending_trigger_order` state, so its contents match
/// that source exactly (same controller, same trigger count). This is the
/// property `process_phase_triggers` now relies on:
/// `build_next_order_triggers_prompt_public(state)` reads `pending_trigger_order`
/// directly instead of cloning `state.waiting_for`, which is what lets it
/// recover a stale/orphaned `waiting_for` and surface the real prompt.
///
/// This is a SHAPE test: it pins the prompt-vs-pending-state correspondence,
/// not a fault-injected corruption scenario. A full corruption-injection
/// recovery test (seed `pending_trigger_order` with a stale `waiting_for` and
/// re-enter `process_phase_triggers`) is not expressible from the integration
/// test crate — `build_next_order_triggers_prompt_public` is `pub(crate)` and
/// `PendingTriggerOrder` cannot be hand-constructed without engine-internal
/// access to `PendingTrigger`/`ResolvedAbility`.
#[test]
fn order_prompt_contents_track_pending_trigger_order_state() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bear_a = scenario
        .add_creature_from_oracle(P0, "Tide Raider A", 2, 2, DRAW_ON_COMBAT_DAMAGE)
        .id();
    let bear_b = scenario
        .add_creature_from_oracle(P0, "Tide Raider B", 2, 2, DRAW_ON_COMBAT_DAMAGE)
        .id();
    for name in ["Lib 1", "Lib 2", "Lib 3", "Lib 4"] {
        scenario.add_card_to_library_top(P0, name);
    }

    let mut runner = scenario.build();
    run_combat(&mut runner, vec![bear_a, bear_b], vec![]);

    let WaitingFor::OrderTriggers {
        player: prompt_player,
        triggers: prompt_triggers,
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected a CR 603.3b OrderTriggers prompt, got {:?}",
            runner.state().waiting_for
        );
    };

    let order = runner
        .state()
        .pending_trigger_order
        .as_ref()
        .expect("a live ordering pass must back the prompt");
    let unordered_group = order
        .groups
        .iter()
        .find(|g| !g.ordered)
        .expect("the prompt corresponds to the first unordered group");

    // The surfaced prompt is a faithful projection of the authoritative
    // `pending_trigger_order` group, not a clone of a stale `waiting_for`.
    assert_eq!(
        prompt_player, unordered_group.controller,
        "the prompt's player is the unordered group's controller (rebuilt from pending state)"
    );
    assert_eq!(
        prompt_triggers.len(),
        unordered_group.triggers.len(),
        "the prompt's trigger count matches the pending group (rebuilt, not cloned)"
    );
    assert_eq!(
        prompt_player, P0,
        "both combat-damage triggers belong to P0"
    );
}

/// CR 510.4 + CR 603.3b mixed-combat regression — the first-strike sub-step's
/// trigger-ordering prompt must NOT cause the mandatory regular (second)
/// combat-damage sub-step to be skipped.
///
/// Pre-fix bug: P0's two first-strike creatures each carry a combat-damage
/// trigger, so the first-strike sub-step paused on a CR 603.3b OrderTriggers
/// prompt with `regular_damage_done == false`. When that ordering pass resolved
/// and all players passed with an empty stack, `handle_priority_pass` saw the
/// empty stack and called `advance_phase`, moving CombatDamage -> EndCombat
/// WITHOUT running the regular sub-step — so the non-first-strike 3/3 attacker
/// dealt no combat damage, silently violating CR 510.4 (after the first-strike
/// step the phase MUST get a second combat-damage step for the remaining
/// attackers). The fix: the empty-stack branch re-enters the combat-damage
/// turn-based action while `regular_damage_done == false` instead of advancing.
#[test]
fn first_strike_order_prompt_does_not_skip_regular_combat_damage_substep() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two P0 first-strike 2/2s, each with the combat-damage trigger → the
    // first-strike sub-step produces a CR 603.3b ordering prompt. The builder
    // borrows `&mut state`, so each creature must be built in its own scope
    // (a fluent `.first_strike().id()` chain or multiple live builders won't
    // compile).
    let fs_a = {
        let mut b = scenario.add_creature_from_oracle(
            P0,
            "FS Raider A",
            2,
            2,
            DRAW_ON_COMBAT_DAMAGE_MANDATORY,
        );
        b.first_strike();
        b.id()
    };
    let fs_b = {
        let mut b = scenario.add_creature_from_oracle(
            P0,
            "FS Raider B",
            2,
            2,
            DRAW_ON_COMBAT_DAMAGE_MANDATORY,
        );
        b.first_strike();
        b.id()
    };
    // One P0 NON-first-strike 3/3 — it deals its damage only in the regular
    // (second) sub-step, which is exactly what the pre-fix bug skipped.
    let regular = scenario.add_creature(P0, "Regular Attacker", 3, 3).id();

    // Stock P0's library so the optional draws never cause a draw-from-empty loss.
    for name in ["Lib 1", "Lib 2", "Lib 3", "Lib 4", "Lib 5", "Lib 6"] {
        scenario.add_card_to_library_top(P0, name);
    }

    let mut runner = scenario.build();
    let life_before = runner.life(P1);

    // All three attack P1 unblocked.
    run_combat(&mut runner, vec![fs_a, fs_b, regular], vec![]);

    // The first-strike sub-step paused on the CR 603.3b ordering prompt, BEFORE
    // the regular sub-step ran.
    match &runner.state().waiting_for {
        WaitingFor::OrderTriggers { player, triggers } => {
            assert_eq!(
                *player, P0,
                "CR 603.3b: P0 controls both first-strike combat-damage triggers"
            );
            assert_eq!(
                triggers.len(),
                2,
                "CR 603.3b: both first-strike combat-damage triggers await ordering"
            );
        }
        other => panic!(
            "expected CR 603.3b OrderTriggers prompt after first-strike damage, got {other:?}"
        ),
    }

    // Discriminator: the regular sub-step has NOT run yet, so only the two
    // first-strike 2/2s have dealt damage (2 + 2 = 4); the 3/3 has not hit.
    assert!(
        !runner
            .state()
            .combat
            .as_ref()
            .expect("combat is still in progress at the first-strike pause")
            .regular_damage_done,
        "CR 510.4: regular combat-damage sub-step must not have run while the first-strike \
         ordering prompt is up"
    );
    assert_eq!(
        runner.life(P1),
        life_before - 4,
        "CR 510.4: only the two first-strike 2/2s have dealt damage so far (the 3/3 hits in \
         the regular sub-step)"
    );

    // Submit the ordering and resolve the two first-strike triggers off the stack.
    // `advance_until_stack_empty` stops the instant the stack is empty — it does NOT
    // pass priority over the empty stack — so on its own it cannot drive a turn-based
    // action that resumes AFTER the stack drains (CR 510.4's regular sub-step). The
    // triggers are mandatory (no "you may"), so they drain cleanly without surfacing
    // an OptionalEffectChoice the helper could not satisfy.
    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("submitting the CR 603.3b trigger order should succeed");
    runner.advance_until_stack_empty();

    // The two first-strike triggers have resolved and the stack is empty, but the
    // CombatDamage step's turn-based action is incomplete (regular_damage_done ==
    // false). Passing priority on the empty stack drives the CR 510.4 completeness
    // gate, which re-enters the regular sub-step instead of advancing to end of
    // combat — this is the precise pass the pre-fix bug skipped.
    runner.pass_both_players();

    // Authoritative proof the CR 510.4 second (regular) sub-step ran: the 3/3
    // dealt its 3 combat damage (4 + 3 = 7 total).
    assert_eq!(
        runner.life(P1),
        life_before - 7,
        "CR 510.4: the regular sub-step ran — the non-first-strike 3/3 dealt its 3 damage"
    );

    // Combat has ended (or, at minimum, the regular sub-step completed).
    assert!(
        runner.state().combat.is_none()
            || runner.state().combat.as_ref().unwrap().regular_damage_done,
        "CR 510.4: combat ended or the regular combat-damage sub-step completed"
    );
}
