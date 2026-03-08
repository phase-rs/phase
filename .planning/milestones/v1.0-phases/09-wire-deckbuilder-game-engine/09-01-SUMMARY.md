---
phase: 09-wire-deckbuilder-game-engine
plan: 01
subsystem: engine
tags: [rust, wasm, deck-loading, serde, game-state]

# Dependency graph
requires:
  - phase: 02-card-parser
    provides: CardFace struct with parsed card data
  - phase: 03-game-state
    provides: GameState, zones, RNG infrastructure
  - phase: 05-triggers-combat
    provides: TriggerDefinition, StaticDefinition, ReplacementDefinition parsing
provides:
  - DeckPayload/DeckEntry serializable deck transport structs
  - create_object_from_card_face for hydrating GameObjects from CardFace data
  - load_deck_into_state for loading full decks into GameState
  - Updated WASM initialize_game that accepts deck data
  - card-data-export CLI binary for pre-computing card definitions as JSON
affects: [09-02, 09-03, client-game-page, deck-builder-flow]

# Tech tracking
tech-stack:
  added: []
  patterns: [deck-payload-transport, card-face-to-game-object-hydration, shard-color-derivation]

key-files:
  created:
    - crates/engine/src/game/deck_loading.rs
    - crates/engine/src/bin/card_data_export.rs
  modified:
    - crates/engine/src/game/mod.rs
    - crates/engine/Cargo.toml
    - crates/engine-wasm/src/lib.rs

key-decisions:
  - "Derive color from ManaCostShard mapping (not a separate color field) with color_override taking precedence"
  - "Variable P/T like '*' defaults to 0 via parse fallback"
  - "Skip unparseable trigger/static/replacement definitions gracefully rather than failing"
  - "Shuffle libraries by cloning+shuffling+replacing to avoid conflicting mutable borrows on GameState"

patterns-established:
  - "DeckPayload as the serializable transport format between client and WASM engine for deck data"
  - "shard_colors() mapping for deriving ManaColor from ManaCostShard variants"

requirements-completed: [PLAT-03, AI-04]

# Metrics
duration: 5min
completed: 2026-03-08
---

# Phase 9 Plan 1: Deck Loading Summary

**Engine-side deck loading with CardFace-to-GameObject hydration, WASM deck acceptance, and card data export CLI**

## Performance

- **Duration:** 5 min
- **Started:** 2026-03-08T16:23:38Z
- **Completed:** 2026-03-08T16:28:38Z
- **Tasks:** 2
- **Files modified:** 5

## Accomplishments
- Created deck_loading module with create_object_from_card_face populating all 15+ GameObject characteristics from CardFace
- Updated WASM initialize_game to deserialize DeckPayload, load decks, and start game with mulligan flow
- Added card-data-export CLI binary for pre-computing card definitions as JSON
- 14 unit tests covering characteristic population, color derivation, shuffle verification, and serialization roundtrips

## Task Commits

Each task was committed atomically:

1. **Task 1: Create deck_loading module and card_data_export CLI** - `1f689a5` (feat)
2. **Task 2: Update WASM initialize_game to accept and process deck data** - `1e8a180` (feat)

## Files Created/Modified
- `crates/engine/src/game/deck_loading.rs` - DeckPayload/DeckEntry structs, create_object_from_card_face, load_deck_into_state with 14 tests
- `crates/engine/src/bin/card_data_export.rs` - CLI binary exporting CardDatabase as JSON map of card name to CardFace
- `crates/engine/src/game/mod.rs` - Registered deck_loading module and re-exports
- `crates/engine/Cargo.toml` - Added card-data-export binary target
- `crates/engine-wasm/src/lib.rs` - initialize_game now accepts DeckPayload and calls start_game

## Decisions Made
- Derive color from ManaCostShard mapping with comprehensive coverage of all hybrid/phyrexian/colorless variants
- Variable P/T ("*") defaults to 0 via parse fallback, matching Forge convention
- Skip unparseable trigger/static/replacement definitions gracefully (filter_map with .ok())
- Clone libraries for shuffle to avoid conflicting mutable borrows on GameState (rng + players)

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Fixed conflicting mutable borrows in library shuffle**
- **Found during:** Task 1 (deck_loading implementation)
- **Issue:** `state.players` and `state.rng` cannot both be mutably borrowed simultaneously
- **Fix:** Clone libraries into temporary Vec, shuffle each, then replace back into players
- **Files modified:** crates/engine/src/game/deck_loading.rs
- **Verification:** All 14 tests pass including shuffle verification
- **Committed in:** 1f689a5 (Task 1 commit)

**2. [Rule 1 - Bug] Removed unused ActionResult import in WASM**
- **Found during:** Task 2 (WASM update)
- **Issue:** After refactoring initialize_game to use start_game's return value, ActionResult import became unused causing a warning
- **Fix:** Removed unused import
- **Files modified:** crates/engine-wasm/src/lib.rs
- **Verification:** Clean WASM build with no warnings
- **Committed in:** 1e8a180 (Task 2 commit)

---

**Total deviations:** 2 auto-fixed (1 blocking, 1 bug)
**Impact on plan:** Both auto-fixes necessary for correctness. No scope creep.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Deck loading pipeline complete: DeckPayload -> load_deck_into_state -> populated GameObjects in libraries
- WASM initialize_game ready to receive deck data from client
- card-data-export CLI ready for build-time card definition pre-computation
- Ready for 09-02 (client-side deck resolution and game start wiring)

---
*Phase: 09-wire-deckbuilder-game-engine*
*Completed: 2026-03-08*
