---
phase: 26-polish-and-fix-multi-player-with-lobby-and-embedded-server
plan: 06
subsystem: ui
tags: [react, websocket, multiplayer, framer-motion, zustand]

# Dependency graph
requires:
  - phase: 26-02
    provides: Server protocol messages (Conceded, Emote, TimerUpdate, GameStarted with opponent_name)
  - phase: 26-03
    provides: Lobby UI and multiplayerStore with opponentDisplayName
provides:
  - Concede flow with confirmation dialog and server message
  - MTGA-style quick emotes (send/receive with auto-fade)
  - Opponent display name in HUD during gameplay
  - Enhanced game over screen with dynamic winner, turn count, duration, Back to Lobby
  - Timer countdown display for turn timers
affects: []

# Tech tracking
tech-stack:
  added: []
  patterns:
    - "WsAdapterEvent discriminated union extension for new server message types"
    - "Auto-clearing received emote state via setTimeout with unique IDs"
    - "Dynamic winner determination using activePlayerId from multiplayerStore"

key-files:
  created:
    - client/src/components/multiplayer/ConcedeDialog.tsx
    - client/src/components/multiplayer/EmoteOverlay.tsx
  modified:
    - client/src/adapter/ws-adapter.ts
    - client/src/components/chrome/GameMenu.tsx
    - client/src/components/hud/OpponentHud.tsx
    - client/src/pages/GamePage.tsx

key-decisions:
  - "Emote auto-fade uses 3s timeout with unique numeric IDs for overlap handling"
  - "Game duration tracked from gameStartedAt timestamp set on GameStarted event"
  - "Back to Lobby navigates to /?view=lobby for lobby return after game over"

patterns-established:
  - "WsAdapterEvent extension: add new discriminated union variants for each server message type"
  - "Multiplayer-only UI: conditional rendering based on isOnlineMode flag"

requirements-completed: [MP-CONCEDE, MP-EMOTE, MP-TIMER-UI, MP-GAMEOVER, MP-OPPONENT-NAME]

# Metrics
duration: 8min
completed: 2026-03-11
---

# Phase 26 Plan 06: In-Game Multiplayer UX Summary

**Concede with confirmation dialog, MTGA-style quick emotes, opponent name display, turn timer UI, and enhanced game over with dynamic winner and lobby return**

## Performance

- **Duration:** 8 min
- **Started:** 2026-03-11T01:55:00Z
- **Completed:** 2026-03-11T02:03:00Z
- **Tasks:** 3 (2 auto + 1 checkpoint)
- **Files modified:** 6

## Accomplishments
- ConcedeDialog with framer-motion animated confirmation modal and server message integration
- EmoteOverlay with 5 quick emotes (Good game, Nice play, Thinking, Hello, Oops) and auto-fading received emote bubbles
- Opponent display name shown in HUD from multiplayerStore, populated via GameStarted server message
- Game over screen with dynamic winner determination using activePlayerId, turn count, game duration, and Back to Lobby button
- Timer countdown display wired from server TimerUpdate messages

## Task Commits

Each task was committed atomically:

1. **Task 1: Implement concede, emotes, and opponent display name** - `76561d1c` (feat)
2. **Task 2: Wire multiplayer UX into GamePage and enhance game over screen** - `5ef5cc03` (feat)
3. **Task 3: Verify in-game multiplayer UX end-to-end** - checkpoint (approved)

## Files Created/Modified
- `client/src/components/multiplayer/ConcedeDialog.tsx` - Modal confirmation dialog for conceding with framer-motion animation
- `client/src/components/multiplayer/EmoteOverlay.tsx` - Emote button bar and received emote floating bubble display
- `client/src/adapter/ws-adapter.ts` - Extended WsAdapterEvent with conceded/emoteReceived/timerUpdate variants, added sendConcede/sendEmote methods
- `client/src/components/chrome/GameMenu.tsx` - Added Concede button for online mode with isOnlineMode/onConcede props
- `client/src/components/hud/OpponentHud.tsx` - Added opponentName prop display below life total
- `client/src/pages/GamePage.tsx` - Wired all multiplayer UX: concede dialog, emotes, timer, game over enhancements, duration tracking

## Decisions Made
- Emote auto-fade uses 3-second timeout with unique numeric IDs for correct overlap handling
- Game duration tracked via gameStartedAt timestamp set when GameStarted event fires
- Back to Lobby navigates to /?view=lobby to return to lobby view after game over
- Dynamic winner check uses activePlayerId from multiplayerStore instead of hardcoded player 0

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

None.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- All in-game multiplayer UX features complete (concede, emotes, opponent name, timer, game over)
- Plan 05 (Tauri sidecar/embedded server) also in wave 3 -- both can proceed independently
- Phase 26 nearing completion with 5/6 plans done after this

## Self-Check: PASSED

- All 6 referenced files exist on disk
- Both task commits verified in git history (76561d1c, 5ef5cc03)

---
*Phase: 26-polish-and-fix-multi-player-with-lobby-and-embedded-server*
*Completed: 2026-03-11*
