---
phase: 29-support-n-players
plan: 06
subsystem: transport
tags: [wasm, server, websocket, ai, multiplayer, n-player]

requires:
  - phase: 29-01
    provides: "N-player GameState, players module, FormatConfig"
  - phase: 29-02
    provides: "N-player priority, turns, elimination"
provides:
  - "WASM bridge with player_id parameter for get_ai_action"
  - "N-player state filtering hiding all opponents' hands/libraries"
  - "N-player server sessions supporting 2-6 players"
  - "N-player protocol with format config, spectator, and eliminated player support"
  - "AI crate fully migrated to players:: functions"
affects: [29-07, 29-08, 29-09, 29-10]

tech-stack:
  added: []
  patterns:
    - "players::opponents() for N-player opponent enumeration in transport/AI"
    - "Vec-based session player tracking instead of fixed arrays"
    - "Broadcast to all connected players pattern in server"

key-files:
  modified:
    - "crates/engine-wasm/src/lib.rs"
    - "crates/server-core/src/filter.rs"
    - "crates/server-core/src/session.rs"
    - "crates/server-core/src/protocol.rs"
    - "crates/phase-server/src/main.rs"
    - "crates/phase-ai/src/eval.rs"
    - "crates/phase-ai/src/combat_ai.rs"
    - "crates/phase-ai/src/card_hints.rs"
    - "crates/phase-ai/src/search.rs"
    - "crates/phase-ai/src/legal_actions.rs"

key-decisions:
  - "eval.rs aggregates opponent stats and uses average opponent life for scoring balance in multiplayer"
  - "combat_ai.rs considers blockers from ALL opponents, uses min opponent life for aggression heuristic"
  - "Session uses Vec-based player_tokens/connected/decks instead of fixed [T; 2] arrays"
  - "GameSession tracks ai_seats HashSet for future AI seat management"
  - "OpponentDisconnected/OpponentReconnected gain optional player field for N-player compat"
  - "Protocol player_count defaults to 2 via serde default for backward compatibility"

patterns-established:
  - "N-player broadcast: iterate all connected players instead of explicit p0/p1"
  - "Seat-based join: first_open_seat() assigns next available seat"

requirements-completed: [NP-WASM, NP-SERVER, NP-FILTER, NP-AI-MIGRATE]

duration: 10min
completed: 2026-03-11
---

# Phase 29 Plan 06: Transport and AI N-Player Migration Summary

**WASM bridge accepts player_id, server sessions support 2-6 players, state filtering hides all opponents, AI crate fully migrated to players:: functions**

## Performance

- **Duration:** 10 min
- **Started:** 2026-03-11T18:19:32Z
- **Completed:** 2026-03-11T18:29:25Z
- **Tasks:** 2
- **Files modified:** 10

## Accomplishments
- WASM get_ai_action now accepts player_id parameter instead of hardcoded PlayerId(1)
- AI eval, combat, card_hints, search, and legal_actions all use players::opponents() instead of PlayerId(1-x)
- State filtering hides ALL opponents' hands and ALL libraries for any viewer
- Server sessions use Vec-based tracking supporting 2-6 players with ai_seats
- Protocol extended with player_count, SpectatorJoin, player_names, eliminated_players
- Server broadcasts state updates to all connected players, not just two
- Zero PlayerId(1-x) remaining in AI, WASM, server-core, and phase-server crates

## Task Commits

Each task was committed atomically:

1. **Task 1: WASM bridge and AI crate N-player migration** - `22ee105ac` (feat)
2. **Task 2: Server session, protocol, and state filtering for N players** - `cc9b7174c` (feat)

## Files Created/Modified
- `crates/engine-wasm/src/lib.rs` - get_ai_action accepts player_id parameter
- `crates/phase-ai/src/eval.rs` - Aggregates opponent stats across all living opponents
- `crates/phase-ai/src/combat_ai.rs` - Collects blockers from all opponents
- `crates/phase-ai/src/card_hints.rs` - Evaluates threats from all opponents
- `crates/phase-ai/src/search.rs` - Uses players::opponents() for attack targets
- `crates/phase-ai/src/legal_actions.rs` - Uses players::opponents() for attack targets
- `crates/server-core/src/filter.rs` - Hides all opponents' hands and all libraries
- `crates/server-core/src/session.rs` - Vec-based N-player session management
- `crates/server-core/src/protocol.rs` - Extended with player_count, SpectatorJoin, player_names, eliminated_players
- `crates/phase-server/src/main.rs` - N-player broadcast, disconnect/reconnect for any player

## Decisions Made
- eval.rs uses average opponent life to avoid inflating score in multiplayer (comparing against sum would overvalue being ahead)
- combat_ai.rs uses minimum opponent life for aggression heuristic (attack the weakest opponent)
- Session tracks ai_seats as HashSet for future use by server AI seat management
- Protocol adds serde defaults for new fields to maintain backward compatibility with existing clients
- OpponentDisconnected/OpponentReconnected keep existing field structure but add optional player field

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
- Borrow checker issue in filter.rs when iterating players while mutating objects -- resolved by collecting IDs into Vec first
- Pre-existing clippy warning in engine json_loader.rs (type_complexity) -- not caused by this plan, out of scope

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- All transport layers ready for N-player games
- Frontend adapters (WasmAdapter, WebSocketAdapter) will need updates to pass player_id to get_ai_action
- P2P adapter filterStateForGuest will need N-player update

---
*Phase: 29-support-n-players*
*Completed: 2026-03-11*
