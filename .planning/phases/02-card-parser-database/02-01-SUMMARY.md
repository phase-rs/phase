---
phase: 02-card-parser-database
plan: 01
subsystem: parser
tags: [rust, parsing, mana-cost, card-type, ability-parser, serde, thiserror]

requires:
  - phase: 01-project-scaffold-core-types
    provides: engine crate structure, ManaColor, ManaPool, serde workspace deps
provides:
  - CardFace, CardRules, CardLayout types for card representation
  - ManaCost and ManaCostShard types with 40+ shard variants
  - CardType, Supertype, CoreType types for type line parsing
  - AbilityDefinition, TriggerDefinition, StaticDefinition, ReplacementDefinition types
  - ManaCost sub-parser (colored, generic, hybrid, phyrexian, X, snow, no cost)
  - CardType sub-parser (supertype/core-type/subtype classification)
  - Ability string parser (SP$/AB$/DB$ pipe-delimited format)
  - Trigger/Static/Replacement parsers (Mode$/Event$ discriminators)
  - ParseError enum with thiserror
affects: [02-card-parser-database, 04-ability-effect-system]

tech-stack:
  added: [thiserror 2.x]
  patterns: [TDD red-green for parser modules, FromStr for enum parsing, pipe-delimited Key$ Value parsing]

key-files:
  created:
    - crates/engine/src/types/card_type.rs
    - crates/engine/src/types/ability.rs
    - crates/engine/src/parser/mod.rs
    - crates/engine/src/parser/mana_cost.rs
    - crates/engine/src/parser/card_type.rs
    - crates/engine/src/parser/ability.rs
  modified:
    - crates/engine/Cargo.toml
    - crates/engine/src/lib.rs
    - crates/engine/src/types/mod.rs
    - crates/engine/src/types/card.rs
    - crates/engine/src/types/mana.rs

key-decisions:
  - "ManaCost as enum with NoCost/Cost variants (not Option wrapping)"
  - "ManaCostShard::from_str for all 40+ shard token mappings"
  - "Shared parse_params helper for pipe-delimited Key$ Value format"
  - "CardType parser uses FromStr on Supertype/CoreType enums for classification"

patterns-established:
  - "TDD red-green: write failing tests with todo!() stubs, then implement"
  - "Parser module convention: pub fn parse(input) -> Result<T, ParseError>"
  - "FromStr impl on enums for string-to-variant parsing"
  - "Pipe-delimited parsing via split('|') + split_once('$')"

requirements-completed: [PARSE-04, ABIL-01]

duration: 5min
completed: 2026-03-07
---

# Phase 2 Plan 1: Card Types & Sub-Parsers Summary

**ManaCost/CardType/Ability sub-parsers with 40+ mana shard variants, type line classification, and pipe-delimited ability string parsing using TDD**

## Performance

- **Duration:** 5 min
- **Started:** 2026-03-07T21:28:35Z
- **Completed:** 2026-03-07T21:33:16Z
- **Tasks:** 2
- **Files modified:** 11

## Accomplishments
- Replaced CardDefinition stub with full CardFace/CardRules/CardLayout types supporting all multi-face card layouts (Single, Split, Flip, Transform, Meld, Adventure, Modal, Omen, Specialize)
- ManaCost sub-parser handles all 40+ shard variants including colored, generic, hybrid, phyrexian, hybrid-phyrexian, X, snow, colorless, two-generic hybrid, colorless-hybrid, and "no cost"
- CardType sub-parser classifies tokens as supertypes (Legendary, Basic, Snow, World, Ongoing), core types (11 variants), or subtypes with multi-word type support
- Ability parser handles SP$/AB$/DB$ ability strings, Mode$-discriminated triggers/statics, and Event$-discriminated replacements
- All 64 engine tests pass (31 new parser tests + 33 existing type tests)

## Task Commits

Each task was committed atomically:

1. **Task 1: Card types + ManaCost/CardType sub-parsers (RED)** - `f322526` (test)
2. **Task 1: Card types + ManaCost/CardType sub-parsers (GREEN)** - `8dab074` (feat)
3. **Task 2: Ability string parser (RED)** - `72cc315` (test)
4. **Task 2: Ability string parser (GREEN)** - `cfb47ca` (feat)

## Files Created/Modified
- `crates/engine/Cargo.toml` - Added thiserror dependency
- `crates/engine/src/lib.rs` - Added parser module
- `crates/engine/src/types/mod.rs` - Updated re-exports for new types
- `crates/engine/src/types/card.rs` - CardFace, CardRules, CardLayout (replaced CardDefinition stub)
- `crates/engine/src/types/mana.rs` - ManaCostShard (40+ variants), ManaCost enum
- `crates/engine/src/types/card_type.rs` - Supertype, CoreType, CardType
- `crates/engine/src/types/ability.rs` - AbilityDefinition, TriggerDefinition, StaticDefinition, ReplacementDefinition
- `crates/engine/src/parser/mod.rs` - ParseError enum with thiserror
- `crates/engine/src/parser/mana_cost.rs` - ManaCost parser with 13 tests
- `crates/engine/src/parser/card_type.rs` - CardType parser with 7 tests
- `crates/engine/src/parser/ability.rs` - Ability/Trigger/Static/Replacement parsers with 11 tests

## Decisions Made
- ManaCost as enum with NoCost/Cost variants rather than Option wrapping -- NoCost is semantically distinct from zero cost
- Shared parse_params helper factored for the pipe-delimited Key$ Value format used by abilities, triggers, statics, and replacements
- CardType parser uses FromStr on Supertype/CoreType enums -- unknown tokens fall through to subtypes matching Forge's lenient behavior
- Multi-word subtype support via prefix checking (Time Lord, Serra's Realm, etc.)

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- All sub-parsers ready for the card file parser (Plan 02) to use
- CardFace/CardRules/CardLayout types ready for the card database (Plan 03)
- Parser module structure established for card_parser.rs to be added

---
## Self-Check: PASSED

All 6 created files verified on disk. All 4 commits verified in git log.

---
*Phase: 02-card-parser-database*
*Completed: 2026-03-07*
