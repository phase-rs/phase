//! GitHub issue #587 — Seasoned Dungeoneer "take the initiative" ETB
//! leaves the Undercity Secret Entrance room trigger orphaned.
//!
//! Oracle (line 1): "When this creature enters, you take the initiative."
//!
//! Bug symptom: after `Effect::TakeTheInitiative` resolved,
//! `effects::venture::queue_room_trigger` set `state.pending_trigger`
//! directly, bypassing the trigger-dispatch pipeline. No engine call site
//! consumed a no-target pending trigger, so the Undercity room 0
//! (Secret Entrance) "Search your library for a basic land" ability
//! never reached the stack and never fired.
//!
//! Fix: `queue_room_trigger` now routes through
//! `triggers::dispatch_synthetic_trigger`, which delegates to the same
//! pipeline as `process_triggers`. No-target room abilities push to the
//! stack; targeted ones (Forge / Lost Well / Trap! / Arena) open the
//! standard target-selection prompt.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 309.4c: room abilities are triggered abilities that go on the stack.
//!   - CR 603.2: a triggered ability triggers when its event occurs.
//!   - CR 701.49: venture into the dungeon (move marker / enter new dungeon).
//!   - CR 726.2 / CR 726.5: taking the initiative ventures into Undercity;
//!     re-taking still triggers a venture.
//!
//! Test (a) covers the orphan-state regression: cast Seasoned Dungeoneer,
//! its ETB resolves, the room trigger must either be on the stack or have
//! opened a follow-on prompt — never orphaned in `pending_trigger`.
//!
//! Test (b) covers the re-take path (CR 726.5): with initiative + Undercity
//! already on P0, casting Seasoned Dungeoneer ventures from room 0 to a
//! branch point (rooms 1/2). `queue_room_trigger` is not reached in this
//! flow, so `pending_trigger` must remain `None`.

use engine::game::dungeon::{dungeon_sentinel_id, DungeonId};
use engine::game::scenario::{GameScenario, P0};
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

/// The ETB-only slice of Seasoned Dungeoneer's Oracle text. We intentionally
/// drop the combat / Explore lines so the cast/resolve path under test stays
/// scoped to "you take the initiative".
const SEASONED_ETB: &str = "When this creature enters, you take the initiative.";

/// (a) Fresh game: P0 has no initiative and no active dungeon. Casting
/// Seasoned Dungeoneer resolves its ETB, sets initiative, auto-ventures
/// into Undercity, and the Secret Entrance room trigger MUST reach the
/// stack (or open a follow-on prompt) — never be orphaned in
/// `pending_trigger`.
#[test]
fn seasoned_dungeoneer_initiative_dispatches_room_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Stock P0's library with two basics so the Secret Entrance
    // "Search your library for a basic land" ability has legal choices.
    scenario.add_card_to_library_top(P0, "Plains");
    scenario.add_card_to_library_top(P0, "Forest");

    let seasoned = scenario
        .add_creature_to_hand_from_oracle(P0, "Seasoned Dungeoneer", 3, 4, SEASONED_ETB)
        .id();

    let mut runner = scenario.build();
    // The harness casts the 0-cost creature (auto-paid), resolves its ETB, and
    // drives resolution; its default SearchPolicy::Stop leaves the Undercity
    // Secret Entrance search prompt for inspection below.
    let outcome = runner.cast(seasoned).resolve();

    let state = outcome.state();

    // CR 726.3: initiative is set on the controller.
    assert_eq!(
        state.initiative,
        Some(P0),
        "CR 726.3: P0 must hold the initiative after the ETB resolves"
    );

    // CR 726.2 + CR 701.49: P0 enters Undercity at room 0.
    let progress = state
        .dungeon_progress
        .get(&P0)
        .expect("P0 must have dungeon progress after venturing");
    assert_eq!(progress.current_dungeon, Some(DungeonId::Undercity));
    assert_eq!(progress.current_room, 0);

    // CR 309.4c regression guard (issue #587): the Secret Entrance room
    // trigger must have been dispatched through the standard pipeline.
    // Before the fix, `queue_room_trigger` left the trigger orphaned in
    // `state.pending_trigger` and no engine path consumed it. After the fix
    // it goes on the stack (or opens a follow-on prompt) and resolves —
    // so `pending_trigger` is always cleared by the time the dust settles.
    assert!(
        state.pending_trigger.is_none(),
        "Issue #587: state.pending_trigger must be drained by dispatch_synthetic_trigger; \
         got {:?}",
        state.pending_trigger,
    );

    // Belt-and-braces: at least one game-rule path consumed the room trigger.
    // Either it's resolving on the stack right now (search has begun and is
    // waiting on the player), or it has already resolved and been popped.
    let room_trigger_on_stack = state
        .stack
        .iter()
        .any(|e| e.source_id == dungeon_sentinel_id(P0));
    let opened_search_prompt = matches!(state.waiting_for, WaitingFor::SearchChoice { .. });
    let stack_clean = state.stack.is_empty();
    assert!(
        room_trigger_on_stack || opened_search_prompt || stack_clean,
        "Issue #587: Undercity room trigger must have reached the dispatch pipeline; \
         stack = {:?}, waiting_for = {:?}",
        state.stack.iter().map(|e| e.source_id).collect::<Vec<_>>(),
        state.waiting_for,
    );
}

/// (b) Re-take path: P0 already holds initiative and is in Undercity at
/// room 0 (a branch point: {1, 2}). Casting Seasoned Dungeoneer still
/// ventures (CR 726.5), but lands on the branch-choice prompt before
/// `queue_room_trigger` runs — so `pending_trigger` must remain `None`.
#[test]
fn seasoned_dungeoneer_initiative_retake_lands_on_room_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let seasoned = scenario
        .add_creature_to_hand_from_oracle(P0, "Seasoned Dungeoneer", 3, 4, SEASONED_ETB)
        .id();

    let mut runner = scenario.build();

    // Pre-seed initiative + Undercity room 0 on P0.
    {
        let state = runner.state_mut();
        state.initiative = Some(P0);
        let progress = state.dungeon_progress.entry(P0).or_default();
        progress.current_dungeon = Some(DungeonId::Undercity);
        progress.current_room = 0;
    }

    // The harness casts + resolves; drive_resolution breaks at the
    // ChooseDungeonRoom branch-point prompt (a prompt it does not answer),
    // leaving it for inspection.
    let outcome = runner.cast(seasoned).resolve();

    let state = outcome.state();

    // CR 726.5: re-taking initiative still ventures. Undercity room 0
    // branches to rooms 1 and 2, so we land on ChooseDungeonRoom.
    match &state.waiting_for {
        WaitingFor::ChooseDungeonRoom {
            dungeon, options, ..
        } => {
            assert_eq!(*dungeon, DungeonId::Undercity);
            assert_eq!(options.as_slice(), &[1, 2]);
        }
        other => panic!("expected ChooseDungeonRoom on re-take, got {other:?}"),
    }

    // No queue_room_trigger call is made on this path, so no orphan can exist.
    assert!(
        state.pending_trigger.is_none(),
        "Issue #587: branch-point re-take must not leave a pending trigger; got {:?}",
        state.pending_trigger,
    );
}
