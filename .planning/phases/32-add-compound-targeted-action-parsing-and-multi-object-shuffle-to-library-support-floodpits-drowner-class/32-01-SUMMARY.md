---
phase: 32-add-compound-targeted-action-parsing-and-multi-object-shuffle-to-library-support-floodpits-drowner-class
plan: 01
subsystem: parser
tags: [oracle-parser, compound-effects, target-filter, sub-ability, anaphoric-reference]

requires:
  - phase: 28-native-ability-data-model
    provides: "TargetFilter enum, AbilityDefinition sub_ability chain, Effect types"
  - phase: 31-kaito-bane-of-nightmares
    provides: "StaticCondition combinators, animation parser infrastructure"
provides:
  - "TargetFilter::ParentTarget variant for anaphoric 'it'/'that creature' references"
  - "Effect::ChangeZone owner_library: bool field for CR 400.7 owner library routing"
  - "try_split_targeted_compound() compound effect splitter"
  - "has_anaphoric_reference() pronoun detection for compound sub-effects"
  - "replace_target_with_parent() target rewriting for ParentTarget emission"
  - "find_compound_connector() for ' and ' / ', then ' splitting"
affects: [32-02, floodpits-drowner, shuffle-to-library, compound-effects]

tech-stack:
  added: []
  patterns:
    - "Compound targeted-action splitting via try_split_targeted_compound in lower_imperative_clause"
    - "Anaphoric reference detection with has_anaphoric_reference() for word-boundary matching"
    - "ParentTarget as no-op in targeting (inherits parent targets via sub_ability chain)"

key-files:
  created: []
  modified:
    - "crates/engine/src/types/ability.rs"
    - "crates/engine/src/parser/oracle_effect.rs"
    - "crates/engine/src/parser/oracle_target.rs"
    - "crates/engine/src/game/filter.rs"
    - "crates/engine/src/game/targeting.rs"
    - "crates/engine/src/game/triggers.rs"
    - "crates/engine/src/game/coverage.rs"

key-decisions:
  - "Used existing CountersGE with string counter_type instead of new HasCounter with typed CounterType (avoids types->game cross-dependency)"
  - "ParentTarget is no-op in find_legal_targets; sub_ability chain already propagates parent targets when sub.targets is empty"
  - "ZoneCounterProxy variant on TargetedImperativeAst bridges zone-counter family into compound splitting"
  - "has_anaphoric_reference uses word-boundary matching instead of contains_object_pronoun (handles end-of-string pronouns)"

patterns-established:
  - "Compound splitting: try_split_targeted_compound intercepts in lower_imperative_clause before standard parse"
  - "Anaphoric ParentTarget: sub-effects with 'it'/'them' get target replaced with ParentTarget"

requirements-completed: [BB-COUNTER, BB-COMPOUND]

duration: 42min
completed: 2026-03-17
---

# Phase 32 Plan 01: Compound Targeted-Action Parsing Summary

**ParentTarget variant, ChangeZone owner_library field, and compound targeted-action splitter for "tap X and put counter on it" patterns**

## Performance

- **Duration:** 42 min
- **Started:** 2026-03-17T16:27:37Z
- **Completed:** 2026-03-17T17:09:24Z
- **Tasks:** 2
- **Files modified:** 19

## Accomplishments
- Added TargetFilter::ParentTarget for compound sub-ability anaphoric references ("it"/"that creature")
- Added owner_library: bool to Effect::ChangeZone for owner's library routing per CR 400.7
- Built try_split_targeted_compound() that splits "tap target X and put counter on it" into Tap + PutCounter(ParentTarget) chain
- Existing CountersGE filter and parse_counter_suffix confirmed working with new tests

## Task Commits

Each task was committed atomically:

1. **Task 1: Type definitions + counter filter parser + filter runtime matching** - `dc3d8c1d4` (feat)
2. **Task 2: Compound effect splitter + ParentTarget emission in parser** - `e8754df8e` (feat)

## Files Created/Modified
- `crates/engine/src/types/ability.rs` - Added ParentTarget, owner_library field
- `crates/engine/src/parser/oracle_effect.rs` - Compound splitter, anaphoric detection, target rewriting
- `crates/engine/src/parser/oracle_target.rs` - Counter suffix parser tests
- `crates/engine/src/game/filter.rs` - ParentTarget match arm
- `crates/engine/src/game/targeting.rs` - ParentTarget no-op in find_legal_targets
- `crates/engine/src/game/triggers.rs` - ParentTarget in target filter matching and extraction
- `crates/engine/src/game/coverage.rs` - ParentTarget label, ChangeZone wildcard
- 12 additional files updated with owner_library: false for ChangeZone backward compat

## Decisions Made
- Used existing CountersGE with string counter_type instead of adding HasCounter with typed CounterType -- avoids creating a types->game cross-layer dependency; CountersGE already handles "creature with a stun counter on it" patterns
- ParentTarget is a no-op in find_legal_targets because resolve_ability_chain already copies parent targets to sub_abilities when sub.targets is empty (line 286 of effects/mod.rs)
- Added ZoneCounterProxy variant to TargetedImperativeAst to bridge destroy/exile/put-counter into compound splitting without duplicating the zone-counter parsing pipeline
- Implemented has_anaphoric_reference with explicit word-boundary checking instead of using contains_object_pronoun -- the latter requires space-bounded matches that miss end-of-string pronouns like "counter on it"

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] HasCounter not needed -- CountersGE already exists**
- **Found during:** Task 1 (type definitions)
- **Issue:** Plan specified adding FilterProp::HasCounter with typed CounterType, but CountersGE { counter_type: String, count: u32 } already handles the same use case and parse_counter_suffix already returns it
- **Fix:** Skipped HasCounter, added tests for existing CountersGE via parse_counter_suffix instead
- **Files modified:** crates/engine/src/parser/oracle_target.rs (tests only)
- **Verification:** All counter filter tests pass

**2. [Rule 3 - Blocking] 21 ChangeZone construction sites needed owner_library: false**
- **Found during:** Task 1 (adding owner_library field)
- **Issue:** Adding a non-default field to ChangeZone broke compilation at all existing construction sites
- **Fix:** Added owner_library: false to all 21 sites across test and production code
- **Files modified:** 12 files in game/ and parser/
- **Verification:** cargo check --all --tests passes

---

**Total deviations:** 2 auto-fixed (1 bug avoidance, 1 blocking)
**Impact on plan:** No scope creep. HasCounter omission simplifies the type system. owner_library fixups were mechanical.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- ParentTarget and compound splitting are ready for use by Plan 02 (if exists) or downstream card implementations
- owner_library field is ready for shuffle-to-owner's-library effect handler wiring
- All 1545 engine tests pass with zero failures

---
*Phase: 32-add-compound-targeted-action-parsing-and-multi-object-shuffle-to-library-support-floodpits-drowner-class*
*Completed: 2026-03-17*
