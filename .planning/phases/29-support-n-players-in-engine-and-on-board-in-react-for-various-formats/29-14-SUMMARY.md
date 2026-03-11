---
phase: 29-support-n-players
plan: 14
subsystem: engine
tags: [rust, multiplayer, n-player, effects, refactor]

requires:
  - phase: 29-support-n-players
    plan: 05
    provides: "players module with next_player(), opponents() and zero PlayerId(1-x) in core engine"
provides:
  - "Zero PlayerId(1-x) in effects modules A-M plus mod.rs"
  - "All effect opponent lookups use players:: functions"
affects: [29-15, 29-16]

tech-stack:
  added: []
  patterns:
    - "Use players::next_player() for default opponent fallback in effects"

key-files:
  created: []
  modified:
    - crates/engine/src/game/effects/mill.rs

key-decisions:
  - "mill.rs was the only effects module with hardcoded 2-player opponent logic; all others already used target-based resolution"
  - "Replaced PlayerId(if controller.0 == 0 { 1 } else { 0 }) with players::next_player() for N-player correctness"

patterns-established:
  - "Effect modules use resolved targets or players::next_player() for opponent selection"

requirements-completed: [NP-OPPONENT-MIGRATION]

duration: 4min
completed: 2026-03-11
---

# Phase 29 Plan 14: Effects Modules A-M N-Player Migration Summary

**Replaced hardcoded 2-player opponent in mill.rs with players::next_player(); all other effects modules already clean**

## Performance

- **Duration:** 4 min
- **Started:** 2026-03-11T18:31:54Z
- **Completed:** 2026-03-11T18:35:56Z
- **Tasks:** 1
- **Files modified:** 1

## Accomplishments
- Audited all 13 target effects modules for PlayerId(1-x) and conditional opponent patterns
- Found and fixed one instance in mill.rs (default opponent fallback)
- Confirmed all other effects modules (deal_damage, draw, life, discard, sacrifice, bounce, destroy, counter, gain_control, pump, mana, mod) were already clean
- All 42 engine tests pass with zero regressions

## Task Commits

Each task was committed atomically:

1. **Task 1: Replace PlayerId(1 - x.0) in effects modules (A-M)** - `e9c57eb67` (feat)

**Plan metadata:** pending (docs: complete plan)

## Files Created/Modified
- `crates/engine/src/game/effects/mill.rs` - Replaced hardcoded 2-player opponent fallback with players::next_player(), removed unused PlayerId import

## Decisions Made
- mill.rs was the only file with a hardcoded opponent pattern: `PlayerId(if ability.controller.0 == 0 { 1 } else { 0 })`. All other target files use explicit TargetRef::Player targets from ability resolution, which are already N-player safe.
- Used `players::next_player()` as the fallback (consistent with Plan 05 pattern) rather than `players::opponents()` since mill defaults to a single target.

## Deviations from Plan

None - plan executed exactly as written. Most target files were already clean from prior migration work.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- All effects modules A-M plus mod.rs are clean of hardcoded 2-player opponent assumptions
- Ready for Plan 15 (effects modules N-Z) if applicable
- Pre-existing clippy warning in json_loader.rs (type_complexity) is out of scope

---
*Phase: 29-support-n-players*
*Completed: 2026-03-11*

## Self-Check: PASSED
