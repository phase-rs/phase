---
phase: 19-recreate-the-mtga-ui-as-faithfully-as-possible
plan: "07"
subsystem: ui
tags: [react, framer-motion, canvas, menu, splash-screen, deck-gallery]

requires:
  - phase: 19-01
    provides: "art_crop image support, useCardImage hook with size option"
provides:
  - "SplashScreen component with logo and loading bar"
  - "MenuParticles canvas particle background"
  - "DeckGallery with Scryfall art tiles, color dots, difficulty selector"
  - "Mode-first MenuPage flow (Play vs AI, Play Online, Deck Builder)"
affects: [19-08]

tech-stack:
  added: []
  patterns: ["Canvas-based particle animation with requestAnimationFrame", "Mode-first menu state machine"]

key-files:
  created:
    - client/src/components/splash/SplashScreen.tsx
    - client/src/components/menu/MenuParticles.tsx
    - client/src/components/menu/DeckGallery.tsx
  modified:
    - client/src/pages/MenuPage.tsx
    - client/src/App.tsx

key-decisions:
  - "Splash progress is cosmetic (1.5s rAF timer) since WASM loads on game start, not app start"
  - "DeckGallery uses first non-basic-land card name from deck for representative art tile"
  - "Difficulty selector is inline segmented control in DeckGallery, not a separate screen"

patterns-established:
  - "Canvas particles: create/animate/cleanup pattern with resize handler"
  - "Mode-first flow: state machine in MenuPage routes to sub-views"

requirements-completed: [ARENA-10, ARENA-12]

duration: 2min
completed: 2026-03-09
---

# Phase 19 Plan 07: Menu & Splash Overhaul Summary

**Mode-first menu flow with splash screen, canvas particle background, and art-tile deck gallery using Scryfall art_crop images**

## Performance

- **Duration:** 2min
- **Started:** 2026-03-09T22:59:19Z
- **Completed:** 2026-03-09T23:01:30Z
- **Tasks:** 2
- **Files modified:** 5

## Accomplishments
- Splash screen with Forge.rs logo, animated loading bar, and fade-out transition on app launch
- Mode-first menu flow: Play vs AI, Play Online, Deck Builder as primary choices
- Deck gallery with Scryfall art_crop tiles, color dots, card counts, and inline AI difficulty selector
- Canvas-based animated particle background with indigo/cyan/amber floating particles

## Task Commits

Each task was committed atomically:

1. **Task 1: SplashScreen + MenuParticles + DeckGallery components** - `214b1d7` (feat)
2. **Task 2: Overhaul MenuPage to mode-first flow with splash integration** - `c7bdff8` (feat)

## Files Created/Modified
- `client/src/components/splash/SplashScreen.tsx` - Logo splash screen with progress bar and fade-out
- `client/src/components/menu/MenuParticles.tsx` - Canvas particle background with 50 floating particles
- `client/src/components/menu/DeckGallery.tsx` - Deck selection gallery with art tiles, difficulty control
- `client/src/pages/MenuPage.tsx` - Overhauled to mode-first flow with particle background
- `client/src/App.tsx` - Integrated SplashScreen with simulated loading progress

## Decisions Made
- Splash progress is cosmetic (1.5s rAF timer) since WASM loads when entering a game, not at app startup
- DeckGallery uses the first non-basic-land card name from a deck for representative art tile
- AI difficulty is an inline segmented control in the deck gallery, not a separate screen as before
- Last-used deck restored from localStorage via ACTIVE_DECK_KEY on DeckGallery mount

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Menu overhaul complete with all planned components
- Ready for plan 19-08 (final polish/integration)

## Self-Check: PASSED

- All 5 files verified on disk
- Both task commits verified: 214b1d7, c7bdff8
- Type-check passed after each task

---
*Phase: 19-recreate-the-mtga-ui-as-faithfully-as-possible*
*Completed: 2026-03-09*
