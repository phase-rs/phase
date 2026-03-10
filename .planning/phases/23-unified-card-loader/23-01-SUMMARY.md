---
phase: 23-unified-card-loader
plan: 01
subsystem: database
tags: [mtgjson, card-loading, json, serde, mana-synthesis, equip]

requires:
  - phase: 21-schema-mtgjson-foundation
    provides: AbilityFile schema, AbilityDefinition types, MTGJSON types and parser, test fixture
provides:
  - CardDatabase::load_json() constructor merging MTGJSON + ability JSON into CardRules
  - FaceAbilities struct for multi-face card ability definitions
  - scryfall_oracle_id on CardFace for frontend image lookups
  - synthesize_basic_land_mana() per CR 305.6
  - synthesize_equip() per CR 702.6
  - parse_loyalty_cost() prefers typed AbilityCost::Loyalty
affects: [23-02-PLAN, 24-migration, 25-forge-removal]

tech-stack:
  added: []
  patterns: [json-loader-merge-pipeline, implicit-ability-synthesis, case-insensitive-mtgjson-lookup]

key-files:
  created:
    - crates/engine/src/database/json_loader.rs
  modified:
    - crates/engine/src/types/card.rs
    - crates/engine/src/schema/mod.rs
    - crates/engine/src/database/mod.rs
    - crates/engine/src/database/card_db.rs
    - crates/engine/src/game/planeswalker.rs
    - crates/engine/src/parser/card_parser.rs
    - crates/engine/src/game/deck_loading.rs
    - crates/server-core/src/session.rs

key-decisions:
  - "Case-insensitive MTGJSON name matching for filename-to-card lookup (handles title-cased prepositions)"
  - "FaceAbilities uses flat #[serde(default)] fields mirroring AbilityFile, not a nested AbilityFile"
  - "Equip synthesis uses Effect::Attach with TargetSpec::Filtered for Creature.YouCtrl"
  - "CardDatabase fields made pub(crate) for cross-module construction by json_loader"

patterns-established:
  - "JSON loader merge pipeline: MTGJSON metadata + ability JSON -> CardFace -> CardRules"
  - "Implicit ability synthesis: basic lands and equipment get abilities from card type/keywords"
  - "filename_to_card_name(): snake_case -> Title Case for ability JSON file naming convention"

requirements-completed: [DATA-03, MIGR-04]

duration: 9min
completed: 2026-03-10
---

# Phase 23 Plan 01: JSON Card Loader Summary

**JSON card loader merging MTGJSON metadata + ability JSON into CardDatabase with basic land mana synthesis (CR 305.6), equipment equip synthesis (CR 702.6), multi-face card support, and typed planeswalker loyalty costs**

## Performance

- **Duration:** 9 min
- **Started:** 2026-03-10T20:42:31Z
- **Completed:** 2026-03-10T20:52:04Z
- **Tasks:** 2
- **Files modified:** 9

## Accomplishments
- JSON card loader pipeline that merges MTGJSON atomic card data with per-card ability JSON files into engine-ready CardFace/CardRules objects
- Multi-face card support (Transform, Adventure, Split, Modal DFC) with per-face ability definitions via FaceAbilities
- Implicit ability synthesis for basic lands (mana abilities) and equipment (equip activated abilities)
- scryfall_oracle_id threading from MTGJSON identifiers for frontend card image lookups
- parse_loyalty_cost() prefers typed AbilityCost::Loyalty from JSON ability files over legacy remaining_params
- 24 unit tests for json_loader, 5 new tests for schema/types, 2 new planeswalker tests

## Task Commits

Each task was committed atomically:

1. **Task 1: Type contracts and AbilityFile extension** - `5d29618` (feat)
2. **Task 2: JSON loader module with merge logic, implicit synthesis, and load_json()** - `cde9005` (feat)

## Files Created/Modified
- `crates/engine/src/database/json_loader.rs` - Core JSON loader: build_card_face, build_card_type, map_layout, synthesize_basic_land_mana, synthesize_equip, build_card_rules, load_json, filename_to_card_name
- `crates/engine/src/types/card.rs` - Added scryfall_oracle_id: Option<String> to CardFace
- `crates/engine/src/schema/mod.rs` - Added FaceAbilities struct, faces field on AbilityFile, made abilities #[serde(default)]
- `crates/engine/src/database/mod.rs` - Added pub mod json_loader
- `crates/engine/src/database/card_db.rs` - Added CardDatabase::load_json(), made fields pub(crate)
- `crates/engine/src/game/planeswalker.rs` - Updated parse_loyalty_cost() to prefer AbilityCost::Loyalty
- `crates/engine/src/parser/card_parser.rs` - Added scryfall_oracle_id: None to CardFace construction
- `crates/engine/src/game/deck_loading.rs` - Added scryfall_oracle_id: None to test CardFace constructors
- `crates/server-core/src/session.rs` - Added scryfall_oracle_id: None to test CardFace constructor

## Decisions Made
- Case-insensitive MTGJSON name matching: filename_to_card_name() capitalizes all words including prepositions ("of" -> "Of"), so lookup uses case-insensitive comparison against MTGJSON keys
- FaceAbilities struct mirrors AbilityFile's four definition fields (abilities, triggers, statics, replacements) with all #[serde(default)]
- Equip synthesis targets Creature.YouCtrl via Effect::Attach, matching the engine's existing Attach effect handler pattern
- CardDatabase fields changed from private to pub(crate) to allow json_loader module to construct CardDatabase directly

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Fixed missing scryfall_oracle_id field in server-core**
- **Found during:** Task 1 (Type contracts)
- **Issue:** Adding scryfall_oracle_id to CardFace broke server-core/src/session.rs which constructs CardFace in tests
- **Fix:** Added scryfall_oracle_id: None to the test CardFace constructor in session.rs
- **Files modified:** crates/server-core/src/session.rs
- **Verification:** cargo test --all passes
- **Committed in:** 5d29618 (Task 1 commit)

---

**Total deviations:** 1 auto-fixed (1 blocking)
**Impact on plan:** Necessary fix for compile-time field completeness. No scope creep.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- load_json() ready for integration testing in Plan 02
- FaceAbilities schema extension allows multi-face ability JSON authoring
- scryfall_oracle_id available for frontend card image resolution

---
*Phase: 23-unified-card-loader*
*Completed: 2026-03-10*
