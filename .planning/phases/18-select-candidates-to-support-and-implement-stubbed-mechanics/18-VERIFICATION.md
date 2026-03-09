---
phase: 18-select-candidates-to-support-and-implement-stubbed-mechanics
verified: 2026-03-09T16:00:00Z
status: passed
score: 14/14 must-haves verified
re_verification: false
---

# Phase 18: Select Candidates to Support and Implement Stubbed Mechanics — Verification Report

**Phase Goal:** Implement stubbed game mechanics in tiered batches -- combat keywords, effect handlers, static abilities, and damage mechanics -- with test infrastructure, coverage reporting, and UI warnings for remaining gaps
**Verified:** 2026-03-09T16:00:00Z
**Status:** passed
**Re-verification:** No — initial verification

## Goal Achievement

### Observable Truths

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | Fear creatures can only be blocked by artifact or black creatures | VERIFIED | `combat.rs:203` — `attacker.has_keyword(&Keyword::Fear)` with artifact/black checks; 3 tests |
| 2 | Intimidate creatures can only be blocked by artifact or color-sharing creatures | VERIFIED | `combat.rs:215` — `Keyword::Intimidate` with color-sharing logic; 2 tests |
| 3 | Skulk creatures cannot be blocked by creatures with greater power | VERIFIED | `combat.rs:226` — `Keyword::Skulk` with power comparison; 3 tests |
| 4 | Horsemanship creatures can only be blocked by other horsemanship creatures | VERIFIED | `combat.rs:239-240` — reciprocal Horsemanship check; 2 tests |
| 5 | Reusable test helper loads Forge card definitions and spawns creatures | VERIFIED | `test_helpers.rs` (131 lines) with `forge_db()`, `load_card()`, `spawn_creature()` |
| 6 | Mill effect moves top N cards from library to graveyard | VERIFIED | `effects/mill.rs` (143 lines) with `resolve()` handler registered in `mod.rs:51` |
| 7 | Scry effect lets controller reorder top N (simplified: to bottom) | VERIFIED | `effects/scry.rs` (118 lines) with `resolve()` registered in `mod.rs:52`; known simplification documented |
| 8 | PumpAll, DamageAll, DestroyAll, ChangeZoneAll handle mass effects | VERIFIED | `mod.rs:53-56` registers all four `resolve_all` handlers; shared `matches_filter()` in `mod.rs` |
| 9 | Ward, Protection, CantBeBlocked promoted from stubs to real handlers | VERIFIED | `static_abilities.rs:60-62` — explicit `registry.insert` for all three with dedicated handler functions |
| 10 | Prowess triggers +1/+1 on noncreature spell cast | VERIFIED | `triggers.rs:76-118` — synthetic trigger injection for Prowess keyword; 3 dedicated tests |
| 11 | Dig effect reveals top N, picks some to hand, rest to bottom | VERIFIED | `effects/dig.rs` (138 lines) registered at `mod.rs:57` |
| 12 | GainControl changes controller of target permanent | VERIFIED | `effects/gain_control.rs` (80 lines) registered at `mod.rs:58` |
| 13 | Wither/Infect modify damage, poison counters cause SBA loss at 10 | VERIFIED | `combat_damage.rs:342-402` — Wither/Infect checks; `sba.rs:32,75-81` — poison counter check; `player.rs:24` — `poison_counters: u32` field |
| 14 | Cards with unimplemented mechanics show UI warning badge | VERIFIED | Full stack wired: `coverage.rs:40-74` -> `game_object.rs:75,124-125` -> `lib.rs:89-90` (WASM) -> `types.ts:109` -> `CardImage.tsx` amber badge -> `PermanentCard.tsx:140` + `PlayerHand.tsx:104` pass flag |

**Score:** 14/14 truths verified

### Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/engine/src/game/test_helpers.rs` | Forge DB loading helpers | VERIFIED | 131 lines, exports `forge_db()`, `load_card()`, `spawn_creature()` |
| `crates/engine/src/game/effects/mill.rs` | Mill effect handler | VERIFIED | 143 lines with `resolve()` and tests |
| `crates/engine/src/game/effects/scry.rs` | Scry effect handler | VERIFIED | 118 lines with `resolve()` and tests |
| `crates/engine/src/game/effects/dig.rs` | Dig effect handler | VERIFIED | 138 lines with `resolve()` and tests |
| `crates/engine/src/game/effects/gain_control.rs` | GainControl handler | VERIFIED | 80 lines with `resolve()` and tests |
| `crates/engine/src/game/effects/mod.rs` | Registry with 23+ entries | VERIFIED | 8 new `registry.insert` calls; `matches_filter()` shared helper |
| `crates/engine/src/game/combat.rs` | Evasion + CantBeBlocked + Protection | VERIFIED | Fear/Intimidate/Skulk/Horsemanship/CantBeBlocked/Protection checks |
| `crates/engine/src/game/static_abilities.rs` | Ward/Protection/CantBeBlocked promoted | VERIFIED | Dedicated handlers at lines 142-175; registered at lines 60-62 |
| `crates/engine/src/game/targeting.rs` | Protection targeting + Ward recognition | VERIFIED | Protection color check at line 190-192; Ward recognized at line 200 |
| `crates/engine/src/game/triggers.rs` | Prowess trigger matcher | VERIFIED | Synthetic trigger injection at lines 76-118; 3 tests |
| `crates/engine/src/game/combat_damage.rs` | Wither/Infect damage modification | VERIFIED | Keyword checks at 342-343; counters at 372; poison at 398-402 |
| `crates/engine/src/game/sba.rs` | Poison counter SBA | VERIFIED | `check_poison_counters` at line 75; threshold >= 10 at line 81 |
| `crates/engine/src/types/player.rs` | poison_counters field | VERIFIED | `pub poison_counters: u32` at line 24; initialized to 0 at line 38 |
| `crates/engine/src/game/coverage.rs` | Coverage analysis module | VERIFIED | 434 lines; `has_unimplemented_mechanics()` + `analyze_standard_coverage()` + 6 tests |
| `crates/engine/src/game/game_object.rs` | has_unimplemented_mechanics field+method | VERIFIED | Field at line 75; method delegating to coverage at lines 124-125 |
| `crates/engine-wasm/src/lib.rs` | WASM serialization of flag | VERIFIED | Computed at lines 89-90 in `get_game_state()` |
| `client/src/adapter/types.ts` | TS interface field | VERIFIED | `has_unimplemented_mechanics?: boolean` at line 109 |
| `client/src/components/card/CardImage.tsx` | Amber warning badge | VERIFIED | Conditional amber "!" badge with tooltip |
| `client/src/components/board/PermanentCard.tsx` | Pass flag to CardImage | VERIFIED | `hasUnimplementedMechanics={obj.has_unimplemented_mechanics}` at line 140 |
| `client/src/components/hand/PlayerHand.tsx` | Pass flag through HandCard | VERIFIED | Flag passed at line 104 and line 199 |

### Key Link Verification

| From | To | Via | Status | Details |
|------|----|-----|--------|---------|
| combat.rs | keywords.rs | `has_keyword()` for Fear/Intimidate/Skulk/Horsemanship | WIRED | All 4 keyword checks present |
| effects/mod.rs | mill.rs | `registry.insert("Mill", mill::resolve)` | WIRED | Line 51 |
| effects/mod.rs | scry.rs | `registry.insert("Scry", scry::resolve)` | WIRED | Line 52 |
| effects/mod.rs | pump/damage/destroy/change_zone | `registry.insert` for all 4 "All" variants | WIRED | Lines 53-56 |
| effects/mod.rs | dig.rs | `registry.insert("Dig", dig::resolve)` | WIRED | Line 57 |
| effects/mod.rs | gain_control.rs | `registry.insert("GainControl", gain_control::resolve)` | WIRED | Line 58 |
| targeting.rs | static_abilities.rs | Protection color check | WIRED | Lines 190-192 |
| combat.rs | static_abilities.rs | CantBeBlocked check | WIRED | Line 153 via `static_definitions` |
| combat_damage.rs | player.rs | `player.poison_counters += amount` | WIRED | Line 402 |
| sba.rs | player.rs | `poison_counters >= 10` | WIRED | Line 81 |
| game_object.rs | coverage.rs | `coverage::has_unimplemented_mechanics(self)` | WIRED | Lines 124-125 |
| engine-wasm/lib.rs | game_object.rs | Compute flag before serialization | WIRED | Lines 89-90 |
| CardImage.tsx | types.ts | `hasUnimplementedMechanics` prop | WIRED | Prop at line 9, used at line 42 |
| PermanentCard.tsx | CardImage.tsx | Pass flag | WIRED | Line 140 |
| PlayerHand.tsx | CardImage.tsx | Pass flag through HandCard | WIRED | Lines 104, 199 |

### Requirements Coverage

| Requirement | Source Plan | Description | Status | Evidence |
|-------------|------------|-------------|--------|----------|
| MECH-01 | 18-01 | Reusable test helper for Forge card loading | SATISFIED | `test_helpers.rs` with forge_db/load_card/spawn_creature |
| MECH-02 | 18-01 | Combat evasion keywords (Fear, Intimidate, Skulk, Horsemanship) | SATISFIED | 4 keyword checks in validate_blockers with 10+ tests |
| MECH-03 | 18-02 | New effect handlers (Mill, Scry) | SATISFIED | 2 new modules registered in effect registry |
| MECH-04 | 18-02 | "All" effect variants (PumpAll, DamageAll, DestroyAll, ChangeZoneAll) | SATISFIED | 4 resolve_all handlers with shared matches_filter |
| MECH-05 | 18-03 | Static abilities (Ward, Protection, CantBeBlocked) promoted from stubs | SATISFIED | Dedicated handlers, targeting/combat integration |
| MECH-06 | 18-03 | Prowess trigger on noncreature spell cast | SATISFIED | Synthetic trigger injection with 3 tests |
| MECH-07 | 18-04 | Dig and GainControl effect handlers | SATISFIED | 2 new modules registered |
| MECH-08 | 18-04 | Wither/Infect damage modification with poison counter SBA | SATISFIED | Combat damage modification + SBA check + player field |
| MECH-09 | 18-05 | Mechanic coverage report | SATISFIED | `analyze_standard_coverage()` with full registry checks |
| MECH-10 | 18-05 | Visual warning indicator for unimplemented mechanics | SATISFIED | Full stack: engine -> WASM -> TS -> amber badge on cards |

No orphaned requirements found.

### Anti-Patterns Found

| File | Line | Pattern | Severity | Impact |
|------|------|---------|----------|--------|
| effects/scry.rs | 8 | TODO: WaitingFor::ScryChoice | Info | Known design decision; simplified implementation acknowledged in plan |
| effects/dig.rs | 10 | TODO: WaitingFor::DigChoice | Info | Known design decision; simplified implementation acknowledged in plan |
| triggers.rs | 367 | TODO: implement when relevant | Info | Catch-all for future trigger modes |

No blocker anti-patterns. All TODOs are documented design decisions for deferred interactive-choice features.

### Human Verification Required

### 1. UI Warning Badge Visibility

**Test:** Load a game with cards that have unimplemented mechanics (e.g., cards with unusual keywords). Check battlefield and hand views.
**Expected:** Small amber "!" badge appears at top-left corner of affected cards with tooltip "This card has mechanics not yet fully implemented"
**Why human:** Visual appearance, badge positioning, tooltip readability cannot be verified programmatically.

### 2. Evasion Keywords in Live Game

**Test:** Play a game with creatures having Fear/Intimidate/Skulk/Horsemanship. Attempt to assign illegal blockers.
**Expected:** Blocking attempts that violate evasion rules are rejected with appropriate error messages.
**Why human:** Real-time combat interaction flow, error message presentation.

### 3. Prowess Trigger in Game Flow

**Test:** Control a creature with Prowess, cast a noncreature spell.
**Expected:** Prowess triggers, creature gets +1/+1 until end of turn.
**Why human:** Trigger timing, visual feedback of the pump effect, stack interaction.

---

_Verified: 2026-03-09T16:00:00Z_
_Verifier: Claude (gsd-verifier)_
