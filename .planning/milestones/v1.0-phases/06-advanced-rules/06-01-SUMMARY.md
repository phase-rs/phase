---
phase: 06-advanced-rules
plan: 01
subsystem: engine
tags: [replacement-effects, game-events, pipeline, indexmap, proposed-event]

requires:
  - phase: 05-triggers-combat
    provides: trigger_definitions on GameObject, event-driven architecture
provides:
  - ProposedEvent enum with 13 typed variants and applied-set tracking
  - replace_event() pipeline with once-per-event enforcement and player choice
  - 35 Forge ReplacementType variants in registry (14 real, 21 stubs)
  - PendingReplacement on GameState for WaitingFor round-trip
  - GameObject base_* fields and timestamp for layer system
affects: [06-02-layer-system, 06-03-static-abilities]

tech-stack:
  added: [indexmap]
  patterns: [replacement-pipeline, proposed-event-interception, fn-pointer-registry]

key-files:
  created:
    - crates/engine/src/types/proposed_event.rs
    - crates/engine/src/game/replacement.rs
  modified:
    - crates/engine/Cargo.toml
    - crates/engine/src/types/mod.rs
    - crates/engine/src/types/game_state.rs
    - crates/engine/src/types/actions.rs
    - crates/engine/src/types/events.rs
    - crates/engine/src/game/game_object.rs
    - crates/engine/src/game/mod.rs
    - crates/engine/src/game/engine.rs

key-decisions:
  - "indexmap for deterministic replacement ordering in player choice scenarios"
  - "Flat replacement.rs file (not submodules) -- all 14 handlers inline until file grows unwieldy"
  - "affected_player() takes &GameState to look up controller from object store"
  - "Refactored pipeline_loop and apply_single_replacement helpers to eliminate code duplication between replace_event and continue_replacement"

patterns-established:
  - "ReplacementMatcher/ReplacementApplier fn pointer pair per handler type"
  - "ProposedEvent carries HashSet<ReplacementId> for once-per-event tracking"
  - "Pipeline loop: find candidates -> single auto-apply / multiple NeedsChoice -> depth cap"

requirements-completed: [REPL-01, REPL-02, REPL-03, REPL-04]

duration: 5min
completed: 2026-03-08
---

# Phase 06 Plan 01: Replacement Effect Pipeline Summary

**ProposedEvent interception pipeline with 14 handler implementations, once-per-event tracking, and player choice flow via WaitingFor round-trip**

## Performance

- **Duration:** 5 min
- **Started:** 2026-03-08T04:37:21Z
- **Completed:** 2026-03-08T04:42:51Z
- **Tasks:** 2
- **Files modified:** 10

## Accomplishments
- ProposedEvent enum with 13 variants covering all MTG mutation sites, each carrying applied-set for once-per-event enforcement
- replace_event() pipeline with depth cap (16), single-candidate auto-apply, multi-candidate NeedsChoice, and continue_replacement for player choice
- 14 core replacement handlers with real match/apply logic (Moved, DamageDone, Destroy, Draw, GainLife, LifeReduced, AddCounter, RemoveCounter, CreateToken, Discard, Tap, Untap, Sacrifice, Counter)
- 21 additional Forge ReplacementType variants registered as recognized stubs
- GameObject extended with replacement_definitions, static_definitions, base_* fields, and timestamp for layer system readiness

## Task Commits

1. **Task 1: ProposedEvent types, replacement pipeline, and GameState extensions** - `208c5a3` (feat)
2. **Task 2: Replacement handler implementations and pipeline tests** - `9524987` (feat)

## Files Created/Modified
- `crates/engine/src/types/proposed_event.rs` - ProposedEvent enum with 13 variants and ReplacementId
- `crates/engine/src/game/replacement.rs` - Pipeline, registry, 14 handler implementations, 8 tests
- `crates/engine/Cargo.toml` - Added indexmap dependency
- `crates/engine/src/types/game_state.rs` - PendingReplacement struct, WaitingFor::ReplacementChoice
- `crates/engine/src/types/actions.rs` - GameAction::ChooseReplacement
- `crates/engine/src/types/events.rs` - GameEvent::ReplacementApplied
- `crates/engine/src/game/game_object.rs` - replacement_definitions, static_definitions, base_*, timestamp
- `crates/engine/src/game/engine.rs` - ChooseReplacement dispatch in apply()
- `crates/engine/src/game/mod.rs` - pub mod replacement
- `crates/engine/src/types/mod.rs` - pub mod proposed_event, re-exports

## Decisions Made
- Used indexmap for deterministic replacement ordering -- HashMap iteration order is non-deterministic, which matters when presenting choices to players
- Kept all 14 handler implementations in a flat replacement.rs file rather than splitting into submodules -- simpler until the file grows unwieldy
- ProposedEvent::affected_player() takes &GameState reference to look up controller from object store (not just PlayerId field)
- Extracted pipeline_loop() and apply_single_replacement() helpers to eliminate code duplication between replace_event() and continue_replacement()

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed partial move in discard_applier and sacrifice_applier**
- **Found during:** Task 2 (handler implementations)
- **Issue:** Destructuring event to extract fields then trying to return the original event caused partial move errors
- **Fix:** Restructured to check params first, only destructure when needed for the modification path
- **Files modified:** crates/engine/src/game/replacement.rs
- **Committed in:** 9524987 (Task 2 commit)

---

**Total deviations:** 1 auto-fixed (1 bug)
**Impact on plan:** Ownership fix required by Rust's borrow checker. No scope creep.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Replacement pipeline ready for Plan 02 (Layer System) to use base_* fields and static_definitions
- All 358 engine tests pass (350 existing + 8 new replacement tests)

---
*Phase: 06-advanced-rules*
*Completed: 2026-03-08*
