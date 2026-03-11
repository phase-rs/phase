---
phase: 29-support-n-players
plan: 01
subsystem: engine
tags: [rust, n-player, format, game-state, player-iteration]

requires:
  - phase: 28-native-ability-data-model
    provides: typed ability definitions and GameState structure
provides:
  - GameFormat enum and FormatConfig struct with 4 format factory methods
  - GameState::new(config, player_count, seed) N-player constructor
  - Player iteration functions (next_player, opponents, apnap_order, teammates, is_alive)
  - seat_order, eliminated_players, commander_damage, priority_passes on GameState
  - is_eliminated on Player
affects: [29-02, 29-03, 29-04, 29-05, 29-06, 29-07, 29-08, 29-09, 29-10, 29-11, 29-12, 29-13, 29-14, 29-15, 29-16]

tech-stack:
  added: []
  patterns: [format-config-factory, seat-order-iteration, elimination-tracking]

key-files:
  created:
    - crates/engine/src/types/format.rs
    - crates/engine/src/game/players.rs
  modified:
    - crates/engine/src/types/game_state.rs
    - crates/engine/src/types/player.rs
    - crates/engine/src/types/mod.rs
    - crates/engine/src/game/mod.rs

key-decisions:
  - "CommanderDamageEntry struct instead of HashMap<(PlayerId, ObjectId), u32> for serde_json compatibility (tuple keys don't serialize as JSON object keys)"
  - "PartialOrd + Ord derived on PlayerId for BTreeSet<PlayerId> support"
  - "priority_pass_count kept alongside new priority_passes BTreeSet for backward compat (removed in Plan 02)"

patterns-established:
  - "FormatConfig factory pattern: FormatConfig::standard(), ::commander(), etc."
  - "Player iteration via seat_order + is_eliminated filtering"
  - "GameState::new(config, player_count, seed) as primary constructor; new_two_player delegates to it"

requirements-completed: [NP-FORMAT, NP-SEAT, NP-ITER]

duration: 7min
completed: 2026-03-11
---

# Phase 29 Plan 01: N-Player Foundation Types Summary

**FormatConfig with 4 format factories, N-player GameState constructor, and seat-order-based player iteration (next_player, opponents, apnap_order, teammates)**

## Performance

- **Duration:** 7 min
- **Started:** 2026-03-11T17:47:58Z
- **Completed:** 2026-03-11T17:55:00Z
- **Tasks:** 2
- **Files modified:** 6

## Accomplishments
- FormatConfig with Standard/Commander/FreeForAll/TwoHeadedGiant format factories defining starting life, player counts, deck rules, and team-based play
- GameState extended with seat_order, format_config, eliminated_players, commander_damage, and priority_passes for N-player tracking
- Player iteration functions (next_player, opponents, apnap_order, teammates, is_alive) that respect seat order and skip eliminated players
- Full backward compatibility: new_two_player delegates to new(FormatConfig::standard(), 2, seed), all 710 existing tests pass

## Task Commits

Each task was committed atomically:

1. **Task 1: Define FormatConfig types and GameState/Player extensions** - `ad464c701` (feat)
2. **Task 2: Create player iteration functions** - `15c054f88` (feat)

## Files Created/Modified
- `crates/engine/src/types/format.rs` - GameFormat enum and FormatConfig struct with 4 factory methods
- `crates/engine/src/game/players.rs` - next_player, opponents, apnap_order, teammates, is_alive functions with 21 tests
- `crates/engine/src/types/game_state.rs` - Extended with seat_order, format_config, eliminated_players, commander_damage, priority_passes; added new() constructor
- `crates/engine/src/types/player.rs` - Added is_eliminated field and Ord derive on PlayerId
- `crates/engine/src/types/mod.rs` - Added format module and exports
- `crates/engine/src/game/mod.rs` - Added players module

## Decisions Made
- Used CommanderDamageEntry struct (player, commander, damage) instead of HashMap<(PlayerId, ObjectId), u32> because serde_json cannot serialize tuple keys as JSON object keys
- Derived PartialOrd + Ord on PlayerId to support BTreeSet<PlayerId> for priority_passes
- Kept priority_pass_count alongside new priority_passes field for backward compatibility; migration deferred to Plan 02

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Changed commander_damage from HashMap<(PlayerId, ObjectId), u32> to Vec<CommanderDamageEntry>**
- **Found during:** Task 1 (compilation)
- **Issue:** serde_json cannot serialize HashMap with tuple keys as JSON object keys
- **Fix:** Created CommanderDamageEntry struct with player, commander, damage fields; used Vec instead of HashMap
- **Files modified:** crates/engine/src/types/game_state.rs
- **Verification:** Serde roundtrip test passes
- **Committed in:** ad464c701 (Task 1 commit)

---

**Total deviations:** 1 auto-fixed (1 blocking)
**Impact on plan:** Necessary for serde compatibility. Same data, different container. No scope creep.

## Issues Encountered
- Pre-existing clippy type_complexity warning in coverage module (out of scope, not related to our changes)

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- FormatConfig, seat_order, and player iteration functions are ready for all subsequent Plan 29 work
- Plan 02 can now migrate priority.rs and turns.rs to use priority_passes and next_player
- No blockers

---
*Phase: 29-support-n-players*
*Completed: 2026-03-11*
