---
phase: 05-triggers-combat
plan: 01
subsystem: engine
tags: [keyword, trigger, enum, type-safety, mtg-rules]

requires:
  - phase: 04-ability-system
    provides: "AbilityDefinition, ResolvedAbility, effect handlers, GameObject with string keywords"
provides:
  - "Keyword enum with 50+ typed variants and FromStr parsing"
  - "TriggerMode enum with 137+ variants matching Forge TriggerType"
  - "Vec<Keyword> typed keyword storage on GameObject"
  - "has_keyword discriminant-based keyword checking"
  - "parse_keywords batch conversion from Vec<String>"
affects: [05-triggers-combat, 06-layers-replacements, combat-system]

tech-stack:
  added: []
  patterns: ["discriminant-based keyword matching", "Infallible FromStr for forward-compatible enums"]

key-files:
  created:
    - crates/engine/src/types/keywords.rs
    - crates/engine/src/types/triggers.rs
    - crates/engine/src/game/keywords.rs
  modified:
    - crates/engine/src/types/mod.rs
    - crates/engine/src/game/game_object.rs
    - crates/engine/src/game/mod.rs
    - crates/engine/src/game/casting.rs
    - crates/engine/src/game/targeting.rs
    - crates/engine/src/game/effects/destroy.rs

key-decisions:
  - "Keyword FromStr uses Infallible error type (never fails, unknown -> Keyword::Unknown)"
  - "TriggerMode FromStr is case-sensitive matching Forge's CamelCase conventions"
  - "has_keyword uses std::mem::discriminant for parameterized keyword matching"
  - "CardFace.keywords stays Vec<String> (parser layer); conversion via parse_keywords at GameObject creation"

patterns-established:
  - "Discriminant matching: use std::mem::discriminant to match enum variants ignoring associated data"
  - "Forward-compatible enums: Unknown(String) fallback variant for unrecognized values"

requirements-completed: [KWRD-01, KWRD-02]

duration: 5min
completed: 2026-03-08
---

# Phase 5 Plan 1: Keyword & Trigger Type Enums Summary

**Keyword enum (50+ variants) and TriggerMode enum (137 variants) with typed Vec<Keyword> migration replacing all stringly-typed keyword matching**

## Performance

- **Duration:** 5 min
- **Started:** 2026-03-08T01:01:34Z
- **Completed:** 2026-03-08T01:06:30Z
- **Tasks:** 2
- **Files modified:** 9

## Accomplishments
- Keyword enum with 50+ variants covering all standard MTG keywords, parameterized keywords (Kicker, Cycling, Protection, etc.), and Unknown fallback
- TriggerMode enum with 137+ variants matching every entry in Forge's TriggerType.java
- Complete migration of GameObject.keywords from Vec<String> to Vec<Keyword> across the entire codebase
- Discriminant-based has_keyword helper and convenience functions for common keyword checks
- All 300 workspace tests pass with zero regressions

## Task Commits

Each task was committed atomically:

1. **Task 1: Keyword enum, TriggerMode enum, and type definitions** - `8400ce4` (feat)
2. **Task 2: GameObject keyword migration and helpers** - `0c8f874` (feat)

## Files Created/Modified
- `crates/engine/src/types/keywords.rs` - Keyword enum with 50+ variants, ProtectionTarget, FromStr
- `crates/engine/src/types/triggers.rs` - TriggerMode enum with 137+ variants, FromStr
- `crates/engine/src/types/mod.rs` - Added keywords and triggers modules with re-exports
- `crates/engine/src/game/keywords.rs` - has_keyword, convenience helpers, parse_keywords
- `crates/engine/src/game/game_object.rs` - Migrated keywords field to Vec<Keyword>, added has_keyword method
- `crates/engine/src/game/mod.rs` - Added keywords module with parse_keywords re-export
- `crates/engine/src/game/casting.rs` - Flash check uses typed Keyword::Flash
- `crates/engine/src/game/targeting.rs` - Hexproof/Shroud checks use typed keywords
- `crates/engine/src/game/effects/destroy.rs` - Indestructible check uses typed keyword

## Decisions Made
- Keyword::from_str uses Infallible error type -- never fails, unrecognized values become Unknown(String)
- TriggerMode::from_str is case-sensitive to match Forge's CamelCase naming convention
- has_keyword uses std::mem::discriminant for parameterized keyword matching (Kicker:"1G" matches Kicker:"X")
- CardFace.keywords remains Vec<String> in the parser layer; conversion happens via parse_keywords at GameObject creation time

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

None.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- Keyword and TriggerMode types ready for trigger system implementation (05-02)
- has_keyword helper ready for combat system keyword checks (05-03)
- All existing tests pass with typed keywords

---
*Phase: 05-triggers-combat*
*Completed: 2026-03-08*
