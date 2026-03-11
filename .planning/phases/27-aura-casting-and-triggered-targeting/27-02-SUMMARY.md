---
phase: 27-aura-casting-and-triggered-targeting
plan: 02
subsystem: engine
tags: [aura, casting, targeting, enchant, attachment, stack, resolution]

requires:
  - phase: 27-aura-casting-and-triggered-targeting
    plan: 01
    provides: "find_legal_targets_typed(), ResolvedAbility.duration, Keyword::Enchant(TargetFilter)"
provides:
  - "Aura spell casting with enchant target selection via Keyword::Enchant TargetFilter"
  - "Aura attachment on resolution in resolve_top"
  - "Aura fizzle check (target leaves battlefield before resolution)"
affects: [27-03]

tech-stack:
  added: []
  patterns:
    - "Aura detection via subtype check + Keyword::Enchant extraction in casting.rs"
    - "Aura attachment after move_to_zone in resolve_top using effects::attach::attach_to"
    - "spell_targets clone before fizzle path for post-resolution attachment access"

key-files:
  created: []
  modified:
    - "crates/engine/src/game/casting.rs"
    - "crates/engine/src/game/stack.rs"

key-decisions:
  - "Aura targeting inserted before has_targeting_requirement check -- Auras target via Enchant keyword, not via Effect target field"
  - "Re-read obj after evaluate_layers to avoid borrow checker conflict with mutable layer evaluation"
  - "Clone spell_targets before fizzle path to preserve targets for Aura attachment after resolution"
  - "Tests push keywords to both keywords and base_keywords to survive layer evaluation (per 22-03 convention)"

patterns-established:
  - "Aura detection: check subtypes for 'Aura', extract TargetFilter from Keyword::Enchant"
  - "Aura attachment: after move_to_zone(Battlefield), check is_aura, verify target on battlefield, call attach_to"

requirements-completed: [P27-AURA, P27-TEST]

duration: 6min
completed: 2026-03-11
---

# Phase 27 Plan 02: Aura Casting Summary

**Aura spell casting with typed enchant target selection and automatic attachment on resolution via Keyword::Enchant TargetFilter**

## Performance

- **Duration:** 6 min
- **Started:** 2026-03-11T08:35:28Z
- **Completed:** 2026-03-11T08:41:28Z
- **Tasks:** 2
- **Files modified:** 2

## Accomplishments
- Aura spells detect Enchant keyword and use find_legal_targets_typed for typed target selection during casting
- Single legal target auto-targets, multiple targets prompt WaitingFor::TargetSelection, no legal targets prevents casting
- Auras attach to their chosen target when resolving from the stack (after entering battlefield)
- Aura fizzle works correctly -- target leaving battlefield causes Aura to go to graveyard
- 8 new tests across casting.rs and stack.rs covering all Aura casting and resolution behaviors

## Task Commits

Each task was committed atomically (TDD: test + feat per task):

1. **Task 1: Aura enchant target selection during casting** - `2328c527` (test) + `cb37aada` (feat)
2. **Task 2: Aura attachment on resolution in resolve_top** - `89cd95dc` (test) + `9a8030e9` (feat)

_Note: TDD tasks have separate test and implementation commits_

## Files Created/Modified
- `crates/engine/src/game/casting.rs` - Aura detection and typed target selection before existing has_targeting_requirement check; 5 Aura-specific tests
- `crates/engine/src/game/stack.rs` - Aura attachment after battlefield entry in resolve_top; 3 Aura resolution tests

## Decisions Made
- Aura targeting logic placed before has_targeting_requirement check because Auras target via Enchant keyword, not via Effect target field
- Re-read obj reference after evaluate_layers call to satisfy borrow checker (layers takes &mut state)
- Clone spell_targets before fizzle check block (which moves ability) to preserve targets for Aura attachment
- Tests push Hexproof to both keywords and base_keywords per 22-03 convention (layer evaluation resets keywords from base_keywords)

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Re-read obj after evaluate_layers to fix borrow checker error**
- **Found during:** Task 1 (Aura targeting implementation)
- **Issue:** The `obj` reference from line 44 was held across the mutable `evaluate_layers(state)` call, violating Rust borrow rules
- **Fix:** Added `let obj = state.objects.get(&object_id).unwrap();` re-read after the layer evaluation block
- **Files modified:** crates/engine/src/game/casting.rs
- **Verification:** cargo test -p engine passes
- **Committed in:** cb37aada (Task 1 feat commit)

**2. [Rule 1 - Bug] Push Hexproof to base_keywords in test to survive layer evaluation**
- **Found during:** Task 1 (hexproof test)
- **Issue:** Hexproof keyword pushed only to `keywords` was cleared by `evaluate_layers` (which resets from `base_keywords`)
- **Fix:** Test pushes to both `keywords` and `base_keywords`
- **Files modified:** crates/engine/src/game/casting.rs (test code)
- **Verification:** aura_targeting_respects_hexproof test passes
- **Committed in:** cb37aada (Task 1 feat commit)

**3. [Rule 3 - Blocking] Clone spell_targets before fizzle path to avoid use-after-move**
- **Found during:** Task 2 (Aura attachment implementation)
- **Issue:** `ability` is moved on line 51 (`let mut ability = ability;`) inside the fizzle check block, making `ability.targets` inaccessible for Aura attachment after resolution
- **Fix:** Clone targets before fizzle block: `let spell_targets = ability.targets.clone();`
- **Files modified:** crates/engine/src/game/stack.rs
- **Verification:** All 3 stack tests pass
- **Committed in:** 9a8030e9 (Task 2 feat commit)

---

**Total deviations:** 3 auto-fixed (2 bugs, 1 blocking)
**Impact on plan:** All auto-fixes required for compilation/correctness. No scope creep.

## Issues Encountered
None beyond the auto-fixed deviations above.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Aura casting and attachment complete, ready for Plan 03 (triggered ability targeting and exile-return system)
- find_legal_targets_typed proven to work for Aura casting target selection
- attach_to integration in resolve_top demonstrated and tested
- spell_targets clone pattern available for other post-resolution operations

---
*Phase: 27-aura-casting-and-triggered-targeting*
*Completed: 2026-03-11*
