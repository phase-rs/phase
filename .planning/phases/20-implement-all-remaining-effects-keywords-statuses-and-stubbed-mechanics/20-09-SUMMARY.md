---
phase: 20-implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics
plan: 09
subsystem: engine
tags: [morph, manifest, disguise, face-down, mtg-mechanics, rust]

requires:
  - phase: 20-05
    provides: "Transform characteristic swap patterns reused for face-up restoration"
  - phase: 20-06
    provides: "Trigger matchers and static keyword handler patterns"
  - phase: 20-07
    provides: "Effect handlers that may trigger manifest"
provides:
  - "Morph module with play_face_down, turn_face_up, and manifest functions"
  - "GameAction::PlayFaceDown and GameAction::TurnFaceUp engine wiring"
  - "TurnedFaceUp event and promoted TurnFaceUp trigger matcher"
  - "TypeScript types for face-down action/event support"
affects: [engine-wasm, forge-ai, client-ui]

tech-stack:
  added: []
  patterns: ["BackFaceData reuse for storing original face-down characteristics", "Face-down 2/2 override with characteristic restoration on turn-face-up"]

key-files:
  created:
    - "crates/engine/src/game/morph.rs"
  modified:
    - "crates/engine/src/types/events.rs"
    - "crates/engine/src/types/actions.rs"
    - "crates/engine/src/game/engine.rs"
    - "crates/engine/src/game/triggers.rs"
    - "crates/engine/src/game/mod.rs"
    - "client/src/adapter/types.ts"

key-decisions:
  - "Reuse BackFaceData struct to store original face-down characteristics (same pattern as DFC transform)"
  - "Manifested creature cards can turn face up (by paying mana cost); noncreature manifested cards cannot"
  - "TurnFaceUp trigger supports ValidCard filter for conditional triggering"

patterns-established:
  - "Face-down override pattern: store originals in back_face, apply 2/2 vanilla creature overrides"
  - "Manifest uses library-top object lookup via player.library.first() cross-referenced with objects map"

requirements-completed: [ENG-17]

duration: 5min
completed: 2026-03-10
---

# Phase 20 Plan 09: Morph/Manifest/Disguise Summary

**Face-down mechanics module with morph play, turn-face-up restoration, manifest from library, TurnFaceUp trigger, and engine action wiring**

## Performance

- **Duration:** 5 min
- **Started:** 2026-03-10T00:30:10Z
- **Completed:** 2026-03-10T00:35:22Z
- **Tasks:** 2
- **Files modified:** 7

## Accomplishments
- Created morph module with play_face_down, turn_face_up, and manifest functions
- Face-down creatures become 2/2 with no name, types, or abilities; originals stored in back_face
- Engine wired for PlayFaceDown and TurnFaceUp actions during Priority
- TurnFaceUp trigger promoted from unimplemented to real matcher with ValidCard filter
- TypeScript types updated for PlayFaceDown, TurnFaceUp actions and TurnedFaceUp event

## Task Commits

Each task was committed atomically:

1. **Task 1: Face-down play and turn-face-up mechanics** - `9a07296` (feat)
2. **Task 2: Manifest, engine wiring, trigger promotion, and TS types** - `e6ac0a1` (feat)

## Files Created/Modified
- `crates/engine/src/game/morph.rs` - Morph/manifest/disguise face-down mechanics (play_face_down, turn_face_up, manifest)
- `crates/engine/src/types/events.rs` - Added TurnedFaceUp event variant
- `crates/engine/src/types/actions.rs` - Added PlayFaceDown and TurnFaceUp action variants
- `crates/engine/src/game/engine.rs` - Match arms for PlayFaceDown and TurnFaceUp during Priority
- `crates/engine/src/game/triggers.rs` - Promoted TurnFaceUp trigger with match_turn_face_up matcher
- `crates/engine/src/game/mod.rs` - Added pub mod morph
- `client/src/adapter/types.ts` - TypeScript GameAction and GameEvent union updates

## Decisions Made
- Reused BackFaceData to store original face-down characteristics, same pattern as DFC transform module
- Manifested creature cards can be turned face up by paying mana cost; noncreature manifested cards cannot
- TurnFaceUp trigger matcher supports ValidCard filter param for conditional triggering on specific permanents

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Morph/manifest mechanics complete, ready for Plan 10 (final plan)
- Face-down mechanics integrate with existing transform, trigger, and layer systems

## Self-Check: PASSED

All files verified present, all commits verified in history.

---
*Phase: 20-implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics*
*Completed: 2026-03-10*
