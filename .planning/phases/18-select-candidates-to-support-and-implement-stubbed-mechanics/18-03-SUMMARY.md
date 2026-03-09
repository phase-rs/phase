---
phase: 18-select-candidates-to-support-and-implement-stubbed-mechanics
plan: 03
subsystem: engine
tags: [static-abilities, ward, protection, cant-be-blocked, prowess, combat, targeting, triggers]

requires:
  - phase: 18-select-candidates-to-support-and-implement-stubbed-mechanics
    provides: research on mechanic frequency and implementation patterns
provides:
  - Promoted Ward, Protection, CantBeBlocked static ability handlers
  - Protection from color blocking restriction in combat validation
  - Protection from color targeting restriction
  - Prowess keyword-based trigger matcher (noncreature spell -> +1/+1 pump)
affects: [engine-wasm, forge-ai, combat-damage, mana-payment]

tech-stack:
  added: []
  patterns:
    - "Keyword-based triggers: synthetic trigger injection in process_triggers for keywords without explicit trigger_definitions"
    - "Static ability promotion: move from stub handler to real RuleModification handler with typed mode string"

key-files:
  created: []
  modified:
    - crates/engine/src/game/static_abilities.rs
    - crates/engine/src/game/targeting.rs
    - crates/engine/src/game/combat.rs
    - crates/engine/src/game/triggers.rs

key-decisions:
  - "CantBeBlocked checked via static_definitions on attacker, not keyword, matching Forge card data format"
  - "Protection checks only Color variant; CardType and Quality deferred (lower frequency)"
  - "Ward cost enforcement deferred to mana payment UI; targeting remains legal with Ward"
  - "Prowess uses synthetic trigger injection since K:Prowess cards lack explicit trigger_definitions"

patterns-established:
  - "Keyword-based trigger pattern: check has_keyword in process_triggers loop, synthetically build ResolvedAbility"
  - "Static ability handler promotion: explicit registry.insert with dedicated handler function"

requirements-completed: [MECH-05, MECH-06]

duration: 7min
completed: 2026-03-09
---

# Phase 18 Plan 03: Static Abilities & Prowess Trigger Summary

**Promoted Ward/Protection/CantBeBlocked from stubs to real handlers with targeting and combat integration, plus Prowess keyword-based trigger matcher**

## Performance

- **Duration:** 7 min
- **Started:** 2026-03-09T15:03:38Z
- **Completed:** 2026-03-09T15:11:41Z
- **Tasks:** 2
- **Files modified:** 4

## Accomplishments
- CantBeBlocked static ability prevents blocking in validate_blockers
- Protection from color restricts both targeting and blocking by matching color
- Ward recognized as active static (cost enforcement deferred to Phase 17 mana payment UI)
- Prowess keyword triggers +1/+1 Pump ability on noncreature spell cast by controller

## Task Commits

Each task was committed atomically:

1. **Task 1: Promote Ward, Protection, CantBeBlocked static abilities** - `2193f38` (feat)
2. **Task 2: Add Prowess trigger matcher** - `0ff724e` (feat)

## Files Created/Modified
- `crates/engine/src/game/static_abilities.rs` - Promoted 3 statics from stub to real handlers with RuleModification effects
- `crates/engine/src/game/targeting.rs` - Added Protection from color targeting restriction in can_target
- `crates/engine/src/game/combat.rs` - Added CantBeBlocked and Protection from color blocking restrictions
- `crates/engine/src/game/triggers.rs` - Added Prowess keyword-based trigger with synthetic ability injection

## Decisions Made
- CantBeBlocked checked via static_definitions on the attacker object, matching how Forge card data encodes it (S:Mode$ CantBlockBy)
- Protection only handles Color variant for now; CardType and Quality variants deferred as lower frequency
- Ward is recognized and promoted from stub but cost enforcement deferred -- targeting is still legal
- Prowess uses synthetic trigger injection pattern since Forge defines it as K:Prowess with no explicit trigger definition

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
- Pre-existing compilation errors in test_helpers.rs (unrelated to changes) required using `--lib` flag for test runs
- Pre-existing clippy warnings across codebase; only addressed warnings in modified files

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- Static ability framework proven for future promotions (Indestructible, Shroud, etc.)
- Keyword-based trigger pattern established for future keywords (Exalted, Flanking, etc.)
- Ward cost enforcement needs mana payment UI integration

---
*Phase: 18-select-candidates-to-support-and-implement-stubbed-mechanics*
*Completed: 2026-03-09*
