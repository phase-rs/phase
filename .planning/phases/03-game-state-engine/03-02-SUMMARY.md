---
phase: 03-game-state-engine
plan: 02
subsystem: engine
tags: [rust, game-engine, turns, priority, stack, action-response]

requires:
  - phase: 03-game-state-engine
    plan: 01
    provides: GameObject, GameState with central object store, zone transfer operations, WaitingFor, ActionResult, StackEntry
provides:
  - apply() entry point for action-response game loop
  - Turn progression through all 12 MTG phases with auto-advance
  - Priority system tracking consecutive passes
  - Stack push/resolve with LIFO ordering
  - PlayLand action with validation
  - EngineError enum for action validation
  - new_game() and start_game() convenience functions
affects: [03-03, 04-ability-system, 05-triggers-combat]

tech-stack:
  added: []
  patterns: [action-response-engine, auto-advance-loop, priority-pass-tracking, lifo-stack-resolution]

key-files:
  created:
    - crates/engine/src/game/engine.rs
    - crates/engine/src/game/turns.rs
    - crates/engine/src/game/priority.rs
    - crates/engine/src/game/stack.rs
  modified:
    - crates/engine/src/game/mod.rs
    - crates/engine/src/types/game_state.rs
    - crates/engine/src/types/events.rs

key-decisions:
  - "Auto-advance loop pattern: phases needing no player input auto-skip in a loop until reaching main/end phases"
  - "Priority pass count on GameState for consecutive-pass tracking (not per-player)"
  - "Stack LIFO via Vec pop (last element = top of stack)"
  - "Permanent type detection for stack resolution destination (battlefield vs graveyard)"

patterns-established:
  - "Action-response: apply(state, action) -> Result<ActionResult, EngineError>"
  - "Auto-advance: loop through phases, execute automatic steps, stop at priority-granting phases"
  - "Priority pass tracking: counter increments per pass, resets on non-pass actions or phase changes"

requirements-completed: [ENG-01, ENG-02]

duration: 4min
completed: 2026-03-07
---

# Phase 03 Plan 02: Game Loop Engine Summary

**Turn progression with auto-advance through 12 phases, priority system with consecutive-pass detection, stack LIFO resolution, and apply() action-response entry point**

## Performance

- **Duration:** 4 min
- **Started:** 2026-03-07T22:46:15Z
- **Completed:** 2026-03-07T22:50:41Z
- **Tasks:** 2
- **Files modified:** 7

## Accomplishments
- apply() entry point implementing action-response pattern: validates actions against WaitingFor, dispatches to handlers
- Full turn cycle: Untap -> Draw -> PreCombatMain -> (combat skip) -> PostCombatMain -> End -> Cleanup -> next turn
- Priority system: tracks consecutive passes, advances phase on empty stack, resolves top on non-empty stack
- Stack push/resolve with LIFO ordering, permanent type detection for destination zone
- PlayLand action with main-phase validation, land limit enforcement, card-in-hand check
- 40 new tests (165 total engine tests) including integration test for full turn cycle

## Task Commits

Each task was committed atomically:

1. **Task 1: Turn progression and priority system** - `48aa2ed` (feat)
2. **Task 2: Stack resolution and apply() engine entry point** - `016de5e` (feat)

## Files Created/Modified
- `crates/engine/src/game/engine.rs` - apply() entry point, EngineError, new_game, start_game
- `crates/engine/src/game/turns.rs` - Phase progression, auto-advance, untap/draw/cleanup execution
- `crates/engine/src/game/priority.rs` - Priority pass handling, reset, opponent helper
- `crates/engine/src/game/stack.rs` - Stack push, LIFO resolve, permanent type detection
- `crates/engine/src/game/mod.rs` - Module declarations and re-exports
- `crates/engine/src/types/game_state.rs` - Added priority_pass_count field
- `crates/engine/src/types/events.rs` - Added CardDrawn, PermanentUntapped, LandPlayed, StackPushed, StackResolved, Discarded, DamageCleared events

## Decisions Made
- Auto-advance implemented as a loop that processes phases needing no player input, stops at main phases and end step
- priority_pass_count added to GameState (single counter, not per-player) since only active/non-active player distinction matters
- Stack uses Vec with pop (last element = top) for LIFO resolution
- Permanent type detection checks card_types.core_types for battlefield vs graveyard routing

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 2 - Missing Critical] Added priority_pass_count to GameState**
- **Found during:** Task 1
- **Issue:** GameState had no field to track consecutive priority passes
- **Fix:** Added `priority_pass_count: u8` to GameState, PartialEq, and new_two_player constructor
- **Files modified:** crates/engine/src/types/game_state.rs
- **Committed in:** 48aa2ed (Task 1 commit)

**2. [Rule 2 - Missing Critical] Added new GameEvent variants**
- **Found during:** Task 1
- **Issue:** Events needed for turns/stack/engine (CardDrawn, PermanentUntapped, LandPlayed, etc.)
- **Fix:** Added 7 new event variants to GameEvent enum
- **Files modified:** crates/engine/src/types/events.rs
- **Committed in:** 48aa2ed (Task 1 commit)

---

**Total deviations:** 2 auto-fixed (2 missing critical)
**Impact on plan:** Both were anticipated by the plan but needed to be implemented as part of task execution. No scope creep.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Engine ready for mana payment system (plan 03 scope)
- apply() ready to add CastSpell handler
- State-based actions stub ready for plan 03 implementation
- Stack resolution handles permanent vs non-permanent routing
- Turn/priority/stack fully tested with integration coverage

---
*Phase: 03-game-state-engine*
*Completed: 2026-03-07*
