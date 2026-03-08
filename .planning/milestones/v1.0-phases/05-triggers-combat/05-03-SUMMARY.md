---
phase: 05-triggers-combat
plan: 03
subsystem: engine
tags: [combat, damage, first-strike, double-strike, trample, deathtouch, lifelink, menace, flying, vigilance, keywords]

requires:
  - phase: 05-triggers-combat
    provides: "Keyword enum with 50+ variants, TriggerMode enum, trigger pipeline"
  - phase: 04-ability-system
    provides: "Effect handlers, SVar resolution, stack resolution, SBA fixpoint loop"
provides:
  - "CombatState lifecycle: creation at BeginCombat, population during declare phases, cleanup at EndCombat"
  - "Attack/block legality validation with all combat keywords (flying/reach, menace, shadow, defender, haste, vigilance)"
  - "Combat damage resolution with first strike/double strike two-step processing"
  - "Trample damage assignment with deathtouch interaction (1=lethal per blocker)"
  - "Lifelink life gain on combat damage"
  - "Engine integration: DeclareAttackers/DeclareBlockers action handling"
  - "Turn system: combat phases interactive when creatures present, auto-skip otherwise"
  - "SBA deathtouch and indestructible checks for combat damage"
affects: [06-layers-replacements, ui-phase, ai-phase]

tech-stack:
  added: []
  patterns: ["CombatState struct on GameState for combat lifecycle", "damage assignment via ordered blocker list with lethal-then-excess"]

key-files:
  created:
    - crates/engine/src/game/combat.rs
    - crates/engine/src/game/combat_damage.rs
  modified:
    - crates/engine/src/game/mod.rs
    - crates/engine/src/game/engine.rs
    - crates/engine/src/game/turns.rs
    - crates/engine/src/game/sba.rs
    - crates/engine/src/types/game_state.rs

key-decisions:
  - "Auto-order blockers by ObjectId ascending (deterministic default, full player choice deferred to UI phase)"
  - "CombatState on GameState as Option<CombatState> -- None outside combat, Some during combat phases"
  - "SBA updated: deathtouch flag + damage > 0 = lethal, indestructible prevents destruction from lethal damage"
  - "Combat phases auto-skip via has_potential_attackers check in auto_advance"

patterns-established:
  - "CombatState lifecycle: created at BeginCombat, populated by declare_attackers/declare_blockers, consumed by resolve_combat_damage, cleared at EndCombat"
  - "Damage assignment: assign_attacker_damage determines per-attacker damage distribution based on blocker count, trample, deathtouch"
  - "Two-step damage: first_strike_damage_step + SBAs + triggers + regular_damage_step when first/double strike present"

requirements-completed: [COMB-01, COMB-02, COMB-03, COMB-04]

duration: 5min
completed: 2026-03-08
---

# Phase 5 Plan 3: Combat System Summary

**Full combat system with CombatState lifecycle, keyword-based legality, first strike/double strike two-step damage, trample+deathtouch overflow, lifelink, and turn system integration**

## Performance

- **Duration:** 5 min
- **Started:** 2026-03-08T01:17:22Z
- **Completed:** 2026-03-08T01:22:45Z
- **Tasks:** 2
- **Files modified:** 7

## Accomplishments
- Complete combat declaration system with attack/block legality validation for all combat keywords (flying/reach, menace, shadow, defender, haste, vigilance)
- Combat damage resolution with first strike/double strike two-step processing, SBAs between steps (dead creatures can't deal regular damage)
- Trample + deathtouch interaction: 1 damage counts as lethal per blocker, excess tramples to defending player
- Lifelink gains life for source controller on combat damage
- Combat phases integrated into turn system: interactive when creatures can attack, auto-skipped otherwise
- 29 total combat tests covering all keyword interactions and damage scenarios

## Task Commits

Each task was committed atomically:

1. **Task 1: CombatState, attack/block declaration, and legality** - `559f497` (feat)
2. **Task 2: Combat damage resolution and engine integration** - `11b7ca2` (feat)

## Files Created/Modified
- `crates/engine/src/game/combat.rs` - CombatState, declare_attackers, declare_blockers, validate_attackers, validate_blockers, has_potential_attackers
- `crates/engine/src/game/combat_damage.rs` - resolve_combat_damage, first/regular damage steps, assign_attacker_damage, apply_combat_damage
- `crates/engine/src/game/mod.rs` - Added combat and combat_damage modules
- `crates/engine/src/game/engine.rs` - DeclareAttackers/DeclareBlockers action handling with trigger processing
- `crates/engine/src/game/turns.rs` - Combat phase auto_advance with has_potential_attackers check, combat cleanup
- `crates/engine/src/game/sba.rs` - Deathtouch lethal damage check, indestructible prevents destruction
- `crates/engine/src/types/game_state.rs` - combat: Option<CombatState> field, DeclareAttackers/DeclareBlockers WaitingFor variants

## Decisions Made
- Auto-order blockers by ObjectId ascending as deterministic default (full player choice deferred to UI phase per CONTEXT.md)
- CombatState as Option on GameState -- None outside combat, Some during combat phases for clear lifecycle
- SBA updated: deathtouch flag + damage > 0 triggers destruction; indestructible keyword prevents lethal damage destruction
- Combat skip when no potential attackers jumps directly to PostCombatMain (no unnecessary phase events)

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 2 - Missing Critical] Added indestructible check to lethal damage SBA**
- **Found during:** Task 2 (SBA update for deathtouch)
- **Issue:** Lethal damage SBA would destroy indestructible creatures
- **Fix:** Added `!obj.has_keyword(Keyword::Indestructible)` check to check_lethal_damage
- **Files modified:** crates/engine/src/game/sba.rs
- **Committed in:** 11b7ca2 (Task 2 commit)

**2. [Rule 2 - Missing Critical] Clear dealt_deathtouch_damage in cleanup step**
- **Found during:** Task 2 (cleanup integration)
- **Issue:** dealt_deathtouch_damage flag would persist across turns causing incorrect SBA behavior
- **Fix:** Reset dealt_deathtouch_damage to false alongside damage_marked in execute_cleanup
- **Files modified:** crates/engine/src/game/turns.rs
- **Committed in:** 11b7ca2 (Task 2 commit)

---

**Total deviations:** 2 auto-fixed (2 missing critical)
**Impact on plan:** Both fixes essential for correctness per MTG rules. No scope creep.

## Issues Encountered

None.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- Full combat system ready for Phase 6 (layers/replacements) to modify combat with continuous effects
- All 347 workspace tests pass
- Combat integrates cleanly with trigger pipeline (death triggers fire when creatures die in combat)
- CombatState provides extensibility for Phase 6 static abilities (e.g., Goblin Lord +1/+1 affecting combat damage)

---
*Phase: 05-triggers-combat*
*Completed: 2026-03-08*
