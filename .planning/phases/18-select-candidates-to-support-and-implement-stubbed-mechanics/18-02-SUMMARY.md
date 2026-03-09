---
phase: 18-select-candidates-to-support-and-implement-stubbed-mechanics
plan: 02
subsystem: engine
tags: [effects, mill, scry, pump-all, damage-all, destroy-all, change-zone-all, filter]

requires:
  - phase: 18-01
    provides: "Evasion keywords and combat validation"
provides:
  - "Mill and Scry effect handlers"
  - "PumpAll, DamageAll, DestroyAll, ChangeZoneAll mass effect handlers"
  - "Shared matches_filter() helper for Forge Valid patterns"
  - "21-entry effect registry (was 15)"
affects: [18-03, 18-04, card-coverage]

tech-stack:
  added: []
  patterns: ["resolve_all pattern for mass effects", "matches_filter shared helper for Forge Valid strings"]

key-files:
  created:
    - crates/engine/src/game/effects/mill.rs
    - crates/engine/src/game/effects/scry.rs
  modified:
    - crates/engine/src/game/effects/mod.rs
    - crates/engine/src/game/effects/pump.rs
    - crates/engine/src/game/effects/deal_damage.rs
    - crates/engine/src/game/effects/destroy.rs
    - crates/engine/src/game/effects/change_zone.rs

key-decisions:
  - "Scry simplified: all scryed cards go to bottom (TODO: WaitingFor::ScryChoice for interactive ordering)"
  - "Shared matches_filter() in effects/mod.rs handles Forge Valid patterns: type.controller (YouCtrl/OppCtrl)"
  - "DamageAll bypasses replacement effects for simplicity (direct damage_marked increment)"
  - "DestroyAll bypasses replacement effects, uses direct move_to_zone for simplicity"

patterns-established:
  - "resolve_all pattern: collect matching IDs first, then mutate, to avoid borrow conflicts"
  - "matches_filter(obj, filter, controller) for Forge Valid string parsing"

requirements-completed: [MECH-03, MECH-04]

duration: 8min
completed: 2026-03-09
---

# Phase 18 Plan 02: Core Effect Handlers Summary

**Mill, Scry, PumpAll, DamageAll, DestroyAll, ChangeZoneAll effect handlers with shared Forge Valid filter**

## Performance

- **Duration:** 8 min
- **Started:** 2026-03-09T15:03:52Z
- **Completed:** 2026-03-09T15:12:36Z
- **Tasks:** 2
- **Files modified:** 7

## Accomplishments
- Created Mill effect (moves top N cards from library to graveyard, handles empty/partial library)
- Created Scry effect (simplified: moves top N to bottom; TODO for interactive choice)
- Added PumpAll, DamageAll, DestroyAll, ChangeZoneAll to existing effect modules
- Created shared matches_filter() helper supporting Forge Valid patterns (Creature.YouCtrl, etc.)
- Registry grew from 15 to 21 entries; all 52 effects tests pass

## Task Commits

Each task was committed atomically:

1. **Task 1: Create Mill and Scry effect handlers** - `6d28326` (test), `d1f3aea` (feat)
2. **Task 2: Add PumpAll, DamageAll, DestroyAll, ChangeZoneAll** - `ad4cf53` (test), `7b98f28` (feat), `9b751e5` (chore: format)

_TDD: RED (failing tests) then GREEN (implementation) commits for each task._

## Files Created/Modified
- `crates/engine/src/game/effects/mill.rs` - Mill effect: moves top N from library to graveyard
- `crates/engine/src/game/effects/scry.rs` - Scry effect: simplified impl, moves top N to bottom
- `crates/engine/src/game/effects/mod.rs` - Registry (21 entries), shared matches_filter() helper
- `crates/engine/src/game/effects/pump.rs` - Added resolve_all for PumpAll
- `crates/engine/src/game/effects/deal_damage.rs` - Added resolve_all for DamageAll
- `crates/engine/src/game/effects/destroy.rs` - Added resolve_all for DestroyAll
- `crates/engine/src/game/effects/change_zone.rs` - Added resolve_all for ChangeZoneAll

## Decisions Made
- Scry simplified to move all scryed cards to bottom (proper implementation needs WaitingFor::ScryChoice)
- DamageAll and DestroyAll bypass replacement effects for simplicity (direct state mutation)
- matches_filter() placed in effects/mod.rs as a pub function for reuse across all mass effect handlers
- Mill defaults to opponent when no player target specified

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
- Pre-existing compilation errors in test_helpers.rs and targeting.rs from plan 18-01 (not fixed, out of scope)

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- 6 new effect handlers ready for card resolution
- matches_filter() helper available for future mass effects
- Scry needs WaitingFor::ScryChoice in a future phase for proper interactive implementation

---
*Phase: 18-select-candidates-to-support-and-implement-stubbed-mechanics*
*Completed: 2026-03-09*
