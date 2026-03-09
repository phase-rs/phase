---
phase: 17-mtg-specific-ui
plan: 03
subsystem: ui
tags: [react, framer-motion, zustand, combat, priority, mtg]

requires:
  - phase: 17-mtg-specific-ui
    provides: usePhaseInfo hook, gameButtonClass utility, buttonStyles
provides:
  - ActionButton unified combat/priority orchestrator component
  - Skip-confirm guard pattern for destructive combat actions
  - Resolve All with interrupt support
affects: [17-04-mana-payment, 17-05-polish]

tech-stack:
  added: []
  patterns: [skip-confirm guard with armed timer, resolve-all async loop with ref interrupt]

key-files:
  created:
    - client/src/components/board/ActionButton.tsx
  modified:
    - client/src/pages/GamePage.tsx

key-decisions:
  - "ActionButton uses dispatchAction directly instead of useGameDispatch hook for Resolve All async loop"
  - "Skip-confirm guard uses 1200ms armed timer with useRef for timeout tracking"
  - "Old PassButton/CombatOverlay/AttackerControls/BlockerControls files kept (not deleted) to avoid breaking potential importers"

patterns-established:
  - "Skip-confirm guard: first tap arms with label change + timer, second tap within window dispatches"
  - "Resolve All pattern: ref-gated async loop checking store state each iteration for interrupt"

requirements-completed: [STACK-02, STACK-03, STACK-04, COMBAT-01, COMBAT-02, COMBAT-03]

duration: 2min
completed: 2026-03-09
---

# Phase 17 Plan 03: ActionButton Orchestrator Summary

**Unified ActionButton replacing PassButton + CombatOverlay with skip-confirm guards, Resolve All interrupt, and mode-driven rendering**

## Performance

- **Duration:** 2 min
- **Started:** 2026-03-09T17:52:17Z
- **Completed:** 2026-03-09T17:54:17Z
- **Tasks:** 2
- **Files modified:** 2

## Accomplishments
- ActionButton renders correct controls for all 4 game modes: combat-attackers, combat-blockers, priority-stack, priority-empty
- Skip-confirm guard on No Attacks and No Blocks prevents accidental skipping (1200ms armed window)
- Resolve All dispatches PassPriority sequentially with interrupt on stack growth or player change

## Task Commits

Each task was committed atomically:

1. **Task 1: Create ActionButton unified orchestrator** - `83af114` (feat)
2. **Task 2: Wire ActionButton into GamePage, remove old components** - `6073aee` (feat)

## Files Created/Modified
- `client/src/components/board/ActionButton.tsx` - Unified combat/priority orchestrator (233 lines)
- `client/src/pages/GamePage.tsx` - Replaced PassButton + CombatOverlay with ActionButton

## Decisions Made
- Used `dispatchAction` directly for Resolve All async loop since it needs to call store outside React component lifecycle
- Kept old component files (PassButton, CombatOverlay, AttackerControls, BlockerControls) to avoid breaking CombatOverlay test suite or other potential importers
- Pending blocker indicator positioned as absolute element above the button bar rather than fixed top banner

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- ActionButton is live and handles all priority/combat interactions
- Ready for Plan 04 (mana payment UI upgrade)
- Auto-pass and full-control continue to work via existing autoPass.ts and FullControlToggle

---
*Phase: 17-mtg-specific-ui*
*Completed: 2026-03-09*

## Self-Check: PASSED
