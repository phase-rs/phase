---
phase: 32-add-compound-targeted-action-parsing-and-multi-object-shuffle-to-library-support-floodpits-drowner-class
plan: 02
subsystem: parser
tags: [oracle-parser, compound-subject, shuffle-to-library, auto-shuffle, owner-library, selfref, change-zone, integration-test]

requires:
  - phase: 32-add-compound-targeted-action-parsing-and-multi-object-shuffle-to-library-support-floodpits-drowner-class
    plan: 01
    provides: "ParentTarget, owner_library field, try_split_targeted_compound, compound effect splitting"
provides:
  - "try_split_compound_subject() verb-agnostic compound subject splitter"
  - "try_parse_compound_shuffle() for 'shuffle X and Y into libraries' patterns"
  - "ChangeZone SelfRef pre-loop guard (processes source through zone-change pipeline)"
  - "ChangeZone owner_library routing to owner's library (CR 400.7)"
  - "CR 401.3 auto-shuffle after ChangeZone to Library"
  - "shuffle_library() reusable helper for library shuffling"
  - "Floodpits Drowner fully parsed (no Unimplemented effects)"
affects: [shuffle-to-library, terminus, hallowed-burial, condemn, compound-subjects]

tech-stack:
  added: []
  patterns:
    - "Compound subject splitting via try_split_compound_subject for verb-agnostic 'X and Y' patterns"
    - "Auto-shuffle at ChangeZone resolve level per CR 401.3 (suppressed on replacement redirect)"
    - "SelfRef pre-loop guard pattern: empty targets + SelfRef filter uses ability.source_id"

key-files:
  created:
    - "crates/engine/tests/floodpits_drowner.rs"
  modified:
    - "crates/engine/src/parser/oracle_effect.rs"
    - "crates/engine/src/game/effects/change_zone.rs"
    - "client/public/card-data.json"

key-decisions:
  - "Compound shuffle intercepted in lower_imperative_clause (before parse_shuffle_ast) rather than in parse_shuffle_ast itself, avoiding AST variant proliferation"
  - "Auto-shuffle fires at ChangeZone resolve level after replacement check, covering all cards that move objects to Library"
  - "shuffle_library() extracted as pub helper on change_zone module for reuse by other effects"

patterns-established:
  - "try_split_compound_subject: verb-agnostic splitter for 'X and Y [remainder]' patterns"
  - "SelfRef pre-loop guard: ChangeZone with SelfRef + empty targets creates synthetic TargetRef from source_id"
  - "Auto-shuffle: any ChangeZone to Library triggers owner library shuffle after zone move"

requirements-completed: [BB-SUBJECT, BB-SHUFFLE, BB-INTEGRATE]

duration: 39min
completed: 2026-03-17
---

# Phase 32 Plan 02: Compound Subject Splitting + Auto-Shuffle + Integration Summary

**Verb-agnostic compound subject splitter, CR 401.3 auto-shuffle on ChangeZone to Library, SelfRef pre-loop guard, and Floodpits Drowner fully parsed with integration tests**

## Performance

- **Duration:** 39 min
- **Started:** 2026-03-17T17:13:37Z
- **Completed:** 2026-03-17T17:52:37Z
- **Tasks:** 2
- **Files modified:** 4

## Accomplishments
- Built try_split_compound_subject() for verb-agnostic "X and Y" compound subject splitting
- Added CR 401.3 auto-shuffle: ChangeZone to Library automatically shuffles owner's library (suppressed on replacement redirect per CR 614.6)
- SelfRef pre-loop guard enables ChangeZone to process the source object when targets are empty
- Floodpits Drowner now fully parsed: ETB trigger (Tap + PutCounter chain) and activated ability (compound ChangeZone chain with owner_library)
- Pipeline regeneration updates card-data.json with improved compound shuffle coverage

## Task Commits

Each task was committed atomically:

1. **Task 1: Compound subject splitter + ChangeZone auto-shuffle + SelfRef guard + owner_library routing** - `3288dcfbb` (feat)
2. **Task 2: Floodpits Drowner integration test + pipeline regeneration** - `eadb26d8f` (feat)

## Files Created/Modified
- `crates/engine/src/parser/oracle_effect.rs` - try_split_compound_subject, try_parse_compound_shuffle, lower_imperative_clause intercept
- `crates/engine/src/game/effects/change_zone.rs` - SelfRef guard, owner_library routing, auto-shuffle, shuffle_library helper
- `crates/engine/tests/floodpits_drowner.rs` - 4 integration tests for ETB + activated ability building blocks
- `client/public/card-data.json` - Regenerated with compound shuffle parsing coverage

## Decisions Made
- Compound shuffle intercepted in lower_imperative_clause via try_parse_compound_shuffle rather than extending ShuffleImperativeAst -- cleaner separation since compound subjects need ParsedEffectClause with sub_ability, not a flat Effect
- Auto-shuffle fires at ChangeZone resolve level after replacement::replace_event check -- covers all Library-destination effects universally (Terminus, Hallowed Burial, Condemn, etc.)
- SelfRef pre-loop guard creates synthetic TargetRef::Object(source_id) when filter is SelfRef and targets are empty -- follows established pattern from counters.rs and pump.rs

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- All Phase 32 building blocks complete: compound parsing, auto-shuffle, owner_library routing
- Floodpits Drowner fully parsed and integration-tested
- Auto-shuffle covers the full class of "shuffle into library" cards (Terminus, Hallowed Burial, Condemn, etc.)
- 1555 engine tests passing with zero failures

---
*Phase: 32-add-compound-targeted-action-parsing-and-multi-object-shuffle-to-library-support-floodpits-drowner-class*
*Completed: 2026-03-17*
