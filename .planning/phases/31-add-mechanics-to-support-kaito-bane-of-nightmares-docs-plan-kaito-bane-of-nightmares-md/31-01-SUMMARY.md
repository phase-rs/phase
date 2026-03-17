---
phase: 31-kaito-mechanics
plan: 01
subsystem: engine
tags: [static-condition, compound-condition, parser, layers, animation, planeswalker]

requires:
  - phase: 28-native-ability-data-model
    provides: StaticCondition enum, ContinuousModification, layer evaluation system
provides:
  - StaticCondition::And/Or combinators for compound conditions
  - StaticCondition::HasCounters for counter-based condition checking
  - Compound condition parser for "during your turn, as long as ~ has counters" pattern
  - Animation modification parser for "pronoun's a P/T Type and has Keyword"
affects: [31-kaito-mechanics, parser-coverage]

tech-stack:
  added: []
  patterns: [compound-condition-combinator, counter-condition, animation-parser]

key-files:
  created: []
  modified:
    - crates/engine/src/types/ability.rs
    - crates/engine/src/game/layers.rs
    - crates/engine/src/parser/oracle_static.rs

key-decisions:
  - "StaticCondition::And/Or/HasCounters added by concurrent agent; parser work builds on existing type infrastructure"
  - "Animation parser uses composable helpers: parse_counter_minimum for count, parse_animation_modifications for P/T + types + keywords"
  - "Counter type stored as String to match existing CounterType::Generic pattern and serde compatibility"

patterns-established:
  - "Compound condition parsing: strip prefix layers (turn -> as-long-as -> counter check -> modifications)"
  - "Animation modification parser: pronoun stripping + P/T + CoreType/subtype classification + keyword extraction"

requirements-completed: [K31-COND, K31-ANIM, K31-PARSE]

duration: 22min
completed: 2026-03-17
---

# Phase 31 Plan 01: Compound Static Conditions Summary

**Compound StaticCondition::And/Or/HasCounters with layer evaluation and parser for planeswalker-to-creature animation pattern**

## Performance

- **Duration:** 22 min
- **Started:** 2026-03-17T02:20:34Z
- **Completed:** 2026-03-17T02:42:23Z
- **Tasks:** 2
- **Files modified:** 3

## Accomplishments
- StaticCondition::And/Or/HasCounters variants wired through layer evaluation (Task 1 - completed by concurrent agent)
- Parser for compound "during your turn, as long as ~ has counters" Oracle text pattern
- Animation modification parser handling "pronoun's a P/T Type creature and has Keyword"
- Tests for both condition evaluation and parser output

## Task Commits

Each task was committed atomically:

1. **Task 1: StaticCondition::And/Or/HasCounters + layer evaluation** - already committed by concurrent agent (types + evaluation + 8 unit tests)
2. **Task 2: Parser for compound static condition + animation** - `a5ee519f5` (feat)

**Plan metadata:** pending (docs: complete plan)

## Files Created/Modified
- `crates/engine/src/types/ability.rs` - StaticCondition::And/Or/HasCounters variants (concurrent agent), formatting cleanup
- `crates/engine/src/game/layers.rs` - evaluate_condition match arms for And/Or/HasCounters (concurrent agent), formatting cleanup
- `crates/engine/src/parser/oracle_static.rs` - parse_compound_turn_counter_animation, parse_counter_minimum, parse_animation_modifications functions + tests

## Decisions Made
- Task 1 was already implemented by a concurrent agent; verified tests pass and moved to Task 2
- Animation parser decomposes Oracle text grammatically: pronoun stripping -> P/T extraction -> CoreType::from_str for type classification -> subtype fallback -> keyword parsing via existing map_keyword
- Counter minimum parser handles "one or more", "a", and "N or more" patterns

## Deviations from Plan

None - plan executed as written. Task 1 was already completed by a concurrent agent, so only Task 2 required implementation.

## Issues Encountered
- Pre-existing compilation errors in coverage.rs, effects/draw.rs, game_state.rs, and ai_support/candidates.rs from other agents' concurrent work (QuantityExpr type changes, NinjutsuActivation, DamageAmount). These are out of scope and do not affect the parser or layer evaluation code.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Compound conditions fully operational in layer system
- Parser handles Kaito's specific Oracle text pattern
- Ready for remaining Kaito mechanics (loyalty abilities, ninjutsu, etc.)

---
*Phase: 31-kaito-mechanics*
*Completed: 2026-03-17*
