---
phase: 25-forge-removal-relicensing
plan: 01
subsystem: engine
tags: [dispatch, enums, typed-matching, effect-handlers, static-abilities, replacement-effects]

# Dependency graph
requires:
  - phase: 21-schema-mtgjson-foundation
    provides: "Typed enums (Effect, StaticMode, ReplacementEvent, TriggerMode) and compat bridge methods"
provides:
  - "effect_variant_name() standalone function for human-readable effect names"
  - "Static ability registry using HashMap<StaticMode, StaticAbilityHandler>"
  - "Replacement effect registry using IndexMap<ReplacementEvent, ReplacementHandlerEntry>"
  - "All production code dispatch uses typed enums, no compat bridge methods"
affects: [25-02-forge-compat-gating, 25-03-license-cleanup]

# Tech tracking
tech-stack:
  added: []
  patterns: ["effect_variant_name() for variant-to-string mapping", "direct enum field access for registry lookup"]

key-files:
  created: []
  modified:
    - "crates/engine/src/types/ability.rs"
    - "crates/engine/src/game/effects/*.rs (27 files)"
    - "crates/engine/src/game/static_abilities.rs"
    - "crates/engine/src/game/replacement.rs"
    - "crates/engine/src/game/combat.rs"
    - "crates/engine/src/game/layers.rs"
    - "crates/engine/src/game/coverage.rs"
    - "crates/engine/src/game/casting.rs"
    - "crates/engine/src/game/mana_abilities.rs"
    - "crates/engine/src/game/planeswalker.rs"
    - "crates/engine/src/game/triggers.rs"

key-decisions:
  - "effect_variant_name() as standalone function (not method) for production variant-to-string mapping"
  - "to_params() on Effect stays ungated -- legitimate typed-to-HashMap serialization for SubAbility chains"
  - "Test code keeps compat bridge methods (api_type, params, from_raw) -- gated in Plan 02"

patterns-established:
  - "effect_variant_name(&ability.effect) for GameEvent::EffectResolved api_type strings"
  - "Direct enum field access (def.mode, def.event) instead of string bridge methods"
  - "effect.to_params() + remaining_params merge for building ResolvedAbility params"

requirements-completed: [MIGR-02]

# Metrics
duration: 13min
completed: 2026-03-11
---

# Phase 25 Plan 01: Typed Dispatch Migration Summary

**All engine dispatch converted from string-based HashMap lookup to typed enum pattern matching across 40+ files**

## Performance

- **Duration:** 13 min
- **Started:** 2026-03-11T00:00:17Z
- **Completed:** 2026-03-11T00:14:17Z
- **Tasks:** 2
- **Files modified:** 43

## Accomplishments
- Created `effect_variant_name()` standalone function and replaced 41 `ability.api_type()` calls across 27 effect handler files
- Converted static ability registry from `HashMap<String, StaticAbilityHandler>` to `HashMap<StaticMode, StaticAbilityHandler>` with 63 typed entries
- Converted replacement effect registry from `IndexMap<String, ReplacementHandlerEntry>` to `IndexMap<ReplacementEvent, ReplacementHandlerEntry>` with 35 typed entries
- Eliminated all `mode_str()`, `event_str()`, and `params()` compat bridge calls from production code paths
- All 830+ tests pass across all crates, zero clippy warnings

## Task Commits

Each task was committed atomically:

1. **Task 1: Create effect_variant_name() and convert effect handlers + registries** - `c7b74e89` (feat)
2. **Task 2: Convert params() and remaining api_type() calls in casting, mana, planeswalker, triggers** - `2bc616df` (feat)

## Files Created/Modified
- `crates/engine/src/types/ability.rs` - Added `effect_variant_name()` standalone function
- `crates/engine/src/game/effects/*.rs` (27 files) - Replaced `ability.api_type()` with `effect_variant_name(&ability.effect)`
- `crates/engine/src/game/effects/mod.rs` - Converted `def.params()` to `effect.to_params()` + remaining_params merge
- `crates/engine/src/game/effects/counter.rs` - Replaced `mode_str()` with direct `StaticMode` comparison
- `crates/engine/src/game/static_abilities.rs` - Registry: `HashMap<StaticMode, ...>`, `check_static_ability` uses parsed `StaticMode`
- `crates/engine/src/game/replacement.rs` - Registry: `IndexMap<ReplacementEvent, ...>`, pipeline uses direct `.event` field
- `crates/engine/src/game/combat.rs` - Replaced `mode_str()` with `StaticMode::Other("CantBeBlocked".into())`
- `crates/engine/src/game/layers.rs` - Replaced `mode_str()` with `def.mode.to_string()`
- `crates/engine/src/game/coverage.rs` - Updated `check_statics` to use `HashMap<StaticMode, ...>`, `check_abilities` to use `effect_variant_name()`
- `crates/engine/src/game/casting.rs` - Replaced `ability_def.params()` with `effect.to_params()` + remaining_params
- `crates/engine/src/game/mana_abilities.rs` - Replaced `api_type() == "Mana"` with `matches!(Effect::Mana)`
- `crates/engine/src/game/planeswalker.rs` - Replaced `ability_def.params()` with `effect.to_params()` + remaining_params
- `crates/engine/src/game/triggers.rs` - Replaced `ability_def.params()` with `effect.to_params()` + remaining_params

## Decisions Made
- `effect_variant_name()` is a standalone function (not a method on Effect) to clearly separate it from the compat `api_type()` method
- `Effect::to_params()` remains ungated as it's a legitimate serialization method used by SubAbility chain resolution
- Test code retains compat bridge methods (api_type, params, from_raw) -- these will be feature-gated behind `forge-compat` in Plan 02
- Pre-existing clippy warnings in parser/ability.rs (if_same_then_else) and bin/migrate.rs (redundant_closure) fixed inline

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Fixed pre-existing clippy warning in parser/ability.rs**
- **Found during:** Task 1 (clippy verification)
- **Issue:** `if_same_then_else` warning: "T" and "Untap"/"Q" branches had identical bodies
- **Fix:** Combined the conditions into a single `if comp == "T" || comp == "Untap" || comp == "Q"` branch
- **Files modified:** crates/engine/src/parser/ability.rs
- **Verification:** `cargo clippy --all-targets -- -D warnings` passes clean
- **Committed in:** c7b74e89 (Task 1 commit)

**2. [Rule 3 - Blocking] Fixed pre-existing clippy warning in bin/migrate.rs**
- **Found during:** Task 1 (clippy verification)
- **Issue:** `redundant_closure` warning: `.map(|v| face_to_abilities(v))` should be `.map(face_to_abilities)`
- **Fix:** Simplified the closure to a function reference
- **Files modified:** crates/engine/src/bin/migrate.rs
- **Verification:** `cargo clippy --all-targets -- -D warnings` passes clean
- **Committed in:** c7b74e89 (Task 1 commit)

---

**Total deviations:** 2 auto-fixed (2 blocking -- pre-existing clippy warnings that block CI)
**Impact on plan:** Both fixes are trivial, out-of-scope pre-existing issues that blocked clippy -D warnings. No scope creep.

## Issues Encountered
None - plan executed cleanly.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- All production dispatch uses typed enums; compat bridge methods (api_type, mode_str, event_str, params) only called from test code
- Ready for Plan 02: feature-gating compat methods behind `forge-compat` cfg flag
- Test code using compat methods identified in 6 files: engine.rs, triggers.rs, scenario.rs, transform.rs, copy_spell.rs (all in `#[cfg(test)]` blocks)

---
*Phase: 25-forge-removal-relicensing*
*Completed: 2026-03-11*

## Self-Check: PASSED
- All key files exist
- Both task commits verified (c7b74e89, 2bc616df)
- effect_variant_name() function confirmed in types/ability.rs
- HashMap<StaticMode, ...> confirmed in static_abilities.rs
- IndexMap<ReplacementEvent, ...> confirmed in replacement.rs
