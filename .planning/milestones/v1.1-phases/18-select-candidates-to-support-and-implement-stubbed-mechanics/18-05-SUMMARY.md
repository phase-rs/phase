---
phase: 18-select-candidates-to-support-and-implement-stubbed-mechanics
plan: 05
subsystem: engine
tags: [coverage, wasm, ui, mechanic-detection, warning-badge]

requires:
  - phase: 18-01
    provides: "Evasion keywords and test_helpers"
  - phase: 18-02
    provides: "Effect registry with 21 handlers"
  - phase: 18-03
    provides: "Static abilities and trigger registries"
provides:
  - "has_unimplemented_mechanics() per-object check against all registries"
  - "Coverage analysis module with analyze_standard_coverage()"
  - "UI warning badge for cards with unsupported mechanics"
  - "WASM serialization of has_unimplemented_mechanics flag"
affects: [forge-ai, deck-builder, card-coverage]

tech-stack:
  added: []
  patterns:
    - "skip_deserializing + WASM-side computation for derived display fields"
    - "Registry-based mechanic detection: check keywords/effects/triggers/statics against registries"

key-files:
  created: []
  modified:
    - crates/engine/src/game/coverage.rs
    - crates/engine/src/game/game_object.rs
    - crates/engine-wasm/src/lib.rs
    - client/src/adapter/types.ts
    - client/src/components/card/CardImage.tsx
    - client/src/components/board/PermanentCard.tsx
    - client/src/components/hand/PlayerHand.tsx

key-decisions:
  - "has_unimplemented_mechanics checks all 4 registry types: keywords (Unknown variant), effects, triggers, statics"
  - "Field added to GameObject with skip_deserializing; computed in WASM get_game_state() before serialization"
  - "Warning badge is a small amber '!' at top-left corner with title tooltip"

patterns-established:
  - "Derived display fields: add skip_deserializing field to Rust struct, compute in WASM layer before serialization"

requirements-completed: [MECH-09, MECH-10]

duration: 5min
completed: 2026-03-09
---

# Phase 18 Plan 05: Mechanic Coverage Report & UI Warning Summary

**Engine-side has_unimplemented_mechanics check against all registries with amber warning badge on affected cards**

## Performance

- **Duration:** 5 min
- **Started:** 2026-03-09T15:16:06Z
- **Completed:** 2026-03-09T15:21:00Z
- **Tasks:** 2
- **Files modified:** 7

## Accomplishments
- has_unimplemented_mechanics() checks keywords, abilities, triggers, and statics against engine registries
- Warning flag serialized through WASM boundary to TypeScript without keyword list duplication
- Amber "!" badge displayed on battlefield and hand cards with unsupported mechanics
- 6 new unit tests covering all mechanic check paths

## Task Commits

Each task was committed atomically:

1. **Task 1: Create has_unimplemented_mechanics check** - `af56ba8` (feat)
2. **Task 2: Wire flag through WASM and add UI warning badge** - `dc11920` (feat)

## Files Created/Modified
- `crates/engine/src/game/coverage.rs` - Added has_unimplemented_mechanics() checking all 4 registry types
- `crates/engine/src/game/game_object.rs` - Added has_unimplemented_mechanics field and method delegation
- `crates/engine-wasm/src/lib.rs` - Compute flag on each object in get_game_state() before serialization
- `client/src/adapter/types.ts` - Added hasUnimplementedMechanics to GameObject interface
- `client/src/components/card/CardImage.tsx` - Amber "!" warning badge with tooltip
- `client/src/components/board/PermanentCard.tsx` - Pass flag to CardImage
- `client/src/components/hand/PlayerHand.tsx` - Pass flag through HandCard to CardImage

## Decisions Made
- Used skip_deserializing + default(false) on the Rust field so existing serialized states roundtrip without breaking
- Compute the flag in WASM get_game_state() rather than at object creation to keep the core engine pure
- Warning badge uses 8px amber "!" text rather than an SVG icon for minimal footprint
- Only battlefield (PermanentCard) and hand (PlayerHand) show the badge; mulligan and deck builder do not

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- Coverage analysis available for reporting via analyze_standard_coverage()
- Per-object mechanic detection available for any future UI or AI use
- Warning badge pattern reusable for other card-level indicators

## Self-Check: PASSED
- FOUND: crates/engine/src/game/coverage.rs
- FOUND: crates/engine/src/game/game_object.rs
- FOUND: crates/engine-wasm/src/lib.rs
- FOUND: client/src/adapter/types.ts
- FOUND: client/src/components/card/CardImage.tsx
- FOUND: client/src/components/board/PermanentCard.tsx
- FOUND: client/src/components/hand/PlayerHand.tsx
- FOUND: commit af56ba8 (feat task 1)
- FOUND: commit dc11920 (feat task 2)
