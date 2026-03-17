---
phase: 31-kaito-mechanics
plan: 03
subsystem: engine
tags: [emblem, static-ability, layer-system, oracle-parser, command-zone]

# Dependency graph
requires:
  - phase: 31-01
    provides: StaticCondition And/Or/HasCounters, animation parser infrastructure
provides:
  - Effect::CreateEmblem handler for emblem creation in command zone
  - is_emblem flag on GameObject for emblem identification
  - command_zone Vec<ObjectId> on GameState for zone tracking
  - Emblem immunity in destroy, change_zone, bounce, sacrifice handlers
  - Layer system evaluates emblem statics from command zone
  - Oracle parser for "you get an emblem with" pattern
affects: [planeswalker-ultimates, kaito-abilities, static-abilities]

# Tech tracking
tech-stack:
  added: []
  patterns: [command-zone-tracking, emblem-immunity-guard, emblem-static-layer-evaluation]

key-files:
  created:
    - crates/engine/src/game/effects/create_emblem.rs
  modified:
    - crates/engine/src/types/ability.rs
    - crates/engine/src/types/game_state.rs
    - crates/engine/src/game/game_object.rs
    - crates/engine/src/game/zones.rs
    - crates/engine/src/game/layers.rs
    - crates/engine/src/game/effects/mod.rs
    - crates/engine/src/game/effects/destroy.rs
    - crates/engine/src/game/effects/change_zone.rs
    - crates/engine/src/game/effects/bounce.rs
    - crates/engine/src/game/effects/sacrifice.rs
    - crates/engine/src/game/coverage.rs
    - crates/engine/src/parser/oracle_effect.rs

key-decisions:
  - "Emblem immunity via is_emblem guard at top of each removal handler (simple, explicit)"
  - "Command zone tracked as Vec<ObjectId> on GameState (parallels battlefield/exile)"
  - "Emblem statics enter same layer pipeline as battlefield statics (no special path)"
  - "Parser delegates emblem quoted text to parse_static_line for full reuse"

patterns-established:
  - "Emblem immunity: check is_emblem at top of removal handlers before any processing"
  - "Command zone iteration: gather_active_continuous_effects iterates command_zone after battlefield"

requirements-completed: [K31-EMBLEM, K31-PARSE]

# Metrics
duration: 20min
completed: 2026-03-17
---

# Phase 31 Plan 03: Emblem Infrastructure Summary

**Full emblem lifecycle: Effect::CreateEmblem creates persistent command zone object with static abilities evaluated through standard layer system, immune to all removal effects**

## Performance

- **Duration:** 20 min
- **Started:** 2026-03-17T03:12:56Z
- **Completed:** 2026-03-17T03:33:10Z
- **Tasks:** 2
- **Files modified:** 13

## Accomplishments
- Effect::CreateEmblem handler creates emblem GameObject in Zone::Command with is_emblem flag
- Command zone tracking via GameState.command_zone with proper add/remove in zones.rs
- Emblem immunity guards in destroy, change_zone, bounce, and sacrifice handlers
- Layer system extension iterates command zone emblems alongside battlefield for static gathering
- Oracle parser handles "you get an emblem with" pattern, delegating quoted text to parse_static_line
- 8 new tests covering creation, zone tracking, immunity, layer evaluation, and parsing

## Task Commits

Each task was committed atomically:

1. **Task 1: is_emblem, command zone tracking, CreateEmblem handler, immunity** - `bffffc990` (feat)
2. **Task 2: Layer system extension + Oracle parser** - `7d1ad184c` (feat)

## Files Created/Modified
- `crates/engine/src/game/effects/create_emblem.rs` - CreateEmblem effect handler and emblem immunity tests
- `crates/engine/src/types/ability.rs` - Effect::CreateEmblem variant, EffectKind, effect_variant_name
- `crates/engine/src/types/game_state.rs` - command_zone field on GameState
- `crates/engine/src/game/game_object.rs` - is_emblem field on GameObject
- `crates/engine/src/game/zones.rs` - Zone::Command handling in add_to_zone/remove_from_zone
- `crates/engine/src/game/layers.rs` - Command zone iteration in gather_active_continuous_effects
- `crates/engine/src/game/effects/mod.rs` - CreateEmblem dispatch
- `crates/engine/src/game/effects/destroy.rs` - Emblem immunity guard
- `crates/engine/src/game/effects/change_zone.rs` - Emblem immunity guard
- `crates/engine/src/game/effects/bounce.rs` - Emblem immunity guard
- `crates/engine/src/game/effects/sacrifice.rs` - Emblem immunity guard
- `crates/engine/src/game/coverage.rs` - CreateEmblem in coverage no-op arm
- `crates/engine/src/parser/oracle_effect.rs` - try_parse_emblem_creation parser

## Decisions Made
- Emblem immunity implemented as simple `is_emblem` guard at top of each removal handler rather than a centralized check -- explicit and easy to audit per CR 114.4
- Command zone tracked as `Vec<ObjectId>` on GameState, replacing the previous no-op stubs in zones.rs
- Emblem statics enter the same layer evaluation pipeline as battlefield statics -- no special code path needed, just an additional iteration over command_zone objects
- Parser delegates quoted emblem text to existing `parse_static_line` for full reuse of continuous static parsing infrastructure

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
- TypedFilter test construction required direct struct literal (not `TypedFilter::new()`) since emblem filters may have no card_type but still have a subtype
- Subtype must be set on both card_types and base_card_types for layer evaluation to preserve it after reset

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Emblem infrastructure complete for Kaito's +1 loyalty ability
- Parser handles the emblem creation pattern for any planeswalker ultimate
- Ready for Plan 04 (Kaito card definition and integration)

---
*Phase: 31-kaito-mechanics*
*Completed: 2026-03-17*
