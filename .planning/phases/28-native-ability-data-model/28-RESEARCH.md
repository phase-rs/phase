# Phase 28: Native Ability Data Model - Research

**Researched:** 2026-03-10
**Domain:** Rust type system design, serde data migration, card data pipeline
**Confidence:** HIGH

## Summary

Phase 28 eliminates all Forge scripting DSL remnants from the runtime data model. The current system has typed enums (`Effect`, `TriggerMode`, `StaticMode`, `ReplacementEvent`) introduced in Phase 21, but these wrap `HashMap<String, String>` params bags for trigger conditions, static ability effects, replacement behavior, and SubAbility chains. At runtime, 15,930 triggers use `Execute` params pointing to raw Forge SVar strings (`"DB$ Draw | NumCards$ 1"`) that get parsed via `parse_ability()` -- a Forge string parser that should be fully gated behind `forge-compat`.

The transformation is large (32,274 card files, 158 trigger param keys, 165 static param keys, 72 replacement param keys, 623 remaining_params keys) but mechanical. The engine only *consumes* roughly 25-30 distinct param keys at runtime. The vast majority of params are metadata (AI hints, descriptions, conditions the engine doesn't implement yet) that can be categorized as `Option<T>` typed fields or moved to an `unimplemented: HashMap<String, String>` catch-all that is explicitly NOT used at runtime (purely for data preservation during migration).

**Primary recommendation:** Work incrementally -- convert the ~25 runtime-consumed param keys to typed struct fields first, then migrate the remaining keys to typed `Option<T>` fields or a separate metadata struct, and finally run a batch migration script across all 32K JSON files.

<user_constraints>

## User Constraints (from CONTEXT.md)

### Locked Decisions

- **No `remaining_params` catch-all.** The `remaining_params: HashMap<String, String>` field on `AbilityDefinition` must be eliminated entirely. Every parameter currently flowing through `remaining_params` (619 unique Forge-style keys) must be mapped to typed fields on the appropriate struct.
- **No `Effect::Other` with Forge params.** The `Effect::Other { api_type, params }` variant must be removed or replaced with specific typed variants for every effect the engine actually handles. Effects the engine doesn't handle yet should be represented as `Effect::Unimplemented { name: String }` or similar -- NOT as a bag of Forge strings.
- **No SVar strings.** The `svars: HashMap<String, String>` field on `CardFace` must be eliminated. All SubAbility chaining currently done via `Execute` -> raw SVar string -> `parse_ability()` must be converted to typed SubAbility references resolved at data-load time.
- **No `parse_ability()` at runtime.** The `parser::ability` module (currently ungated) must be gated behind `forge-compat` or removed entirely. Zero Forge string parsing at runtime.
- **No Forge parameter key names.** Keys like `Execute`, `ValidCard`, `SubAbility`, `TriggerDescription`, `AddKeyword`, `Affected`, etc. must not appear as string keys in any runtime data structure. They become typed struct fields.

### Claude's Discretion

- Exact typed struct designs for trigger/static/replacement definitions
- Whether to process the migration incrementally (triggers -> statics -> replacements -> abilities) or all at once
- How to handle the long tail of rarely-used Forge params -- typed `Option<T>` fields vs grouped sub-structs
- Whether SubAbility chains are inlined (nested structs) or referenced by index/name
- Schema versioning strategy for the JSON files
- How to handle cards whose abilities the engine doesn't yet fully implement (typed `Unimplemented` variant vs omission)

### Deferred Ideas (OUT OF SCOPE)

None -- this phase is the final step in data model independence.

</user_constraints>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| serde | current | JSON serialization/deserialization | Already used throughout, #[serde(tag, default, flatten)] |
| schemars | current | JSON Schema generation from Rust types | Already integrated, auto-generates schema.json |
| serde_json | current | JSON parsing and writing | Already used for ability files |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| rayon | already in deps | Parallel iteration for migration script | Processing 32K files in batch |
| insta | already in deps | Snapshot testing | Verify JSON format changes |

No new dependencies required -- this is purely a type system refactor using existing infrastructure.

## Architecture Patterns

### Current Architecture (What Must Change)

```
data/abilities/*.json
    |
    v
AbilityFile (serde) --> CardFace { svars: HashMap, triggers: Vec<TriggerDef{params: HashMap}>, ... }
    |
    v
GameObject { svars: HashMap, trigger_definitions: Vec<TriggerDef{params: HashMap}>, ... }
    |
    v (at runtime, when trigger fires)
build_triggered_ability() --> parse_ability(svar_string) --> ResolvedAbility { params: HashMap, svars: HashMap }
    |
    v (during resolution)
resolve_ability_chain() --> ability.params.get("SubAbility") --> ability.svars.get(name) --> parse_ability()
```

### Target Architecture

```
data/abilities/*.json (new format: typed fields, no params HashMaps)
    |
    v
AbilityFile (serde) --> CardFace { triggers: Vec<TriggerDef{...typed fields...}>, ... }
                                   (NO svars field)
    |
    v
GameObject { trigger_definitions: Vec<TriggerDef{...typed fields...}>, ... }
              (NO svars field)
    |
    v (at runtime, when trigger fires)
build_triggered_ability() --> trigger_def.execute (typed Option<Box<AbilityDefinition>>)
                              --> ResolvedAbility { effect: Effect, sub_ability: Option<Box<...>>, ... }
    |
    v (during resolution)
resolve_ability_chain() --> ability.sub_ability --> resolve recursively (no string parsing)
```

### Pattern 1: Typed Definition Structs

**What:** Replace `params: HashMap<String, String>` with typed struct fields on each definition type.

**Approach:** Only the ~25 keys the engine actually reads at runtime need typed fields. The remaining keys (AI hints, descriptions, unimplemented conditions) go into grouped metadata sub-structs or are dropped if purely Forge-specific.

**Trigger example -- keys actually consumed at runtime:**
```rust
pub struct TriggerDefinition {
    pub mode: TriggerMode,
    // Zone transition params (ChangesZone triggers)
    pub origin: Option<String>,       // was params["Origin"]
    pub destination: Option<String>,   // was params["Destination"]
    // Target/source filtering
    pub valid_card: Option<String>,    // was params["ValidCard"]
    pub valid_source: Option<String>,  // was params["ValidSource"]
    pub valid_player: Option<String>,  // was params["ValidActivatingPlayer"] / params["ValidPlayer"]
    pub valid_token: Option<String>,   // was params["ValidToken"]
    // Counter specifics
    pub counter_type: Option<String>,  // was params["CounterType"]
    // Damage
    pub damage_amount: Option<u32>,    // was params["DamageAmount"]
    pub combat_damage: Option<bool>,   // was params["CombatDamage"]
    // Phase trigger
    pub phase: Option<String>,         // was params["Phase"]
    // Ability to execute when triggered
    pub execute: Option<Box<AbilityDefinition>>,  // was params["Execute"] + svars lookup + parse_ability()
    // Description (kept for UI/logging, not runtime logic)
    pub description: Option<String>,   // was params["TriggerDescription"]
    // Trigger zones
    pub trigger_zones: Option<String>, // was params["TriggerZones"]
    // Metadata the engine does not consume at runtime
    pub metadata: Option<TriggerMetadata>,
}

pub struct TriggerMetadata {
    // AI hints, optional deciders, conditions the engine doesn't implement
    pub ai_logic: Option<String>,
    pub optional_decider: Option<String>,
    // ... other non-runtime keys as needed
}
```

### Pattern 2: Static Definition Typed Fields

**Keys consumed at runtime by layers.rs and static_abilities.rs:**
```rust
pub struct StaticDefinition {
    pub mode: StaticMode,
    // Layer system fields (consumed by layers.rs)
    pub affected: Option<String>,       // was params["Affected"]
    pub add_power: Option<i32>,         // was params["AddPower"]
    pub add_toughness: Option<i32>,     // was params["AddToughness"]
    pub set_power: Option<i32>,         // was params["SetPower"]
    pub set_toughness: Option<i32>,     // was params["SetToughness"]
    pub add_keyword: Option<String>,    // was params["AddKeyword"]
    pub remove_keyword: Option<String>, // was params["RemoveKeyword"]
    pub add_type: Option<String>,       // was params["AddType"]
    pub remove_type: Option<String>,    // was params["RemoveType"]
    pub set_color: Option<String>,      // was params["SetColor"]
    pub add_color: Option<String>,      // was params["AddColor"]
    pub add_ability: Option<String>,    // was params["AddAbility"]
    pub remove_all_abilities: Option<bool>, // was params.contains_key("RemoveAllAbilities")
    // Devotion check (layers.rs)
    pub check_svar: Option<String>,     // was params["CheckSVar"]
    pub svar_compare: Option<String>,   // was params["SVarCompare"]
    // Rule modification fields
    pub cost: Option<String>,           // was params["Cost"] (for Ward)
    pub target: Option<String>,         // was params["Target"] (for Protection)
    // Description
    pub description: Option<String>,
    // Metadata
    pub metadata: Option<StaticMetadata>,
}
```

### Pattern 3: Replacement Definition Typed Fields

**Keys consumed at runtime by replacement.rs:**
```rust
pub struct ReplacementDefinition {
    pub event: ReplacementEvent,
    // Zone fields
    pub active_zones: Option<String>,    // was params["ActiveZones"]
    pub origin: Option<String>,          // was params["Origin$"]
    pub destination: Option<String>,     // was params["Destination$"]
    pub new_destination: Option<String>, // was params["NewDestination$"]
    // Source/target filtering
    pub valid_card: Option<String>,      // was params["ValidCard"]
    pub valid_source: Option<String>,    // was params["ValidSource"]
    pub valid_player: Option<String>,    // was params["ValidPlayer"]
    // Behavior
    pub prevent: Option<bool>,           // was params["Prevent"] == "True"
    pub exile: Option<bool>,             // was params["Exile"] == "True"
    pub double: Option<bool>,            // was params["Double"] == "True"
    pub new_amount: Option<String>,      // was params["NewAmount$"]
    pub new_count: Option<String>,       // was params["NewCount$"]
    pub new_name: Option<String>,        // was params["NewName$"]
    pub damage_type: Option<String>,     // was params["DamageType$"]
    pub layer: Option<String>,           // was params["Layer"]
    // Description
    pub description: Option<String>,
    // Metadata
    pub metadata: Option<ReplacementMetadata>,
}
```

### Pattern 4: SubAbility Chain Resolution at Load Time

**What:** Convert `Execute` -> SVar string -> runtime `parse_ability()` into typed `Option<Box<AbilityDefinition>>` resolved when the JSON is loaded.

**Current flow (runtime):**
```
trigger fires -> trig_def.params["Execute"] = "TrigDraw"
              -> obj.svars["TrigDraw"] = "DB$ Draw | NumCards$ 1"
              -> parse_ability("DB$ Draw | NumCards$ 1")
              -> AbilityDefinition { effect: Draw { count: 1 }, ... }
```

**Target flow (load time):**
```
JSON loaded -> trigger.execute = Some(Box::new(AbilityDefinition {
                  effect: Draw { count: 1 },
                  ...
              }))

trigger fires -> trigger.execute.as_ref() -> already typed, no parsing
```

The migration script resolves all SVar references at migration time. The `execute` field on `TriggerDefinition` becomes `Option<Box<AbilityDefinition>>`. The `sub_ability` field on `AbilityDefinition` (already present but always `None`) gets populated with resolved chains.

### Pattern 5: Effect::Unimplemented Variant

**What:** Replace `Effect::Other { api_type, params }` with `Effect::Unimplemented { name: String }` for effects the engine doesn't handle.

```rust
pub enum Effect {
    // ... existing 38 typed variants ...
    /// Effect type recognized but not yet implemented by the engine.
    /// Cards with only Unimplemented effects are flagged via has_unimplemented_mechanics.
    Unimplemented { name: String },
}
```

No HashMap params on this variant. The name is preserved for coverage reporting.

### Anti-Patterns to Avoid

- **Partial migration:** Do not leave some definitions using typed fields and others using params HashMaps. Each definition type must be fully converted before merging.
- **Runtime string parsing:** Do not introduce any new string parsing of ability text at runtime. All parsing must happen at data-load time or in the migration script.
- **Over-typing rare metadata:** Do not create 600 typed fields for every Forge parameter. Group AI hints, conditions the engine skips, and descriptions into metadata sub-structs or a single `extra: HashMap<String, String>` if needed for round-trip preservation. But this must NOT be used by runtime logic -- it is purely for data migration fidelity.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| JSON migration of 32K files | Manual file-by-file editing | Migration binary with serde | Consistency, automation, round-trip validation |
| SVar chain resolution | Custom parser for chain walking | Existing `parse_ability()` at migration time only | Parser already handles all Forge syntax |
| JSON Schema generation | Manual schema documentation | `schemars::schema_for!(AbilityFile)` | Schema stays in sync with Rust types automatically |
| Parallel file processing | Manual threading | `rayon::par_iter` | Already in deps, handles work distribution |

**Key insight:** The migration binary should use the existing Forge parser (`parse_ability()`) to resolve SVars, then emit the result as typed structs. This is the one place where runtime Forge parsing is acceptable -- it runs once, then never again.

## Common Pitfalls

### Pitfall 1: Breaking Existing Tests
**What goes wrong:** Changing TriggerDefinition/StaticDefinition structs breaks every test that constructs them, plus all snapshot tests.
**Why it happens:** Tests in triggers.rs, layers.rs, static_abilities.rs, deck_loading.rs, replacement.rs all create definitions with `params: HashMap::from([...])`.
**How to avoid:** Change types and all tests in the same commit. Use a compatibility constructor if needed for transition.
**Warning signs:** `cargo test --all` failures after struct changes.

### Pitfall 2: SVar Chain Depth and Cycles
**What goes wrong:** Some cards chain SVars deeply (Execute -> SubAbility -> SubAbility -> ...). The migration script could infinite-loop on circular references.
**Why it happens:** Forge allows arbitrary SVar naming that could theoretically self-reference.
**How to avoid:** Track visited SVar names during resolution, limit depth to 10 (matching existing `MAX_CHAIN_DEPTH`).
**Warning signs:** Migration script hanging on specific cards.

### Pitfall 3: Serialization Format Change Breaks card-data.json Consumers
**What goes wrong:** `card-data.json` format changes break the frontend (TypeScript types), WASM bridge, and server.
**Why it happens:** `CardFace` is serialized via `serde` and deserialized in `CardDatabase::from_export()`.
**How to avoid:** The `from_export()` path uses the same `CardFace` struct. Changing the struct changes the format automatically. But TypeScript adapter/types.ts may need updates if it reads trigger/static/replacement data.
**Warning signs:** WASM build or server failing to load card-data.json after migration.

### Pitfall 4: Not Handling Missing SVars During Migration
**What goes wrong:** A trigger's `Execute` param points to an SVar name that doesn't exist in the card's SVar map.
**Why it happens:** 15,930 triggers reference SVars. Some SVars may be defined on the card but others may be dynamically generated or missing.
**How to avoid:** Migration script should handle missing SVars gracefully -- emit `execute: None` with a warning, not a hard error.
**Warning signs:** Migration script reporting thousands of "SVar not found" errors.

### Pitfall 5: HashMap Key Ordering Affecting Test Snapshots
**What goes wrong:** Removing params HashMaps changes JSON field ordering, breaking insta snapshot tests.
**Why it happens:** HashMaps have nondeterministic ordering; typed fields have deterministic ordering.
**How to avoid:** Update all affected insta snapshots after the format change. Use `--review` flag.
**Warning signs:** Snapshot test failures.

### Pitfall 6: The `to_params()` Bridge Method
**What goes wrong:** `Effect::to_params()` is used by the runtime SubAbility chain resolver and by `ResolvedAbility` construction. Removing params breaks these paths.
**Why it happens:** The current architecture flows: typed Effect -> `to_params()` -> HashMap -> string key lookups.
**How to avoid:** When SubAbility chains are resolved at load time, `to_params()` becomes unnecessary for runtime. Gate it behind `forge-compat` or remove it entirely after all consumers are migrated.
**Warning signs:** Runtime errors when resolving abilities after removing `to_params()`.

## Code Examples

### Example 1: Current TriggerDefinition in JSON (must change)
```json
{
  "mode": "ChangesZone",
  "params": {
    "Origin": "Any",
    "Execute": "TrigDraw",
    "TriggerDescription": "When this enters, draw a card.",
    "ValidCard": "Card.Self",
    "Destination": "Battlefield"
  }
}
```

### Example 2: Target TriggerDefinition in JSON
```json
{
  "mode": "ChangesZone",
  "origin": "Any",
  "destination": "Battlefield",
  "valid_card": "Card.Self",
  "description": "When this enters, draw a card.",
  "execute": {
    "kind": "Database",
    "effect": { "type": "Draw", "count": 1 }
  }
}
```

### Example 3: Current StaticDefinition in JSON (must change)
```json
{
  "mode": "Continuous",
  "params": {
    "Affected": "Creature.YouCtrl",
    "AddPower": "1",
    "AddToughness": "1",
    "Description": "Creatures you control get +1/+1."
  }
}
```

### Example 4: Target StaticDefinition in JSON
```json
{
  "mode": "Continuous",
  "affected": "Creature.YouCtrl",
  "add_power": 1,
  "add_toughness": 1,
  "description": "Creatures you control get +1/+1."
}
```

### Example 5: Migration Script Pseudocode
```rust
// Migration binary: reads old format, writes new format
fn migrate_trigger(old: &OldTriggerDef, svars: &HashMap<String, String>) -> NewTriggerDef {
    let execute = old.params.get("Execute").and_then(|svar_name| {
        svars.get(svar_name).and_then(|svar_str| {
            parse_ability(svar_str).ok().map(|def| Box::new(def))
        })
    });

    NewTriggerDef {
        mode: old.mode.clone(),
        origin: old.params.get("Origin").cloned(),
        destination: old.params.get("Destination").cloned(),
        valid_card: old.params.get("ValidCard").cloned(),
        execute,
        description: old.params.get("TriggerDescription").cloned(),
        // ... map remaining consumed keys
    }
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| `api_type: String` + raw params | Typed `Effect` enum with 38 variants | Phase 21 | Effect dispatch via match, not string lookup |
| `TriggerMode` as raw string | Typed `TriggerMode` enum | Phase 21 | Trigger registry uses typed keys |
| Forge `.txt` files as primary source | MTGJSON + ability JSON | Phase 23-24 | Forge files removed, parser gated |
| `params: HashMap<String, String>` on definitions | **This phase: typed struct fields** | Phase 28 | Zero string key lookups at runtime |
| `svars: HashMap<String, String>` on CardFace/GameObject | **This phase: resolved at load time** | Phase 28 | Zero Forge string parsing at runtime |

## Key Measurements

These numbers scope the work precisely:

| Metric | Count | Significance |
|--------|-------|-------------|
| Total card JSON files | 32,274 | All must be migrated |
| Triggers with `Execute` param (SVar chains) | 15,930 | Must resolve SVars at migration time |
| Abilities with `SubAbility`/`Execute` in remaining_params | 5,168 | Must resolve SubAbility chains |
| Unique trigger param keys | 158 | ~15 consumed at runtime, rest is metadata |
| Unique static param keys | 165 | ~18 consumed at runtime, rest is metadata |
| Unique replacement param keys | 72 | ~15 consumed at runtime, rest is metadata |
| Unique remaining_params keys | 623 | Need categorization: typed fields vs metadata |
| Files with `svars` usage in `game/` | 22 files | All svars references must be eliminated |
| Runtime calls to `parse_ability()` | 3 call sites | triggers.rs:176, effects/mod.rs:163, effects/effect.rs:26 |

### Runtime-Consumed Keys (Must Become Typed Fields)

**Triggers (15 keys used by matchers):**
`Execute`, `Origin`, `Destination`, `ValidCard`, `ValidSource`, `ValidActivatingPlayer`, `ValidPlayer`, `ValidToken`, `CounterType`, `DamageAmount`, `CombatDamage`, `Phase`, `TriggerZones`, `ApiType`, `TriggerDescription`

**Statics (18 keys used by layers.rs/static_abilities.rs):**
`Affected`, `AddPower`, `AddToughness`, `SetPower`, `SetToughness`, `AddKeyword`, `RemoveKeyword`, `AddType`, `RemoveType`, `SetColor`, `AddColor`, `AddAbility`, `RemoveAllAbilities`, `CheckSVar`, `SVarCompare`, `Cost`, `Target`, `Mode`, `Description`

**Replacements (16 keys used by replacement.rs):**
`ActiveZones`, `Origin$`, `Destination$`, `NewDestination$`, `ValidCard`, `ValidSource`, `DamageType$`, `Prevent`, `NewAmount$`, `Exile`, `Double`, `NewCount$`, `NewName$`, `Layer`, `ValidCard$`, `ValidPlayer`

**Effects remaining_params (10 keys used at runtime by effect handlers):**
`SubAbility`, `Execute`, `Defined`, `ConditionCompare`, `ConditionPresent`, `ConditionZone`, `ConditionSVarCompare`, `StaticAbilities`, `Types`, `Power`, `Toughness`

## Recommended Incremental Strategy

### Wave 1: Type System Changes (Rust structs)
1. Define new typed fields on `TriggerDefinition`, `StaticDefinition`, `ReplacementDefinition`
2. Change `AbilityDefinition.remaining_params` to typed fields
3. Replace `Effect::Other` with `Effect::Unimplemented`
4. Remove `svars` from `CardFace` and `GameObject`
5. Add `execute: Option<Box<AbilityDefinition>>` to `TriggerDefinition`
6. Populate `sub_ability` on `AbilityDefinition` (already exists as always-None)
7. Update all runtime consumers (triggers.rs, layers.rs, static_abilities.rs, replacement.rs, effects/)
8. Gate `parse_ability()` behind `forge-compat`
9. Update `to_params()` -- gate or remove
10. Fix all tests

### Wave 2: Migration Script
1. Write a migration binary that reads old-format JSON + SVars from Forge
2. Resolves all SVar chains using `parse_ability()` (behind forge-compat)
3. Outputs new-format JSON files
4. Validate round-trip: new format -> deserialize -> serialize -> matches

### Wave 3: Data Migration + Validation
1. Run migration across all 32,274 files
2. Regenerate `card-data.json`
3. Update `schema.json` via schemars test
4. Run `cargo test --all`
5. Run coverage report
6. Verify WASM build and frontend compatibility

## Open Questions

1. **Metadata handling strategy**
   - What we know: 623 unique remaining_params keys, only ~10 consumed at runtime
   - What's unclear: Whether to drop non-runtime keys entirely or preserve them in a metadata struct
   - Recommendation: Drop non-runtime keys. They are Forge-specific metadata (AI hints, descriptions, activation limits) that phase.rs doesn't use. This dramatically simplifies the migration. If future engine work needs any of these, the Forge source data is always available for re-extraction.

2. **JSON field naming convention**
   - What we know: Rust uses snake_case, current JSON uses CamelCase Forge names
   - What's unclear: Whether to use snake_case in JSON (serde default) or explicit `#[serde(rename)]`
   - Recommendation: Use serde's default snake_case. This is the native Rust/serde convention and makes the JSON self-documenting as a phase.rs format, not Forge's.

3. **Filter string representation**
   - What we know: Values like "Creature.YouCtrl", "Card.Self", "Card.OppCtrl" are filter strings used by `object_matches_filter()`
   - What's unclear: Whether to parse these into typed filter enums now or keep as strings
   - Recommendation: Keep as `String` for now. Filter parsing is a separate concern and these strings are already consumed by a dedicated filter module. Typing them would be a Phase 29+ concern.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in #[test] + insta for snapshots |
| Config file | Cargo.toml (per-crate test configuration) |
| Quick run command | `cargo test -p engine` |
| Full suite command | `cargo test --all` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| SC-1 | TriggerDef uses typed fields, not params HashMap | unit | `cargo test -p engine -- trigger` | Wave 0 |
| SC-2 | StaticDef uses typed fields, not params HashMap | unit | `cargo test -p engine -- static` | Wave 0 |
| SC-3 | ReplacementDef uses typed fields, not params HashMap | unit | `cargo test -p engine -- replacement` | Wave 0 |
| SC-4 | svars eliminated from CardFace | unit | `cargo test -p engine -- card` | Wave 0 |
| SC-5 | remaining_params eliminated from AbilityDefinition | unit | `cargo test -p engine -- ability` | Wave 0 |
| SC-6 | parse_ability() gated behind forge-compat | compile | `cargo test -p engine` (without forge-compat) | Wave 0 |
| SC-7 | 32K JSON files in new format | integration | `cargo test -p engine -- json` | Wave 2 |
| SC-8 | card-data.json uses new format | integration | `cargo test --all` | Wave 3 |
| SC-9 | All existing tests pass | integration | `cargo test --all` | Existing |

### Sampling Rate
- **Per task commit:** `cargo test -p engine`
- **Per wave merge:** `cargo test --all`
- **Phase gate:** Full suite green before verification

### Wave 0 Gaps
- [ ] Migration binary `crates/engine/src/bin/migrate_typed.rs` -- reads old format, writes new
- [ ] Round-trip validation test: old format -> migrate -> new format -> engine loads correctly
- [ ] Snapshot updates for all affected insta tests

## Sources

### Primary (HIGH confidence)
- Direct codebase inspection of all affected files (types/ability.rs, types/triggers.rs, types/statics.rs, types/replacements.rs, types/card.rs, game/triggers.rs, game/static_abilities.rs, game/replacement.rs, game/layers.rs, game/effects/mod.rs, game/effects/effect.rs, game/deck_loading.rs, game/game_object.rs, database/json_loader.rs, schema/mod.rs)
- Analysis of all 32,274 ability JSON files for param key frequency and patterns
- CONTEXT.md decisions from user discussion session

### Secondary (MEDIUM confidence)
- STATE.md project history documenting Phase 21-25 decisions
- REQUIREMENTS.md traceability matrix

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - no new deps, pure refactor of existing types
- Architecture: HIGH - struct field replacement is well-understood; runtime consumers fully mapped
- Pitfalls: HIGH - all runtime param access points identified via grep; SVar chain depth already has limits
- Migration: MEDIUM - 32K file batch migration is mechanical but any edge case in SVar resolution could require fixes

**Research date:** 2026-03-10
**Valid until:** Indefinite (codebase-specific research, no external dependency drift)
