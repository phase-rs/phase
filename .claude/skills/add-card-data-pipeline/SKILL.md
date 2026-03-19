---
name: add-card-data-pipeline
description: Use when modifying the card data pipeline — adding new fields to card exports, changing Oracle parser output shape, updating card database loading, modifying the coverage report, adding synthesis functions, or debugging why a card's data looks wrong at runtime.
---

# Card Data Pipeline

> **Hard rules — all pipeline work must respect these (see CLAUDE.md § Design Principles):**
> 1. **CR-correctness is non-negotiable.** The pipeline is the bridge between Oracle text and engine types. Parser output must faithfully represent the Comprehensive Rules semantics of each card. If a synthesis function or parser pattern produces an incorrect typed representation, the engine will enforce wrong rules. Verify every new pattern against the relevant CR section and annotate the code.
> 2. **Build for the class, not the card.** Every parser pattern, synthesis function, and export field must handle a *category* of cards. A synthesis function that works for one card but mishandles the next card with the same keyword pattern is a defect. Test with multiple representative cards from the class.
> 3. **Test the building block.** Every new parser pattern needs a unit test. Every new synthesis function needs a test verifying its output. Run `cargo coverage` after changes to verify coverage improvements. Run `cargo insta review` if snapshot tests are affected.

The card data pipeline converts raw MTGJSON card data into the typed `card-data.json` consumed by the WASM frontend and multiplayer server. Understanding this pipeline is essential when adding new ability types, fields, or synthesis steps — a change to the export format must propagate through all loading paths.

**Before you start:** Run the pipeline end-to-end once to see what it produces: `cargo run --release --bin oracle-gen -- data --stats > /tmp/test-export.json 2>/tmp/stats.txt && head -1 /tmp/stats.txt`

---

## Pipeline Flow

```
MTGJSON (AtomicCards.json, ~30k cards)
    ↓ download: scripts/gen-card-data.sh
data/mtgjson/AtomicCards.json
    ↓ parse: crates/engine/src/database/mtgjson.rs
HashMap<String, Vec<AtomicCard>>
    ↓ process: crates/engine/src/database/oracle_loader.rs
    │   For each card face:
    │   ├─ build_card_type() → CardType
    │   ├─ parse_oracle_text() → ParsedAbilities { abilities, triggers, statics, replacements }
    │   ├─ synthesize_basic_land_mana()
    │   ├─ synthesize_equip()
    │   ├─ synthesize_ninjutsu()
    │   ├─ synthesize_changeling_cda()
    │   └─ synthesize_kicker()
    ↓ export: crates/engine/src/bin/oracle_gen.rs
client/public/card-data.json (~49 MB in the current export)
    ↓ consume (three loading paths):
    ├─ WASM: CardDatabase::from_json_str()  [browser, via engine-wasm]
    ├─ Server: CardDatabase::from_export()   [phase-server, multiplayer]
    └─ CI: CardDatabase::from_export()       [coverage-report binary]
```

## Quick Commands

**Regenerate card data:**
```bash
cargo run --release --bin oracle-gen -- data --stats > client/public/card-data.json
```

**Look up a specific card's exported data:**
```bash
jq '.["card name in lowercase"]' client/public/card-data.json
# Example:
jq '.["lightning bolt"]' client/public/card-data.json
```

**Check if a card has unimplemented abilities:**
```bash
jq '.["card name"] | {abilities: [.abilities[]? | select(.effect.type == "Unimplemented")], triggers: [.triggers[]? | select(.mode == "Unknown")]}' client/public/card-data.json
```

**Run coverage report:**
```bash
cargo coverage                                    # Standard-legal cards
cargo run --bin coverage-report -- data --all     # All cards
cargo run --bin coverage-report -- data --ci      # CI mode (exits 1 on gaps)
```

---

## Key Files

### Data Source — MTGJSON

**`crates/engine/src/database/mtgjson.rs`** — Deserialization types for MTGJSON format.

```rust
pub struct AtomicCardsFile {
    pub data: HashMap<String, Vec<AtomicCard>>,  // name → faces
}

pub struct AtomicCard {
    pub name: String,
    pub mana_cost: Option<String>,        // "{2}{R}" format
    pub colors: Option<Vec<String>>,       // ["R", "G"]
    pub types: Vec<String>,               // ["Creature"]
    pub subtypes: Option<Vec<String>>,     // ["Elf", "Warrior"]
    pub supertypes: Option<Vec<String>>,   // ["Legendary"]
    pub power: Option<String>,            // "3" or "*"
    pub toughness: Option<String>,
    pub text: Option<String>,             // Oracle text
    pub keywords: Option<Vec<String>>,     // ["Flying", "Ward"]
    pub layout: Option<String>,           // "normal", "transform", "split", etc.
    pub legalities: Option<HashMap<String, String>>,
    pub identifiers: Option<Identifiers>,  // includes scryfall_oracle_id
}
```

### Oracle Loader — `crates/engine/src/database/oracle_loader.rs`

**`build_oracle_face(mtgjson, oracle_id) → CardFace`** — Converts one MTGJSON face into a typed `CardFace`:
1. `build_card_type()` — maps type strings to `CardType { supertypes, core_types, subtypes }`
2. `parse_oracle_text()` — runs Oracle parser → `ParsedAbilities`
3. `synthesize_basic_land_mana()` — injects tap-for-mana for Plains/Island/Swamp/Mountain/Forest
4. `synthesize_equip()` — converts `Keyword::Equip(cost)` → activated `Effect::Attach`
5. `synthesize_ninjutsu()` — converts `Keyword::Ninjutsu(cost)` → activated ninjutsu ability
6. `synthesize_changeling_cda()` — converts `Keyword::Changeling` → CDA static ability
7. `synthesize_kicker()` — converts `Keyword::Kicker(cost)` into `additional_cost`

### Export Binary — `crates/engine/src/bin/oracle_gen.rs`

Orchestrates the full export:
1. Load MTGJSON via `load_atomic_cards()`
2. For each card: `build_oracle_face()` per face
3. Map layout to `CardLayout` enum (Single, Split, Transform, etc.)
4. Normalize legalities
5. Flatten to `HashMap<String, CardExportEntry>` (key = lowercase face name, value = flattened `CardFace` + `legalities`)
6. Serialize to JSON → stdout

### Card Database — `crates/engine/src/database/card_db.rs`

Three loading methods:

| Method | Used By | What It Loads |
|--------|---------|--------------|
| `from_mtgjson(path)` | `oracle_gen`, tests | Full parse — runs Oracle parser on raw MTGJSON |
| `from_export(path)` | `phase-server`, `coverage-report` | Pre-processed `card-data.json` with flattened face entries + legalities |
| `from_json_str(json)` | `engine-wasm` (browser), deck validation tests | Same as `from_export` but takes string input |

**Internal structure:**
```rust
pub struct CardDatabase {
    pub cards: HashMap<String, CardRules>,     // Populated by from_mtgjson
    pub face_index: HashMap<String, CardFace>, // Populated by all loading paths
    pub legalities: HashMap<String, CardLegalities>,
    pub errors: Vec<(PathBuf, String)>,
}
```

### Card Face — `crates/engine/src/types/card.rs`

The serialized unit in card-data.json:

```rust
pub struct CardFace {
    pub name: String,
    pub mana_cost: ManaCost,
    pub card_type: CardType,
    pub power: Option<PtValue>,
    pub toughness: Option<PtValue>,
    pub loyalty: Option<String>,
    pub defense: Option<String>,
    pub oracle_text: Option<String>,
    pub non_ability_text: Option<String>,
    pub flavor_name: Option<String>,
    pub keywords: Vec<Keyword>,
    pub abilities: Vec<AbilityDefinition>,
    pub triggers: Vec<TriggerDefinition>,
    pub static_abilities: Vec<StaticDefinition>,
    pub replacements: Vec<ReplacementDefinition>,
    pub color_override: Option<Vec<ManaColor>>,
    pub scryfall_oracle_id: Option<String>,
    pub modal: Option<ModalChoice>,
    pub additional_cost: Option<AdditionalCost>,
    pub casting_restrictions: Vec<CastingRestriction>,
    pub casting_options: Vec<SpellCastingOption>,
}
```

### Coverage Report — `crates/engine/src/bin/coverage_report.rs`

Checks each card face for:
- `Keyword::Unknown(_)` → unsupported keyword
- `Effect::Unimplemented { .. }` → unsupported effect
- `TriggerMode::Unknown(_)` → unsupported trigger
- Unrecognized `StaticMode` → unsupported static

Outputs JSON summary: `{ total_cards, supported_cards, coverage_pct, cards: [...], missing_handler_frequency: [...] }`

Important current behavior:
- `coverage-report` loads `card-data.json` via `CardDatabase::from_export()`, not raw MTGJSON
- Without `--all`, it filters to `standard-cards.txt`
- It strips known-benign MTGJSON keyword mismatches (bare parameterized keywords like `Keyword:Ward` and action-keyword noise like `Keyword:Scry`) before computing final manifest coverage

CI mode (`--ci`): exits 1 if any Standard-legal cards have gaps.

### Coverage Module — `crates/engine/src/game/coverage.rs`

```rust
pub fn unimplemented_mechanics(obj: &GameObject) -> Vec<String>
```

Checks a game object at runtime for unsupported mechanics. Used by the frontend to show amber "!" badges on cards with partial support.

---

## Checklist — Modifying the Pipeline

### Adding a New Field to Card Export

When a new feature needs data that isn't currently exported:

- [ ] **`crates/engine/src/types/card.rs` — `CardFace` struct**
  Add the field. Use `#[serde(default)]` for backward compatibility — existing card-data.json files must still deserialize.

- [ ] **`crates/engine/src/database/oracle_loader.rs` — `build_oracle_face()`**
  Populate the new field during card processing.

- [ ] **`crates/engine/src/bin/oracle_gen.rs`** — Usually no changes needed (serializes `CardFace` automatically via serde).

- [ ] **Regenerate card-data.json**: `cargo run --release --bin oracle-gen -- data --stats > client/public/card-data.json`

- [ ] **`client/src/adapter/types.ts` — TypeScript types** (if frontend needs the field)
  Add the optional field to the card type definition.

### Adding a New Synthesis Function

When a keyword or ability implies game mechanics that Oracle text doesn't make explicit:

- [ ] **`crates/engine/src/database/oracle_loader.rs` — new `synthesize_*()` function**
  Pattern: takes `&mut CardFace`, checks for the triggering condition (keyword, type, etc.), adds abilities/triggers/statics to the face.

- [ ] **`crates/engine/src/database/oracle_loader.rs` — call from `build_oracle_face()`**
  Add the call in the synthesis section (after `parse_oracle_text()`, alongside existing synthesis calls).

- [ ] **Test**: Add a test in `oracle_loader.rs` verifying the synthesis produces the expected output.

### Modifying the Oracle Parser Output Shape

When a parser change affects the structure of `ParsedAbilities`:

- [ ] **Update all three loading paths** — `from_mtgjson`, `from_export`, and `from_json_str` all need to understand the serialized `CardFace` shape. If you add/rename fields, verify both raw-MTGJSON loading and flattened export loading.

- [ ] **Regenerate card-data.json** — Always regenerate after parser changes.

- [ ] **Update coverage report** — If the parser now recognizes previously-unimplemented patterns, coverage numbers will change. Run the report to verify improvements, and update any benign-keyword filtering in `coverage_report.rs` if the mismatch profile changed.

- [ ] **Update snapshot tests** — `crates/engine/tests/oracle_parser.rs` has `insta` snapshots that must be updated: `cargo insta review`.

### Adding Cards to the Standard Manifest

The CI coverage gate checks cards listed in `data/standard-cards.txt`:

- [ ] **`data/standard-cards.txt`** — Add card names (one per line, exact match). These cards must have 100% ability support or CI fails.

---

## WASM Loading Path

Understanding how card data reaches the browser:

1. **Build time**: `oracle-gen` produces `client/public/card-data.json`
2. **Runtime**: Frontend fetches `/card-data.json` via HTTP
3. **WASM init**: `load_card_database(json_str)` in `crates/engine-wasm/src/lib.rs`
4. **Parse**: `CardDatabase::from_json_str()` → deserializes flattened export entries into `face_index` + normalized `legalities`
5. **Storage**: Thread-local `CARD_DB: RefCell<Option<CardDatabase>>`
6. **Usage**: `initialize_game()` resolves deck card names via `face_index` lookup

**Important**: The WASM bridge receives the JSON as a string from JavaScript, not as a file path. Any changes to the JSON structure must round-trip through serde correctly.

---

## Common Mistakes

| Mistake | Consequence | Fix |
|---------|-------------|-----|
| Missing `#[serde(default)]` on new `CardFace` fields | Existing card-data.json fails to deserialize | Always default new optional fields |
| Changing field types without regenerating card-data.json | Runtime deserialization panic | Always regenerate after type changes |
| Adding synthesis but not calling from `build_oracle_face()` | Synthesis exists but never runs — abilities missing from export | Add the call |
| Not updating TypeScript types | Frontend can't access new fields, or gets wrong types | Update `adapter/types.ts` |
| Modifying `CardFace` but only testing `from_mtgjson` path | `from_export` and `from_json_str` may fail differently | Test all three paths |
| Adding a card to `standard-cards.txt` before it's fully supported | CI breaks | Only add fully-supported cards |
| Forgetting to regenerate after parser changes | Card-data.json contains stale parsed data | Run `oracle-gen` after any parser modification |
| Parser produces new support but coverage still looks wrong | Manifest filtering or benign MTGJSON keyword mismatches hide the real result | Check `coverage_report.rs` filtering before assuming the parser regressed |

---

## Self-Maintenance

After completing work using this skill:

1. **Verify references** with the check below
2. **Update the synthesis function list** if you added a new one
3. **Update the CardFace fields** if the struct changed

### Verification

```bash
rg -q "fn build_oracle_face" crates/engine/src/database/oracle_loader.rs && \
rg -q "fn synthesize_basic_land_mana" crates/engine/src/database/oracle_loader.rs && \
rg -q "fn synthesize_equip" crates/engine/src/database/oracle_loader.rs && \
rg -q "fn synthesize_changeling_cda" crates/engine/src/database/oracle_loader.rs && \
rg -q "fn from_export" crates/engine/src/database/card_db.rs && \
rg -q "fn from_json_str" crates/engine/src/database/card_db.rs && \
rg -q "fn from_mtgjson" crates/engine/src/database/card_db.rs && \
rg -q "struct CardFace" crates/engine/src/types/card.rs && \
rg -q "fn parse_oracle_text" crates/engine/src/parser/oracle.rs && \
rg -q "fn unimplemented_mechanics" crates/engine/src/game/coverage.rs && \
test -f crates/engine/src/bin/oracle_gen.rs && \
test -f crates/engine/src/bin/coverage_report.rs && \
test -f data/standard-cards.txt && \
echo "✓ add-card-data-pipeline skill references valid" || \
echo "✗ STALE — update skill references"
```
