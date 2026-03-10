---
phase: 21-schema-mtgjson-foundation
verified: 2026-03-10T19:30:00Z
status: passed
score: 4/4 ROADMAP success criteria verified; 7/7 Plan 02 truths verified (gap closed by Plan 04)
re_verification:
  previous_status: gaps_found
  previous_score: 3/4 ROADMAP (all 4 passed); 6/7 Plan 02 truths
  gaps_closed:
    - "Effect handlers receive typed data via ResolvedAbility.effect — Plan 04 added effect: Effect field to ResolvedAbility, replaced build_registry() + string dispatch with match &ability.effect, and updated all 39 construction sites"
  gaps_remaining: []
  regressions: []
---

# Phase 21: Schema & MTGJSON Foundation Verification Report

**Phase Goal:** The engine has a validated, schema-documented ability format and can load card metadata from MTGJSON
**Verified:** 2026-03-10T19:30:00Z
**Status:** passed
**Re-verification:** Yes — after gap closure (Plan 04 closed the typed dispatch gap)

## Goal Achievement

### ROADMAP Success Criteria (Primary Truths)

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | `cargo test` includes a passing test that loads card metadata (name, mana cost, types, P/T, colors, keywords, oracle text, layout) from MTGJSON for a known card | VERIFIED | `database::mtgjson::tests::find_lightning_bolt` passes; 12 additional mtgjson tests pass. Fixture at `data/mtgjson/test_fixture.json` has Lightning Bolt with scryfallOracleId, manaCost, types, text. |
| 2 | A hand-authored ability JSON file for a test card deserializes into typed AbilityDefinition/TriggerDefinition/StaticDefinition/ReplacementDefinition types without error | VERIFIED | `schema::tests::lightning_bolt_deserializes` passes; asserts `Effect::DealDamage { amount: Fixed(3), target: Any }`. `data/abilities/lightning_bolt.json` exists with correct typed JSON. |
| 3 | Running `cargo test` produces (or validates against) a JSON Schema file that documents every field in the ability format, usable for editor autocompletion | VERIFIED | `schema::tests::generate_ability_schema_and_write_file` passes; `data/abilities/schema.json` exists (35KB, 1491 lines); schema contains AbilityFile, Effect, DealDamage definitions. |
| 4 | Round-trip test: an ability JSON file serialized from Rust types and deserialized back produces identical typed structures | VERIFIED | `schema::tests::ability_json_roundtrip` passes; JSON values compared structurally after serialize-deserialize cycle. |

**Score:** 4/4 ROADMAP success criteria verified

### Plan 01 Observable Truths

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | Effect enum has typed variants with fields for all 38 registry entries plus an Other fallback | VERIFIED | `effect_has_39_variants` test asserts len == 39. `pub enum Effect` at ability.rs:56 with DealDamage, Draw, Pump, ... Discard, Other variants. Regression: confirmed enum still exists. |
| 2 | StaticMode enum has typed variants for all 15+ registered static modes | VERIFIED | `statics.rs` has 16 named variants + Other fallback. `parse_known_static_modes` test passes. |
| 3 | ReplacementEvent enum has typed variants for known replacement events | VERIFIED | `replacements.rs` has 10 named variants + Other fallback. |
| 4 | AbilityDefinition uses typed Effect enum instead of api_type String + HashMap params | VERIFIED | `pub struct AbilityDefinition { kind: AbilityKind, effect: Effect, cost: ..., sub_ability: ..., remaining_params: ... }` at ability.rs:603. Regression: confirmed. |
| 5 | TriggerDefinition, StaticDefinition, ReplacementDefinition use typed enums instead of String mode/event + HashMap params | VERIFIED | TriggerDefinition has `mode: TriggerMode`; StaticDefinition has `mode: StaticMode`; ReplacementDefinition has `event: ReplacementEvent`. |
| 6 | All definition types derive Serialize, Deserialize, and JsonSchema | VERIFIED | All definition types and enums show `#[derive(... Serialize, Deserialize, JsonSchema)]`. Clippy passes with no warnings. |
| 7 | TargetSpec and DamageAmount supporting types exist with typed variants | VERIFIED | `pub enum TargetSpec` (6 variants) and `pub enum DamageAmount` (Fixed, Variable) both defined with full serde/schema derives. |

**Score:** 7/7 Plan 01 truths verified

### Plan 02 Observable Truths

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | CardFace.abilities is Vec<AbilityDefinition> not Vec<String> | VERIFIED | `card.rs` — `pub abilities: Vec<AbilityDefinition>`. Regression: `grep -c` confirms 1 match on card.rs. |
| 2 | GameObject.abilities is Vec<AbilityDefinition> not Vec<String> | VERIFIED | `game_object.rs` — `pub abilities: Vec<AbilityDefinition>`. Regression: `grep -c` confirms 2 matches on game_object.rs. |
| 3 | BackFaceData.abilities is Vec<AbilityDefinition> not Vec<String> | VERIFIED | `game_object.rs:24` — `pub abilities: Vec<AbilityDefinition>`. |
| 4 | FaceDownData.abilities is Vec<AbilityDefinition> not Vec<String> | VERIFIED | `morph.rs:22` — `pub abilities: Vec<AbilityDefinition>`. |
| 5 | No runtime string parsing of abilities -- typed data flows from CardFace through to effect resolution | VERIFIED | `deck_loading.rs:137` — `obj.abilities = card_face.abilities.clone()` (direct typed clone, no re-parsing). |
| 6 | All existing tests pass after the refactor | VERIFIED | `cargo test --all` — 626 engine + 55 forge-ai + 28 server-core = 709 tests pass, 0 failures, 1 ignored. |
| 7 | Effect handlers receive typed data via ResolvedAbility.effect | VERIFIED | **Gap closed by Plan 04.** `ResolvedAbility` now has `pub effect: Effect` field (ability.rs:707). `resolve_effect()` dispatches via `match &ability.effect` (effects/mod.rs:42). `casting.rs` populates `effect: ability_def.effect.clone()` at both construction sites (lines 98, 237). `planeswalker.rs::build_pw_resolved()` and `triggers.rs::build_triggered_ability()` also use typed Effect. `build_registry()` is gone — confirmed by grep returning zero matches. |

**Score:** 7/7 Plan 02 truths verified

### Plan 03 Observable Truths

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | cargo test includes a passing test that loads card metadata from MTGJSON for Lightning Bolt | VERIFIED | `database::mtgjson::tests::find_lightning_bolt` passes. |
| 2 | A hand-authored ability JSON file (lightning_bolt.json) deserializes into typed AbilityDefinition without error | VERIFIED | `schema::tests::lightning_bolt_deserializes` passes. File exists at `data/abilities/lightning_bolt.json`. |
| 3 | Running cargo test produces a JSON Schema file that documents every field in the ability format | VERIFIED | `schema::tests::generate_ability_schema_and_write_file` passes and writes `data/abilities/schema.json` (35KB). |
| 4 | Round-trip test: ability JSON serialized from Rust types and deserialized back produces identical typed structures | VERIFIED | `schema::tests::ability_json_roundtrip` passes. |
| 5 | Schema snapshot via insta ensures schema stability across commits | VERIFIED | Snapshot file exists at `crates/engine/src/schema/snapshots/engine__schema__tests__ability_schema.snap`. `ability_schema_snapshot` test passes. |

**Score:** 5/5 Plan 03 truths verified

### Plan 04 Observable Truths (Gap Closure)

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | ResolvedAbility has an effect: Effect field carrying typed data | VERIFIED | `pub effect: Effect` confirmed at ability.rs:707. |
| 2 | resolve_effect() dispatches via match on Effect enum, not string HashMap lookup | VERIFIED | `match &ability.effect {` at effects/mod.rs:42. `build_registry()` absent — grep returns zero matches across entire src tree. |
| 3 | Production code (casting, triggers, planeswalker) builds ResolvedAbility with typed Effect from AbilityDefinition | VERIFIED | `casting.rs`: `effect: ability_def.effect.clone()` at lines 98 and 237. `planeswalker.rs::build_pw_resolved()`: `effect: ability_def.effect.clone()` at line 165. `triggers.rs::build_triggered_ability()`: `effect: ability_def.effect.clone()` at line 179. Prowess trigger uses `Effect::Pump { power: 1, toughness: 1, target: TargetSpec::None }` directly (line 93). |
| 4 | All 627+ engine tests still pass after the refactor | VERIFIED | `cargo test --all` — 626 passed, 0 failed, 1 ignored. forge-ai: 55 passed. server-core: 28 passed. Total: 709 pass. Commit 7d18efa verified in git history. |

**Score:** 4/4 Plan 04 truths verified

## Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/engine/src/types/ability.rs` | ResolvedAbility with effect: Effect field, new() and from_raw() constructors; Effect enum (39 variants); AbilityDefinition with typed fields | VERIFIED | `pub effect: Effect` at line 707. `ResolvedAbility::new()` and `::from_raw()` constructors added. Effect enum at line 56 with 39 variants. |
| `crates/engine/src/game/effects/mod.rs` | resolve_effect() using match on ability.effect; build_registry() removed; is_known_effect() added | VERIFIED | `match &ability.effect` at line 42. `build_registry()` absent (zero grep matches). `pub fn is_known_effect(api_type: &str) -> bool` exported and used by coverage.rs. |
| `crates/engine/src/game/casting.rs` | ResolvedAbility constructed with effect: ability_def.effect.clone() | VERIFIED | Both construction sites (lines ~98, ~237) use `effect: ability_def.effect.clone()`. |
| `crates/engine/src/game/planeswalker.rs` | build_pw_resolved() uses typed Effect from AbilityDefinition | VERIFIED | `effect: ability_def.effect.clone()` at line 165. |
| `crates/engine/src/game/triggers.rs` | build_triggered_ability() uses typed Effect; Prowess uses Effect::Pump | VERIFIED | `effect: ability_def.effect.clone()` at line 179. Prowess synthetic trigger uses `Effect::Pump { power: 1, toughness: 1, target: TargetSpec::None }`. |
| `crates/engine/src/types/statics.rs` | StaticMode typed enum | VERIFIED | 150 lines. `pub enum StaticMode` with 16 named variants + Other. |
| `crates/engine/src/types/replacements.rs` | ReplacementEvent typed enum | VERIFIED | 127 lines. `pub enum ReplacementEvent` with 10 named variants + Other. |
| `crates/engine/src/database/mtgjson.rs` | MTGJSON AtomicCards deserialization types and loader | VERIFIED | AtomicCardsFile, AtomicCard, AtomicIdentifiers structs. load_atomic_cards, find_card, parse_mtgjson_mana_cost functions. |
| `data/mtgjson/test_fixture.json` | MTGJSON card metadata (test fixture with 7 cards) | VERIFIED | File exists with Lightning Bolt, Grizzly Bears, Counterspell, Wrath of God, Delver, Mox Pearl, Sphinx. |
| `data/abilities/lightning_bolt.json` | First hand-authored ability JSON file | VERIFIED | File exists with `$schema` reference, DealDamage effect with Fixed(3) amount, Any target. |
| `data/abilities/schema.json` | Auto-generated JSON Schema for ability format | VERIFIED | 35KB, 1491 lines, generated by schemars from Rust types. Contains all Effect variants. |
| `crates/engine/src/schema/mod.rs` | Schema generation logic | VERIFIED | AbilityFile struct defined, generate_schema() using schema_for!(AbilityFile). |
| `crates/engine/src/schema/snapshots/engine__schema__tests__ability_schema.snap` | Insta snapshot for schema stability | VERIFIED | File exists, contains full JSON schema snapshot. |

## Key Link Verification

| From | To | Via | Status | Details |
|------|----|-----|--------|---------|
| `casting.rs` | `types/ability.rs` | `effect: ability_def.effect.clone()` in ResolvedAbility construction | VERIFIED | Confirmed at both ResolvedAbility construction sites in casting.rs (lines ~98, ~237). |
| `planeswalker.rs` | `types/ability.rs` | `effect: ability_def.effect.clone()` in build_pw_resolved() | VERIFIED | Line 165 confirmed. |
| `triggers.rs` | `types/ability.rs` | `effect: ability_def.effect.clone()` in build_triggered_ability() | VERIFIED | Line 179 confirmed. Prowess uses direct typed Effect::Pump. |
| `effects/mod.rs` | `effects/*.rs` handlers | `match &ability.effect { Effect::DealDamage { .. } => deal_damage::resolve(...), ... }` | VERIFIED | 38-arm match statement at line 42. Each arm calls the corresponding handler module. Effect::Other returns Err(Unregistered). |
| `coverage.rs` | `effects/mod.rs` | `is_known_effect(api_type)` for coverage gate | VERIFIED | `use crate::game::effects::is_known_effect` at coverage.rs:6. Called at lines 50, 111, 159. |
| `database/mtgjson.rs` | `data/mtgjson/test_fixture.json` | serde_json deserialization into AtomicCardsFile | VERIFIED | `load_atomic_cards` deserializes JSON. `find_lightning_bolt` test proves it. |
| `data/abilities/lightning_bolt.json` | `types/ability.rs` | serde deserialization into AbilityDefinition types | VERIFIED | `lightning_bolt_deserializes` test imports the file and asserts typed fields match. |
| `schema/mod.rs` | `data/abilities/schema.json` | schemars schema_for writes to file | VERIFIED | `generate_ability_schema_and_write_file` test writes to `data/abilities/schema.json` on each test run. |

## Requirements Coverage

| Requirement | Source Plans | Description | Status | Evidence |
|-------------|-------------|-------------|--------|----------|
| DATA-01 | Plan 03 | Engine loads card metadata (name, mana cost, types, P/T, colors, keywords, oracle text, layout) from MTGJSON AtomicCards.json using custom Rust types | SATISFIED | `mtgjson.rs` defines AtomicCard with all required fields. Test fixture contains Lightning Bolt with all metadata. 13 passing tests including `find_lightning_bolt`, `find_creature_with_power_toughness`. |
| DATA-02 | Plans 01, 02, 04 | Engine defines a typed JSON ability schema mapping to AbilityDefinition, TriggerDefinition, StaticDefinition, and ReplacementDefinition types | SATISFIED | All four definition types defined with typed enum fields. ResolvedAbility carries typed `effect: Effect` field threaded through to handlers. Effect dispatch is now fully typed via match. Serde round-trip tests pass for all types. |
| DATA-04 | Plan 03 | Ability JSON schema exports a JSON Schema definition via schemars for editor autocompletion and build-time validation | SATISFIED | `schema/mod.rs::generate_schema()` uses `schemars::schema_for!(AbilityFile)`. `data/abilities/schema.json` exists (35KB). `lightning_bolt.json` has `$schema` reference. |

**No orphaned requirements:** REQUIREMENTS.md maps DATA-01, DATA-02, DATA-04 to Phase 21. All three are claimed by plans and satisfied. No other requirement IDs from REQUIREMENTS.md are mapped to Phase 21.

## Anti-Patterns Found

None. Previous warnings (string-based dispatch, compat bridge on ResolvedAbility) have been resolved by Plan 04. No blocker anti-patterns exist.

| File | Pattern | Severity | Status |
|------|---------|----------|--------|
| `effects/mod.rs` | String-based HashMap registry | (was Warning) | RESOLVED — match on Effect enum now used. build_registry() removed. |
| `ability.rs: ResolvedAbility` | api_type: String retained as a backward compat field | Info | Acceptable — api_type is still populated via `effect.api_type()` for EffectResolved events and coverage analysis. It is not used for dispatch. |

## Human Verification Required

None — all checks are verifiable programmatically. Schema generation, file existence, test execution, type structure, and dispatch mechanism are all confirmed.

## Re-verification Summary

**Gap from previous verification:** "Effect handlers receive typed data via ResolvedAbility.effect" — Plan 02 left ResolvedAbility with string-based dispatch as a transitional state.

**Gap closure evidence (Plan 04, commit 7d18efa):**
- `ResolvedAbility.effect: Effect` field added and confirmed at ability.rs:707
- `match &ability.effect` dispatch confirmed at effects/mod.rs:42
- `build_registry()` removed — zero grep matches across entire src tree
- All 3 production construction sites (casting.rs x2, planeswalker.rs, triggers.rs) confirmed using `ability_def.effect.clone()`
- All 709 tests pass (626 engine + 55 forge-ai + 28 server-core), 0 failures

**Regressions:** None. All previously-verified items continue to hold.

---

_Verified: 2026-03-10T19:30:00Z_
_Verifier: Claude (gsd-verifier)_
