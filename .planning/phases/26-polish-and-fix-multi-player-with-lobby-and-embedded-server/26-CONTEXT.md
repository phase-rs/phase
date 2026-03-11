# Phase 26: Polish and Fix Multi-player with Lobby and Embedded Server - Context

**Gathered:** 2026-03-10
**Status:** Ready for planning

<domain>
## Phase Boundary

Fix 5 known multiplayer bugs (A-E: stale session reconnect, missing deck validation, opponent action drops, missing getAiAction, hardcoded PLAYER_ID), add a game lobby with real-time game listing, embed the server as a Tauri sidecar for seamless hosting, add P2P WebRTC hosting for browser/PWA users, and provide server address configuration UX.

</domain>

<decisions>
## Implementation Decisions

### Bug Fixes (A-E)
- Fix Bug A: Clear stale `phase-ws-session` from sessionStorage on new game creation — prevents reconnect to dead sessions
- Fix Bug B: Validate deck selection before navigating to game page for online mode (same guard as WASM path)
- Fix Bug C: Handle `StateUpdate` when no `pendingResolve` exists — update Zustand store directly for opponent actions
- Fix Bug D: Implement `getAiAction()` on WebSocketAdapter as a no-op/error (AI not used in multiplayer)
- Fix Bug E: Use `your_player` from `GameStarted` message instead of hardcoded `PLAYER_ID = 0`

### Lobby Design
- Browseable game list + manual code entry in a single view
- Real-time lobby updates via WebSocket (server pushes game list changes)
- Each game listing shows: host display name, wait time, game code
- Public/private toggle at host time — "List in lobby" checkbox
- Optional password protection for games (public or private can have passwords)
- Password-protected games show lock icon; modal dialog for password entry on join
- Only waiting games shown in lobby (in-progress games removed from list)
- Auto-expire stale games after timeout (server-side cleanup)
- Show online player count in the lobby
- P2P games do NOT appear in lobby — code-only for P2P

### Player Identity
- Player-chosen display name, persisted to localStorage
- Generate a stable UUID as player ID (localStorage) — future-proofs for accounts
- Display name editable in Multiplayer settings section
- Opponent display name shown during gameplay (near life total area)
- No uniqueness enforcement on names — cosmetic only

### Hosting Modes
- **Tauri sidecar** (default for desktop): Ship phase-server as Tauri sidecar binary. Spawned on "Host Game", auto-stops when game ends. Binds to 0.0.0.0 for LAN access.
- **P2P WebRTC** (default for browser/PWA, also available in Tauri): Port Alchemy's WebRTC P2P implementation. Host-authoritative — host's browser runs the engine, opponent receives state over DataChannel. Uses Alchemy's TURN/signaling setup.
- Both modes available on Tauri; sidecar is the default
- Browser/PWA users use P2P only (can also join server-hosted games via code)

### P2P Architecture
- Separate `P2PAdapter` class implementing `EngineAdapter` — not extending WebSocketAdapter
- Host-authoritative model: host runs engine WASM, validates actions, sends filtered state
- Signaling and TURN credentials ported from Alchemy project (../alchemy)
- P2P games are code/invite only — no lobby listing

### Game Settings
- Host setup screen with: display name, hosting mode (Server/P2P), public/private toggle, password, format (Standard/Casual), per-turn timer
- Per-turn chess-clock timer: configurable time per turn (e.g. 30s, 60s, 120s)
- Settings remembered across sessions via localStorage

### Connection UX
- Smart server detection priority: (1) Tauri sidecar localhost, (2) last-used server from settings, (3) manual entry
- `CODE@IP:PORT` syntax for join codes — auto-connects to specified server
- Toast notification on connection failure with Retry / Change Server actions
- Subtle connection status dot (green/yellow/red) visible throughout multiplayer flow
- Dedicated "Multiplayer" section in settings for server address and display name

### Menu Flow
- Revised state machine: `mode-select → deck-gallery-online → lobby → host-setup → waiting`
- Lobby view is the new join flow — browse games + code entry integrated
- Lobby shows current active deck with option to go back to deck gallery to change
- Deck selectable before or from the lobby (link to deck gallery, shows current deck)
- Host setup screen after clicking "Host Game" from lobby

### Waiting Screen
- Show game code prominently, LAN IP:port, "Listed in lobby" label (if public), cancel button
- Animated waiting indicator (pulsing dots or spinner)
- Helpful text: "Share the code with a friend, or wait for someone from the lobby"

### In-Game Features
- Concede button accessible from game menu (escape/hamburger), with confirmation dialog
- Quick emotes only — pre-set messages like "Good game", "Nice play", "Thinking..." (MTGA-style, no free text)
- Emotes require new protocol messages (ClientMessage::Emote, ServerMessage::Emote)

### Game Over
- Simple results screen: winner announcement, total turns, game duration
- "Back to Lobby" button for quick re-match path
- No detailed stats for now

### Claude's Discretion
- Exact emote set and UI placement
- Timer enforcement implementation details (server-side vs client-side)
- Sidecar lifecycle management details (port selection, health check)
- Auto-expire timeout duration for stale lobby games
- P2P signaling relay implementation details
- Connection status dot exact positioning and animation

</decisions>

<specifics>
## Specific Ideas

- "I want to be able to change devices and keep the same player info eventually" — UUID player ID is the forward-thinking step
- Port Alchemy's WebRTC P2P implementation directly — it already works (credentials in ../alchemy)
- Lobby should feel like a game lobby — real-time updates, player count, easy to browse
- Waiting screen shows IP + port + code for LAN sharing
- Join code supports `CODE@IP` syntax for direct server targeting
- MTGA-style emotes instead of free text chat

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `EngineAdapter` interface in `adapter/types.ts` — new `P2PAdapter` follows this pattern
- `WebSocketAdapter` in `adapter/ws-adapter.ts` — reference for protocol handling, will be fixed (Bugs C, D, E)
- `WsAdapterEvent` discriminated union — extend for new events (lobby updates, emotes)
- `menuButtonClass` utility for consistent button styling across menu views
- `DeckGallery` component — reusable for deck selection from lobby
- Server protocol `ClientMessage`/`ServerMessage` enums in `server-core/protocol.rs` — extend for lobby, emotes, settings

### Established Patterns
- Discriminated unions with `#[serde(tag = "type", content = "data")]` for all protocol messages
- `SessionManager` in `server-core/session.rs` — extend for lobby game listing and game settings
- `GameProvider.tsx` orchestrates adapter creation — needs refactoring for P2P path
- `MenuView` type union for menu state machine — extend with lobby, host-setup states
- Zustand stores for state management — may need a lobby/multiplayer store

### Integration Points
- `GameProvider.tsx` — entry point for game initialization, needs P2P adapter path
- `MenuPage.tsx` — menu state machine needs new views (lobby, host-setup, waiting)
- `GamePage.tsx` — needs concede button, emote UI, opponent display name, connection status dot
- `phase-server/main.rs` — extend for lobby protocol (ListGames, LobbyUpdate, game settings)
- `server-core/protocol.rs` — new message types for lobby, emotes, concede, settings
- Tauri config (`tauri.conf.json`) — sidecar binary configuration

</code_context>

<deferred>
## Deferred Ideas

- Cross-device player identity / account system — requires auth infrastructure, separate phase
- Friends list / friend invites — requires persistent accounts, separate phase
- Spectator mode — requires spectator state filter, separate phase
- Rematch button — requires protocol extension, nice-to-have for future
- Detailed game statistics (damage dealt, spells cast, creatures killed) — future enhancement
- Turn timer with time bank (total game timer) — alternative timer model for future

</deferred>

---

*Phase: 26-polish-and-fix-multi-player-with-lobby-and-embedded-server*
*Context gathered: 2026-03-10*
