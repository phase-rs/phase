---
phase: 02-card-parser-database
plan: 02
subsystem: parser
tags: [rust, parsing, card-parser, multi-face, tdd]

requires:
  - phase: 02-card-parser-database
    provides: ManaCost sub-parser, CardType sub-parser, CardFace/CardRules/CardLayout types, ParseError
provides:
  - parse_card_file() function for parsing Forge .txt card definitions into typed CardRules
  - Support for all multi-face card layouts (Split, Flip, Transform, Meld, Adventure, Modal, Omen)
  - ALTERNATE delimiter face-switching and AlternateMode-to-CardLayout mapping
  - SVar double-colon parsing, PT parsing with star values, Colors override parsing
affects: [02-card-parser-database, 04-ability-effect-system]

tech-stack:
  added: []
  patterns: [CardFaceBuilder pattern for accumulating parsed fields, first-byte dispatch for key matching]

key-files:
  created:
    - crates/engine/src/parser/card_parser.rs
  modified:
    - crates/engine/src/parser/mod.rs

key-decisions:
  - "First-byte dispatch on key character then exact match -- direct Rust translation of Forge's parseLine switch"
  - "CardFaceBuilder with build() validation -- requires name, defaults ManaCost to zero"
  - "Lenient parsing: unknown keys silently skipped matching Forge behavior"

patterns-established:
  - "CardFaceBuilder pattern: mutable builder accumulates fields, build() converts to immutable CardFace"
  - "ALTERNATE delimiter switches face context index (0 -> 1)"

requirements-completed: [PARSE-01, PARSE-02]

duration: 4min
completed: 2026-03-07
---

# Phase 2 Plan 2: Card File Parser Summary

**Card file parser supporting all Forge .txt fields and 7 multi-face layouts (Split, Flip, Transform, Meld, Adventure, Modal, Omen) with lenient unknown-key handling**

## Performance

- **Duration:** 4 min
- **Started:** 2026-03-07T21:36:16Z
- **Completed:** 2026-03-07T21:40:16Z
- **Tasks:** 1
- **Files modified:** 2

## Accomplishments
- Implemented parse_card_file() handling all single-card fields: Name, ManaCost, Types, PT, K, A, T, S, R, SVar, Oracle, Loyalty, Defense, Text, FlavorName, Colors
- All 7 multi-face card types parse via ALTERNATE delimiter and AlternateMode mapping (including DoubleFaced->Transform alias)
- SVar double-colon format (SVar:Name:Value) correctly splits on first colon for var name, remainder for value
- 18 new parser tests, 82 total engine tests pass

## Task Commits

Each task was committed atomically:

1. **Task 1: Card file parser (RED)** - `f578b68` (test)
2. **Task 1: Card file parser (GREEN)** - `3fafa51` (feat)

## Files Created/Modified
- `crates/engine/src/parser/card_parser.rs` - Card file parser with CardFaceBuilder, parse_card_file(), parse_line()
- `crates/engine/src/parser/mod.rs` - Added card_parser module and re-export of parse_card_file

## Decisions Made
- First-byte dispatch on key character then exact match -- matches Forge's parseLine switch/case structure for performance
- CardFaceBuilder requires name field, defaults ManaCost to zero if not set (matching Forge behavior for back faces)
- Lenient parsing silently skips unknown keys -- matches Forge's switch fallthrough

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Card file parser ready for the card database (Plan 03) to use for bulk loading
- parse_card_file is re-exported from parser module for external use

---
## Self-Check: PASSED

All created files verified on disk. All 2 commits verified in git log.

---
*Phase: 02-card-parser-database*
*Completed: 2026-03-07*
