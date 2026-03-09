---
phase: 13-foundation-board-layout
plan: 03
subsystem: ui
tags: [react, typescript, battlefield, grouping, permanent, p-t-box, tailwind]

requires:
  - phase: 13-01
    provides: "View model layer (toCardProps, computePTDisplay, partitionByType, groupByName)"
provides:
  - "Arena-style P/T box with color-coded power/toughness"
  - "Permanent grouping with stacked display and count badges"
  - "Loyalty shield badge for planeswalkers"
  - "Refined attachment tuck rendering"
affects: [14-game-log, 15-responsive-layout]

tech-stack:
  added: []
  patterns: [view-model-consumption, grouped-permanent-display]

key-files:
  created:
    - client/src/components/board/PTBox.tsx
    - client/src/components/board/GroupedPermanent.tsx
  modified:
    - client/src/components/board/PermanentCard.tsx
    - client/src/components/board/GameBoard.tsx
    - client/src/components/board/BattlefieldRow.tsx

key-decisions:
  - "P/T box replaces damage overlay for creatures; non-creatures keep damage overlay"
  - "Counter badges moved to top-right to avoid overlap with P/T box at bottom-right"
  - "Attachment tuck uses 15px offset per attachment with marginTop reservation"

patterns-established:
  - "View model consumption: board components import from viewmodel/ for all derived display data"
  - "Grouped permanent pattern: expand/collapse for stacked same-name permanents"

requirements-completed: [BOARD-02, BOARD-03, BOARD-04, BOARD-06, BOARD-07, BOARD-08]

duration: 2min
completed: 2026-03-09
---

# Phase 13 Plan 03: Battlefield Grouping & P/T Display Summary

**Arena-style P/T box with color coding, permanent stacking with count badges, loyalty shield, and view-model-driven battlefield layout**

## Performance

- **Duration:** 2 min
- **Started:** 2026-03-09T00:59:38Z
- **Completed:** 2026-03-09T01:02:06Z
- **Tasks:** 2
- **Files modified:** 5

## Accomplishments
- PTBox component with Arena-style color-coded power/toughness (green buffed, red damaged/debuffed, white normal)
- GroupedPermanentDisplay with stacked shadow layers, count badge, and click-to-expand/collapse
- GameBoard restructured to use view model partitionByType and groupByName instead of inline logic
- BattlefieldRow updated to render grouped permanents instead of raw object IDs
- Loyalty shield badge for planeswalkers, counter badges repositioned to top-right

## Task Commits

Each task was committed atomically:

1. **Task 1: P/T box and enhanced PermanentCard** - `bcb8664` (feat)
2. **Task 2: Battlefield grouping and GameBoard restructure** - `3505f37` (feat)

## Files Created/Modified
- `client/src/components/board/PTBox.tsx` - Arena-style P/T display with color-coded power/toughness
- `client/src/components/board/GroupedPermanent.tsx` - Stacked permanent display with count badge and expand/collapse
- `client/src/components/board/PermanentCard.tsx` - Enhanced with PTBox, loyalty shield, repositioned counters, refined attachment tuck
- `client/src/components/board/GameBoard.tsx` - Restructured to use view model partitionByType and groupByName
- `client/src/components/board/BattlefieldRow.tsx` - Updated to render GroupedPermanent instead of raw objectIds

## Decisions Made
- P/T box replaces damage overlay for creatures; non-creatures retain the damage overlay for edge cases
- Counter badges moved to top-right to avoid visual overlap with P/T box at bottom-right
- Attachment tuck uses 15px offset per attachment with marginTop space reservation on host

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed React hook ordering violation in PermanentCard**
- **Found during:** Task 1
- **Issue:** useUiStore call for validTargetIds was placed after early return, violating Rules of Hooks
- **Fix:** Moved useUiStore(validTargetIds) selector before the early return
- **Files modified:** client/src/components/board/PermanentCard.tsx
- **Committed in:** bcb8664 (Task 1 commit)

---

**Total deviations:** 1 auto-fixed (1 bug fix)
**Impact on plan:** Essential fix for React hook rules compliance. No scope creep.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- Battlefield layout complete with grouping, P/T display, and attachment rendering
- Ready for HUD/controls overlay (Plan 04/05) and game log integration

---
*Phase: 13-foundation-board-layout*
*Completed: 2026-03-09*
