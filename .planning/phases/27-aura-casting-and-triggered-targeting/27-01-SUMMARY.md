---
phase: 27-aura-casting-and-triggered-targeting
plan: 01
subsystem: engine
tags: [targeting, triggers, serde, wasm, aura, exile]

requires:
  - phase: 28-native-ability-data-model
    provides: "Typed TargetFilter, AbilityDefinition, TriggerDefinition, Effect enum"
provides:
  - "find_legal_targets_typed() function for typed TargetFilter targeting"
  - "ResolvedAbility.duration field for exile-until-leaves tracking"
  - "PendingTrigger with Serialize/Deserialize for WASM boundary"
  - "WaitingFor::TriggerTargetSelection for trigger target selection flow"
  - "ExileLink struct for exile-return tracking"
  - "GameState.pending_trigger and exile_links fields"
  - "Card data execute fields for Sheltered by Ghosts, Banishing Light, Oblivion Ring"
affects: [27-02, 27-03]

tech-stack:
  added: []
  patterns:
    - "find_legal_targets_typed delegates to filter::matches_target_filter_controlled for battlefield objects"
    - "PendingTrigger carries full TriggerDefinition for re-evaluation at resolution"

key-files:
  created: []
  modified:
    - "crates/engine/src/types/ability.rs"
    - "crates/engine/src/types/game_state.rs"
    - "crates/engine/src/game/targeting.rs"
    - "crates/engine/src/game/triggers.rs"
    - "crates/engine/src/game/casting.rs"
    - "crates/phase-ai/src/legal_actions.rs"
    - "crates/server-core/src/session.rs"
    - "client/src/adapter/types.ts"
    - "data/abilities/sheltered_by_ghosts.json"
    - "data/abilities/banishing_light.json"
    - "data/abilities/oblivion_ring.json"

key-decisions:
  - "build_resolved_from_def in both casting.rs and triggers.rs carries duration from AbilityDefinition"
  - "ExileLink tracks exiled_id and source_id for exile-return resolution"
  - "Oblivion Ring second trigger (LTB return) left without execute -- handled by ExileLink system in Plan 03"

patterns-established:
  - "find_legal_targets_typed: typed targeting delegates to filter module for battlefield, adds players/stack separately"
  - "PendingTrigger serde: full trigger state serializable for WASM boundary crossing"

requirements-completed: [P27-FILTER, P27-TYPED, P27-TEST]

duration: 11min
completed: 2026-03-11
---

# Phase 27 Plan 01: Type Contracts Summary

**Typed targeting function, ResolvedAbility.duration, PendingTrigger serde, WaitingFor::TriggerTargetSelection, ExileLink, and card data execute fields for 3 exile-until-leaves cards**

## Performance

- **Duration:** 11 min
- **Started:** 2026-03-11T08:21:00Z
- **Completed:** 2026-03-11T08:32:00Z
- **Tasks:** 2
- **Files modified:** 11

## Accomplishments
- Added find_legal_targets_typed() that works with all TargetFilter variants including Typed, Any, Player, and Card
- Added type infrastructure for triggered targeting: PendingTrigger serde, WaitingFor::TriggerTargetSelection, ExileLink, GameState fields
- Updated 3 card JSON files (Sheltered by Ghosts, Banishing Light, Oblivion Ring) with typed execute fields for exile-until-leaves triggers

## Task Commits

Each task was committed atomically:

1. **Task 1: Type contracts** - `f7b3eddf` (feat)
2. **Task 2: Typed targeting + card data + frontend types** - `fe516139` (feat)

## Files Created/Modified
- `crates/engine/src/types/ability.rs` - Added duration field to ResolvedAbility
- `crates/engine/src/types/game_state.rs` - Added ExileLink, TriggerTargetSelection, pending_trigger, exile_links
- `crates/engine/src/game/triggers.rs` - PendingTrigger serde derives, duration in build_resolved_from_def
- `crates/engine/src/game/targeting.rs` - Added find_legal_targets_typed() function
- `crates/engine/src/game/casting.rs` - Duration propagation in build_resolved_from_def
- `crates/engine/src/game/effects/mod.rs` - Duration field in struct literal
- `crates/phase-ai/src/legal_actions.rs` - TriggerTargetSelection match arm
- `crates/server-core/src/session.rs` - TriggerTargetSelection in acting_player match arms
- `client/src/adapter/types.ts` - TriggerTargetSelection in WaitingFor union
- `data/abilities/sheltered_by_ghosts.json` - Added execute field with ChangeZone to Exile
- `data/abilities/banishing_light.json` - Added execute field with ChangeZone to Exile
- `data/abilities/oblivion_ring.json` - Added execute field to first trigger

## Decisions Made
- build_resolved_from_def in both casting.rs and triggers.rs carries duration from AbilityDefinition to ResolvedAbility
- ExileLink tracks exiled_id and source_id as simple struct (no HashMap, no enum)
- Oblivion Ring second trigger (LTB return) left without execute field -- handled by ExileLink system in Plan 03
- legal_actions TriggerTargetSelection generates one SelectTargets action per legal target for AI evaluation

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Added duration: None to all existing ResolvedAbility struct literals**
- **Found during:** Task 1 (adding duration field)
- **Issue:** Adding a new field to ResolvedAbility broke all existing struct literal constructions across 5 files
- **Fix:** Added `duration: None` to struct literals in casting.rs, triggers.rs, effects/mod.rs, and ability.rs tests
- **Files modified:** casting.rs, triggers.rs, effects/mod.rs, ability.rs
- **Verification:** Full test suite passes (648 tests)
- **Committed in:** f7b3eddf (Task 1 commit)

**2. [Rule 3 - Blocking] Added TriggerTargetSelection match arms to server-core and legal_actions**
- **Found during:** Task 1 (adding WaitingFor variant)
- **Issue:** New WaitingFor variant caused exhaustive match failures in phase-ai and server-core
- **Fix:** Added match arms for TriggerTargetSelection in legal_actions.rs and session.rs
- **Files modified:** legal_actions.rs, session.rs
- **Verification:** cargo check -p engine-wasm succeeds
- **Committed in:** f7b3eddf (Task 1 commit)

---

**Total deviations:** 2 auto-fixed (2 blocking)
**Impact on plan:** Both auto-fixes necessary for compilation. No scope creep -- legal_actions implementation pulled forward from Task 2 since it was blocking.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- All type contracts stable for Plans 02 (aura casting) and 03 (exile-return system)
- find_legal_targets_typed ready for trigger target selection in Plan 02
- ExileLink and pending_trigger fields ready for Plan 03 exile tracking
- Card data execute fields ready for trigger resolution in Plan 02

---
*Phase: 27-aura-casting-and-triggered-targeting*
*Completed: 2026-03-11*
