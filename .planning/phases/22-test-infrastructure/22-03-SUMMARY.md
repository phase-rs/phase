---
phase: 22-test-infrastructure
plan: 03
subsystem: testing
tags: [rust, integration-tests, insta, snapshot, combat, keywords, layers, game-engine]

# Dependency graph
requires:
  - phase: 22-test-infrastructure
    plan: 01
    provides: GameScenario builder, CardBuilder, GameRunner, GameSnapshot, integration test scaffolding
provides:
  - Combat damage integration tests (unblocked, blocked, first strike, double strike, defender, multi-attacker)
  - Keyword interaction integration tests (deathtouch+trample, lifelink, flying/reach, trample+lifelink, vigilance, first strike+lifelink)
  - Layer system integration tests (set-before-modify ordering, lord stacking, counters, timestamp ordering, type-change-before-PT)
  - 7 insta snapshot golden masters as regression anchors
  - CardBuilder base_keywords fix (keywords survive layer evaluation)
  - GameRunner::snapshot() for step-by-step test snapshots
  - CardBuilder::with_continuous_static() for parameterized lord effects
  - CardBuilder::with_plus_counters()/with_minus_counters() for counter tests
affects: [future-rules-tests, 22-02]

# Tech tracking
tech-stack:
  added: []
  patterns: [run_combat() helper for driving full combat cycle through engine, lord_params()/set_pt_params() helpers for layer test setup]

key-files:
  created:
    - crates/engine/tests/rules/snapshots/rules__combat__combat_first_strike_kills_before_regular.snap
    - crates/engine/tests/rules/snapshots/rules__combat__combat_multiple_attackers_mixed_blocking.snap
    - crates/engine/tests/rules/snapshots/rules__keywords__keywords_deathtouch_trample_damage_assignment.snap
    - crates/engine/tests/rules/snapshots/rules__keywords__keywords_trample_lifelink_excess.snap
    - crates/engine/tests/rules/snapshots/rules__layers__layers_set_then_modify.snap
    - crates/engine/tests/rules/snapshots/rules__layers__layers_timestamp_ordering.snap
    - crates/engine/tests/rules/snapshots/rules__layers__layers_type_change_before_pt.snap
  modified:
    - crates/engine/tests/rules/combat.rs
    - crates/engine/tests/rules/keywords.rs
    - crates/engine/tests/rules/layers.rs
    - crates/engine/src/game/scenario.rs

key-decisions:
  - "CardBuilder must push keywords to both keywords and base_keywords to survive layer evaluation -- this was a bug fix, not a design choice"
  - "Combat integration tests use run_combat() helper that drives the full engine pipeline (PassPriority -> DeclareAttackers -> DeclareBlockers -> auto-resolve)"
  - "Layer tests trigger evaluation by calling PassPriority (which runs SBAs, which evaluate layers when layers_dirty=true)"
  - "GameRunner::snapshot() added for snapshot tests in step-by-step mode (ScenarioResult::snapshot() only available after build_and_run)"

patterns-established:
  - "run_combat(runner, attacker_ids, blocker_assignments) for driving combat through the engine"
  - "lord_params(filter, add_power, add_toughness) for creating lord static effect params"
  - "set_pt_params(filter, set_power, set_toughness) for creating set-P/T static effect params"
  - "PassPriority as trigger for layer evaluation in integration tests"

requirements-completed: [TEST-02, TEST-03]

# Metrics
duration: 15min
completed: 2026-03-10
---

# Phase 22 Plan 03: Combat, Keywords & Layers Integration Tests Summary

**22 integration tests with 7 insta snapshot golden masters covering combat damage, keyword interactions (deathtouch+trample, lifelink, flying/reach), and layer system ordering (CR 613) -- plus CardBuilder base_keywords bug fix enabling keywords to survive layer evaluation**

## Performance

- **Duration:** 15 min
- **Started:** 2026-03-10T19:17:05Z
- **Completed:** 2026-03-10T19:32:05Z
- **Tasks:** 2
- **Files modified:** 11

## Accomplishments
- 7 combat integration tests covering unblocked damage, blocked exchange, first strike ordering, double strike, defender restriction, multi-attacker scenarios, attacker tapping
- 7 keyword interaction tests covering deathtouch+trample damage assignment, lifelink, first strike+lifelink, flying/reach blocking, trample+lifelink excess, vigilance
- 8 layer system tests covering set-before-modify ordering, lord stacking, counter sublayers, lord+counter interaction, timestamp ordering, layer reset, type-change before P/T
- Fixed critical CardBuilder bug: keywords were only pushed to `keywords` (not `base_keywords`), causing them to be wiped during layer evaluation
- 7 insta snapshot golden masters for complex multi-keyword and multi-layer scenarios

## Task Commits

Each task was committed atomically:

1. **Task 1: Combat damage and keyword interaction tests** - `d09f95b` (feat)
2. **Task 2: Layer system tests and final snapshot verification** - `0052a8c` (feat)

## Files Created/Modified
- `crates/engine/tests/rules/combat.rs` - 7 combat damage integration tests with run_combat() helper
- `crates/engine/tests/rules/keywords.rs` - 7 keyword interaction tests (deathtouch+trample, lifelink, flying/reach, vigilance)
- `crates/engine/tests/rules/layers.rs` - 8 layer system tests (set/modify ordering, lords, counters, timestamps)
- `crates/engine/src/game/scenario.rs` - Fixed base_keywords in CardBuilder, added GameRunner::snapshot(), with_continuous_static(), with_plus_counters(), with_minus_counters()
- `crates/engine/tests/rules/snapshots/*.snap` - 7 new insta snapshot golden masters

## Decisions Made
- CardBuilder must push keywords to both `keywords` and `base_keywords` -- discovered as a bug when first strike/lifelink/defender keywords were silently wiped by layer evaluation
- Combat integration tests drive the full engine pipeline via `run_combat()` helper rather than calling internal functions directly, ensuring the full action-resolution-SBA cycle is exercised
- Layer tests trigger evaluation indirectly via `PassPriority` (which triggers SBA check, which evaluates layers when `layers_dirty = true`), matching the real game flow
- Added `GameRunner::snapshot()` for step-by-step test snapshots since `ScenarioResult::snapshot()` is only available after `build_and_run()`

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] CardBuilder keywords not persisted in base_keywords**
- **Found during:** Task 1 (first strike combat test)
- **Issue:** `CardBuilder::first_strike()` (and all keyword methods) pushed to `obj.keywords` but not `obj.base_keywords`. When `evaluate_layers()` ran (triggered by `layers_dirty = true` in SBA check), it reset `keywords = base_keywords.clone()`, wiping all CardBuilder-set keywords. This caused: first strike creatures not getting first strike damage, defenders able to attack, lifelink not healing, etc.
- **Fix:** Added `push_keyword()` helper that pushes to both `keywords` and `base_keywords`. All keyword convenience methods and `with_keyword()` now use this helper.
- **Files modified:** crates/engine/src/game/scenario.rs
- **Verification:** All 14 combat/keyword tests pass; 643 unit tests pass with no regressions
- **Committed in:** d09f95b (Task 1 commit)

**2. [Rule 2 - Missing] GameRunner::snapshot() method**
- **Found during:** Task 1 (snapshot tests in step-by-step mode)
- **Issue:** `GameSnapshot::from_state()` is private; `snapshot()` only available on `ScenarioResult`. Step-by-step tests using `GameRunner::act()` had no way to produce snapshots.
- **Fix:** Added `pub fn snapshot(&self) -> GameSnapshot` to `GameRunner` that calls the private `from_state()` with empty events.
- **Files modified:** crates/engine/src/game/scenario.rs
- **Committed in:** d09f95b (Task 1 commit)

**3. [Rule 2 - Missing] CardBuilder::with_continuous_static() for parameterized lord effects**
- **Found during:** Task 2 (layer test setup)
- **Issue:** Existing `with_static()` creates definitions with empty params. Layer tests need params like Affected, AddPower, AddToughness.
- **Fix:** Added `with_continuous_static(params: HashMap<String, String>)` to CardBuilder.
- **Files modified:** crates/engine/src/game/scenario.rs
- **Committed in:** 0052a8c (Task 2 commit)

**4. [Rule 2 - Missing] CardBuilder counter methods**
- **Found during:** Task 2 (counter sublayer tests)
- **Issue:** No way to add +1/+1 or -1/-1 counters via CardBuilder.
- **Fix:** Added `with_plus_counters(count)` and `with_minus_counters(count)`.
- **Files modified:** crates/engine/src/game/scenario.rs
- **Committed in:** 0052a8c (Task 2 commit)

---

**Total deviations:** 4 auto-fixed (1 bug fix, 3 missing functionality)
**Impact on plan:** Bug fix was critical for test correctness. Missing functionality additions were necessary for the test harness to support the planned test scenarios. No scope creep.

## Issues Encountered
None beyond the auto-fixed issues above.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- All 7 rules test modules (etb, combat, stack, sba, layers, keywords, targeting) now have tests
- 42 total integration tests across all modules
- 7 insta snapshot golden masters as regression anchors
- CardBuilder API is complete for all current test patterns
- Test infrastructure phase (22) is complete pending Plan 22-02

---
*Phase: 22-test-infrastructure*
*Completed: 2026-03-10*

## Self-Check: PASSED
- All 10 created/modified files exist
- Both task commits (d09f95b, 0052a8c) exist in git log
- SUMMARY.md exists
