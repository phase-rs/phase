---
phase: 07-platform-bridges-ui
plan: 05
subsystem: ui
tags: [react, framer-motion, zustand, targeting, mana, modal, overlay]

requires:
  - phase: 07-platform-bridges-ui/03
    provides: "Game board with PermanentCard, battlefield layout, uiStore"
provides:
  - "TargetingOverlay with cyan glow rings and SVG arrows"
  - "ManaPaymentUI with auto-tap and manual land override"
  - "ChoiceModal for mulligan and modal decisions"
  - "ReplacementModal for ordering competing replacement effects"
  - "GamePage wiring for all WaitingFor prompt variants"
affects: [07-06, 07-08]

tech-stack:
  added: []
  patterns: [WaitingFor-driven overlay rendering, uiStore validTargetIds for cross-component glow state]

key-files:
  created:
    - client/src/components/targeting/TargetArrow.tsx
    - client/src/components/targeting/TargetingOverlay.tsx
    - client/src/components/mana/ManaBadge.tsx
    - client/src/components/mana/ManaPaymentUI.tsx
    - client/src/components/modal/ChoiceModal.tsx
    - client/src/components/modal/ReplacementModal.tsx
  modified:
    - client/src/stores/uiStore.ts
    - client/src/components/board/PermanentCard.tsx
    - client/src/pages/GamePage.tsx

key-decisions:
  - "uiStore extended with validTargetIds/sourceObjectId for targeting glow state"
  - "data-object-id attribute on PermanentCard for DOM position lookups"
  - "Engine validates target selection; client highlights all battlefield objects as valid"
  - "MulliganBottomCards and GameOver as inline components in GamePage"

patterns-established:
  - "WaitingFor switch: GamePage renders overlay per waitingFor.type"
  - "Targeting flow: engine sets WaitingFor -> uiStore targeting mode -> PermanentCard glow -> dispatch SelectTargets"

requirements-completed: [UI-06, UI-07, UI-09]

duration: 4min
completed: 2026-03-08
---

# Phase 7 Plan 5: Interactive Game Prompts Summary

**Targeting overlay with cyan glow rings and SVG arrows, mana payment with manual land tap override, Arena-style choice/replacement modals, and GamePage WaitingFor wiring**

## Performance

- **Duration:** 4 min
- **Started:** 2026-03-08T07:50:47Z
- **Completed:** 2026-03-08T07:54:35Z
- **Tasks:** 2
- **Files modified:** 9

## Accomplishments
- TargetingOverlay activates on WaitingFor::TargetSelection with cyan glow rings on valid targets and SVG arrows from source to selected target
- ManaPaymentUI shows mana pool summary with manual land tapping override buttons
- ChoiceModal and ReplacementModal provide Arena-style centered overlays for decisions
- GamePage wires all 9 WaitingFor variants (Priority, MulliganDecision, MulliganBottomCards, ManaPayment, TargetSelection, DeclareAttackers, DeclareBlockers, ReplacementChoice, GameOver) to appropriate prompt components

## Task Commits

Each task was committed atomically:

1. **Task 1: Targeting overlay with glow rings, arrows, and auto-target** - `6a5212b` (feat)
2. **Task 2: Mana payment UI, choice modal, replacement modal, and GamePage wiring** - `15d9700` (feat)

## Files Created/Modified
- `client/src/components/targeting/TargetArrow.tsx` - SVG arrow with framer-motion draw animation
- `client/src/components/targeting/TargetingOverlay.tsx` - Targeting mode overlay with valid target highlighting
- `client/src/components/mana/ManaBadge.tsx` - Colored mana circle badge
- `client/src/components/mana/ManaPaymentUI.tsx` - Mana payment panel with auto-tap and manual override
- `client/src/components/modal/ChoiceModal.tsx` - Generic Arena-style choice modal
- `client/src/components/modal/ReplacementModal.tsx` - Replacement effect ordering modal
- `client/src/stores/uiStore.ts` - Added validTargetIds, sourceObjectId for targeting state
- `client/src/components/board/PermanentCard.tsx` - Added valid-target glow and data-object-id attribute
- `client/src/pages/GamePage.tsx` - WaitingFor-driven prompt rendering for all variants

## Decisions Made
- Extended uiStore with validTargetIds and sourceObjectId rather than reading gameState directly in PermanentCard -- keeps targeting glow decoupled from engine state shape
- Added data-object-id DOM attribute to PermanentCard for arrow position lookups via querySelector
- Client highlights all battlefield objects as valid targets; engine validates on submission (plan-specified approach)
- MulliganBottomCards and GameOver implemented as inline components in GamePage rather than separate files (small and tightly coupled to page logic)

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] GamePage had been restructured by 07-04**
- **Found during:** Task 2
- **Issue:** GamePage had been updated by plan 07-04 with side panel layout, not the original simple layout
- **Fix:** Preserved the 07-04 layout and added WaitingFor overlays on top
- **Files modified:** client/src/pages/GamePage.tsx
- **Verification:** Build passes, existing layout preserved

---

**Total deviations:** 1 auto-fixed (1 blocking)
**Impact on plan:** Necessary adaptation to preserve prior plan's work. No scope creep.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- All interactive game prompts are in place
- DeclareAttackers and DeclareBlockers WaitingFor variants render via existing game board (no separate overlay needed)
- Ready for 07-06 (animations) and 07-08 (integration testing)

---
*Phase: 07-platform-bridges-ui*
*Completed: 2026-03-08*
