---
phase: 06-advanced-rules
plan: 02
subsystem: engine
tags: [layers, continuous-effects, static-abilities, petgraph, dependency-ordering]

requires:
  - phase: 06-advanced-rules
    provides: base_* fields on GameObject, static_definitions, timestamp for layer system
provides:
  - Layer enum with 11 variants covering all 7 layers and P/T sublayers
  - evaluate_layers() function with per-layer processing and petgraph dependency ordering
  - Static ability handler registry with 63 registered modes (15 core + 47 stubs)
  - check_static_ability() utility for rule-modification checks
  - layers_dirty flag and next_timestamp on GameState
affects: [06-03-static-abilities, 07-ui]

tech-stack:
  added: [petgraph]
  patterns: [seven-layer-evaluation, dependency-aware-toposort, fn-pointer-static-registry]

key-files:
  created:
    - crates/engine/src/types/layers.rs
    - crates/engine/src/game/layers.rs
    - crates/engine/src/game/static_abilities.rs
  modified:
    - crates/engine/Cargo.toml
    - crates/engine/src/types/mod.rs
    - crates/engine/src/types/game_state.rs
    - crates/engine/src/game/mod.rs

key-decisions:
  - "petgraph DiGraph for dependency ordering with toposort fallback on cycles (CR 613.8)"
  - "Timestamp + source_id + def_index as deterministic sort key within layers"
  - "object_matches_filter duplicated in layers.rs (not extracted to shared) to avoid coupling with triggers.rs"
  - "StaticAbilityHandler returns Vec<StaticEffect> for both Continuous and RuleModification modes"

patterns-established:
  - "Layer evaluation resets computed to base then applies all effects top-down"
  - "Counter P/T applied as final step after all layer effects"
  - "Static registry built per call (cheap HashMap, same pattern as effect/trigger registries)"

requirements-completed: [STAT-01, STAT-02, STAT-03, STAT-04]

duration: 5min
completed: 2026-03-08
---

# Phase 06 Plan 02: Layer System and Static Ability Registry Summary

**Seven-layer continuous effect evaluation with petgraph dependency ordering and 63-mode static ability handler registry**

## Performance

- **Duration:** 5 min
- **Started:** 2026-03-08T04:44:58Z
- **Completed:** 2026-03-08T04:50:20Z
- **Tasks:** 2
- **Files modified:** 7

## Accomplishments
- Layer enum with 11 variants (Copy, Control, Text, Type, Color, Ability, CharDef, SetPT, ModifyPT, SwitchPT, CounterPT) processing all 7 MTG layers in correct order
- evaluate_layers() resets computed characteristics to base values then applies effects per-layer with dependency-aware ordering via petgraph toposort
- Static ability handler registry with 63 modes: 1 Continuous, 15 core rule-modification handlers, 47 stubs
- check_static_ability() utility for rule-modification checks (CantAttack, CantBlock, etc.) at game decision points

## Task Commits

1. **Task 1: Layer types, evaluate_layers() with dependency ordering, and layers_dirty flag** - `9b476c7` (feat)
2. **Task 2: Static ability handler registry with core continuous and rule-modification handlers** - `7170351` (feat)

## Files Created/Modified
- `crates/engine/src/types/layers.rs` - Layer enum, ActiveContinuousEffect, ContinuousEffectApplication
- `crates/engine/src/game/layers.rs` - evaluate_layers(), dependency ordering, 7 tests
- `crates/engine/src/game/static_abilities.rs` - Registry, check_static_ability(), 4 tests
- `crates/engine/Cargo.toml` - Added petgraph dependency
- `crates/engine/src/types/game_state.rs` - layers_dirty flag, next_timestamp field and helper
- `crates/engine/src/types/mod.rs` - pub mod layers, re-exports
- `crates/engine/src/game/mod.rs` - pub mod layers, pub mod static_abilities, pub use evaluate_layers

## Decisions Made
- Used petgraph DiGraph for dependency ordering with automatic fallback to timestamp ordering on cycle detection per CR 613.8
- Deterministic sort key: (timestamp, source_id.0, def_index) ensures consistent ordering even with identical timestamps
- Duplicated object filter matching in layers.rs rather than extracting shared utility with triggers.rs -- avoids coupling and keeps each module self-contained
- StaticAbilityHandler returns Vec<StaticEffect> enum covering both Continuous (layer params) and RuleModification (mode string) effects

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed borrow checker violations in test helpers**
- **Found during:** Task 1 (layer tests)
- **Issue:** state.next_timestamp() borrows state mutably while obj reference holds mutable borrow
- **Fix:** Extracted timestamp to local variable before borrowing objects
- **Files modified:** crates/engine/src/game/layers.rs
- **Committed in:** 9b476c7 (Task 1 commit)

---

**Total deviations:** 1 auto-fixed (1 bug)
**Impact on plan:** Standard Rust borrow checker fix. No scope creep.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Layer system ready for integration with game engine (evaluate_layers on SBA or state change)
- Static ability checks ready for use in combat, casting, and targeting modules
- All 372 engine tests pass (358 existing + 14 new)

---
*Phase: 06-advanced-rules*
*Completed: 2026-03-08*
