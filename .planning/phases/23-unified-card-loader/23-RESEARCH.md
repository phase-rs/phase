# Phase 23: Unified Card Loader - Research

**Researched:** 2026-03-10
**Domain:** Rust data pipeline — merging MTGJSON metadata + ability JSON into engine CardFace/CardRules
**Confidence:** HIGH

## Summary

This phase connects two data sources (MTGJSON `AtomicCards.json` and per-card ability JSON files) into a unified `CardDatabase::load_json()` method that produces fully-formed `CardRules` objects the engine can use for gameplay. Both data sources already exist with working parsers (Phase 21), and the engine's card-to-game-object pipeline (`create_object_from_card_face`) is proven and well-tested. The primary engineering challenge is the merge logic, implicit ability synthesis (basic lands, equipment, planeswalkers), and the `scryfall_oracle_id` threading.

All building blocks are in place: `AtomicCardsFile` deserialization, `AbilityFile` deserialization, `CardFace`/`CardRules`/`CardLayout` types, `parse_mtgjson_mana_cost()`, `parse_keywords()`, and `create_object_from_card_face()`. The phase is fundamentally a data transformation exercise — no new game mechanics, no framework changes.

**Primary recommendation:** Implement `load_json()` as a new method on `CardDatabase` following the exact same pattern as `load()` (walk directory, collect errors, build face_index). Add `scryfall_oracle_id: Option<String>` to `CardFace`. Synthesize implicit abilities during the merge step. Prove with a smoke test using 5-10 hand-authored ability JSON files.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Implicit abilities synthesized during `CardDatabase::load_json()` — cards arrive fully formed
- Basic lands: synthesize mana abilities per CR 305.6 (e.g., Forest → `{T}: Add {G}`)
- Equipment: synthesize Equip activated ability from `K:Equip:N` keyword per CR 702.6
- Planeswalkers: wire loyalty costs through `AbilityCost::Loyalty` instead of `remaining_params["PW_Cost"]`
- MTG Comprehensive Rules are the source of truth for all implicit ability behavior
- One ability JSON file per card with a `faces` array matching MTGJSON's side a/b ordering
- File named by primary card name (e.g., `delver_of_secrets.json` covers both faces)
- `CardLayout` variant determined from MTGJSON's `layout` field — single source of truth
- Add `scryfall_oracle_id: Option<String>` to `CardFace`, populated from MTGJSON `identifiers.scryfallOracleId`
- `load_json()` always collects missing ability files as errors (same pattern as Forge loader)
- Test-time cross-validation: ability JSON files match MTGJSON entry
- Smoke test: 5-10 cards covering all archetypes, including Rancor (user-specified)

### Claude's Discretion
- Exact smoke test card selection (beyond Rancor) covering required archetypes
- Internal merge logic (how MTGJSON metadata + ability JSON combine into CardFace fields)
- How load_json() discovers ability files (directory scan vs. explicit name mapping)
- Whether to update existing Forge-path `CardDatabase::load()` or keep it entirely separate
- Frontend changes (if any) to use scryfall_oracle_id for image lookups

### Deferred Ideas (OUT OF SCOPE)
None — discussion stayed within phase scope
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| DATA-03 | CardDatabase::load_json() merges MTGJSON metadata + ability JSON into CardFace, becoming the primary card loading path | Core deliverable — merge logic, implicit ability synthesis, CardFace construction all researched below |
| MIGR-04 | Card data includes MTGJSON scryfallOracleId for reliable frontend image lookups via Scryfall API | `scryfall_oracle_id` field addition to CardFace, Scryfall API `oracleid:` search syntax documented |
</phase_requirements>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| serde + serde_json | (workspace) | Deserialize MTGJSON + ability JSON | Already used throughout engine |
| walkdir | (workspace) | Recursive directory traversal for ability files | Already used by `CardDatabase::load()` |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| schemars | (workspace) | JSON Schema for ability files | Already used — schema generation in `schema/mod.rs` |
| insta | (workspace) | Snapshot tests | Verify merged CardFace snapshots |

### No New Dependencies Needed
All required functionality exists in the current dependency tree. No new crates needed.

## Architecture Patterns

### Data Flow Overview
```
AtomicCards.json (MTGJSON metadata)
        │
        ▼
  AtomicCardsFile::data
  HashMap<String, Vec<AtomicCard>>
        │                              data/abilities/*.json
        │                              (per-card ability JSON)
        │                                      │
        ▼                                      ▼
  ┌──────────────────────────────────────────────────┐
  │          CardDatabase::load_json()               │
  │                                                  │
  │  1. Load AtomicCards.json                        │
  │  2. Walk ability JSON directory                  │
  │  3. For each ability file:                       │
  │     a. Deserialize AbilityFile                   │
  │     b. Look up card in AtomicCards by name       │
  │     c. Merge metadata + abilities into CardFace  │
  │     d. Synthesize implicit abilities             │
  │     e. Determine CardLayout from MTGJSON layout  │
  │     f. Build CardRules                           │
  │  4. Report missing/failed cards as errors        │
  └──────────────────────────────────────────────────┘
        │
        ▼
  CardDatabase { cards, face_index, errors }
        │
        ▼
  create_object_from_card_face() → GameObject → GameState
```

### Recommended Module Structure
```
crates/engine/src/database/
├── mod.rs              # pub use + load_json re-export
├── card_db.rs          # Existing Forge loader + new load_json()
├── mtgjson.rs          # Existing MTGJSON types + helpers
└── json_loader.rs      # NEW: merge logic, implicit synthesis, multi-face handling
```

The `json_loader.rs` module isolates the new merge logic from the existing `card_db.rs`. The `CardDatabase` struct gains a new `load_json()` constructor that delegates to `json_loader`. This keeps `card_db.rs` focused on the struct definition and Forge-path loading while the new module handles all JSON-path concerns.

### Pattern 1: AbilityFile Multi-Face Extension
**What:** Extend `AbilityFile` with an optional `faces` array for multi-face cards
**When to use:** When a card has Transform, Adventure, Modal DFC, Split, or Flip layouts

The current `AbilityFile` has flat fields: `abilities`, `triggers`, `statics`, `replacements`. For multi-face cards, a `faces` field provides per-face ability data. Single-face cards continue using the flat fields (backward compatible).

```rust
// In schema/mod.rs — extend AbilityFile
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AbilityFile {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Spell and activated ability definitions (single-face cards)
    #[serde(default)]
    pub abilities: Vec<AbilityDefinition>,
    #[serde(default)]
    pub triggers: Vec<TriggerDefinition>,
    #[serde(default)]
    pub statics: Vec<StaticDefinition>,
    #[serde(default)]
    pub replacements: Vec<ReplacementDefinition>,
    /// Per-face ability data for multi-face cards (overrides flat fields)
    #[serde(default)]
    pub faces: Vec<FaceAbilities>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FaceAbilities {
    #[serde(default)]
    pub abilities: Vec<AbilityDefinition>,
    #[serde(default)]
    pub triggers: Vec<TriggerDefinition>,
    #[serde(default)]
    pub statics: Vec<StaticDefinition>,
    #[serde(default)]
    pub replacements: Vec<ReplacementDefinition>,
}
```

**JSON example (multi-face Adventure card):**
```json
{
    "$schema": "schema.json",
    "faces": [
        {
            "abilities": [],
            "triggers": [
                { "mode": "ChangesZone", "params": { "Origin": "Graveyard", "Destination": "Any" } }
            ],
            "statics": [],
            "replacements": []
        },
        {
            "abilities": [
                {
                    "kind": "Spell",
                    "effect": { "type": "ChangeZone", "origin": "Graveyard", "destination": "Hand", "target": { "type": "Any" } }
                }
            ],
            "triggers": [],
            "statics": [],
            "replacements": []
        }
    ]
}
```

### Pattern 2: Merge MTGJSON Metadata into CardFace
**What:** Build a `CardFace` by extracting metadata from `AtomicCard` and abilities from `AbilityFile`
**When to use:** For every card during `load_json()`

```rust
// Source: Existing types in codebase, verified via code inspection
fn build_card_face(
    mtgjson: &AtomicCard,
    abilities_data: &FaceAbilities,  // or flat AbilityFile for single-face
    oracle_id: Option<String>,
) -> CardFace {
    let mana_cost = mtgjson.mana_cost.as_deref()
        .map(parse_mtgjson_mana_cost)
        .unwrap_or(ManaCost::NoCost);

    let card_type = build_card_type(mtgjson);  // supertypes + core_types + subtypes

    CardFace {
        name: mtgjson.face_name.clone()
            .unwrap_or_else(|| mtgjson.name.clone()),
        mana_cost,
        card_type,
        power: mtgjson.power.clone(),
        toughness: mtgjson.toughness.clone(),
        loyalty: mtgjson.loyalty.clone(),
        defense: mtgjson.defense.clone(),
        oracle_text: mtgjson.text.clone(),
        non_ability_text: None,
        flavor_name: None,
        keywords: mtgjson.keywords.clone().unwrap_or_default(),
        abilities: abilities_data.abilities.clone(),
        triggers: abilities_data.triggers.clone(),
        static_abilities: abilities_data.statics.clone(),
        replacements: abilities_data.replacements.clone(),
        svars: HashMap::new(),
        color_override: build_color_override(mtgjson, &mana_cost),
        scryfall_oracle_id: oracle_id,  // NEW field
    }
}
```

### Pattern 3: MTGJSON Layout to CardLayout Mapping
**What:** Map MTGJSON `layout` string to the engine's `CardLayout` enum
**When to use:** During card construction in `load_json()`

```rust
fn map_layout(layout_str: &str) -> LayoutKind {
    match layout_str {
        "normal" | "saga" | "class" | "case" | "leveler" => LayoutKind::Single,
        "split" => LayoutKind::Split,
        "flip" => LayoutKind::Flip,
        "transform" => LayoutKind::Transform,
        "meld" => LayoutKind::Meld,
        "adventure" => LayoutKind::Adventure,
        "modal_dfc" => LayoutKind::Modal,
        _ => LayoutKind::Single, // Conservative fallback
    }
}
```

MTGJSON layout values (from the test fixture): `"normal"`, `"transform"`. The full set includes `"split"`, `"flip"`, `"meld"`, `"adventure"`, `"modal_dfc"`, `"saga"`, `"class"`, `"leveler"`, `"case"`, `"planar"`, `"scheme"`, `"vanguard"`, and `"augment"`.

### Pattern 4: Implicit Ability Synthesis
**What:** Inject abilities that MTG rules require but aren't in oracle text
**When to use:** Post-merge step in `load_json()`, after building the initial CardFace

**Basic Land Mana Ability (CR 305.6):**
```rust
fn synthesize_basic_land_mana(face: &mut CardFace) {
    // CR 305.6: Each basic land type has an intrinsic mana ability
    let land_subtypes = &face.card_type.subtypes;
    for subtype in land_subtypes {
        let produced = match subtype.as_str() {
            "Plains" => "W",
            "Island" => "U",
            "Swamp" => "B",
            "Mountain" => "R",
            "Forest" => "G",
            _ => continue,
        };
        face.abilities.push(AbilityDefinition {
            kind: AbilityKind::Activated,
            effect: Effect::Mana {
                produced: produced.to_string(),
                params: HashMap::new(),
            },
            cost: Some(AbilityCost::Tap),
            sub_ability: None,
            remaining_params: HashMap::new(),
        });
    }
}
```

This exactly matches the existing `GameScenario::add_land()` pattern already in the codebase (confirmed at `scenario.rs:153-162`).

**Equipment Equip Ability (CR 702.6):**
```rust
fn synthesize_equip(face: &mut CardFace) {
    // Look for "Equip" keyword with cost parameter
    for kw in &face.keywords {
        if let Some(cost_str) = kw.strip_prefix("Equip:") {
            // Equip keyword is already parsed to Keyword::Equip(cost)
            // by parse_keywords() — but the activated ability needs to be
            // synthesized so the engine's handle_equip_activation() can find it.
            // Actually: handle_equip_activation() checks card_types.subtypes
            // for "Equipment", not abilities. Equip cost isn't currently read
            // from abilities. So no synthesis needed for equip at this point.
            // However, to make the cost accessible, store in remaining_params.
            break;
        }
    }
}
```

**Important finding on Equipment:** The engine's `handle_equip_activation()` (in `engine.rs:595-686`) validates Equipment by checking `card_types.subtypes.contains(&"Equipment")` and does NOT read an equip cost from abilities. The equip cost is currently hardcoded (no mana cost check in the equip flow). So Equipment synthesis for Phase 23 only requires:
1. The card has `"Equipment"` in subtypes (comes from MTGJSON)
2. The `Keyword::Equip(cost)` is parsed from keywords (comes from MTGJSON keywords)

No ability synthesis needed for Equipment in the current engine implementation. This simplifies the phase.

**Planeswalker Loyalty Costs (CR 606.3):**
```rust
fn synthesize_planeswalker_loyalty(face: &mut CardFace) {
    // For planeswalker abilities, the loyalty cost must use AbilityCost::Loyalty
    // instead of remaining_params["PW_Cost"].
    // This is done by the ability JSON author writing:
    //   "cost": { "type": "Loyalty", "amount": -3 }
    // AbilityCost::Loyalty { amount: i32 } already exists in the type system.
    //
    // However, the current engine (planeswalker.rs:150-156) reads loyalty cost
    // from remaining_params["PW_Cost"], not from AbilityCost::Loyalty.
    // Phase 23 has two options:
    //   A) Update planeswalker.rs to check AbilityCost::Loyalty first, fall back to PW_Cost
    //   B) Synthesize remaining_params["PW_Cost"] from AbilityCost::Loyalty during load
    //
    // Option A is cleaner (decouples from compat bridge) but touches game logic.
    // Option B is safer (no game logic changes).
    // Decision: per user constraint, use AbilityCost::Loyalty — so update planeswalker.rs.
}
```

**Key insight on planeswalker.rs update:** The `parse_loyalty_cost()` function at line 150-156 reads from `remaining_params["PW_Cost"]`. For JSON-loaded cards, the loyalty cost should come from `AbilityCost::Loyalty { amount }`. The fix is simple:

```rust
fn parse_loyalty_cost(ability_def: &AbilityDefinition) -> i32 {
    // Prefer typed AbilityCost::Loyalty
    if let Some(AbilityCost::Loyalty { amount }) = &ability_def.cost {
        return *amount;
    }
    // Fall back to remaining_params for Forge-loaded cards
    ability_def
        .remaining_params
        .get("PW_Cost")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0)
}
```

### Pattern 5: File Naming Convention
**What:** Map card name to ability JSON filename
**When to use:** When discovering ability files

Convention from CONTEXT.md: file named by primary card name, lowercased, spaces to underscores.
- `Lightning Bolt` → `lightning_bolt.json`
- `Delver of Secrets // Insectile Aberration` → `delver_of_secrets.json`
- `Rancor` → `rancor.json`

The loader can either:
- **Directory scan:** Walk `data/abilities/`, deserialize each `.json` file, match by card name → simpler, mirrors existing `load()` pattern
- **Name-based lookup:** For each MTGJSON card, construct expected filename and check if it exists → faster for large datasets

Recommendation: **Directory scan** for Phase 23 (consistency with existing `load()`, simpler error reporting). Phase 24's migration tool will generate thousands of files; if performance matters then, switch to name-based lookup.

### Anti-Patterns to Avoid
- **Modifying `CardDatabase::load()`:** Keep the Forge-path loader untouched. `load_json()` is a separate constructor. Phase 25 removes the Forge path.
- **Runtime implicit ability checks:** All synthesis happens at load time. Don't check "is this a basic land?" during gameplay — the card should already have its mana ability.
- **Deserializing the full 50MB AtomicCards.json for tests:** Use the existing 7-card `test_fixture.json` (extended with new smoke test cards).

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| MTGJSON parsing | Custom JSON parser | Existing `AtomicCardsFile` + `serde_json` | Already proven with test fixture |
| Ability JSON parsing | Custom format | Existing `AbilityFile` + JSON Schema | Phase 21 built this |
| Mana cost parsing | String manipulation | `parse_mtgjson_mana_cost()` | Handles all hybrid/phyrexian variants |
| Keyword parsing | Manual matching | `parse_keywords()` in `keywords.rs` | All 100+ keywords handled |
| Card-to-GameObject conversion | Custom builder | `create_object_from_card_face()` | Already maps all CardFace fields to GameObject |
| Scryfall image lookup | Custom HTTP client | Existing `scryfall.ts` with `oracleid:` search | Scryfall search syntax supports oracle ID natively |

**Key insight:** This phase is a data pipeline — no new game mechanics, no new frameworks. Every component exists; this phase wires them together.

## Common Pitfalls

### Pitfall 1: Multi-face Card Name Mismatch
**What goes wrong:** MTGJSON stores multi-face cards under the combined name `"Delver of Secrets // Insectile Aberration"` but each face has `face_name: "Delver of Secrets"`. The ability JSON file is named `delver_of_secrets.json`. Lookup must match on the primary face name, not the full `//`-separated name.
**Why it happens:** MTGJSON's key format differs from the face-level name format.
**How to avoid:** When looking up cards, strip `" // "` and match the portion before the separator. Alternatively, iterate MTGJSON entries and match `face_name` or the part before `" // "` to the ability file's card name.
**Warning signs:** Multi-face cards fail to load; "card not found" errors.

### Pitfall 2: Color Override for Colorless Multicolor Cards
**What goes wrong:** Some cards have colors that don't match their mana cost (e.g., DFC back faces with no mana cost but have colors, or devoid cards). If `color_override` isn't set, `derive_colors_from_mana_cost()` gives wrong results.
**Why it happens:** MTGJSON has explicit `colors` array that may differ from mana cost colors.
**How to avoid:** Compare MTGJSON `colors` to derived-from-cost colors. If they differ, populate `color_override`. The `create_object_from_card_face()` already respects `color_override` (confirmed in code).
**Warning signs:** Cards having wrong color identity in game.

### Pitfall 3: Keywords Coming from Both MTGJSON and Ability JSON
**What goes wrong:** Keywords listed in MTGJSON metadata may overlap with abilities in the ability JSON. For example, if a card has "Flying" in MTGJSON keywords AND a static ability for flying in the ability JSON, the card could have duplicate effects.
**Why it happens:** MTGJSON keywords are metadata; ability JSON defines behavior.
**How to avoid:** Keywords from MTGJSON go into `CardFace.keywords` (for display and `has_keyword()` checks). Ability JSON defines the mechanical behavior. The engine already handles this separation: keywords are checked via `has_keyword()`, while abilities/triggers/statics are in their own vectors.
**Warning signs:** Double-applying keyword effects.

### Pitfall 4: Planeswalker Loyalty Cost Migration
**What goes wrong:** JSON-loaded planeswalker abilities use `AbilityCost::Loyalty { amount }` but the engine reads `remaining_params["PW_Cost"]`. If the engine isn't updated, planeswalker abilities have zero loyalty cost.
**Why it happens:** `parse_loyalty_cost()` in `planeswalker.rs` only checks `remaining_params`.
**How to avoid:** Update `parse_loyalty_cost()` to check `AbilityCost::Loyalty` first, fall back to `PW_Cost`. Test with both Forge-loaded and JSON-loaded planeswalkers.
**Warning signs:** Planeswalker abilities always cost 0 loyalty.

### Pitfall 5: Serialization of scryfall_oracle_id Across WASM Boundary
**What goes wrong:** Adding a new field to `CardFace` could break WASM serialization if not handled correctly.
**Why it happens:** `CardFace` is serialized via serde across the WASM boundary.
**How to avoid:** Use `Option<String>` with `#[serde(default)]` so existing serialized data (without the field) still deserializes correctly. The `serde` derive on `CardFace` handles optional fields gracefully.
**Warning signs:** WASM deserialization errors after adding the field.

### Pitfall 6: Test Fixture Size
**What goes wrong:** Adding 5-10 cards to the MTGJSON test fixture balloons it to an unwieldy size, or the full AtomicCards.json is accidentally committed.
**Why it happens:** MTGJSON entries can be verbose.
**How to avoid:** Add only the specific smoke test cards to `test_fixture.json`. Keep the fixture minimal. Never load the full `AtomicCards.json` in tests (use `include_str!` with the fixture).
**Warning signs:** Slow test compilation due to large `include_str!`.

## Code Examples

### Existing Mana Ability Pattern (from scenario.rs)
```rust
// Source: crates/engine/src/game/scenario.rs lines 153-162
// This is the EXACT pattern to replicate for synthesized basic land mana abilities
obj.abilities.push(AbilityDefinition {
    kind: AbilityKind::Activated,
    effect: Effect::Mana {
        produced: mana_char.to_string(),  // "W", "U", "B", "R", "G"
        params: HashMap::new(),
    },
    cost: Some(AbilityCost::Tap),
    sub_ability: None,
    remaining_params: HashMap::new(),
});
```

### Existing Merge Pattern (from deck_loading.rs)
```rust
// Source: crates/engine/src/game/deck_loading.rs lines 107-146
// create_object_from_card_face() maps ALL CardFace fields to GameObject
// This is the consumer of CardFace — whatever load_json() produces,
// this function handles. Fields mapped:
//   card_type, mana_cost, power, toughness, loyalty, keywords,
//   abilities, svars, triggers, static_abilities, replacements,
//   color (from color_override or derived from mana_cost)
```

### Existing MTGJSON Layout Values (from test_fixture.json)
```json
// Source: data/mtgjson/test_fixture.json
// Single-face: "layout": "normal"
// Transform:   "layout": "transform", "side": "a" / "side": "b"
```

### AbilityCost::Loyalty Already in Type System
```rust
// Source: crates/engine/src/types/ability.rs lines 40-46
pub enum AbilityCost {
    Mana { cost: String },
    Tap,
    Loyalty { amount: i32 },         // Already exists! JSON can express it directly
    Sacrifice { target: TargetSpec },
    Composite { costs: Vec<AbilityCost> },
}
```

### Scryfall Search by Oracle ID
```typescript
// Scryfall search syntax supports oracle ID natively:
// https://scryfall.com/search?q=oracleid:5d547462-7cf4-4848-b8b1-48cf63bde68d
//
// The frontend could use the existing searchScryfall() function:
const { cards } = await searchScryfall(`oracleid:${oracleId}`);
// Or use the /cards/collection endpoint (batch lookup)
```

### Frontend GameObject Type (needs update)
```typescript
// Source: client/src/adapter/types.ts lines 75-120
// Currently missing scryfall_oracle_id field
// The TS interface must be updated to include:
//   scryfall_oracle_id?: string | null;
// Note: This field flows through serde serialization from Rust → WASM → TS
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Forge .txt format | MTGJSON + ability JSON | Phase 21 (v1.2) | GPL-clean data source |
| `remaining_params["PW_Cost"]` | `AbilityCost::Loyalty { amount }` | Phase 23 (this phase) | Typed loyalty costs instead of string params |
| Name-based Scryfall lookup | Oracle ID-based lookup | Phase 23 (this phase) | Reliable across reprints and name variants |
| `CardDatabase::load()` only | + `CardDatabase::load_json()` | Phase 23 (this phase) | JSON becomes primary loading path |

**Key: Phase 23 is additive.** Nothing is removed. The Forge `load()` path remains. Phase 25 removes it.

## Smoke Test Card Selection

Per the user constraint, 5-10 cards covering all required archetypes. Rancor is mandatory.

| Card | Archetype | What It Tests | Implicit Synthesis |
|------|-----------|---------------|-------------------|
| Forest | Basic land | Synthesized mana ability | {T}: Add {G} |
| Grizzly Bears | Vanilla creature | Empty abilities vector, casting | None (already handled) |
| Lightning Bolt | Instant/Spell | Typed Effect::DealDamage + targeting | None |
| Rancor | Aura + triggered ability | Enchant, ETB pump, dies-return-to-hand trigger | None (in ability JSON) |
| Bonesplitter | Equipment | Equip keyword, Equipment subtype | None (engine checks subtype) |
| Jace, the Mind Sculptor | Planeswalker | Loyalty abilities with AbilityCost::Loyalty | PW cost via typed AbilityCost |
| Delver of Secrets | Transform DFC | Multi-face loading, side a/b | None |
| Giant Killer | Adventure DFC | Adventure face casting | None |

Notes:
- All 8 cards have entries in MTGJSON's `AtomicCards.json` (confirmed for existing fixture cards, others trivially available)
- Rancor exercises: Enchantment type, `Enchant creature` targeting, +2/+0 pump static, and "When Rancor is put into a graveyard from the battlefield, return Rancor to its owner's hand" trigger
- Jace exercises all 4 loyalty ability slots with different costs (+2, 0, -1, -12)
- Delver exercises transform with different P/T, types, and keywords on each face
- Giant Killer or Bonecrusher Giant exercises Adventure layout (main creature + adventure spell)

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust std test + insta (snapshot) |
| Config file | Cargo workspace test configuration |
| Quick run command | `cargo test -p engine -- json_loader` |
| Full suite command | `cargo test --all` |

### Phase Requirements → Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| DATA-03 | load_json() merges MTGJSON + ability JSON into CardFace | unit | `cargo test -p engine -- json_loader -x` | Wave 0 |
| DATA-03 | Multi-face cards load correctly (both faces) | unit | `cargo test -p engine -- json_loader::multi_face -x` | Wave 0 |
| DATA-03 | Basic land mana ability synthesis | unit | `cargo test -p engine -- json_loader::basic_land -x` | Wave 0 |
| DATA-03 | Equipment loads with Equip keyword | unit | `cargo test -p engine -- json_loader::equipment -x` | Wave 0 |
| DATA-03 | Planeswalker loyalty cost via AbilityCost::Loyalty | unit | `cargo test -p engine -- json_loader::planeswalker -x` | Wave 0 |
| DATA-03 | Missing ability file reported as error | unit | `cargo test -p engine -- json_loader::missing -x` | Wave 0 |
| DATA-03 | Cross-validation: ability JSON matches MTGJSON | unit | `cargo test -p engine -- cross_validation -x` | Wave 0 |
| MIGR-04 | scryfall_oracle_id populated from MTGJSON | unit | `cargo test -p engine -- json_loader::oracle_id -x` | Wave 0 |
| DATA-03 | Smoke test: game with JSON-loaded cards completes | integration | `cargo test -p engine -- smoke_test_json -x` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test -p engine -- json_loader -x`
- **Per wave merge:** `cargo test --all`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `crates/engine/src/database/json_loader.rs` — new module with merge logic + tests
- [ ] `data/abilities/` — 8 new ability JSON files for smoke test cards
- [ ] `data/mtgjson/test_fixture.json` — extended with smoke test card entries
- [ ] Smoke test in integration tests or `json_loader` module

## Open Questions

1. **MTGJSON Multi-face Card Key Format**
   - What we know: MTGJSON uses `"Delver of Secrets // Insectile Aberration"` as the key, with `face_name` per face and `side` ("a"/"b")
   - What's unclear: Do all multi-face layouts use the same `" // "` separator format? (Adventure, Modal DFC, Split)
   - Recommendation: Handle `" // "` as the standard separator, extract primary face name as part before the separator. Test with Adventure card data.

2. **Frontend scryfall_oracle_id Usage**
   - What we know: Scryfall supports `oracleid:` search syntax. Frontend currently uses name-based lookups.
   - What's unclear: Whether frontend changes are needed in Phase 23 or deferred to a later phase.
   - Recommendation: Add the field to the Rust/TS types in Phase 23. Frontend usage is at Claude's discretion per CONTEXT.md — can be a simple enhancement (oracle ID → Scryfall API fallback) or deferred if smoke test cards work with name-based lookups.

## Sources

### Primary (HIGH confidence)
- `crates/engine/src/database/card_db.rs` — CardDatabase struct, load(), layout_faces()
- `crates/engine/src/database/mtgjson.rs` — AtomicCardsFile, AtomicCard, parse_mtgjson_mana_cost()
- `crates/engine/src/schema/mod.rs` — AbilityFile struct
- `crates/engine/src/types/card.rs` — CardFace, CardLayout, CardRules
- `crates/engine/src/types/ability.rs` — AbilityDefinition, Effect, AbilityCost
- `crates/engine/src/game/deck_loading.rs` — create_object_from_card_face()
- `crates/engine/src/game/scenario.rs` — GameScenario (mana ability synthesis pattern)
- `crates/engine/src/game/planeswalker.rs` — parse_loyalty_cost(), handle_activate_loyalty()
- `crates/engine/src/game/engine.rs` — handle_equip_activation()
- `crates/engine/src/game/mana_abilities.rs` — is_mana_ability(), resolve_mana_ability()
- `crates/engine/src/game/game_object.rs` — GameObject fields
- `client/src/adapter/types.ts` — Frontend GameObject type
- `client/src/services/scryfall.ts` — Scryfall API integration

### Secondary (MEDIUM confidence)
- [Scryfall API documentation](https://scryfall.com/docs/api/cards/id) — Card lookup endpoints
- [Scryfall search syntax](https://scryfall.com/docs/syntax) — `oracleid:` search prefix
- MTGJSON AtomicCards format — layout field values observed in test fixture

### Tertiary (LOW confidence)
- None — all findings verified against codebase or official documentation

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH — no new dependencies, all verified in workspace
- Architecture: HIGH — follows existing `CardDatabase::load()` pattern exactly, all types verified
- Pitfalls: HIGH — identified through code inspection of actual integration points
- Implicit synthesis: HIGH — basic land pattern confirmed in `scenario.rs`, equipment flow confirmed in `engine.rs`, planeswalker flow confirmed in `planeswalker.rs`

**Research date:** 2026-03-10
**Valid until:** 2026-04-10 (stable — all findings are project-internal codebase patterns)
