# Phase 2: Card Parser & Database - Research

**Researched:** 2026-03-07
**Domain:** Forge card definition parsing, Rust text parsing, card database indexing
**Confidence:** HIGH

## Summary

This phase ports Forge's `CardRules.Reader.parseLine()` — a 175-line switch statement — into idiomatic Rust, along with `ManaCostParser`, `CardType.parse()`, and `CardDb`'s name-indexed lookup. The Forge source has been analyzed in detail: the parser is a single-pass line-by-line reader that splits on the first `:`, dispatches by first character then exact key match, and accumulates fields into a `CardFace`. Multi-face cards use the `ALTERNATE` delimiter (or `SPECIALIZE:COLOR` for Alchemy) to switch the active face index.

The card definition format is stable and well-documented by the 32,300+ files in `../forge/forge-gui/res/cardsfolder/`. Key sub-parsers (ManaCost and CardType) are straightforward: ManaCost splits on spaces and maps tokens to shard variants or generic integers; CardType splits on spaces and classifies each token as supertype, core type, or subtype by lookup. The ability string format (`Key$ Value | Key$ Value`) is pipe-delimited with `$` separating parameter name from value — this phase parses structure only, not semantics.

**Primary recommendation:** Translate Forge's `CardRules.Reader.parseLine()` directly, using Rust `match` on `&str` instead of Java's switch-on-char-then-if-chain. Use `HashMap<String, CardRules>` with lowercased keys for the database. All types derive `Debug, Clone, Serialize, Deserialize` per established project patterns.

<user_constraints>

## User Constraints (from CONTEXT.md)

### Locked Decisions
- Lenient parsing: unknown/unrecognized keys are silently skipped (matching Forge's switch fallthrough behavior)
- Single parse function with `match` on first character of key, then exact key match — direct Rust translation of Forge's `CardRules.Reader.parseLine`
- Card files use Forge's exact directory layout: `cardsfolder/{first_letter}/{card_name}.txt` with configurable root path
- Lines starting with `#` or empty lines are skipped (matching Forge)
- Line format: split on first `:` to get key and value
- `CardLayout` enum with face data as discriminated union: `Single(CardFace)`, `Split(CardFace, CardFace)`, `Transform(CardFace, CardFace)`, `Adventure(CardFace, CardFace)`, `Flip(CardFace, CardFace)`, `Meld(CardFace, CardFace)`, `Modal(CardFace, CardFace)`, `Specialize(CardFace, Vec<CardFace>)`
- Specialize included as a stub (parses but no runtime behavior) — Alchemy is out of scope
- `ALTERNATE` delimiter switches face context during parsing (matching Forge's `curFace` toggle)
- `AlternateMode` field maps to `CardLayout` variant (with `DoubleFaced` → `Transform` alias matching Forge's `smartValueOf`)
- All Forge ICardFace fields: name, mana_cost (parsed ManaCost), card_type (parsed CardType), pt, loyalty, defense, oracle_text, non_ability_text, flavor_name
- Lists: abilities (A:), triggers (T:), static_abilities (S:), replacement_effects (R:), keywords (K:)
- SVars stored as `HashMap<String, String>` — raw values, resolution deferred to Phase 4 (ABIL-02)
- ManaCost and CardType as proper parsed Rust types with their own sub-parsers (PARSE-04 requirement)
- ManaCost handles: colored, generic, hybrid, phyrexian, X costs, snow, "no cost"
- CardType separates supertypes, card types, and subtypes
- Eager loading of all card files at startup (matching Forge's approach)
- Primary index: `HashMap<String, CardRules>` with lowercased name keys for case-insensitive O(1) lookup
- Face-level index: separate `HashMap<String, CardFace>` for individual face lookup
- Target: sub-10ms single-card lookup
- Parse pipe-delimited `Key$ Value` format into typed `AbilityDefinition` struct
- Typed `AbilityKind` enum: `Spell` (SP$), `Activated` (AB$), `Database` (DB$)
- Extract ApiType as a string field
- Store all `Key$Value` parameters as `HashMap<String, String>`
- Triggers (T:) and statics (S:) use `Mode$` as their type discriminator
- Replacement effects (R:) use `Event$` as their type discriminator
- No semantic interpretation — that's Phase 4
- Port Forge's parser and database tests to Rust
- Test against real Forge card files
- Forge parity is the guiding principle

### Claude's Discretion
- Internal error types and Result patterns for parse failures
- Exact module organization within the `engine` crate (parser module, database module)
- Whether CardRules wraps CardLayout or is the same struct
- Performance optimization details (parallel file loading, memory layout)

### Deferred Ideas (OUT OF SCOPE)
- Forge's UI/mock card tests — Phase 7
- Card edition/set metadata (CardEdition, PaperCard with art preferences) — not needed for card rules parsing
- DeckHints/DeckNeeds/DeckHas parsing — Phase 7 deck builder; parse as raw strings for now
- AI hints (AI:RemoveDeck) — Phase 8 AI implementation

</user_constraints>

<phase_requirements>

## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| PARSE-01 | Parse Forge's .txt card definition format (Name, ManaCost, Types, PT, K, A, T, S, R, SVar, Oracle) | Full Forge `CardRules.Reader.parseLine()` analyzed — exact key mapping documented, `CardFace` field list confirmed |
| PARSE-02 | Support all multi-face card types (Split, Flip, Transform, Meld, Adventure, MDFC) | `CardSplitType` enum analyzed, `ALTERNATE`/`SPECIALIZE:COLOR` delimiters confirmed, example files for each type examined |
| PARSE-03 | Card database indexing by name with lazy loading | `CardDb` indexing analyzed — uses case-insensitive `TreeMap` by name plus face-level index. Decision: eager loading (not lazy), HashMap for O(1) |
| PARSE-04 | ManaCost and CardType sub-parsers | `ManaCostParser` and `ManaCostShard` enum fully analyzed (space-split tokens), `CardType.parse()` logic confirmed (supertype/coretype/subtype classification) |
| ABIL-01 | Ability parser for A:, T:, S:, R: strings into typed structures | Pipe-delimited `Key$ Value` format confirmed from card file examples; `SP$`/`AB$`/`DB$` ability kinds and `Mode$`/`Event$` discriminators verified |

</phase_requirements>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| serde | 1.x | Serialize/Deserialize for all types | Already in workspace, all types derive Serialize/Deserialize |
| serde_json | 1.x | JSON serialization for tests/WASM bridge | Already in dev-dependencies |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| rayon | 1.10 | Parallel file loading for 32k card files | Optional optimization for `CardDatabase::load()` — can be added later if needed |
| thiserror | 2.x | Derive macro for error types | Clean `ParseError` enum definition |
| walkdir | 2.x | Recursive directory traversal | Collecting `.txt` files from `cardsfolder/` tree |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| walkdir | std::fs::read_dir recursive | walkdir handles edge cases (symlinks, permissions) and is the ecosystem standard |
| thiserror | manual Error impl | thiserror reduces boilerplate for error enums, no runtime cost |
| rayon | std threads | rayon's par_iter is simpler; but 32k small files may not need parallelism — benchmark first |

**Installation (Cargo.toml):**
```toml
[dependencies]
serde = { workspace = true }
thiserror = "2"
walkdir = "2"

[dev-dependencies]
serde_json = "1"
```

## Architecture Patterns

### Recommended Module Structure
```
crates/engine/src/
├── lib.rs               # pub mod types; pub mod parser; pub mod database;
├── types/
│   ├── mod.rs           # Re-exports (existing)
│   ├── card.rs          # CardFace, CardRules, CardLayout (replaces stub)
│   ├── mana.rs          # ManaColor, ManaPool (existing) + ManaCost, ManaCostShard
│   ├── card_type.rs     # CardType, Supertype, CoreType
│   └── ability.rs       # AbilityDefinition, AbilityKind
├── parser/
│   ├── mod.rs           # pub use card_parser::*, mana_cost::*, card_type::*, ability::*
│   ├── card_parser.rs   # parse_card_file(), parse_line() — the main parser
│   ├── mana_cost.rs     # ManaCost::parse() sub-parser
│   ├── card_type.rs     # CardType::parse() sub-parser
│   └── ability.rs       # AbilityDefinition::parse() for pipe-delimited format
└── database/
    ├── mod.rs
    └── card_db.rs       # CardDatabase struct, load(), get_by_name(), get_face_by_name()
```

### Pattern 1: Line Parser with Match Dispatch
**What:** Direct translation of Forge's `CardRules.Reader.parseLine()`
**When to use:** Parsing each line of a card .txt file
**Example:**
```rust
// Source: Forge's CardRules.java lines 639-815
fn parse_line(line: &str, face: &mut CardFaceBuilder, state: &mut ParseState) {
    let Some((key, value)) = line.split_once(':') else {
        // Handle keys with no value (e.g., "ALTERNATE")
        match line.trim() {
            "ALTERNATE" => { state.cur_face = 1; }
            _ => {} // silently skip unknown keys
        }
        return;
    };
    let value = value.trim();

    match key.as_bytes().first() {
        Some(b'A') => match key {
            "A" => face.add_ability(value),
            "AlternateMode" => state.alt_mode = CardLayout::from_mode_str(value),
            "ALTERNATE" => state.cur_face = 1,
            _ => {} // silently skip (AI, etc.)
        },
        Some(b'K') => if key == "K" { face.add_keyword(value) },
        Some(b'M') => match key {
            "ManaCost" => face.set_mana_cost(ManaCost::parse(value)),
            "MeldPair" => state.meld_with = Some(value.to_string()),
            _ => {}
        },
        Some(b'N') => if key == "Name" { face.set_name(value) },
        Some(b'O') => if key == "Oracle" { face.set_oracle_text(value) },
        Some(b'P') => if key == "PT" { face.set_pt(value) },
        Some(b'R') => if key == "R" { face.add_replacement(value) },
        Some(b'S') => match key {
            "S" => face.add_static_ability(value),
            "SVar" => { /* split on first : for variable name */ },
            k if k.starts_with("SPECIALIZE") => { /* set face index by color */ },
            _ => {}
        },
        Some(b'T') => match key {
            "T" => face.add_trigger(value),
            "Types" => face.set_type(CardType::parse(value)),
            "Text" => face.set_non_ability_text(value),
            _ => {}
        },
        // C(olors), D(efense, eckHints), F(lavorName), H(andLifeModifier), L(oyalty, ights), V(ariant)
        _ => {} // silently skip unrecognized
    }
}
```

### Pattern 2: Builder Pattern for CardFace
**What:** Mutable builder during parsing, converted to immutable CardFace when complete
**When to use:** Accumulating parsed fields during line-by-line processing
**Example:**
```rust
struct CardFaceBuilder {
    name: Option<String>,
    mana_cost: Option<ManaCost>,
    card_type: Option<CardType>,
    power: Option<String>,
    toughness: Option<String>,
    loyalty: Option<String>,
    defense: Option<String>,
    oracle_text: Option<String>,
    non_ability_text: Option<String>,
    flavor_name: Option<String>,
    keywords: Vec<String>,
    abilities: Vec<String>,        // raw A: strings
    triggers: Vec<String>,         // raw T: strings
    static_abilities: Vec<String>, // raw S: strings
    replacements: Vec<String>,     // raw R: strings
    svars: HashMap<String, String>,
}

impl CardFaceBuilder {
    fn build(self) -> Result<CardFace, ParseError> {
        let name = self.name.ok_or(ParseError::MissingField("Name"))?;
        let mana_cost = self.mana_cost.unwrap_or(ManaCost::NO_COST);
        // ... assign defaults matching Forge's assignMissingFields()
        Ok(CardFace { name, mana_cost, /* ... */ })
    }
}
```

### Pattern 3: AlternateMode → CardLayout Mapping
**What:** Map the `AlternateMode` string to `CardLayout` enum variant
**When to use:** After parsing completes, constructing the final CardRules
**Example:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CardLayout {
    Single(CardFace),
    Split(CardFace, CardFace),
    Flip(CardFace, CardFace),
    Transform(CardFace, CardFace),
    Meld(CardFace, CardFace),
    Adventure(CardFace, CardFace),
    Modal(CardFace, CardFace),
    Omen(CardFace, CardFace),
    Specialize(CardFace, Vec<CardFace>), // base + 5 color variants
}

impl CardLayout {
    fn from_mode_str(s: &str) -> Self {
        // Placeholder — actual variant determined after parsing when faces are available
        // This just records the mode during parsing
        match s {
            "Split" => /* Split */,
            "Flip" => /* Flip */,
            "Transform" | "DoubleFaced" => /* Transform */, // smartValueOf alias
            "Meld" => /* Meld */,
            "Adventure" => /* Adventure */,
            "Modal" => /* Modal */,
            "Omen" => /* Omen */,
            "Specialize" => /* Specialize */,
            _ => /* default Single */,
        }
    }
}
```

### Pattern 4: Ability String Parser
**What:** Parse pipe-delimited `Key$ Value` strings into structured `AbilityDefinition`
**When to use:** Parsing A:, T:, S:, R: line values
**Example:**
```rust
// Input: "SP$ DealDamage | ValidTgts$ Any | NumDmg$ 3 | SpellDescription$ CARDNAME deals 3 damage to any target."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityDefinition {
    pub kind: AbilityKind,
    pub api_type: String,           // "DealDamage"
    pub params: HashMap<String, String>, // {"ValidTgts": "Any", "NumDmg": "3", ...}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AbilityKind {
    Spell,      // SP$
    Activated,  // AB$
    Database,   // DB$
}

impl AbilityDefinition {
    pub fn parse(raw: &str) -> Result<Self, ParseError> {
        let parts: Vec<&str> = raw.split('|').map(str::trim).collect();
        let mut params = HashMap::new();
        let mut kind = None;
        let mut api_type = String::new();

        for part in &parts {
            if let Some((key, value)) = part.split_once('$') {
                let key = key.trim();
                let value = value.trim();
                match key {
                    "SP" | "AB" | "DB" => {
                        kind = Some(match key {
                            "SP" => AbilityKind::Spell,
                            "AB" => AbilityKind::Activated,
                            _ => AbilityKind::Database,
                        });
                        api_type = value.to_string();
                    }
                    _ => { params.insert(key.to_string(), value.to_string()); }
                }
            }
        }

        Ok(AbilityDefinition {
            kind: kind.ok_or(ParseError::MissingAbilityKind)?,
            api_type,
            params,
        })
    }
}
```

### Pattern 5: Trigger/Static/Replacement Parsing
**What:** T:, S:, R: lines use same pipe format but with `Mode$`/`Event$` as discriminator instead of `SP$`/`AB$`/`DB$`
**When to use:** Parsing trigger, static ability, and replacement effect lines
**Example:**
```rust
// Input: "Mode$ ChangesZone | Origin$ Any | Destination$ Battlefield | ValidCard$ Card.Self | Execute$ TrigDiscard | TriggerDescription$ ..."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerDefinition {
    pub mode: String,  // "ChangesZone"
    pub params: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticDefinition {
    pub mode: String,  // "CantBeCast", "Continuous", etc.
    pub params: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplacementDefinition {
    pub event: String,  // "DamageDone"
    pub params: HashMap<String, String>,
}
```

### Anti-Patterns to Avoid
- **Regex-based parsing:** The format is simple key:value with split-on-first-colon. Regex adds complexity and overhead for no benefit.
- **Strict parsing with errors for unknown keys:** Forge silently ignores unknown keys via switch fallthrough. Our parser MUST do the same — new Forge card fields should not crash the parser.
- **Parsing ability semantics in Phase 2:** A: lines are parsed structurally (kind, api_type, params HashMap) but what `DealDamage` actually does is Phase 4.
- **Lazy HashMap from CardType:** Don't try to lazily classify supertypes/core types. Just check against known sets at parse time like Forge does.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Recursive directory traversal | Manual `read_dir` recursive function | `walkdir` crate | Handles symlinks, permissions, `.` prefixed dirs correctly |
| ManaCost shard variants | String matching conditionals | Rust enum with `FromStr` impl | 40+ shard variants (hybrid, phyrexian, colored X) — use Forge's `ManaCostShard` enum as reference |
| Supertype/CoreType classification | String comparison chains | Enum sets with `contains()` | Forge uses `EnumUtils.getEnumMap()` — Rust equivalent is a match or `HashSet<&str>` |
| Case-insensitive name lookup | Manual `.to_lowercase()` on every query | Store lowercased keys at insert time | O(1) lookup without allocation on query path |
| P/T parsing with `*` | Custom arithmetic parser | Forge's `parsePT()` approach: strip `*`, replace with `0`, then `parse::<i32>()` | Handles `*`, `1+*`, `*+1` cleanly |

**Key insight:** The parser is a direct conceptual translation of Forge's Java. Don't innovate on the parsing logic — match Forge's behavior exactly. Innovation belongs in the type representations (discriminated unions instead of nullable arrays).

## Common Pitfalls

### Pitfall 1: ALTERNATE Key Has No Value
**What goes wrong:** The `ALTERNATE` line has no colon, so `split_once(':')` returns `None`.
**Why it happens:** Most lines are `Key:Value` but `ALTERNATE` is a bare keyword.
**How to avoid:** Handle the case where `split_once(':')` returns None — check the full line for `ALTERNATE` and `SPECIALIZE:COLOR` patterns before the main key-value dispatch.
**Warning signs:** Parser ignores the `ALTERNATE` delimiter, second face never gets populated.

### Pitfall 2: SVar Double-Colon Parsing
**What goes wrong:** SVar lines have format `SVar:VarName:Value`. After splitting on first `:` you get key=`SVar`, value=`VarName:Value`. Must split value on first `:` again to get variable name and value.
**Why it happens:** Nested colon semantics.
**How to avoid:** Explicitly handle `SVar` key with a second `split_once(':')` on the value string, matching Forge's approach.
**Warning signs:** SVars stored with names like `TrigDmg:DB$ DealDamage | ...` instead of name=`TrigDmg`, value=`DB$ DealDamage | ...`.

### Pitfall 3: DoubleFaced → Transform Alias
**What goes wrong:** Cards like Nicol Bolas use `AlternateMode:DoubleFaced` but the enum variant is `Transform`.
**Why it happens:** Forge's `smartValueOf()` maps "DoubleFaced" to Transform. This is a historical naming difference.
**How to avoid:** Include "DoubleFaced" as an alias in the `AlternateMode` → `CardLayout` mapping.
**Warning signs:** Transform cards parse as unknown layout type.

### Pitfall 4: ManaCost "no cost" Special Case
**What goes wrong:** Back faces of Transform cards and Meld results have `ManaCost:no cost` — this is NOT zero mana, it means the card literally has no mana cost (can't be cast normally).
**Why it happens:** Forge's `ManaCost.NO_COST` is a special sentinel value distinct from `ManaCost.ZERO`.
**How to avoid:** `ManaCost` enum needs a `NoCost` variant separate from `Cost(Vec<ManaCostShard>, u32)`.
**Warning signs:** Back faces appear to have 0 CMC when they should have no cost.

### Pitfall 5: Omen Layout Type
**What goes wrong:** Forge recently added `Omen` as a `CardSplitType` (Dragon Omen cards). Missing this causes parse failures.
**Why it happens:** Not a well-known MTG mechanic yet. Only 3 cards use it currently.
**How to avoid:** Include `Omen` in the `CardLayout` enum (behaves like Adventure — `USE_PRIMARY_FACE`).
**Warning signs:** Omen cards fail to parse or get wrong layout.

### Pitfall 6: SPECIALIZE Uses Different Delimiters Than ALTERNATE
**What goes wrong:** Specialize cards use `SPECIALIZE:WHITE`, `SPECIALIZE:BLUE`, etc. — not bare `ALTERNATE`.
**Why it happens:** Specialize needs to map to specific color face indices (2-6), not just toggle to face 1.
**How to avoid:** Handle `SPECIALIZE:COLOR` as a separate case that maps color to face index: WHITE=2, BLUE=3, BLACK=4, RED=5, GREEN=6.
**Warning signs:** Only 2 faces parsed for Specialize cards instead of 7.

### Pitfall 7: CardType Multi-Word Subtypes
**What goes wrong:** Some subtypes are multi-word: "Time Lord", "Serra's Realm", "Bolas's Meditation Realm".
**Why it happens:** CardType.parse() splits on spaces, but these subtypes contain spaces.
**How to avoid:** Check for known multi-word types before splitting, matching Forge's `getMultiwordType()`.
**Warning signs:** "Time" and "Lord" parsed as separate subtypes.

### Pitfall 8: P/T with Variable Values
**What goes wrong:** Some cards have P/T like `*/1+*` or `1+*/*`. Simple integer parsing fails.
**Why it happens:** `*` represents a variable value determined by a characteristic-defining ability.
**How to avoid:** Store P/T as strings (like Forge does), with a separate `int_power`/`int_toughness` field that strips `*` and parses the numeric component.
**Warning signs:** Cards with `*` in P/T cause parse panics.

## Code Examples

### ManaCost Parsing (from Forge's ManaCostParser)
```rust
// Source: forge/forge-core/src/main/java/forge/card/mana/ManaCostParser.java
// Format: space-separated tokens, e.g., "2 W U" or "X R R" or "1 W/U B/G/P"
pub fn parse_mana_cost(input: &str) -> ManaCost {
    if input == "no cost" {
        return ManaCost::NoCost;
    }

    let mut shards = Vec::new();
    let mut generic: u32 = 0;

    for token in input.split_whitespace() {
        if let Ok(n) = token.parse::<u32>() {
            generic += n;
        } else {
            shards.push(ManaCostShard::from_str(token));
        }
    }

    ManaCost::Cost { shards, generic }
}
```

### ManaCostShard Enum (from Forge's ManaCostShard.java)
```rust
// Source: forge/forge-core/src/main/java/forge/card/mana/ManaCostShard.java
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ManaCostShard {
    White,        // W
    Blue,         // U
    Black,        // B
    Red,          // R
    Green,        // G
    Colorless,    // C
    Snow,         // S
    X,            // X
    // Hybrid
    WhiteBlue,    // W/U
    WhiteBlack,   // W/B
    BlueBlack,    // U/B
    BlueRed,      // U/R
    BlackRed,     // B/R
    BlackGreen,   // B/G
    RedWhite,     // R/W
    RedGreen,     // R/G
    GreenWhite,   // G/W
    GreenBlue,    // G/U
    // Two-generic hybrid
    TwoWhite,     // 2/W
    TwoBlue,      // 2/U
    TwoBlack,     // 2/B
    TwoRed,       // 2/R
    TwoGreen,     // 2/G
    // Phyrexian
    PhyrexianWhite,  // W/P
    PhyrexianBlue,   // U/P
    PhyrexianBlack,  // B/P
    PhyrexianRed,    // R/P
    PhyrexianGreen,  // G/P
    // Hybrid phyrexian (10 combinations)
    PhyrexianBlackGreen,  // B/G/P
    PhyrexianBlackRed,    // B/R/P
    PhyrexianGreenBlue,   // G/U/P
    PhyrexianGreenWhite,  // G/W/P
    PhyrexianRedGreen,    // R/G/P
    PhyrexianRedWhite,    // R/W/P
    PhyrexianBlueBlack,   // U/B/P
    PhyrexianBlueRed,     // U/R/P
    PhyrexianWhiteBlack,  // W/B/P
    PhyrexianWhiteBlue,   // W/U/P
    // Colorless hybrid
    ColorlessWhite,  // C/W
    ColorlessBlue,   // C/U
    ColorlessBlack,  // C/B
    ColorlessRed,    // C/R
    ColorlessGreen,  // C/G
}
```

### CardType Parsing (from Forge's CardType.java)
```rust
// Source: forge/forge-core/src/main/java/forge/card/CardType.java lines 799-820
// Format: "Legendary Creature Human Wizard" or "Basic Land Forest" or "Instant Adventure"
pub fn parse_card_type(input: &str) -> CardType {
    let mut supertypes = Vec::new();
    let mut core_types = Vec::new();
    let mut subtypes = Vec::new();

    let mut remaining = input;
    while !remaining.is_empty() {
        // Check multi-word types first
        let token = check_multiword_type(remaining)
            .unwrap_or_else(|| {
                remaining.split_whitespace().next().unwrap_or("")
            });

        if Supertype::from_str(token).is_some() {
            supertypes.push(token.to_string());
        } else if CoreType::from_str(token).is_some() {
            core_types.push(token.to_string());
        } else {
            subtypes.push(token.to_string());
        }

        remaining = remaining[token.len()..].trim_start();
    }

    CardType { supertypes, core_types, subtypes }
}
```

### Card File Loading (from Forge's CardStorageReader.java)
```rust
// Source: forge/forge-core/src/main/java/forge/CardStorageReader.java
pub fn load_all_cards(root: &Path) -> Result<CardDatabase, LoadError> {
    let mut cards = HashMap::new();
    let mut face_index = HashMap::new();

    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension() == Some("txt".as_ref()))
        .filter(|e| !e.path().components().any(|c| c.as_os_str() == "."))
    {
        let content = std::fs::read_to_string(entry.path())?;
        let card_rules = parse_card_file(&content)?;
        let key = card_rules.name().to_lowercase();

        // Index each face by name for face-level lookup
        for face_name in card_rules.face_names() {
            face_index.insert(face_name.to_lowercase(), /* face ref */);
        }

        cards.insert(key, card_rules);
    }

    Ok(CardDatabase { cards, face_index })
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Forge's `CardSplitType.None` | Rust's `CardLayout::Single(CardFace)` | This port | Every card has an explicit layout, no null checks |
| Forge's nullable `ICardFace otherPart` | Rust discriminated union in `CardLayout` | This port | Compile-time guarantee: can't access second face of a Single card |
| Forge's `ICardFace[]` array with null slots | Typed enum variants with exact face count | This port | Specialize is `(CardFace, Vec<CardFace>)` not 7-element nullable array |
| Forge's recent `Omen` CardSplitType | Include in `CardLayout` enum | 2025-2026 | New layout type for Dragon Omen mechanic |

**Deprecated/outdated:**
- Forge's `CardRulesPredicates.java` — filtering predicates. Not needed for Phase 2 (database just stores and retrieves by name).
- Forge's `CardEdition`/`PaperCard` layer — card editions, art variants. Deferred per CONTEXT.md.

## Open Questions

1. **Should `CardRules` wrap `CardLayout` or be the same struct?**
   - What we know: Forge has `CardRules` with `mainPart`, `otherPart`, `splitType` as separate fields, plus metadata like `meldWith`, `partnerWith`, `colorIdentity`.
   - What's unclear: Whether the Rust type should be `CardRules { layout: CardLayout, meld_with: Option<String>, ... }` or if `CardLayout` IS the top-level type.
   - Recommendation: Use `CardRules { layout: CardLayout, meld_with: Option<String>, partner_with: Option<String> }` — separates the face structure from card-level metadata. This aligns with Forge's architecture.

2. **Handling `Colors:` explicit color override**
   - What we know: Back faces of Transform cards sometimes specify `Colors:blue,black,red` when their mana cost is `no cost`. Forge's `assignMissingFields()` derives color from ManaCost if not explicit.
   - Recommendation: Store `color_override: Option<Vec<ManaColor>>` on CardFace. Derive color from ManaCost during `build()` unless override is set.

3. **Error handling strategy for malformed card files**
   - What we know: Decision says lenient parsing. Forge silently ignores unknown keys.
   - Recommendation: Use `Result<CardRules, ParseError>` for the file-level parser but log warnings (not errors) for unexpected fields. Only error on missing required fields (Name). Use `tracing::warn!` for diagnostics.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in test framework (`#[cfg(test)]` / `#[test]`) |
| Config file | None needed — Cargo handles test discovery |
| Quick run command | `cargo test -p engine --lib` |
| Full suite command | `cargo test -p engine` |

### Phase Requirements → Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| PARSE-01 | Parse Lightning Bolt (simple card) into CardFace with name, mana_cost, types, oracle_text, ability | unit | `cargo test -p engine parser::card_parser::tests::parse_lightning_bolt -x` | Wave 0 |
| PARSE-01 | Parse all key types (Name, ManaCost, Types, PT, K, A, T, S, R, SVar, Oracle) | unit | `cargo test -p engine parser::card_parser::tests::parse_all_keys -x` | Wave 0 |
| PARSE-01 | Skip comment lines and empty lines | unit | `cargo test -p engine parser::card_parser::tests::skip_comments -x` | Wave 0 |
| PARSE-01 | Silently skip unknown keys | unit | `cargo test -p engine parser::card_parser::tests::skip_unknown_keys -x` | Wave 0 |
| PARSE-02 | Parse Adventure card (Bonecrusher Giant) into two-face Adventure layout | unit | `cargo test -p engine parser::card_parser::tests::parse_adventure -x` | Wave 0 |
| PARSE-02 | Parse Transform card (Nicol Bolas) with DoubleFaced alias | unit | `cargo test -p engine parser::card_parser::tests::parse_transform -x` | Wave 0 |
| PARSE-02 | Parse Split card (Fire // Ice) | unit | `cargo test -p engine parser::card_parser::tests::parse_split -x` | Wave 0 |
| PARSE-02 | Parse Meld card (Gisela/Brisela) with MeldPair | unit | `cargo test -p engine parser::card_parser::tests::parse_meld -x` | Wave 0 |
| PARSE-02 | Parse Flip card (Akki Lavarunner) | unit | `cargo test -p engine parser::card_parser::tests::parse_flip -x` | Wave 0 |
| PARSE-02 | Parse MDFC (Valki // Tibalt) with Modal layout | unit | `cargo test -p engine parser::card_parser::tests::parse_mdfc -x` | Wave 0 |
| PARSE-03 | CardDatabase loads directory and indexes by lowercased name | integration | `cargo test -p engine database::tests::load_and_lookup -x` | Wave 0 |
| PARSE-03 | Face-level index finds individual faces (e.g., "stomp" → Bonecrusher Giant) | integration | `cargo test -p engine database::tests::face_lookup -x` | Wave 0 |
| PARSE-03 | Lookup is case-insensitive | unit | `cargo test -p engine database::tests::case_insensitive -x` | Wave 0 |
| PARSE-04 | ManaCost parses colored, generic, hybrid, phyrexian, X, snow, "no cost" | unit | `cargo test -p engine parser::mana_cost::tests -x` | Wave 0 |
| PARSE-04 | CardType parses supertypes, core types, subtypes correctly | unit | `cargo test -p engine parser::card_type::tests -x` | Wave 0 |
| ABIL-01 | Parse SP$ ability string into AbilityDefinition with Spell kind | unit | `cargo test -p engine parser::ability::tests::parse_spell -x` | Wave 0 |
| ABIL-01 | Parse AB$ ability string into AbilityDefinition with Activated kind | unit | `cargo test -p engine parser::ability::tests::parse_activated -x` | Wave 0 |
| ABIL-01 | Parse T: trigger line with Mode$ discriminator | unit | `cargo test -p engine parser::ability::tests::parse_trigger -x` | Wave 0 |
| ABIL-01 | Parse R: replacement line with Event$ discriminator | unit | `cargo test -p engine parser::ability::tests::parse_replacement -x` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test -p engine --lib`
- **Per wave merge:** `cargo test -p engine`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `crates/engine/src/parser/` — entire module is new
- [ ] `crates/engine/src/database/` — entire module is new
- [ ] `crates/engine/src/types/card_type.rs` — new file for CardType
- [ ] `crates/engine/src/types/ability.rs` — new file for AbilityDefinition
- [ ] Test fixtures: embed sample card file contents as `const` strings in test modules (Lightning Bolt, Bonecrusher Giant, Nicol Bolas, Fire // Ice, Gisela, Akki Lavarunner, Valki)
- [ ] `walkdir` and `thiserror` added to `Cargo.toml`

## Sources

### Primary (HIGH confidence)
- Forge source code analyzed directly:
  - `forge/forge-core/src/main/java/forge/card/CardRules.java` — full `Reader.parseLine()` (lines 639-815), `getCard()` builder, `fromScript()` entry point
  - `forge/forge-core/src/main/java/forge/card/CardFace.java` — all fields, `assignMissingFields()` defaults, `parsePT()` logic
  - `forge/forge-core/src/main/java/forge/card/CardSplitType.java` — all 9 variants including `Omen`, `smartValueOf()` alias
  - `forge/forge-core/src/main/java/forge/card/CardType.java` — `parse()` method, `Supertype` and `CoreType` enums, multi-word type handling
  - `forge/forge-core/src/main/java/forge/card/mana/ManaCostParser.java` — space-split token parser
  - `forge/forge-core/src/main/java/forge/card/mana/ManaCostShard.java` — all 40+ shard variants with string representations
  - `forge/forge-core/src/main/java/forge/CardStorageReader.java` — directory traversal, file loading, threaded batch processing
  - `forge/forge-core/src/main/java/forge/card/CardDb.java` — case-insensitive name index, face-level index
- Card file examples examined directly:
  - `lightning_bolt.txt` — simple creature/instant
  - `bonecrusher_giant_stomp.txt` — Adventure
  - `nicol_bolas_the_ravager_nicol_bolas_the_arisen.txt` — Transform (DoubleFaced)
  - `fire_ice.txt` — Split
  - `gisela_the_broken_blade_brisela_voice_of_nightmares.txt` — Meld
  - `akki_lavarunner_tok_tok_volcano_born.txt` — Flip
  - `valki_god_of_lies_tibalt_cosmic_impostor.txt` — Modal (MDFC)
  - `imoen_trickster_friend.txt` — Specialize (7 faces)
- Existing project code: `crates/engine/src/types/` — established patterns, workspace deps

### Secondary (MEDIUM confidence)
- None needed — all findings come from direct source analysis

### Tertiary (LOW confidence)
- None

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH — using existing workspace dependencies plus well-known Rust ecosystem crates
- Architecture: HIGH — direct translation of analyzed Forge source code, with Rust-idiomatic improvements
- Pitfalls: HIGH — all pitfalls identified from actual Forge source code analysis and real card file examples
- Ability parsing: HIGH — format confirmed across multiple card files

**Research date:** 2026-03-07
**Valid until:** Indefinite — Forge's card format is 15+ years stable; changes are additive (new keys), not breaking
