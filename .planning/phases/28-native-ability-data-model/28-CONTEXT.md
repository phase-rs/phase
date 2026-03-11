# Phase 28: Native Ability Data Model — Context

**Gathered:** 2026-03-10 (updated 2026-03-10)
**Status:** Ready for planning

<domain>
## Phase Boundary

Eliminate ALL Forge scripting DSL from the card data format and engine runtime. Every ability, trigger, static, replacement, SVar chain, filter expression, and mana cost string must be expressed through fully typed Rust structures — no `HashMap<String, String>` params, no raw SVar strings, no runtime string parsing via `parse_ability()`, no Forge-style filter syntax.

The Rust type definitions must read as MTG domain documentation — someone reading the structs should understand Magic abilities without knowing Forge ever existed. This is the phase that makes phase.rs the cleanest architectural MTG rules engine.

**Dependency reversal:** Phase 28 executes BEFORE Phase 27 (aura casting / triggered targeting). Phase 27 builds on the typed data model rather than adding Forge-style SVar code. ROADMAP.md must be updated.

</domain>

<decisions>
## Implementation Decisions

### Scope: Zero HashMap<String, String> Anywhere in the Type System

- **No `remaining_params` catch-all.** The `remaining_params: HashMap<String, String>` field on `AbilityDefinition` must be eliminated entirely.
- **No `Effect::Other` with Forge params.** Removed entirely — not replaced with an Unimplemented variant. All tests rewritten with typed Effect variants.
- **No `GenericEffect { params: HashMap }`.** Replaced with typed fields: `static_abilities: Vec<StaticDefinition>` resolved at JSON load time (currently parses SVars via `parse_static()` at runtime — this is eliminated).
- **No `Cleanup { params: HashMap }`.** Replaced with typed bool fields: `clear_remembered`, `clear_chosen_player`, `clear_chosen_color`, `clear_chosen_type`, `clear_chosen_card`, `clear_imprinted`, `clear_triggers`, `clear_coin_flips`.
- **No `Mana { params: HashMap }`.** Mana variant gets `produced: Vec<ManaColor>`, no HashMap.
- **No SVar strings.** `svars: HashMap<String, String>` eliminated from `CardFace`, `GameObject`, and `ResolvedAbility`. All SubAbility chaining resolved at data-load time as inlined nested structs.
- **No `parse_ability()` at runtime.** Gate behind `forge-compat` or remove entirely.
- **No Forge parameter key names.** Keys like `Execute`, `ValidCard`, `SubAbility`, `AddKeyword`, `Affected`, etc. must not appear as string keys in any runtime data structure.
- **No Forge filter syntax.** Filter expressions like `"Creature.YouCtrl"`, `"Permanent.nonLand+OppCtrl"` replaced with typed `TargetFilter` enum.

### What "Typed" Means — Use Existing Enums and Create New Ones

**CRITICAL: Do NOT use `Option<String>` where a typed enum exists or should exist.** Moving Forge strings from a HashMap into named `Option<String>` fields is NOT a real conversion.

**Existing enums that MUST be used:**

| Engine Enum | Location | Use For |
|-------------|----------|---------|
| `Zone` | `types/zones.rs` | `origin`, `destination`, `trigger_zones`, `active_zones` |
| `Phase` | `types/phase.rs` | `phase` on TriggerDefinition |
| `Layer` | `types/layers.rs` | `layer` on definitions |
| `ManaColor` | `types/mana.rs` | `set_color`, `add_color`, `produced` |
| `StaticMode` | `types/statics.rs` | `mode` on ActiveContinuousEffect (currently `String` — must fix) |
| `ManaCost` | `types/mana.rs` | Ability costs, keyword costs (Ward, Equip, Kicker, etc.) |
| `Keyword` | `types/keywords.rs` | `CardFace.keywords` (currently `Vec<String>` — must fix) |

**New types to create:**

| New Type | Purpose |
|----------|---------|
| `TargetFilter` | Typed filter enum replacing all Forge filter strings |
| `Duration` | `UntilEndOfTurn`, `UntilYourNextTurn`, `UntilHostLeavesPlay`, `Permanent` |
| `PtValue` | `Fixed(i32)` or `Variable(String)` for power/toughness on CardFace |
| `DevotionCheck` | `{ color: ManaColor, threshold: u32 }` replacing SVar-based devotion |

### TargetFilter — Typed Filter Enum

Replaces both `TargetSpec` and all Forge filter strings. One type for all filtering:

```rust
pub enum TargetFilter {
    None,
    Any,
    Player,
    Controller,
    SelfRef,
    Typed {
        card_type: Option<CardType>,
        subtype: Option<String>,     // genuinely open-ended (Human, Elf, Equipment)
        controller: Option<ControllerRef>,
        properties: Vec<FilterProp>,
    },
    Not(Box<TargetFilter>),
    Or(Vec<TargetFilter>),
    And(Vec<TargetFilter>),
}

pub enum ControllerRef { You, Opponent }

pub enum FilterProp {
    Token, Attacking, Tapped, NonType(String),
    WithKeyword(String), CountersGE(String, u32),
    CmcGE(u32), InZone(Zone), Owned(ControllerRef), Other,
}
```

Plus a separate `targeting_mode: TargetingMode` (Single vs All) where needed.

### SubAbility Chains — Inlined Nested Structs

SubAbility chains are inlined as `Option<Box<AbilityDefinition>>` resolved at JSON load time:

```json
{
  "triggers": [{
    "mode": "ChangesZone",
    "origin": "Battlefield",
    "valid_card": { "type": "SelfRef" },
    "execute": {
      "effect": { "type": "ChangeZone", "origin": "Battlefield", "destination": "Exile" },
      "target": { "type": "Typed", "card_type": "Permanent", "controller": "Opponent",
                  "properties": [{ "type": "NonType", "value": "Land" }] },
      "duration": "UntilHostLeavesPlay"
    }
  }]
}
```

- `resolve_ability_chain()` becomes simple recursion: resolve current effect → if `sub_ability.is_some()`, recurse. Keep `MAX_CHAIN_DEPTH` guard.
- Migration binary detects and errors on circular SVar references.
- Unresolvable SVars → card gets `Effect::Unimplemented` marker, logged for manual review.

### Effect Variant Cleanup

All Effect variants with zone/color String fields become typed:

- `ChangeZone { origin: Option<Zone>, destination: Option<Zone>, target: TargetFilter }` (not String)
- `ChangeZoneAll { origin: Option<Zone>, destination: Option<Zone>, target: TargetFilter }`
- `Bounce { target: TargetFilter, destination: Option<Zone> }` (not String)
- `Dig { count: u32, destination: Option<Zone> }` (not String)
- `Mana { produced: Vec<ManaColor> }` (not String, no HashMap)
- `Token { ..., colors: Vec<ManaColor> }` (not `Vec<String>`)
- All `target: TargetSpec` fields → `target: TargetFilter`
- `counter_type: String` stays String (genuinely open-ended: +1/+1, loyalty, lore, etc.)

### AbilityCost — Expanded Typed Variants

```rust
pub enum AbilityCost {
    Mana { cost: ManaCost },             // uses existing ManaCost type
    Tap,
    Loyalty { amount: i32 },
    Sacrifice { target: TargetFilter },
    PayLife { amount: u32 },
    Discard { count: u32, filter: Option<TargetFilter> },
    Exile { count: u32, filter: Option<TargetFilter> },
    TapCreatures { count: u32, filter: TargetFilter },
    Composite { costs: Vec<AbilityCost> },
}
```

### AbilityDefinition — Minimal with Cross-Cutting Concerns

Targeting lives on Effect variants (per MTG rules, targeting is per-effect in a chain). AbilityDefinition carries only cross-cutting concerns:

```rust
pub struct AbilityDefinition {
    pub kind: AbilityKind,
    pub effect: Effect,
    pub cost: Option<AbilityCost>,
    pub sub_ability: Option<Box<AbilityDefinition>>,
    pub duration: Option<Duration>,
    pub description: Option<String>,
}
```

### Keyword Typing

- `CardFace.keywords: Vec<String>` → `Vec<Keyword>` (parsed at load time)
- Cost-bearing keywords use `ManaCost`: `Ward(ManaCost)`, `Equip(ManaCost)`, `Kicker(ManaCost)`, `Cycling(ManaCost)`, `Flashback(ManaCost)`, etc.
- `Enchant(String)` → `Enchant(TargetFilter)` (uses the new typed filter)
- `EtbCounter(String)` → typed counter struct

### CardFace Cleanup

- `svars: HashMap<String, String>` → removed
- `keywords: Vec<String>` → `Vec<Keyword>`
- `power: Option<String>` → `Option<PtValue>` (Fixed(i32) | Variable(String))
- `toughness: Option<String>` → `Option<PtValue>`

### GameObject Cleanup

- `svars: HashMap<String, String>` → removed
- `deck_loading.rs` no longer copies SVars

### ResolvedAbility Simplification

```rust
pub struct ResolvedAbility {
    pub effect: Effect,
    pub targets: Vec<TargetRef>,
    pub source_id: ObjectId,
    pub controller: PlayerId,
    pub sub_ability: Option<Box<ResolvedAbility>>,
}
```

- `params: HashMap<String, String>` → removed (Effect carries all data)
- `svars: HashMap<String, String>` → removed (SubAbility chains inlined)

### ActiveContinuousEffect Typing

- `mode: String` → `mode: StaticMode`
- `params: HashMap<String, String>` → Claude's discretion (recommended: discriminated union by layer, replacing both `layer` and `params` with a single `ContinuousModification` enum)

### Compat Method Elimination

- `Effect::api_type()` → deleted entirely (not just gated)
- `Effect::to_params()` → deleted entirely
- `ResolvedAbility::api_type()` → deleted
- `ResolvedAbility::from_raw()` → deleted
- `AbilityDefinition::api_type()` → deleted
- `AbilityDefinition::params()` → deleted
- Tests rely on typed variants, not compat bridges

### Test Migration

- All 56 `Effect::Other` usages across 22 files → rewritten with correct typed Effect variants
- No `HashMap` construction in tests — Effect variant carries all data
- No test helper builder — tests directly construct typed abilities

### Data Migration

- All 32,274 `data/abilities/*.json` files migrated to the new schema
- Migration binary resolves SVar references into inlined structs
- Cycle detection: track visited SVars, error on cycles
- Unresolvable SVars: skip card, log for manual review
- Filter strings converted to typed TargetFilter JSON
- Mana cost strings parsed to ManaCost struct
- Round-trip validation: old format → parse → new format → serialize → deserialize → engine accepts

### Frontend Impact

- TypeScript types auto-generated via tsify — changes propagate automatically
- Breaking changes: svars removed from game objects, TargetSpec becomes TargetFilter, Effect variants change shape, keywords become typed
- Frontend components reading game state types will need updates guided by TS type errors

### Claude's Discretion

- Whether to process the migration incrementally (triggers → statics → replacements → abilities) or all at once
- Exact design of `ContinuousModification` enum for ActiveContinuousEffect
- Schema versioning strategy for the JSON files
- How to handle cards whose abilities the engine doesn't yet fully implement
- Exact design of `DevotionCheck` struct
- Exact AbilityDefinition field mapping from remaining_params keys
- Whether TargetFilter needs additional filter properties beyond those listed
- Exact Duration enum variants beyond the known four

</decisions>

<specifics>
## Specific Ideas

- The Rust types must read as MTG domain documentation — the quality bar is "cleanest architectural MTG rules engine"
- Consider generating the new JSON schema with `schemars` so schema.json stays in sync
- The `coverage_report` binary should validate against the new format
- Migration binary should validate round-trip integrity
- After this phase, no Forge DSL exists anywhere in the runtime — phase.rs is fully independent

</specifics>

<code_context>
## Existing Code Insights

### Files That Must Change

**Type definitions:**
- `crates/engine/src/types/ability.rs` — AbilityDefinition, TriggerDefinition, StaticDefinition, ReplacementDefinition, Effect enum, AbilityCost enum, TargetSpec → TargetFilter
- `crates/engine/src/types/card.rs` — CardFace (svars, keywords, power, toughness)
- `crates/engine/src/types/keywords.rs` — Keyword enum parameterized variants (Ward, Equip, Enchant, etc.)
- `crates/engine/src/types/layers.rs` — ActiveContinuousEffect (mode, params)
- `crates/engine/src/types/mana.rs` — ManaCost already exists (reuse for AbilityCost)

**Runtime consumers (most params/svars accesses):**
- `crates/engine/src/game/layers.rs` — 17 `.params.` accesses on ActiveContinuousEffect
- `crates/engine/src/game/triggers.rs` — 4 `.params.` + 7 `.svars.` accesses
- `crates/engine/src/game/effects/mod.rs` — SubAbility chain resolution (4 + 2 accesses)
- `crates/engine/src/game/effects/effect.rs` — GenericEffect SVar parsing
- `crates/engine/src/game/effects/token.rs` — 2 param accesses
- `crates/engine/src/game/effects/animate.rs` — 2 param accesses
- `crates/engine/src/game/static_abilities.rs` — 1 param access
- `crates/engine/src/game/replacement.rs` — 1 param access
- `crates/engine/src/game/stack.rs` — 1 param access
- `crates/engine/src/game/casting.rs` — 1 + 1 accesses
- `crates/engine/src/game/deck_loading.rs` — copies svars into GameObjects
- `crates/engine/src/game/game_object.rs` — svars field on GameObject

**Test code (Effect::Other usages):**
- 56 total usages across 22 files in `crates/engine/src/game/`
- Heaviest: engine.rs (5), life.rs (4), destroy.rs (4), counter.rs (4)

**Data pipeline:**
- `crates/engine/src/database/json_loader.rs` — loads ability JSON (must consume new format)
- `crates/engine/src/bin/card_data_export.rs` — generates card-data.json
- `data/abilities/*.json` — 32,274 files in Forge-style JSON format

**Parser (to be gated/removed):**
- `crates/engine/src/parser/ability.rs` — `parse_ability()` currently ungated, used at runtime

### Reusable Assets

- `ManaCost` enum (types/mana.rs) with ManaCostShard — already typed, reuse for AbilityCost and keyword costs
- Typed enums from Phase 21: `Effect` (38+ variants), `TriggerMode`, `StaticMode`, `ReplacementEvent`
- `schemars` integration for JSON schema generation
- `json_loader.rs` pattern for MTGJSON + abilities merge
- Test infrastructure from Phase 22 (GameScenario, CardBuilder)
- `filter.rs` — existing filter matching logic (to be refactored for typed TargetFilter)

### Integration Points

- WASM boundary via serde + tsify — all new types must be serializable
- Frontend TypeScript types auto-generated — breaking changes expected
- `card-data.json` export format changes

</code_context>

<deferred>
## Deferred Ideas

None — this phase is the final step in data model independence.

</deferred>

---

*Phase: 28-native-ability-data-model*
*Context gathered: 2026-03-10*
