---
phase: 03-game-state-engine
plan: 01
subsystem: engine
tags: [rust, game-state, zones, mana, rng, chacha, serde]

requires:
  - phase: 01-project-scaffold
    provides: engine crate structure, type scaffolding (GameState, Phase, Zone, Player, ManaPool, identifiers)
provides:
  - GameObject struct with ~20 rules-relevant fields
  - Expanded GameState with central object store, zone collections, seeded RNG, WaitingFor
  - Expanded Player with per-player zone collections (library, hand, graveyard)
  - ManaPool with tracked ManaUnit approach (source, restrictions, snow)
  - Zone transfer operations (create_object, move_to_zone, move_to_library_position)
  - WaitingFor, ActionResult, StackEntry, StackEntryKind types
affects: [03-02, 03-03, 04-ability-system, 05-triggers-combat]

tech-stack:
  added: [rand 0.9, rand_chacha 0.9]
  patterns: [central-object-store, zone-id-references, mana-unit-tracking, serde-skip-rng]

key-files:
  created:
    - crates/engine/src/game/game_object.rs
    - crates/engine/src/game/zones.rs
    - crates/engine/src/game/mod.rs
  modified:
    - crates/engine/src/types/game_state.rs
    - crates/engine/src/types/player.rs
    - crates/engine/src/types/mana.rs
    - crates/engine/src/types/mod.rs
    - crates/engine/src/lib.rs
    - crates/engine/Cargo.toml
    - crates/engine-wasm/src/lib.rs

key-decisions:
  - "ChaCha20Rng for cross-platform deterministic seeded RNG (not StdRng)"
  - "HashMap<ObjectId, GameObject> central object store with zones as Vec<ObjectId>"
  - "ManaPool as Vec<ManaUnit> with source tracking and restrictions (not counter fields)"
  - "serde(skip) on RNG field with seed-based reconstruction on deserialization"
  - "Custom PartialEq on GameState excluding RNG (compared via seed)"

patterns-established:
  - "Central object store: all game objects in HashMap, zones hold ObjectId references"
  - "Zone transfer pattern: remove from source, add to destination, update object.zone, emit event"
  - "ManaUnit tracking: individual mana units with source_id, snow flag, restrictions"

requirements-completed: [ENG-04, ENG-05]

duration: 4min
completed: 2026-03-07
---

# Phase 03 Plan 01: Foundation Types & Zone Management Summary

**GameObject struct, expanded GameState with central object store and seeded ChaCha20Rng, ManaPool restructured to tracked ManaUnit approach, and zone transfer operations with event generation**

## Performance

- **Duration:** 4 min
- **Started:** 2026-03-07T22:38:29Z
- **Completed:** 2026-03-07T22:43:10Z
- **Tasks:** 2
- **Files modified:** 10

## Accomplishments
- GameObject struct with all rules-relevant fields (~20 fields covering tapped, counters, power/toughness, attachments, keywords, abilities)
- GameState expanded with central object store (HashMap<ObjectId, GameObject>), shared zone collections, seeded ChaCha20Rng, WaitingFor enum, StackEntry
- ManaPool restructured from simple counter fields to Vec<ManaUnit> with source tracking, snow flag, and mana restrictions
- Zone transfer operations (create_object, move_to_zone, move_to_library_position) with ZoneChanged event generation
- 125 total engine tests passing

## Task Commits

Each task was committed atomically:

1. **Task 1: Foundation types -- GameObject, expanded GameState/Player, ManaPool restructure** - `0d53123` (feat)
2. **Task 2: Zone transfer operations** - `dbd4051` (feat)

## Files Created/Modified
- `crates/engine/src/game/game_object.rs` - GameObject struct with CounterType enum
- `crates/engine/src/game/zones.rs` - Zone transfer operations (create_object, move_to_zone, etc.)
- `crates/engine/src/game/mod.rs` - Game module re-exports
- `crates/engine/src/types/game_state.rs` - Expanded GameState with objects, zones, RNG, WaitingFor, StackEntry
- `crates/engine/src/types/player.rs` - Expanded Player with per-player zones
- `crates/engine/src/types/mana.rs` - Restructured ManaPool with ManaUnit, ManaType, ManaRestriction
- `crates/engine/src/types/mod.rs` - Updated re-exports for new types
- `crates/engine/src/lib.rs` - Added game module
- `crates/engine/Cargo.toml` - Added rand, rand_chacha dependencies
- `crates/engine-wasm/src/lib.rs` - Updated for new ManaPool/ManaType imports

## Decisions Made
- Used ChaCha20Rng instead of StdRng for guaranteed cross-platform deterministic seeding
- Custom PartialEq on GameState that compares by seed rather than RNG internal state
- serde(skip) on RNG field with default_rng() function for deserialization reconstruction
- Stack zone managed separately via StackEntry (not tracked in add_to_zone/remove_from_zone for ObjectIds)

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] ChaCha20Rng doesn't implement Default for serde skip**
- **Found during:** Task 1 (GameState expansion)
- **Issue:** `#[serde(skip)]` requires Default trait, but ChaCha20Rng doesn't implement it
- **Fix:** Added `#[serde(skip, default = "default_rng")]` with a helper function that creates a zeroed RNG
- **Files modified:** crates/engine/src/types/game_state.rs
- **Verification:** Serialization roundtrip test passes
- **Committed in:** 0d53123 (Task 1 commit)

---

**Total deviations:** 1 auto-fixed (1 blocking)
**Impact on plan:** Necessary for serde compatibility. No scope creep.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Foundation types ready for engine logic (plans 02 and 03)
- GameObject, GameState, Player, ManaPool all expanded and tested
- Zone transfer operations ready for turn/priority engine to use
- WaitingFor and ActionResult types ready for action-response pattern

---
*Phase: 03-game-state-engine*
*Completed: 2026-03-07*
