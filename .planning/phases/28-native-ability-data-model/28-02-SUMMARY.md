---
phase: 28-native-ability-data-model
plan: 02
subsystem: engine
tags: [rust, typed-enums, continuous-effects, layers, filter, static-abilities, replacement, deck-loading]

# Dependency graph
requires:
  - phase: 28-native-ability-data-model/01
    provides: "Typed definitions (TargetFilter, ContinuousModification, StaticDefinition, ReplacementDefinition, PtValue)"
provides:
  - "Typed filter matching via matches_target_filter(state, id, &TargetFilter, source_id)"
  - "Typed layer evaluation via ContinuousModification pattern matching"
  - "Typed static ability gathering via StaticDefinition fields"
  - "Typed replacement effect processing via ReplacementDefinition fields"
  - "svars-free deck loading with PtValue and Vec<Keyword>"
affects: [28-native-ability-data-model/03, 28-native-ability-data-model/04, 28-native-ability-data-model/05]

# Tech tracking
tech-stack:
  added: []
  patterns: ["TargetFilter enum matching replaces Forge string parsing", "ContinuousModification pattern match replaces HashMap key lookup", "ActiveContinuousEffect with typed modification field"]

key-files:
  created: []
  modified:
    - crates/engine/src/game/filter.rs
    - crates/engine/src/game/layers.rs
    - crates/engine/src/game/static_abilities.rs
    - crates/engine/src/game/replacement.rs
    - crates/engine/src/game/deck_loading.rs

key-decisions:
  - "Handler function signatures in static_abilities.rs still accept HashMap<String, String> — internal handler rewrite deferred to Plans 03-06"
  - "replacement.rs handler internals still use HashMap::new() placeholders — handler rewrite deferred to later plans"
  - "CmcGE computed inline from ManaCost fields since ManaCost has no cmc() method"
  - "player_matches_filter kept as string-based since it is only used in a few places and is minimal"

patterns-established:
  - "matches_target_filter pattern: public fn accepts &TargetFilter, delegates to filter_inner with resolved source_controller"
  - "ContinuousModification::layer() determines the CR 613 layer — no separate determine_layers_from_params function"
  - "StaticCondition evaluation: DevotionGE uses count_devotion, CheckSVar uses evaluate_compare"

requirements-completed: [NAT-01, NAT-04]

# Metrics
duration: 13min
completed: 2026-03-11
---

# Phase 28 Plan 02: Continuous Effects Subsystem Summary

**Typed TargetFilter matching, ContinuousModification pattern dispatch in layers, and svars-free deck loading replacing all Forge-style string parsing in the continuous effects pipeline**

## Performance

- **Duration:** 13 min
- **Started:** 2026-03-11T06:00:48Z
- **Completed:** 2026-03-11T06:14:30Z
- **Tasks:** 3
- **Files modified:** 5

## Accomplishments
- Rewrote filter.rs to match against TargetFilter enum variants instead of parsing Forge-style filter strings — zero string parsing remaining
- Rewrote layers.rs to use ContinuousModification pattern matching instead of HashMap string-key lookups — deleted determine_layers_from_params entirely
- Updated static_abilities.rs to read typed StaticDefinition fields (affected, modifications, condition) instead of params HashMap
- Updated replacement.rs tests to use typed ReplacementDefinition construction
- Rewrote deck_loading.rs to use PtValue enum and Vec<Keyword> directly, removing svars propagation

## Task Commits

Each task was committed atomically:

1. **Task 1: Rewrite filter.rs for typed TargetFilter matching** - `3ddc6a7a` (feat)
2. **Task 2: Rewrite layers.rs, static_abilities.rs, replacement.rs for typed definitions** - `de09cd69` (feat)
3. **Task 3: Update deck_loading.rs and remove svars propagation** - `82c42a74` (feat)

**Plan metadata:** `4106fe55` (docs: complete plan)

## Files Created/Modified
- `crates/engine/src/game/filter.rs` - Complete rewrite: matches_target_filter with TargetFilter enum, matches_type_filter, matches_filter_prop, filter_inner recursive dispatch
- `crates/engine/src/game/layers.rs` - Complete rewrite: gather_active_continuous_effects reads typed StaticDefinition, apply_continuous_effect pattern-matches ContinuousModification, depends_on checks typed variants
- `crates/engine/src/game/static_abilities.rs` - check_static_ability reads typed def.affected, static_filter_matches accepts &TargetFilter, StaticEffect::Continuous simplified to unit variant
- `crates/engine/src/game/replacement.rs` - Handler calls use HashMap::new() placeholder; all tests rewritten with make_repl() helper using typed ReplacementDefinition
- `crates/engine/src/game/deck_loading.rs` - parse_pt matches PtValue enum, keywords cloned directly from Vec<Keyword>, svars propagation removed

## Decisions Made
- Handler function signatures in static_abilities.rs still accept `HashMap<String, String>` — rewriting handler internals deferred to Plans 03-06 where individual effect modules are migrated
- replacement.rs handler internals still use `HashMap::new()` as placeholder — handler bodies need the effect module rewrite (Plan 03+) to become fully typed
- CMC computed inline from ManaCost.generic + ManaCost.shards.len() since ManaCost lacks a cmc() method
- player_matches_filter kept as string-based for now — minimal usage, not worth a full typed enum at this stage

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed ManaCost missing cmc() method**
- **Found during:** Task 1 (filter.rs rewrite)
- **Issue:** FilterProp::CmcGE handler used `obj.mana_cost.cmc()` but ManaCost has no cmc() method
- **Fix:** Computed CMC inline: `obj.mana_cost.generic as usize + obj.mana_cost.shards.len()`
- **Files modified:** crates/engine/src/game/filter.rs
- **Committed in:** 3ddc6a7a

**2. [Rule 3 - Blocking] Fixed unused imports across multiple files**
- **Found during:** Task 2 (layers.rs, static_abilities.rs rewrite)
- **Issue:** TypeFilter, CoreType, Keyword, ManaColor unused in layers.rs; player_matches_filter unused in static_abilities.rs; ContinuousModification unused in static_abilities test module
- **Fix:** Removed unused imports, added missing Keyword import to layers.rs test module
- **Files modified:** crates/engine/src/game/layers.rs, crates/engine/src/game/static_abilities.rs
- **Committed in:** de09cd69

---

**Total deviations:** 2 auto-fixed (1 bug, 1 blocking)
**Impact on plan:** Both fixes necessary for compilation. No scope creep.

## Issues Encountered
- Crate has ~271 compilation errors in other consumer files (effects/, triggers.rs, parser/, etc.) from Plan 01's type changes. These are expected and documented — this plan only targets the continuous effects subsystem files. Tests cannot be run in isolation because the crate won't compile; verification was done by checking specific files for compilation errors via compiler output filtering.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Continuous effects pipeline is fully typed on the reading side
- Plans 03-06 can now migrate effect handler internals, triggers, and casting — the pattern for reading typed definitions is established
- Handler function bodies in static_abilities.rs and replacement.rs are the primary remaining HashMap consumers for this subsystem

## Self-Check: PASSED

All 5 modified files exist. All 3 task commits verified (3ddc6a7a, de09cd69, 82c42a74).

---
*Phase: 28-native-ability-data-model*
*Completed: 2026-03-11*
