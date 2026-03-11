---
phase: 29-support-n-players
plan: 08
subsystem: ui
tags: [react, typescript, combat, multiplayer, attack-targets, blocking]

requires:
  - phase: 29-03
    provides: N-player combat engine with per-creature attack targets
  - phase: 29-06
    provides: AI combat with attack target selection
  - phase: 29-07
    provides: N-player board UI, AttackTarget types, buildAttacks utility
provides:
  - AttackTargetPicker component for per-creature target selection
  - "Attack All" and "Split Attacks" modes for multiplayer combat
  - Multi-defender blocking with visual dimming of irrelevant attackers
  - Attacker grouping utilities for N-player battlefield rendering
affects: [29-10-lobby, 29-12-deckbuilder]

tech-stack:
  added: []
  patterns: [AttackTargetPicker modal with mode toggle, isAttackerTargetingPlayer for multi-defender filtering]

key-files:
  created:
    - client/src/components/controls/AttackTargetPicker.tsx
  modified:
    - client/src/utils/combat.ts
    - client/src/components/board/ActionButton.tsx
    - client/src/components/combat/CombatOverlay.tsx
    - client/src/components/board/BlockAssignmentLines.tsx
    - client/src/viewmodel/battlefieldProps.ts

key-decisions:
  - "AttackTargetPicker modal with two modes: 'Attack All' (one-click) and 'Split Attacks' (per-creature)"
  - "Picker only shown when valid_attack_targets has 2+ entries; 2-player mode unchanged"
  - "BlockAssignmentLines dims lines for attackers not targeting the current player (opacity 0.25 vs 0.7)"
  - "Engine valid_block_targets already restricts blocker assignments to relevant attackers; no additional client filtering needed"

patterns-established:
  - "Modal overlay pattern for combat target selection with mode toggle"
  - "isAttackerTargetingPlayer utility for multi-defender visual filtering"

requirements-completed: [NP-ATTACK-UI, NP-BLOCK-UI]

duration: 6min
completed: 2026-03-11
---

# Phase 29 Plan 08: N-Player Combat Attack/Block UI Summary

**Per-creature attack target picker with "Attack All" shortcut and multi-defender blocking with dimmed irrelevant attacker lines**

## Performance

- **Duration:** 6 min
- **Started:** 2026-03-11T19:14:42Z
- **Completed:** 2026-03-11T19:21:00Z
- **Tasks:** 2
- **Files modified:** 6

## Accomplishments
- AttackTargetPicker supports both "Attack All" (send all attackers at one target) and "Split Attacks" (per-creature assignment) modes
- Multiplayer attack target selection integrates into both ActionButton and CombatOverlay
- Block assignment lines dim and suppress pulse animation for attackers targeting other players
- Utility functions for grouping attackers by target and checking attacker-defender relationships

## Task Commits

Each task was committed atomically:

1. **Task 1: Attack target picker and "Attack all" button** - `5d52ac1ce` (feat)
2. **Task 2: Multi-defender blocking UI** - `cc8feb939` (feat)

## Files Created/Modified
- `client/src/components/controls/AttackTargetPicker.tsx` - Modal component for multiplayer attack target selection
- `client/src/utils/combat.ts` - Added getValidAttackTargets, hasMultipleAttackTargets, exported getDefaultAttackTarget
- `client/src/components/board/ActionButton.tsx` - Integrated AttackTargetPicker for multiplayer games
- `client/src/components/combat/CombatOverlay.tsx` - Integrated AttackTargetPicker for multiplayer games
- `client/src/components/board/BlockAssignmentLines.tsx` - Multi-defender dimming for block lines
- `client/src/viewmodel/battlefieldProps.ts` - Added groupAttackersByTarget, getAttackersTargeting, isAttackerTargetingPlayer

## Decisions Made
- AttackTargetPicker uses a modal overlay pattern with "Attack All" / "Split Attacks" toggle (keeps common case fast while supporting per-creature assignment)
- Picker only appears when valid_attack_targets has 2+ entries, preserving identical 2-player behavior
- Block assignment lines use opacity dimming (not hiding) for attackers targeting other players, maintaining spatial awareness
- No additional client-side blocker filtering needed -- the engine's valid_block_targets already restricts assignments to relevant attackers

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Full N-player combat UI complete (attack target selection + blocking)
- Ready for lobby UI (Plan 29-10) and deck builder updates (Plan 29-12)
- All 226 frontend tests pass, type checking clean

---
*Phase: 29-support-n-players*
*Completed: 2026-03-11*
