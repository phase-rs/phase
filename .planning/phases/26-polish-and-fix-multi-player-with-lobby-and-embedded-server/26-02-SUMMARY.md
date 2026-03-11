---
phase: 26-polish-and-fix-multi-player-with-lobby-and-embedded-server
plan: 02
subsystem: server
tags: [websocket, lobby, protocol, serde, axum, multiplayer]

requires:
  - phase: 08-ai-multiplayer
    provides: "Original server-core protocol and session management"
provides:
  - "Extended ClientMessage with SubscribeLobby, UnsubscribeLobby, CreateGameWithSettings, JoinGameWithPassword, Concede, Emote"
  - "Extended ServerMessage with LobbyUpdate, LobbyGameAdded, LobbyGameRemoved, PlayerCount, PasswordRequired, Conceded, Emote, TimerUpdate"
  - "GameStarted now includes opponent_name: Option<String>"
  - "LobbyManager module for game listing, password verification, and stale game expiry"
  - "SessionManager extended with display_names, timer_seconds, create_game_with_settings, join_game_with_name"
  - "phase-server handles all new message types with lobby subscriber broadcasting"
affects: [26-03, 26-05, 26-06]

tech-stack:
  added: []
  patterns: ["LobbyManager for game listing with password and expiry", "AtomicU32 for player count tracking", "Lobby subscriber broadcast pattern"]

key-files:
  created:
    - crates/server-core/src/lobby.rs
  modified:
    - crates/server-core/src/protocol.rs
    - crates/server-core/src/session.rs
    - crates/server-core/src/lib.rs
    - crates/phase-server/src/main.rs

key-decisions:
  - "Direct string comparison for game passwords (no bcrypt -- appropriate for game passwords)"
  - "AtomicU32 for player count instead of Mutex<u32> (lock-free, only needs atomic increment/decrement)"
  - "Lobby subscribers tracked as Vec<UnboundedSender> with retain-on-closed cleanup"
  - "password_required sentinel string from verify_password triggers PasswordRequired message vs generic Error"

patterns-established:
  - "LobbyManager: HashMap-based game registry with register/unregister/verify/expire lifecycle"
  - "Opponent name injection: read session.display_names and swap indices for each player"

requirements-completed: [MP-LOBBY-SRV, MP-CONCEDE-SRV, MP-EMOTE-SRV, MP-TIMER-SRV, MP-OPPONENT-NAME]

duration: 5min
completed: 2026-03-11
---

# Phase 26 Plan 02: Server Protocol & Lobby Summary

**Extended server protocol with 15 new message variants, LobbyManager with password/expiry, and phase-server handlers for lobby, concede, emote, and opponent name delivery**

## Performance

- **Duration:** 5 min
- **Started:** 2026-03-11T01:10:56Z
- **Completed:** 2026-03-11T01:16:27Z
- **Tasks:** 2
- **Files modified:** 5

## Accomplishments
- Extended protocol with 6 new ClientMessage variants and 8 new ServerMessage variants, all with roundtrip serde tests
- Created LobbyManager module with game registration, password verification, public game listing, and stale game expiry (300s)
- Wired all new message types into phase-server with lobby subscriber broadcasting, player count tracking, and opponent name injection into GameStarted
- Extended GameSession with display_names and timer_seconds fields, added create_game_with_settings and join_game_with_name methods

## Task Commits

Each task was committed atomically:

1. **Task 1: Extend protocol with lobby, concede, emote, timer messages and opponent_name** - `d986a25f` (feat)
2. **Task 2: Wire lobby protocol into phase-server message handler with opponent name injection** - `96b4209b` (feat)

## Files Created/Modified
- `crates/server-core/src/protocol.rs` - Extended ClientMessage/ServerMessage enums with 14 new variants, LobbyGame struct, 17 roundtrip serde tests
- `crates/server-core/src/lobby.rs` - New LobbyManager with register/unregister/verify_password/public_games/timer_seconds/check_expired, 10 unit tests
- `crates/server-core/src/session.rs` - Extended GameSession with display_names/timer_seconds, added create_game_with_settings and join_game_with_name
- `crates/server-core/src/lib.rs` - Added lobby module and re-exports for LobbyManager, LobbyGame
- `crates/phase-server/src/main.rs` - Full lobby integration: shared state, subscriber management, all new message handlers, background expiry, player count

## Decisions Made
- Direct string comparison for game passwords (no bcrypt) -- game passwords are low-stakes and don't need hashing overhead
- AtomicU32 for player count tracking -- avoids mutex contention for a simple counter
- Lobby subscribers stored as Vec<UnboundedSender<ServerMessage>> with closed-channel cleanup on retain
- password_required sentinel from verify_password triggers PasswordRequired response (distinct from Error for wrong password)

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Server protocol fully extended and handlers wired -- ready for Plan 03 (frontend lobby UI) to consume
- GameStarted opponent_name field available for Plan 06 (in-game UX) opponent display
- Timer infrastructure in session ready for future timer enforcement logic

## Self-Check: PASSED

All 5 files verified present. Both task commits (d986a25f, 96b4209b) verified in git log.

---
*Phase: 26-polish-and-fix-multi-player-with-lobby-and-embedded-server*
*Completed: 2026-03-11*
