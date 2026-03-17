---
phase: 30-implement-the-required-building-blocks-specified-in-the-summary
plan: 01
subsystem: engine
tags: [rust, types, parser, targeting, restrictions, adventure]

requires:
  - phase: 28-native-ability-data-model
    provides: "Typed Effect enum, TargetFilter, EffectKind, typed ability definitions"
provides:
  - "TargetFilter event-context variants (TriggeringSpellController/Owner/Player/Source)"
  - "GameRestriction/RestrictionExpiry/RestrictionScope types for damage prevention disabling"
  - "CastingPermission enum with AdventureCreature variant"
  - "Effect::AddRestriction variant"
  - "SpellCastingOptionKind::CastAdventure"
  - "GameState.restrictions and current_trigger_event fields"
  - "StackEntryKind::TriggeredAbility.trigger_event field"
  - "GameObject.casting_permissions field"
  - "parse_event_context_ref() parser function"
affects: [30-02, 30-03, 30-04]

tech-stack:
  added: []
  patterns:
    - "Event-context TargetFilter variants auto-resolve (no player targeting required)"
    - "GameRestriction system with typed expiry and scope"
    - "parse_event_context_ref() called before parse_target() in effect parsing"

key-files:
  created: []
  modified:
    - "crates/engine/src/types/ability.rs"
    - "crates/engine/src/types/game_state.rs"
    - "crates/engine/src/types/events.rs"
    - "crates/engine/src/game/game_object.rs"
    - "crates/engine/src/game/filter.rs"
    - "crates/engine/src/game/triggers.rs"
    - "crates/engine/src/game/effects/mod.rs"
    - "crates/engine/src/parser/oracle_target.rs"
    - "crates/engine/src/parser/oracle_effect.rs"

key-decisions:
  - "GameEvent derives Eq to satisfy StackEntryKind Eq derive with new trigger_event field"
  - "Event-context TargetFilter variants excluded from extract_target_filter_from_effect to prevent unwanted target selection"
  - "TriggeringSource used for both 'that source' and 'that permanent' (same semantic in trigger context)"
  - "Effect::AddRestriction handler is no-op stub pending Plan 02 wiring"

patterns-established:
  - "Event-context refs: parse_event_context_ref() before parse_target() in any parsing path"
  - "Auto-resolve variants: add to exclusion list in extract_target_filter_from_effect()"
  - "New TargetFilter variants: add matching arms to both filter.rs and triggers.rs exhaustive matches"

requirements-completed: [BB-01, BB-02, BB-04]

duration: 21min
completed: 2026-03-16
---

# Phase 30 Plan 01: Building Block Type Definitions Summary

**Event-context TargetFilter variants, GameRestriction system, CastingPermission type, and parser patterns for trigger-context possessive references**

## Performance

- **Duration:** 21 min
- **Started:** 2026-03-16T23:43:56Z
- **Completed:** 2026-03-17T00:04:42Z
- **Tasks:** 2
- **Files modified:** 13

## Accomplishments

- Four new TargetFilter variants for event-context trigger resolution (TriggeringSpellController, TriggeringSpellOwner, TriggeringPlayer, TriggeringSource)
- GameRestriction type system with DamagePreventionDisabled, typed expiry, and scoped restriction
- CastingPermission enum and casting_permissions field on GameObject for Adventure support
- Parser produces correct event-context targets from Oracle text ("that spell's controller", "that player", etc.)
- Effect parsing routes "deals N damage to that spell's controller" through event-context resolution

## Task Commits

Each task was committed atomically:

1. **Task 1: Add type definitions for all four building blocks** - `e9393a9e8` (feat)
2. **Task 2: Add parser patterns for event-context possessive references** - `d64ca3dc4` (feat)

## Files Created/Modified

- `crates/engine/src/types/ability.rs` - TargetFilter event-context variants, GameRestriction/RestrictionExpiry/RestrictionScope, CastingPermission, Effect::AddRestriction, CastAdventure
- `crates/engine/src/types/game_state.rs` - restrictions field, current_trigger_event field, trigger_event on TriggeredAbility
- `crates/engine/src/types/events.rs` - GameEvent derives Eq
- `crates/engine/src/game/game_object.rs` - casting_permissions field on GameObject
- `crates/engine/src/game/filter.rs` - Event-context TargetFilter match arms (return false)
- `crates/engine/src/game/triggers.rs` - Event-context exclusion in extract_target_filter_from_effect, match arms in target_filter_matches_object
- `crates/engine/src/game/engine.rs` - trigger_event: None in TriggeredAbility construction
- `crates/engine/src/game/priority.rs` - trigger_event: None in TriggeredAbility construction
- `crates/engine/src/game/effects/counter.rs` - trigger_event: None in test TriggeredAbility
- `crates/engine/src/game/effects/mod.rs` - AddRestriction handler stub
- `crates/engine/src/parser/oracle_target.rs` - parse_event_context_ref() function with 7 tests
- `crates/engine/src/parser/oracle_effect.rs` - Event-context integration in try_parse_damage(), 2 tests

## Decisions Made

- GameEvent derives Eq: required because StackEntryKind derives Eq and trigger_event is Option<GameEvent>
- Event-context TargetFilter variants excluded from extract_target_filter_from_effect(): they auto-resolve like SelfRef/Controller, no player selection needed
- TriggeringSource handles both "that source" and "that permanent" since in trigger context both refer to the triggering event's source object
- Effect::AddRestriction handler is a no-op stub -- actual restriction wiring deferred to Plan 02

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Added GameEvent Eq derive**
- **Found during:** Task 1 (compilation)
- **Issue:** StackEntryKind derives Eq, but new trigger_event: Option<GameEvent> requires GameEvent: Eq
- **Fix:** Added Eq to GameEvent derive macro
- **Files modified:** crates/engine/src/types/events.rs
- **Committed in:** e9393a9e8

**2. [Rule 3 - Blocking] Added TargetFilter match arms in filter.rs and triggers.rs**
- **Found during:** Task 1 (compilation)
- **Issue:** Two exhaustive matches on TargetFilter didn't cover new variants
- **Fix:** Added match arms returning false (event-context refs resolve to players, not objects)
- **Files modified:** crates/engine/src/game/filter.rs, crates/engine/src/game/triggers.rs
- **Committed in:** e9393a9e8

**3. [Rule 3 - Blocking] Added Effect::AddRestriction to effects/mod.rs dispatch**
- **Found during:** Task 1 (compilation)
- **Issue:** Exhaustive match on Effect in resolve_effect() didn't cover AddRestriction
- **Fix:** Added no-op handler stub
- **Files modified:** crates/engine/src/game/effects/mod.rs
- **Committed in:** e9393a9e8

**4. [Rule 3 - Blocking] Updated all TriggeredAbility struct literal sites with trigger_event: None**
- **Found during:** Task 1 (compilation)
- **Issue:** 4 construction sites needed new trigger_event field
- **Files modified:** engine.rs (2), triggers.rs (1), priority.rs (1), effects/counter.rs (1)
- **Committed in:** e9393a9e8

---

**Total deviations:** 4 auto-fixed (all Rule 3 blocking)
**Impact on plan:** All auto-fixes were compilation-required propagation of new type fields/variants. No scope creep.

## Issues Encountered

None -- all issues were expected type propagation during compilation.

## Next Phase Readiness

- All type contracts stable for Plan 02 (pipeline wiring) and Plans 03-04 (Adventure)
- GameState.restrictions ready for AddRestriction handler wiring
- current_trigger_event ready for stack.rs to set before triggered ability resolution
- casting_permissions ready for Adventure exile-to-cast flow
- 1402 tests passing, zero clippy warnings

---
*Phase: 30-implement-the-required-building-blocks-specified-in-the-summary*
*Completed: 2026-03-16*
