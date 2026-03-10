---
phase: 17-mtg-specific-ui
plan: 02
subsystem: ui
tags: [react, framer-motion, tailwind, stack, mtg, arena]

requires:
  - phase: 13-foundation-board-layout
    provides: CardImage component and game store infrastructure
provides:
  - Arena-style staggered stack visualization with full card images
  - Dynamic card sizing for variable stack depths
  - Fixed right-column stack overlay with glassmorphism
affects: [17-mtg-specific-ui]

tech-stack:
  added: []
  patterns: [fixed-position overlay panels, staggered pile layout, dynamic sizing based on item count]

key-files:
  created: []
  modified:
    - client/src/components/stack/StackDisplay.tsx
    - client/src/components/stack/StackEntry.tsx
    - client/src/pages/GamePage.tsx

key-decisions:
  - "StackDisplay moved from inline center-divider to fixed right-column overlay for Arena-style presentation"
  - "Stagger offsets (28px Y, 3px X) chosen to show card art while maintaining pile illusion"
  - "Card sizing uses linear scale reduction: max(0.5, 1 - max(0, count-2) * 0.083) for smooth shrinking"

patterns-established:
  - "Fixed overlay pattern: self-managing visibility (return null when empty) with AnimatePresence container slide"

requirements-completed: [STACK-01]

duration: 2min
completed: 2026-03-09
---

# Phase 17 Plan 02: Stack Display Summary

**Arena-style staggered card pile with full Scryfall images, dynamic sizing, and "Resolves Next" badge**

## Performance

- **Duration:** 2 min
- **Started:** 2026-03-09T17:47:06Z
- **Completed:** 2026-03-09T17:48:37Z
- **Tasks:** 1
- **Files modified:** 3

## Accomplishments
- StackDisplay upgraded from flat text list to staggered card pile with glassmorphism container
- Full Scryfall card images rendered per stack entry with dynamic width/height scaling
- "Resolves Next" amber badge on top stack item
- Ability overlay badges for activated/triggered abilities with source card name
- Controller indicators (P1/P2) on each stack entry

## Task Commits

Each task was committed atomically:

1. **Task 1: Upgrade StackDisplay to right-column staggered card pile** - `0b62212` (feat)

## Files Created/Modified
- `client/src/components/stack/StackDisplay.tsx` - Fixed right-column staggered pile with dynamic sizing and glassmorphism
- `client/src/components/stack/StackEntry.tsx` - Full card image entry with badges for resolution order, abilities, and controller
- `client/src/pages/GamePage.tsx` - Moved StackDisplay to top-level fixed overlay position

## Decisions Made
- StackDisplay moved from inline center-divider to fixed right-column overlay for Arena-style presentation
- Stack array reversed for display (engine stack[0] = oldest, display shows newest on top)
- Stagger offsets (28px Y, 3px X) balance showing card art with pile depth illusion
- Card sizing uses linear scale: max(0.5, 1 - max(0, count-2) * 0.083) starting shrink at 3+ items

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Stack visualization complete, ready for remaining phase 17 plans
- CardImage integration reuses existing Scryfall image pipeline

---
*Phase: 17-mtg-specific-ui*
*Completed: 2026-03-09*
