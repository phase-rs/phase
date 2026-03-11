---
phase: 29-support-n-players
plan: 05
subsystem: engine
tags: [rust, multiplayer, n-player, refactor]

requires:
  - phase: 29-support-n-players
    plan: 01
    provides: "players module with next_player(), opponents(), apnap_order()"
  - phase: 29-support-n-players
    plan: 02
    provides: "deprecated opponent() wrapper in priority.rs"
  - phase: 29-support-n-players
    plan: 03
    provides: "N-player combat with per-attacker defending_player"
provides:
  - "Zero PlayerId(1-x) in core engine modules"
  - "opponent() removed from priority.rs"
affects: [29-06, 29-07, phase-ai, server-core, phase-server]

tech-stack:
  added: []
  patterns:
    - "Use players::next_player() instead of PlayerId(1-x) for opponent lookup"
    - "Use WaitingFor player field instead of recomputing defending player"

key-files:
  created: []
  modified:
    - crates/engine/src/game/engine.rs
    - crates/engine/src/game/combat.rs
    - crates/engine/src/game/priority.rs

key-decisions:
  - "engine.rs DeclareBlockers: removed redundant defending player computation, WaitingFor already carries the correct player"
  - "combat.rs: unwrap_or fallback uses players::next_player() for N-player correctness"
  - "opponent() removed entirely (no external callers found in engine crate)"

patterns-established:
  - "No PlayerId(1-x) arithmetic in core engine modules"

requirements-completed: [NP-OPPONENT-MIGRATION]

duration: 3min
completed: 2026-03-11
---

# Phase 29 Plan 05: Remove PlayerId(1-x) from Core Engine Modules Summary

**Eliminated all hardcoded 2-player opponent assumptions from core engine modules using players:: functions**

## Performance

- **Duration:** 3 min
- **Started:** 2026-03-11T18:20:11Z
- **Completed:** 2026-03-11T18:23:30Z
- **Tasks:** 1
- **Files modified:** 3

## Accomplishments
- Removed all `PlayerId(1 - x.0)` from engine.rs, combat.rs, and priority.rs
- Removed deprecated `opponent()` function and its test from priority.rs
- All 42 engine tests pass with zero regressions

## Task Commits

Each task was committed atomically:

1. **Task 1: Replace PlayerId(1 - x.0) in core engine modules** - `784d9fc87` (feat)

**Plan metadata:** pending (docs: complete plan)

## Files Created/Modified
- `crates/engine/src/game/engine.rs` - Removed redundant defending player computation in DeclareBlockers handler
- `crates/engine/src/game/combat.rs` - Replaced PlayerId(1-x) fallback with players::next_player()
- `crates/engine/src/game/priority.rs` - Removed deprecated opponent() function and its test

## Decisions Made
- engine.rs DeclareBlockers: the `player` field in `WaitingFor::DeclareBlockers` already holds the correct defending player, so the redundant `PlayerId(1 - state.active_player.0)` validation was removed entirely
- combat.rs: the unwrap_or fallback for empty attacker lists now uses `players::next_player()` for N-player correctness
- opponent() had zero external callers in the engine crate -- removed without a deprecated wrapper

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Also fixed combat.rs PlayerId(1-x)**
- **Found during:** Task 1
- **Issue:** combat.rs had a `PlayerId(1 - state.active_player.0)` fallback not listed in the plan's file list
- **Fix:** Replaced with `players::next_player(state, state.active_player)`
- **Files modified:** crates/engine/src/game/combat.rs
- **Verification:** cargo test -p engine passes
- **Committed in:** 784d9fc87

---

**Total deviations:** 1 auto-fixed (1 bug fix)
**Impact on plan:** Necessary for completeness -- combat.rs is a core engine module that had the same pattern.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Core engine modules are clean of PlayerId(1-x)
- Remaining instances in phase-ai, server-core, and phase-server are targets for Plans 06-07
- filter.rs, targeting.rs, casting.rs, stack.rs, static_abilities.rs, triggers.rs, zones.rs were already clean

---
*Phase: 29-support-n-players*
*Completed: 2026-03-11*
