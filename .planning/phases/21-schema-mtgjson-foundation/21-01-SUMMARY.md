---
phase: 21-schema-mtgjson-foundation
plan: 01
subsystem: types
tags: [serde, schemars, typed-enums, ability-schema, mtg-effects]

# Dependency graph
requires: []
provides:
  - "Effect enum with 39 typed variants (38 registry + Other) replacing api_type String + HashMap params"
  - "StaticMode typed enum (16 variants + Other) replacing mode String"
  - "ReplacementEvent typed enum (10 variants + Other) replacing event String"
  - "DamageAmount, TargetSpec, AbilityCost supporting enums"
  - "Compat bridge methods: api_type(), params(), mode_str(), event_str()"
  - "schemars JsonSchema derives on all definition types"
  - "insta snapshot testing dependency"
affects: [21-02-PLAN, 21-03-PLAN]

# Tech tracking
tech-stack:
  added: [schemars v1, insta v1]
  patterns: [typed-enum-with-other-fallback, compat-bridge-methods, internally-tagged-serde]

key-files:
  created:
    - crates/engine/src/types/statics.rs
    - crates/engine/src/types/replacements.rs
  modified:
    - crates/engine/src/types/ability.rs
    - crates/engine/src/types/mod.rs
    - crates/engine/src/types/triggers.rs
    - crates/engine/src/types/identifiers.rs
    - crates/engine/src/types/player.rs
    - crates/engine/src/parser/ability.rs
    - crates/engine/Cargo.toml

key-decisions:
  - "remaining_params field on AbilityDefinition preserves unconsumed parser params for compat"
  - "Display impl on TriggerMode uses Debug formatting for known variants (simple, correct)"
  - "ResolvedAbility left unchanged per plan -- transitional approach for Plan 02"
  - "Compat methods (api_type(), params(), mode_str(), event_str()) bridge typed enums to string consumers"

patterns-established:
  - "Typed enums with Other(String) fallback: all definition enums follow this pattern for forward compat"
  - "FromStr + Display roundtrip: all new enums implement both for string interop"
  - "Compat bridge methods: definition structs expose string-returning methods for gradual consumer migration"
  - "internally-tagged serde: Effect uses #[serde(tag = \"type\")] for discriminated union JSON"

requirements-completed: [DATA-02]

# Metrics
duration: 19min
completed: 2026-03-10
---

# Phase 21 Plan 01: Typed Enum Schema Summary

**Effect enum with 39 typed variants, StaticMode/ReplacementEvent enums, and compat bridge methods replacing HashMap-based ability definitions**

## Performance

- **Duration:** 19 min
- **Started:** 2026-03-10T16:23:16Z
- **Completed:** 2026-03-10T16:42:00Z
- **Tasks:** 2
- **Files modified:** 24

## Accomplishments
- Defined Effect enum with 38 typed variants matching the effect handler registry, plus Other fallback -- each variant has typed fields (DamageAmount, TargetSpec, etc.)
- Created StaticMode (16+1 variants) and ReplacementEvent (10+1 variants) typed enums with FromStr/Display/Serialize/Deserialize/JsonSchema
- Restructured AbilityDefinition, TriggerDefinition, StaticDefinition, ReplacementDefinition to use typed enums instead of String fields
- Updated parser to produce typed structs from Forge format strings, with unconsumed params preserved in remaining_params
- Added compat bridge methods so all 38+ existing consumer files continue compiling without changes
- All 610 existing tests pass plus new serde round-trip tests

## Task Commits

Each task was committed atomically:

1. **Task 1: Add schemars and insta dependencies** - `8f853d0` (chore)
2. **Task 2: Define typed enums and update definition structs** - `2009a4d` (feat)

## Files Created/Modified
- `crates/engine/src/types/statics.rs` - StaticMode enum (Continuous, CantAttack, ..., Other)
- `crates/engine/src/types/replacements.rs` - ReplacementEvent enum (DamageDone, Moved, ..., Other)
- `crates/engine/src/types/ability.rs` - Effect enum (39 variants), DamageAmount, TargetSpec, AbilityCost, restructured definition types
- `crates/engine/src/types/mod.rs` - New module declarations and re-exports
- `crates/engine/src/types/triggers.rs` - Added JsonSchema derive, Display impl to TriggerMode
- `crates/engine/src/types/identifiers.rs` - Added JsonSchema derive to CardId, ObjectId
- `crates/engine/src/types/player.rs` - Added JsonSchema derive to PlayerId
- `crates/engine/src/parser/ability.rs` - Rewritten to produce typed Effect variants from Forge params
- `crates/engine/Cargo.toml` - schemars v1, insta v1
- `crates/engine/src/game/*.rs` - 15 consumer files updated to use typed enum comparisons and compat methods

## Decisions Made
- Added `remaining_params` field to AbilityDefinition to preserve unconsumed parser params (SubAbility, Execute, SpellDescription, etc.) that effect handlers still read from ResolvedAbility.params during resolution
- Used `mode_str()` and `event_str()` compat methods rather than changing consumer function signatures to accept typed enums -- minimizes diff for Plan 02
- TriggerMode Display uses Debug formatting for known variants which produces correct CamelCase output matching Forge strings
- ResolvedAbility intentionally unchanged (has api_type: String and params: HashMap) per plan's transitional approach

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Added JsonSchema derive to ObjectId and PlayerId**
- **Found during:** Task 2 (compilation)
- **Issue:** TargetRef enum contains ObjectId and PlayerId which lacked JsonSchema derive, causing compilation failure
- **Fix:** Added `schemars::JsonSchema` derive to ObjectId, PlayerId, and CardId
- **Files modified:** crates/engine/src/types/identifiers.rs, crates/engine/src/types/player.rs
- **Committed in:** 2009a4d (Task 2 commit)

**2. [Rule 3 - Blocking] Added remaining_params field to preserve unconsumed parser params**
- **Found during:** Task 2 (test failure: sub_ability_chain_damage_then_draw)
- **Issue:** SubAbility/Execute params were consumed by parser and lost in typed Effect, causing sub-ability chain resolution to fail
- **Fix:** Added remaining_params field to AbilityDefinition, populated by parser with unconsumed params, merged into params() compat output
- **Files modified:** crates/engine/src/types/ability.rs, crates/engine/src/parser/ability.rs
- **Committed in:** 2009a4d (Task 2 commit)

---

**Total deviations:** 2 auto-fixed (2 blocking)
**Impact on plan:** Both auto-fixes necessary for compilation and test correctness. No scope creep.

## Issues Encountered
None beyond the auto-fixed deviations above.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- All typed enums defined and derives in place for schema generation
- Parser produces typed structs, enabling Plan 02 to convert effect handlers from HashMap to typed Effect matching
- Compat bridge methods provide clean migration path -- Plan 02 can convert consumers one-by-one, removing compat calls as it goes

---
*Phase: 21-schema-mtgjson-foundation*
*Completed: 2026-03-10*
