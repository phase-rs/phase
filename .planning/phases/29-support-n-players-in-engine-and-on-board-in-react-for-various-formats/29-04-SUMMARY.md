---
phase: 29-support-n-players
plan: 04
subsystem: engine
tags: [commander, command-zone, commander-tax, color-identity, mtg-rules]

# Dependency graph
requires:
  - phase: 29-01
    provides: "N-player foundation types (PlayerId, FormatConfig, Zone::Command)"
provides:
  - "Commander module with tax calculation, zone redirection, color identity validation"
  - "Commander casting from command zone with tax applied"
  - "Zone redirection for commanders (graveyard/exile -> command zone)"
  - "create_commander_from_card_face() for deck loading"
  - "is_commander flag on GameObject"
  - "commander_cast_count tracking on GameState"
affects: [29-05, 29-06, 29-07]

# Tech tracking
tech-stack:
  added: []
  patterns: ["Commander zone redirection in move_to_zone", "Commander tax via cost mutation in casting.rs"]

key-files:
  created:
    - "crates/engine/src/game/commander.rs"
  modified:
    - "crates/engine/src/game/mod.rs"
    - "crates/engine/src/game/game_object.rs"
    - "crates/engine/src/game/casting.rs"
    - "crates/engine/src/game/zones.rs"
    - "crates/engine/src/game/deck_loading.rs"
    - "crates/engine/src/types/game_state.rs"

key-decisions:
  - "is_commander flag on GameObject rather than separate tracking structure -- simple, direct, serializable"
  - "commander_cast_count as HashMap<ObjectId, u32> on GameState for per-commander tax tracking"
  - "Zone redirection in move_to_zone() intercepts at the lowest level to catch all zone changes"
  - "Commander tax mutates mana cost in-place (adds to generic) rather than separate payment step"
  - "validate_commander_deck uses basic land name list for singleton exemption"

patterns-established:
  - "Commander rules module pattern: standalone functions operating on GameState"
  - "Zone redirection pattern: intercept in move_to_zone before actual zone change"

requirements-completed: [NP-COMMANDER, NP-CMDZONE, NP-CMDTAX]

# Metrics
duration: 13min
completed: 2026-03-11
---

# Phase 29 Plan 04: Commander Rules Summary

**Commander module with command zone management, per-commander tax tracking, zone redirection for graveyard/exile, color identity enforcement, and deck validation**

## Performance

- **Duration:** 13 min
- **Started:** 2026-03-11T17:58:30Z
- **Completed:** 2026-03-11T18:11:30Z
- **Tasks:** 2
- **Files modified:** 8

## Accomplishments
- Commander module (commander.rs) with tax calculation, zone redirection, color identity, and deck validation
- Casting system extended to allow casting from command zone with automatic commander tax
- Zone system intercepts graveyard/exile moves for commanders and redirects to command zone
- Deck loading supports creating commanders directly in command zone via create_commander_from_card_face()
- 25 commander-specific tests including 4 integration tests for full lifecycle

## Task Commits

Each task was committed atomically:

1. **Task 1: Commander module with tax, zone redirection, and color identity** - `dd86a6c94` (feat)
2. **Task 2: Integrate commander rules into casting and zone management** - `d6bba6435` (chore - formatting; substantive changes merged into concurrent Plan 03 commit `38949b375`)

## Files Created/Modified
- `crates/engine/src/game/commander.rs` - Commander rules: tax, redirection, color identity, deck validation
- `crates/engine/src/game/mod.rs` - Register commander module, export create_commander_from_card_face
- `crates/engine/src/game/game_object.rs` - Added is_commander flag
- `crates/engine/src/types/game_state.rs` - Added commander_cast_count HashMap
- `crates/engine/src/game/casting.rs` - Command zone casting, commander tax, color identity check
- `crates/engine/src/game/zones.rs` - Zone redirection for commanders
- `crates/engine/src/game/deck_loading.rs` - create_commander_from_card_face()

## Decisions Made
- is_commander flag on GameObject (not separate tracking) -- simple, serializable, direct field check
- commander_cast_count as HashMap<ObjectId, u32> on GameState -- per-commander tracking supports partners
- Zone redirection in move_to_zone() at lowest level -- catches all zone changes including SBA-driven ones
- Commander tax adds to generic cost -- cleanest integration with existing mana payment system
- Basic land names hardcoded for singleton exemption in deck validation -- MTG has exactly 5 basic land names

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Added is_commander flag to GameObject**
- **Found during:** Task 1
- **Issue:** Plan references is_commander "added in Plan 03" but Plan 03 hadn't created it yet
- **Fix:** Added `is_commander: bool` field with `#[serde(default)]` to GameObject
- **Files modified:** crates/engine/src/game/game_object.rs
- **Committed in:** dd86a6c94

**2. [Rule 3 - Blocking] Fixed pre-existing DeclareAttackers compilation errors**
- **Found during:** Task 1
- **Issue:** Plan 29-01/03 changed DeclareAttackers from `attacker_ids` to `attacks: Vec<(ObjectId, AttackTarget)>` but some tests weren't updated
- **Fix:** Updated combat.rs tests, keywords.rs integration tests, and game_state.rs test to use new signature
- **Files modified:** crates/engine/src/game/combat.rs, crates/engine/tests/rules/keywords.rs, crates/engine/src/types/game_state.rs
- **Committed in:** dd86a6c94

---

**Total deviations:** 2 auto-fixed (2 blocking)
**Impact on plan:** Both fixes necessary to compile and run tests. No scope creep.

## Issues Encountered
- Concurrent Plan 02/03 executors committed between Task 1 and Task 2, incorporating Task 2's changes into their commits. Task 2 commit contains only formatting adjustments as a result.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Commander rules fully functional and integrated
- Ready for commander damage tracking (Plan 05+)
- Zone redirection, tax, and color identity all tested and working

---
*Phase: 29-support-n-players*
*Completed: 2026-03-11*
