---
phase: 16-audio-system
plan: 01
subsystem: audio
tags: [web-audio-api, audiocontext, sfx, music, zustand, crossfade]

# Dependency graph
requires:
  - phase: 14-animation-pipeline
    provides: StepEffect type and animation step pipeline
provides:
  - AudioManager singleton with SFX preload, playback, consolidation, music streaming, crossfade, volume control
  - SFX_MAP mapping 15 GameEvent types to Forge SFX filenames
  - MUSIC_TRACKS list of 4 battle music tracks
  - initAudioOnInteraction for iOS/iPadOS AudioContext warm-up
  - preferencesStore audio fields (sfxVolume, musicVolume, sfxMuted, musicMuted, masterMuted)
affects: [16-02-dispatch-integration, 16-03-ui-controls]

# Tech tracking
tech-stack:
  added: []
  patterns: [AudioManager singleton pattern, Web Audio API gain node graph, preferences-driven volume control]

key-files:
  created:
    - client/src/audio/AudioManager.ts
    - client/src/audio/sfxMap.ts
    - client/src/audio/__tests__/AudioManager.test.ts
  modified:
    - client/src/stores/preferencesStore.ts
    - client/src/stores/__tests__/preferencesStore.test.ts

key-decisions:
  - "AudioManager is a plain TypeScript singleton, not a React component -- matches dispatch.ts pattern"
  - "Module-level usePreferencesStore.subscribe() wires real-time volume updates automatically"
  - "dispose() fully resets AudioManager state for clean test isolation without vi.resetModules()"

patterns-established:
  - "AudioManager singleton: module-level instance accessed via import, no React coupling"
  - "Web Audio gain node graph: sfxGain and musicGain connected to destination, per-sound gain for consolidation boost"
  - "getState() non-reactive reads for audio volume checks during playback"

requirements-completed: [AUDIO-01, AUDIO-02, AUDIO-03, AUDIO-04, AUDIO-05]

# Metrics
duration: 4min
completed: 2026-03-09
---

# Phase 16 Plan 01: Core Audio System Summary

**AudioManager singleton with SFX preload/playback (15 event mappings, consolidation), music streaming with crossfade/shuffle, and preferencesStore audio controls**

## Performance

- **Duration:** 4min
- **Started:** 2026-03-09T22:16:10Z
- **Completed:** 2026-03-09T22:20:50Z
- **Tasks:** 2
- **Files modified:** 5

## Accomplishments
- AudioManager singleton handles full AudioContext lifecycle, SFX buffer preloading, SFX playback with same-type consolidation, music streaming with crossfade and Fisher-Yates shuffle rotation
- SFX_MAP maps 15 core GameEvent types to confirmed-existing Forge SFX filenames
- preferencesStore extended with 5 audio fields (sfxVolume, musicVolume, sfxMuted, musicMuted, masterMuted) and 5 setters, all persisted to localStorage
- iOS/iPadOS AudioContext warm-up via one-shot interaction listeners

## Task Commits

Each task was committed atomically:

1. **Task 1: AudioManager singleton, SFX map, and tests** - `27709c5` (feat)
2. **Task 2: Extend preferencesStore with audio preferences and tests** - `36b07a9` (feat)

## Files Created/Modified
- `client/src/audio/AudioManager.ts` - Singleton audio system: AudioContext lifecycle, SFX preload+playback, music streaming+crossfade+rotation, volume control
- `client/src/audio/sfxMap.ts` - GameEvent type to Forge SFX filename mapping (15 events) and music track list (4 tracks)
- `client/src/audio/__tests__/AudioManager.test.ts` - 15 unit tests covering warmUp, preload, playSfx, consolidation, music, stopMusic, updateVolumes, initAudioOnInteraction
- `client/src/stores/preferencesStore.ts` - Extended with sfxVolume (70), musicVolume (40), sfxMuted, musicMuted, masterMuted fields and setters
- `client/src/stores/__tests__/preferencesStore.test.ts` - 8 new tests for audio defaults, setters, persistence, non-interference

## Decisions Made
- AudioManager is a plain TypeScript class singleton (not a React component) matching the project's established dispatch.ts/ScreenShake pattern
- Module-level `usePreferencesStore.subscribe()` auto-wires real-time volume updates without explicit React coupling
- Test isolation uses `dispose()` method to reset singleton state rather than `vi.resetModules()` to avoid Zustand store instance divergence

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- AudioManager ready for dispatch pipeline integration (Plan 02)
- preferencesStore audio fields ready for UI controls (Plan 03)
- All exported APIs stable: `audioManager`, `initAudioOnInteraction`, `SFX_MAP`, `MUSIC_TRACKS`

---
*Phase: 16-audio-system*
*Completed: 2026-03-09*
