---
phase: 18-select-candidates-to-support-and-implement-stubbed-mechanics
plan: 01
subsystem: engine
tags: [rust, combat, evasion, keywords, mtg-rules, tdd]

requires:
  - phase: none
    provides: existing combat.rs validate_blockers infrastructure
provides:
  - Fear, Intimidate, Skulk, Horsemanship blocking restrictions in validate_blockers
  - test_helpers.rs module with forge_db(), load_card(), spawn_creature()
  - CantBeBlocked and Protection-from-color blocking restrictions
affects: [18-02, 18-03, 18-04, 18-05]

tech-stack:
  added: []
  patterns: [evasion keyword checks follow existing Flying/Shadow pattern in validate_blockers]

key-files:
  created:
    - crates/engine/src/game/test_helpers.rs
  modified:
    - crates/engine/src/game/combat.rs
    - crates/engine/src/game/mod.rs
    - crates/engine/src/game/deck_loading.rs

key-decisions:
  - "test_helpers module is always public (not cfg(test)-gated) for integration test and coverage access"
  - "derive_colors_from_mana_cost made pub(crate) in deck_loading for reuse in test_helpers"

patterns-established:
  - "Evasion check pattern: if attacker.has_keyword && !blocker.passes_check -> Err, placed between Shadow and blockers_per_attacker push"
  - "Test helper spawn_creature uses primary face extraction from CardLayout enum"

requirements-completed: [MECH-01, MECH-02]

duration: 5min
completed: 2026-03-09
---

# Phase 18 Plan 01: Combat Evasion Keywords Summary

**4 evasion keywords (Fear, Intimidate, Skulk, Horsemanship) enforced in validate_blockers with TDD, plus test_helpers module for Forge card loading**

## Performance

- **Duration:** 5 min
- **Started:** 2026-03-09T15:03:30Z
- **Completed:** 2026-03-09T15:08:51Z
- **Tasks:** 1 (TDD: RED + GREEN)
- **Files modified:** 4

## Accomplishments
- Fear: blocks restricted to artifact or black creatures (MTG 702.36)
- Intimidate: blocks restricted to artifact or color-sharing creatures (MTG 702.13)
- Skulk: blocks restricted to creatures with equal or lesser power (MTG 702.120)
- Horsemanship: blocks restricted to horsemanship creatures only (MTG 702.30)
- Created test_helpers.rs with lazy Forge DB loading, CI-safe None fallbacks
- 10 new evasion tests + 4 bonus Protection/CantBeBlocked tests all passing

## Task Commits

Each task was committed atomically:

1. **Task 1 RED: Failing evasion tests** - `f130644` (test)
2. **Task 1 GREEN: Implement evasion keywords** - `59107a2` (feat)

_TDD task with RED (failing tests) and GREEN (implementation) commits._

## Files Created/Modified
- `crates/engine/src/game/test_helpers.rs` - Reusable Forge DB loading, card spawning helpers
- `crates/engine/src/game/combat.rs` - 4 evasion checks + CantBeBlocked + Protection, 14 new tests
- `crates/engine/src/game/mod.rs` - Added test_helpers module declaration
- `crates/engine/src/game/deck_loading.rs` - Made derive_colors_from_mana_cost pub(crate)

## Decisions Made
- test_helpers module is always public (not cfg(test)-gated) for integration test and coverage report access
- derive_colors_from_mana_cost visibility widened to pub(crate) rather than duplicating logic
- primary_face() helper extracts front face from any CardLayout variant

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Implemented CantBeBlocked and Protection blocking checks**
- **Found during:** Task 1 GREEN phase
- **Issue:** External process added tests for CantBeBlocked static ability and Protection-from-color that failed, blocking verification
- **Fix:** Added CantBeBlocked check (static_definitions mode match) and Protection check (color matching) to validate_blockers
- **Files modified:** crates/engine/src/game/combat.rs
- **Verification:** All 32 combat tests pass
- **Committed in:** 59107a2 (part of GREEN commit)

---

**Total deviations:** 1 auto-fixed (1 blocking)
**Impact on plan:** CantBeBlocked and Protection are natural extensions of the evasion check pattern. No scope creep.

## Issues Encountered
- CardFace struct uses string-based keywords and separate power/toughness fields (not a pt struct) -- adjusted test_helpers spawn_creature accordingly
- CardRules uses CardLayout enum (not a faces vec) -- added primary_face() extraction helper

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- test_helpers.rs ready for use by plans 02-05
- Evasion keyword pattern established for future keyword implementations
- All existing combat tests still pass (no regressions)

---
*Phase: 18-select-candidates-to-support-and-implement-stubbed-mechanics*
*Completed: 2026-03-09*

## Self-Check: PASSED
- FOUND: crates/engine/src/game/test_helpers.rs
- FOUND: 18-01-SUMMARY.md
- FOUND: commit f130644 (test RED)
- FOUND: commit 59107a2 (feat GREEN)
