---
phase: 19-recreate-the-mtga-ui-as-faithfully-as-possible
plan: 02
subsystem: ui
tags: [react, art-crop, framer-motion, battlefield, preferences]

requires:
  - phase: 19-01
    provides: ArtCropCard component with art_crop image loading and CSS variables

provides:
  - Battlefield rendering with ArtCropCard for MTGA-style art crops
  - Preference-driven tap rotation (17deg MTGA / 90deg classic)
  - Instant card preview without animation
  - Full-bleed battlefield backgrounds without darkening overlay

affects: [19-03, 19-04, 19-05]

tech-stack:
  added: []
  patterns:
    - "Conditional rendering branches for art_crop vs full_card display modes"
    - "Preference-driven animation parameters (tapAngle, tapOpacity)"

key-files:
  created: []
  modified:
    - client/src/components/board/PermanentCard.tsx
    - client/src/components/board/BattlefieldRow.tsx
    - client/src/components/card/CardPreview.tsx
    - client/src/components/board/BattlefieldBackground.tsx

key-decisions:
  - "ArtCropCard renders P/T, loyalty, counters internally — PermanentCard skips those in art_crop mode"
  - "Tap opacity 0.85 only in MTGA mode for tapped non-attacking permanents"
  - "CardPreview uses normal size instead of large for faster loading via cache hits"

patterns-established:
  - "Display mode branching: useArtCrop boolean gates full rendering path"
  - "Preference-driven CSS variables: --art-crop-h vs --card-h for row min-heights"

requirements-completed: [ARENA-03, ARENA-05]

duration: 2min
completed: 2026-03-09
---

# Phase 19 Plan 02: Battlefield Rendering Summary

**ArtCropCard wired into battlefield with 17deg tap rotation, instant card preview, and full-bleed backgrounds**

## Performance

- **Duration:** 2 min
- **Started:** 2026-03-09T22:59:04Z
- **Completed:** 2026-03-09T23:01:00Z
- **Tasks:** 2
- **Files modified:** 4

## Accomplishments
- Battlefield permanents conditionally render ArtCropCard (MTGA) or CardImage (classic) based on preference
- Tapped cards rotate 17deg with 0.85 opacity dim in MTGA mode, 90deg with no dim in classic
- CardPreview appears/disappears instantly with no framer-motion animation
- Battlefield background displays full-bleed images without bg-black/40 overlay

## Task Commits

Each task was committed atomically:

1. **Task 1: Wire ArtCropCard into PermanentCard + change tap rotation** - `da3f562` (feat)
2. **Task 2: Make CardPreview instant + remove battlefield background overlay** - `3754d4f` (feat)

## Files Created/Modified
- `client/src/components/board/PermanentCard.tsx` - Conditional ArtCropCard/CardImage rendering, preference-driven tap rotation
- `client/src/components/board/BattlefieldRow.tsx` - Dynamic min-h based on battlefieldCardDisplay preference
- `client/src/components/card/CardPreview.tsx` - Removed AnimatePresence/motion for instant preview
- `client/src/components/board/BattlefieldBackground.tsx` - Removed bg-black/40 dark overlay

## Decisions Made
- ArtCropCard handles P/T, loyalty, counters, damage overlays internally, so PermanentCard skips those in art_crop mode
- Tap opacity 0.85 only applies in MTGA mode for tapped non-attacking permanents (attacking cards stay full opacity)
- CardPreview switched from "large" to "normal" image size since hand cards already cache normal, reducing load times

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Battlefield now renders MTGA-style art crops with proper tap rotation
- Ready for 19-03 (hand card rendering) and 19-04 (combat zones)
- CardPreview instant behavior ready for hover interactions

## Self-Check: PASSED

- All 4 modified files exist on disk
- Both task commits verified (da3f562, 3754d4f)
- PermanentCard imports ArtCropCard: PASS
- tapAngle used in PermanentCard animate: PASS
- CardPreview has no AnimatePresence: PASS
- BattlefieldBackground has no bg-black/40: PASS
- TypeScript type-check passes: PASS

---
*Phase: 19-recreate-the-mtga-ui-as-faithfully-as-possible*
*Completed: 2026-03-09*
