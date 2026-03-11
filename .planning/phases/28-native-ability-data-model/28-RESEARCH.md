# Phase 28: Native Ability Data Model - Research

**Researched:** 2026-03-10
**Domain:** Rust type system refactoring, serde data model migration, card data pipeline
**Confidence:** HIGH

## Summary

Phase 28 eliminates all Forge scripting DSL remnants from the engine runtime. The current codebase uses typed `Effect` enums (38 variants) but falls back to `HashMap<String, String>` for trigger/static/replacement parameters, SubAbility chains via SVar string references, Forge-style filter strings (`"Creature.Other+YouCtrl"`), and `remaining_params` catch-all bags. The phase replaces every `HashMap<String, String>` with typed struct fields, every Forge filter string with a `TargetFilter` enum, every SVar chain with inlined `Option<Box<AbilityDefinition>>`, and every `String` cost/zone/color field with the existing typed enums.

The codebase is well-positioned for this refactoring. The typed `Effect` enum, `ManaCost`, `ManaColor`, `Zone`, `Keyword`, `TriggerMode`, `StaticMode`, and `ReplacementEvent` enums already exist. The primary work is: (1) designing typed fields for the ~30 trigger param keys, ~25 static param keys, and ~15 replacement param keys currently stored as `HashMap<String, String>`, (2) creating the `TargetFilter` enum to replace Forge filter strings, (3) writing a migration binary to convert 32,274 JSON ability files, and (4) updating all 22+ runtime consumer files.

**Primary recommendation:** Execute incrementally -- type definitions first, then runtime consumers, then JSON migration, then test and cleanup. The type changes are the foundation everything else depends on.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Zero `HashMap<String, String>` anywhere in the type system -- no `remaining_params`, no `params` on definitions, no `GenericEffect { params }`, no `Cleanup { params }`, no `Mana { params }`, no `Effect::Other`
- `TargetFilter` typed enum replacing all Forge filter strings (structure defined in CONTEXT.md)
- `AbilityCost` expanded with `PayLife`, `Discard`, `Exile`, `TapCreatures`, typed `ManaCost` for Mana variant
- SubAbility chains inlined as `Option<Box<AbilityDefinition>>` resolved at JSON load time
- All Effect variants with String zone/color fields become typed (`Zone`, `ManaColor`, `Keyword`)
- `CardFace.keywords: Vec<String>` -> `Vec<Keyword>`, `power/toughness: Option<String>` -> `Option<PtValue>`
- `svars: HashMap<String, String>` removed from `CardFace`, `GameObject`, `ResolvedAbility`
- `ResolvedAbility.params: HashMap<String, String>` removed
- `parse_ability()` gated behind `forge-compat`
- All compat methods (`api_type()`, `to_params()`, `from_raw()`, `params()`) deleted entirely
- `Effect::Other` removed entirely (not replaced with Unimplemented)
- All 32,274 `data/abilities/*.json` migrated to native typed schema
- `ActiveContinuousEffect.mode: String` -> `StaticMode`, `params: HashMap` -> Claude's discretion (recommended: `ContinuousModification` enum)
- New types: `TargetFilter`, `Duration`, `PtValue`, `DevotionCheck`

### Claude's Discretion
- Whether to process migration incrementally (triggers -> statics -> replacements -> abilities) or all at once
- Exact design of `ContinuousModification` enum for ActiveContinuousEffect
- Schema versioning strategy for JSON files
- How to handle cards whose abilities the engine doesn't yet fully implement
- Exact design of `DevotionCheck` struct
- Exact AbilityDefinition field mapping from remaining_params keys
- Whether TargetFilter needs additional filter properties beyond those listed
- Exact Duration enum variants beyond the known four

### Deferred Ideas (OUT OF SCOPE)
None -- this phase is the final step in data model independence.
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| NAT-01 | `TriggerDefinition`, `StaticDefinition`, `ReplacementDefinition` use typed struct fields instead of `params: HashMap<String, String>` | Trigger params analysis (15K Execute, 15K TriggerDescription, 11K ValidCard, etc.), static params (6.5K Description, 4.3K Affected, 1.8K AddPower), replacement params (1.5K Description, 1.1K ReplaceWith) -- all mappable to typed fields |
| NAT-02 | `svars: HashMap<String, String>` eliminated from `CardFace` and `GameObject` -- SubAbility chains resolved at data-load time | 5,137 SubAbility + 15,158 Execute references in ability files; triggers.rs `build_triggered_ability()` and effects/mod.rs `resolve_ability_chain()` are the two runtime SVar consumers |
| NAT-03 | `remaining_params` field removed from `AbilityDefinition` | 16,874 abilities have remaining_params; top keys are SpellDescription (17K), SubAbility (5K), TgtPrompt (3.5K), Defined (2K) -- mappable to typed fields on AbilityDefinition |
| NAT-04 | `TargetSpec` replaced with typed `TargetFilter` enum | filter.rs implements Forge-style filter matching; 17 `.params.` accesses in layers.rs read `Affected` filter strings; TargetSpec has 6 variants with String-based filters |
| NAT-05 | `parser::ability::parse_ability()` gated behind `forge-compat` | Currently ungated, called from triggers.rs line 176 and effects/mod.rs line 163 -- both resolve SVars at runtime. After SubAbility inlining, these call sites are eliminated |
| NAT-06 | All 32K `data/abilities/*.json` files migrated to native typed JSON schema | 32,274 files; 12,876 with triggers, 5,815 with statics, 1,443 with replacements; migration binary converts HashMap params to typed struct fields + TargetFilter |
</phase_requirements>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| serde | 1.x | JSON serialization of all new types | Already used throughout; `#[serde(tag = "type")]` for discriminated unions |
| schemars | 0.8.x | JSON Schema generation from Rust types | Already used; schema.json auto-generated from `AbilityFile` |
| serde_json | 1.x | JSON parsing in migration binary | Already used for card data pipeline |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| thiserror | 1.x | Error types for migration failures | Already in use; extend for migration errors |
| indicatif | 0.17.x | Progress bar for 32K file migration | Optional quality-of-life for migration binary |

### Alternatives Considered
None -- the stack is fully determined by existing project choices.

## Architecture Patterns

### Recommended Project Structure
```
crates/engine/src/types/
├── ability.rs           # Effect, AbilityDefinition, AbilityCost, TargetFilter (MAJOR changes)
├── card.rs              # CardFace (svars removed, keywords Vec<Keyword>, PtValue)
├── keywords.rs          # Keyword enum (cost params -> ManaCost)
├── layers.rs            # ActiveContinuousEffect (typed ContinuousModification)
├── statics.rs           # StaticMode (unchanged)
├── triggers.rs          # TriggerMode (unchanged)
├── replacements.rs      # ReplacementEvent (unchanged)
├── mana.rs              # ManaCost (already typed, reused)
├── zones.rs             # Zone (already typed, reused)
├── duration.rs          # NEW: Duration enum
├── filter.rs            # NEW: TargetFilter enum (or in ability.rs)
crates/engine/src/game/
├── filter.rs            # REWRITTEN: typed TargetFilter matching instead of string parsing
├── layers.rs            # REWRITTEN: typed ContinuousModification instead of HashMap params
├── triggers.rs          # REWRITTEN: typed TriggerDefinition fields, no SVar resolution
├── effects/mod.rs       # REWRITTEN: typed sub_ability chain, no SVar parsing
├── effects/effect.rs    # REWRITTEN: typed static_abilities instead of SVar parsing
├── deck_loading.rs      # Simplified: no svars copy
crates/engine/src/bin/
├── migrate_abilities.rs # NEW: migration binary
```

### Pattern 1: Discriminated Union per Definition Type
**What:** Each definition type (Trigger, Static, Replacement) gets a typed struct with enum-specific fields instead of HashMap
**When to use:** For all definition types

TriggerDefinition currently:
```rust
pub struct TriggerDefinition {
    pub mode: TriggerMode,
    pub params: HashMap<String, String>,  // REMOVED
}
```

TriggerDefinition after:
```rust
pub struct TriggerDefinition {
    pub mode: TriggerMode,
    pub execute: Option<Box<AbilityDefinition>>,  // resolved from SVar at load time
    pub valid_card: Option<TargetFilter>,          // "ValidCard" -> typed
    pub origin: Option<Zone>,                       // "Origin" -> typed
    pub destination: Option<Zone>,                  // "Destination" -> typed
    pub trigger_zones: Vec<Zone>,                   // "TriggerZones" -> typed
    pub valid_player: Option<String>,               // "ValidActivatingPlayer"/"ValidPlayer"
    pub phase: Option<Phase>,                       // "Phase" -> typed
    pub optional: bool,                             // "OptionalDecider" presence
    pub combat_damage: bool,                        // "CombatDamage" flag
    pub description: Option<String>,                // "TriggerDescription"
}
```

StaticDefinition currently:
```rust
pub struct StaticDefinition {
    pub mode: StaticMode,
    pub params: HashMap<String, String>,  // REMOVED
}
```

StaticDefinition after:
```rust
pub struct StaticDefinition {
    pub mode: StaticMode,
    pub affected: Option<TargetFilter>,        // "Affected" -> typed
    pub modifications: Vec<ContinuousModification>,  // replaces AddPower/AddKeyword/etc params
    pub condition: Option<StaticCondition>,     // CheckSVar/SVarCompare/IsPresent
    pub affected_zone: Option<Zone>,           // "AffectedZone"
    pub description: Option<String>,
}
```

### Pattern 2: ContinuousModification Enum (replaces HashMap params on ActiveContinuousEffect)
**What:** A discriminated union carrying exactly what a continuous effect does
**When to use:** In StaticDefinition and ActiveContinuousEffect

```rust
pub enum ContinuousModification {
    AddPower(i32),
    AddToughness(i32),
    SetPower(i32),
    SetToughness(i32),
    AddKeyword(Keyword),
    RemoveKeyword(Keyword),
    AddAbility(String),         // ability text, genuinely open-ended
    RemoveAllAbilities,
    AddType(CoreType),
    RemoveType(CoreType),
    SetColor(Vec<ManaColor>),
    AddColor(ManaColor),
}
```

This replaces `determine_layers_from_params()` and `apply_continuous_effect()` in layers.rs -- each variant knows its layer implicitly.

### Pattern 3: SubAbility Inlining at Load Time
**What:** SVar references resolved during JSON deserialization/migration into nested `Option<Box<AbilityDefinition>>`
**When to use:** All SubAbility/Execute chains

Before (runtime):
```rust
// triggers.rs build_triggered_ability: looks up Execute SVar, calls parse_ability()
if let Some(execute_svar) = trig_def.params.get("Execute") {
    if let Some(svar_value) = svars.get(execute_svar) {
        if let Ok(ability_def) = parse_ability(svar_value) { ... }
    }
}
```

After (data load time):
```rust
// TriggerDefinition.execute is already resolved
if let Some(ability) = &trig_def.execute {
    // Direct typed access, no parsing
}
```

### Anti-Patterns to Avoid
- **Moving HashMap keys to `Option<String>` fields:** This is NOT typing. `origin: Option<String>` must be `origin: Option<Zone>`.
- **Creating a mega-struct with all possible fields:** Each definition type should only carry fields relevant to its mode.
- **Partial migration:** Leaving some params in HashMap "for later" creates technical debt. The whole point is zero HashMap.
- **Breaking the WASM boundary:** All new types must derive Serialize/Deserialize. Test round-trip serialization.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Mana cost parsing | String manipulation for `{1}{W}` | Existing `ManaCost` / `ManaCostShard` enum + `parse_mtgjson_mana_cost()` | 42 ManaCostShard variants already cover every hybrid/phyrexian/snow case |
| Zone string matching | `match zone_str { "Battlefield" => ... }` | Existing `Zone` enum with serde | Zone already has 8 variants matching all Forge zone strings |
| Keyword parsing from strings | Custom parser in migration | Existing `Keyword::from_str()` (Infallible) | 190+ keyword variants with parameterized parsing already implemented |
| Filter string parsing | Regex or manual splitting | TargetFilter enum with serde deserialization | Forge filter syntax has known grammar; typed enum is simpler and safer |
| JSON schema updates | Manual schema.json editing | `schemars::schema_for!(AbilityFile)` | Already wired up in schema/mod.rs test |

## Common Pitfalls

### Pitfall 1: Breaking Serialization Compatibility During Migration
**What goes wrong:** Changing struct fields breaks existing JSON deserialization, meaning the engine can't load cards until ALL 32K files are migrated
**Why it happens:** New typed fields don't match old JSON format
**How to avoid:** Migration binary runs FIRST to produce new-format JSON. Use `#[serde(default)]` liberally on new optional fields. Consider a two-phase approach: add new fields with defaults, migrate JSON, then remove old fields.
**Warning signs:** `serde_json::from_str` failures on ability JSON files

### Pitfall 2: Circular SVar References During Migration
**What goes wrong:** SVar A references SVar B which references SVar A, causing infinite recursion during inlining
**Why it happens:** Forge format allows arbitrary SVar cross-references
**How to avoid:** Track visited SVars during resolution (HashSet<String>). MAX_CHAIN_DEPTH guard (already exists at 10). Log and skip circular cards with `Effect::Unimplemented` marker.
**Warning signs:** Stack overflow in migration binary

### Pitfall 3: Test Code Using Effect::Other / from_raw() Extensively
**What goes wrong:** 56 `Effect::Other` usages across 22 test files. Removing `Effect::Other` breaks compilation of almost every test module.
**Why it happens:** Tests were written using `from_raw()` shortcut instead of typed Effect construction
**How to avoid:** Must rewrite all 56 test sites to use typed Effect variants. This is significant work but cannot be skipped. Group by file and batch-convert.
**Warning signs:** Compilation errors in test modules after removing `Effect::Other`

### Pitfall 4: ActiveContinuousEffect Coupling to Layers.rs
**What goes wrong:** layers.rs has 17 `.params.` accesses with string-key lookups. Changing ActiveContinuousEffect's structure requires rewriting the entire layer evaluation.
**Why it happens:** Layer evaluation was written against Forge's HashMap-based data model
**How to avoid:** Rewrite `gather_active_continuous_effects()`, `determine_layers_from_params()`, and `apply_continuous_effect()` as a unit. Each `ContinuousModification` variant knows its own layer.
**Warning signs:** Layer evaluation tests failing with wrong P/T values

### Pitfall 5: Frontend TypeScript Type Breakage
**What goes wrong:** tsify auto-generates TS types from Rust structs. Removing `svars`, changing `keywords` from `string[]` to typed, changing Effect shape breaks frontend compilation.
**Why it happens:** Frontend `types.ts` manually duplicates some types, and tests construct mock objects with old shape
**How to avoid:** Update `client/src/adapter/types.ts` (line 101: `svars: Record<string, string>` removal, line 96: `keywords: string[]` change). Update test mocks in 5 test files. The tsify-generated types will guide this.
**Warning signs:** `pnpm run type-check` failures after WASM rebuild

### Pitfall 6: Effect::to_params() Removal Cascade
**What goes wrong:** `to_params()` is used in 3 critical runtime paths: `ResolvedAbility::new()`, `build_triggered_ability()`, and `resolve_ability_chain()`. Removing it before those consumers are rewritten causes compilation failure.
**Why it happens:** `to_params()` was the bridge from typed Effect back to HashMap for consumers that still read params by key
**How to avoid:** Remove consumers of `to_params()` first (rewrite to read from Effect fields directly), THEN remove `to_params()`. Or remove simultaneously in one wave.
**Warning signs:** Compilation errors in triggers.rs and effects/mod.rs

## Code Examples

### TargetFilter Enum (from CONTEXT.md, verified against filter.rs needs)
```rust
// Source: CONTEXT.md locked decision + filter.rs analysis
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum TargetFilter {
    None,
    Any,
    Player,
    Controller,
    SelfRef,
    Typed {
        card_type: Option<CardType>,  // maps to CoreType
        subtype: Option<String>,
        controller: Option<ControllerRef>,
        properties: Vec<FilterProp>,
    },
    Not(Box<TargetFilter>),
    Or(Vec<TargetFilter>),
    And(Vec<TargetFilter>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ControllerRef { You, Opponent }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum FilterProp {
    Token,
    Attacking,
    Tapped,
    NonType(String),
    WithKeyword(String),
    CountersGE { counter_type: String, count: u32 },
    CmcGE(u32),
    InZone(Zone),
    Owned(ControllerRef),
    Other,            // permissive fallback
    EnchantedBy,      // needed per filter.rs
    EquippedBy,       // needed per filter.rs
}
```

### Typed AbilityDefinition (from CONTEXT.md)
```rust
pub struct AbilityDefinition {
    pub kind: AbilityKind,
    pub effect: Effect,
    pub cost: Option<AbilityCost>,
    pub sub_ability: Option<Box<AbilityDefinition>>,  // was SVar reference
    pub duration: Option<Duration>,
    pub description: Option<String>,                   // was remaining_params["SpellDescription"]
    pub target_prompt: Option<String>,                 // was remaining_params["TgtPrompt"]
    pub defined: Option<String>,                       // was remaining_params["Defined"]
    pub sorcery_speed: bool,                           // was remaining_params["SorcerySpeed"]
}
```

### Duration Enum
```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Duration {
    UntilEndOfTurn,
    UntilYourNextTurn,
    UntilHostLeavesPlay,
    Permanent,
}
```

### PtValue Type
```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "value")]
pub enum PtValue {
    Fixed(i32),
    Variable(String),  // "*", "X", etc.
}
```

### Typed filter matching (replacing filter.rs)
```rust
// Replaces string-based object_matches_filter()
pub fn matches_target_filter(
    state: &GameState,
    object_id: ObjectId,
    filter: &TargetFilter,
    source_id: ObjectId,
) -> bool {
    match filter {
        TargetFilter::None => false,
        TargetFilter::Any => true,
        TargetFilter::SelfRef => object_id == source_id,
        TargetFilter::Typed { card_type, subtype, controller, properties } => {
            let obj = match state.objects.get(&object_id) { Some(o) => o, None => return false };
            // Type check
            if let Some(ct) = card_type { /* ... */ }
            // Controller check
            if let Some(ctrl) = controller { /* ... */ }
            // All properties must match
            properties.iter().all(|p| matches_filter_prop(state, obj, object_id, source_id, p))
        }
        TargetFilter::Not(inner) => !matches_target_filter(state, object_id, inner, source_id),
        TargetFilter::Or(filters) => filters.iter().any(|f| matches_target_filter(state, object_id, f, source_id)),
        TargetFilter::And(filters) => filters.iter().all(|f| matches_target_filter(state, object_id, f, source_id)),
        _ => true,
    }
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| `params: HashMap<String, String>` on all definitions | Typed struct fields per definition type | Phase 28 | Eliminates all runtime string-key lookups |
| `svars: HashMap<String, String>` for SubAbility chains | `sub_ability: Option<Box<AbilityDefinition>>` inlined at load time | Phase 28 | Eliminates runtime SVar resolution + parse_ability() calls |
| `TargetSpec` with Forge filter strings | `TargetFilter` typed enum | Phase 28 | No more "Creature.Other+YouCtrl" string parsing at runtime |
| `Vec<String>` for keywords on CardFace | `Vec<Keyword>` typed enum | Phase 28 | Keywords are typed at load time, not string-parsed at use time |
| `Option<String>` for power/toughness | `Option<PtValue>` | Phase 28 | Variable P/T explicitly typed |

**Deprecated/outdated:**
- `Effect::to_params()` -- deleted (was compat bridge)
- `Effect::api_type()` -- already feature-gated, now deleted
- `ResolvedAbility::from_raw()` -- deleted (was test shortcut)
- `parse_ability()` -- gated behind `forge-compat` (was runtime parser)

## Open Questions

1. **Remaining_params keys that need AbilityDefinition fields**
   - What we know: Top keys are SpellDescription (17K), SubAbility (5K), TgtPrompt (3.5K), Defined (2K), StackDescription (1.2K), ValidTgts (1.1K), KW (1K), AILogic (1K)
   - What's unclear: How many of these are metadata-only (description, AI hints) vs. mechanically relevant. `Defined`, `TargetMin`/`TargetMax`, `Mode`, `Duration`, `StaticAbilities` are mechanically relevant.
   - Recommendation: `description`, `target_prompt`, `stack_description` as Option<String>. `defined` as Option<String>. `sorcery_speed` as bool. `duration` as Option<Duration>. `target_min`/`target_max` as Option<u32>. AI-only keys (`AILogic`, `IsCurse`, `Ultimate`) as Option<String> or ignored. `StaticAbilities` resolved at load time into the AbilityDefinition's sub-structures.

2. **Migration order: types-first or JSON-first?**
   - What we know: Types and JSON must be consistent. Changing types before JSON means temporary deserialization failure.
   - What's unclear: Whether to use serde's `#[serde(alias)]` / `#[serde(flatten)]` for transition
   - Recommendation: Write migration binary first (reads old format, writes new format), run it, THEN change Rust types. Or change types with `#[serde(default)]` on new fields so old JSON still parses (missing fields get defaults), migrate JSON, then tighten.

3. **Cards with unimplemented abilities (`Effect::Other` in JSON)**
   - What we know: 2,533 abilities use `Effect::Other` with Forge api_type strings like "Venture", "Amass", etc.
   - What's unclear: Whether to create stub Effect variants or use a single `Effect::Unimplemented { name: String }` marker
   - Recommendation: Use `Effect::Unimplemented { name: String, description: Option<String> }` as the sole fallback. This is semantically honest -- it says "we know this effect exists but haven't implemented a handler." Unlike `Effect::Other`, it carries no HashMap and cannot be confused with a real implementation.

4. **TargetFilter: `card_type` field type**
   - What we know: CONTEXT.md says `Option<CardType>` but CardType is `{ supertypes, core_types, subtypes }` (a struct). Filter matching needs core type checks like "Creature", "Permanent", "Land".
   - What's unclear: Whether to use `Option<CoreType>` or a separate `TypeFilter` enum that includes "Permanent" (which is multi-type)
   - Recommendation: Use a `TypeFilter` enum: `Creature`, `Land`, `Artifact`, `Enchantment`, `Instant`, `Sorcery`, `Planeswalker`, `Permanent`, `Card`, `Any`. This maps cleanly to existing `is_type_keyword()` and `matches_type()` in filter.rs.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in `#[test]` + insta 1.x for snapshots |
| Config file | Cargo.toml (workspace-level test config) |
| Quick run command | `cargo test -p engine` |
| Full suite command | `cargo test --all` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| NAT-01 | TriggerDefinition/StaticDefinition/ReplacementDefinition typed fields | unit | `cargo test -p engine types::ability::tests -x` | Partial (existing tests use HashMap) |
| NAT-02 | svars eliminated, SubAbility inlined | unit+integration | `cargo test -p engine game::triggers::tests -x` | Partial (triggers.rs tests use svars) |
| NAT-03 | remaining_params removed from AbilityDefinition | unit | `cargo test -p engine schema::tests -x` | Partial (schema roundtrip tests exist) |
| NAT-04 | TargetFilter replaces TargetSpec | unit+integration | `cargo test -p engine game::filter::tests -x` | Yes (filter.rs has 10 tests, need rewrite) |
| NAT-05 | parse_ability() gated behind forge-compat | unit | `cargo test -p engine -- --test-threads=1` (compile without forge-compat) | No -- Wave 0 |
| NAT-06 | 32K JSON files migrated | integration | `cargo test -p engine database::json_loader::tests -x` | Partial (json_loader tests exist) |

### Sampling Rate
- **Per task commit:** `cargo test -p engine`
- **Per wave merge:** `cargo test --all`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] New test for typed TriggerDefinition construction and serialization roundtrip
- [ ] New test for typed StaticDefinition with ContinuousModification
- [ ] New test for TargetFilter matching (typed replacement for filter.rs tests)
- [ ] New test for SubAbility chain resolution without SVar lookup
- [ ] Migration binary tests (old format -> new format -> round-trip)
- [ ] Compile-without-forge-compat test to verify parse_ability() gating

## Quantitative Migration Analysis

### Current HashMap Usage (runtime types)
| Type | Field | Occurrences in Code | Occurrences in Data |
|------|-------|---------------------|---------------------|
| TriggerDefinition | params: HashMap | 4 `.params.` + 7 `.svars.` in triggers.rs | 12,876 files with triggers |
| StaticDefinition | params: HashMap | 17 `.params.` in layers.rs | 5,815 files with statics |
| ReplacementDefinition | params: HashMap | 1 `.params.` in replacement.rs | 1,443 files with replacements |
| AbilityDefinition | remaining_params: HashMap | Implicit via compat methods | 16,874 abilities with remaining_params |
| ResolvedAbility | params: HashMap + svars: HashMap | Used in 22+ files | Runtime only |
| ActiveContinuousEffect | params: HashMap | 17 accesses in layers.rs | Runtime only |
| Effect::GenericEffect | params: HashMap | 1 call site (effect.rs) | 546 files |
| Effect::Cleanup | params: HashMap | 1 call site (cleanup.rs) | 0 files directly |
| Effect::Mana | params: HashMap | 1 call site (mana.rs) | 1,898 files |
| Effect::Other | api_type + params: HashMap | 22 files, 56 usages in tests | 2,533 files |
| CardFace | svars: HashMap | deck_loading.rs line 138 | All files (empty for most) |
| GameObject | svars: HashMap | game_object.rs line 73 | Runtime only |

### Effect::Other Test Rewrite Scope (56 usages across 22 files)
| File | Count | Notes |
|------|-------|-------|
| engine.rs | 5 | Integration test helpers |
| life.rs | 4 | GainLife/LoseLife tests |
| destroy.rs | 4 | Destroy/DestroyAll tests |
| counter.rs | 4 | Counter tests |
| bounce.rs | 3 | Bounce tests |
| change_zone.rs | 3 | ChangeZone tests |
| cleanup.rs | 3 | Cleanup tests |
| choose_card.rs | 3 | ChooseCard tests |
| copy_spell.rs | 3 | CopySpell tests |
| discard.rs | 3 | Discard tests |
| fight.rs | 3 | Fight tests |
| pump.rs | 3 | Pump tests |
| sacrifice.rs | 2 | Sacrifice tests |
| casting.rs | 2 | Casting tests |
| priority.rs | 2 | Priority tests |
| transform.rs | 2 | Transform tests |
| token.rs | 1 | Token tests |
| animate.rs | 1 | Animate tests |
| stack.rs | 1 | Stack tests |
| planeswalker.rs | 1 | Planeswalker tests |
| triggers.rs | 1 | Trigger tests |
| mod.rs (effects) | 2 | Dispatch tests |

### Trigger Param Key -> Typed Field Mapping
| Forge Key | Count | Typed Field | Type |
|-----------|-------|-------------|------|
| Execute | 15,158 | `execute: Option<Box<AbilityDefinition>>` | Resolved at load time |
| TriggerDescription | 14,998 | `description: Option<String>` | Metadata only |
| ValidCard | 10,974 | `valid_card: Option<TargetFilter>` | Typed filter |
| TriggerZones | 7,899 | `trigger_zones: Vec<Zone>` | Typed zone list |
| Destination | 6,973 | `destination: Option<Zone>` | Typed zone |
| Origin | 6,751 | `origin: Option<Zone>` | Typed zone |
| ValidPlayer | 2,221 | `valid_player: Option<String>` | Player filter |
| Phase | 2,074 | `phase: Option<Phase>` | Typed phase |
| OptionalDecider | 1,417 | `optional: bool` | Boolean flag |
| ValidTarget | 1,091 | `valid_target: Option<TargetFilter>` | Typed filter |
| ValidActivatingPlayer | 1,080 | (merged into valid_player) | Player filter |
| ValidSource | 1,034 | `valid_source: Option<TargetFilter>` | Typed filter |
| CombatDamage | 770 | `combat_damage: bool` | Boolean flag |
| Secondary | 548 | `secondary: bool` | Boolean flag |
| IsPresent | 544 | (part of condition) | Condition check |
| CheckSVar | 409 | (part of condition) | Condition check |

### Static Param Key -> Typed Field/Modification Mapping
| Forge Key | Count | Typed Representation |
|-----------|-------|---------------------|
| Description | 6,471 | `description: Option<String>` on StaticDefinition |
| Affected | 4,296 | `affected: Option<TargetFilter>` on StaticDefinition |
| AddPower | 1,794 | `ContinuousModification::AddPower(i32)` |
| AddKeyword | 1,741 | `ContinuousModification::AddKeyword(Keyword)` |
| AddToughness | 1,440 | `ContinuousModification::AddToughness(i32)` |
| ValidCard | 1,367 | Part of condition |
| EffectZone | 775 | `effect_zone: Option<Zone>` on StaticDefinition |
| SetPower | 356 | `ContinuousModification::SetPower(i32)` |
| SetToughness | 304 | `ContinuousModification::SetToughness(i32)` |
| AddAbility | 322 | `ContinuousModification::AddAbility(String)` |
| CheckSVar/SVarCompare | 308+ | Part of condition on StaticDefinition |
| CharacteristicDefining | ~200 | `characteristic_defining: bool` on StaticDefinition |

## Sources

### Primary (HIGH confidence)
- Codebase analysis: `crates/engine/src/types/ability.rs` -- 900+ lines, Effect enum (38 variants + Other), all definition types
- Codebase analysis: `crates/engine/src/game/layers.rs` -- 770 lines, 17 HashMap param accesses in layer evaluation
- Codebase analysis: `crates/engine/src/game/triggers.rs` -- 550+ lines, SVar resolution + trigger matching
- Codebase analysis: `crates/engine/src/game/effects/mod.rs` -- 550+ lines, SubAbility chain resolution
- Codebase analysis: `crates/engine/src/game/filter.rs` -- 447 lines, Forge-style filter matching
- Codebase analysis: `crates/engine/src/game/game_object.rs` -- 241 lines, svars field on GameObject
- Data analysis: 32,274 ability JSON files, quantitative param key frequency analysis
- CONTEXT.md: User decisions from context session, locked implementation choices

### Secondary (MEDIUM confidence)
- Codebase analysis: `crates/engine/src/game/deck_loading.rs` -- svars copy behavior
- Codebase analysis: `crates/engine/src/database/json_loader.rs` -- card loading pipeline
- Codebase analysis: `crates/engine/src/schema/mod.rs` -- JSON schema generation

### Tertiary (LOW confidence)
- Estimated migration binary complexity based on SVar chain depth (not measured -- recommend profiling a sample)
- TargetFilter property completeness (FilterProp enum may need additional variants discovered during migration)

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- all libraries already in use, no new dependencies
- Architecture: HIGH -- types and data fully analyzed, quantitative param frequency known
- Pitfalls: HIGH -- based on direct code inspection of all affected files, counted Effect::Other usages
- Migration scope: HIGH -- 32,274 files analyzed programmatically with param key frequency counts

**Research date:** 2026-03-10
**Valid until:** Indefinite (internal codebase analysis, no external dependency versioning concerns)
