# Technology Stack: v1.2 Data Source Migration & Test Infrastructure

**Project:** Forge.rs v1.2
**Researched:** 2026-03-10
**Scope:** NEW stack additions for MTGJSON integration, custom ability JSON schema, Forge parser removal, and comprehensive test suite. Excludes existing validated stack (Rust engine, React/TS frontend, Zustand, Framer Motion, Tailwind v4, Tauri v2, Axum, etc.).

## Recommended Stack Additions

### MTGJSON Data Integration

| Technology | Version | Purpose | Why |
|------------|---------|---------|-----|
| **serde_json** (existing) | 1 | Deserialize MTGJSON AtomicCards.json | Already in the workspace. MTGJSON data is JSON -- serde_json is the standard Rust JSON library and already a dependency. |
| **Own MTGJSON types** (not the `mtgjson` crate) | N/A | Type-safe card metadata deserialization | The `mtgjson` crate (5.2.2) is usable but brings `chrono`, `semver`, and `uuid` as required dependencies for fields we will never use (purchase URLs, rulings, release dates). Rolling our own subset struct with ~15 fields is simpler, avoids 3 transitive deps, and lets us add `defense` (missing from the crate's `AtomicCard`). |

**Data file choice: `StandardAtomic.json`** because:
- Contains only Standard-legal cards (matches the 78-card curated subset scope)
- Uses the Card (Atomic) model: oracle-like, printing-independent, exactly what a rules engine needs
- Much smaller than AllPrintings.json (which includes every printing of every card across all sets)
- Fields we need: `name`, `manaCost`, `manaValue`, `types`, `subtypes`, `supertypes`, `colors`, `colorIdentity`, `colorIndicator`, `power`, `toughness`, `loyalty`, `defense`, `keywords`, `text`, `layout`, `side`, `faceName`, `identifiers.scryfallOracleId`
- Fields we ignore: `purchaseUrls`, `rulings`, `foreignData`, `printings`, `edhrecRank`, `leadershipSkills`, `hand`, `life`, `attractionLights`

**Why NOT the `mtgjson` crate (5.2.2):**
1. Missing `defense` field on `AtomicCard` (needed for Battle cards)
2. Requires `chrono`, `semver`, `uuid` -- adds ~200KB to WASM binary for unused fields
3. `AtomicCard.identifiers` is `Identifiers` with 16 fields when we need 1 (`scryfallOracleId`)
4. The crate deserializes ALL fields including `foreignData: Vec<ForeignData>` and `rulings: Option<Vec<Ruling>>` which allocate for data we discard
5. 15-field custom struct with `#[serde(rename_all = "camelCase")]` is ~50 lines vs. an opaque dependency

**Why NOT `mtgjson-sdk` (0.1.2):**
- Pulls in DuckDB, reqwest, polars, tokio -- designed for runtime querying, not static data embedding
- Massively heavy for a game engine that just needs to load card metadata at startup

### Custom Ability JSON Schema

| Technology | Version | Purpose | Why |
|------------|---------|---------|-----|
| **schemars** | 1.2.1 | Generate JSON Schema from Rust ability types | Derives `JsonSchema` alongside `Serialize`/`Deserialize`, ensuring the schema always matches Rust types. MIT-licensed, deep serde attribute compatibility. Schema generation is a dev-time tool, not a runtime dep. |
| **serde** (existing) | 1 | Serialize/deserialize ability definitions | Already used throughout. The custom ability format is JSON, serde handles it. |
| **serde_json** (existing) | 1 | Parse ability JSON files | Already a dependency. |

**Ability format approach:** Define Rust enums/structs for `AbilityDef`, `TriggerDef`, `StaticDef`, `ReplacementDef` with strongly-typed variants (not `HashMap<String, String>`), then derive `Serialize + Deserialize + JsonSchema`. This replaces the current string-based Forge format where abilities are `"SP$ DealDamage | NumDmg$ 3"` pipe-delimited text parsed at runtime.

**Why schemars over hand-written JSON Schema:**
- Schema auto-updates when Rust types change -- single source of truth
- `#[serde(tag = "type", content = "data")]` attributes automatically generate discriminated union schemas
- Can export `.schema.json` files for editor autocompletion when hand-writing card definitions
- MIT license, v1.2.1 released Feb 2026, actively maintained

**Why NOT jsonschema crate for validation:**
- We don't need runtime JSON Schema validation -- serde's deserialization IS the validation
- If JSON doesn't match the Rust types, `serde_json::from_str` returns `Err` with field path
- jsonschema is useful when accepting untrusted/external JSON -- our card files are first-party

### Test Infrastructure

| Technology | Version | Purpose | Why |
|------------|---------|---------|-----|
| **insta** | 1.46.3 | Snapshot testing for game state assertions | Serde-native: `assert_json_snapshot!` on `GameState` captures exact game state. Snapshot diffs show precisely what changed. Eliminates hand-written assertion chains. `cargo-insta` CLI for review workflow. |
| **test-case** | 3.3.1 | Parameterized test cases | `#[test_case("Lightning Bolt", 3 ; "bolt deals 3")]` generates named sub-tests. Perfect for "same mechanic, different cards" patterns. Proc macro, zero runtime cost. |

**Why insta:**
- GameState already derives `Serialize` -- zero adaptation cost
- Snapshot files (`.snap`) provide reviewable, diffable game state records
- `assert_json_snapshot!` or `assert_yaml_snapshot!` for structured state comparison
- `cargo insta review` CLI for accepting/rejecting snapshot changes
- Eliminates brittle hand-written assertions like `assert_eq!(obj.power, Some(5))`
- Perfect for regression tests: "cast Lightning Bolt targeting creature, snapshot resulting state"

**Why test-case over alternatives:**
- Most popular parameterized test crate (3.3.1 stable)
- Clean proc macro syntax: `#[test_case(input => expected ; "description")]`
- Named test cases appear in `cargo test` output for debugging
- Lighter than `proptest` (which is for property-based testing, not parameterized scenarios)

**Why NOT proptest/quickcheck:**
- MTG rules are deterministic given a seed -- snapshot tests are more appropriate than random generation
- Game state is deeply structured (not amenable to random generation)
- XMage-style scenario tests ("set up board, take action, verify result") are the right pattern

### Forge Parser Retention (Dev-Only)

| Technology | Version | Purpose | Why |
|------------|---------|---------|-----|
| **walkdir** (existing, feature-gated) | 2 | Traverse Forge card directories | Needed only for migration tooling and dev-only conversion scripts. Gate behind `cfg(feature = "forge-compat")` so it's not compiled into release/WASM builds. |

**Approach:** Move the Forge parser and `walkdir` behind a Cargo feature flag:
```toml
[features]
default = []
forge-compat = ["dep:walkdir"]

[dependencies]
walkdir = { version = "2", optional = true }
```

This means:
- `cargo build` / `cargo build --target wasm32-unknown-unknown` excludes Forge parser
- `cargo build --features forge-compat` includes it for migration scripts
- `cargo test --features forge-compat` runs parser tests
- Release WASM binary stays small (walkdir adds ~15KB)

## Alternatives Considered

| Category | Recommended | Alternative | Why Not |
|----------|-------------|-------------|---------|
| MTGJSON types | Own subset struct | `mtgjson` crate (5.2.2) | Missing `defense` field, pulls `chrono`/`semver`/`uuid`, deserializes unused fields |
| MTGJSON types | Own subset struct | `mtgjson-sdk` (0.1.2) | DuckDB + reqwest + polars + tokio -- massive overkill |
| Schema generation | `schemars` 1.2.1 | Hand-written JSON Schema | Drifts from Rust types, error-prone, no auto-update |
| Schema validation | serde deserialization | `jsonschema` crate | Runtime validation unnecessary -- serde IS the validator |
| Snapshot testing | `insta` 1.46.3 | Manual `assert_eq!` chains | Verbose, brittle, no diff visualization, no review workflow |
| Parameterized tests | `test-case` 3.3.1 | `parameterized` crate | Less popular, similar features, test-case has better named case syntax |
| Parameterized tests | `test-case` 3.3.1 | `proptest` | Random generation wrong for deterministic rule verification |
| Data file | `StandardAtomic.json` | `AllPrintings.json` | ~100x larger, includes printing data (art, rarity, set) we don't need |
| Data file | `StandardAtomic.json` | `AtomicCards.json` | Includes all formats -- Standard subset is what we need |
| Data file | Checked into repo | Downloaded at build time | Reproducible builds, offline CI, no network dependency |

## What NOT to Add

| Technology | Reason |
|------------|--------|
| `reqwest` | No runtime HTTP needed -- MTGJSON data is checked into the repo as static JSON |
| `tokio` (in engine crate) | Engine is synchronous and single-threaded by design (WASM compat) |
| `duckdb` / `polars` | Query engine for 78 cards is absurd -- HashMap lookup is fine |
| `chrono` | No date handling needed in card data |
| `uuid` | Card identity uses `ObjectId(u64)` and string names, not UUIDs |
| `semver` | No version comparison needed |
| `jsonschema` | serde deserialization provides validation; schema generation (schemars) is dev-only |
| `derive_builder` | Card/ability types are simple enough for manual construction |
| `strum` | Existing enum patterns with serde tag/content work well |
| Any ORM/database | 78 cards in a HashMap is the right data structure |

## Dependency Changes Summary

### engine crate (`Cargo.toml`)

```toml
[dependencies]
# Existing (unchanged)
serde = { workspace = true }
rpds = { workspace = true }
thiserror = "2"
rand = "0.9"
rand_chacha = "0.9"
indexmap = { version = "2", features = ["serde"] }
petgraph = "0.6"
serde_json = "1"

# CHANGED: Feature-gate walkdir for Forge compat
walkdir = { version = "2", optional = true }

[features]
default = []
forge-compat = ["dep:walkdir"]

[dev-dependencies]
tempfile = "3"
# NEW
insta = { version = "1.46", features = ["json"] }
test-case = "3.3"
schemars = "1.2"   # dev-only: generate .schema.json for ability format
```

### Workspace-level changes

None. No new workspace dependencies needed.

### Frontend (no changes)

The frontend requires zero new dependencies. Card metadata from MTGJSON is consumed on the Rust side only. The TypeScript types generated by `tsify` from Rust structs will change shape (new ability format fields) but no new npm packages are needed.

## Installation

```bash
# Rust dev tools:
cargo install cargo-insta    # Snapshot review CLI

# Download MTGJSON StandardAtomic.json (one-time, check into repo)
curl -L https://mtgjson.com/api/v5/StandardAtomic.json.gz | gunzip > data/mtgjson/StandardAtomic.json
```

## Integration Points

### MTGJSON -> Engine Type Mapping

| MTGJSON Field | Engine Type | Notes |
|---------------|-------------|-------|
| `name` | `CardFace.name: String` | Direct mapping |
| `manaCost` | `CardFace.mana_cost: ManaCost` | Parse `"{2}{W}{U}"` format (different from Forge's `2 W U`) |
| `types` | `CardType.core_types` | `["Creature"]` -> `vec![CoreType::Creature]` |
| `subtypes` | `CardType.subtypes` | `["Human", "Wizard"]` |
| `supertypes` | `CardType.supertypes` | `["Legendary"]` |
| `power` / `toughness` | `CardFace.power/toughness: Option<String>` | Direct mapping (handles `*` values) |
| `loyalty` | `CardFace.loyalty: Option<String>` | Direct mapping |
| `defense` | `CardFace.defense: Option<String>` | Direct mapping |
| `colors` | `CardFace.color_override` | `["W", "U"]` -> `vec![ManaColor::White, ManaColor::Blue]` |
| `keywords` | `CardFace.keywords: Vec<String>` | Direct mapping |
| `layout` | `CardLayout` enum | `"transform"` -> `CardLayout::Transform(...)` |
| `side` | Face ordering in layout | `"a"` = front face, `"b"` = back face |
| `faceName` | Per-face name for multi-face cards | Used to match faces in `StandardAtomic.json` (cards with `//` in name) |
| `text` | `CardFace.oracle_text` | Oracle text for display |
| `identifiers.scryfallOracleId` | Frontend Scryfall image lookup | Replaces name-based lookup for reliability |

### MTGJSON Mana Cost Format Difference

Forge format: `2 W U` (space-separated, bare letters)
MTGJSON format: `{2}{W}{U}` (curly-brace wrapped)

A new mana cost parser is needed for the `{...}` format. The existing `mana_cost::parse()` handles Forge's format. Add `mana_cost::parse_mtgjson()` that strips braces and delegates to the same internal logic. Hybrid costs in MTGJSON: `{W/U}`, phyrexian: `{W/P}`, X: `{X}`.

### Ability JSON -> Engine Type Mapping

Current Forge format: `"SP$ DealDamage | NumDmg$ 3 | ValidTgts$ Any"`
New JSON format maps directly to existing Rust types:

```json
{
  "type": "Spell",
  "effect": "DealDamage",
  "params": { "amount": 3 },
  "targets": { "valid": "Any", "prompt": "Choose a target" }
}
```

This maps to the existing `AbilityDefinition` / `ResolvedAbility` pipeline. The effect registry keys (`"DealDamage"`, `"Draw"`, etc.) remain the same -- only the card definition format changes, not the engine dispatch.

### Scryfall Image Integration Improvement

Current: Frontend fetches by card name (`/cards/named?exact=Lightning+Bolt`)
With MTGJSON: Can use `identifiers.scryfallOracleId` for direct lookup (`/cards/{scryfallId}`)
Benefit: More reliable than name matching (handles special characters, alternative names)
Note: This is a frontend-only optimization, not a hard dependency -- name-based lookup still works as fallback.

### Test Infrastructure Pattern (XMage-Inspired)

XMage tests (MIT-licensed) follow this pattern:
```java
addCard(Zone.BATTLEFIELD, playerA, "Lightning Bolt");
castSpell(1, PhaseStep.PRECOMBAT_MAIN, playerA, "Lightning Bolt");
assertPermanentCount(playerA, "Some Creature", 0);
```

Forge.rs equivalent using the new test stack:
```rust
#[test_case("Lightning Bolt", "Grizzly Bears", 0 ; "bolt kills bear")]
#[test_case("Shock", "Grizzly Bears", 0 ; "shock kills bear")]
#[test_case("Shock", "Hill Giant", 1 ; "shock does not kill giant")]
fn damage_spell_kills_creature(spell: &str, creature: &str, expected_count: usize) {
    let mut state = setup_game_at_main_phase();
    let creature_id = spawn_creature(&mut state, creature, PlayerId(1));
    cast_and_resolve(&mut state, spell, vec![TargetRef::Object(creature_id)]);

    let battlefield = state.zones.get(&Zone::Battlefield).unwrap();
    assert_eq!(battlefield.len(), expected_count);
}
```

For complex state verification, use insta snapshots:
```rust
#[test]
fn enchantment_enters_and_grants_ability() {
    let mut state = setup_game_at_main_phase();
    // ... setup and actions ...
    assert_json_snapshot!(extract_battlefield_state(&state));
}
```

## WASM Size Impact

| Change | Estimated Impact |
|--------|-----------------|
| Remove `walkdir` from default build | -15 KB |
| Own MTGJSON struct vs `mtgjson` crate | -0 KB (no crate added) |
| `schemars` (dev-only, not in release) | +0 KB |
| `insta` (dev-dependency only) | +0 KB |
| `test-case` (dev-dependency only) | +0 KB |
| Net WASM size change | **-15 KB** (smaller) |

## Confidence Assessment

| Decision | Confidence | Rationale |
|----------|------------|-----------|
| Own MTGJSON types over `mtgjson` crate | HIGH | Verified: crate missing `defense`, pulls 3 unnecessary deps. Official docs confirm fields needed. |
| `StandardAtomic.json` as data source | HIGH | Official MTGJSON docs confirm it's Standard-only atomic data. Perfect match for 78-card scope. |
| `schemars` for schema generation | HIGH | v1.2.1 released Feb 2026, MIT license, deep serde compat verified on docs.rs. |
| `insta` for snapshot testing | HIGH | v1.46.3, widely used, GameState already implements Serialize. |
| `test-case` for parameterized tests | HIGH | v3.3.1, stable proc macro, clean syntax verified on docs.rs. |
| Feature-gating Forge parser | HIGH | Standard Cargo feature pattern, `optional = true` for walkdir confirmed working. |
| No frontend stack changes | HIGH | MTGJSON consumed in Rust only; tsify generates new TS types automatically. |
| MTGJSON mana cost parser needed | HIGH | Format difference verified: `{2}{W}{U}` vs `2 W U`. Straightforward string transformation. |

## Sources

- [MTGJSON Official Documentation](https://mtgjson.com/getting-started/)
- [MTGJSON Card (Atomic) Data Model](https://mtgjson.com/data-models/card/card-atomic/)
- [MTGJSON Card (Set) Data Model](https://mtgjson.com/data-models/card/card-set/)
- [MTGJSON All Files Downloads](https://mtgjson.com/downloads/all-files/)
- [mtgjson Rust crate docs (5.2.2)](https://docs.rs/mtgjson/latest/mtgjson/index.html)
- [mtgjson AtomicCard struct](https://docs.rs/mtgjson/latest/mtgjson/struct.AtomicCard.html)
- [mtgjson SetCard struct](https://docs.rs/mtgjson/latest/mtgjson/struct.SetCard.html)
- [mtgjson-sdk crate docs](https://docs.rs/mtgjson-sdk/latest/mtgjson_sdk/index.html)
- [schemars documentation](https://graham.cool/schemars/)
- [schemars GitHub](https://github.com/GREsau/schemars)
- [insta snapshot testing](https://insta.rs/)
- [insta crate docs](https://docs.rs/insta)
- [test-case crate docs](https://docs.rs/test-case/latest/test_case/)
- [jsonschema crate](https://docs.rs/jsonschema)
- [XMage GitHub (MIT license)](https://github.com/magefree/mage)
- [XMage Testing Tools Wiki](https://github.com/magefree/mage/wiki/Development-Testing-Tools)
