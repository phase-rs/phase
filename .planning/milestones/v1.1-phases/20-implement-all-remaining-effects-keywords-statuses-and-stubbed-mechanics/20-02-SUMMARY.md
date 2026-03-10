---
phase: 20-implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics
plan: 02
subsystem: engine
tags: [equipment, aura, attachment, sba, mtg-rules]

requires:
  - phase: 18-select-candidates-to-support-and-implement-stubbed-mechanics
    provides: "Core effects infrastructure and effect registry"
provides:
  - "Attach effect handler (attach.rs) with resolve() and attach_to()"
  - "GameAction::Equip variant for equipment activation"
  - "WaitingFor::EquipTarget for target selection flow"
  - "Equipment unattach SBA (stays on battlefield when creature dies)"
  - "TypeScript types for Equip action and EquipTarget waiting state"
affects: [layers, combat, static-abilities, card-rendering]

tech-stack:
  added: []
  patterns:
    - "attach_to() shared utility for both effect resolution and engine equip action"
    - "Equipment SBA: clear attached_to without zone change (stays on battlefield)"

key-files:
  created:
    - crates/engine/src/game/effects/attach.rs
  modified:
    - crates/engine/src/game/effects/mod.rs
    - crates/engine/src/game/engine.rs
    - crates/engine/src/game/sba.rs
    - crates/engine/src/types/actions.rs
    - crates/engine/src/types/game_state.rs
    - client/src/adapter/types.ts

key-decisions:
  - "Equip action is a direct GameAction rather than going through ActivateAbility, to simplify the flow and avoid stack interaction"
  - "attach_to() is a shared function used by both the Attach effect handler and the engine equip flow"
  - "Auto-equip when only one valid creature target exists, matching the auto-target pattern"
  - "Equipment SBA clears attached_to without generating events (pure state cleanup)"

patterns-established:
  - "attach_to(): detach-then-attach pattern with old target cleanup"
  - "Equipment stays on battlefield after creature dies (unlike auras)"

requirements-completed: [ENG-03, ENG-04]

duration: 9min
completed: 2026-03-09
---

# Phase 20 Plan 02: Equipment and Aura Attachment Summary

**Equipment equip action with WaitingFor::EquipTarget flow, attach effect handler, and SBA for unattached equipment**

## Performance

- **Duration:** 9 min
- **Started:** 2026-03-09T23:54:19Z
- **Completed:** 2026-03-10T00:03:53Z
- **Tasks:** 2
- **Files modified:** 7

## Accomplishments
- Equipment can be equipped to creatures via GameAction::Equip with sorcery-speed validation
- Attachment state (attached_to/attachments) properly managed on both source and target objects
- Equipment falls off when creature dies (stays on battlefield, attached_to cleared)
- Aura graveyard SBA behavior preserved (existing tests still pass)
- TypeScript types updated for WASM boundary compatibility

## Task Commits

Each task was committed atomically:

1. **Task 1: Attach effect handler and equip action with WaitingFor::EquipTarget** - `0203272` (feat)
2. **Task 2: Extend SBA for unattached equipment and update TS types** - `ece97eb` (feat)

## Files Created/Modified
- `crates/engine/src/game/effects/attach.rs` - Attach effect handler with resolve() and attach_to() utility
- `crates/engine/src/game/effects/mod.rs` - Register Attach in effect registry
- `crates/engine/src/game/engine.rs` - Match arms for equip activation and target selection
- `crates/engine/src/game/sba.rs` - check_unattached_equipment SBA function
- `crates/engine/src/types/actions.rs` - GameAction::Equip variant
- `crates/engine/src/types/game_state.rs` - WaitingFor::EquipTarget variant
- `client/src/adapter/types.ts` - TypeScript Equip action and EquipTarget waiting state

## Decisions Made
- Equip action is a direct GameAction rather than using ActivateAbility path, simplifying the flow
- attach_to() is a shared utility for both effect resolution and direct equip action
- Auto-equip when single valid target (matches existing auto-target pattern)
- Equipment SBA is silent (no events emitted for the unattach, only state cleanup)

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
- Previous plan execution (20-01, 20-03) had already committed some of the files referenced in this plan (engine.rs, game_state.rs, effects/mod.rs) with partial equip-related code. The attach.rs and actions.rs changes completed the implementation.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- Equipment attachment foundation complete
- Ready for equipment stat-boosting (layers integration) in future plans
- Aura attachment on resolution can reuse attach_to() utility

---
*Phase: 20-implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics*
*Completed: 2026-03-09*
