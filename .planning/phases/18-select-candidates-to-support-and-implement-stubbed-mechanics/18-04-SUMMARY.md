---
phase: 18-select-candidates-to-support-and-implement-stubbed-mechanics
plan: 04
subsystem: engine
tags: [effects, dig, gain-control, wither, infect, poison, combat-damage, sba]

requires:
  - phase: 18-02
    provides: "Effect handler registry and matches_filter helper"
provides:
  - "Dig and GainControl effect handlers"
  - "Wither/Infect combat damage modification"
  - "Poison counter subsystem with SBA loss condition"
  - "23-entry effect registry (was 21)"
affects: [18-05, card-coverage]

tech-stack:
  added: []
  patterns: ["Wither/Infect check in apply_combat_damage", "poison_counters on Player struct"]

key-files:
  created:
    - crates/engine/src/game/effects/dig.rs
    - crates/engine/src/game/effects/gain_control.rs
  modified:
    - crates/engine/src/game/effects/mod.rs
    - crates/engine/src/game/combat_damage.rs
    - crates/engine/src/game/sba.rs
    - crates/engine/src/types/player.rs

key-decisions:
  - "Dig simplified: first ChangeNum cards go to hand (TODO: WaitingFor::DigChoice for player selection)"
  - "Wither/Infect counters applied directly without replacement effects for simplicity"
  - "Infect damage to players skips LifeChanged event since no life is lost"

patterns-established:
  - "Keyword-based damage modification in apply_combat_damage"
  - "Player-level counter fields for game mechanics (poison_counters)"

requirements-completed: [MECH-07, MECH-08]

duration: 4min
completed: 2026-03-09
---

# Phase 18 Plan 04: Dig, GainControl, Wither/Infect, and Poison Summary

**Dig and GainControl effect handlers plus Wither/Infect combat damage with poison counter SBA**

## Performance

- **Duration:** 4 min
- **Started:** 2026-03-09T15:16:01Z
- **Completed:** 2026-03-09T15:20:56Z
- **Tasks:** 2
- **Files modified:** 6

## Accomplishments
- Created Dig effect handler (reveals top N, keeps ChangeNum in hand, rest to bottom)
- Created GainControl effect handler (changes controller of target permanent)
- Wither/Infect modify combat damage: -1/-1 counters to creatures instead of marked damage
- Infect gives poison counters to players instead of life loss
- Poison >= 10 triggers SBA loss condition (MTG rule 704.5c)
- Lifelink correctly interacts with Wither/Infect (attacker gains life)
- Registry grows from 21 to 23 entries; all 449 engine tests pass

## Task Commits

Each task was committed atomically:

1. **Task 1: Dig and GainControl effects** - `64ba089` (test), `d5f1ef4` (feat)
2. **Task 2: Wither/Infect damage and poison SBA** - `efd8f35` (test), `d95469a` (feat)

_TDD: RED (failing tests) then GREEN (implementation) commits for each task._

## Files Created/Modified
- `crates/engine/src/game/effects/dig.rs` - Dig effect: reveal top N, keep ChangeNum, rest to bottom
- `crates/engine/src/game/effects/gain_control.rs` - GainControl: change controller of target permanent
- `crates/engine/src/game/effects/mod.rs` - Registry (23 entries), module declarations
- `crates/engine/src/game/combat_damage.rs` - Wither/Infect damage modification in apply_combat_damage
- `crates/engine/src/game/sba.rs` - Poison counter loss condition check
- `crates/engine/src/types/player.rs` - Added poison_counters field to Player struct

## Decisions Made
- Dig simplified to automatically pick first N cards (proper implementation needs WaitingFor::DigChoice)
- Wither/Infect counter application bypasses replacement effects for simplicity
- Infect damage to players does not emit LifeChanged event since no life changes

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
- Pre-existing clippy warnings in engine crate (not related to this plan, out of scope)

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- 2 new effect handlers ready for card resolution
- Wither/Infect combat damage fully functional
- Poison counter subsystem complete with SBA
- Dig needs WaitingFor::DigChoice in a future phase for proper interactive implementation

---
*Phase: 18-select-candidates-to-support-and-implement-stubbed-mechanics*
*Completed: 2026-03-09*
