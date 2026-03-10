---
phase: 23-unified-card-loader
plan: 02
subsystem: testing
tags: [mtgjson, json, smoke-test, integration-test, cross-validation, card-loading]

requires:
  - phase: 23-unified-card-loader
    provides: CardDatabase::load_json(), FaceAbilities, scryfall_oracle_id, synthesize_basic_land_mana, synthesize_equip
provides:
  - 7 ability JSON files covering all required card archetypes
  - MTGJSON test fixture with 12 card entries (8 smoke test cards)
  - 10 integration smoke tests proving JSON card loading pipeline end-to-end
  - Cross-validation test ensuring ability JSON files match MTGJSON entries
  - Smoke game tests proving JSON-loaded cards work through apply() for casting and combat
affects: [24-migration, 25-forge-removal]

tech-stack:
  added: []
  patterns: [normalize-for-match-punctuation-stripping, integration-test-json-card-pipeline]

key-files:
  created:
    - crates/engine/tests/json_smoke_test.rs
    - data/abilities/forest.json
    - data/abilities/grizzly_bears.json
    - data/abilities/rancor.json
    - data/abilities/bonesplitter.json
    - data/abilities/jace_the_mind_sculptor.json
    - data/abilities/delver_of_secrets.json
    - data/abilities/giant_killer.json
  modified:
    - data/mtgjson/test_fixture.json
    - crates/engine/src/database/json_loader.rs

key-decisions:
  - "normalize_for_match() strips punctuation for card name matching (handles comma in 'Jace, the Mind Sculptor' vs filename 'jace_the_mind_sculptor')"
  - "Smoke game cast spell test uses direct mana pool injection rather than TapLandForMana (Bolt costs {R}, Forest produces {G})"
  - "Combat smoke test drives full engine pipeline via PassPriority loop through declare/combat damage phases"

patterns-established:
  - "Ability JSON files use Effect::Other for complex card-specific effects not yet covered by typed variants"
  - "Multi-face ability JSON files use faces array with per-face abilities/triggers/statics/replacements"
  - "Integration tests use env!(CARGO_MANIFEST_DIR)/../../data for workspace-relative data paths"

requirements-completed: [DATA-03, MIGR-04]

duration: 9min
completed: 2026-03-10
---

# Phase 23 Plan 02: Smoke Test Cards & Integration Tests Summary

**8 ability JSON files covering all required archetypes (vanilla, instant, aura, equipment, planeswalker, transform DFC, adventure DFC, basic land) with 10 integration smoke tests proving JSON-loaded cards work through the engine's apply() pipeline for casting spells and dealing combat damage**

## Performance

- **Duration:** 9 min
- **Started:** 2026-03-10T20:55:43Z
- **Completed:** 2026-03-10T21:05:04Z
- **Tasks:** 2
- **Files modified:** 10

## Accomplishments
- 7 new ability JSON files + extended MTGJSON test fixture covering all 8 required card archetypes
- 10 integration tests validating the full JSON card loading pipeline end-to-end
- Smoke game tests proving JSON-loaded cards cast spells (Lightning Bolt deals 3 damage) and deal combat damage (Grizzly Bears 2/2 attack) through the engine's apply() action pipeline
- Cross-validation test confirming every ability JSON file has a matching MTGJSON entry
- Verified synthesized abilities: Forest mana, Bonesplitter Equip, Jace loyalty costs
- Verified multi-face layouts: Delver Transform, Giant Killer Adventure

## Task Commits

Each task was committed atomically:

1. **Task 1: Author ability JSON files and extend MTGJSON test fixture** - `cc36ec6` (feat)
2. **Task 2: Integration smoke test and cross-validation** - `46411aa` (test)

## Files Created/Modified
- `data/abilities/forest.json` - Empty abilities (mana ability synthesized by loader)
- `data/abilities/grizzly_bears.json` - Empty abilities (vanilla 2/2 creature)
- `data/abilities/rancor.json` - Enchant + pump/trample statics + dies-return trigger
- `data/abilities/bonesplitter.json` - Equipment pump static (Equip synthesized from keyword)
- `data/abilities/jace_the_mind_sculptor.json` - 4 loyalty abilities with AbilityCost::Loyalty costs (+2, 0, -1, -12)
- `data/abilities/delver_of_secrets.json` - Multi-face with upkeep transform trigger on face 0
- `data/abilities/giant_killer.json` - Adventure multi-face: creature tap ability + destroy spell
- `data/mtgjson/test_fixture.json` - Extended with Forest, Rancor, Bonesplitter, Jace, Giant Killer entries
- `crates/engine/tests/json_smoke_test.rs` - 10 integration tests for card loading and gameplay
- `crates/engine/src/database/json_loader.rs` - Added normalize_for_match() for punctuation-tolerant name matching

## Decisions Made
- normalize_for_match() strips non-alphanumeric characters for fuzzy card name matching -- handles punctuation in card names like "Jace, the Mind Sculptor" that can't be represented in snake_case filenames
- Smoke game cast spell test adds red mana directly to pool rather than tapping Forest (which produces green) -- keeps test focused on proving JSON-loaded card data flows through apply()
- Combat smoke test uses PassPriority loop to drive through combat phases, matching the engine's full game loop

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] normalize_for_match() for punctuation in card names**
- **Found during:** Task 2 (Integration smoke tests)
- **Issue:** `filename_to_card_name("jace_the_mind_sculptor")` produces "Jace The Mind Sculptor" which doesn't match MTGJSON key "Jace, the Mind Sculptor" -- comma is missing
- **Fix:** Added `normalize_for_match()` function that strips punctuation for comparison, plus normalized prefix matching for multi-face cards
- **Files modified:** crates/engine/src/database/json_loader.rs
- **Verification:** Jace loads correctly, all 8 cards load without errors
- **Committed in:** 46411aa (Task 2 commit)

---

**Total deviations:** 1 auto-fixed (1 bug)
**Impact on plan:** Necessary fix for card name matching correctness. No scope creep.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- All 8 smoke test card archetypes validated through the JSON loading pipeline
- JSON card loader proven to work end-to-end: MTGJSON + ability JSON -> CardDatabase -> CardFace -> apply()
- Ready for Phase 24 (migration) to start converting remaining cards to JSON format

## Self-Check: PASSED

All 10 created/modified files verified present. Both task commits (cc36ec6, 46411aa) verified in git log.

---
*Phase: 23-unified-card-loader*
*Completed: 2026-03-10*
