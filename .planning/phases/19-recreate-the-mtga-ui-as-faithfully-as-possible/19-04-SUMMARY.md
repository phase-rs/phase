---
phase: 19-recreate-the-mtga-ui-as-faithfully-as-possible
plan: 04
subsystem: ui
tags: [svg, bezier, framer-motion, tailwind, targeting, combat, animation]

requires:
  - phase: 19-02
    provides: MTGA card display mode with art crops and tap rotation
  - phase: 19-03
    provides: MTGA HUD layout and combat phase indicator

provides:
  - Golden curved SVG bezier targeting arcs with glow filter
  - Orange combat selection glow for attackers and blockers
  - Golden valid target glow during target selection
  - Attacker slide-forward animation toward board center

affects: [19-06]

tech-stack:
  added: []
  patterns: [SVG quadratic bezier arcs, SVG glow filter with feMerge, directional slide animation by controller]

key-files:
  created: []
  modified:
    - client/src/components/targeting/TargetArrow.tsx
    - client/src/components/board/PermanentCard.tsx

key-decisions:
  - "Blocker glow matches attacker glow (both orange) per MTGA convention"
  - "Valid target glow uses amber-400 with gold rgba for Arena-style golden highlight"
  - "Attack slide direction determined by controller ID (0=up, 1=down)"

patterns-established:
  - "SVG filter pattern: feGaussianBlur + feMerge for glow effects on paths"
  - "Directional animation based on controller: obj.controller === 0 for player"

requirements-completed: [ARENA-06]

duration: 2min
completed: 2026-03-09
---

# Phase 19 Plan 04: Targeting & Combat Visuals Summary

**Golden bezier targeting arcs with SVG glow, orange attacker/blocker combat selection, and slide-forward attack animation**

## Performance

- **Duration:** 2 min
- **Started:** 2026-03-09T23:07:52Z
- **Completed:** 2026-03-09T23:09:17Z
- **Tasks:** 2
- **Files modified:** 2

## Accomplishments
- Replaced straight cyan targeting lines with golden quadratic Bezier curves featuring SVG glow filter
- Unified attacker and blocker glow to orange, matching MTGA combat selection style
- Changed valid target glow from cyan to golden/amber
- Added 30px slide-forward animation for declared attackers toward board center

## Task Commits

Each task was committed atomically:

1. **Task 1: Golden curved targeting arcs** - `0443735` (feat)
2. **Task 2: Orange attacker/blocker glow + attacker slide-forward** - `9c63308` (feat)

## Files Created/Modified
- `client/src/components/targeting/TargetArrow.tsx` - Rewrote to use quadratic Bezier curves with gold color, SVG glow filter, and animated pathLength
- `client/src/components/board/PermanentCard.tsx` - Changed blocker glow to orange, valid target glow to golden, added y-translation for attacking creatures

## Decisions Made
- Blocker glow uses same orange as attacker glow (MTGA uses identical orange for both combat selections)
- Valid target glow uses amber-400/60 with rgba(201,176,55,0.8) to match the gold color (#C9B037) used in targeting arcs
- Attack slide direction is -30px (up) for player (controller 0) and +30px (down) for opponent (controller 1)

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Targeting and combat visuals now match MTGA style
- Ready for plan 06 (remaining Arena UI work)

## Self-Check: PASSED

All files exist. All commits verified.

---
*Phase: 19-recreate-the-mtga-ui-as-faithfully-as-possible*
*Completed: 2026-03-09*
