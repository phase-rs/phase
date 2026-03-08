---
phase: 11-tech-debt-cleanup-constants-combat-ui-wasm-tests-ci-coverage
plan: 02
subsystem: ui
tags: [react, zustand, framer-motion, combat, tailwind]

requires:
  - phase: 07-frontend-react-ui
    provides: PermanentCard, GamePage, uiStore, TargetArrow pattern
provides:
  - CombatOverlay component for attacker/blocker declaration
  - Combat state management in uiStore (combatMode, selectedAttackers, blockerAssignments)
  - Visual feedback (orange glow + tilt for attackers, blue glow for blockers, arrows)
affects: [game-ui, combat-flow]

tech-stack:
  added: []
  patterns: [combat click delegation via uiStore combatClickHandler, two-click blocker assignment]

key-files:
  created:
    - client/src/components/combat/CombatOverlay.tsx
    - client/src/components/combat/AttackerControls.tsx
    - client/src/components/combat/BlockerControls.tsx
    - client/src/components/combat/BlockerArrow.tsx
  modified:
    - client/src/stores/uiStore.ts
    - client/src/components/board/PermanentCard.tsx
    - client/src/pages/GamePage.tsx

key-decisions:
  - "Combat click delegation via combatClickHandler fn on uiStore (blocker mode sets handler, PermanentCard calls it)"
  - "Two-click blocker assignment: first click selects blocker, second click assigns to attacker"
  - "Combat glow takes priority over targeting glow in PermanentCard"

patterns-established:
  - "Combat overlay pattern: mode-driven component with uiStore state cleanup on unmount"
  - "Click delegation: store a callback in zustand for cross-component click routing"

requirements-completed: [TD-02, TD-03]

duration: 3min
completed: 2026-03-08
---

# Phase 11 Plan 02: Combat UI Overlay Summary

**Arena-style combat overlay with attacker toggle, blocker assignment arrows, and visual feedback (orange glow + 15-degree tilt)**

## Performance

- **Duration:** 3 min
- **Started:** 2026-03-08T18:37:32Z
- **Completed:** 2026-03-08T18:40:19Z
- **Tasks:** 2
- **Files modified:** 7

## Accomplishments
- CombatOverlay component with attacker and blocker modes, auto-cleaning state on unmount
- PermanentCard shows orange glow + 15-degree spring-animated tilt for selected attackers, blue glow for assigned blockers
- BlockerArrow draws animated SVG lines from each blocker to its assigned attacker
- Attack All / Skip / Confirm Attackers buttons and Confirm Blockers button

## Task Commits

1. **Task 1: Combat state in uiStore and CombatOverlay component** - `f4ef06b` (feat)
2. **Task 2: Wire CombatOverlay into GamePage and PermanentCard visuals** - `130a3f1` (feat)

## Files Created/Modified
- `client/src/components/combat/CombatOverlay.tsx` - Main combat overlay with attacker/blocker modes
- `client/src/components/combat/AttackerControls.tsx` - Attack All / Skip / Confirm buttons
- `client/src/components/combat/BlockerControls.tsx` - Confirm Blockers button
- `client/src/components/combat/BlockerArrow.tsx` - SVG arrow from blocker to attacker
- `client/src/stores/uiStore.ts` - Extended with combat state and actions
- `client/src/components/board/PermanentCard.tsx` - Combat glow, tilt, click routing
- `client/src/pages/GamePage.tsx` - Conditional CombatOverlay rendering

## Decisions Made
- Used combatClickHandler function stored in uiStore for blocker click delegation (simpler than event bus)
- Two-click blocker pattern: click blocker first, then click attacker to assign
- Combat glow styles take priority over targeting glow in PermanentCard render order

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Combat UI is fully wired; DeclareAttackers/DeclareBlockers WaitingFor states now have a complete UI loop
- Ready for WASM test and CI coverage plans

---
*Phase: 11-tech-debt-cleanup-constants-combat-ui-wasm-tests-ci-coverage*
*Completed: 2026-03-08*
