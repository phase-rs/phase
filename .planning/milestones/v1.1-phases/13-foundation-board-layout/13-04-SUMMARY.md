---
phase: 13-foundation-board-layout
plan: 04
subsystem: ui
tags: [react, framer-motion, drag-to-play, hud, mana-pool, tailwind]

requires:
  - phase: 13-01
    provides: view model functions, preferencesStore with hudLayout
  - phase: 13-02
    provides: CSS card sizing variables, eventHistory

provides:
  - MTGA-style player hand with fan layout, drag-to-play, green glow highlighting
  - PlayerHud and OpponentHud components with inline/floating layout support
  - ManaPoolSummary component with WUBRG colored pills
  - LifeTotal with red/green flash on damage/gain

affects: [13-05, game-page-layout]

tech-stack:
  added: []
  patterns: [drag-to-play with threshold, touch tap-select-play flow, HUD layout toggle]

key-files:
  created:
    - client/src/components/hud/ManaPoolSummary.tsx
    - client/src/components/hud/PlayerHud.tsx
    - client/src/components/hud/OpponentHud.tsx
  modified:
    - client/src/components/hand/PlayerHand.tsx
    - client/src/components/hand/OpponentHand.tsx
    - client/src/components/controls/LifeTotal.tsx

key-decisions:
  - "All hand cards highlighted as playable when player has priority (engine legal action filtering deferred)"
  - "HUD layout toggle between inline and floating via preferencesStore hudLayout"

patterns-established:
  - "Drag-to-play: 50px upward threshold triggers PlayLand or CastSpell"
  - "Touch flow: tap to expand hand, tap card to select, tap again to play"
  - "ManaPoolSummary: count-by-color with ordered WUBRG display"

requirements-completed: [HAND-01, HAND-02, HAND-03, HAND-04, HUD-01, HUD-02, HUD-03]

duration: 3min
completed: 2026-03-09
---

# Phase 13 Plan 04: Hand & HUD Summary

**MTGA-style hand fan with drag-to-play, green glow highlighting, and inline HUD with life flash and mana pool summary**

## Performance

- **Duration:** 3 min
- **Started:** 2026-03-09T00:59:35Z
- **Completed:** 2026-03-09T01:02:44Z
- **Tasks:** 2
- **Files modified:** 6

## Accomplishments
- Player hand rewritten as MTGA-style fan with rotation, expand-on-hover, and drag-to-play (50px threshold)
- Green glow on playable cards, dimmed non-playable, cyan ring on selected card
- Touch interaction: tap to expand, tap card to select, tap again to play, long-press for preview
- Opponent hand refined with 0.6x scale card backs for reduced visual weight
- PlayerHud and OpponentHud with inline/floating layout via preferencesStore
- ManaPoolSummary shows colored pills per mana type (WUBRG + Colorless)
- LifeTotal enhanced with red flash on damage, green flash on life gain

## Task Commits

Each task was committed atomically:

1. **Task 1: Player hand fan layout with drag-to-play and highlighting** - `564c599` (feat)
2. **Task 2: HUD components and LifeTotal flash enhancement** - `9b68093` (feat)

## Files Created/Modified
- `client/src/components/hand/PlayerHand.tsx` - MTGA-style fan with drag-to-play, rotation, green glow, touch support
- `client/src/components/hand/OpponentHand.tsx` - Compact card backs at 0.6x scale
- `client/src/components/hud/ManaPoolSummary.tsx` - Compact mana pool display with WUBRG colors
- `client/src/components/hud/PlayerHud.tsx` - Inline HUD with life, mana pool, phase indicator, settings gear
- `client/src/components/hud/OpponentHud.tsx` - Opponent HUD with life and mana pool
- `client/src/components/controls/LifeTotal.tsx` - Enhanced with red/green flash on life change

## Decisions Made
- All hand cards highlighted as playable when player has priority (legal action filtering needs engine support, deferred)
- HUD layout toggleable between inline and floating via preferencesStore hudLayout setting
- Settings gear button renders as placeholder (will connect to preferences modal in future plan)

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
- Hooks called after early return caused React rules-of-hooks lint error -- moved early return after all hook calls

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- Hand and HUD components ready for integration into GamePage layout
- PlayerHud/OpponentHud can be wired into GamePage sidebar or as floating overlays
- Settings gear button ready for preferences modal connection

---
*Phase: 13-foundation-board-layout*
*Completed: 2026-03-09*
