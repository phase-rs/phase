# Architecture Patterns

**Domain:** MTG game engine data source migration and test infrastructure
**Researched:** 2026-03-10

## Executive Summary

The v1.2 milestone introduces three architectural changes to the existing engine:

1. **MTGJSON integration** -- a new data source providing card metadata (names, types, costs, colors, keywords, legalities) from MIT-licensed JSON files
2. **Custom ability JSON schema** -- replacing Forge's pipe-delimited ability strings (`SP$ DealDamage | NumDmg$ 3`) with a typed JSON format that maps directly to the engine's existing `AbilityDefinition`, `TriggerDefinition`, `StaticDefinition`, and `ReplacementDefinition` types
3. **Comprehensive test infrastructure** -- scenario-based integration tests that set up game state, execute actions, and assert outcomes, inspired by XMage's MIT-licensed test patterns

The core insight is that **the engine's internal types remain unchanged**. The migration affects the *input pipeline* (how card data enters the engine) and *validation pipeline* (how correctness is verified), not the engine itself. The `apply(state, action) -> ActionResult` reducer, the `HashMap<ObjectId, GameObject>` store, the effect/trigger/static/replacement registries, and the WASM bridge all stay exactly as they are.

## Recommended Architecture

### Current Data Flow (v1.1)

```
Forge .txt files  -->  parser/card_parser.rs  -->  CardFace { abilities: Vec<String>, ... }
                                                       |
                                              deck_loading.rs / create_object_from_card_face()
                                                       |
                                              GameObject { abilities: Vec<String>, ... }
                                                       |
                                              engine.rs: parse_ability() at use-time
                                                       |
                                              ResolvedAbility { api_type, params, ... }
                                                       |
                                              effects::resolve_effect(registry, ...)
```

### Target Data Flow (v1.2)

```
MTGJSON (card metadata)  +  Custom JSON (ability definitions)
         |                              |
    mtgjson/mod.rs                json_abilities/mod.rs
         |                              |
         +-------> CardFace <-----------+
                      |
              deck_loading.rs (unchanged)
                      |
              GameObject (unchanged)
                      |
              engine.rs (unchanged)
```

The key architectural decision: **MTGJSON provides card metadata, the custom JSON format provides ability behavior, and they merge into the same `CardFace` type the engine already consumes.** No downstream code changes.

## Component Boundaries

### New Components

| Component | Location | Responsibility | Communicates With |
|-----------|----------|---------------|-------------------|
| MTGJSON loader | `crates/engine/src/mtgjson/` | Deserialize MTGJSON AtomicCards into internal types | `types/card.rs` (produces `CardFace`) |
| MTGJSON types | `crates/engine/src/mtgjson/types.rs` | serde structs matching MTGJSON v5 Card (Atomic) schema | MTGJSON loader |
| Ability JSON schema | `crates/engine/src/abilities/` | Define + deserialize typed JSON ability format | `types/ability.rs` (produces definitions) |
| Ability JSON cards | `data/cards/` | JSON card files with ability definitions | Ability JSON loader |
| Unified CardDatabase v2 | `crates/engine/src/database/` | Load cards from MTGJSON + ability JSON, replacing txt-only loading | `mtgjson/`, `abilities/`, `types/card.rs` |
| Test harness | `crates/engine/src/testing/` | Scenario builder API for game state setup and assertion | `game/engine.rs`, `types/game_state.rs` |
| Test scenarios | `crates/engine/tests/` or inline `#[cfg(test)]` | Individual rule/card correctness tests | Test harness |

### Modified Components

| Component | File | Change | Risk |
|-----------|------|--------|------|
| `CardDatabase` | `database/card_db.rs` | Add `load_json()` method alongside existing `load()` | LOW -- additive |
| `CardFace` | `types/card.rs` | Possibly add optional fields (MTGJSON identifiers, oracle_id) | LOW -- additive |
| `deck_loading.rs` | `game/deck_loading.rs` | None if CardFace stays compatible | NONE |
| `coverage.rs` | `game/coverage.rs` | Update to work with new card source | LOW |
| `test_helpers.rs` | `game/test_helpers.rs` | Replace Forge DB dependency with JSON-based card loading | MEDIUM |
| Forge parser | `parser/` | Mark as optional/dev-only, gate behind feature flag | LOW |

### Unchanged Components

Everything downstream of `CardFace` and `GameObject`:
- `engine.rs` (apply function)
- All effect handlers (`effects/`)
- All trigger matchers (`triggers.rs`)
- All static ability handlers (`static_abilities.rs`)
- All replacement handlers (`replacement.rs`)
- Layer system (`layers.rs`)
- Combat system (`combat.rs`, `combat_damage.rs`)
- WASM bridge (`engine-wasm/`)
- AI (`forge-ai/`)
- Frontend (React/TypeScript)
- Server (`forge-server/`)

## Detailed Component Design

### 1. MTGJSON Integration

**Source file:** `StandardAtomic.json` from MTGJSON v5 API (MIT license)
**Size:** A subset of AtomicCards.json (122.8 MB uncompressed). StandardAtomic-equivalent data is much smaller -- only Standard-legal cards.
**Format:** Top-level `{ "data": { "CardName": [{ ... }], ... }, "meta": { ... } }`

**MTGJSON Rust types** (new module `crates/engine/src/mtgjson/types.rs`):

```rust
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct MtgjsonFile {
    pub data: HashMap<String, Vec<MtgjsonCard>>,
    pub meta: MtgjsonMeta,
}

#[derive(Debug, Deserialize)]
pub struct MtgjsonMeta {
    pub date: String,
    pub version: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MtgjsonCard {
    pub name: String,
    pub mana_cost: Option<String>,       // e.g. "{2}{R}"
    pub mana_value: f32,
    pub colors: Vec<String>,             // ["R"]
    pub color_identity: Vec<String>,     // ["R"]
    pub types: Vec<String>,              // ["Instant"]
    pub supertypes: Vec<String>,         // ["Legendary"]
    pub subtypes: Vec<String>,           // ["Goblin", "Warrior"]
    pub power: Option<String>,           // "2"
    pub toughness: Option<String>,       // "2"
    pub loyalty: Option<String>,         // "3"
    pub defense: Option<String>,         // "3"
    pub text: Option<String>,            // Oracle text
    pub keywords: Option<Vec<String>>,   // ["Flying", "Haste"]
    pub layout: String,                  // "normal", "transform", "split", etc.
    pub side: Option<String>,            // "a", "b"
    pub face_name: Option<String>,       // Individual face name
    pub legalities: MtgjsonLegalities,
    // Fields we don't need but should tolerate via #[serde(flatten)]
    // or simply mark as Option and ignore.
}

#[derive(Debug, Deserialize)]
pub struct MtgjsonLegalities {
    pub standard: Option<String>,        // "Legal", "Banned", "Restricted"
    pub modern: Option<String>,
    pub pioneer: Option<String>,
    // ... other formats
}
```

**Key mapping from MTGJSON to existing types:**

| MTGJSON field | Forge.rs type | Notes |
|---------------|---------------|-------|
| `name` | `CardFace.name` | Direct 1:1 |
| `manaCost` | `CardFace.mana_cost` | Parse `{2}{R}` to `ManaCost` (different format from Forge's `2 R`) |
| `types` | `CardFace.card_type.core_types` | Map strings to `CoreType` enum |
| `supertypes` | `CardFace.card_type.supertypes` | Map strings to `Supertype` enum |
| `subtypes` | `CardFace.card_type.subtypes` | Direct strings |
| `power` / `toughness` | `CardFace.power` / `CardFace.toughness` | Already `Option<String>` |
| `loyalty` | `CardFace.loyalty` | Already `Option<String>` |
| `defense` | `CardFace.defense` | Already `Option<String>` |
| `colors` | `CardFace.color_override` | Map `["R", "G"]` to `Vec<ManaColor>` |
| `keywords` | `CardFace.keywords` | Direct strings (already parsed downstream by `parse_keywords()`) |
| `layout` / `side` | `CardLayout` variant | `"normal"` -> `Single`, `"transform"` + sides -> `Transform(a, b)`, etc. |
| `text` | `CardFace.oracle_text` | Direct 1:1 |
| `legalities.standard` | Filter criterion | Used to curate Standard-legal subset |

**What MTGJSON does NOT provide:** Ability definitions. MTGJSON has `text` (oracle text as a string) and `keywords`, but not machine-readable ability definitions like Forge's `SP$ DealDamage | NumDmg$ 3`. This is the exact gap the custom ability JSON format fills.

**Mana cost format difference:** Forge uses `2 R` (space-separated), MTGJSON uses `{2}{R}` (brace-wrapped). A new parser function `parse_mtgjson_mana_cost("{2}{R}") -> ManaCost` is needed alongside the existing `mana_cost::parse("2 R")`.

### 2. Custom Ability JSON Format

The ability format must express the same information currently in Forge's pipe-delimited strings, but as typed JSON that maps 1:1 to the engine's existing Rust types.

**Current Forge format (strings on CardFace):**
```
abilities: ["SP$ DealDamage | Cost$ R | NumDmg$ 3 | ValidTgts$ Any"]
triggers: ["Mode$ ChangesZone | Origin$ Any | Destination$ Battlefield | Execute$ TrigDraw"]
static_abilities: ["Mode$ Continuous | Affected$ Creature.YouCtrl | AddPower$ 1"]
replacements: ["Event$ DamageDone | ActiveZones$ Battlefield | ValidSource$ Card.Self"]
svars: {"TrigDraw": "DB$ Draw | NumCards$ 1"}
```

**Proposed JSON format (per-card file):**

```json
{
  "name": "Lightning Bolt",
  "abilities": [
    {
      "kind": "Spell",
      "api_type": "DealDamage",
      "params": {
        "Cost": "R",
        "NumDmg": "3",
        "ValidTgts": "Any"
      }
    }
  ],
  "triggers": [],
  "static_abilities": [],
  "replacements": [],
  "svars": {}
}
```

This maps directly to the existing types without any changes:

```rust
// Already exists in types/ability.rs -- NO changes needed
pub struct AbilityDefinition {
    pub kind: AbilityKind,      // Spell | Activated | Database
    pub api_type: String,       // "DealDamage"
    pub params: HashMap<String, String>,  // {"NumDmg": "3", ...}
}

pub struct TriggerDefinition {
    pub mode: String,           // "ChangesZone"
    pub params: HashMap<String, String>,
}

pub struct StaticDefinition {
    pub mode: String,           // "Continuous"
    pub params: HashMap<String, String>,
}

pub struct ReplacementDefinition {
    pub event: String,          // "DamageDone"
    pub params: HashMap<String, String>,
}
```

**The critical insight:** The engine already has these typed definitions with serde derive. The Forge parser produces them by parsing pipe-delimited strings at parse time via `parse_ability()`, `parse_trigger()`, `parse_static()`, `parse_replacement()`. The JSON format simply skips the string-parsing step and deserializes directly into these same types.

**Multi-face card example:**

```json
{
  "name": "Bonecrusher Giant",
  "layout": "Adventure",
  "faces": [
    {
      "name": "Bonecrusher Giant",
      "abilities": [],
      "triggers": [
        {
          "mode": "BecomesTarget",
          "params": {
            "ValidCard": "Card.Self",
            "Execute": "TrigDmg"
          }
        }
      ],
      "static_abilities": [],
      "replacements": [],
      "svars": {
        "TrigDmg": "DB$ DealDamage | NumDmg$ 2 | Defined$ TriggeredSourceController"
      }
    },
    {
      "name": "Stomp",
      "abilities": [
        {
          "kind": "Spell",
          "api_type": "DealDamage",
          "params": { "Cost": "1 R", "NumDmg": "2", "ValidTgts": "Any" }
        }
      ],
      "triggers": [],
      "static_abilities": [],
      "replacements": [],
      "svars": {}
    }
  ]
}
```

**SVars in JSON:** Currently SVars are stored as `HashMap<String, String>` where values are pipe-delimited ability strings. The JSON ability files keep this same format for SVars because the engine's SVar resolution in `resolve_ability_chain()` calls `parse_ability(raw)` on SVar string values at runtime. Keeping SVar values as strings avoids touching the resolution path. A future optimization could type SVars as `HashMap<String, AbilityDefinition>`, but this is not required for v1.2.

**Why keep params as `HashMap<String, String>` rather than strongly typed enums:** The engine's 38+ effect handlers each consume different param keys. Strongly typing every param combination would mean creating one Rust enum variant per effect type's param schema -- a massive refactor with no gameplay benefit. The handlers already validate params at use-time with `params.get("NumDmg").ok_or(...)`.

### 3. Unified Card Loading Pipeline

**New `CardDatabase` loading path:**

```rust
impl CardDatabase {
    /// Existing: load from Forge .txt files (retained behind feature flag)
    #[cfg(feature = "forge-compat")]
    pub fn load(root: &Path) -> Result<Self, ParseError> { ... }

    /// New: load from MTGJSON + ability JSON files
    pub fn load_json(
        mtgjson_path: &Path,     // StandardAtomic.json or curated subset
        abilities_dir: &Path,     // data/cards/ directory with per-card JSON
    ) -> Result<Self, LoadError> {
        // 1. Deserialize MTGJSON file -> HashMap<String, Vec<MtgjsonCard>>
        // 2. For each card name, load matching ability JSON from abilities_dir
        // 3. Merge: MTGJSON provides metadata, ability JSON provides behavior
        // 4. Produce CardFace + CardLayout (same types as existing parser)
        // 5. Index into cards HashMap + face_index HashMap
    }
}
```

**Merge strategy:**
- MTGJSON provides: name, mana_cost, types/supertypes/subtypes, power, toughness, loyalty, defense, colors, keywords, oracle_text, layout, legalities
- Ability JSON provides: abilities, triggers, static_abilities, replacements, svars
- If a card has MTGJSON data but no ability JSON, it loads with empty abilities (vanilla creature/land works fine)
- If a card has ability JSON but no MTGJSON data, this is an error (should not happen in practice)

**Multi-face card handling:**
- MTGJSON uses `layout` + `side` + `faceName` for multi-face cards. Multi-face cards appear as separate entries in the AtomicCards array, one per face
- Ability JSON uses a `faces` array with per-face ability definitions
- The merge groups MTGJSON entries by name (split by `" // "`), matches faces by `faceName`, and combines into `CardLayout::Transform(a, b)`, `CardLayout::Adventure(a, b)`, etc.

**MTGJSON layout string to CardLayout mapping:**

| MTGJSON `layout` | Forge.rs `CardLayout` | Notes |
|-------------------|----------------------|-------|
| `"normal"` | `Single` | Single-face cards |
| `"transform"` | `Transform` | DFCs (Innistrad style) |
| `"modal_dfc"` | `Modal` | MDFCs (Zendikar Rising style) |
| `"split"` | `Split` | Split cards (Fire // Ice) |
| `"flip"` | `Flip` | Flip cards (Kamigawa) |
| `"adventure"` | `Adventure` | Adventure cards (Eldraine) |
| `"meld"` | `Meld` | Meld cards (Eldritch Moon) |
| `"token"`, `"emblem"` | Skipped | Not player cards |

### 4. Test Infrastructure

**Inspiration:** XMage (MIT-licensed) organizes tests by mechanic -- `abilities/`, `triggers/`, `combat/`, `rules/`, `single/` (individual cards). Their test base provides methods like `addCard(Zone.HAND, playerA, "Lightning Bolt", 1)`, action sequencing, and state assertions. This is the pattern to follow.

**Current test state:** The existing `test_helpers.rs` relies on the Forge card database being present on disk (`../../forge/forge-gui/res/cardsfolder/`), which is never available in CI. Tests that use `spawn_creature()` silently skip via `Option`. This is effectively untested.

**Proposed test harness** (new module `crates/engine/src/testing/`):

```rust
pub struct GameScenario {
    state: GameState,
}

impl GameScenario {
    /// Create a new two-player scenario with a seed
    pub fn new(seed: u64) -> Self;

    /// Add a card to a player's zone by constructing a GameObject from inline data
    pub fn add_card(&mut self, name: &str, owner: PlayerId, zone: Zone) -> ObjectId;

    /// Add a card with specific state modifications
    pub fn add_card_with(
        &mut self,
        name: &str,
        owner: PlayerId,
        zone: Zone,
        configure: impl FnOnce(&mut GameObject),
    ) -> ObjectId;

    /// Set a player's life total
    pub fn set_life(&mut self, player: PlayerId, life: i32);

    /// Set the current phase and active player
    pub fn set_phase(&mut self, phase: Phase);
    pub fn set_active_player(&mut self, player: PlayerId);

    /// Execute an action and return the result
    pub fn act(&mut self, action: GameAction) -> Result<ActionResult, EngineError>;

    /// Assertion helpers
    pub fn assert_life(&self, player: PlayerId, expected: i32);
    pub fn assert_zone_count(&self, player: PlayerId, zone: Zone, expected: usize);
    pub fn assert_zone_contains(&self, player: PlayerId, zone: Zone, card_name: &str);
    pub fn assert_battlefield_has(&self, card_name: &str) -> ObjectId;
    pub fn assert_battlefield_tapped(&self, object_id: ObjectId);
    pub fn assert_has_keyword(&self, object_id: ObjectId, keyword: Keyword);
    pub fn assert_waiting_for(&self, variant: &str);  // e.g. "Priority", "ManaPayment"

    /// Get the underlying state for custom assertions
    pub fn state(&self) -> &GameState;
    pub fn state_mut(&mut self) -> &mut GameState;
}
```

**Card data for tests:** Tests should NOT depend on the MTGJSON pipeline or external files. Instead, the `add_card` method creates `GameObject` instances inline using a small card registry of common test cards (Lightning Bolt, Grizzly Bears, etc.) embedded in the test module. This mirrors how `deck_loading.rs` tests already create `CardFace` instances manually. For card-specific tests that need exact ability definitions, the test can call `add_card_with` and configure abilities directly.

**Test organization:**

```
crates/engine/tests/
    rules/
        combat_test.rs          # Trample, first strike, deathtouch interactions
        priority_test.rs        # Stack ordering, priority passing
        state_based_test.rs     # 0 life, 0 toughness, legend rule
        layers_test.rs          # Layer 613 ordering
    effects/
        deal_damage_test.rs     # DealDamage effect correctness
        draw_test.rs            # Draw effect edge cases
        token_test.rs           # Token creation
    cards/
        lightning_bolt_test.rs  # Individual card behavior
        bonecrusher_giant_test.rs
    keywords/
        flying_test.rs          # Flying/reach blocking
        trample_test.rs         # Trample damage assignment
        deathtouch_test.rs      # Deathtouch + trample combo
```

**CI integration:** Tests run as standard `cargo test` with no external dependencies. No Forge directory, no MTGJSON download. All test data is embedded or in small fixture files checked into the repo.

## Patterns to Follow

### Pattern 1: Layered Data Loading (Metadata + Behavior Separation)

**What:** Separate card metadata (MTGJSON) from card behavior (ability JSON). Load metadata first, then overlay behavior definitions.

**When:** Loading any card for gameplay.

**Why:** MTGJSON provides comprehensive, MIT-licensed metadata that updates automatically with each set release. Ability definitions are hand-crafted and version-controlled. Separating concerns means MTGJSON updates don't require ability rework, and ability changes don't require re-fetching MTGJSON data.

### Pattern 2: Feature-Gated Parser Retention

**What:** Keep the Forge .txt parser behind a Cargo feature flag (`forge-compat`) rather than deleting it.

**When:** Development/migration tooling needs it.

**Why:** The parser is useful for: (a) validating ability JSON against Forge's known-good definitions during migration, (b) batch-converting additional cards from Forge format to JSON, (c) keeping the option to sync upstream if needed. Feature-gated means it adds zero bytes to the WASM binary in production.

```toml
# Cargo.toml
[features]
default = []
forge-compat = ["walkdir"]  # enables Forge .txt parser and CardDatabase::load()
```

### Pattern 3: Builder-Based Test Scenarios

**What:** Use a builder pattern for game test setup instead of manual state construction.

**When:** Every integration test.

**Why:** Manual `GameState::new_two_player()` + manual object creation is verbose and error-prone (see current `test_helpers.rs` which requires Forge DB on disk and silently returns `None` when unavailable). A builder makes tests readable and self-contained.

```rust
#[test]
fn lightning_bolt_deals_3_damage() {
    let mut s = GameScenario::new(42);
    let bolt_id = s.add_card("Lightning Bolt", PlayerId(0), Zone::Hand);
    s.set_phase(Phase::Main1);

    s.act(GameAction::CastSpell {
        card_id: CardId(1),
        targets: Some(vec![TargetRef::Player(PlayerId(1))]),
    }).unwrap();

    // Resolve the spell (pass priority twice)
    s.act(GameAction::PassPriority).unwrap();
    s.act(GameAction::PassPriority).unwrap();

    s.assert_life(PlayerId(1), 17);
}
```

### Pattern 4: Preserve Internal String-Based Ability Resolution

**What:** The JSON ability definitions produce the same `CardFace` with `abilities: Vec<String>` and `svars: HashMap<String, String>` that the Forge parser produces. The engine's runtime path remains unchanged.

**When:** During v1.2 migration. This is the safe approach.

**Why:** The engine has 38 effect handlers, 137 trigger matchers, 61 static handlers, and 45 replacement handlers. The `apply()` function, `resolve_ability_chain()`, `process_triggers()`, `check_static_ability()`, and `build_*_registry()` all work with string-based definitions. Changing the runtime representation would touch ~13 source files -- high risk, high effort, zero gameplay value.

**How:** The JSON loader reads typed `AbilityDefinition` from JSON (no string parsing needed), then serializes them back to the pipe-delimited string format for `CardFace.abilities`. This means the JSON gives authoring-time validation (schema errors caught at load time, not runtime) while preserving the proven runtime path.

**Future optimization:** Have `GameObject.abilities` store `Vec<AbilityDefinition>` instead of `Vec<String>`, eliminating the parse step in `engine.rs` and the 13 files that call `parse_ability()`. This is a cleaner end state but a separate refactor.

## Anti-Patterns to Avoid

### Anti-Pattern 1: Downloading MTGJSON at Runtime

**What:** Fetching MTGJSON data from the API during game initialization or build.

**Why bad:** Network dependency makes offline play unreliable, WASM builds cannot make arbitrary HTTP requests, and the full AtomicCards.json is 122.8 MB which would destroy startup time.

**Instead:** Curate a subset of MTGJSON data at development time using a build script. Check the resulting compact JSON (<500 KB for Standard) into the repository. Update periodically when new sets release.

### Anti-Pattern 2: One Monolithic Card JSON File

**What:** Putting all 78 card ability definitions in a single JSON file.

**Why bad:** Merge conflicts on every card change, hard to review individual cards, hard to diff. Individual card files in Forge's model (`data/cardsfolder/a/act_of_treason.txt`) are one of its strengths.

**Instead:** One JSON file per card in `data/cards/`, named by slug (`lightning_bolt.json`, `bonecrusher_giant.json`). A build-time step can concatenate into a single bundled file if needed for WASM binary size.

### Anti-Pattern 3: Strongly Typing Every Ability Parameter

**What:** Creating Rust enums for every possible parameter value (`ValidTgts::Any`, `ValidTgts::Creature`, etc.).

**Why bad:** There are hundreds of distinct parameter combinations across 38+ effect types. The current `HashMap<String, String>` design is intentional -- it allows adding new parameter patterns without changing the type system. Each effect handler validates its own params at runtime.

**Instead:** Keep `HashMap<String, String>` for params. The JSON schema provides structural validation (no more malformed pipes/dollars), while the runtime preserves the flexibility that made it possible to implement 202 effect types without a massive type hierarchy.

### Anti-Pattern 4: Testing Against Forge DB on Disk

**What:** The current `test_helpers.rs` loads `../../forge/forge-gui/res/cardsfolder/` which may or may not exist, and returns `Option::None` when it does not.

**Why bad:** Tests silently skip when the DB is missing (CI never has it). Tests depend on external unversioned data. There is no way to know if card behavior regressed.

**Instead:** All tests use self-contained data. Either inline card definitions in the test, or small JSON fixture files checked into the repo. No external filesystem dependencies. Every test runs in CI.

## Data File Organization

```
data/
    mtgjson/
        standard_cards.json          # Curated MTGJSON Standard subset (~200-500 KB)
    cards/
        lightning_bolt.json          # Ability definitions per card
        bonecrusher_giant.json
        sheoldred_the_apocalypse.json
        ...                          # 78 cards for initial migration
    cardsfolder/                     # Existing Forge .txt files (feature-gated, eventually removed)
        a/
        b/
        ...
```

### MTGJSON Data Curation

Rather than shipping the full 122 MB AtomicCards.json, create a build-time script that:
1. Downloads AtomicCards.json from `https://mtgjson.com/api/v5/AtomicCards.json`
2. Filters to Standard-legal cards using `legalities.standard == "Legal"`
3. Strips unnecessary fields (foreignData, purchaseUrls, identifiers, edhrecRank, etc.)
4. Retains only: name, manaCost, manaValue, colors, colorIdentity, types, supertypes, subtypes, power, toughness, loyalty, defense, text, keywords, layout, side, faceName, legalities
5. Writes a compact `standard_cards.json` (~200-500 KB estimated for ~2,000 Standard-legal cards)
6. This file is checked into the repo and updated when new sets release

## Build Order (Dependency-Aware)

The following build order respects dependencies and minimizes risk:

```
Phase 1: MTGJSON types + mana cost parser + loader (no downstream deps)
    |
Phase 2: Ability JSON schema + loader (no downstream deps, parallelizable with 1)
    |
Phase 3: Unified CardDatabase.load_json() merging MTGJSON + ability JSON
    |         (depends on Phases 1+2)
    |
Phase 4: Test harness -- GameScenario builder (can start during Phase 3)
    |
Phase 5: Migrate 78 cards from Forge .txt to MTGJSON + ability JSON
    |         (depends on Phase 3; uses Phase 4 tests to validate each card)
    |
Phase 6: Gate Forge parser behind feature flag (depends on Phase 5)
    |
Phase 7: Comprehensive test suite using GameScenario
    |         (partially parallelizable with Phase 5)
    |
Phase 8: License change + cleanup (depends on Phase 6)
```

Phases 1 and 2 are fully parallelizable.
Phases 4 and 5 can overlap (write tests as cards are migrated).
Phase 7 can start during Phase 5 (rules tests don't need all 78 cards).
Phase 8 is a non-code task that depends on removing the Forge data dependency.

## Scalability Considerations

| Concern | 78 cards (v1.2) | 1,000 cards (full Standard) | 32,000+ cards (all MTG) |
|---------|-----------------|----------------------------|-------------------------|
| Card ability files | 78 JSON files, <100 KB total | ~500 JSON files, ~1 MB | ~10,000 files, ~10 MB; consider bundling |
| MTGJSON data | ~50 KB curated | ~500 KB curated | 122 MB; must use lazy loading or binary format |
| Load time | Negligible | <100ms | Needs indexed lookup, not full deserialization |
| WASM binary size | Negligible impact | Card data embedded in binary, ~1 MB | Cannot embed; need external loading |
| Test fixture size | Inline in tests | Shared fixture files | Test fixture registry with lazy loading |

For v1.2 with 78 cards, every approach works. The architecture is designed to scale to full Standard (~2,000 cards) without redesign.

## Sources

- MTGJSON Card (Atomic) Data Model: https://mtgjson.com/data-models/card/card-atomic/ (HIGH confidence -- official documentation)
- MTGJSON Downloads / File sizes: https://mtgjson.com/api/v5/ (HIGH confidence -- verified file listing, AtomicCards.json is 122.8 MB)
- MTGJSON Rust crate: https://docs.rs/mtgjson (MEDIUM confidence -- exists with serde support, may not match latest v5 schema exactly; recommend custom types over this crate)
- XMage test structure: https://github.com/magefree/mage (HIGH confidence -- MIT license confirmed, test organization by mechanic verified)
- XMage testing tools: https://github.com/magefree/mage/wiki/Development-Testing-Tools (MEDIUM confidence -- wiki documentation, shows init.txt and zone-based card setup pattern)
- Existing codebase analysis: `crates/engine/src/` (HIGH confidence -- direct code inspection of parser/, database/, types/, game/ modules)
