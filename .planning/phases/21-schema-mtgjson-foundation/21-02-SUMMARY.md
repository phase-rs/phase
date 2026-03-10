---
phase: 21-schema-mtgjson-foundation
plan: 02
subsystem: engine
tags: [rust, typed-enums, ability-definitions, refactor, type-migration]

# Dependency graph
requires:
  - phase: 21-01
    provides: "Typed Effect, TriggerMode, StaticMode, ReplacementEvent enums and AbilityDefinition/TriggerDefinition/StaticDefinition/ReplacementDefinition structs with compat bridge methods"
provides:
  - "All ability fields use typed definition vectors (Vec<AbilityDefinition>, Vec<TriggerDefinition>, etc.)"
  - "Parse-time typing: abilities parsed into typed structs at card file parse time"
  - "No runtime re-parsing of ability strings in any consumer"
  - "Typed dispatch via api_type() compat methods across engine, AI, and server crates"
affects: [22-standard-card-coverage, 23-ai-opponent, engine-wasm]

# Tech tracking
tech-stack:
  added: []
  patterns:
    - "Parse-time typing: card parser produces typed AbilityDefinition instead of raw strings"
    - "Compat bridge pattern: api_type()/params() methods allow incremental migration without changing all dispatch at once"
    - "parse_test_ability() helper in test modules for constructing typed test data from Forge format strings"

key-files:
  created: []
  modified:
    - crates/engine/src/types/card.rs
    - crates/engine/src/game/game_object.rs
    - crates/engine/src/game/morph.rs
    - crates/engine/src/parser/card_parser.rs
    - crates/engine/src/game/engine.rs
    - crates/engine/src/game/casting.rs
    - crates/engine/src/game/mana_abilities.rs
    - crates/engine/src/game/coverage.rs
    - crates/engine/src/game/deck_loading.rs
    - crates/engine/src/game/planeswalker.rs
    - crates/engine/src/game/transform.rs
    - crates/forge-ai/src/card_hints.rs
    - crates/forge-ai/src/legal_actions.rs
    - crates/server-core/src/filter.rs

key-decisions:
  - "Kept compat bridge methods (api_type(), params()) for transitional dispatch rather than converting all consumers to pattern matching on Effect enum in one step"
  - "Used parse_test_ability() helpers in test modules to keep tests readable while using typed abilities"
  - "Tasks 1 and 2 executed atomically since changing struct field types requires all consumers to change simultaneously"
  - "remaining_params field on AbilityDefinition preserves PW_Cost and other unconsumed parser params for backward compat"

patterns-established:
  - "parse_test_ability pattern: test modules use the parser to construct typed AbilityDefinition from Forge format strings"
  - "Typed field access: obj.abilities[i].api_type() instead of parse_ability(&obj.abilities[i])"
  - "AbilityKind enum for ability classification: replaced string matching (contains AB$) with AbilityKind::Activated"

requirements-completed: [DATA-02]

# Metrics
duration: 13min
completed: 2026-03-10
---

# Phase 21 Plan 02: Typed Ability Vectors Summary

**End-to-end typed ability pipeline: CardFace through effect resolution uses Vec<AbilityDefinition> instead of Vec<String>, eliminating all runtime re-parsing of ability strings**

## Performance

- **Duration:** 13 min
- **Started:** 2026-03-10T16:57:12Z
- **Completed:** 2026-03-10T17:10:42Z
- **Tasks:** 2
- **Files modified:** 14

## Accomplishments

- Migrated all ability-related fields (abilities, triggers, static_abilities, replacements) from Vec<String> to typed definition vectors across CardFace, GameObject, BackFaceData, and FaceDownData
- Eliminated all runtime string parsing of stored abilities -- abilities are now parsed once at card file parse time and flow typed through the entire engine
- Updated all 14 consumer files across engine, forge-ai, and server-core crates to use typed ability access
- All 627 engine tests + 28 server-core tests + AI tests pass after migration

## Task Commits

Each task was committed atomically:

1. **Task 1: Migrate CardFace, GameObject, BackFaceData, FaceDownData, and parser to typed ability vectors** - `293d436` (feat)
2. **Task 2: Convert effect resolution, casting, coverage, and mana abilities to use typed Effect enum** - `36cd6f9` (feat)

## Files Created/Modified

- `crates/engine/src/types/card.rs` - CardFace fields changed from Vec<String> to typed vectors
- `crates/engine/src/game/game_object.rs` - GameObject and BackFaceData abilities changed to Vec<AbilityDefinition>
- `crates/engine/src/game/morph.rs` - FaceDownData abilities changed to Vec<AbilityDefinition>
- `crates/engine/src/parser/card_parser.rs` - Parser now produces typed definitions at parse time instead of raw strings
- `crates/engine/src/game/engine.rs` - Mana ability and PW_Cost checks use typed access
- `crates/engine/src/game/casting.rs` - Spell casting uses typed AbilityDefinition directly (no parse_ability)
- `crates/engine/src/game/mana_abilities.rs` - is_mana_ability and resolve_mana_ability accept &AbilityDefinition
- `crates/engine/src/game/coverage.rs` - Coverage analysis iterates typed vectors directly
- `crates/engine/src/game/deck_loading.rs` - Direct clone of typed vectors instead of re-parsing strings
- `crates/engine/src/game/planeswalker.rs` - Loyalty cost parsed from remaining_params field
- `crates/engine/src/game/transform.rs` - Test setup uses typed AbilityDefinition
- `crates/forge-ai/src/card_hints.rs` - Card hints use api_type() instead of string contains
- `crates/forge-ai/src/legal_actions.rs` - Ability kind check uses AbilityKind enum
- `crates/server-core/src/filter.rs` - Test abilities use parse_ability for typed construction

## Decisions Made

- **Kept compat bridge methods:** api_type() and params() on AbilityDefinition remain as transitional helpers. Full pattern-matching dispatch on Effect enum variants can happen in a future phase when effect handlers are refactored.
- **Atomic migration:** Tasks 1 and 2 were inherently coupled -- changing the struct field types requires all consumers to change simultaneously for compilation. Committed as two logical groups (structural types vs consumers).
- **PW_Cost via remaining_params:** Planeswalker loyalty costs are read from `remaining_params.get("PW_Cost")` rather than string parsing, leveraging the remaining_params preservation from Plan 01.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Added parse_test_ability helpers across test modules**
- **Found during:** Task 2 (consumer migration)
- **Issue:** Test code throughout the engine pushes raw strings like `obj.abilities.push("SP$ DealDamage | NumDmg$ 3".to_string())` which no longer compiles with Vec<AbilityDefinition>
- **Fix:** Added `parse_test_ability(raw: &str) -> AbilityDefinition` helper functions to test modules in engine.rs, casting.rs, mana_abilities.rs, planeswalker.rs, and coverage.rs. These reuse the existing parser to construct typed test data.
- **Files modified:** engine.rs, casting.rs, mana_abilities.rs, planeswalker.rs, coverage.rs (test modules only)
- **Verification:** All tests compile and pass
- **Committed in:** 36cd6f9

**2. [Rule 3 - Blocking] Fixed server-core filter test abilities**
- **Found during:** Task 2 (consumer migration)
- **Issue:** server-core/src/filter.rs test setup assigned `vec!["DealDamage".to_string()]` to abilities field, incompatible with new Vec<AbilityDefinition> type
- **Fix:** Changed to use `engine::parser::ability::parse_ability("SP$ DealDamage | NumDmg$ 3").unwrap()` for typed construction
- **Files modified:** crates/server-core/src/filter.rs
- **Verification:** `cargo test -p server-core` passes
- **Committed in:** 36cd6f9

---

**Total deviations:** 2 auto-fixed (2 blocking)
**Impact on plan:** Both auto-fixes necessary for compilation after type migration. No scope creep.

## Issues Encountered

None - migration executed cleanly once all consumers were updated atomically.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

- Phase 21 is now complete (all 3 plans done): typed enums defined (01), types integrated into data model (02), MTGJSON loader ready (03)
- Engine consumes typed ability definitions end-to-end from parse time through to effect resolution
- Ready for Phase 22 (Standard Card Coverage) which will use the typed pipeline to validate card support

## Self-Check: PASSED

All 14 modified files verified present. Both task commits (293d436, 36cd6f9) verified in git log. SUMMARY.md created successfully.

---
*Phase: 21-schema-mtgjson-foundation*
*Completed: 2026-03-10*
