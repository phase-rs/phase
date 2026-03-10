---
phase: 20-implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics
plan: 06
subsystem: engine
tags: [rust, static-abilities, triggers, indestructible, cant-be-countered, flashback, combat-triggers]

# Dependency graph
requires:
  - phase: 20-02
    provides: Equipment/Aura attachment system
  - phase: 20-04
    provides: Planeswalker loyalty mechanics
  - phase: 20-05
    provides: Transform/DFC mechanics
provides:
  - 17 promoted static ability handlers (Indestructible, CantBeCountered, FlashBack, keyword statics)
  - 22 promoted trigger matchers (AttackerBlocked, Milled, Exiled, Attached, Always, etc.)
  - CantBeCountered wired into counter effect
affects: [20-07, 20-08, 20-09, 20-10]

# Tech tracking
tech-stack:
  added: []
  patterns:
    - "Static handler pattern: fn(state, params, source_id) -> Vec<StaticEffect::RuleModification>"
    - "Trigger matcher pattern: fn(event, params, source_id, state) -> bool"

key-files:
  modified:
    - crates/engine/src/game/static_abilities.rs
    - crates/engine/src/game/triggers.rs
    - crates/engine/src/game/effects/counter.rs
    - crates/engine/src/game/effects/fight.rs

key-decisions:
  - "Static keyword handlers (Vigilance, Flying, etc.) return RuleModification for static-granted keywords via layer system"
  - "CantBeCountered checked both via check_static_ability and direct static_definitions on the spell object"
  - "Milled matcher matches Library-to-Graveyard zone changes (approximation of mill mechanic)"
  - "Always/Immediate matchers always return true (state triggers that check conditions elsewhere)"

patterns-established:
  - "Static stub -> real handler promotion: move from stubs array to explicit registry.insert with dedicated handler fn"
  - "Trigger stub -> real matcher promotion: move from unimplemented_modes array to explicit r.insert with dedicated matcher fn"

requirements-completed: [ENG-12, ENG-13]

# Metrics
duration: 11min
completed: 2026-03-09
---

# Phase 20 Plan 06: Static & Trigger Stub Promotion Summary

**Promoted 17 static ability handlers and 22 trigger matchers from stubs to real implementations, with CantBeCountered counter-effect integration**

## Performance

- **Duration:** 11 min
- **Started:** 2026-03-10T00:17:30Z
- **Completed:** 2026-03-10T00:28:34Z
- **Tasks:** 2
- **Files modified:** 7

## Accomplishments
- Promoted 17 static ability stubs to real handlers returning proper StaticEffect::RuleModification values
- Promoted 22 trigger matcher stubs to real matcher functions that respond to correct GameEvent variants
- Wired CantBeCountered into effects/counter.rs to prevent countering of uncounterable spells
- Added 16 new tests across static abilities, triggers, and counter effects

## Task Commits

Each task was committed atomically:

1. **Task 1: Promote Standard-relevant static ability stubs** - `12af064` (feat)
2. **Task 2: Promote Standard-relevant trigger matchers** - `f586256` (feat, combined with 20-07 by concurrent agent)

## Files Created/Modified
- `crates/engine/src/game/static_abilities.rs` - 17 promoted static handlers (Indestructible, CantBeCountered, FlashBack, Vigilance, Menace, Reach, Flying, Trample, Deathtouch, Lifelink, Shroud, CantBeDestroyed, CantTap, CantUntap, MustBeBlocked, CantAttackAlone, CantBlockAlone)
- `crates/engine/src/game/triggers.rs` - 22 promoted trigger matchers (AttackerBlocked, AttackerUnblocked, Milled, Exiled, Attached, Unattach, Cycled, Shuffled, Revealed, TapsForMana, ChangesController, Transformed, Fight, Always, Explored)
- `crates/engine/src/game/effects/counter.rs` - CantBeCountered integration preventing counterspells
- `crates/engine/src/game/effects/fight.rs` - Fix pre-existing u32/u64 type mismatch in test helper
- `crates/engine/src/game/effects/choose_card.rs` - Fix missing Zone import in tests
- `crates/engine/src/game/effects/proliferate.rs` - Fix missing Zone import in tests
- `crates/engine/src/game/effects/copy_spell.rs` - Fix missing StackEntryKind import in tests

## Decisions Made
- Static keyword handlers (Vigilance, Flying, etc.) return RuleModification for static-granted keywords via the layer system, matching how the engine applies keywords through static abilities
- CantBeCountered is checked both via check_static_ability (for battlefield permanents granting it) and direct static_definitions on the spell object (for self-referencing "can't be countered" cards)
- Milled trigger matcher matches Library-to-Graveyard ZoneChanged events as the closest approximation until a dedicated Mill event variant is added
- Always/Immediate matchers return true unconditionally, enabling state triggers that evaluate conditions elsewhere in the engine

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed u32/u64 type mismatch in fight.rs test helper**
- **Found during:** Task 1 (compilation verification)
- **Issue:** `CardId(state.next_object_id as u32)` failed because `next_object_id` is u64 and CardId expects u64
- **Fix:** Changed to `CardId(state.next_object_id)` (no cast needed)
- **Files modified:** crates/engine/src/game/effects/fight.rs
- **Verification:** Compilation succeeds, all tests pass
- **Committed in:** 12af064 (Task 1 commit)

**2. [Rule 3 - Blocking] Fixed missing test imports in 3 effect files**
- **Found during:** Task 2 (test compilation)
- **Issue:** choose_card.rs, proliferate.rs, copy_spell.rs had missing imports (Zone, StackEntryKind) in test modules, blocking compilation
- **Fix:** Added missing imports to test modules
- **Files modified:** choose_card.rs, proliferate.rs, copy_spell.rs
- **Verification:** All 536 engine tests compile and pass
- **Committed in:** f586256 (Task 2 commit)

---

**Total deviations:** 2 auto-fixed (1 bug, 1 blocking)
**Impact on plan:** Both auto-fixes necessary for compilation. No scope creep.

## Issues Encountered
- Concurrent agent (20-07) committed Task 2 changes along with its own work, resulting in a combined commit rather than a separate Task 2 commit

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Static stubs reduced from ~47 to ~28 (Standard-relevant mechanics promoted)
- Trigger stubs reduced from ~100 to ~78 (Standard-relevant matchers promoted)
- CantBeCountered, Indestructible, and FlashBack ready for integration testing with actual cards
- Foundation laid for 20-07 (replacement effects) and 20-08+ plans

---
*Phase: 20-implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics*
*Completed: 2026-03-09*
