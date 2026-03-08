# Phase 12: Multiplayer Wiring & Final Cleanup - Context

**Gathered:** 2026-03-08
**Status:** Ready for planning

<domain>
## Phase Boundary

Wire the remaining multiplayer integration gaps so host→join→play and disconnect→reconnect flows work end-to-end. Clean up orphaned endpoint and close remaining tech debt. Two success criteria are already complete (PermanentCard constant, TD requirements traceability).

</domain>

<decisions>
## Implementation Decisions

### Server deck resolution
- Load `CardDatabase` at server startup in `main()`, pass as shared state (`Arc<CardDatabase>`)
- Card files path via `FORGE_CARDS_DIR` environment variable; error on startup if missing
- Reject game creation/join if any card name in `DeckData.main_deck` can't be resolved via `CardDatabase`
- Deck-to-GameObjects conversion function lives in the `engine` crate (not server-core) so both WASM and server paths can reuse it

### Client reconnect trigger
- Reconnect on both WebSocket `onclose` event and page reload (check `sessionStorage` on mount)
- 3 retries with exponential backoff (1s, 2s, 4s)
- Non-blocking overlay banner at top of GamePage: "Reconnecting... (attempt N/3)" — game board visible but interactions disabled
- After all retries fail: banner changes to "Connection lost" with a "Retry" button and a "Return to Menu" link

### Orphaned /games endpoint
- Remove the `/games` HTTP route from `forge-server/src/main.rs`
- Keep `open_games()` method on `SessionManager` — lobby UI is planned soon
- Remove the `list_games` handler function

### Claude's Discretion
- Exact retry timing constants (1s/2s/4s suggested but flexible)
- Banner styling and animation
- Error message wording for deck resolution failures
- Whether to log unresolvable card names before rejecting

</decisions>

<code_context>
## Existing Code Insights

### Reusable Assets
- `CardDatabase::load(path)`: Loads and indexes Forge card files by name — already used by `card_data_export` and `coverage_report` binaries
- `WsAdapter.tryReconnect()`: Full reconnect implementation exists (reads sessionStorage, opens new WebSocket, sends Reconnect message) — just needs to be called
- `SessionManager.handle_reconnect()`: Server-side reconnect with grace period tracking already implemented
- `ReconnectManager`: Tracks disconnects with configurable grace period, checks expiry

### Established Patterns
- Shared state via `Arc<Mutex<T>>` in forge-server (used for `SessionManager`)
- `FORGE_CARDS_DIR` follows existing env var pattern (server already reads `PORT` env var)
- Engine crate's `start_game()` initializes GameState — deck loading extends this path
- WsAdapter event emitter pattern (`emit()` + `onEvent()`) for UI state decoupling

### Integration Points
- `server-core/session.rs`: `join_game()` and `create_game()` need CardDatabase access for deck resolution
- `forge-server/main.rs`: Startup needs CardDatabase loading, shared state plumbing
- `client/src/pages/GamePage.tsx`: Needs reconnect trigger on mount + WsAdapter onclose handling
- `client/src/adapter/ws-adapter.ts`: Needs retry logic wrapping `tryReconnect()`

</code_context>

<specifics>
## Specific Ideas

- User wants lobby UI soon — keep `open_games()` on SessionManager as foundation for that
- Reconnect pattern should match idiomatic WebSocket game clients (Lichess-style: auto-retry on close + session restore on reload)

</specifics>

<deferred>
## Deferred Ideas

- Lobby UI with game list and join buttons — future phase (user confirmed this is planned)

</deferred>

---

*Phase: 12-multiplayer-wiring-final-cleanup*
*Context gathered: 2026-03-08*
