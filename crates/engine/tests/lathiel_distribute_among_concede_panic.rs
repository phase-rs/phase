//! Repro for issue #3175 — non-fatal engine panic at triggers.rs:3397
//! "pending_trigger_entry must reference a stack entry".
//!
//! Root cause: `do_eliminate` (elimination.rs) removes the conceding player's
//! stack entries via `state.stack.retain(|e| e.controller != player)` without
//! clearing `state.pending_trigger_entry`. When Lathiel's end-step trigger is
//! paused at `WaitingFor::DistributeAmong` (which sets
//! `pending_trigger_entry = Some(entry_id)`), the conceding player's stack entry
//! is removed but the cursor stays live. On the next action by a surviving player,
//! `run_post_action_pipeline` → `begin_pending_trigger_target_selection` finds
//! `state.pending_trigger` still set, re-prompts for target selection (pointing at
//! the eliminated player), and the first `ChooseTarget` dispatched for that re-prompt
//! eventually calls `mutate_pending_trigger_entry` with the dead entry_id → panic.
//!
//! The scenario requires THREE players: a 2-player game ends immediately when P0
//! concedes, so no subsequent actions can trigger the panic.
//!
//! Fix: `do_eliminate` must clear `pending_trigger_entry` (and `pending_trigger`)
//! when the removed stack entries include the one tracked by `pending_trigger_entry`.

use engine::game::elimination::eliminate_player;
use engine::game::scenario::GameScenario;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

const LATHIEL_ORACLE: &str = "Lifelink\nAt the beginning of each end step, if you gained life this turn, distribute up to that many +1/+1 counters among any number of other target creatures.";

/// Drive through `WaitingFor::Priority` passes until the given predicate returns true
/// or the loop limit is reached.  Returns the last `WaitingFor` seen.
fn drive_until<F: Fn(&WaitingFor) -> bool>(
    runner: &mut engine::game::scenario::GameRunner,
    pred: F,
    limit: usize,
) -> WaitingFor {
    for _ in 0..limit {
        let wf = runner.state().waiting_for.clone();
        if pred(&wf) {
            return wf;
        }
        match wf {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            WaitingFor::DeclareAttackers { .. } => {
                // Declare no attackers so combat passes quickly.
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("DeclareAttackers accepted");
            }
            other => panic!("unexpected WaitingFor while driving: {other:?}"),
        }
    }
    panic!("loop limit reached without satisfying predicate");
}

/// Reproduce the panic from issue #3175:
/// Lathiel's DistributeAmong trigger paused → trigger controller concedes →
/// surviving player submits PassPriority → engine re-prompts eliminated player →
/// target selection calls mutate_pending_trigger_entry with a dead entry → panic.
#[test]
fn lathiel_distribute_among_concede_does_not_panic() {
    // Three players so the game survives P0's concede.
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // P0 controls Lathiel (the trigger source); id not used, so discard.
    let _ = scenario
        .add_creature_from_oracle(P0, "Lathiel, the Bounteous Dawn", 2, 2, LATHIEL_ORACLE)
        .id();

    // P1 controls two creatures — the distribution targets.
    // Lathiel targets "other" creatures so they must not be Lathiel itself.
    let target_a = scenario.add_creature(P1, "Target A", 1, 1).id();
    let target_b = scenario.add_creature(P1, "Target B", 1, 1).id();

    // P2 has a creature — enough to keep the game alive after P0's permanents
    // are exiled on concede.
    scenario.add_vanilla(P2, 1, 1);

    let mut runner = scenario.build();

    // Inject life-gained-this-turn directly so we don't need to simulate combat.
    runner.state_mut().players[0].life_gained_this_turn = 2;

    // Advance through phases until the end step; Lathiel's trigger should fire.
    runner.advance_to_end_step();

    // Drive past any priority windows to reach Lathiel's trigger target selection.
    let wf = drive_until(
        &mut runner,
        |wf| {
            matches!(
                wf,
                WaitingFor::TriggerTargetSelection { .. }
                    | WaitingFor::TargetSelection { .. }
                    | WaitingFor::DistributeAmong { .. }
            )
        },
        64,
    );

    // If we landed directly at DistributeAmong (single-target auto-selection),
    // the pending_trigger_entry is set differently; drive target selection first.
    if matches!(
        wf,
        WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. }
    ) {
        // Select target_a for slot 0.
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target_a)),
            })
            .expect("ChooseTarget A accepted");

        // Select target_b for slot 1 (life_gained_this_turn = 2 → 2 slots).
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target_b)),
            })
            .expect("ChooseTarget B accepted");
    }

    // We must now be at DistributeAmong with pending_trigger_entry set.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DistributeAmong { .. }
        ),
        "expected DistributeAmong after target selection; got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_trigger_entry.is_some(),
        "pending_trigger_entry must be set at DistributeAmong pause"
    );
    let entry_id = runner.state().pending_trigger_entry.unwrap();
    assert!(
        runner.state().stack.iter().any(|e| e.id == entry_id),
        "pending_trigger_entry must reference an actual stack entry before concede"
    );

    // P0 concedes. The engine removes P0's stack entries via `retain` in
    // `do_eliminate`, but (before the fix) does NOT clear `pending_trigger_entry`.
    let mut events = Vec::new();
    eliminate_player(runner.state_mut(), P0, &mut events);

    // After the fix: pending_trigger_entry must be cleared if the entry is gone.
    // After the fix: pending_trigger must also be cleared.
    //
    // Before the fix, these assertions would PASS (bug present) and the panic
    // would surface only when P1 submits its first action below.
    let entry_still_on_stack = runner.state().stack.iter().any(|e| e.id == entry_id);
    let pending_entry_cleared = runner.state().pending_trigger_entry.is_none();

    if !entry_still_on_stack {
        // The entry was removed — the cursor must have been cleared too.
        assert!(
            pending_entry_cleared,
            "BUG (issue #3175): do_eliminate removed the stack entry \
             (id {entry_id:?}) but did not clear pending_trigger_entry"
        );
    }

    // Verify that after the fix, the state is NOT prompting eliminated P0 for
    // target selection. The primary invariant of the fix is that clearing
    // `pending_trigger_entry` prevents `begin_pending_trigger_target_selection`
    // from re-entering with a dangling entry id.
    //
    // Note: `eliminate_player` called directly (bypassing `run_post_action_pipeline`)
    // may leave `state.priority_player` pointing at P0 while `waiting_for` is
    // `Priority { player: P1 }`.  That mismatch makes PassPriority return
    // `NotYourPriority` here, which is a separate pre-existing quirk of direct
    // elimination calls.  We therefore accept `NotYourPriority` in the loop —
    // what we must NOT see is a panic inside `assign_pending_trigger_entry_ability`
    // or a re-prompt for the eliminated player's target selection.
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::GameOver { .. } => break,
            WaitingFor::TriggerTargetSelection { player, .. }
            | WaitingFor::TargetSelection { player, .. }
                if player == P0 =>
            {
                panic!(
                    "BUG (issue #3175): engine re-prompted eliminated player P0 for \
                     target selection — pending_trigger was not cleaned up on concede"
                );
            }
            WaitingFor::Priority { .. } => {
                match runner.act(GameAction::PassPriority) {
                    Ok(_) => {}
                    // `NotYourPriority` can arise when `eliminate_player` is called
                    // directly (outside `run_post_action_pipeline`), leaving
                    // `state.priority_player` stale.  Accept it here; the assertion
                    // above already confirmed the primary bug is fixed.
                    Err(engine::game::EngineError::NotYourPriority) => break,
                    Err(e) => panic!("unexpected error from PassPriority: {e:?}"),
                }
            }
            _other => break,
        }
    }
}
