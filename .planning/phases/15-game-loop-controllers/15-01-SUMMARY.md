---
phase: 15-game-loop-controllers
plan: 01
subsystem: ui
tags: [dispatch, auto-pass, ai-controller, zustand, game-loop]

requires:
  - phase: 14-animation-pipeline
    provides: Animation store, event normalizer, snapshot-animate-update flow
provides:
  - Standalone dispatch module (dispatchAction) for non-React callers
  - OpponentController interface for AI/WS opponent abstraction
  - shouldAutoPass heuristic for MTGA-style auto-passing
  - Phase stop preferences in preferencesStore
affects: [15-game-loop-controllers]

tech-stack:
  added: []
  patterns: [module-level mutex for dispatch pipeline, conservative auto-pass heuristics]

key-files:
  created:
    - client/src/game/dispatch.ts
    - client/src/game/autoPass.ts
    - client/src/game/controllers/types.ts
  modified:
    - client/src/game/controllers/aiController.ts
    - client/src/stores/preferencesStore.ts
    - client/src/constants/game.ts
    - client/src/pages/GamePage.tsx

key-decisions:
  - "Module-level boolean mutex replaces useRef for dispatch pipeline"
  - "AI controller reads gameStore directly instead of injected callbacks"
  - "Auto-pass conservative heuristic: stops when player has mana + instants/flash/abilities"

patterns-established:
  - "Module-level dispatch: non-React code uses dispatchAction() instead of useGameDispatch hook"
  - "OpponentController interface: start/stop/dispose lifecycle for opponent implementations"

requirements-completed: [LOOP-01, LOOP-04]

duration: 2min
completed: 2026-03-09
---

# Phase 15 Plan 01: Foundation Modules Summary

**Standalone dispatch pipeline with module-level mutex, MTGA-style auto-pass heuristics, and OpponentController abstraction for AI**

## Performance

- **Duration:** 2 min
- **Started:** 2026-03-09T04:38:32Z
- **Completed:** 2026-03-09T04:40:42Z
- **Tasks:** 2
- **Files modified:** 7

## Accomplishments
- Extracted dispatch pipeline from React hook into standalone module with module-level mutex
- Created OpponentController interface and refactored AI controller to use standalone dispatch
- Built MTGA-style auto-pass heuristics with conservative instant/ability detection
- Added phase stop preferences to preferencesStore with sensible defaults

## Task Commits

Each task was committed atomically:

1. **Task 1: Extract standalone dispatch, create OpponentController types, and add phase stop preferences** - `c173e91` (feat)
2. **Task 2: Build auto-pass heuristics and refactor AI controller to use standalone dispatch** - `55455e1` (feat)

## Files Created/Modified
- `client/src/game/dispatch.ts` - Standalone dispatch with snapshot-animate-update flow and module-level mutex
- `client/src/game/autoPass.ts` - shouldAutoPass heuristic with MTGA-style rules
- `client/src/game/controllers/types.ts` - OpponentController interface
- `client/src/game/controllers/aiController.ts` - Refactored to use dispatchAction and gameStore directly
- `client/src/stores/preferencesStore.ts` - Added phaseStops with defaults
- `client/src/constants/game.ts` - Added PLAYER_ID, AUTO_PASS_BEAT_MS; adjusted AI_BASE_DELAY_MS to 500ms
- `client/src/pages/GamePage.tsx` - Updated createAIController call site for new signature

## Decisions Made
- Module-level boolean mutex replaces useRef for dispatch pipeline (no React dependency)
- AI controller reads gameStore directly instead of injected callbacks (simpler API)
- Auto-pass conservative heuristic: stops when player has mana + instants/flash/activated abilities
- AI_BASE_DELAY_MS reduced from 800 to 500ms for 500-900ms range (closer to user's 300-800ms guidance)

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Updated GamePage.tsx call site for new createAIController signature**
- **Found during:** Task 2 (AI controller refactor)
- **Issue:** createAIController signature changed from 3 params to 1 (config only), breaking GamePage.tsx call site
- **Fix:** Updated call to `createAIController({ difficulty })` removing injected getState/submitAction callbacks
- **Files modified:** client/src/pages/GamePage.tsx
- **Verification:** Type-check passes
- **Committed in:** 55455e1 (Task 2 commit)

---

**Total deviations:** 1 auto-fixed (1 blocking)
**Impact on plan:** Necessary fix for API change. No scope creep.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- dispatch.ts ready for game loop controller (Plan 02) to use
- OpponentController interface ready for game loop to manage AI lifecycle
- shouldAutoPass ready for game loop to call during priority windows
- Phase stop preferences persisted and accessible via preferencesStore

---
*Phase: 15-game-loop-controllers*
*Completed: 2026-03-09*
