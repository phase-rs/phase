---
phase: 19-recreate-the-mtga-ui-as-faithfully-as-possible
plan: 03
subsystem: ui
tags: [react, tailwind, mtga, layout, game-board, hud, phase-indicators]

requires:
  - phase: 19-01
    provides: art-crop card rendering and image infrastructure

provides:
  - MTGA-faithful zone ordering (creatures near center, lands far)
  - Centered player/opponent avatars with flanking phase indicators
  - Split PhaseStopBar into PhaseIndicatorLeft, PhaseIndicatorRight, CombatPhaseIndicator
  - Bottom-left zone indicator cluster (graveyard/exile for both players)

affects: [19-04, 19-05, 19-06, 19-07, 19-08]

tech-stack:
  added: []
  patterns:
    - "Split phase indicators flanking centered avatar pill"
    - "Fixed-position zone indicators instead of inline center divider"

key-files:
  created: []
  modified:
    - client/src/components/board/GameBoard.tsx
    - client/src/pages/GamePage.tsx
    - client/src/components/hud/PlayerHud.tsx
    - client/src/components/hud/OpponentHud.tsx
    - client/src/components/controls/PhaseStopBar.tsx
    - client/src/components/settings/PreferencesModal.tsx

key-decisions:
  - "HUD always centered (removed floating option) — MTGA does not have a floating HUD"
  - "CombatPhaseIndicator placed near ActionButton (fixed right side) per MTGA layout"
  - "HudLayout type and store field preserved for backward compat of persisted prefs"

patterns-established:
  - "Phase indicators split into Left/Right/Combat: PhaseIndicatorLeft (UP/DR/M1), PhaseIndicatorRight (M2/EN), CombatPhaseIndicator"
  - "Zone indicators use fixed bottom-left positioning instead of inline center divider"

requirements-completed: [ARENA-04, ARENA-05]

duration: 3min
completed: 2026-03-09
---

# Phase 19 Plan 03: Board & HUD Layout Summary

**MTGA-faithful board layout with creatures near center, centered avatar pills, split phase indicators flanking player HUD, and borderless floating zones**

## Performance

- **Duration:** 3 min
- **Started:** 2026-03-09T22:59:04Z
- **Completed:** 2026-03-09T23:02:26Z
- **Tasks:** 2
- **Files modified:** 6

## Accomplishments
- Board zone order reversed: creatures near center, lands far from center (both sides)
- Center divider removed: no borders, bars, or dividers anywhere on the board
- Player avatar centered between flanking phase indicators (Upkeep/Draw/Main1 left, Main2/End right)
- Opponent avatar centered below opponent hand
- Zone indicators (graveyard/exile) moved to fixed bottom-left cluster
- Combat phase indicator placed near ActionButton on the right side

## Task Commits

Each task was committed atomically:

1. **Task 1: Restructure GameBoard zone ordering + update GamePage layout** - `62a5f02` (feat)
2. **Task 2: Center HUD avatars + split PhaseStopBar into flanking indicators** - `2702e67` (feat)

## Files Created/Modified
- `client/src/components/board/GameBoard.tsx` - Reversed zone order: other/lands/creatures (top) and creatures/lands/other (bottom)
- `client/src/pages/GamePage.tsx` - Restructured layout, removed center divider, added fixed zone indicators and combat phase indicator
- `client/src/components/hud/PlayerHud.tsx` - Centered avatar pill with PhaseIndicatorLeft/Right flanking
- `client/src/components/hud/OpponentHud.tsx` - Centered avatar pill, removed hudLayout floating option
- `client/src/components/controls/PhaseStopBar.tsx` - Split into PhaseIndicatorLeft, PhaseIndicatorRight, CombatPhaseIndicator
- `client/src/components/settings/PreferencesModal.tsx` - Removed HUD Layout toggle (no longer relevant)

## Decisions Made
- HUD always centered (removed floating option) since MTGA does not offer a floating HUD mode
- CombatPhaseIndicator placed in fixed position near ActionButton on right side, matching MTGA combat phase display
- HudLayout type and store field preserved in preferencesStore for backward compatibility of persisted preferences

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Removed HUD Layout toggle from PreferencesModal**
- **Found during:** Task 2
- **Issue:** PreferencesModal imported HudLayout type and rendered a HUD Layout toggle, but hudLayout is no longer used by HUD components
- **Fix:** Removed HudLayout import, hudLayout state usage, setHudLayout usage, and the setting group from the modal
- **Files modified:** client/src/components/settings/PreferencesModal.tsx
- **Verification:** pnpm run type-check passes
- **Committed in:** 2702e67 (Task 2 commit)

---

**Total deviations:** 1 auto-fixed (1 blocking)
**Impact on plan:** Necessary cleanup to remove dead UI control. No scope creep.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Board layout is MTGA-faithful, ready for card styling (19-02) and animation (19-04) changes
- Split phase indicator components available for any future per-component customization

---
*Phase: 19-recreate-the-mtga-ui-as-faithfully-as-possible*
*Completed: 2026-03-09*
