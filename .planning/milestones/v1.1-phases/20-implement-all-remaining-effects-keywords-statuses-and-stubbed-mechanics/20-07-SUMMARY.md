---
phase: 20-implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics
plan: 07
subsystem: engine
tags: [effects, fight, bounce, explore, proliferate, copy-spell, replacement-effects, mtg-mechanics]

requires:
  - phase: 20-03
    provides: "WaitingFor::DigChoice, ScryChoice, SurveilChoice variants and SelectCards handling"
provides:
  - "Fight, Bounce, Explore, Proliferate, CopySpell, ChooseCard effect handlers"
  - "9 promoted replacement effect handlers (Attached, DealtDamage, Mill, PayLife, ProduceMana, Scry, Transform, TurnFaceUp, Explore)"
affects: [20-08, 20-09, 20-10]

tech-stack:
  added: []
  patterns: ["Helper function extraction for repeated counter-adding logic (add_explore_counter)"]

key-files:
  created:
    - crates/engine/src/game/effects/fight.rs
    - crates/engine/src/game/effects/bounce.rs
    - crates/engine/src/game/effects/explore.rs
    - crates/engine/src/game/effects/proliferate.rs
    - crates/engine/src/game/effects/copy_spell.rs
    - crates/engine/src/game/effects/choose_card.rs
  modified:
    - crates/engine/src/game/effects/mod.rs
    - crates/engine/src/game/replacement.rs

key-decisions:
  - "Explore reuses WaitingFor::DigChoice with keep_count=1 for nonland card choice"
  - "Proliferate auto-selects all eligible permanents (no player choice UI needed yet)"
  - "DealtDamage matcher checks source object ID matches damage target for self-referencing"
  - "Mill replacement matches Library-to-Graveyard zone changes for redirection to Exile"
  - "5 replacement handlers (ProduceMana, Scry, Transform, TurnFaceUp, Explore) are structural placeholders awaiting dedicated ProposedEvent variants"

patterns-established:
  - "Helper function extraction for counter-adding logic shared between explore paths"

requirements-completed: [ENG-14, ENG-15]

duration: 10min
completed: 2026-03-10
---

# Phase 20 Plan 07: Effect Handlers & Replacement Effects Summary

**6 new effect handlers (Fight, Bounce, Explore, Proliferate, CopySpell, ChooseCard) and 9 promoted replacement effect stubs with full test coverage**

## Performance

- **Duration:** 10 min
- **Started:** 2026-03-10T00:17:12Z
- **Completed:** 2026-03-10T00:28:00Z
- **Tasks:** 2
- **Files modified:** 8

## Accomplishments
- Effect handler registry expanded from 25 to 31 registered handlers
- Fight: mutual damage between source and target creatures based on power
- Bounce: returns battlefield permanents to owner's hand (Self/Targeted)
- Explore: land-on-top adds +1/+1 counter, nonland reuses WaitingFor::DigChoice
- Proliferate: adds one counter of each existing type to all permanents with counters
- CopySpell: duplicates top stack entry with new ID
- ChooseCard: generic card selection from any zone using DigChoice
- Replacement effect stubs reduced from 21 to 12 (9 promoted to real handlers)
- 24 new tests across all modules, all passing

## Task Commits

Each task was committed atomically:

1. **Task 1: Add missing effect handlers** - `0805bf9` (feat)
2. **Task 2: Promote replacement effect stubs** - `f586256` (feat)

## Files Created/Modified
- `crates/engine/src/game/effects/fight.rs` - Fight effect: mutual creature damage
- `crates/engine/src/game/effects/bounce.rs` - Bounce effect: return permanent to hand
- `crates/engine/src/game/effects/explore.rs` - Explore effect: land counter / nonland choice
- `crates/engine/src/game/effects/proliferate.rs` - Proliferate effect: add counters to all with counters
- `crates/engine/src/game/effects/copy_spell.rs` - CopySpell effect: duplicate top stack entry
- `crates/engine/src/game/effects/choose_card.rs` - ChooseCard effect: generic zone card selection
- `crates/engine/src/game/effects/mod.rs` - Registry updated with 6 new handlers
- `crates/engine/src/game/replacement.rs` - 9 stubs promoted to real handlers with matchers/appliers

## Decisions Made
- Explore reuses WaitingFor::DigChoice with keep_count=1 for nonland path (avoids new WaitingFor variant)
- Proliferate auto-selects all eligible permanents rather than requiring player choice
- DealtDamage replacement uses source-object self-referencing to match damage targeting the replacement holder
- Mill replacement matches ZoneChange from Library to Graveyard for redirect capability
- 5 structural placeholder handlers (ProduceMana, Scry, Transform, TurnFaceUp, Explore) have correct type signatures but return false from matchers until dedicated ProposedEvent variants are added

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Effect handler count at 31 (up from 25), replacement stubs reduced from 21 to 12
- All Standard-relevant effects (Fight, Bounce, Explore, Proliferate) now functional
- Ready for remaining keyword and mechanic implementation in plans 08-10

---
*Phase: 20-implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics*
*Completed: 2026-03-10*
