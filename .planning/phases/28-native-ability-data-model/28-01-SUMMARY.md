---
phase: 28-native-ability-data-model
plan: 01
subsystem: types
tags: [serde, schemars, discriminated-unions, type-system, mtg-data-model]

# Dependency graph
requires:
  - phase: 21-mtgjson-typed-abilities
    provides: Effect enum (38 variants), TriggerMode, StaticMode, ReplacementEvent, ManaCost
provides:
  - TargetFilter enum replacing TargetSpec with typed filter matching
  - Typed TriggerDefinition, StaticDefinition, ReplacementDefinition (zero HashMap)
  - ContinuousModification enum with layer() method
  - Duration, PtValue, TypeFilter, ControllerRef, FilterProp, StaticCondition types
  - Expanded AbilityCost (PayLife, Discard, Exile, TapCreatures, ManaCost)
  - Simplified ResolvedAbility (zero HashMap params/svars)
  - CardFace with Vec<Keyword> and Option<PtValue>
  - GameObject without svars field
  - Keywords with ManaCost for cost-bearing variants
affects: [28-02, 28-03, 28-04, 28-05, 28-06, phase-ai, engine-wasm, phase-server]

# Tech tracking
tech-stack:
  added: []
  patterns: [typed-definitions-zero-hashmap, continuous-modification-enum, target-filter-enum]

key-files:
  created: []
  modified:
    - crates/engine/src/types/ability.rs
    - crates/engine/src/types/card.rs
    - crates/engine/src/types/keywords.rs
    - crates/engine/src/types/layers.rs
    - crates/engine/src/types/mod.rs
    - crates/engine/src/game/game_object.rs
    - crates/engine/src/types/zones.rs
    - crates/engine/src/types/phase.rs
    - crates/engine/src/types/mana.rs
    - crates/engine/src/types/card_type.rs

key-decisions:
  - "Effect::Unimplemented replaces Effect::Other -- zero HashMap, semantic marker for 2,533 unimplemented abilities per RESEARCH.md"
  - "ContinuousModification defined in ability.rs with layer() impl in layers.rs -- single enum for all continuous effect modifications"
  - "TargetFilter uses nested And/Or/Not combinators with struct wrapper fields for serde compatibility"
  - "Keywords parse cost strings through parse_keyword_mana_cost() -- supports both MTGJSON brace format and simple format"
  - "JsonSchema added to Zone, Phase, ManaColor, ManaCost, ManaCostShard, CoreType, Keyword, ProtectionTarget for schema generation"

patterns-established:
  - "Zero-HashMap definitions: all definition structs use typed fields, never HashMap<String, String>"
  - "ContinuousModification.layer(): each modification variant knows its own CR 613 layer"
  - "TargetFilter as universal filter type: replaces TargetSpec, Forge filter strings, and all String-based filtering"

requirements-completed: [NAT-01, NAT-03, NAT-04]

# Metrics
duration: 10min
completed: 2026-03-11
---

# Phase 28 Plan 01: Type Definitions Summary

**Fully typed data model with TargetFilter, ContinuousModification, typed definitions, ManaCost keywords, and PtValue -- zero HashMap fields in types/ability.rs, card.rs, layers.rs**

## Performance

- **Duration:** 10 min
- **Started:** 2026-03-11T05:46:35Z
- **Completed:** 2026-03-11T05:56:32Z
- **Tasks:** 2
- **Files modified:** 10

## Accomplishments
- All definition structs (TriggerDefinition, StaticDefinition, ReplacementDefinition, AbilityDefinition) rewritten with typed fields, zero HashMap
- TargetFilter enum with full type hierarchy (TypeFilter, ControllerRef, FilterProp, And/Or/Not combinators)
- ContinuousModification enum replaces both ContinuousEffectApplication and HashMap params on ActiveContinuousEffect
- Effect::Other removed, Effect::Unimplemented added as zero-HashMap semantic marker
- Keywords use ManaCost for 37 cost-bearing variants; Enchant uses TargetFilter; EtbCounter is typed struct
- CardFace uses Vec<Keyword> and Option<PtValue> instead of Vec<String> and Option<String>
- ResolvedAbility simplified to 5 fields (zero params/svars HashMap)

## Task Commits

Each task was committed atomically:

1. **Task 1: Define new types, rewrite definition structs, and add serde roundtrip tests** - `1b606a83` (feat)
2. **Task 2: Clean Effect enum variants and CardFace/GameObject** - `c22c0e3e` (feat)
3. **JsonSchema derives for supporting types** - `27ac755d` (chore)

## Files Created/Modified
- `crates/engine/src/types/ability.rs` - Complete rewrite: TargetFilter, Duration, PtValue, typed definitions, cleaned Effect enum, typed AbilityCost, serde roundtrip tests
- `crates/engine/src/types/card.rs` - CardFace without svars, with Vec<Keyword> and PtValue
- `crates/engine/src/types/keywords.rs` - Cost-bearing keywords use ManaCost, Enchant uses TargetFilter, EtbCounter typed
- `crates/engine/src/types/layers.rs` - ContinuousModification enum with layer() method, typed ActiveContinuousEffect
- `crates/engine/src/types/mod.rs` - Updated exports for new types, removed TargetSpec and ContinuousEffectApplication
- `crates/engine/src/game/game_object.rs` - Removed svars HashMap field
- `crates/engine/src/types/zones.rs` - Added JsonSchema derive
- `crates/engine/src/types/phase.rs` - Added JsonSchema derive
- `crates/engine/src/types/mana.rs` - Added JsonSchema to ManaColor, ManaCost, ManaCostShard
- `crates/engine/src/types/card_type.rs` - Added JsonSchema to CoreType

## Decisions Made
- Effect::Unimplemented { name, description } replaces Effect::Other -- carries zero HashMap, architecturally distinct, needed for 2,533 abilities per RESEARCH.md Open Question #3
- ContinuousModification defined in ability.rs (near StaticDefinition that uses it), with layer() impl in layers.rs via an impl block on the imported type
- TargetFilter nested variants (Not, Or, And) use struct wrapper fields (`filter: Box<TargetFilter>`, `filters: Vec<TargetFilter>`) for serde tagged enum compatibility
- Keywords FromStr parses cost parameters through parse_keyword_mana_cost() supporting both MTGJSON brace format ({1}{W}) and simple format (1W)
- Added JsonSchema derives to 8 types that are referenced by ability definitions -- required for schemars schema generation

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Added JsonSchema derives to supporting types**
- **Found during:** Task 2 (compilation check)
- **Issue:** Zone, Phase, ManaColor, ManaCost, ManaCostShard, CoreType, Keyword, ProtectionTarget lacked JsonSchema derive but are used in types that derive JsonSchema
- **Fix:** Added `schemars::JsonSchema` derive to all 8 types
- **Files modified:** zones.rs, phase.rs, mana.rs, card_type.rs, keywords.rs
- **Verification:** Types module compiles with zero errors
- **Committed in:** 27ac755d

---

**Total deviations:** 1 auto-fixed (1 blocking)
**Impact on plan:** Required for compilation. No scope creep.

## Issues Encountered
- Serde roundtrip tests cannot be executed via `cargo test -p engine types::ability::tests` because the engine crate has 264 compilation errors in consumer files (game/, parser/, database/) that reference removed types (TargetSpec, Effect::Other, HashMap params). This is expected and documented in the plan: "cargo check -p engine will NOT succeed." The types module itself compiles with zero errors. Plans 02-06 fix consumer files, after which tests will pass.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Type definitions are the foundation for Plans 02-06
- Plan 02: Runtime consumer migration (triggers.rs, effects/, layers.rs, filter.rs)
- Plan 03: Additional consumer fixes and test migration
- 264 consumer file compilation errors expected and documented -- all traced to the type changes made here

## Self-Check: PASSED

All 10 modified files verified present. All 3 task commits verified in git log.

---
*Phase: 28-native-ability-data-model*
*Completed: 2026-03-11*
