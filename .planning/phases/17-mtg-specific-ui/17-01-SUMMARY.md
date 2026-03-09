---
phase: 17-mtg-specific-ui
plan: 01
subsystem: ui
tags: [tailwind, framer-motion, zustand, mtg-phases, react-hooks]

requires:
  - phase: 13-foundation-board-layout
    provides: Board components, hand fan, HUD, BattlefieldRow
provides:
  - gameButtonClass helper with 7 tone variants and 4 sizes
  - usePhaseInfo hook mapping MTG 12-phase to 5-slot MTGA strip
  - getCardSize and getStackCardSize container-aware card sizing
  - Upgraded hand fan with 6-degree rotation and perspective
  - Upgraded board with zone visual hierarchy
  - Upgraded HUD with larger life totals and mana display
affects: [17-02-stack-display, 17-03-action-controls]

tech-stack:
  added: []
  patterns: [module-level-constants-for-zustand-selectors, tone-based-button-styling]

key-files:
  created:
    - client/src/components/ui/buttonStyles.ts
    - client/src/hooks/usePhaseInfo.ts
    - client/src/components/board/boardSizing.ts
  modified:
    - client/src/components/hand/PlayerHand.tsx
    - client/src/components/hand/OpponentHand.tsx
    - client/src/components/board/GameBoard.tsx
    - client/src/components/hud/PlayerHud.tsx
    - client/src/components/hud/OpponentHud.tsx
    - client/src/components/hud/ManaPoolSummary.tsx
    - client/src/components/controls/LifeTotal.tsx

key-decisions:
  - "usePhaseInfo groups MTG 12 phases into 5 display keys: draw, main1, combat, main2, end"
  - "LifeTotal accepts size prop for context-aware rendering (lg in HUD, default elsewhere)"
  - "OpponentHand upgraded from static divs to motion.div with AnimatePresence for fan animation"

patterns-established:
  - "gameButtonClass: tone-based styling utility for consistent button look across all controls"
  - "usePhaseInfo: centralized phase interpretation hook shared by HUD, action controls, phase tracker"
  - "boardSizing: container-aware card sizing with MTG aspect ratio (63/88)"

requirements-completed: [STACK-02, STACK-03, STACK-04]

duration: 3min
completed: 2026-03-09
---

# Phase 17 Plan 01: Foundation Utilities & Visual Polish Summary

**Shared button/phase/sizing utilities plus MTGA-quality hand fan, board hierarchy, and HUD upgrades**

## Performance

- **Duration:** 3 min
- **Started:** 2026-03-09T17:47:14Z
- **Completed:** 2026-03-09T17:50:00Z
- **Tasks:** 2
- **Files modified:** 10

## Accomplishments
- Created three shared utility modules (buttonStyles, usePhaseInfo, boardSizing) consumed by Plans 02 and 03
- Hand fan upgraded to 6-degree rotation with perspective transform, stagger animations, and deeper hover effects
- Board layout has opponent-side darkening, enlarged middle spacer with inner glow, and breathing room
- HUD components wrapped in background pill with larger life totals and glowing mana badges

## Task Commits

Each task was committed atomically:

1. **Task 1: Create buttonStyles, usePhaseInfo, and boardSizing utilities** - `ce2a407` (feat)
2. **Task 2: Upgrade hand fan, board layout, and HUD** - `4b13e4d` (feat)

## Files Created/Modified
- `client/src/components/ui/buttonStyles.ts` - gameButtonClass helper with tone/size variants
- `client/src/hooks/usePhaseInfo.ts` - MTG phase-to-MTGA-strip mapping hook
- `client/src/components/board/boardSizing.ts` - Container-aware card sizing with MTG aspect ratio
- `client/src/components/hand/PlayerHand.tsx` - 6-degree fan, perspective, stagger, deeper glow
- `client/src/components/hand/OpponentHand.tsx` - Matching fan with motion animations
- `client/src/components/board/GameBoard.tsx` - Zone hierarchy with darkened opponent side
- `client/src/components/hud/PlayerHud.tsx` - Background pill, uses usePhaseInfo, larger life
- `client/src/components/hud/OpponentHud.tsx` - Matching background pill and larger life
- `client/src/components/hud/ManaPoolSummary.tsx` - Larger badges with glow effect
- `client/src/components/controls/LifeTotal.tsx` - Added size prop for context-aware rendering

## Decisions Made
- usePhaseInfo groups MTG 12 phases into 5 display keys (draw, main1, combat, main2, end) for simplified MTGA-style strip
- LifeTotal accepts a `size` prop ("default" | "lg") to allow HUD-context-specific rendering without separate components
- OpponentHand migrated from static divs to framer-motion AnimatePresence for fan entry/exit animations

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 2 - Missing Critical] Added size prop to LifeTotal component**
- **Found during:** Task 2 (HUD upgrade)
- **Issue:** Plan called for larger life totals in HUD but LifeTotal had no size control
- **Fix:** Added `size` prop ("default" | "lg") to LifeTotal, used text-2xl for lg
- **Files modified:** client/src/components/controls/LifeTotal.tsx
- **Verification:** Type-check passes, all 187 tests pass
- **Committed in:** 4b13e4d (Task 2 commit)

---

**Total deviations:** 1 auto-fixed (1 missing critical)
**Impact on plan:** Minimal - added prop to existing component for the planned feature to work.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- buttonStyles.ts ready for ActionButton (Plan 03)
- usePhaseInfo ready for ActionButton phase-strip (Plan 03) and StackDisplay (Plan 02)
- boardSizing ready for StackDisplay card sizing (Plan 02)
- All type-checks and 187 tests pass

---
*Phase: 17-mtg-specific-ui*
*Completed: 2026-03-09*

## Self-Check: PASSED
