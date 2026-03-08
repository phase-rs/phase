---
phase: 11-tech-debt-cleanup-constants-combat-ui-wasm-tests-ci-coverage
plan: 01
subsystem: ui
tags: [typescript, constants, react, modal]

requires:
  - phase: 09-game-play-integration
    provides: card-data.json fetch, AI controller, game constants
provides:
  - Canonical constants/game.ts with all game logic constants
  - Canonical constants/ui.ts with UI-specific constants
  - CardDataMissingModal component for missing card data UX
affects: [11-02, 11-03]

tech-stack:
  added: []
  patterns: [centralized-constants-modules]

key-files:
  created:
    - client/src/constants/ui.ts
    - client/src/components/modal/CardDataMissingModal.tsx
  modified:
    - client/src/constants/game.ts
    - client/src/stores/gameStore.ts
    - client/src/hooks/useKeyboardShortcuts.ts
    - client/src/game/controllers/aiController.ts
    - client/src/pages/GamePage.tsx

key-decisions:
  - "AI constants use _MS suffix for clarity (AI_BASE_DELAY_MS, AI_DELAY_VARIANCE_MS)"
  - "CardDataMissingModal uses Continue anyway as text link rather than primary button"

patterns-established:
  - "Centralized constants: all game logic constants in constants/game.ts, UI constants in constants/ui.ts"

requirements-completed: [TD-01, TD-04]

duration: 2min
completed: 2026-03-08
---

# Phase 11 Plan 01: Constants Consolidation & CardDataMissingModal Summary

**Eliminated 3 duplicate constant definitions, extracted AI magic numbers to constants/game.ts, and added CardDataMissingModal for missing card-data.json**

## Performance

- **Duration:** 2 min
- **Started:** 2026-03-08T18:37:31Z
- **Completed:** 2026-03-08T18:39:50Z
- **Tasks:** 2
- **Files modified:** 7

## Accomplishments
- Consolidated UNDOABLE_ACTIONS and MAX_UNDO_HISTORY to single canonical definition in constants/game.ts
- Extracted AI_PLAYER_ID, AI_BASE_DELAY_MS, AI_DELAY_VARIANCE_MS from aiController.ts to constants/game.ts
- Created CardDataMissingModal with generation instructions and Continue anyway escape hatch
- Verified ROADMAP.md and REQUIREMENTS.md are already accurate (no fixes needed)

## Task Commits

Each task was committed atomically:

1. **Task 1: Consolidate constants and extract magic numbers** - `9774e88` (refactor)
2. **Task 2: CardDataMissingModal and documentation fixes** - `f4ef06b` (feat)

## Files Created/Modified
- `client/src/constants/game.ts` - Added AI_PLAYER_ID, AI_BASE_DELAY_MS, AI_DELAY_VARIANCE_MS
- `client/src/constants/ui.ts` - New file with COMBAT_TILT_DEGREES, DEFAULT_ANIMATION_DURATION_MS
- `client/src/stores/gameStore.ts` - Removed duplicate constants, imports from constants/game
- `client/src/hooks/useKeyboardShortcuts.ts` - Removed duplicate constants and re-export
- `client/src/game/controllers/aiController.ts` - Imports AI constants from constants/game
- `client/src/components/modal/CardDataMissingModal.tsx` - New blocking modal component
- `client/src/pages/GamePage.tsx` - Renders CardDataMissingModal when card-data.json missing

## Decisions Made
- AI constants use _MS suffix for clarity (AI_BASE_DELAY_MS instead of AI_BASE_DELAY)
- CardDataMissingModal uses "Continue anyway" as a small gray text link rather than a primary button, to discourage proceeding without card data

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- constants/ui.ts ready for plan 11-02 (combat UI) to consume COMBAT_TILT_DEGREES
- All tests passing, no type errors

---
*Phase: 11-tech-debt-cleanup-constants-combat-ui-wasm-tests-ci-coverage*
*Completed: 2026-03-08*
