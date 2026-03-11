---
phase: 26-polish-and-fix-multi-player-with-lobby-and-embedded-server
plan: 04
subsystem: networking
tags: [peerjs, webrtc, p2p, multiplayer, typescript, react]

# Dependency graph
requires:
  - phase: 26-01
    provides: "multiplayerStore, ws-adapter bug fixes, dynamic player ID"
provides:
  - PeerJS-based P2P networking layer (connection.ts, peer.ts, protocol.ts)
  - P2PHostAdapter wrapping WasmAdapter for browser-based game hosting
  - P2PGuestAdapter receiving filtered state from host
  - GameProvider p2p-host and p2p-join mode support
  - P2P host button in lobby UI
  - Smart code routing (5-char P2P vs server codes)
affects: [26-05, 26-06]

# Tech tracking
tech-stack:
  added: [peerjs@1.5.5]
  patterns: [P2P adapter wrapping WasmAdapter, filterStateForGuest for zone privacy, PeerSession message buffering]

key-files:
  created:
    - client/src/network/protocol.ts
    - client/src/network/peer.ts
    - client/src/network/connection.ts
    - client/src/network/__tests__/peer.test.ts
    - client/src/adapter/p2p-adapter.ts
  modified:
    - client/src/adapter/index.ts
    - client/src/providers/GameProvider.tsx
    - client/src/pages/GamePage.tsx
    - client/src/pages/MenuPage.tsx
    - client/src/components/lobby/LobbyView.tsx
    - client/package.json

key-decisions:
  - "P2P host is player 0, guest is player 1 (matching WasmAdapter/server convention)"
  - "filterStateForGuest uses JSON round-trip clone to hide host hand/library"
  - "5-char P2P codes auto-detected via parseRoomCode for smart join routing"
  - "P2P games skip lobby listing (code-only, not public)"
  - "Guest sends deck payload as-is; host combines into WASM deckPayload"

patterns-established:
  - "P2PHostAdapter pattern: wraps WasmAdapter + PeerSession for browser hosting"
  - "P2PGuestAdapter pattern: pending resolve for action/response cycle (mirrors WsAdapter)"
  - "P2PAdapterEvent type: mirrors WsAdapterEvent for consistent UI event handling"

requirements-completed: [MP-P2P, MP-P2P-HOST, MP-P2P-GUEST]

# Metrics
duration: 7min
completed: 2026-03-11
---

# Phase 26 Plan 04: P2P Networking Summary

**PeerJS-based WebRTC P2P networking with host/guest adapters wrapping WasmAdapter for serverless browser multiplayer**

## Performance

- **Duration:** 7 min
- **Started:** 2026-03-11T01:22:56Z
- **Completed:** 2026-03-11T01:30:41Z
- **Tasks:** 3
- **Files modified:** 12

## Accomplishments
- Ported PeerJS networking layer from Alchemy (connection, peer session, protocol) with phase-rs message types
- P2PHostAdapter runs WASM engine locally and sends filtered state (hidden hand/library) to guest via WebRTC DataChannel
- P2PGuestAdapter receives state updates and can submit actions through PeerSession
- GameProvider, GamePage, and MenuPage wired for p2p-host and p2p-join URL modes
- 7 passing tests for protocol validation and PeerSession behavior

## Task Commits

Each task was committed atomically:

1. **Task 0: Create test scaffolds** - `1cc3b2e0` (test)
2. **Task 1: Port Alchemy network layer and install PeerJS** - `0e9bbacd` (feat)
3. **Task 2: Implement P2P adapters and wire into GameProvider** - `c232cbc6` (feat)

## Files Created/Modified
- `client/src/network/protocol.ts` - P2P message types (guest_deck, game_setup, action, state_update, etc.) with validateMessage
- `client/src/network/peer.ts` - PeerSession with keep-alive, disconnect handling, message buffering
- `client/src/network/connection.ts` - hostRoom/joinRoom with metered.ca TURN servers (prefix: phase-)
- `client/src/network/__tests__/peer.test.ts` - 7 tests for protocol validation and PeerSession behavior
- `client/src/adapter/p2p-adapter.ts` - P2PHostAdapter (wraps WasmAdapter) and P2PGuestAdapter (receives state)
- `client/src/adapter/index.ts` - Re-exports P2P adapter classes and event type
- `client/src/providers/GameProvider.tsx` - P2P mode lifecycle (host room creation, guest join, deck exchange)
- `client/src/pages/GamePage.tsx` - Maps p2p-host/p2p-join URL modes, handles P2P events
- `client/src/pages/MenuPage.tsx` - Smart code routing (5-char P2P vs server), P2P host navigation
- `client/src/components/lobby/LobbyView.tsx` - Host (P2P) button alongside Host (Server)
- `client/package.json` - peerjs@1.5.5 dependency added

## Decisions Made
- P2P host is player 0, guest is player 1 -- matching the WasmAdapter/server convention where player 0 is the "home" player
- filterStateForGuest uses JSON.parse(JSON.stringify()) for deep clone, zeroing out host hand/library arrays
- 5-char codes auto-detected as P2P via parseRoomCode validation; longer codes route to server join
- Guest sends its built deck payload to host, host combines both into the WASM initializeGame payload
- P2P games are code-only: they never appear in the server lobby listing

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- P2P networking layer complete and ready for browser-based multiplayer without server
- P2P adapter pattern established for any future enhancements (emotes, concede UI)
- Smart code routing enables seamless UX for both server and P2P games from the same lobby

## Self-Check: PASSED

All 10 created/modified files verified on disk. All 3 task commit hashes verified in git log.

---
*Phase: 26-polish-and-fix-multi-player-with-lobby-and-embedded-server*
*Completed: 2026-03-11*
