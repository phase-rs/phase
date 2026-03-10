---
phase: 22-test-infrastructure
plan: 01
subsystem: testing
tags: [rust, test-harness, insta, snapshot, builder-pattern, game-engine]

# Dependency graph
requires:
  - phase: 21-schema-mtgjson
    provides: Typed Effect/TriggerMode/StaticMode enums for ability attachment in CardBuilder
provides:
  - GameScenario builder for constructing game states with inline card definitions
  - CardBuilder with fluent keyword/ability chaining
  - GameRunner for step-by-step action execution via apply()
  - ScenarioResult with query methods (zone, life, battlefield_count)
  - GameSnapshot for insta-compatible JSON projections
  - Integration test directory structure with 7 mechanic stub modules
affects: [22-02, 22-03, future-rules-tests]

# Tech tracking
tech-stack:
  added: []
  patterns: [GameScenario builder pattern, CardBuilder fluent chaining, GameSnapshot projection]

key-files:
  created:
    - crates/engine/src/game/scenario.rs
    - crates/engine/tests/rules.rs
    - crates/engine/tests/rules/etb.rs
    - crates/engine/tests/rules/combat.rs
    - crates/engine/tests/rules/stack.rs
    - crates/engine/tests/rules/sba.rs
    - crates/engine/tests/rules/layers.rs
    - crates/engine/tests/rules/keywords.rs
    - crates/engine/tests/rules/targeting.rs
    - crates/engine/src/game/snapshots/engine__game__scenario__tests__scenario_basic_bear.snap
  modified:
    - crates/engine/src/game/mod.rs

key-decisions:
  - "CardBuilder borrows &mut GameState (not &mut GameScenario) to avoid borrow checker conflicts when adding multiple cards"
  - "scenario.rs is not #[cfg(test)] gated to allow integration test access from tests/rules.rs"
  - "GameSnapshot uses Debug formatting for keyword names (simple, correct, no extra serialization code)"
  - "#[path] attributes on test modules to work around Cargo integration test module resolution"

patterns-established:
  - "GameScenario::new().at_phase().add_creature().build() for test setup"
  - "CardBuilder.flying().deathtouch().trample() for keyword chaining"
  - "ScenarioResult.zone(id)/life(P0)/battlefield_count(P0) for assertions"
  - "GameSnapshot for insta::assert_json_snapshot! regression anchors"

requirements-completed: [TEST-01, TEST-03]

# Metrics
duration: 8min
completed: 2026-03-10
---

# Phase 22 Plan 01: GameScenario Test Harness Summary

**GameScenario builder with CardBuilder fluent chaining, GameRunner step-by-step execution, and GameSnapshot insta-compatible projections -- zero filesystem dependencies, replacing old forge_db test_helpers**

## Performance

- **Duration:** 8 min
- **Started:** 2026-03-10T19:05:00Z
- **Completed:** 2026-03-10T19:13:14Z
- **Tasks:** 2
- **Files modified:** 12

## Accomplishments
- GameScenario builder creates board states with inline card definitions (no filesystem deps)
- CardBuilder supports fluent keyword chaining (.flying().deathtouch()) and ability/static/trigger attachment
- GameRunner wraps apply() for step-by-step execution; ScenarioResult provides query methods for assertions
- GameSnapshot produces stable insta-compatible JSON projections using names (not IDs)
- Integration test scaffolding with 7 mechanic stub modules ready for Plans 02 and 03
- Old test_helpers.rs (forge_db/spawn_creature/load_card) completely removed with zero regressions

## Task Commits

Each task was committed atomically:

1. **Task 1: Build GameScenario builder, CardBuilder, GameRunner, and GameSnapshot** - `7ee68ad` (feat)
2. **Task 2: Create integration test scaffolding and remove old test_helpers** - `c8979cd` (feat)

## Files Created/Modified
- `crates/engine/src/game/scenario.rs` - GameScenario builder, CardBuilder, GameRunner, ScenarioResult, GameSnapshot (~500 lines)
- `crates/engine/src/game/mod.rs` - Added scenario module, removed test_helpers module
- `crates/engine/tests/rules.rs` - Integration test entry point with common imports
- `crates/engine/tests/rules/{etb,combat,stack,sba,layers,keywords,targeting}.rs` - Stub modules for Plans 02/03
- `crates/engine/src/game/snapshots/...scenario_basic_bear.snap` - First insta snapshot
- `crates/engine/src/game/test_helpers.rs` - Deleted (replaced by scenario.rs)

## Decisions Made
- CardBuilder borrows `&mut GameState` directly rather than `&mut GameScenario` to avoid borrow checker conflicts when adding multiple cards sequentially
- Removed `#[cfg(test)]` gate from scenario module since integration tests (which compile the crate as a dependency) cannot access `cfg(test)` modules
- Used `#[path]` attributes on test module declarations in `rules.rs` to work around Cargo's integration test module resolution
- GameSnapshot uses `Debug` formatting for keyword names -- simple, correct, no custom serialization needed

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Removed #[cfg(test)] gate from scenario module**
- **Found during:** Task 2 (integration test compilation)
- **Issue:** Integration tests compile the engine crate as a regular dependency, not in test mode, so `#[cfg(test)]` modules are invisible
- **Fix:** Removed `#[cfg(test)]` from `pub mod scenario` declaration; the module's content has no test-only dependencies
- **Files modified:** crates/engine/src/game/mod.rs
- **Verification:** `cargo build -p engine` and `cargo test -p engine --test rules` both pass
- **Committed in:** c8979cd (Task 2 commit)

**2. [Rule 3 - Blocking] Used #[path] attributes for integration test module resolution**
- **Found during:** Task 2 (integration test compilation)
- **Issue:** Cargo resolves modules from the `tests/` directory root for integration test binaries, not from a subdirectory matching the binary name
- **Fix:** Added `#[path = "rules/etb.rs"]` attributes to module declarations in `rules.rs`
- **Files modified:** crates/engine/tests/rules.rs
- **Verification:** `cargo test -p engine --test rules` compiles and runs
- **Committed in:** c8979cd (Task 2 commit)

**3. [Rule 3 - Blocking] Removed conflicting rules/mod.rs**
- **Found during:** Task 2 (integration test compilation)
- **Issue:** Both `tests/rules.rs` and `tests/rules/mod.rs` existed, causing Rust E0761 "file for module found at both locations"
- **Fix:** Removed `rules/mod.rs`, moved its content (common imports) into `rules.rs` directly
- **Files modified:** crates/engine/tests/rules.rs
- **Verification:** `cargo test -p engine --test rules` compiles and runs
- **Committed in:** c8979cd (Task 2 commit)

---

**Total deviations:** 3 auto-fixed (all Rule 3 - blocking issues)
**Impact on plan:** All fixes were necessary for the integration test to compile. No scope creep.

## Issues Encountered
None beyond the auto-fixed blocking issues above.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- GameScenario API is complete and ready for Plans 02 (combat/SBA/keywords tests) and 03 (ETB/stack/layers/targeting tests)
- All 7 mechanic stub modules are in place -- just add test functions
- insta snapshot infrastructure is working (first snapshot created and accepted)

---
*Phase: 22-test-infrastructure*
*Completed: 2026-03-10*

## Self-Check: PASSED
- All 10 created files exist
- test_helpers.rs confirmed deleted
- Both task commits (7ee68ad, c8979cd) exist in git log
- SUMMARY.md exists
