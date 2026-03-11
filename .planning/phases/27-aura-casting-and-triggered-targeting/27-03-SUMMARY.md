---
phase: 27-aura-casting-and-triggered-targeting
plan: 03
subsystem: engine
tags: [triggers, targeting, exile, exile-return, etb, stack, game-loop]

# Dependency graph
requires:
  - phase: 27-01
    provides: "Type contracts: WaitingFor::TriggerTargetSelection, ExileLink, PendingTrigger, Duration::UntilHostLeavesPlay"
provides:
  - "Triggered ability target selection with auto-target/multi-target/skip"
  - "ExileLink recording on exile with UntilHostLeavesPlay duration"
  - "Automatic exile return when source leaves battlefield"
  - "check_exile_returns integrated into apply loop (after SBAs, before triggers)"
affects: [27-04, 27-05, aura-casting, oblivion-ring, banishing-light]

# Tech tracking
tech-stack:
  added: []
  patterns: ["extract_target_filter_from_effect for targeting detection", "check_exile_returns in apply loop for exile return", "ExileLink tracking pattern"]

key-files:
  created: []
  modified:
    - crates/engine/src/game/triggers.rs
    - crates/engine/src/game/engine.rs
    - crates/engine/src/game/effects/change_zone.rs

key-decisions:
  - "extract_target_filter_from_effect excludes SelfRef/Controller/None as non-targeting (no player choice needed)"
  - "Multi-target triggers set pending_trigger and return early from process_triggers; remaining triggers processed after selection"
  - "check_exile_returns placed after SBAs and before triggers so returned permanents get ETB triggers"
  - "Exile return only fires for cards still in exile zone; graceful no-op for already-moved cards"

patterns-established:
  - "Targeting detection via extract_target_filter_from_effect: reusable for any effect targeting analysis"
  - "Exile return ordering: SBAs -> exile returns -> triggers in the apply loop"
  - "Auto-target pattern: single legal target auto-selects without player prompt"

requirements-completed: [P27-TRIG, P27-EXILE, P27-TEST]

# Metrics
duration: 13min
completed: 2026-03-11
---

# Phase 27 Plan 03: Triggered Targeting and Exile Return Summary

**Triggered ability target selection with auto/multi/skip logic and ExileLink-based exile return tracking for "until leaves battlefield" effects**

## Performance

- **Duration:** 13 min
- **Started:** 2026-03-11T08:36:03Z
- **Completed:** 2026-03-11T08:49:00Z
- **Tasks:** 2
- **Files modified:** 3

## Accomplishments
- Triggered abilities with targeting requirements pause for player selection (WaitingFor::TriggerTargetSelection)
- Auto-target optimization: single legal target skips player prompt and goes directly to stack
- Zero legal targets: trigger skipped entirely (not placed on stack)
- ExileLink recording when ChangeZone exiles with UntilHostLeavesPlay duration
- Automatic exile return when source permanent leaves the battlefield
- Correct ordering: SBAs -> exile returns -> triggers (returned permanents get ETB triggers)
- Full workspace passes: 774 tests, clippy clean

## Task Commits

Each task was committed atomically:

1. **Task 1: Triggered ability target selection** - `0d6fec3c` (test) + `b7948a9a` (feat)
2. **Task 2: Exile return tracking** - `5ece8d6d` (test) + `f5fc99e1` (feat)

_TDD: Each task has separate RED (test) and GREEN (implementation) commits._

## Files Created/Modified
- `crates/engine/src/game/triggers.rs` - Added extract_target_filter_from_effect(), modified process_triggers loop for targeting detection, added targeting import
- `crates/engine/src/game/engine.rs` - Added TriggerTargetSelection handler, pending_trigger check in apply loop, check_exile_returns function, exile_return integration in apply loop
- `crates/engine/src/game/effects/change_zone.rs` - ExileLink recording on exile with UntilHostLeavesPlay duration

## Decisions Made
- extract_target_filter_from_effect excludes SelfRef/Controller/None as non-targeting effects (no player choice needed)
- Multi-target triggers return early from process_triggers; remaining triggers deferred until after target selection
- check_exile_returns placed between SBAs and triggers so returned permanents generate their own ETB triggers
- Exile return gracefully handles cards already moved from exile (no panic, no-op)

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed clippy unnecessary_unwrap warning**
- **Found during:** Task 2
- **Issue:** `if state.pending_trigger.is_some() { let trigger = state.pending_trigger.as_ref().unwrap(); }` triggers clippy unnecessary_unwrap
- **Fix:** Refactored to `if let Some(trigger) = state.pending_trigger.as_ref()`
- **Files modified:** crates/engine/src/game/engine.rs
- **Committed in:** f5fc99e1

**2. [Rule 1 - Bug] Removed unused imports in test modules**
- **Found during:** Task 2
- **Issue:** AbilityDefinition, AbilityKind, ObjectId imports unused after test refactoring
- **Fix:** Removed unused imports, kept CoreType which was needed
- **Files modified:** crates/engine/src/game/engine.rs
- **Committed in:** f5fc99e1

---

**Total deviations:** 2 auto-fixed (2 bugs/lint)
**Impact on plan:** Trivial lint fixes. No scope creep.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Triggered targeting enables ETB exile effects (Banishing Light, Sheltered by Ghosts)
- ExileLink system enables "until leaves" return pattern for 50+ cards
- Plan 04 (if any) can build on these primitives for full aura/trigger integration tests

---
*Phase: 27-aura-casting-and-triggered-targeting*
*Completed: 2026-03-11*
