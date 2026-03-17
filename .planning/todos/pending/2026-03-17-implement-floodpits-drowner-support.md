---
created: 2026-03-17T14:20:55.710Z
title: Implement Floodpits Drowner support
area: engine
files:
  - crates/engine/src/parser/oracle_effect.rs
  - crates/engine/src/types/ability.rs
  - crates/engine/src/game/effects/change_zone.rs
  - crates/engine/src/game/effects/shuffle.rs
  - crates/engine/src/game/triggers.rs
---

## Problem

Floodpits Drowner has two abilities that don't fully parse:

1. **ETB trigger** "tap target creature an opponent controls and put a stun counter on it" — the "and put a stun counter on it" remainder is silently dropped by `parse_targeted_action_ast()` which discards the `parse_target()` remainder. Affects ~20 cards with "tap/untap target X and Y" patterns.

2. **Activated ability** "{1}{U}, {T}: Shuffle this creature and target creature with a stun counter on it into their owners' libraries" — fully Unimplemented. The shuffle parser has no multi-object shuffle-to-library support.

## Solution

Detailed plan at `.claude/plans/flickering-booping-journal.md` with 4 changes:

1. **TargetFilter::ParentTarget** — new variant for anaphoric "it" in compound sub_abilities (vs SelfRef = source)
2. **try_split_targeted_compound()** — compound split at ParsedEffectClause level (like try_split_pump_compound), ~20 card impact
3. **ChangeZone SelfRef handling** — pre-loop guard routing through replacement pipeline
4. **Shuffle multi-object parser** — decompose into ChangeZone + Shuffle chain, extend shuffle::resolve for TargetRef::Object owner lookup
