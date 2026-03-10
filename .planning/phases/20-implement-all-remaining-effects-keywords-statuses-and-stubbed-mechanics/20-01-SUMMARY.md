---
phase: 20-implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics
plan: 01
subsystem: engine
tags: [mana-abilities, mtg-rule-605, game-engine, rust]

requires:
  - phase: 18-select-candidates-to-support-and-implement-stubbed-mechanics
    provides: "Effect registry, ability parser, mana payment system"
provides:
  - "Mana ability detection (is_mana_ability) from Forge ability text"
  - "Instant mana ability resolution without stack interaction"
  - "Mana ability activation during ManaPayment state (mid-cast)"
  - "TapLandForMana during ManaPayment state"
affects: [20-02, 20-07, forge-ai, engine-wasm]

tech-stack:
  added: []
  patterns: ["Mana abilities bypass stack via is_mana_ability guard in engine.rs match arms"]

key-files:
  created:
    - crates/engine/src/game/mana_abilities.rs
  modified:
    - crates/engine/src/game/engine.rs
    - crates/engine/src/game/mod.rs

key-decisions:
  - "Mana ability detection uses Forge api_type 'Mana' (not 'ProduceMana' which is a replacement effect)"
  - "Combo produced colors default to first listed color (e.g., 'Combo U B' produces U)"
  - "Non-mana ActivateAbility during ManaPayment returns error (only mana abilities allowed)"

patterns-established:
  - "Mana ability guard pattern: check is_mana_ability before routing to stack-based resolution"

requirements-completed: [ENG-01, ENG-02]

duration: 5min
completed: 2026-03-09
---

# Phase 20 Plan 01: Mana Abilities Summary

**MTG Rule 605 mana abilities with instant resolution via is_mana_ability guard and resolve_mana_ability bypassing the stack**

## Performance

- **Duration:** 5min
- **Started:** 2026-03-09T23:54:16Z
- **Completed:** 2026-03-09T23:59:39Z
- **Tasks:** 2
- **Files modified:** 3

## Accomplishments
- Mana abilities (Forge api_type "Mana") detected and resolved instantly without stack interaction
- Engine routing intercepts ActivateAbility for mana abilities in both Priority and ManaPayment states
- TapLandForMana action now also works during ManaPayment state for basic lands
- 8 total tests covering detection, resolution, tap cost, amount, and engine integration

## Task Commits

Each task was committed atomically:

1. **Task 1: Create mana_abilities module with detection and instant resolution** - `0c00a4a` (feat)
2. **Task 2: Wire mana abilities into engine.rs ActivateAbility path** - `8713eba` (feat)

## Files Created/Modified
- `crates/engine/src/game/mana_abilities.rs` - Mana ability detection (is_mana_ability) and instant resolution (resolve_mana_ability)
- `crates/engine/src/game/engine.rs` - Added mana ability guard in ActivateAbility match arms, ManaPayment ActivateAbility/TapLandForMana arms
- `crates/engine/src/game/mod.rs` - Added pub mod mana_abilities declaration

## Decisions Made
- Mana ability detection uses Forge api_type "Mana" (not "ProduceMana" which is a replacement effect for modifying mana production)
- Combo produced colors default to first listed color (e.g., "Combo U B" produces U) -- proper color choice deferred
- Non-mana ActivateAbility during ManaPayment returns error rather than silently ignoring
- Amount$ parameter supported for multi-mana producers (e.g., Sol Ring with Amount$ 2)

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Mana ability foundation complete for all future plans that add nonbasic lands or mana creatures
- Plan 20-07 can build on this for ProduceMana replacement effects (e.g., Chromatic Lantern)

---
*Phase: 20-implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics*
*Completed: 2026-03-09*
