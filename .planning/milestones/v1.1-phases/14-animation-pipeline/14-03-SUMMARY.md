---
phase: 14-animation-pipeline
plan: 03
subsystem: ui
tags: [framer-motion, canvas, particles, screen-shake, vfx, animation]

requires:
  - phase: 14-01
    provides: "Animation types, SPEED_MULTIPLIERS, VfxQuality, wubrgColors"
  - phase: 14-02
    provides: "Animation step queue and dispatch pipeline"
provides:
  - "ParticleCanvas with glow, gravity, variable radius, quality gating"
  - "FloatingNumber with speedMultiplier and scale-in animation"
  - "applyScreenShake CSS transform function with 3 intensity levels"
  - "DamageVignette red radial gradient overlay"
  - "TurnBanner slide-through announcement component"
  - "CardRevealBurst scale + WUBRG particle burst component"
affects: [14-04-animation-overlay]

tech-stack:
  added: []
  patterns: ["VFX quality gating pattern (full/reduced/minimal)", "rAF-based CSS transform animation for screen shake"]

key-files:
  created:
    - client/src/components/animation/ScreenShake.tsx
    - client/src/components/animation/DamageVignette.tsx
    - client/src/components/animation/TurnBanner.tsx
    - client/src/components/animation/CardRevealBurst.tsx
    - client/src/components/animation/__tests__/FloatingNumber.test.tsx
    - client/src/components/animation/__tests__/screenShake.test.ts
  modified:
    - client/src/components/animation/ParticleCanvas.tsx
    - client/src/components/animation/FloatingNumber.tsx

key-decisions:
  - "VFX quality reads via getState() (non-reactive) in ParticleCanvas for performance"
  - "ScreenShake is a plain function, not a React component — applies CSS transform via rAF"
  - "ParticleCanvas halves count internally for reduced quality, keeping logic centralized"

patterns-established:
  - "VFX quality gating: full=everything, reduced=no glow/vignette/shake + halved particles, minimal=floating numbers + text only"
  - "Speed multiplier applied to all animation durations for consistent pacing"

requirements-completed: [VFX-02, VFX-03, VFX-04, VFX-05, VFX-08]

duration: 3min
completed: 2026-03-09
---

# Phase 14 Plan 03: VFX Components Summary

**Six VFX components: enhanced particles with glow/gravity, floating numbers with speed scaling, screen shake with 3 intensities, damage vignette, turn banner, and card reveal burst — all quality-tiered**

## Performance

- **Duration:** 3 min
- **Started:** 2026-03-09T03:34:17Z
- **Completed:** 2026-03-09T03:37:00Z
- **Tasks:** 2
- **Files modified:** 8

## Accomplishments
- Enhanced ParticleCanvas with variable radius (2-6px), gravity pull, and glow (shadowBlur, full quality only)
- FloatingNumber now accepts speedMultiplier prop and has scale-in animation (1.2 -> 1.0)
- Created applyScreenShake rAF function with light/medium/heavy intensity levels and decaying sine wave
- Created DamageVignette with red radial gradient, opacity scaled by damage amount (0.2-0.8)
- Created TurnBanner with slide-in/pause/slide-out animation and player turn text
- Created CardRevealBurst with scale 0.8->1.0 and WUBRG particle burst via ParticleCanvas ref
- All six components respect VFX quality tiers and animation speed multiplier

## Task Commits

Each task was committed atomically:

1. **Task 1: Enhance ParticleCanvas and FloatingNumber** - `3ec257b` (feat)
2. **Task 2 RED: Failing screen shake tests** - `60fb5c9` (test)
3. **Task 2 GREEN: Create ScreenShake, DamageVignette, TurnBanner, CardRevealBurst** - `9183e63` (feat)

## Files Created/Modified
- `client/src/components/animation/ParticleCanvas.tsx` - Enhanced with glow, gravity, variable radius, quality gating
- `client/src/components/animation/FloatingNumber.tsx` - Added speedMultiplier prop and scale-in animation
- `client/src/components/animation/ScreenShake.tsx` - CSS transform rAF animation with 3 intensity levels
- `client/src/components/animation/DamageVignette.tsx` - Red radial gradient overlay on player damage
- `client/src/components/animation/TurnBanner.tsx` - Turn announcement banner with slide animation
- `client/src/components/animation/CardRevealBurst.tsx` - Scale + particle burst on card battlefield entry
- `client/src/components/animation/__tests__/FloatingNumber.test.tsx` - Render and speed multiplier tests
- `client/src/components/animation/__tests__/screenShake.test.ts` - Config lookup and duration scaling tests

## Decisions Made
- VFX quality reads via `getState()` (non-reactive) in ParticleCanvas for performance — avoids re-renders on quality changes
- ScreenShake is a plain function, not a React component — applies CSS transform via requestAnimationFrame directly on an element
- ParticleCanvas halves particle count internally for reduced quality, keeping quality logic centralized in the particle system

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
- FloatingNumber tests initially failed due to stale DOM elements across test cases — fixed with `cleanup()` in `afterEach`

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- All six VFX components ready to be wired into AnimationOverlay in Plan 04
- ParticleCanvas, FloatingNumber, ScreenShake, DamageVignette, TurnBanner, CardRevealBurst all export clean APIs
- Quality tiers and speed multiplier patterns established for consistent use in overlay integration

---
*Phase: 14-animation-pipeline*
*Completed: 2026-03-09*
