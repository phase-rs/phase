---
phase: 03-game-state-engine
plan: 03
subsystem: engine
tags: [mana, state-based-actions, mulligan, game-loop, mtg-rules]

requires:
  - phase: 03-game-state-engine/03-01
    provides: "GameState types, zones, game objects, mana pool data structures"
  - phase: 03-game-state-engine/03-02
    provides: "Turn cycle, priority system, stack resolution, auto-advance"
provides:
  - "Mana production from lands with source tracking"
  - "Cost payment algorithm with hybrid/phyrexian support"
  - "State-based actions fixpoint loop (0-life, 0-toughness, lethal damage, legend rule, unattached auras)"
  - "London mulligan flow with keep/mull/bottom-cards"
  - "Complete game loop: mulligan -> play land -> tap for mana -> pass through turn cycle"
affects: [abilities, combat, triggers]

tech-stack:
  added: []
  patterns: [sba-fixpoint-loop, london-mulligan-flow, greedy-mana-payment]

key-files:
  created:
    - crates/engine/src/game/mana_payment.rs
    - crates/engine/src/game/sba.rs
    - crates/engine/src/game/mulligan.rs
  modified:
    - crates/engine/src/game/engine.rs
    - crates/engine/src/game/mod.rs
    - crates/engine/src/types/actions.rs
    - crates/engine/src/types/events.rs

key-decisions:
  - "Greedy mana payment: colored shards first, hybrid prefers more available color, phyrexian falls back to life"
  - "SBA fixpoint capped at 9 iterations per Forge convention"
  - "start_game auto-detects libraries for mulligan vs skip-mulligan"
  - "start_game_skip_mulligan added for backward compat with tests needing direct game start"
  - "ManaAdded event uses ManaType (not ManaColor) to support colorless mana production"

patterns-established:
  - "SBA fixpoint: loop until no actions performed, max 9 iterations"
  - "Mulligan flow: state machine via WaitingFor variants (MulliganDecision -> MulliganBottomCards -> Priority)"
  - "Action dispatch: match (waiting_for, action) tuple for clean validation"
  - "TapLandForMana as Phase 3 convenience action; generalizes to ActivateAbility later"

requirements-completed: [ENG-03, ENG-05, ENG-06]

duration: 7min
completed: 2026-03-07
---

# Phase 3 Plan 3: Mana Payment, SBAs, and London Mulligan Summary

**Mana production/payment with greedy algorithm, SBA fixpoint loop enforcing 5 game rules, London mulligan flow, and full integration test proving complete game loop**

## Performance

- **Duration:** 7 min
- **Started:** 2026-03-07T23:07:15Z
- **Completed:** 2026-03-07T23:14:24Z
- **Tasks:** 2
- **Files modified:** 7

## Accomplishments
- Mana production from basic lands with source tracking, cost payment with hybrid/phyrexian/snow/X support
- State-based actions fixpoint loop handling 5 MTG rules (704.5a/f/g/j/n)
- London mulligan with draw-7, keep/mull, put-N-on-bottom flow for both players
- Full integration test: mulligan -> play land -> tap for mana -> pass priority through complete turn cycle
- All Phase 3 game engine requirements satisfied (204 tests passing)

## Task Commits

Each task was committed atomically:

1. **Task 1: Mana production and payment system** - `0a9c52c` (feat)
2. **Task 2: State-based actions, London mulligan, and integration** - `685d3a2` (feat)

## Files Created/Modified
- `crates/engine/src/game/mana_payment.rs` - Mana production, can_pay, pay_cost with greedy algorithm, land subtype mapping
- `crates/engine/src/game/sba.rs` - State-based actions fixpoint loop (5 rules, max 9 iterations)
- `crates/engine/src/game/mulligan.rs` - London mulligan flow with shuffle/draw/keep/bottom
- `crates/engine/src/game/engine.rs` - TapLandForMana handler, mulligan/SBA wiring, integration test
- `crates/engine/src/game/mod.rs` - Module declarations and re-exports
- `crates/engine/src/types/actions.rs` - TapLandForMana and SelectCards action variants
- `crates/engine/src/types/events.rs` - ManaAdded updated to ManaType, PermanentTapped/PlayerLost/MulliganStarted/CardsDrawn added

## Decisions Made
- Greedy mana payment algorithm: pay colored shards first, hybrids prefer more available color (Forge style), phyrexian falls back to 2 life
- Generic mana payment prefers colorless first, then least-available color to preserve options
- SBA fixpoint capped at 9 iterations per Forge convention
- ManaAdded event changed from ManaColor to ManaType to support colorless mana production
- start_game auto-detects whether to run mulligan based on library contents
- start_game_skip_mulligan added for tests that need direct game start without mulligan overhead
- Action dispatch refactored to match on (waiting_for, action) tuple for cleaner validation

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Updated existing test for mulligan-aware start_game**
- **Found during:** Task 2
- **Issue:** `start_game_skips_draw_on_first_turn` test failed because start_game now runs mulligan which draws cards from library
- **Fix:** Changed test to use `start_game_skip_mulligan` which preserves the original behavior being tested
- **Files modified:** crates/engine/src/game/engine.rs
- **Committed in:** 685d3a2

**2. [Rule 1 - Bug] ManaAdded event field changed from ManaColor to ManaType**
- **Found during:** Task 1
- **Issue:** ManaAdded used ManaColor which has no Colorless variant, preventing colorless mana event tracking
- **Fix:** Changed to ManaType with source_id tracking
- **Files modified:** crates/engine/src/types/events.rs
- **Committed in:** 0a9c52c

---

**Total deviations:** 2 auto-fixed (2 bugs)
**Impact on plan:** Both fixes necessary for correctness. No scope creep.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Complete game engine loop operational: zones, objects, turns, priority, stack, mana, SBAs, mulligan
- Ready for Phase 4 (abilities) which will build on TapLandForMana -> ActivateAbility generalization
- All 204 engine tests passing, full workspace compiles

---
*Phase: 03-game-state-engine*
*Completed: 2026-03-07*
