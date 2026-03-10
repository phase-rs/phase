---
phase: 22-test-infrastructure
plan: 02
subsystem: testing
tags: [rust, integration-tests, sba, triggers, stack, targeting, game-engine]

# Dependency graph
requires:
  - phase: 22-test-infrastructure
    plan: 01
    provides: GameScenario builder, CardBuilder, GameRunner, ScenarioResult for test setup
provides:
  - ETB trigger scenario tests (3 scenarios covering ChangesZone triggers)
  - Stack resolution scenario tests (5 scenarios covering LIFO, priority, fizzle)
  - State-based action scenario tests (7 scenarios covering lethal damage, toughness, life, deathtouch, indestructible)
  - Targeting and fizzle scenario tests (4 scenarios covering fizzle, no-targets, hexproof, shroud)
affects: [22-03, future-rules-tests]

# Tech tracking
tech-stack:
  added: []
  patterns: [step-by-step act(PassPriority) for stack drain, direct GameState construction for deathtouch flag testing]

key-files:
  created: []
  modified:
    - crates/engine/tests/rules/etb.rs
    - crates/engine/tests/rules/stack.rs
    - crates/engine/tests/rules/sba.rs
    - crates/engine/tests/rules/targeting.rs

key-decisions:
  - "ChangesZone trigger tests verify Hand->Stack zone transition (not just ETB) because the engine fires triggers on all zone changes"
  - "Deathtouch SBA test uses direct GameState construction since GameRunner doesn't expose &mut state for setting dealt_deathtouch_damage flag"
  - "Stack drain uses explicit act(PassPriority) loop instead of resolve_top() to handle triggers that add new entries during resolution"

patterns-established:
  - "Step-by-step PassPriority loop for resolving stack with triggers: for _ in 0..20 { if stack.is_empty() { break; } runner.act(PassPriority); }"
  - "Direct GameState construction for testing SBA edge cases that require pre-set damage flags"
  - "TargetSelection handling pattern: if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) { runner.act(SelectTargets { targets }); }"

requirements-completed: [TEST-02]

# Metrics
duration: 12min
completed: 2026-03-10
---

# Phase 22 Plan 02: Rules Correctness Tests Summary

**19 integration tests covering ETB triggers, LIFO stack resolution, SBA lethal damage/zero-toughness/game-loss/deathtouch/indestructible, and targeting fizzle/hexproof/shroud mechanics -- all using GameScenario builder with zero filesystem dependencies**

## Performance

- **Duration:** 12 min
- **Started:** 2026-03-10T19:17:23Z
- **Completed:** 2026-03-10T19:29:23Z
- **Tasks:** 2
- **Files modified:** 4

## Accomplishments
- 3 ETB trigger tests verifying ChangesZone trigger firing, multiple triggers on same event, and triggered abilities placed on stack with priority
- 5 stack resolution tests proving LIFO order, active player priority after resolve, empty stack post-resolution, instant damage effect, and both-players-must-pass requirement
- 7 SBA tests covering lethal damage destruction, zero toughness, zero/negative life game loss, deathtouch lethality, indestructible prevention, and automatic SBA integration
- 4 targeting tests verifying fizzle on removed target, no-legal-targets cast prevention, hexproof vs opponent, and shroud vs all players

## Task Commits

Each task was committed atomically:

1. **Task 1: ETB trigger and stack resolution tests** - `3db53ed` (feat)
2. **Task 2: State-based action and targeting tests** - `4872a21` (feat)

## Files Created/Modified
- `crates/engine/tests/rules/etb.rs` - 3 ETB trigger scenarios (174 lines)
- `crates/engine/tests/rules/stack.rs` - 5 stack resolution scenarios (294 lines)
- `crates/engine/tests/rules/sba.rs` - 7 SBA scenarios (343 lines)
- `crates/engine/tests/rules/targeting.rs` - 4 targeting/fizzle scenarios (260 lines)

## Decisions Made
- ChangesZone triggers fire on any zone transition (Hand->Stack, Stack->Battlefield), not just ETB. Tests verify the trigger system works on actual zone changes rather than simulating a narrower ETB-only path.
- Deathtouch SBA test constructs a GameState directly rather than using GameScenario, because the `dealt_deathtouch_damage` flag on `GameObject` isn't settable through the builder API. This is the correct approach for testing SBA edge cases with pre-set internal state.
- Stack tests use explicit `act(PassPriority)` loops instead of `resolve_top()` for scenarios where triggers add new entries during resolution. `resolve_top()` only resolves until the stack shrinks below initial size, which can leave spell entries unresolved when triggers inflate the stack.

## Deviations from Plan

None - plan executed exactly as written. All scenarios adapted to actual engine API patterns discovered in Plan 01.

## Issues Encountered
- `resolve_top()` helper resolves only until stack size drops below initial count, which is insufficient when triggers add entries during resolution. Tests use explicit `PassPriority` loops instead. This is a known limitation of the helper, not a bug.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- All four test modules (ETB, stack, SBA, targeting) are populated with passing scenarios
- Plan 03 (combat, keywords, layers tests) can proceed using the same GameScenario patterns
- Pre-existing uncommitted changes in combat.rs and keywords.rs (from a separate session) have failing tests -- Plan 03 will need to address or replace those

---
*Phase: 22-test-infrastructure*
*Completed: 2026-03-10*

## Self-Check: PASSED
- All 4 test files exist (etb.rs, stack.rs, sba.rs, targeting.rs)
- SUMMARY.md exists
- Both task commits (3db53ed, 4872a21) exist in git log
