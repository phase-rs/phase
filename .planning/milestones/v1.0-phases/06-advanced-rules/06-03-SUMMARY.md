---
phase: 06-advanced-rules
plan: 03
subsystem: engine
tags: [replacement-hooks, layer-integration, mutation-pipeline, combat-damage, sba]

requires:
  - phase: 06-advanced-rules
    provides: replace_event() pipeline, ProposedEvent enum, ReplacementResult types
  - phase: 06-advanced-rules
    provides: evaluate_layers() function, layers_dirty flag, base_* fields
provides:
  - All 14 mutation sites wired through replace_event() pipeline
  - Layer evaluation integrated into SBA, engine loop, and target checks
  - Nested replacement in destroy handler (Destroy -> ZoneChange)
  - 5 end-to-end integration tests proving full pipeline
affects: [07-ui, 08-ai]

tech-stack:
  added: []
  patterns: [replace-event-before-mutation, nested-replacement, conditional-layer-reset]

key-files:
  created: []
  modified:
    - crates/engine/src/game/effects/deal_damage.rs
    - crates/engine/src/game/effects/draw.rs
    - crates/engine/src/game/effects/life.rs
    - crates/engine/src/game/effects/change_zone.rs
    - crates/engine/src/game/effects/destroy.rs
    - crates/engine/src/game/effects/counters.rs
    - crates/engine/src/game/effects/token.rs
    - crates/engine/src/game/effects/discard.rs
    - crates/engine/src/game/effects/tap_untap.rs
    - crates/engine/src/game/effects/sacrifice.rs
    - crates/engine/src/game/zones.rs
    - crates/engine/src/game/combat_damage.rs
    - crates/engine/src/game/turns.rs
    - crates/engine/src/game/sba.rs
    - crates/engine/src/game/engine.rs
    - crates/engine/src/game/casting.rs
    - crates/engine/src/game/layers.rs

key-decisions:
  - "Nested replacement in destroy: Destroy handler creates ZoneChange proposal after Execute, enabling Moved replacements to intercept destruction zone transfer"
  - "Conditional layer reset: only reset P/T and keywords when base values are set, preventing layer evaluation from wiping non-layer objects"
  - "layers_dirty set on all battlefield zone changes and P/T counter modifications"

patterns-established:
  - "replace_event wrapping pattern: create ProposedEvent, match on Execute/Prevented/NeedsChoice"
  - "Nested replacement: Destroy -> Execute -> ZoneChange -> Moved replacement intercepts"
  - "Layer evaluation gates: if layers_dirty before SBA, engine loop, and targeting"

requirements-completed: [REPL-01, STAT-01, STAT-02, STAT-03]

duration: 9min
completed: 2026-03-08
---

# Phase 06 Plan 03: Replacement Hook Wiring and Layer Integration Summary

**All 14 mutation sites wired through replace_event() pipeline with layer evaluation in SBA/engine/targeting checkpoints and 5 end-to-end integration tests**

## Performance

- **Duration:** 9 min
- **Started:** 2026-03-08T04:52:23Z
- **Completed:** 2026-03-08T05:01:23Z
- **Tasks:** 2
- **Files modified:** 17

## Accomplishments
- All 10 effect handlers, combat damage, untap step, and zone transfers route mutations through replace_event() before applying changes
- Layer evaluation integrated at three checkpoints: before SBA fixpoint loop, after SBA+trigger processing in engine apply(), and before target legality checks in casting
- 5 integration tests prove: destruction-to-exile replacement, layer eval -> SBA 0-toughness kill, combat damage prevention, lord buff lifecycle, and full destroy -> replacement -> trigger pipeline
- Nested replacement pattern: destroy handler creates ZoneChange proposal after Destroy executes, allowing Moved replacements to intercept the actual zone transfer

## Task Commits

1. **Task 1: Add replace_event hooks to all mutation sites** - `78f5a06` (feat)
2. **Task 2: Layer evaluation integration and end-to-end tests** - `2756215` (feat)

## Files Created/Modified
- `crates/engine/src/game/effects/deal_damage.rs` - Damage through replace_event with Prevented/NeedsChoice handling
- `crates/engine/src/game/effects/draw.rs` - Draw count through replace_event
- `crates/engine/src/game/effects/life.rs` - Life gain/loss through replace_event
- `crates/engine/src/game/effects/change_zone.rs` - Zone changes through replace_event with layers_dirty
- `crates/engine/src/game/effects/destroy.rs` - Nested replacement: Destroy then ZoneChange proposals
- `crates/engine/src/game/effects/counters.rs` - Counter add/remove through replace_event with layers_dirty for P/T counters
- `crates/engine/src/game/effects/token.rs` - Token creation through replace_event
- `crates/engine/src/game/effects/discard.rs` - Discard through replace_event with ZoneChange redirect support
- `crates/engine/src/game/effects/tap_untap.rs` - Tap/untap through replace_event
- `crates/engine/src/game/effects/sacrifice.rs` - Sacrifice through replace_event with ZoneChange redirect support
- `crates/engine/src/game/zones.rs` - layers_dirty flag on battlefield zone changes
- `crates/engine/src/game/combat_damage.rs` - Combat damage through replace_event with is_combat: true
- `crates/engine/src/game/turns.rs` - Untap step through replace_event for "doesn't untap" effects
- `crates/engine/src/game/sba.rs` - evaluate_layers before SBA fixpoint loop
- `crates/engine/src/game/engine.rs` - evaluate_layers after SBA+triggers, 5 integration tests
- `crates/engine/src/game/casting.rs` - evaluate_layers before target legality
- `crates/engine/src/game/layers.rs` - Conditional reset guards for base values

## Decisions Made
- Nested replacement in destroy: Destroy handler creates a second ZoneChange proposal after the Destroy event executes, so Moved replacements (like "if would die, exile instead") intercept the actual zone transfer
- Conditional layer reset: evaluate_layers only resets power/toughness when base_power/base_toughness are Some, preventing layer evaluation from wiping objects that don't participate in the layer system
- layers_dirty set automatically on all battlefield zone changes in zones::move_to_zone and on P/T counter modifications

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed layer evaluation wiping non-layer object P/T**
- **Found during:** Task 2 (integration testing)
- **Issue:** evaluate_layers() reset power/toughness to base_power/base_toughness unconditionally, but base values default to None for objects not created through the layer-aware path, causing 9 test failures
- **Fix:** Added guards: only reset P/T when base values are Some, only reset keywords when base_keywords is non-empty
- **Files modified:** crates/engine/src/game/layers.rs
- **Committed in:** 2756215 (Task 2 commit)

**2. [Rule 1 - Bug] Fixed destroy handler not routing zone change through replacement**
- **Found during:** Task 2 (integration test for destruction-to-exile)
- **Issue:** Destroy handler called zones::move_to_zone directly after Execute(Destroy), bypassing Moved replacements that should intercept the zone transfer
- **Fix:** Added nested replacement: after Destroy executes, create a ZoneChange ProposedEvent and run it through replace_event before the actual zone transfer
- **Files modified:** crates/engine/src/game/effects/destroy.rs
- **Committed in:** 2756215 (Task 2 commit)

---

**Total deviations:** 2 auto-fixed (2 bugs)
**Impact on plan:** Both fixes were necessary for correct pipeline behavior. The layer reset guard prevents regression in existing tests. The nested replacement enables the core "if would die, exile instead" pattern.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Replacement + layer + trigger pipeline fully integrated into game flow
- All 377 engine tests pass (372 existing + 5 new integration tests)
- Ready for Phase 07 (UI integration) -- engine provides complete game rules processing
- Phase 06 complete: all 3 plans delivered (replacement pipeline, layer system, integration wiring)

---
*Phase: 06-advanced-rules*
*Completed: 2026-03-08*
