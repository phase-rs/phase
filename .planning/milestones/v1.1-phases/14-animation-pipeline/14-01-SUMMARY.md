---
phase: 14-animation-pipeline
plan: 01
subsystem: ui
tags: [animation, wubrg, zustand, vitest, typescript]

requires:
  - phase: 13-foundation-board-layout
    provides: preferencesStore with zustand persist pattern
provides:
  - AnimationStep and StepEffect types for step-based animation pipeline
  - WUBRG color mapping constants and getCardColors utility
  - Event normalizer translating GameEvent[] to AnimationStep[]
  - VfxQuality and AnimationSpeed preferences with localStorage persistence
affects: [14-02-step-queue, 14-03-vfx-components, 14-04-wiring]

tech-stack:
  added: []
  patterns: [event-normalization-grouping, step-based-animation-pipeline]

key-files:
  created:
    - client/src/animation/types.ts
    - client/src/animation/wubrgColors.ts
    - client/src/animation/eventNormalizer.ts
    - client/src/animation/__tests__/eventNormalizer.test.ts
    - client/src/animation/__tests__/wubrgColors.test.ts
  modified:
    - client/src/stores/preferencesStore.ts
    - client/src/stores/__tests__/preferencesStore.test.ts

key-decisions:
  - "Non-visual events defined as set of 12 event types skipped by normalizer"
  - "Groupable events (DamageDealt, CreatureDestroyed, PermanentSacrificed) merge consecutive same-type into one step"
  - "Merge types (ZoneChanged, LifeChanged) attach to preceding step rather than creating new ones"

patterns-established:
  - "Event normalizer pattern: skip/own-step/group/merge classification for GameEvent types"
  - "Animation step duration: max of constituent effect durations"

requirements-completed: [ANIM-02, ANIM-05, ANIM-06, VFX-01]

duration: 2min
completed: 2026-03-09
---

# Phase 14 Plan 01: Animation Pipeline Foundation Summary

**Event normalizer with grouping heuristics, WUBRG color mapping, and VFX/speed preferences in zustand store**

## Performance

- **Duration:** 2 min
- **Started:** 2026-03-09T03:25:02Z
- **Completed:** 2026-03-09T03:27:16Z
- **Tasks:** 2
- **Files modified:** 7

## Accomplishments
- Animation type system with VfxQuality, AnimationSpeed, SPEED_MULTIPLIERS, AnimationStep, StepEffect
- Event normalizer that groups 34 GameEvent variants into sequential AnimationSteps using skip/own-step/group/merge heuristics
- WUBRG color mapping with getCardColors utility for mana color to hex conversion
- Extended preferencesStore with vfxQuality (default: full) and animationSpeed (default: normal)

## Task Commits

Each task was committed atomically:

1. **Task 1: Animation types, WUBRG colors, and event normalizer** - `eb67e5c` (feat)
2. **Task 2: Extend preferences store with VFX quality and animation speed** - `03c2e93` (feat)

## Files Created/Modified
- `client/src/animation/types.ts` - VfxQuality, AnimationSpeed, SPEED_MULTIPLIERS, StepEffect, AnimationStep, PositionSnapshot, EVENT_DURATIONS
- `client/src/animation/wubrgColors.ts` - WUBRG_COLORS record and getCardColors utility
- `client/src/animation/eventNormalizer.ts` - normalizeEvents function with grouping heuristics
- `client/src/animation/__tests__/eventNormalizer.test.ts` - 14 tests for normalizer grouping behavior
- `client/src/animation/__tests__/wubrgColors.test.ts` - 5 tests for color mapping
- `client/src/stores/preferencesStore.ts` - Added vfxQuality and animationSpeed fields
- `client/src/stores/__tests__/preferencesStore.test.ts` - 5 new tests for animation preferences

## Decisions Made
- Non-visual events defined as a Set of 12 event types (PriorityPassed, MulliganStarted, GameStarted, ManaAdded, DamageCleared, CardsDrawn, CardDrawn, PermanentTapped, PermanentUntapped, StackPushed, StackResolved, ReplacementApplied) skipped by normalizer
- Groupable events (DamageDealt, CreatureDestroyed, PermanentSacrificed) merge consecutive same-type into one step
- Merge types (ZoneChanged, LifeChanged) attach to preceding step rather than creating new ones
- EVENT_DURATIONS duplicated in animation/types.ts (from animationStore.ts) for clean module separation

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

None.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- All animation types exported and ready for step queue (Plan 02)
- Event normalizer ready for wiring into game dispatch pipeline (Plan 04)
- VFX preferences available for conditional rendering in VFX components (Plan 03)

---
*Phase: 14-animation-pipeline*
*Completed: 2026-03-09*
