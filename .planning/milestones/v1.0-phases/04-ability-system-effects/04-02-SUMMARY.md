---
phase: 04-ability-system-effects
plan: 02
subsystem: engine
tags: [casting, targeting, stack-resolution, effects, fizzle, hexproof, shroud]

requires:
  - phase: 04-ability-system-effects
    plan: 01
    provides: "Effect handler registry, ResolvedAbility, TargetRef, resolve_effect dispatch"
  - phase: 03-game-state-engine
    provides: "GameState, StackEntry, mana_payment, zones, priority system"
provides:
  - "Casting flow: handle_cast_spell, handle_select_targets, handle_activate_ability"
  - "Targeting system: find_legal_targets, validate_targets, check_fizzle"
  - "Stack resolution with effect execution and fizzle checking"
  - "CastSpell, ActivateAbility, SelectTargets action dispatch in engine"
  - "PendingCast struct and WaitingFor::TargetSelection variant"
  - "StackEntryKind expanded with ability data and ActivatedAbility variant"
affects: [04-03-sub-ability-chaining, 05-triggers-combat]

tech-stack:
  added: []
  patterns: [casting-flow-pattern, targeting-filter-pattern, fizzle-check-pattern]

key-files:
  created:
    - crates/engine/src/game/casting.rs
    - crates/engine/src/game/targeting.rs
  modified:
    - crates/engine/src/game/stack.rs
    - crates/engine/src/game/engine.rs
    - crates/engine/src/game/priority.rs
    - crates/engine/src/game/mod.rs
    - crates/engine/src/types/game_state.rs
    - crates/engine/src/types/actions.rs
    - crates/engine/src/game/effects/counter.rs

key-decisions:
  - "Build effect registry per apply() call -- cheap (15 inserts) and avoids static/lazy patterns"
  - "Auto-target when exactly one legal target exists (no WaitingFor prompt needed)"
  - "Effect handler errors logged but don't crash stack resolution"
  - "Fizzle check uses ValidTgts param to revalidate targets on resolution"

patterns-established:
  - "Casting flow: validate timing -> parse ability -> find targets -> pay cost -> push to stack"
  - "Targeting filter: string-based ValidTgts$ dispatch to find_legal_targets"
  - "Fizzle pattern: validate_targets + check_fizzle before effect execution"

requirements-completed: [ABIL-03, ABIL-04]

duration: 7min
completed: 2026-03-08
---

# Phase 04 Plan 02: Casting Flow, Targeting, and Stack Resolution Summary

**Spell casting from hand with timing/targeting/cost validation, stack resolution executing effect handlers, and fizzle rule enforcement**

## Performance

- **Duration:** 7 min
- **Started:** 2026-03-08T00:08:15Z
- **Completed:** 2026-03-08T00:14:57Z
- **Tasks:** 2
- **Files modified:** 11

## Accomplishments
- Full casting flow: timing validation (sorcery vs instant speed), target selection (auto or manual), mana cost payment, stack push
- Targeting system with hexproof/shroud enforcement and ValidTgts$ filter parsing (Any, Creature, Creature.YouCtrl, Creature.OppCtrl, Creature.non{Color}, Player)
- Stack resolution executes effect handlers via registry before moving cards to destination zones
- Fizzle rule: all targets illegal on resolution causes spell to go to graveyard without effects
- CastSpell, ActivateAbility, and SelectTargets actions wired into engine dispatch
- 265 total engine tests passing (15 new tests added)

## Task Commits

Each task was committed atomically:

1. **Task 1: Targeting system and casting flow** - `2073636` (feat)
2. **Task 2: Stack resolution with effects and engine action dispatch** - `895e9df` (feat)

## Files Created/Modified
- `crates/engine/src/game/targeting.rs` - Target validation with hexproof/shroud, filter parsing, fizzle checking
- `crates/engine/src/game/casting.rs` - Casting flow with timing, targeting, cost payment, stack push
- `crates/engine/src/game/stack.rs` - Updated resolve_top with effect execution and fizzle checking
- `crates/engine/src/game/engine.rs` - Added CastSpell, ActivateAbility, SelectTargets match arms
- `crates/engine/src/game/priority.rs` - Pass registry through to stack resolution
- `crates/engine/src/game/mod.rs` - Added casting and targeting module declarations
- `crates/engine/src/types/game_state.rs` - PendingCast, WaitingFor::TargetSelection, expanded StackEntryKind
- `crates/engine/src/types/actions.rs` - Added SelectTargets action variant
- `crates/engine/src/game/effects/counter.rs` - Updated for new StackEntryKind signature

## Decisions Made
- Build effect registry per apply() call rather than static/lazy -- build_registry is cheap (15 HashMap inserts) and keeps code simple
- Auto-target when exactly one legal target exists, avoiding unnecessary WaitingFor::TargetSelection round-trip
- Effect handler errors are logged but don't crash stack resolution -- allows graceful degradation for unregistered effects
- Fizzle check uses the ValidTgts param from the ability to revalidate targets at resolution time

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Casting flow and effect execution ready for sub-ability chaining (Plan 03)
- StackEntryKind carries ResolvedAbility with sub_ability field for chaining
- All effect handlers receive targets through ResolvedAbility.targets

---
*Phase: 04-ability-system-effects*
*Completed: 2026-03-08*
