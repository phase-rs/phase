---
phase: 20-implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics
plan: 05
subsystem: engine
tags: [transform, dfc, double-faced-cards, zone-reset, face-switching]

requires:
  - phase: 20-01
    provides: mana abilities module and engine action wiring pattern
  - phase: 20-02
    provides: equipment/aura attachment pattern, shared file modifications

provides:
  - Transform module for DFC face switching
  - GameAction::Transform and GameEvent::Transformed
  - Zone-change reset (Rule 711.8) for transformed permanents
  - BackFaceData struct for storing DFC back face characteristics
  - DFC hover-to-peek UI in ArtCropCard

affects: [engine-effects, ui-card-display, card-database]

tech-stack:
  added: []
  patterns: [BackFaceData swap pattern for face switching, face-index-aware CardPreview]

key-files:
  created:
    - crates/engine/src/game/transform.rs
  modified:
    - crates/engine/src/game/zones.rs
    - crates/engine/src/game/engine.rs
    - crates/engine/src/game/game_object.rs
    - crates/engine/src/game/mod.rs
    - crates/engine/src/types/events.rs
    - crates/engine/src/types/actions.rs
    - client/src/adapter/types.ts
    - client/src/components/card/ArtCropCard.tsx
    - client/src/stores/uiStore.ts
    - client/src/pages/GamePage.tsx

key-decisions:
  - "BackFaceData stores both faces symmetrically -- transformed state swaps current with stored, enabling round-trip"
  - "Zone-change reset in move_to_zone restores front face inline (no separate function call needed)"
  - "inspectedFaceIndex added to uiStore for face-aware CardPreview without modifying CardPreview component"

patterns-established:
  - "DFC face swap: store current characteristics in BackFaceData before overwriting with other face"
  - "Face-index-aware inspection: uiStore.inspectedFaceIndex drives CardPreview name resolution"

requirements-completed: [ENG-10, ENG-11]

duration: 10min
completed: 2026-03-09
---

# Phase 20 Plan 05: Transform/DFC Summary

**Transform module with face switching, zone-change reset (Rule 711.8), GameAction::Transform, and DFC hover-to-peek UI**

## Performance

- **Duration:** 10 min
- **Started:** 2026-03-10T00:05:28Z
- **Completed:** 2026-03-10T00:15:49Z
- **Tasks:** 2
- **Files modified:** 10

## Accomplishments
- Created transform module with transform_permanent() that swaps front/back face characteristics
- Added zone-change reset in move_to_zone implementing MTG Rule 711.8
- Wired GameAction::Transform with full validation (battlefield, controller, back_face exists)
- Added DFC indicator badge and hover-to-peek back face preview in ArtCropCard

## Task Commits

Each task was committed atomically:

1. **Task 1: Transform module with face switching and zone reset** - `63ea4f8` (feat)
2. **Task 2: GameAction::Transform, engine wiring, TS types, and hover-to-peek UI** - `c808c09` (feat)

## Files Created/Modified
- `crates/engine/src/game/transform.rs` - Transform module with transform_permanent() and 4 tests
- `crates/engine/src/game/zones.rs` - Zone-change reset for transformed DFCs
- `crates/engine/src/types/events.rs` - GameEvent::Transformed variant
- `crates/engine/src/types/actions.rs` - GameAction::Transform variant
- `crates/engine/src/game/engine.rs` - Transform action match arm with validation
- `client/src/adapter/types.ts` - back_face on GameObject, Transform action/event
- `client/src/components/card/ArtCropCard.tsx` - DFC indicator badge with hover-to-peek
- `client/src/stores/uiStore.ts` - inspectedFaceIndex for face-aware preview
- `client/src/pages/GamePage.tsx` - Face-index-aware CardPreview name resolution

## Decisions Made
- BackFaceData swap is symmetric: when transforming, current characteristics are saved in back_face before overwriting. This enables unlimited round-trip transforms without data loss.
- Zone-change reset happens inline in move_to_zone rather than requiring callers to explicitly reset. This ensures Rule 711.8 is always enforced.
- Added inspectedFaceIndex to uiStore rather than a separate hover state, keeping the existing inspect pattern intact.

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
- BackFaceData and back_face field on GameObject were already added by plan 20-04, so those edits were redundant (no conflict).
- Pre-existing format issues in engine.rs, combat.rs, combat_damage.rs, etc. are out of scope (not caused by this plan).
- 5 pre-existing client test failures (CombatOverlay, legalActionsHighlight, wasm-adapter) are unrelated to this plan's changes.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- Transform/DFC infrastructure is complete for cards using CardLayout::Transform
- Deck loading needs to populate back_face when creating objects from Transform-layout cards (future work)
- Trigger-based transform (e.g., werewolf day/night) will call transform_permanent() directly from trigger handlers

---
*Phase: 20-implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics*
*Completed: 2026-03-09*
