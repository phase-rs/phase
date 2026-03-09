---
phase: 14-animation-pipeline
plan: 02
subsystem: ui
tags: [animation, zustand, dispatch, snapshot, mutex, vitest]

requires:
  - phase: 14-animation-pipeline
    provides: AnimationStep types, event normalizer, speed preferences
provides:
  - Step-based animation store with enqueueSteps/playNextStep pipeline
  - Snapshot-before-dispatch pattern in useGameDispatch
  - Dispatch mutex for serializing rapid actions
affects: [14-03-vfx-components, 14-04-wiring]

tech-stack:
  added: []
  patterns: [step-based-animation-queue, snapshot-before-dispatch, dispatch-mutex-serialization]

key-files:
  created:
    - client/src/stores/__tests__/animationStore.test.ts
    - client/src/hooks/__tests__/useGameDispatch.test.ts
  modified:
    - client/src/stores/animationStore.ts
    - client/src/hooks/useGameDispatch.ts
    - client/src/components/animation/AnimationOverlay.tsx

key-decisions:
  - "captureSnapshot returns a local Map, not stored in Zustand state, to avoid re-renders during capture"
  - "Dispatch mutex uses useRef (not Zustand state) to prevent re-render cascades"
  - "Animation wait uses setTimeout with summed step durations * speed multiplier"
  - "currentSnapshot exported as module-level variable for AnimationOverlay access"

patterns-established:
  - "Snapshot-before-dispatch: capture DOM positions before WASM call for death animations"
  - "Dispatch serialization: ref-based mutex with pending queue prevents interleaved animations"

requirements-completed: [ANIM-01, ANIM-03, ANIM-04]

duration: 3min
completed: 2026-03-09
---

# Phase 14 Plan 02: Step Queue and Dispatch Pipeline Summary

**Step-based animation store with snapshot-before-dispatch and mutex-serialized game dispatch**

## Performance

- **Duration:** 3 min
- **Started:** 2026-03-09T03:29:30Z
- **Completed:** 2026-03-09T03:32:32Z
- **Tasks:** 2
- **Files modified:** 5

## Accomplishments
- Refactored animationStore from flat AnimationEffect queue to AnimationStep[] step-based pipeline
- Implemented snapshot-before-dispatch in useGameDispatch: captures card positions, calls WASM, normalizes events, plays animations, then defers state update
- Added dispatch mutex preventing interleaved animations from rapid clicks or AI dispatch loops
- Animation speed 'instant' skips animation wait entirely

## Task Commits

Each task was committed atomically:

1. **Task 1: Refactor animationStore to step-based pipeline** - `8a7ad7d` (feat)
2. **Task 2: Refactor useGameDispatch with snapshot-animate-update flow** - `13854d7` (feat)

## Files Created/Modified
- `client/src/stores/animationStore.ts` - Step-based queue with enqueueSteps, playNextStep, captureSnapshot
- `client/src/stores/__tests__/animationStore.test.ts` - 8 tests for step queue, snapshot, and position registry
- `client/src/hooks/useGameDispatch.ts` - Snapshot-animate-update flow with mutex serialization
- `client/src/hooks/__tests__/useGameDispatch.test.ts` - 5 tests for dispatch flow, timing, and serialization
- `client/src/components/animation/AnimationOverlay.tsx` - Updated to use step-based API (StepEffect, playNextStep)

## Decisions Made
- captureSnapshot returns a local Map (not Zustand state) to avoid unnecessary re-renders during position capture
- Dispatch mutex uses useRef rather than Zustand state to prevent re-render cascades from mutex toggling
- Animation wait implemented as setTimeout with summed durations * speed multiplier (simpler than subscribing to isPlaying)
- currentSnapshot exported as module-level variable so AnimationOverlay (Plan 04) can read positions during playback

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Updated AnimationOverlay to use new step-based API**
- **Found during:** Task 1 (animationStore refactor)
- **Issue:** AnimationOverlay.tsx imported removed types (AnimationEffect) and called removed methods (playNext, queue)
- **Fix:** Updated imports to use StepEffect from animation/types, replaced queue/playNext with steps/playNextStep, process all effects in a step
- **Files modified:** client/src/components/animation/AnimationOverlay.tsx
- **Verification:** Type-check passes, all tests pass
- **Committed in:** 8a7ad7d (Task 1 commit)

---

**Total deviations:** 1 auto-fixed (1 blocking)
**Impact on plan:** Necessary fix for existing consumer of refactored API. No scope creep.

## Issues Encountered

None.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- Step-based animation pipeline ready for VFX component rendering (Plan 03)
- Snapshot and dispatch wiring ready for AnimationOverlay integration (Plan 04)
- currentSnapshot export available for position lookups during playback

---
*Phase: 14-animation-pipeline*
*Completed: 2026-03-09*
