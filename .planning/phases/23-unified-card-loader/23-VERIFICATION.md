---
phase: 23-unified-card-loader
verified: 2026-03-10T21:30:00Z
status: passed
score: 15/15 must-haves verified
re_verification: false
---

# Phase 23: Unified Card Loader Verification Report

**Phase Goal:** The engine can load cards from MTGJSON metadata + ability JSON as its primary path, proven end-to-end with sample cards
**Verified:** 2026-03-10T21:30:00Z
**Status:** passed
**Re-verification:** No — initial verification

## Goal Achievement

### Observable Truths (from ROADMAP.md Success Criteria)

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | `CardDatabase::load_json()` loads a card by merging MTGJSON metadata with an ability JSON file and produces a valid CardFace that the engine can use to play a game | VERIFIED | `card_db.rs:16-21` delegates to `json_loader::load_json()`; `test_load_all_smoke_test_cards` confirms ≥8 cards load with 0 errors; `test_smoke_game_cast_spell` uses the returned CardFace through `create_object_from_card_face()` and `apply()` |
| 2 | Multi-face cards (Adventure, Transform, Modal DFC) load correctly with both faces populated | VERIFIED | `test_delver_transform_layout` asserts `CardLayout::Transform("Delver of Secrets", "Insectile Aberration")`; `test_giant_killer_adventure_layout` asserts `CardLayout::Adventure("Giant Killer", "Chop Down")`; both face names verified correct |
| 3 | Loaded cards include MTGJSON scryfallOracleId and the frontend can use it for image lookups via Scryfall API | VERIFIED | `test_scryfall_oracle_id_populated` asserts `bolt.scryfall_oracle_id.is_some()`; fixture has 12 `scryfallOracleId` values; `build_card_face()` at `json_loader.rs:121` sets `scryfall_oracle_id: oracle_id` |
| 4 | A smoke test game using 5-10 JSON-loaded cards completes without errors (cards can be cast, abilities resolve, combat works) | VERIFIED | `test_smoke_game_cast_spell`: Lightning Bolt deals 3 damage, P1 life 20→17; `test_smoke_game_combat_damage`: Grizzly Bears deal 2 combat damage, P1 life 20→18; both tests drive `apply()` pipeline end-to-end |

**Score:** 4/4 roadmap success criteria verified

### Plan-Level Must-Haves (Plan 01)

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | `CardDatabase::load_json()` merges MTGJSON metadata + ability JSON into valid CardFace/CardRules | VERIFIED | `card_db.rs:16-21`; 22 unit tests in `json_loader.rs` all pass |
| 2 | Single-face cards load with correct metadata (name, mana cost, types, P/T, keywords, oracle text) | VERIFIED | `build_card_face_produces_correct_fields` unit test; `test_load_all_smoke_test_cards` integration test |
| 3 | Multi-face cards (Transform, Adventure) load with both faces populated correctly | VERIFIED | `load_json_multi_face_card` unit test; `test_delver_transform_layout`, `test_giant_killer_adventure_layout` |
| 4 | Basic lands get synthesized `{T}: Add {color}` mana abilities per CR 305.6 | VERIFIED | `synthesize_basic_land_mana_forest`, `_plains`, `_dual_type`, `_no_land_subtype` unit tests; `test_forest_has_synthesized_mana_ability` integration test |
| 5 | Equipment cards get synthesized Equip activated ability from keywords per CR 702.6 | VERIFIED | `synthesize_equip_with_cost`, `_no_keyword`, `_variant_cost` unit tests; `test_bonesplitter_has_synthesized_equip_ability` integration test confirms `Effect::Attach` + `AbilityCost::Mana { cost: "1" }` |
| 6 | Planeswalker loyalty costs read from `AbilityCost::Loyalty` (not just remaining_params) | VERIFIED | `parse_loyalty_cost()` at `planeswalker.rs:149-159` checks `AbilityCost::Loyalty` first; `parse_loyalty_cost_prefers_typed_ability_cost` unit test; `test_jace_loyalty_abilities` confirms +2/0/-1/-12 |
| 7 | `scryfall_oracle_id` populated from MTGJSON `identifiers.scryfallOracleId` | VERIFIED | `build_card_face_populates_scryfall_oracle_id` unit test; `test_scryfall_oracle_id_populated` integration test |
| 8 | Missing ability files collected as errors (not panics) | VERIFIED | `load_json_reports_missing_ability_files` unit test asserts `errors()[0].1.contains("No MTGJSON match")` |

### Plan-Level Must-Haves (Plan 02)

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | 8 smoke test cards load from JSON without errors | VERIFIED | `test_load_all_smoke_test_cards` passes: `card_count() >= 8`, `errors().is_empty()` |
| 2 | Smoke test game: cast spell, resolve, verify effect | VERIFIED | `test_smoke_game_cast_spell`: P1 life = 17 after Lightning Bolt |
| 3 | Smoke test game: combat phase, creatures deal damage, life totals change | VERIFIED | `test_smoke_game_combat_damage`: P1 life = 18 after Grizzly Bears attack |
| 4 | Forest has synthesized `{T}: Add {G}` mana ability | VERIFIED | `test_forest_has_synthesized_mana_ability` |
| 5 | Rancor has Enchant + triggered return-to-hand ability | VERIFIED | `rancor.json` contains Spell (Attach), trigger (ChangesZone), 2 statics (pump/Trample); loaded via cross-validation test |
| 6 | Bonesplitter has synthesized Equip activated ability from keyword per CR 702.6 | VERIFIED | `test_bonesplitter_has_synthesized_equip_ability` passes |
| 7 | Jace has 4 loyalty abilities with correct `AbilityCost::Loyalty` costs (+2, 0, -1, -12) | VERIFIED | `test_jace_loyalty_abilities` confirms all four costs |
| 8 | Delver of Secrets loads as Transform with two faces | VERIFIED | `test_delver_transform_layout` passes |
| 9 | Giant Killer loads as Adventure with creature + adventure spell | VERIFIED | `test_giant_killer_adventure_layout` passes |
| 10 | Every ability JSON file matches its MTGJSON entry (cross-validation) | VERIFIED | `test_cross_validation_ability_files_match_mtgjson` passes |
| 11 | `scryfall_oracle_id` is present on loaded cards | VERIFIED | `test_scryfall_oracle_id_populated` passes |

**Total score:** 15/15 must-haves verified

## Required Artifacts

### Plan 01 Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/engine/src/database/json_loader.rs` | Merge logic, synthesis, `load_json()` | VERIFIED | 926 lines (min: 150); all required functions present and substantive |
| `crates/engine/src/types/card.rs` | `CardFace` with `scryfall_oracle_id` field | VERIFIED | `scryfall_oracle_id: Option<String>` with `#[serde(default)]` at line 30-31 |
| `crates/engine/src/schema/mod.rs` | `FaceAbilities` struct, `AbilityFile.faces` field | VERIFIED | `FaceAbilities` at lines 10-23; `faces: Vec<FaceAbilities>` at line 45 |

### Plan 02 Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `data/abilities/forest.json` | Empty ability file (mana synthesized by loader) | VERIFIED | Exists; empty abilities/triggers/statics/replacements |
| `data/abilities/rancor.json` | Enchant + pump static + dies-return trigger | VERIFIED | Spell(Attach), ChangesZone trigger, 2 Continuous statics |
| `data/abilities/bonesplitter.json` | Equipment pump static (Equip synthesized) | VERIFIED | Continuous pump static; no Equip in JSON (synthesized correctly) |
| `data/abilities/jace_the_mind_sculptor.json` | 4 loyalty abilities with `AbilityCost::Loyalty` | VERIFIED | 4 abilities with `{"type":"Loyalty","amount":N}` costs |
| `data/abilities/delver_of_secrets.json` | Multi-face with faces array | VERIFIED | `faces` array with 2 entries; upkeep transform trigger on face 0 |
| `data/abilities/giant_killer.json` | Adventure multi-face ability file | VERIFIED | `faces` array: creature tap ability + destroy spell |
| `crates/engine/tests/json_smoke_test.rs` | Integration test for load + gameplay | VERIFIED | 449 lines (min: 50); 10 tests covering all required scenarios |

## Key Link Verification

### Plan 01 Key Links

| From | To | Via | Status | Details |
|------|----|-----|--------|---------|
| `json_loader.rs` | `mtgjson.rs` | `parse_mtgjson_mana_cost`, `AtomicCard` | WIRED | `json_loader.rs:7-8` imports `load_atomic_cards, parse_mtgjson_mana_cost, AtomicCard`; both functions called in production code |
| `json_loader.rs` | `schema/mod.rs` | `AbilityFile`, `FaceAbilities` deserialization | WIRED | `json_loader.rs:9` imports `AbilityFile, FaceAbilities`; used in `build_card_rules()` and `load_json()` |
| `json_loader.rs` | `types/card.rs` | Builds `CardFace`, `CardRules`, `CardLayout` | WIRED | `json_loader.rs:11` imports all three; `build_card_face()` returns `CardFace`, `build_card_rules()` returns `CardRules` |
| `game/planeswalker.rs` | `types/ability.rs` | `AbilityCost::Loyalty` checked first | WIRED | `planeswalker.rs:151`: `if let Some(crate::types::ability::AbilityCost::Loyalty { amount }) = &ability_def.cost` |

### Plan 02 Key Links

| From | To | Via | Status | Details |
|------|----|-----|--------|---------|
| `json_smoke_test.rs` | `card_db.rs` | `CardDatabase::load_json()` | WIRED | `json_smoke_test.rs:27`; called in every test via `load_test_db()` |
| `json_smoke_test.rs` | `game/deck_loading.rs` | `create_object_from_card_face()` | WIRED | `json_smoke_test.rs:11, 266, 282, 375`; used in both smoke game tests |
| `json_smoke_test.rs` | `game/engine.rs` | `apply()` for casting spells and combat | WIRED | `json_smoke_test.rs:10, 311, 329, 341, 342, 389-437`; full pipeline exercised |
| `data/abilities/*.json` | `data/mtgjson/test_fixture.json` | Card names match between ability files and MTGJSON | WIRED | `test_cross_validation_ability_files_match_mtgjson` confirms all 8 ability files match MTGJSON entries |

## Requirements Coverage

| Requirement | Source Plans | Description | Status | Evidence |
|-------------|-------------|-------------|--------|----------|
| DATA-03 | 23-01-PLAN, 23-02-PLAN | `CardDatabase::load_json()` merges MTGJSON metadata + ability JSON into CardFace, becoming the primary card loading path | SATISFIED | `card_db.rs:16-21` — public API; 22 unit tests + 10 integration tests; smoke game proves it's a functional primary path |
| MIGR-04 | 23-01-PLAN, 23-02-PLAN | Card data includes MTGJSON scryfallOracleId for reliable frontend image lookups via Scryfall API | SATISFIED | `CardFace.scryfall_oracle_id` field; `test_fixture.json` has 12 `scryfallOracleId` values; `test_scryfall_oracle_id_populated` confirms Lightning Bolt has it |

**No orphaned requirements:** REQUIREMENTS.md Traceability maps DATA-03 and MIGR-04 to Phase 23 only, and both plans claim both requirements. Both are satisfied.

## Anti-Patterns Found

None detected. Scanned `json_loader.rs` and `json_smoke_test.rs` for TODO/FIXME/HACK/placeholder patterns — all clean. No stub implementations, empty handlers, or incomplete logic found.

## Test Results

All tests pass as of verification:

```
cargo test --all
  engine unit tests: 672 passed, 0 failed
  json_smoke_test:   10 passed, 0 failed
  rules tests:       42 passed, 0 failed
  server-core:       28 passed, 0 failed

cargo clippy --all-targets -- -D warnings
  Finished — 0 warnings, 0 errors
```

## Human Verification Required

None. All success criteria are verifiable programmatically via the passing test suite.

---

_Verified: 2026-03-10T21:30:00Z_
_Verifier: Claude (gsd-verifier)_
