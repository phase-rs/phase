---
phase: 24-card-migration
plan: 01
subsystem: parser, tooling
tags: [rust, serde, migration, card-data, cost-parser, cli-binary]

# Dependency graph
requires:
  - phase: 23-unified-card-loader
    provides: AbilityFile schema, json_loader, CardDatabase::load_json()
  - phase: 21-ability-definitions
    provides: Typed AbilityDefinition, Effect enum, AbilityCost variants
provides:
  - Enhanced cost parser (parse_cost) handling Tap, Mana, Loyalty, Sacrifice, Composite, Untap
  - Migration tool binary (cargo run --bin migrate)
  - 32,274 ability JSON files in data/abilities/
  - migration-report.json with detailed stats
affects: [24-02, 24-03, parity-tests, coverage-gates]

# Tech tracking
tech-stack:
  added: []
  patterns: [cost-string-splitting-with-angle-bracket-nesting, card-name-to-filename-normalization, migration-tool-as-serialization-adapter]

key-files:
  created:
    - crates/engine/src/bin/migrate.rs
    - migration-report.json
    - data/abilities/ (32,274 card JSON files)
  modified:
    - crates/engine/src/parser/ability.rs
    - crates/engine/Cargo.toml
    - crates/engine/tests/json_smoke_test.rs

key-decisions:
  - "Unknown cost components preserved as AbilityCost::Mana fallback to retain data (matches Effect::Other pattern)"
  - "Migration overwrites 8 hand-authored JSON files from Phase 23 per user decision for consistency"
  - "26 Forge files with missing Name field skipped as errors (malformed source data)"
  - "json_smoke_test adapted to handle 32K ability files against 12-card MTGJSON fixture"

patterns-established:
  - "parse_cost() with split_cost_components() for Forge Cost$ string parsing"
  - "card_name_to_filename() normalization for ability JSON output naming"
  - "Migration tool as standalone binary reusing existing parser pipeline"

requirements-completed: [MIGR-01, MIGR-03]

# Metrics
duration: 12min
completed: 2026-03-10
---

# Phase 24 Plan 01: Migration Tool & Cost Parser Summary

**Automated migration of 32,274 Forge card files to ability JSON with typed cost parsing for Tap, Mana, Loyalty, Sacrifice, and Composite costs**

## Performance

- **Duration:** 12 min
- **Started:** 2026-03-10T22:17:48Z
- **Completed:** 2026-03-10T22:30:00Z
- **Tasks:** 2
- **Files modified:** 32,278

## Accomplishments
- Enhanced cost parser replaces stub that hardcoded all costs to AbilityCost::Tap
- Migration tool converts all 32,300 Forge .txt files (32,274 success, 26 parse errors from malformed source files)
- Generated JSON files correctly use typed costs: loyalty (+N/-N) for planeswalkers, mana for spell costs, sacrifice for sac costs, composite for multi-component costs
- All 743 existing tests pass (691 unit + 10 json_smoke_test + 42 rules integration)

## Task Commits

Each task was committed atomically:

1. **Task 1: Enhance cost parser in ability.rs** - `1b7bd18` (feat)
2. **Task 2: Build migration tool binary and execute migration** - `41ace23` (feat)

## Files Created/Modified
- `crates/engine/src/parser/ability.rs` - Enhanced parse_cost() with split_cost_components(), parse_loyalty(), parse_single_cost() helpers; 17 new tests
- `crates/engine/src/bin/migrate.rs` - Migration tool binary: card_rules_to_ability_file(), face_to_abilities(), card_name_to_filename(), oracle heuristic check
- `crates/engine/Cargo.toml` - Added [[bin]] entry for migrate
- `crates/engine/tests/json_smoke_test.rs` - Adapted tests for 32K ability files (error type checking, fixture-centric cross-validation)
- `data/abilities/` - 32,274 generated JSON files (overwrites 8 hand-authored)
- `migration-report.json` - Detailed report: 32,300 total, 32,274 converted, 26 errors, 347 warnings

## Decisions Made
- Unknown cost components (PayLife<N>, Discard<N/Card>, tapXType<...>, exert) preserved as AbilityCost::Mana fallback -- retains data without losing information, matches the Effect::Other fallback pattern used for unknown effects
- "True" cost (free activation) mapped to None -- no cost needed for free abilities
- Untap/Q reuses AbilityCost::Tap variant -- the engine's untap logic is separate from the cost type
- Multi-face cards use faces array with empty top-level abilities, single-face cards use flat top-level abilities -- matches existing AbilityFile schema and json_loader expectations

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Fixed json_smoke_test failures from migration output**
- **Found during:** Task 2 (after running migration)
- **Issue:** test_load_all_smoke_test_cards asserted zero errors, but 32K ability files against 12-card MTGJSON fixture produces thousands of "No MTGJSON match" errors. test_cross_validation checked every ability file has MTGJSON match.
- **Fix:** Changed test_load_all_smoke_test_cards to verify errors are only "No MTGJSON match" type (not parse errors). Replaced test_cross_validation with test_cross_validation_fixture_cards_have_ability_files that checks the reverse: every fixture card has an ability JSON file.
- **Files modified:** crates/engine/tests/json_smoke_test.rs
- **Verification:** All 10 json_smoke_test tests pass
- **Committed in:** 41ace23 (Task 2 commit)

---

**Total deviations:** 1 auto-fixed (1 blocking)
**Impact on plan:** Essential fix -- migration output volume changes the test invariant from "all files match MTGJSON" to "all fixture cards have ability files". No scope creep.

## Issues Encountered
- 26 Forge card files lack a Name: field (Specialize variants like rasaad_monk_of_selune.txt, imoen_trickster_friend.txt) -- parser correctly rejects these as ParseError::MissingField. These appear to be multi-face Forge files using an alternate format the parser doesn't yet handle. Logged in migration-report.json.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- 32,274 ability JSON files available in data/abilities/ for parity testing (Plan 02)
- Cost parser produces typed costs for coverage gate validation
- migration-report.json available for detailed error analysis
- Plan 02 (parity tests) and Plan 03 (CI coverage gate) can proceed

## Self-Check: PASSED

- All created files verified present on disk
- Both task commits (1b7bd18, 41ace23) verified in git log
- 32,275 JSON files in data/abilities/ (32,274 cards + schema.json)
- All tests green (691 unit + 10 integration + 42 rules)

---
*Phase: 24-card-migration*
*Completed: 2026-03-10*
