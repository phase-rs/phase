---
phase: 04-ability-system-effects
plan: 01
subsystem: engine
tags: [effects, abilities, game-state, registry, targeting]

requires:
  - phase: 03-game-state-engine
    provides: "GameState, GameObject, zones module, StackEntry, Player with life tracking"
provides:
  - "ResolvedAbility struct for runtime ability resolution"
  - "TargetRef enum for unified Object/Player targeting"
  - "EffectError type with thiserror derives"
  - "Effect registry mapping 15 api_type strings to handler functions"
  - "resolve_effect dispatch function"
  - "8 new GameEvent variants for effect tracking"
affects: [04-02-casting-flow, 04-03-sub-ability-chaining, 05-triggers-combat]

tech-stack:
  added: []
  patterns: [effect-handler-registry, param-hashmap-resolve-pattern]

key-files:
  created:
    - crates/engine/src/game/effects/mod.rs
    - crates/engine/src/game/effects/draw.rs
    - crates/engine/src/game/effects/deal_damage.rs
    - crates/engine/src/game/effects/change_zone.rs
    - crates/engine/src/game/effects/pump.rs
    - crates/engine/src/game/effects/destroy.rs
    - crates/engine/src/game/effects/counter.rs
    - crates/engine/src/game/effects/token.rs
    - crates/engine/src/game/effects/life.rs
    - crates/engine/src/game/effects/tap_untap.rs
    - crates/engine/src/game/effects/counters.rs
    - crates/engine/src/game/effects/sacrifice.rs
    - crates/engine/src/game/effects/discard.rs
  modified:
    - crates/engine/src/types/ability.rs
    - crates/engine/src/types/events.rs
    - crates/engine/src/types/mod.rs
    - crates/engine/src/game/mod.rs

key-decisions:
  - "EffectHandler as fn pointer (not trait) for simplicity and HashMap storage"
  - "Each handler emits EffectResolved event after completing for tracking"
  - "Destroy checks indestructible keyword case-insensitively"
  - "Discard supports both targeted and non-targeted modes"
  - "Token uses CardId(0) convention for non-card objects"

patterns-established:
  - "Effect handler pattern: fn(state, ability, events) -> Result<(), EffectError>"
  - "Param reading pattern: ability.params.get(key).parse() with defaults"
  - "Registry pattern: HashMap<String, EffectHandler> built via build_registry()"

requirements-completed: [ABIL-06]

duration: 5min
completed: 2026-03-08
---

# Phase 04 Plan 01: Effect Handler Registry Summary

**15 effect handlers with registry dispatch, ResolvedAbility runtime types, and TargetRef unified targeting**

## Performance

- **Duration:** 5 min
- **Started:** 2026-03-08T00:00:27Z
- **Completed:** 2026-03-08T00:05:53Z
- **Tasks:** 2
- **Files modified:** 17

## Accomplishments
- ResolvedAbility, TargetRef, and EffectError types with full serde support
- Complete effect handler registry with all 15 handlers (DealDamage, Draw, ChangeZone, Pump, Destroy, Counter, Token, GainLife, LoseLife, Tap, Untap, AddCounter, RemoveCounter, Sacrifice, DiscardCard)
- 8 new GameEvent variants for effect resolution tracking
- 247 total engine tests passing (48 new tests added)

## Task Commits

Each task was committed atomically:

1. **Task 1: Runtime ability types and effect error types** - `296fcd1` (feat)
2. **Task 2: Effect handler registry and 15 effect handlers** - `265cf78` (feat)

## Files Created/Modified
- `crates/engine/src/types/ability.rs` - Added TargetRef, ResolvedAbility, EffectError types
- `crates/engine/src/types/events.rs` - Added 8 new GameEvent variants for effects
- `crates/engine/src/types/mod.rs` - Re-exported new types
- `crates/engine/src/game/mod.rs` - Added effects module declaration
- `crates/engine/src/game/effects/mod.rs` - Registry, EffectHandler type, resolve_effect dispatch
- `crates/engine/src/game/effects/deal_damage.rs` - DealDamage handler (NumDmg param)
- `crates/engine/src/game/effects/draw.rs` - Draw handler (NumCards param)
- `crates/engine/src/game/effects/change_zone.rs` - ChangeZone handler (Origin/Destination params)
- `crates/engine/src/game/effects/pump.rs` - Pump handler (NumAtt/NumDef params)
- `crates/engine/src/game/effects/destroy.rs` - Destroy handler with indestructible check
- `crates/engine/src/game/effects/counter.rs` - Counter handler (removes from stack)
- `crates/engine/src/game/effects/token.rs` - Token handler (creates GameObject on battlefield)
- `crates/engine/src/game/effects/life.rs` - GainLife/LoseLife handlers (LifeAmount param)
- `crates/engine/src/game/effects/tap_untap.rs` - Tap/Untap handlers
- `crates/engine/src/game/effects/counters.rs` - AddCounter/RemoveCounter handlers
- `crates/engine/src/game/effects/sacrifice.rs` - Sacrifice handler
- `crates/engine/src/game/effects/discard.rs` - DiscardCard handler (targeted + non-targeted)

## Decisions Made
- Used fn pointer type for EffectHandler rather than a trait -- simpler for HashMap storage and no need for dynamic dispatch complexity
- Each handler emits an EffectResolved event after completing, enabling effect tracking and trigger detection
- Destroy checks for indestructible keyword case-insensitively to match Forge behavior
- Token objects use CardId(0) convention to distinguish them from real cards
- Discard supports both targeted mode (specific cards) and non-targeted mode (from end of hand)

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Effect registry ready for casting flow integration (Plan 02)
- ResolvedAbility struct supports sub_ability chaining needed by Plan 03
- All handlers follow consistent param-reading pattern for easy extension

---
*Phase: 04-ability-system-effects*
*Completed: 2026-03-08*
