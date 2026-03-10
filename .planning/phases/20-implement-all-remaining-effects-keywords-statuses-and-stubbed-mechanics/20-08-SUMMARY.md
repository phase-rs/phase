---
phase: 20-implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics
plan: 08
subsystem: engine
tags: [day-night, daybound, nightbound, transform, triggers, mtg-rules]

requires:
  - phase: 20-05
    provides: "Transform module for DFC permanent face-swapping"
  - phase: 20-06
    provides: "Trigger matcher infrastructure for promoting stub triggers"
provides:
  - "DayNight enum (Day/Night) as global game state"
  - "Spell-cast-per-turn counter on GameState"
  - "Day/night transition logic at cleanup step"
  - "Daybound/Nightbound auto-transform on transition"
  - "DayTimeChanges trigger matcher (promoted from unimplemented)"
  - "DayNightChanged game event"
affects: [werewolves, standard-cards, daybound-nightbound]

tech-stack:
  added: []
  patterns: ["Global game state tracking with Option<DayNight> for lazy initialization"]

key-files:
  created:
    - "crates/engine/src/game/day_night.rs"
  modified:
    - "crates/engine/src/types/game_state.rs"
    - "crates/engine/src/types/events.rs"
    - "crates/engine/src/game/casting.rs"
    - "crates/engine/src/game/turns.rs"
    - "crates/engine/src/game/triggers.rs"
    - "crates/engine/src/game/mod.rs"

key-decisions:
  - "Day/night transition checked at cleanup step per MTG Rule 727.2"
  - "spells_cast_this_turn uses saturating_add for safety"
  - "initialize_day_night sets to Day (not Night) per Rule 727.1"

patterns-established:
  - "Global state tracking: Option<T> for states that are lazily activated"

requirements-completed: [ENG-16]

duration: 5min
completed: 2026-03-10
---

# Phase 20 Plan 08: Day/Night Global State Summary

**Day/night global state with Daybound/Nightbound auto-transform using cleanup-step transition checks and spell-cast counting**

## Performance

- **Duration:** 5min
- **Started:** 2026-03-10T00:30:37Z
- **Completed:** 2026-03-10T00:36:18Z
- **Tasks:** 2
- **Files modified:** 7

## Accomplishments
- DayNight enum and global state tracking on GameState (starts None, initialized on first daybound card)
- Spell-cast counter incremented on cast, reset at turn start
- Day->Night on 0 spells, Night->Day on 2+ spells with automatic Daybound/Nightbound transformation
- DayTimeChanges trigger promoted from unimplemented to real matcher

## Task Commits

Each task was committed atomically:

1. **Task 1: Day/Night global state enum and spell-cast tracking** - `0c0ecff` (feat)
2. **Task 2: Day/Night transition logic and Daybound/Nightbound auto-transform** - `f7a5b9f` (feat)

## Files Created/Modified
- `crates/engine/src/game/day_night.rs` - Day/night transition logic and initialization
- `crates/engine/src/types/game_state.rs` - DayNight enum, day_night and spells_cast_this_turn fields
- `crates/engine/src/types/events.rs` - DayNightChanged event variant
- `crates/engine/src/game/casting.rs` - Increment spells_cast_this_turn on spell cast
- `crates/engine/src/game/turns.rs` - Reset spell counter at turn start, call day/night check at cleanup
- `crates/engine/src/game/triggers.rs` - match_day_time_changes matcher for DayTimeChanges trigger
- `crates/engine/src/game/mod.rs` - Register day_night module

## Decisions Made
- Day/night transition checked at cleanup step per MTG Rule 727.2
- spells_cast_this_turn uses saturating_add for overflow safety
- initialize_day_night sets state to Day (not Night) per Rule 727.1
- Nightbound detection for Day transition uses obj.transformed flag (nightbound creatures are always on the back face)

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed clippy map_or warning**
- **Found during:** Task 2
- **Issue:** clippy flagged map_or(false, ...) as should be is_some_and
- **Fix:** Changed map_or to is_some_and
- **Files modified:** crates/engine/src/game/day_night.rs
- **Verification:** cargo clippy --all-targets -- -D warnings clean
- **Committed in:** f7a5b9f (part of Task 2 commit)

---

**Total deviations:** 1 auto-fixed (1 bug)
**Impact on plan:** Minor style fix required by clippy. No scope creep.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Day/night infrastructure ready for werewolf and similar Standard cards
- initialize_day_night should be called when a daybound/nightbound card enters the battlefield (can be wired in zone change handling)

---
*Phase: 20-implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics*
*Completed: 2026-03-10*
