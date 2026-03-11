---
phase: 29-support-n-players
plan: 02
subsystem: engine
tags: [rust, n-player, priority, turns, elimination, sba, 2hg]

requires:
  - phase: 29-support-n-players
    plan: 01
    provides: FormatConfig, seat_order, player iteration functions, priority_passes BTreeSet
provides:
  - N-player priority passing using BTreeSet (stack resolves when all living players pass)
  - Turn rotation via seat_order (skipping eliminated players)
  - 2HG team-based APNAP priority ordering
  - CR 800.4 elimination system (stack cleanup, permanent exile, team elimination)
  - N-player SBAs using elimination instead of immediate GameOver
  - PlayerEliminated GameEvent variant
affects: [29-03, 29-04, 29-05, 29-06, 29-07, 29-08, 29-09, 29-10, 29-11, 29-12, 29-13, 29-14, 29-15, 29-16]

tech-stack:
  added: []
  patterns: [set-based-priority-tracking, elimination-over-gameover, team-elimination-cascade]

key-files:
  created:
    - crates/engine/src/game/elimination.rs
  modified:
    - crates/engine/src/game/priority.rs
    - crates/engine/src/game/turns.rs
    - crates/engine/src/game/sba.rs
    - crates/engine/src/game/engine.rs
    - crates/engine/src/game/mulligan.rs
    - crates/engine/src/game/casting.rs
    - crates/engine/src/game/planeswalker.rs
    - crates/engine/src/game/combat.rs
    - crates/engine/src/game/mod.rs
    - crates/engine/src/types/events.rs

key-decisions:
  - "priority_passes BTreeSet tracks all consecutive passes; stack resolves when set size >= living player count"
  - "opponent() function kept as deprecated wrapper calling players::next_player() for migration compatibility"
  - "SBAs collect all losers before eliminating (handles simultaneous life loss correctly)"
  - "2HG team elimination cascades: eliminating one teammate also eliminates the partner"
  - "Pre-existing combat.rs test breakage fixed inline (declare_attackers signature changed to accept AttackTarget tuples)"

patterns-established:
  - "Elimination pattern: do_eliminate handles single player cleanup, eliminate_player orchestrates team cascades + game-over check"
  - "Set-based priority: insert into BTreeSet on pass, clear on any action, check len >= living_count for resolution"
  - "N-player turn rotation: players::next_player(state, active_player) instead of hardcoded toggle"

requirements-completed: [NP-PRIORITY, NP-TURNS, NP-ELIM]

duration: 11min
completed: 2026-03-11
---

# Phase 29 Plan 02: N-Player Priority, Turns, and Elimination Summary

**BTreeSet-based priority passing for N players, seat-order turn rotation, CR 800.4 elimination system with team support, and N-player SBAs**

## Performance

- **Duration:** 11 min
- **Started:** 2026-03-11T17:58:14Z
- **Completed:** 2026-03-11T18:09:14Z
- **Tasks:** 2
- **Files modified:** 11

## Accomplishments
- Priority system uses BTreeSet to track passes; resolves only when all living players have passed consecutively (not hardcoded to 2)
- Turn rotation uses players::next_player() in seat order, skipping eliminated players; 2HG uses APNAP team ordering
- Elimination module (CR 800.4) handles stack cleanup, permanent exile, team cascades, and game-over detection
- SBAs call eliminate_player instead of immediate GameOver, enabling multiplayer continuity
- 2-player backward compatibility preserved: 700+ tests pass with identical 2-player behavior

## Task Commits

Each task was committed atomically:

1. **Task 1: N-player priority, turn rotation, and 2HG team turns** - `2811328e4` (feat)
2. **Task 2: Elimination system and N-player SBAs** - `2c2d081fd` (feat)

## Files Created/Modified
- `crates/engine/src/game/elimination.rs` - CR 800.4 elimination: stack cleanup, permanent exile, team cascade, game-over check
- `crates/engine/src/game/priority.rs` - BTreeSet-based N-player priority passing with 2HG APNAP ordering
- `crates/engine/src/game/turns.rs` - Seat-order turn rotation via players::next_player()
- `crates/engine/src/game/sba.rs` - N-player SBAs using elimination; checks all players simultaneously
- `crates/engine/src/game/engine.rs` - Added priority_passes.clear() alongside priority_pass_count=0
- `crates/engine/src/game/mulligan.rs` - Seat-order mulligan iteration
- `crates/engine/src/game/casting.rs` - priority_passes.clear() on spell cast/ability activation
- `crates/engine/src/game/planeswalker.rs` - priority_passes.clear() on PW ability activation
- `crates/engine/src/game/combat.rs` - Fixed pre-existing test breakage (declare_attackers AttackTarget tuples)
- `crates/engine/src/game/mod.rs` - Added elimination module
- `crates/engine/src/types/events.rs` - Added PlayerEliminated event variant

## Decisions Made
- BTreeSet tracks priority passes instead of a counter; this naturally handles N players without needing to know the count upfront
- opponent() kept as deprecated wrapper calling players::next_player() -- Plan 05 will remove it
- SBAs now collect all dying players before eliminating (handles simultaneous life loss edge case in multiplayer)
- 2HG team elimination cascades through teammates() -- if one teammate dies, both are eliminated
- Pre-existing combat.rs test breakage (from uncommitted AttackTarget changes) fixed as Rule 3 blocking issue

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Fixed pre-existing combat.rs test compilation errors**
- **Found during:** Task 1 (compilation)
- **Issue:** combat.rs test code used old declare_attackers(&[ObjectId]) signature, but production code had been changed to accept &[(ObjectId, AttackTarget)] tuples in uncommitted changes
- **Fix:** Updated 3 test calls to use (id, AttackTarget::Player(PlayerId(1))) tuples
- **Files modified:** crates/engine/src/game/combat.rs
- **Verification:** All combat tests compile and pass
- **Committed in:** 2811328e4 (Task 1 commit)

---

**Total deviations:** 1 auto-fixed (1 blocking)
**Impact on plan:** Necessary to fix pre-existing compilation failure. No scope creep.

## Issues Encountered
- Pre-existing clippy type_complexity warning in json_loader.rs (out of scope, not related to our changes)

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Priority, turns, and elimination systems are fully N-player aware
- Plan 03 can build on these for N-player combat (multi-defender attacks)
- Plan 05 can remove the deprecated opponent() wrapper
- No blockers

---
*Phase: 29-support-n-players*
*Completed: 2026-03-11*
