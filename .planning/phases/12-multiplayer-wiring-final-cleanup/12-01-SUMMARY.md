---
phase: 12-multiplayer-wiring-final-cleanup
plan: 01
subsystem: api
tags: [rust, axum, websocket, multiplayer, deck-loading, card-database]

requires:
  - phase: 08-server
    provides: "SessionManager, forge-server WebSocket handlers, DeckData protocol"
  - phase: 09-card-data
    provides: "CardDatabase, CardFace, DeckEntry, load_deck_into_state"
provides:
  - "resolve_deck bridge function converting DeckData name strings to Vec<DeckEntry>"
  - "Server-side CardDatabase loading at startup via FORGE_CARDS_DIR"
  - "Deck validation in create/join handlers rejecting unresolvable card names"
  - "Multiplayer games initialized with real card objects in libraries"
affects: [multiplayer, lobby-ui]

tech-stack:
  added: [tempfile (dev)]
  patterns: [Arc<CardDatabase> shared read-only state, resolve-before-session pattern]

key-files:
  created:
    - crates/server-core/src/deck_resolve.rs
  modified:
    - crates/server-core/src/lib.rs
    - crates/server-core/src/session.rs
    - crates/server-core/Cargo.toml
    - crates/forge-server/src/main.rs

key-decisions:
  - "CardDatabase wrapped in Arc (no Mutex) since it is read-only after startup"
  - "resolve_deck lives in server-core (not forge-server) for testability with CardDatabase fixtures"
  - "SessionManager accepts Vec<DeckEntry> instead of DeckData -- resolution happens before session layer"

patterns-established:
  - "Resolve-before-session: deck name resolution occurs in handler before passing to SessionManager"

requirements-completed: [MP-01]

duration: 3min
completed: 2026-03-08
---

# Phase 12 Plan 01: Server-Side Deck Resolution Summary

**Server resolves DeckData card name strings to CardFace objects via CardDatabase before multiplayer game initialization, closing the empty-library gap**

## Performance

- **Duration:** 3 min
- **Started:** 2026-03-08T20:06:14Z
- **Completed:** 2026-03-08T20:09:42Z
- **Tasks:** 2
- **Files modified:** 5

## Accomplishments
- Created resolve_deck function that maps DeckData name strings to Vec<DeckEntry> with duplicate aggregation
- Wired CardDatabase loading at server startup from FORGE_CARDS_DIR env var
- Integrated deck resolution into CreateGame/JoinGame WebSocket handlers with error feedback
- Changed SessionManager to accept resolved Vec<DeckEntry> and call load_deck_into_state before start_game
- Removed orphaned /games HTTP endpoint and list_games handler

## Task Commits

Each task was committed atomically:

1. **Task 1: Create resolve_deck function and wire CardDatabase into server** - `2dc6067` (feat)
2. **Task 2: Wire deck resolution into forge-server handlers and remove /games endpoint** - `9e4022d` (feat)

## Files Created/Modified
- `crates/server-core/src/deck_resolve.rs` - resolve_deck function with tests (4 test cases)
- `crates/server-core/src/lib.rs` - Added deck_resolve module and re-export
- `crates/server-core/src/session.rs` - Changed create_game/join_game signatures to Vec<DeckEntry>, added load_deck_into_state call
- `crates/server-core/Cargo.toml` - Added tempfile dev-dependency
- `crates/forge-server/src/main.rs` - CardDatabase loading, SharedDb type, resolve_deck in handlers, removed /games

## Decisions Made
- CardDatabase wrapped in Arc (no Mutex) since it is read-only after startup
- resolve_deck lives in server-core (not forge-server) for testability with CardDatabase tempfile fixtures
- SessionManager accepts Vec<DeckEntry> instead of DeckData -- resolution boundary sits in the handler layer

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

None.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- Server now resolves decks before game init, multiplayer games will have populated libraries
- FORGE_CARDS_DIR env var required at server runtime
- open_games() retained on SessionManager for future lobby UI

---
*Phase: 12-multiplayer-wiring-final-cleanup*
*Completed: 2026-03-08*
