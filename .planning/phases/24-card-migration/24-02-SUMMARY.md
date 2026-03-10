---
phase: 24-card-migration
plan: 02
subsystem: testing, parser
tags: [rust, parity-tests, mtgjson, integration-tests, card-data]

# Dependency graph
requires:
  - phase: 24-card-migration
    plan: 01
    provides: Migration tool output (32,274 ability JSON files), cost parser
  - phase: 23-unified-card-loader
    provides: CardDatabase::load_json(), json_loader, AbilityFile schema
provides:
  - Standard card manifest (data/standard-cards.txt, 78 cards)
  - Expanded MTGJSON test fixture (85 entries from real AtomicCards data)
  - Forge vs JSON parity integration tests (parity.rs, 4 tests)
  - Fixed apostrophe card name matching in json_loader
affects: [24-03, ci-coverage-gates, card-loading-regression]

# Tech tracking
tech-stack:
  added: []
  patterns: [manifest-driven-test-coverage, structural-parity-comparison, allowlisted-divergence-filtering]

key-files:
  created:
    - data/standard-cards.txt
    - crates/engine/tests/parity.rs
  modified:
    - data/mtgjson/test_fixture.json
    - crates/engine/src/database/json_loader.rs
    - .gitignore

key-decisions:
  - "Keyword comparison uses base name (before colon) to handle Forge parameterized keywords vs MTGJSON bare keywords"
  - "NoCost and Cost{generic:0, shards:[]} treated as semantically equivalent for basic land mana costs"
  - "JSON-only keywords (Scry, Mill) allowed as extras -- MTGJSON tracks action keywords Forge doesn't"
  - "Real MTGJSON AtomicCards.json downloaded for fixture accuracy, original 12 hand-crafted entries preserved"

patterns-established:
  - "Manifest-driven test coverage: standard-cards.txt decouples CI gates from directory structure"
  - "Parity comparison with allowlisted divergences for synthesized abilities (basic land mana, equipment equip)"

requirements-completed: [TEST-04, MIGR-01]

# Metrics
duration: 22min
completed: 2026-03-10
---

# Phase 24 Plan 02: Parity Tests & Standard Card Manifest Summary

**Structural parity tests comparing Forge-parsed vs JSON-loaded cards across 78 Standard cards with manifest-driven coverage gates**

## Performance

- **Duration:** 22 min
- **Started:** 2026-03-10T22:34:25Z
- **Completed:** 2026-03-10T22:57:00Z
- **Tasks:** 2
- **Files modified:** 5

## Accomplishments
- 78 Standard card names extracted from Forge files into manifest for CI coverage gates
- MTGJSON fixture expanded from 12 to 85 entries using real AtomicCards.json data
- Parity tests structurally compare all 78 cards across both loading paths (abilities, triggers, statics, keywords, P/T, mana costs, layout)
- Only 2 categories of allowlisted divergences: basic land synthesized mana abilities and equipment synthesized equip abilities
- Fixed card name matching bug for apostrophe-containing names (e.g., "Healer's Hawk")

## Task Commits

Each task was committed atomically:

1. **Task 1: Create Standard card manifest and expand MTGJSON fixture** - `921c8cd5` (feat)
2. **Task 2: Build Forge vs JSON parity integration tests** - `f7fa2eb5` (feat)

## Files Created/Modified
- `data/standard-cards.txt` - 78 Standard card names, one per line, alphabetically sorted
- `data/mtgjson/test_fixture.json` - Expanded from 12 to 85 MTGJSON entries with real AtomicCards data
- `crates/engine/tests/parity.rs` - Parity integration tests: main all-cards test, positive implicit ability assertions, spot-checks for Lightning Bolt, Jace, Lovestruck Beast
- `crates/engine/src/database/json_loader.rs` - Fixed normalize_for_match to strip spaces for apostrophe card matching
- `.gitignore` - Added !data/standard-cards.txt exclusion

## Decisions Made
- Keyword comparison uses base name only (before first colon) since Forge preserves parameters ("Ward:1", "Protection:Demon") while MTGJSON strips them to bare names ("Ward", "Protection")
- NoCost and Cost{generic:0, shards:[]} treated as equivalent (basic lands parsed differently by each path but both mean zero mana)
- Extra JSON-only keywords (Scry, Mill from MTGJSON) allowed -- only Forge keywords missing from JSON side fail the test
- Downloaded real MTGJSON AtomicCards.json (123MB) for fixture accuracy instead of hand-crafting 78 entries; original 12 entries preserved to maintain backward compatibility with existing Bonesplitter Equip:1 format

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed apostrophe card name matching in json_loader**
- **Found during:** Task 2 (parity test for all Standard cards)
- **Issue:** normalize_for_match stripped apostrophes but kept spaces, causing "Healer's Hawk" (normalized to "healershawk") to not match "Healer S Hawk" (from filename healer_s_hawk.json, normalized to "healershawk"). Wait -- actually the old normalize kept spaces, so "healer's hawk" became "healers hawk" while "healer s hawk" stayed "healer s hawk" -- space-sensitive comparison failed.
- **Fix:** Changed normalize_for_match to strip ALL non-alphanumeric characters including spaces, comparing as continuous lowercase strings. Also updated normalized prefix match for multi-face cards to use "//" without spaces.
- **Files modified:** crates/engine/src/database/json_loader.rs
- **Verification:** All 10 json_smoke_tests pass, all 4 parity tests pass, Healer's Hawk now loads correctly
- **Committed in:** f7fa2eb5 (Task 2 commit)

**2. [Rule 1 - Bug] Handled systematic mana cost and keyword divergences**
- **Found during:** Task 2 (parity test failures)
- **Issue:** 13 mismatches across basic land mana costs (NoCost vs Cost{0,[]}) and keywords (Forge parameterized vs MTGJSON bare, plus MTGJSON-only action keywords)
- **Fix:** Added mana_costs_equivalent() for NoCost/zero-cost equivalence. Changed keyword comparison to use base names and allow JSON-only extras. These are inherent data source differences, not bugs.
- **Files modified:** crates/engine/tests/parity.rs
- **Verification:** All 78 cards pass parity check with only allowlisted divergences
- **Committed in:** f7fa2eb5 (Task 2 commit)

---

**Total deviations:** 2 auto-fixed (2 bugs)
**Impact on plan:** Essential fixes -- apostrophe matching bug prevented loading cards like "Healer's Hawk", and systematic data source differences required comparison logic adjustments. No scope creep.

## Issues Encountered
- Pre-existing clippy warning in ability.rs (if_same_then_else for Tap/Untap cost branches) -- out of scope, not introduced by this plan

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- All 78 Standard cards verified for parity between Forge and JSON loading paths
- Manifest file ready for CI coverage gate (Plan 03) to reference
- Expanded MTGJSON fixture provides accurate metadata for all Standard cards
- Plan 03 (CI coverage gate) can proceed

## Self-Check: PASSED

- data/standard-cards.txt: FOUND (80 lines, 78 cards)
- data/mtgjson/test_fixture.json: FOUND (85 MTGJSON entries)
- crates/engine/tests/parity.rs: FOUND (696 lines)
- Task 1 commit 921c8cd5: FOUND
- Task 2 commit f7fa2eb5: FOUND
- All 4 parity tests pass
- All 10 json_smoke_tests pass
- All 42 rules integration tests pass

---
*Phase: 24-card-migration*
*Completed: 2026-03-10*
