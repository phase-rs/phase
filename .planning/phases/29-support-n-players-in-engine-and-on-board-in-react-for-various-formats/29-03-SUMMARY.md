---
phase: 29-support-n-players
plan: 03
subsystem: engine-combat
tags: [combat, multiplayer, commander-damage, attack-targets]
dependency_graph:
  requires: [29-01]
  provides: [per-creature-attack-targets, commander-damage-tracking, commander-damage-sba]
  affects: [phase-ai, engine-wasm, combat-tests]
tech_stack:
  added: []
  patterns: [AttackTarget enum for per-creature targeting, CommanderDamageEntry vec for damage tracking]
key_files:
  created: []
  modified:
    - crates/engine/src/types/actions.rs
    - crates/engine/src/game/engine.rs
    - crates/engine/src/game/combat_damage.rs
    - crates/engine/src/game/sba.rs
    - crates/engine/src/game/commander.rs
    - crates/engine-wasm/src/lib.rs
    - crates/phase-ai/src/legal_actions.rs
    - crates/phase-ai/src/search.rs
    - crates/engine/tests/rules/combat.rs
    - crates/engine/tests/rules/keywords.rs
    - crates/engine/tests/json_smoke_test.rs
decisions:
  - DeclareAttackers uses Vec<(ObjectId, AttackTarget)> for per-creature targeting
  - AI builds attacks vector with default target (first opponent) for backward compat
  - Commander damage tracked via existing CommanderDamageEntry vec (from 29-01)
  - SBA check_commander_damage skips when format has no threshold
metrics:
  duration: 25min
  completed: "2026-03-11"
---

# Phase 29 Plan 03: N-Player Combat (Attack Targets + Commander Damage) Summary

Per-creature AttackTarget enum on DeclareAttackers with commander damage tracking and SBA elimination at 21+ threshold.

## What Was Done

### Task 1: AttackTarget Enum and DeclareAttackers Refactor

Changed `DeclareAttackers` from `{ attacker_ids: Vec<ObjectId> }` to `{ attacks: Vec<(ObjectId, AttackTarget)> }` across the entire codebase. Updated all consumers:

- **actions.rs**: Renamed field, added AttackTarget import, added serde roundtrip tests
- **engine.rs**: Updated match arm for new field name
- **engine-wasm**: Added WasmAttackTarget enum and updated WasmGameAction::DeclareAttackers
- **phase-ai/legal_actions.rs**: Rewrote attacker_actions() to build attacks with default target
- **phase-ai/search.rs**: Updated combat AI delegation
- **All test files**: Updated DeclareAttackers calls in combat, keywords, and json_smoke tests

Note: combat.rs, game_object.rs, and game_state.rs changes were already implemented by prior plans (29-02 and 29-04). This plan focused on the remaining consumers.

### Task 2: Commander Damage Tracking and SBA

- Added `source_is_commander` extraction in `apply_combat_damage()` to track when commanders deal combat damage to players
- Commander damage increments existing entries or creates new `CommanderDamageEntry` records
- Added `check_commander_damage` SBA function that eliminates players at the configured threshold (21 for Commander format)
- SBA gracefully skips in non-Commander formats (threshold is None)

## Commits

| Task | Commit | Description |
|------|--------|-------------|
| 1 | 38949b375 | AttackTarget enum and DeclareAttackers per-creature targets |
| 2 | 184819669 | Commander damage tracking and SBA elimination |

## Tests Added

### Task 1 (3 tests in actions.rs)
- attack_target_serde_roundtrip
- declare_attackers_action_serde_roundtrip
- attack_target_player_serializes_correctly

### Task 2 (7 tests)
**combat_damage.rs (4 tests):**
- commander_damage_tracked_when_commander_hits_player
- commander_damage_accumulates_over_multiple_combats
- non_commander_damage_not_tracked
- different_commanders_tracked_separately

**sba.rs (3 tests):**
- sba_commander_damage_21_eliminates_player
- sba_commander_damage_20_does_not_eliminate
- sba_commander_damage_skipped_in_non_commander_format

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] commander.rs Supertype bug**
- **Found during:** Task 1 compilation
- **Issue:** Prior plan (29-04) used `"Legendary".to_string()` instead of `Supertype::Legendary` in test helper
- **Fix:** Changed to `vec![crate::types::card_type::Supertype::Legendary]`
- **Files modified:** crates/engine/src/game/commander.rs
- **Commit:** 38949b375

**2. [Rule 3 - Blocking] Many Task 1 changes already applied by prior plans**
- **Found during:** Task 1 execution
- **Issue:** combat.rs, game_object.rs, game_state.rs changes from this plan were already implemented in 29-02 and 29-04
- **Fix:** Focused only on remaining consumers (actions.rs, engine.rs, WASM, AI, tests)
- **Impact:** Reduced Task 1 scope (no combat.rs or game_object.rs edits needed)

### Out-of-Scope Issues Noted

- Pre-existing clippy type_complexity warning in json_loader.rs (not from this plan's changes)

## Verification

- All 711 engine lib unit tests pass
- All 42 engine integration tests pass
- All 10 json_smoke tests pass

## Self-Check: PASSED
