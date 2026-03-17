---
phase: 31-kaito-mechanics
plan: 02
subsystem: engine
tags: [quantity-expr, dynamic-count, for-each, parser, effects]

requires:
  - phase: 28-native-ability-data-model
    provides: "Effect enum, QuantityRef/QuantityExpr types, parser infrastructure"
provides:
  - "Dynamic quantity resolution via resolve_quantity() for all count-based effects"
  - "PlayerFilter enum for player-level conditions (OpponentLostLife, etc.)"
  - "QuantityRef variants: ObjectCount, PlayerCount, CountersOnSelf, Variable, TargetPower"
  - "Parser support for 'for each [filter]' suffix patterns"
affects: [kaito-bane-of-nightmares, planeswalker-abilities, triggered-abilities]

tech-stack:
  added: []
  patterns: [resolve_quantity_with_targets for target-dependent quantities, for_each_clause parser decomposition]

key-files:
  created:
    - crates/engine/src/game/quantity.rs
  modified:
    - crates/engine/src/types/ability.rs
    - crates/engine/src/game/effects/draw.rs
    - crates/engine/src/game/effects/life.rs
    - crates/engine/src/game/effects/mill.rs
    - crates/engine/src/game/effects/deal_damage.rs
    - crates/engine/src/parser/oracle_effect.rs

key-decisions:
  - "Added Variable(String) and TargetPower to QuantityRef to preserve DamageAmount::Variable and LifeAmount::TargetPower semantics"
  - "DamageAll retains DamageAmount type (not migrated to QuantityExpr) since it is a different effect with different resolution semantics"
  - "resolve_quantity created as new quantity.rs module rather than extending derived.rs (which is display-only)"
  - "layers.rs QuantityComparison delegated to resolve_quantity for DRY"

patterns-established:
  - "resolve_quantity(state, expr, controller, source_id) for evaluating QuantityExpr at resolution time"
  - "resolve_quantity_with_targets(state, expr, ability) for effects needing target access (TargetPower)"
  - "damage_amount_to_quantity/quantity_to_damage_amount for DamageAmount<->QuantityExpr bridging"
  - "parse_for_each_clause() decomposes 'for each [noun phrase]' into QuantityRef variants"

requirements-completed: [K31-QTY, K31-PARSE]

duration: 49min
completed: 2026-03-17
---

# Phase 31 Plan 02: Dynamic Quantity Resolution Summary

**QuantityExpr replaces fixed counts on Draw/LoseLife/Mill/DealDamage/GainLife with dynamic for-each resolution via resolve_quantity() and parser support**

## Performance

- **Duration:** 49 min
- **Started:** 2026-03-17T02:20:37Z
- **Completed:** 2026-03-17T03:10:00Z
- **Tasks:** 2
- **Files modified:** 30

## Accomplishments
- Extended QuantityRef with ObjectCount, PlayerCount, CountersOnSelf, Variable, TargetPower
- Added PlayerFilter enum (Opponent, OpponentLostLife, OpponentGainedLife, All)
- Created resolve_quantity() and resolve_quantity_with_targets() in new quantity.rs module
- Migrated all 5 count-based effects (Draw, LoseLife, Mill, DealDamage, GainLife) from fixed types to QuantityExpr
- Updated all ~30 construction sites across resolvers, parser, tests, and utility modules
- Added "for each" parser detecting OpponentLostLife, ObjectCount, CountersOnSelf patterns
- All 1445 engine tests pass

## Task Commits

Each task was committed atomically:

1. **Task 1: Add QuantityRef variants, PlayerFilter, and resolve_quantity helper** - `11f1371b7` (feat)
2. **Task 2: Replace fixed counts on Draw/LoseLife/Mill/DealDamage/GainLife with QuantityExpr + parser** - `e030c49da` (feat)

## Files Created/Modified
- `crates/engine/src/game/quantity.rs` - New module: resolve_quantity() for dynamic count evaluation
- `crates/engine/src/types/ability.rs` - QuantityRef variants, PlayerFilter enum, Effect type changes
- `crates/engine/src/game/effects/draw.rs` - Resolver calls resolve_quantity for count
- `crates/engine/src/game/effects/life.rs` - Resolvers call resolve_quantity for GainLife/LoseLife amounts
- `crates/engine/src/game/effects/mill.rs` - Resolver calls resolve_quantity for count
- `crates/engine/src/game/effects/deal_damage.rs` - Resolver calls resolve_quantity_with_targets for amount
- `crates/engine/src/parser/oracle_effect.rs` - parse_for_each_effect, parse_for_each_clause, conversion helpers
- `crates/engine/src/game/layers.rs` - Delegated QuantityComparison to resolve_quantity (DRY)
- 22 other files updated for QuantityExpr construction site migration

## Decisions Made
- Added Variable(String) and TargetPower to QuantityRef to subsume DamageAmount::Variable and LifeAmount::TargetPower
- DamageAll left with DamageAmount type since it has different resolution semantics
- quantity.rs as new module (derived.rs is display-only, wrong semantic fit)
- Bridge functions (damage_amount_to_quantity, quantity_to_damage_amount) for DamageAmount<->QuantityExpr interop in parser

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] DamageAll still uses DamageAmount**
- **Found during:** Task 2
- **Issue:** Plan said to migrate DealDamage to QuantityExpr but DamageAll shares DamageAmount type; migrating DamageAll would expand scope
- **Fix:** Kept DamageAll with DamageAmount; added bridge functions for DealDamage<->DamageAmount conversion in parser
- **Files modified:** crates/engine/src/parser/oracle_effect.rs
- **Committed in:** e030c49da

**2. [Rule 2 - Missing Critical] Added TargetPower and Variable to QuantityRef**
- **Found during:** Task 1
- **Issue:** Replacing DamageAmount and LifeAmount with QuantityExpr would lose DamageAmount::Variable and LifeAmount::TargetPower semantics
- **Fix:** Added QuantityRef::Variable(String) and QuantityRef::TargetPower to preserve existing functionality
- **Files modified:** crates/engine/src/types/ability.rs
- **Committed in:** 11f1371b7

---

**Total deviations:** 2 auto-fixed (1 blocking, 1 missing critical)
**Impact on plan:** Both necessary to preserve existing functionality while migrating to QuantityExpr. No scope creep.

## Issues Encountered
- Concurrent agent modifications to ability.rs and layers.rs caused merge conflicts during development; resolved by restoring committed versions
- 77 files needed QuantityExpr imports added after type migration (resolved via Python batch script)

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Dynamic quantity resolution system ready for Kaito's loyalty abilities
- "draw a card for each opponent who lost life this turn" parses correctly
- resolve_quantity infrastructure available for any future "for each" patterns

---
*Phase: 31-kaito-mechanics*
*Completed: 2026-03-17*
