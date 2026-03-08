---
phase: 09-wire-deckbuilder-game-engine
plan: 03
subsystem: ui
tags: [react, localStorage, wasm, deck-builder, game-launch]

requires:
  - phase: 09-wire-deckbuilder-game-engine
    provides: "Storage constants, starter decks, deck parser (plans 01-02)"
provides:
  - "End-to-end deck selection -> game launch flow"
  - "MenuPage deck tile selector with color identity display"
  - "GamePage deck data resolution and WASM payload construction"
  - "Paste import modal for MTGA and .dck deck formats"
affects: [09-04, 09-05]

tech-stack:
  added: []
  patterns:
    - "DeckPayload built from card-data.json for WASM initialization"
    - "Active deck persisted via localStorage ACTIVE_DECK_KEY"
    - "Starter deck auto-seeding on first launch"

key-files:
  created: []
  modified:
    - client/src/adapter/types.ts
    - client/src/adapter/wasm-adapter.ts
    - client/src/adapter/ws-adapter.ts
    - client/src/adapter/tauri-adapter.ts
    - client/src/stores/gameStore.ts
    - client/src/pages/GamePage.tsx
    - client/src/pages/MenuPage.tsx
    - client/src/components/deck-builder/DeckBuilder.tsx
    - client/src/components/deck-builder/DeckList.tsx

key-decisions:
  - "Mirror match for opponent deck (same as player) as simplest viable approach"
  - "card-data.json fetch with graceful 404 fallback to null deck payload"
  - "Import button opens paste modal with From File option inside modal"

patterns-established:
  - "initializeGame as separate adapter method from initialize for deck data passing"
  - "buildDeckPayload async helper for card-data.json resolution"

requirements-completed: [DECK-01, DECK-03, AI-04, PLAT-03]

duration: 4min
completed: 2026-03-08
---

# Phase 9 Plan 3: Wire DeckBuilder-GamePage-WASM Pipeline Summary

**Full deck selection -> game launch pipeline: MenuPage deck tiles with color identity, localStorage persistence, card-data.json resolution to DeckPayload, and paste import modal**

## Performance

- **Duration:** 4 min
- **Started:** 2026-03-08T16:29:51Z
- **Completed:** 2026-03-08T16:33:57Z
- **Tasks:** 2
- **Files modified:** 9

## Accomplishments
- Wired EngineAdapter.initializeGame through gameStore to pass deck data to WASM engine
- MenuPage deck tile selector with WUBRG color identity dots, card count, and localStorage persistence
- GamePage loads active deck, fetches card-data.json, builds DeckPayload with mirror match opponent
- Starter decks auto-seeded on first launch, game buttons disabled without active deck
- DeckBuilder cleaned up: no Start Game button, shared storage constants, paste import modal

## Task Commits

Each task was committed atomically:

1. **Task 1: Wire adapter/store chain and add deck data resolution** - `167a992` (feat)
2. **Task 2: MenuPage deck selector and DeckBuilder cleanup** - `5106baa` (feat)

## Files Created/Modified
- `client/src/adapter/types.ts` - Added initializeGame to EngineAdapter interface
- `client/src/adapter/wasm-adapter.ts` - No changes needed (already had initializeGame)
- `client/src/adapter/ws-adapter.ts` - Added initializeGame no-op (server handles decks)
- `client/src/adapter/tauri-adapter.ts` - Added initializeGame via Tauri IPC invoke
- `client/src/stores/gameStore.ts` - initGame calls adapter.initializeGame with deckData
- `client/src/stores/__tests__/gameStore.test.ts` - Added initializeGame to mock adapter
- `client/src/pages/GamePage.tsx` - Deck loading from localStorage, card-data.json resolution, redirect guard
- `client/src/pages/MenuPage.tsx` - Deck tile selector, starter deck seeding, disabled buttons
- `client/src/components/deck-builder/DeckBuilder.tsx` - Removed Start Game, use shared constants
- `client/src/components/deck-builder/DeckList.tsx` - Paste import modal with MTGA/.dck auto-detection

## Decisions Made
- Mirror match: opponent uses same deck as player (simplest viable, refinable later)
- card-data.json fetched at game start; 404 gracefully falls back to null (empty game)
- Import button opens paste modal rather than just file picker; file import available inside modal
- Cancelled async operations tracked via `cancelled` flag to prevent state updates after unmount

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Added initializeGame to WsAdapter and TauriAdapter**
- **Found during:** Task 1
- **Issue:** Adding initializeGame to EngineAdapter interface broke WsAdapter and TauriAdapter which implement it
- **Fix:** Added no-op initializeGame to WsAdapter (server handles decks), functional initializeGame to TauriAdapter
- **Files modified:** client/src/adapter/ws-adapter.ts, client/src/adapter/tauri-adapter.ts
- **Verification:** TypeScript compiles without errors
- **Committed in:** 167a992

**2. [Rule 1 - Bug] Updated test mock adapter with initializeGame**
- **Found during:** Task 1
- **Issue:** gameStore tests used mock adapter missing new initializeGame method
- **Fix:** Added initializeGame mock to createMockAdapter in test file
- **Files modified:** client/src/stores/__tests__/gameStore.test.ts
- **Verification:** All 48 tests pass
- **Committed in:** 167a992

---

**Total deviations:** 2 auto-fixed (1 blocking, 1 bug)
**Impact on plan:** Both necessary for interface compliance. No scope creep.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Full deck -> game pipeline wired and ready for end-to-end testing
- card-data.json must be generated via card_data_export CLI for populated game libraries
- Remaining plans (04-05) can build on this integration

---
*Phase: 09-wire-deckbuilder-game-engine*
*Completed: 2026-03-08*
