---
phase: 19-recreate-the-mtga-ui-as-faithfully-as-possible
plan: 05
subsystem: ui
tags: [framer-motion, canvas, animation, particles, react]

requires:
  - phase: 14-animation-pipeline
    provides: "AnimationOverlay, ParticleCanvas, animation step processing"
  - phase: 19-01
    provides: "Art-crop image infrastructure, useCardImage hook, fetchCardImageUrl"
provides:
  - "Cinematic layered TurnBanner with amber/slate theming"
  - "Canvas-based DeathShatter card fragment effect"
  - "CastArcAnimation for spell cast and resolve flight arcs"
  - "AnimationOverlay wiring for creature death shatter and cast arcs"
affects: [19-06, 19-07, 19-08]

tech-stack:
  added: []
  patterns:
    - "Canvas-based fragment animation with requestAnimationFrame loop"
    - "Imperative fetchCardImageUrl for async image resolution in effect handlers"
    - "VFX quality tiers: full (all layers), reduced (subset), minimal (text-only)"

key-files:
  created:
    - client/src/components/animation/DeathShatter.tsx
    - client/src/components/animation/CastArcAnimation.tsx
  modified:
    - client/src/components/animation/TurnBanner.tsx
    - client/src/components/animation/AnimationOverlay.tsx

key-decisions:
  - "DeathShatter uses canvas with 3x4 fragment grid for 12 organic-looking pieces"
  - "CastArcAnimation uses Framer Motion keyframe arrays for parabolic arc trajectory"
  - "Death shatter fetches art_crop image imperatively via fetchCardImageUrl, falls back to death clone on failure"
  - "Stack position estimated at 75% viewport width, 40% height for cast arc endpoints"

patterns-established:
  - "Canvas fragment shatter: offscreen canvas draw + per-fragment clip + rAF loop with gravity/rotation/alpha"
  - "Imperative image fetch in animation effect handlers to avoid hook constraints"

requirements-completed: [ARENA-07, ARENA-08]

duration: 3min
completed: 2026-03-09
---

# Phase 19 Plan 05: Cinematic Animations Summary

**Cinematic layered turn banner, canvas-based death shatter, and card flight arc animations with VFX quality tiers**

## Performance

- **Duration:** 3 min
- **Started:** 2026-03-09T22:59:12Z
- **Completed:** 2026-03-09T23:02:53Z
- **Tasks:** 2
- **Files modified:** 4

## Accomplishments
- TurnBanner rewritten with 6-phase cinematic animation: light burst, banner strip, diamond accents, triple-glow text, hold pulse, exit slide
- DeathShatter canvas component shatters card art into 12 fragments with outward velocity, gravity, rotation, red tint flash, and alpha fade
- CastArcAnimation handles three modes: cast (hand-to-stack arc), resolve-permanent (stack-to-battlefield arc), resolve-spell (fade out)
- AnimationOverlay wires DeathShatter for CreatureDestroyed events and CastArcAnimation for SpellCast and ZoneChanged events

## Task Commits

Each task was committed atomically:

1. **Task 1: Cinematic TurnBanner + DeathShatter effect** - `baf9f77` (feat)
2. **Task 2: CastArcAnimation + wire new effects into AnimationOverlay** - `7006121` (feat)

## Files Created/Modified
- `client/src/components/animation/TurnBanner.tsx` - Cinematic layered turn banner with amber/slate theming and 3 VFX quality tiers
- `client/src/components/animation/DeathShatter.tsx` - Canvas-based card fragment shatter with gravity, rotation, and red flash
- `client/src/components/animation/CastArcAnimation.tsx` - Card flight arc animation for cast, resolve-permanent, and resolve-spell modes
- `client/src/components/animation/AnimationOverlay.tsx` - Wires DeathShatter and CastArcAnimation into animation pipeline

## Decisions Made
- DeathShatter uses 3x4 grid (12 fragments) with random perturbation for organic look, matching plan's 8-12 target
- CastArcAnimation uses Framer Motion keyframe arrays rather than a separate animation library for consistency
- Stack position approximated at 75% viewport width for cast arc endpoints (matches right-center stack display)
- Death shatter falls back to existing death clone overlay when art_crop image fetch fails

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- All signature MTGA animations implemented and wired
- Ready for subsequent plans in Phase 19 (board layout, menu/lobby, etc.)

---
*Phase: 19-recreate-the-mtga-ui-as-faithfully-as-possible*
*Completed: 2026-03-09*
