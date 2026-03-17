---
phase: 30-implement-the-required-building-blocks-specified-in-the-summary
plan: 02
subsystem: engine
tags: [triggers, targeting, restrictions, replacement-pipeline, parser, damage-prevention]

requires:
  - phase: 30-01
    provides: "Type definitions for trigger_event, current_trigger_event, GameRestriction, RestrictionExpiry, RestrictionScope, TargetFilter event-context variants, Effect::AddRestriction"
provides:
  - "Event-context target resolution (TriggeringSpellController/Owner/Player/Source)"
  - "Trigger event threading from process_triggers through stack to resolution"
  - "GameRestriction pipeline gating in replacement system"
  - "AddRestriction effect handler"
  - "Restriction cleanup at end of turn (CR 514.2)"
  - "Parser support for 'damage can't be prevented this turn'"
affects: [30-03, 30-04]

tech-stack:
  added: []
  patterns:
    - "Transient state pattern: set before resolve, clear after (current_trigger_event)"
    - "Prevention gating via description-based classification of replacement definitions"
    - "Effect routing priority 14a for non-spell 'can't be prevented' text"

key-files:
  created:
    - "crates/engine/src/game/effects/add_restriction.rs"
  modified:
    - "crates/engine/src/game/triggers.rs"
    - "crates/engine/src/game/stack.rs"
    - "crates/engine/src/game/targeting.rs"
    - "crates/engine/src/game/replacement.rs"
    - "crates/engine/src/game/turns.rs"
    - "crates/engine/src/game/effects/mod.rs"
    - "crates/engine/src/parser/oracle_effect.rs"
    - "crates/engine/src/parser/oracle_replacement.rs"
    - "crates/engine/src/parser/oracle.rs"
    - "crates/engine/src/game/engine.rs"
    - "crates/engine/src/types/game_state.rs"

key-decisions:
  - "PendingTrigger carries trigger_event through stack to StackEntryKind::TriggeredAbility"
  - "Prevention gating uses description-based classification (contains 'prevent' and 'damage') rather than a new field"
  - "'damage can't be prevented' routed from replacement parser to effect parser as Effect::AddRestriction"
  - "Priority 14a added in oracle.rs for non-spell permanent 'can't be prevented' text"
  - "fill_source() in add_restriction.rs fills placeholder ObjectId(0) with actual source_id at resolution time"

patterns-established:
  - "resolve_event_context_target() as public API for effects needing event-context resolution"
  - "extract_source_from_event/extract_player_from_event as reusable event data extraction"

requirements-completed: [BB-01, BB-04]

duration: 32min
completed: 2026-03-17
---

# Phase 30 Plan 02: Pipeline Plumbing Summary

**Event-context target resolution and GameRestriction pipeline gating wired through triggers, stack, replacement system, and parser**

## Performance

- **Duration:** 32 min
- **Started:** 2026-03-17T00:07:52Z
- **Completed:** 2026-03-17T00:39:57Z
- **Tasks:** 2
- **Files modified:** 11

## Accomplishments
- Trigger events now flow from process_triggers through the stack to resolution, enabling event-context target resolution
- TriggeringSpellController/Owner/Player/Source auto-resolve from state.current_trigger_event at effect resolution time
- GameRestriction pipeline fully operational: AddRestriction effect pushes restrictions, replacement pipeline skips prevention when disabled, restrictions cleaned up at end of turn
- Parser produces Effect::AddRestriction from "damage can't be prevented this turn" text, including Stomp-style effect chains

## Task Commits

Each task was committed atomically:

1. **Task 1: Thread trigger event data and wire event-context target resolution** - `6520d904d` (feat)
2. **Task 2: Wire GameRestriction pipeline** - `27778db86` (feat)

## Files Created/Modified
- `crates/engine/src/game/effects/add_restriction.rs` - New AddRestriction effect handler with source fill-in
- `crates/engine/src/game/triggers.rs` - trigger_event field on PendingTrigger, threaded through collection and stack push
- `crates/engine/src/game/stack.rs` - Sets/clears current_trigger_event transient state around triggered ability resolution
- `crates/engine/src/game/targeting.rs` - resolve_event_context_target() with extract_source/player_from_event helpers
- `crates/engine/src/game/replacement.rs` - is_prevention_disabled(), is_damage_prevention_replacement(), pipeline gating
- `crates/engine/src/game/turns.rs` - Restriction cleanup in execute_cleanup (CR 514.2)
- `crates/engine/src/game/effects/mod.rs` - Wired AddRestriction dispatch replacing TODO stub
- `crates/engine/src/parser/oracle_effect.rs` - try_parse_damage_prevention_disabled() producing Effect::AddRestriction
- `crates/engine/src/parser/oracle_replacement.rs` - Removed "can't be prevented" handler (now routed to effects)
- `crates/engine/src/parser/oracle.rs` - Removed "can't be prevented" from is_replacement_pattern, added priority 14a routing
- `crates/engine/src/game/engine.rs` - Added trigger_event: None to all PendingTrigger constructors
- `crates/engine/src/types/game_state.rs` - Added trigger_event: None to test PendingTrigger constructors

## Decisions Made
- PendingTrigger carries trigger_event to preserve the matched event through APNAP ordering and stack push
- Prevention gating uses description-based classification rather than adding a new field to ReplacementDefinition -- descriptions like "Prevent all damage" reliably identify prevention replacements
- "damage can't be prevented" rerouted from replacement to effect parsing -- it disables prevention rather than replacing events, so Effect::AddRestriction is the correct semantic model
- fill_source() pattern fills placeholder ObjectId(0) from the parser with the actual source at resolution time -- parser doesn't know runtime source IDs

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Non-spell permanent "can't be prevented" text not reaching effect parser**
- **Found during:** Task 2 (parser wiring)
- **Issue:** For non-spell permanents like Questing Beast, is_effect_sentence_candidate() didn't match "combat damage..." lines, causing them to fall through to Unimplemented
- **Fix:** Added priority 14a routing in oracle.rs that explicitly checks for "damage" + "can't be prevented" and routes to parse_effect_chain
- **Files modified:** crates/engine/src/parser/oracle.rs
- **Verification:** questing_beast_mixed test passes with AddRestriction effect
- **Committed in:** 27778db86 (Task 2 commit)

---

**Total deviations:** 1 auto-fixed (1 blocking)
**Impact on plan:** Parser routing fix was necessary to complete the end-to-end pipeline. No scope creep.

## Issues Encountered
None beyond the auto-fixed deviation.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Event-context target resolution ready for Plan 03 (continuous effect modifiers)
- GameRestriction system ready for future restriction types (attack restrictions, etc.)
- Plan 03 can build on this foundation for combat-related building blocks

---
*Phase: 30-implement-the-required-building-blocks-specified-in-the-summary*
*Completed: 2026-03-17*
