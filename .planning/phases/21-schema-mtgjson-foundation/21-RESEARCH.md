# Phase 21: Schema & MTGJSON Foundation - Research

**Researched:** 2026-03-10
**Domain:** Rust type system design, JSON schema generation, MTGJSON data integration
**Confidence:** HIGH

## Summary

Phase 21 transforms the engine's ability system from stringly-typed `HashMap<String, String>` parameters to fully typed Rust enums, loads card metadata from MTGJSON's AtomicCards.json, and generates a JSON Schema for editor autocompletion. The codebase already has the four definition types (`AbilityDefinition`, `TriggerDefinition`, `StaticDefinition`, `ReplacementDefinition`) with serde derives -- the work is replacing their `String`/`HashMap` fields with typed enums and updating the 12 files that reference `abilities: Vec<String>` on `CardFace`, `GameObject`, and `BackFaceData`.

**Primary recommendation:** Structure the work in three waves: (1) define the typed enums and update type definitions, (2) update all consumers (the ~12 files) to use typed structs instead of parsing strings at runtime, (3) add MTGJSON loading and schemars schema generation. This ordering ensures each wave compiles and tests pass before the next begins.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- All four definition types get fully typed Rust enums: `Effect` (abilities), `TriggerMode` (triggers -- already exists), `StaticMode` (statics), `ReplacementEvent` (replacements)
- Each enum variant has typed fields (e.g., `Effect::DealDamage { damage: i32, target: TargetSpec }`) -- no `HashMap<String, String>` params
- JSON deserializes directly into typed structs via serde -- no Forge string intermediary
- Sub-ability chaining and SVar-equivalent behavior handled via the typed enum structure
- `CardFace.abilities: Vec<String>` -> `Vec<AbilityDefinition>` (and same for triggers, statics, replacements)
- `GameObject` fields updated to match
- ~13 files that currently parse ability strings at runtime updated to use typed structs directly
- Forge parser updated to produce `AbilityDefinition` directly
- The new JSON format uses our own typed schema, NOT Forge's pipe-delimited string format
- AtomicCards.json committed to repo under `data/mtgjson/`
- Custom Rust types for MTGJSON deserialization (~50 lines, no mtgjson crate dependency)
- Two-file merge model: MTGJSON provides metadata, ability JSON provides behavior
- One ability JSON file per card: `data/abilities/lightning_bolt.json`
- Snake_case filenames derived from card name
- `schemars` crate with `#[derive(JsonSchema)]` on all ability types
- `insta` snapshot test ensures schema hasn't changed unexpectedly
- Validation at test time only; schema file committed at `data/abilities/schema.json`

### Claude's Discretion
- Exact field names and enum variant naming conventions
- Cost representation type design (reuse existing `ManaCost` or new type)
- Target specification type design
- Sub-ability chaining representation (inline nesting vs. named references)
- MTGJSON field mapping details (which fields are essential vs. optional)
- Multi-face card handling in ability JSON (single file with multiple faces vs. separate treatment)

### Deferred Ideas (OUT OF SCOPE)
None -- discussion stayed within phase scope
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| DATA-01 | Engine loads card metadata (name, mana cost, types, P/T, colors, keywords, oracle text, layout) from MTGJSON AtomicCards.json using custom Rust types | MTGJSON Card (Atomic) data model documented with all field names/types; custom deserialization struct design provided |
| DATA-02 | Engine defines a typed JSON ability schema mapping to AbilityDefinition, TriggerDefinition, StaticDefinition, and ReplacementDefinition types | Existing type definitions mapped; Effect enum variant design documented with all 38+ effect types from current registry |
| DATA-04 | Ability JSON schema exports a JSON Schema definition via schemars for editor autocompletion and build-time validation | schemars 1.2 integration documented including adjacently-tagged enum support; insta snapshot testing pattern provided |
</phase_requirements>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| schemars | 1.2 | JSON Schema generation from `#[derive(JsonSchema)]` | Standard Rust crate for deriving JSON Schema; supports serde tag attributes; 2020-12 draft output |
| insta | 1.46 | Snapshot testing for schema stability | Standard Rust snapshot testing; `json` feature for JSON comparisons; `assert_json_snapshot!` macro |
| serde | 1.x | Serialization/deserialization (already in workspace) | Already used throughout; schemars reads `#[serde(...)]` attributes automatically |
| serde_json | 1.x | JSON parsing (already a dependency) | Already used for WASM bridge and tests |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| cargo-insta | 1.46 | CLI tool for reviewing/accepting snapshots | Install as dev tool: `cargo install cargo-insta`; run `cargo insta review` to accept new snapshots |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| schemars | typify (reverse: schema->types) | Wrong direction -- we want types->schema, not schema->types |
| insta | manual JSON comparison | insta provides diff visualization, snapshot files, and `cargo insta review` workflow |
| Custom MTGJSON types | mtgjson crate | mtgjson crate is missing `defense` field and adds unnecessary transitive deps (user decision) |

**Installation:**
```toml
# In crates/engine/Cargo.toml
[dependencies]
schemars = "1.2"

[dev-dependencies]
insta = { version = "1.46", features = ["json"] }
```

## Architecture Patterns

### Recommended Project Structure
```
crates/engine/src/
├── types/
│   ├── ability.rs         # Effect enum, AbilityDefinition, TargetSpec, Cost types
│   ├── triggers.rs        # TriggerMode (already exists, needs params added)
│   ├── statics.rs         # NEW: StaticMode typed enum with fields
│   └── replacements.rs    # NEW: ReplacementEvent typed enum with fields
├── database/
│   ├── card_db.rs         # Existing + new load_json() method
│   └── mtgjson.rs         # NEW: MTGJSON deserialization types
└── schema/
    └── mod.rs             # NEW: Schema generation (schema_for! + write to file)

data/
├── mtgjson/
│   └── AtomicCards.json   # ~50MB, committed, MIT licensed
├── abilities/
│   ├── schema.json        # Auto-generated JSON Schema (committed)
│   └── lightning_bolt.json # Per-card ability definitions
```

### Pattern 1: Typed Effect Enum (Replaces HashMap<String, String>)
**What:** Replace `api_type: String` + `params: HashMap<String, String>` with a discriminated union enum
**When to use:** All ability/trigger/static/replacement definitions
**Example:**
```rust
// Source: Derived from existing effect registry in game/effects/mod.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum Effect {
    DealDamage {
        amount: DamageAmount,
        #[serde(default)]
        target: TargetSpec,
    },
    Draw {
        #[serde(default = "default_one")]
        count: u32,
    },
    Pump {
        #[serde(default)]
        power: i32,
        #[serde(default)]
        toughness: i32,
        #[serde(default)]
        target: TargetSpec,
    },
    Destroy {
        target: TargetSpec,
    },
    GainLife {
        amount: i32,
    },
    LoseLife {
        amount: i32,
    },
    // ... ~35 more variants matching the 38 entries in build_registry()
}
```

### Pattern 2: TargetSpec Type (Replaces ValidTgts$ String)
**What:** Typed targeting specification replacing string-based "Creature.OppCtrl", "Player", "Any"
**When to use:** Every effect/trigger that references targets
**Example:**
```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum TargetSpec {
    /// No target (self or controller implied)
    None,
    /// Any valid target (creature, player, planeswalker)
    Any,
    /// Specific target filter
    Filtered {
        filter: String, // e.g. "Creature.OppCtrl" -- reuse existing filter system
    },
    /// Target player
    Player,
    /// The ability's controller
    Controller,
    /// All matching permanents (for "All" variants)
    All {
        filter: String,
    },
}
```

### Pattern 3: MTGJSON Deserialization (Custom Types)
**What:** Minimal Rust types to deserialize MTGJSON AtomicCards.json
**When to use:** Loading card metadata
**Example:**
```rust
// Source: MTGJSON Card (Atomic) data model at https://mtgjson.com/data-models/card/card-atomic/
use std::collections::HashMap;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct AtomicCardsFile {
    pub data: HashMap<String, Vec<AtomicCard>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AtomicCard {
    pub name: String,
    #[serde(default)]
    pub mana_cost: Option<String>,
    pub colors: Vec<String>,
    pub color_identity: Vec<String>,
    #[serde(default)]
    pub power: Option<String>,
    #[serde(default)]
    pub toughness: Option<String>,
    #[serde(default)]
    pub loyalty: Option<String>,
    #[serde(default)]
    pub defense: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    pub layout: String,
    pub types: Vec<String>,
    pub subtypes: Vec<String>,
    pub supertypes: Vec<String>,
    #[serde(default)]
    pub keywords: Option<Vec<String>>,
    #[serde(default)]
    pub side: Option<String>,
    #[serde(default)]
    pub face_name: Option<String>,
    pub mana_value: f64,
    pub identifiers: AtomicIdentifiers,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AtomicIdentifiers {
    #[serde(default)]
    pub scryfall_oracle_id: Option<String>,
}
```

### Pattern 4: Ability JSON File Format (Per-Card)
**What:** Hand-authored JSON files defining card behavior
**When to use:** Every card that has mechanical behavior beyond vanilla stats
**Example:**
```json
{
    "$schema": "../schema.json",
    "abilities": [
        {
            "kind": "Spell",
            "effect": {
                "type": "DealDamage",
                "amount": { "type": "Fixed", "value": 3 },
                "target": { "type": "Any" }
            },
            "cost": { "mana": "R" }
        }
    ],
    "triggers": [],
    "statics": [],
    "replacements": []
}
```

### Pattern 5: Schema Generation via schemars
**What:** Derive `JsonSchema` on all ability types; generate schema in a test
**When to use:** Build the schema file and snapshot-test it
**Example:**
```rust
use schemars::schema_for;

#[test]
fn generate_ability_schema() {
    let schema = schema_for!(AbilityFile);
    let json = serde_json::to_string_pretty(&schema).unwrap();
    // Write to data/abilities/schema.json
    std::fs::write("../../data/abilities/schema.json", &json).unwrap();
    // Snapshot test ensures stability
    insta::assert_json_snapshot!("ability_schema", schema);
}
```

### Pattern 6: Serde Adjacently-Tagged Enums (Project Convention)
**What:** The project uses `#[serde(tag = "type", content = "data")]` for discriminated unions
**When to use:** All new enum types that cross the WASM boundary
**Note:** For the Effect enum, internally-tagged (`#[serde(tag = "type")]`) is better than adjacently-tagged because effect variants have named fields, not a single content value. schemars supports both.

### Anti-Patterns to Avoid
- **HashMap<String, String> for typed data:** The entire point of this phase is eliminating this pattern. Every new type must use typed fields.
- **String-based dispatch:** Don't add new `match api_type.as_str() { ... }` blocks. Use `match effect { Effect::DealDamage { .. } => ... }` pattern matching.
- **Parsing at runtime:** Effect handlers must NOT parse strings from params. They receive typed data directly.
- **Monolithic refactor:** Don't try to change all 12 files in one commit. Break into compilable increments.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| JSON Schema generation | Manual schema JSON | `schemars` `#[derive(JsonSchema)]` | Schemas stay in sync with types automatically; supports all serde attributes |
| Schema snapshot testing | Manual JSON comparison | `insta::assert_json_snapshot!` | Provides diffs, review workflow, auto-accept |
| MTGJSON field naming | Manual camelCase conversion | `#[serde(rename_all = "camelCase")]` | MTGJSON uses camelCase; serde handles it |
| Mana cost parsing from MTGJSON | New parser | Adapt existing `mana_cost::parse()` | MTGJSON format `{W}{U}{B}` differs from Forge `W U B` but parser can be extended |
| Color parsing | New color mapper | Reuse existing `ManaColor` enum | MTGJSON uses "W","U","B","R","G" strings; map to existing enum |

**Key insight:** The project already has most of the type infrastructure. The work is primarily about replacing `String` with typed enums in existing types, not building new frameworks.

## Common Pitfalls

### Pitfall 1: Breaking Compilation Mid-Refactor
**What goes wrong:** Changing `abilities: Vec<String>` to `abilities: Vec<AbilityDefinition>` breaks 12+ files simultaneously
**Why it happens:** The type is used across card parsing, game objects, zones, engine, casting, coverage, etc.
**How to avoid:** Use a phased approach: (1) Add new typed fields alongside old ones temporarily, (2) migrate consumers one at a time, (3) remove old fields. Or use type aliases for the transition.
**Warning signs:** Compiler errors cascading across many files after a single type change

### Pitfall 2: Serde Tag Mismatch Between Schema and Runtime
**What goes wrong:** JSON files deserialize differently than expected because of serde tagging mode
**Why it happens:** `#[serde(tag = "type")]` (internally tagged) vs `#[serde(tag = "type", content = "data")]` (adjacently tagged) produce different JSON shapes
**How to avoid:** Write round-trip tests early: serialize a typed struct to JSON, verify the JSON shape matches expectations, then deserialize it back. The project already has this pattern in `ability.rs` tests.
**Warning signs:** Deserialization errors when loading hand-authored JSON files

### Pitfall 3: MTGJSON AtomicCards Structure is Name-Indexed Array
**What goes wrong:** Expecting `data: HashMap<String, AtomicCard>` but getting `data: HashMap<String, Vec<AtomicCard>>`
**Why it happens:** MTGJSON wraps each card name in an array (some cards have multiple faces/versions under the same name key)
**How to avoid:** Use `Vec<AtomicCard>` as the value type. For single-faced cards, take the first element. For multi-faced cards, match by `side` or `faceName` field.
**Warning signs:** Deserialization panics on the first test with real MTGJSON data

### Pitfall 4: Effect Enum Exhaustiveness vs. Extensibility
**What goes wrong:** Adding a new effect requires modifying the enum everywhere
**Why it happens:** Rust enums are exhaustive by default
**How to avoid:** Include an `Other { api_type: String, params: HashMap<String, String> }` fallback variant for forward compatibility. This handles effects not yet typed while keeping the type system strict for known effects. Mark it `#[serde(other)]` so unknown types deserialize gracefully.
**Warning signs:** Tests breaking when adding new effect types

### Pitfall 5: MTGJSON File Size in Tests
**What goes wrong:** `cargo test` takes 10+ seconds parsing 50MB AtomicCards.json on every test run
**Why it happens:** Loading the full file in unit tests
**How to avoid:** Use `#[ignore]` for the full-file integration test. For unit tests, create a small test fixture with a few known cards. The success criteria says "a known card" (singular), not "all cards."
**Warning signs:** Test suite slowing down significantly after adding MTGJSON

### Pitfall 6: Forgetting BackFaceData
**What goes wrong:** `GameObject.abilities` is updated but `BackFaceData.abilities` is not
**Why it happens:** BackFaceData has its own `abilities: Vec<String>` field that's easy to miss
**How to avoid:** Search for ALL occurrences of `abilities: Vec<String>` including in `BackFaceData`. The grep found it in `game_object.rs` line 22.
**Warning signs:** Transform/DFC cards break after the refactor

### Pitfall 7: Coverage Module String Parsing
**What goes wrong:** `coverage.rs` calls `parse_ability()` on raw strings from `obj.abilities` -- after refactor, abilities are already typed
**Why it happens:** The coverage module was built assuming string-based abilities
**How to avoid:** Update `has_unimplemented_mechanics()` and `analyze_standard_coverage()` to work with typed `AbilityDefinition` directly. Pattern-match on the `Effect` enum instead of parsing strings.
**Warning signs:** Coverage analysis breaks or reports incorrect results

## Code Examples

### Current Effect Handler Signature (Before)
```rust
// Source: crates/engine/src/game/effects/deal_damage.rs
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let num_dmg: u32 = ability
        .params
        .get("NumDmg")
        .ok_or_else(|| EffectError::MissingParam("NumDmg".to_string()))?
        .parse()
        .map_err(|_| EffectError::InvalidParam("NumDmg must be a number".to_string()))?;
    // ...
}
```

### Target Effect Handler Signature (After -- Conceptual)
```rust
// Effect handlers receive typed data, no string parsing needed
pub fn resolve(
    state: &mut GameState,
    amount: &DamageAmount,
    targets: &[TargetRef],
    source_id: ObjectId,
    controller: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let num_dmg = amount.resolve(state); // DamageAmount::Fixed(3) -> 3
    // ...
}
```

### Files That Reference abilities: Vec<String> (Must Update)
```
crates/engine/src/types/card.rs          # CardFace.abilities, triggers, static_abilities, replacements
crates/engine/src/game/game_object.rs    # GameObject.abilities, BackFaceData.abilities
crates/engine/src/game/engine.rs         # Reads obj.abilities[index], parses strings
crates/engine/src/game/casting.rs        # parse_ability(&obj.abilities[0])
crates/engine/src/game/coverage.rs       # Iterates obj.abilities, parses each
crates/engine/src/game/deck_loading.rs   # Assigns card_face.abilities.clone() to obj
crates/engine/src/game/zones.rs          # Copies abilities during transform
crates/engine/src/game/layers.rs         # References abilities
crates/engine/src/game/morph.rs          # References abilities
crates/engine/src/game/planeswalker.rs   # Reads abilities for loyalty costs
crates/engine/src/game/transform.rs      # (implied via zones.rs back-face swap)
crates/engine/src/parser/card_parser.rs  # Builds Vec<String> abilities from raw text
```

### MTGJSON AtomicCards.json Top-Level Structure
```json
{
    "meta": { "date": "2026-03-01", "version": "5.x.x" },
    "data": {
        "Lightning Bolt": [
            {
                "name": "Lightning Bolt",
                "manaCost": "{R}",
                "colors": ["R"],
                "colorIdentity": ["R"],
                "types": ["Instant"],
                "subtypes": [],
                "supertypes": [],
                "text": "Lightning Bolt deals 3 damage to any target.",
                "layout": "normal",
                "manaValue": 1.0,
                "identifiers": {
                    "scryfallOracleId": "5f8287b1-..."
                }
            }
        ]
    }
}
```

### MTGJSON Mana Cost Format vs Existing Parser
```
MTGJSON: "{2}{W}{U}"  -- curly braces around each symbol
Forge:   "2 W U"       -- space-separated, no braces
Engine:  ManaCost::Cost { generic: 2, shards: [White, Blue] }
```
The existing `mana_cost::parse()` handles the Forge format. A new parsing path (strip braces, map symbols) is needed for MTGJSON format. This is straightforward -- about 15 lines.

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| schemars 0.8.x | schemars 1.2.x | 2024 | New major version; `#[derive(JsonSchema)]` works the same but output is JSON Schema 2020-12 |
| Forge .txt card format | MTGJSON JSON + ability JSON | This phase | Decouples from GPL; enables typed schema |
| `HashMap<String, String>` params | Typed enum variants | This phase | Eliminates runtime parsing errors; enables pattern matching |

**Deprecated/outdated:**
- schemars 0.8.x: Use 1.2.x. The derive macro API is the same but the output schema version differs.
- MTGJSON v4: Current version is v5.x. The data model uses `manaValue` (not `convertedManaCost` which is deprecated but still present for backward compat).

## Open Questions

1. **DamageAmount type design**
   - What we know: Most effects use fixed amounts (`NumDmg$ 3`), but some reference game state (`X`, `Count$Creature`)
   - What's unclear: How many dynamic amount patterns exist in the current card pool
   - Recommendation: Start with `Fixed(i32)` and `Variable(String)` variants; expand as migration reveals patterns

2. **Sub-ability chaining in typed enum**
   - What we know: Current system uses `SubAbility$ SvarName` -> `SVar:SvarName:DB$ Draw | NumCards$ 1` for chaining
   - What's unclear: Whether inline nesting (`sub_ability: Option<Box<AbilityDefinition>>`) or named references are cleaner in JSON
   - Recommendation: Use inline nesting for simplicity. The existing `ResolvedAbility` already has `sub_ability: Option<Box<ResolvedAbility>>` -- follow this pattern.

3. **Cost type for ability JSON**
   - What we know: Activated abilities have costs like `T` (tap), `1 R` (mana), `AddCounter<1/LOYALTY>` (loyalty)
   - What's unclear: Whether to reuse `ManaCost` or create a new `AbilityCost` type
   - Recommendation: Create an `AbilityCost` enum: `Mana(ManaCost)`, `Tap`, `Loyalty(i32)`, `Sacrifice(TargetSpec)`, `Composite(Vec<AbilityCost>)`. This is cleaner than overloading ManaCost.

4. **Multi-face cards in ability JSON**
   - What we know: MTGJSON handles faces via `side` field ("a", "b") and `faceName`
   - What's unclear: Whether `lightning_bolt.json` should handle multi-face or if DFCs need `nicol_bolas_the_ravager.json` with faces inside
   - Recommendation: Single file per card (not per face). Include a `faces` array for multi-face cards, matching the existing `CardLayout` pattern.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | cargo test (built-in) + insta 1.46 |
| Config file | crates/engine/Cargo.toml (add insta dev-dep) |
| Quick run command | `cargo test -p engine -- --lib` |
| Full suite command | `cargo test --all` |

### Phase Requirements to Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| DATA-01 | Load card metadata from MTGJSON AtomicCards.json | integration | `cargo test -p engine test_load_mtgjson_card -x` | Wave 0 |
| DATA-02 | Ability JSON deserializes into typed definitions | unit | `cargo test -p engine test_ability_json_roundtrip -x` | Wave 0 |
| DATA-04 | JSON Schema generated and validates ability files | unit | `cargo test -p engine test_schema_generation -x` | Wave 0 |
| SC-1 (from success criteria) | Round-trip: serialize typed struct -> JSON -> deserialize -> identical | unit | `cargo test -p engine test_ability_roundtrip -x` | Wave 0 |
| SC-2 (from success criteria) | Schema snapshot stability via insta | unit | `cargo test -p engine test_schema_snapshot -x` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test -p engine -- --lib`
- **Per wave merge:** `cargo test --all`
- **Phase gate:** Full suite green + `cargo clippy --all-targets -- -D warnings`

### Wave 0 Gaps
- [ ] `insta` dev-dependency in `crates/engine/Cargo.toml`
- [ ] `schemars` dependency in `crates/engine/Cargo.toml`
- [ ] `data/mtgjson/AtomicCards.json` -- download from MTGJSON (or subset for tests)
- [ ] `data/abilities/lightning_bolt.json` -- first test ability file
- [ ] Test fixture: small MTGJSON excerpt for unit tests (avoid parsing 50MB in quick tests)

## Sources

### Primary (HIGH confidence)
- Codebase analysis: `crates/engine/src/types/ability.rs` -- current type definitions with HashMap<String, String>
- Codebase analysis: `crates/engine/src/game/effects/mod.rs` -- all 38 effect registry entries (definitive list of Effect enum variants needed)
- Codebase analysis: `crates/engine/src/types/triggers.rs` -- TriggerMode enum already exists with 137+ variants
- Codebase analysis: `crates/engine/src/game/game_object.rs` -- GameObject fields that need updating
- [schemars docs](https://docs.rs/schemars/latest/schemars/derive.JsonSchema.html) -- schemars 1.2.1 supports adjacently-tagged enums, all serde attributes
- [MTGJSON Card (Atomic) data model](https://mtgjson.com/data-models/card/card-atomic/) -- all field names, types, optionality documented
- [MTGJSON Identifiers model](https://mtgjson.com/data-models/identifiers/) -- confirms scryfallOracleId available (introduced v4.3.1)

### Secondary (MEDIUM confidence)
- [schemars GitHub](https://github.com/GREsau/schemars) -- version 1.2.1 confirmed; JSON Schema 2020-12 output
- [insta GitHub](https://github.com/mitsuhiko/insta) -- version 1.46+; `json` feature for `assert_json_snapshot!`
- [MTGJSON downloads](https://mtgjson.com/downloads/all-files/) -- AtomicCards.json (all cards) vs StandardAtomic.json (Standard-legal only)

### Tertiary (LOW confidence)
- MTGJSON file size estimates (~50MB for AtomicCards.json) -- mentioned in user context, not independently verified

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- schemars and insta are well-documented, stable crates with clear APIs
- Architecture: HIGH -- based on direct codebase analysis of existing types, patterns, and the 38-entry effect registry
- Pitfalls: HIGH -- derived from actual code patterns found in the 12 files that need updating
- MTGJSON integration: MEDIUM -- data model documented but actual file parsing not tested yet

**Research date:** 2026-03-10
**Valid until:** 2026-04-10 (stable domain; schemars 1.x and MTGJSON v5 are established)
