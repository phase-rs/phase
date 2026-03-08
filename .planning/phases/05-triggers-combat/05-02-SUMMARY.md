---
phase: 05-triggers-combat
plan: 02
subsystem: engine
tags: [triggers, event-pipeline, apnap, mtg-rules, stack, triggered-abilities]

requires:
  - phase: 05-triggers-combat
    provides: "TriggerMode enum with 137 variants, Keyword enum with 50+ variants"
  - phase: 04-ability-system
    provides: "ResolvedAbility, effect handlers, SVar resolution, stack resolution"
provides:
  - "Trigger matching pipeline: event -> scan battlefield -> match triggers -> APNAP order -> stack"
  - "TriggerMatcher registry with all 137 TriggerMode variants registered"
  - "20+ core trigger matchers with real logic (ChangesZone, DamageDone, SpellCast, etc.)"
  - "StackEntryKind::TriggeredAbility for triggered abilities on the stack"
  - "trigger_definitions field on GameObject for parsed trigger storage"
  - "process_triggers integrated into engine::apply() after SBA processing"
affects: [05-triggers-combat, 06-layers-replacements, combat-system]

tech-stack:
  added: []
  patterns: ["fn-pointer trigger registry keyed by TriggerMode enum", "APNAP sort-then-reverse for stack placement"]

key-files:
  created:
    - crates/engine/src/game/triggers.rs
  modified:
    - crates/engine/src/game/engine.rs
    - crates/engine/src/game/mod.rs
    - crates/engine/src/game/game_object.rs
    - crates/engine/src/game/stack.rs
    - crates/engine/src/types/game_state.rs
    - crates/engine/src/types/events.rs

key-decisions:
  - "Build trigger registry per process_triggers call (cheap, same pattern as effect registry)"
  - "Trigger definitions stored on GameObject at creation time (avoid re-parsing on every scan)"
  - "APNAP ordering via sort-by-key then reverse for correct LIFO stack placement"
  - "Unimplemented trigger modes return false (recognized but don't fire until events exist)"
  - "Trigger Execute param resolves SVars via existing parser::parse_ability"

patterns-established:
  - "TriggerMatcher fn pointer registry: HashMap<TriggerMode, fn> parallel to effect handler registry"
  - "PendingTrigger struct collects matched triggers before APNAP sorting and stack placement"
  - "card_matches_filter helper: Forge-style dot-separated qualifiers (Creature.YouCtrl, Card.Self)"
  - "zone_matches helper: param string to Zone enum comparison with Any wildcard"

requirements-completed: [TRIG-01, TRIG-02, TRIG-03, TRIG-04]

duration: 5min
completed: 2026-03-08
---

# Phase 5 Plan 2: Trigger Pipeline & Engine Integration Summary

**Event-to-trigger pipeline with 137-mode registry, APNAP ordering, and automatic stack placement integrated into engine::apply()**

## Performance

- **Duration:** 5 min
- **Started:** 2026-03-08T01:08:44Z
- **Completed:** 2026-03-08T01:14:39Z
- **Tasks:** 2
- **Files modified:** 7

## Accomplishments
- Full trigger matching pipeline: events scan battlefield permanents, match trigger definitions against registry, order APNAP, place on stack
- All 137 TriggerMode variants registered in the trigger registry with 20+ core matchers having real matching logic
- Engine integration: triggers automatically fire after every action and SBA processing
- Triggered abilities resolve through existing stack/priority system seamlessly
- 18 tests covering matchers, APNAP ordering, helpers, and integration scenarios

## Task Commits

Each task was committed atomically:

1. **Task 1: Trigger matching pipeline and mode registry** - `8080a4e` (feat)
2. **Task 2: Engine integration -- trigger processing after actions** - `73e2b85` (feat)

## Files Created/Modified
- `crates/engine/src/game/triggers.rs` - Trigger matching pipeline, registry, matchers, helpers, tests
- `crates/engine/src/game/engine.rs` - process_triggers integration after SBA check
- `crates/engine/src/game/mod.rs` - Added triggers module and process_triggers re-export
- `crates/engine/src/game/game_object.rs` - Added trigger_definitions field to GameObject
- `crates/engine/src/game/stack.rs` - TriggeredAbility variant handling in resolve_top
- `crates/engine/src/types/game_state.rs` - TriggeredAbility variant in StackEntryKind
- `crates/engine/src/types/events.rs` - AttackersDeclared, BlockersDeclared, BecomesTarget variants

## Decisions Made
- Build trigger registry per call (cheap HashMap creation, consistent with effect registry pattern)
- Store trigger_definitions on GameObject at creation time to avoid re-parsing during hot path
- APNAP ordering via sort-by-key(controller, timestamp) then reverse for correct LIFO placement
- Unimplemented trigger modes use match_unimplemented returning false (no incorrect behavior)
- Execute param resolves SVars using existing parse_ability infrastructure from Phase 04

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

None.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- Trigger pipeline ready for combat triggers (05-03)
- AttackersDeclared/BlockersDeclared events ready for combat system to emit
- StackEntryKind::TriggeredAbility integrates with existing stack resolution
- All 318 workspace tests pass

---
*Phase: 05-triggers-combat*
*Completed: 2026-03-08*
