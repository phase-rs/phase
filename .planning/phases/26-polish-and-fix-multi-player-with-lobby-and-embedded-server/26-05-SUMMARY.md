---
phase: 26-polish-and-fix-multi-player-with-lobby-and-embedded-server
plan: 05
subsystem: networking
tags: [tauri, sidecar, webrtc, connection-ux, server-detection, typescript, react]

# Dependency graph
requires:
  - phase: 26-02
    provides: "Server protocol, lobby manager, session management"
  - phase: 26-04
    provides: "P2P networking, multiplayerStore, ws-adapter"
provides:
  - Tauri sidecar configuration and lifecycle management (spawnSidecar/stopSidecar)
  - Smart server detection (sidecar > last-used > default)
  - CODE@IP:PORT join code parsing for cross-server joins
  - ConnectionDot status indicator (green/yellow/red)
  - ConnectionToast for connection failure recovery
  - Connection status tracking in GameProvider
affects: [26-06]

# Tech tracking
tech-stack:
  added: [tauri-plugin-shell@2, "@tauri-apps/plugin-shell@2.3.5"]
  patterns: [sidecar port scanning with health check, smart server detection cascade, CODE@IP:PORT join syntax]

key-files:
  created:
    - client/src/services/sidecar.ts
    - client/src/services/serverDetection.ts
    - client/src/components/multiplayer/ConnectionDot.tsx
    - client/src/components/multiplayer/ConnectionToast.tsx
  modified:
    - client/src-tauri/tauri.conf.json
    - client/src-tauri/capabilities/default.json
    - client/src-tauri/Cargo.toml
    - client/src-tauri/src/lib.rs
    - client/src/providers/GameProvider.tsx
    - client/src/components/lobby/LobbyView.tsx
    - client/src/stores/multiplayerStore.ts
    - client/src/pages/GamePage.tsx
    - crates/phase-server/src/main.rs

key-decisions:
  - "Static top-level import of @tauri-apps/plugin-shell with runtime isTauri() guard"
  - "Port scanning 9374-9383 for sidecar with reuse of existing server on active port"
  - "Smart detection cascade: Tauri sidecar localhost > stored server address > default"
  - "parseJoinCode extracts CODE and optional server address from CODE@IP:PORT format"
  - "Default server port standardized to 9374 (avoids common port conflicts)"

patterns-established:
  - "Sidecar lifecycle: spawn with port scan, health check polling, module-level handle for cleanup"
  - "Server detection cascade: try Tauri sidecar, then last-used, then default"
  - "ConnectionDot: framer-motion pulse for connecting, static dot for connected/disconnected"
  - "ConnectionToast: auto-dismiss 5s with Retry/Settings actions"

requirements-completed: [MP-SIDECAR, MP-CONNECT-UX, MP-SERVER-DETECT]

# Metrics
duration: 8min
completed: 2026-03-11
---

# Phase 26 Plan 05: Embedded Server & Connection UX Summary

**Tauri sidecar lifecycle for desktop server hosting, smart server detection cascade, CODE@IP:PORT join syntax, and ConnectionDot/Toast connection UX components**

## Performance

- **Duration:** 8 min
- **Started:** 2026-03-11T01:56:28Z
- **Completed:** 2026-03-11T02:05:26Z
- **Tasks:** 2
- **Files modified:** 15

## Accomplishments
- Configured Tauri sidecar with externalBin, shell plugin registration, and capabilities for spawn/kill
- Created sidecar.ts with spawnSidecar (port scanning 9374-9383, health check polling) and stopSidecar lifecycle management
- Built smart server detection service (sidecar > stored address > default) with 2s timeout health checks
- Implemented CODE@IP:PORT join code parsing for cross-server game joining
- Created ConnectionDot component with green/yellow/red status and framer-motion pulse animation
- Created ConnectionToast for connection failure with auto-dismiss, Retry, and Settings actions
- Wired connection status tracking into GameProvider with async server detection
- Standardized default server port to 9374 across all client, server, and test code

## Task Commits

Each task was committed atomically:

1. **Task 1: Configure Tauri sidecar and implement sidecar lifecycle service** - `2f0c9daf` (feat)
2. **Task 2: Smart server detection, CODE@IP parsing, connection dot, and failure toasts** - `70830c70` (feat)

## Files Created/Modified
- `client/src/services/sidecar.ts` - Sidecar lifecycle: spawnSidecar with port scanning, health check, stopSidecar cleanup
- `client/src/services/serverDetection.ts` - detectServerUrl cascade and parseJoinCode for CODE@IP:PORT
- `client/src/components/multiplayer/ConnectionDot.tsx` - Color-coded connection status indicator with pulse animation
- `client/src/components/multiplayer/ConnectionToast.tsx` - Connection failure toast with Retry/Settings actions
- `client/src-tauri/tauri.conf.json` - Added externalBin for phase-server sidecar
- `client/src-tauri/capabilities/default.json` - Added shell:allow-spawn, shell:allow-kill, shell:allow-stdin-write
- `client/src-tauri/Cargo.toml` - Added tauri-plugin-shell dependency
- `client/src-tauri/src/lib.rs` - Registered tauri_plugin_shell::init()
- `client/src/providers/GameProvider.tsx` - Async smart server detection, connection status lifecycle tracking
- `client/src/components/lobby/LobbyView.tsx` - CODE@IP:PORT parsing in join-by-code flow
- `client/src/stores/multiplayerStore.ts` - Added toastMessage, showToast, clearToast state
- `client/src/pages/GamePage.tsx` - Wired ConnectionDot and ConnectionToast for online mode

## Decisions Made
- Static import of Command from @tauri-apps/plugin-shell at top of file (per CLAUDE.md no-inline-imports rule) with runtime isTauri() guard in function bodies
- Port scanning starts at 9374 (avoids conflict with common dev ports like 8080/3000)
- Existing server on scanned port is reused rather than spawning a new one
- parseJoinCode defaults to port 9374 when no port specified in CODE@IP format
- Connection status tracking integrated into GameProvider WebSocket event handler rather than separate subscription

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Port standardization from 8080 to 9374**
- **Found during:** Task 2
- **Issue:** External port changes (8080->9374) were present in working tree across server, client, and tests
- **Fix:** Incorporated coordinated port change into Task 2 commit for consistency
- **Files modified:** sidecar.ts, GameProvider.tsx, multiplayerStore.ts, PreferencesModal.tsx, ws-adapter.test.ts, phase-server/main.rs
- **Committed in:** 70830c70

---

**Total deviations:** 1 auto-fixed (1 blocking)
**Impact on plan:** Port standardization was necessary for consistency across codebase. No scope creep.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required. Sidecar binary must be built and placed per instructions in sidecar.ts comments.

## Next Phase Readiness
- Sidecar infrastructure ready for Tauri desktop builds to host games seamlessly
- Connection UX components (ConnectionDot, ConnectionToast) available for all multiplayer modes
- Smart server detection enables zero-config multiplayer for Tauri users
- Plan 06 can proceed with its GamePage.tsx changes (concede, emotes, game over) independently

## Self-Check: PASSED

All 4 created files verified on disk. Both task commits (2f0c9daf, 70830c70) verified in git log.

---
*Phase: 26-polish-and-fix-multi-player-with-lobby-and-embedded-server*
*Completed: 2026-03-11*
