# Phase 24: Card Migration - Research

**Researched:** 2026-03-10
**Domain:** Rust binary tooling, serde serialization, card data migration, CI pipeline
**Confidence:** HIGH

## Summary

Phase 24 builds an automated migration tool that converts 32,300 Forge `.txt` card files into the MTGJSON metadata + ability JSON format established in Phases 21-23. The core insight is that this is primarily a **serialization adapter** -- the existing Forge parser pipeline (`parse_card_file()` -> `CardFace` with typed `AbilityDefinition`, `TriggerDefinition`, `StaticDefinition`, `ReplacementDefinition`) already produces the exact Rust types that the `AbilityFile` schema expects. The migration tool's job is to: (1) parse each Forge `.txt` file, (2) extract the ability/trigger/static/replacement definitions, (3) serialize them to `AbilityFile` JSON via serde, and (4) write one JSON file per card to `data/abilities/`.

The major technical gap is **cost parsing**: the current `parse_ability()` function hardcodes `Cost$ ...` to `AbilityCost::Tap` regardless of input. The migration tool needs to parse Forge cost strings like `Cost$ 3 W` (mana), `Cost$ T` (tap), `Cost$ AddCounter<2/LOYALTY>` (planeswalker loyalty), and composite costs like `Cost$ R Discard<0/Hand> Sac<1/CARDNAME>`. This cost parser must produce properly typed `AbilityCost` variants (`Mana`, `Tap`, `Loyalty`, `Sacrifice`, `Composite`).

The parity testing strategy compares the 78 Standard cards loaded via both paths (Forge `.txt` and JSON) by comparing `CardFace` fields structurally. The CI gate adds a JSON coverage check alongside the existing Forge coverage gate.

**Primary recommendation:** Build the migration tool as a standalone binary that reuses `parse_card_file()` + `FaceAbilities` serialization, with the main new work being a proper `Cost$` string parser and a card-name-to-filename normalization function for output files.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Migration tool is a standalone Rust binary at `crates/engine/src/bin/migrate.rs`, invoked via `cargo run --bin migrate`
- Processes all 32,300 Forge card files in `data/cardsfolder/` (not just the 78 Standard cards)
- Reuses existing Forge parser pipeline: `parse_ability()` -> `AbilityDefinition` (with typed `Effect` enum via `params_to_effect()`) -> serialize to JSON via serde
- Overwrites the 8 existing hand-authored ability JSON files from Phase 23 -- ensures consistency across all cards
- Outputs all generated files to `data/abilities/`
- Uses `Effect::Other { api_type, params }` for effects/triggers the parser can't map to typed enum variants
- Generates ability JSON for ALL 32,300 cards, including those with unsupported mechanics
- Validates generated output against MTGJSON oracle text as a heuristic smoke check (advisory, not blocking)
- Summary + detailed log: print summary stats to stdout, write per-card detailed report to `migration-report.json`
- Structural comparison for parity: load each of the 78 Standard cards via both paths, compare `CardFace`/`CardRules` fields
- All 78 Standard cards tested for parity -- these are the CI-gated cards
- Parity tests live in `crates/engine/tests/parity.rs`, run via `cargo test --test parity`
- Add JSON coverage gate alongside existing Forge coverage gate -- both must pass during Phase 24
- Phase 25 removes the Forge gate; JSON gate becomes the sole gate
- Introduce explicit Standard card manifest file (e.g., `data/standard-cards.txt`) listing the 78 card names
- All 32,300 generated ability JSON files committed to repo under `data/abilities/` (~16MB total)

### Claude's Discretion
- Which card list the JSON coverage gate uses (same 78-card list is the natural choice)
- Internal migration tool architecture (batch processing order, parallelism, error handling)
- Exact oracle text validation heuristics (which keywords to check, matching strategy)
- Manifest file format (plain text vs JSON)
- Whether to organize `data/abilities/` with subdirectories (a/, b/, c/) or keep flat

### Deferred Ideas (OUT OF SCOPE)
None -- discussion stayed within phase scope
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| MIGR-01 | All engine-supported cards converted from Forge .txt to MTGJSON metadata + ability JSON via automated migration | Migration tool binary at `bin/migrate.rs` reuses existing parser pipeline; `AbilityFile` serialization via serde handles conversion |
| MIGR-03 | Automated Forge-to-JSON migration tool converts all 32,300+ Forge .txt card definitions to the new ability JSON format | Tool processes `data/cardsfolder/` recursively, parses each `.txt` file, serializes to `AbilityFile` JSON in `data/abilities/` |
| MIGR-05 | CI coverage gate updated to validate against JSON card data; all previously supported cards remain supported | Dual coverage gate in `ci.yml` and `coverage_report.rs` -- add `--json` mode loading via `CardDatabase::load_json()` |
| TEST-04 | Per-card behavioral parity tests confirm migrated cards produce identical game outcomes | Integration test at `tests/parity.rs` comparing `CardFace` fields from Forge vs JSON loading paths for all 78 Standard cards |
</phase_requirements>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| serde + serde_json | 1.x | Serialize `AbilityFile` structs to JSON | Already used throughout codebase; `AbilityFile`, `Effect`, `TriggerDefinition` all derive `Serialize`/`Deserialize` |
| walkdir | 2.x | Recursive directory traversal of `data/cardsfolder/` | Already a dependency in engine crate; used by `CardDatabase::load()` |
| schemars | 1.x | JSON Schema generation for ability files | Already used; schema.json regenerated by test |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| tempfile | 3.x (dev) | Temporary directories for parity tests | Already in dev-dependencies; used by existing tests |

### No New Dependencies Required
The migration tool reuses only existing engine crate dependencies. No new crates needed.

## Architecture Patterns

### Recommended Project Structure
```
crates/engine/src/
├── bin/
│   ├── migrate.rs           # NEW: Migration tool binary
│   ├── coverage_report.rs   # MODIFIED: Add --json mode
│   └── card_data_export.rs  # Existing (untouched)
├── parser/
│   ├── ability.rs           # MODIFIED: Enhanced cost parsing
│   └── card_parser.rs       # Existing (reused by migration tool)
├── database/
│   ├── json_loader.rs       # Existing (used by parity tests)
│   └── card_db.rs           # Existing (both load paths)
└── schema/
    └── mod.rs               # Existing (AbilityFile struct)

crates/engine/tests/
├── parity.rs                # NEW: Forge vs JSON parity tests
├── json_smoke_test.rs       # Existing (Phase 23)
└── rules/                   # Existing (Phase 22)

data/
├── abilities/               # OUTPUT: ~32,300 JSON files (overwrites existing 8)
├── standard-cards.txt       # NEW: Manifest of 78 Standard card names
├── standard-cards/          # Existing: 78 .txt files (Forge format)
├── cardsfolder/             # INPUT: 32,300 .txt files (gitignored, not committed)
└── mtgjson/
    └── test_fixture.json    # Existing
```

### Pattern 1: Migration Tool as Serialization Adapter
**What:** Parse Forge `.txt` -> extract typed Rust structs -> serialize to `AbilityFile` JSON
**When to use:** For each of the 32,300 card files

The migration tool's core loop:
```rust
// Source: Existing crate APIs
use engine::parser::parse_card_file;
use engine::schema::{AbilityFile, FaceAbilities};

fn migrate_card(content: &str) -> Result<AbilityFile, MigrationError> {
    let card_rules = parse_card_file(content)?;
    match &card_rules.layout {
        CardLayout::Single(face) => Ok(AbilityFile {
            schema: Some("schema.json".to_string()),
            abilities: face.abilities.clone(),
            triggers: face.triggers.clone(),
            statics: face.static_abilities.clone(),
            replacements: face.replacements.clone(),
            faces: vec![],
        }),
        // Multi-face: populate `faces` array
        CardLayout::Transform(a, b) | CardLayout::Adventure(a, b) | ... => {
            Ok(AbilityFile {
                schema: Some("schema.json".to_string()),
                abilities: vec![],
                triggers: vec![],
                statics: vec![],
                replacements: vec![],
                faces: vec![
                    face_to_abilities(a),
                    face_to_abilities(b),
                ],
            })
        }
    }
}
```

### Pattern 2: Cost String Parser Enhancement
**What:** Enhance `parse_ability()` to properly parse Forge `Cost$` strings into typed `AbilityCost` variants
**When to use:** Critical for migration correctness -- all activated abilities need proper costs

Current state (broken):
```rust
// Line 465-468 in parser/ability.rs
let cost = params.remove("Cost").map(|_cost_str| {
    // Cost parsing is basic for now -- Plan 02 will enrich this
    crate::types::ability::AbilityCost::Tap
});
```

Required enhancement:
```rust
fn parse_cost(cost_str: &str) -> AbilityCost {
    let cost_str = cost_str.trim();

    // Simple cases
    if cost_str == "T" { return AbilityCost::Tap; }

    // Planeswalker loyalty: "AddCounter<N/LOYALTY>" or "SubCounter<N/LOYALTY>"
    if let Some(captures) = parse_loyalty_cost(cost_str) {
        return AbilityCost::Loyalty { amount: captures };
    }

    // Composite: "R Discard<0/Hand> Sac<1/CARDNAME>"
    // Split on spaces, parse each component, compose if multiple
    let parts = split_cost_components(cost_str);
    if parts.len() == 1 {
        parse_single_cost(&parts[0])
    } else {
        AbilityCost::Composite { costs: parts.iter().map(|p| parse_single_cost(p)).collect() }
    }
}
```

### Pattern 3: Card Name to Filename Normalization
**What:** Convert card names to snake_case filenames for ability JSON output
**When to use:** Migration tool output naming

```rust
fn card_name_to_filename(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() { c.to_lowercase().next().unwrap() } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}
// "Lightning Bolt" -> "lightning_bolt"
// "Jace, the Mind Sculptor" -> "jace_the_mind_sculptor"
```

### Pattern 4: Dual Coverage Gate
**What:** Run coverage analysis on both Forge-loaded and JSON-loaded card databases
**When to use:** CI pipeline during Phase 24 (both must pass)

The coverage report binary (`coverage_report.rs`) needs a `--json` flag:
```rust
// Existing: cargo run --bin coverage-report -- data/standard-cards/ --ci
// New:      cargo run --bin coverage-report -- --json --ci
// Where --json loads via CardDatabase::load_json() instead of CardDatabase::load()
```

### Anti-Patterns to Avoid
- **Don't re-parse Forge format in the migration tool**: Reuse `parse_card_file()` which already produces typed structs. The migration tool should never touch raw `SP$` / `AB$` strings directly.
- **Don't filter cards during migration**: Generate JSON for ALL 32,300 cards. `coverage.rs` handles the "supported vs unsupported" distinction. The migration tool just converts format.
- **Don't try to match MTGJSON during migration**: The migration tool only reads Forge `.txt` files and outputs ability JSON. The MTGJSON merge happens at load time in `json_loader.rs`. Migration output does NOT need MTGJSON data.
- **Don't hand-roll JSON serialization**: Use `serde_json::to_string_pretty()` on `AbilityFile` structs. The types already have correct serde derives.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Card file parsing | Custom Forge parser | `parse_card_file()` from `parser/card_parser.rs` | Already handles all card layouts, multi-face, SVars, keywords |
| Ability serialization | Manual JSON construction | `serde_json::to_string_pretty(&AbilityFile { .. })` | `AbilityFile` + all nested types already derive `Serialize` |
| Directory traversal | Manual `fs::read_dir` recursion | `walkdir::WalkDir` (already a dependency) | Handles nested `a/`, `b/`, etc. subdirectories correctly |
| Coverage analysis | New coverage checker | Existing `analyze_standard_coverage()` in `coverage.rs` | Already checks abilities, triggers, keywords, statics against registries |
| Card name normalization | New normalizer | Existing `normalize_for_match()` in `json_loader.rs` for comparison | Already handles punctuation stripping for name matching |

## Common Pitfalls

### Pitfall 1: Cost Parsing Is Currently Broken
**What goes wrong:** The current `parse_ability()` maps ALL `Cost$` values to `AbilityCost::Tap`, regardless of input. This means every migrated activated ability will have `cost: { "type": "Tap" }` in JSON, which is wrong for mana-cost abilities, loyalty abilities, sacrifice costs, etc.
**Why it happens:** The cost parser was left as a stub with a TODO comment: "Cost parsing is basic for now -- Plan 02 will enrich this."
**How to avoid:** Enhance `parse_cost()` before running migration. Must handle at minimum: `T` (tap), mana costs (`3 W`, `R`, `{1}{W}`), `AddCounter<N/LOYALTY>` / `SubCounter<N/LOYALTY>` (loyalty), `Sac<...>` (sacrifice), and composite costs.
**Warning signs:** Parity tests will fail for any card with non-tap costs. Jace's loyalty abilities are a canary.

### Pitfall 2: SVars Are NOT Part of AbilityFile Schema
**What goes wrong:** Many Forge cards reference sub-abilities via `SVar:Name:DB$ ...` and `Execute$ Name` / `SubAbility$ Name`. The current `AbilityFile` schema has no `svars` field. The `TriggerDefinition` has `params` which includes `Execute$ TrigName`, but the SVar resolution happens at runtime in the engine, not at load time.
**Why it happens:** The hand-authored JSON files in Phase 23 manually inlined the SVar content into trigger `params` (e.g., Rancor's JSON has the ChangeZone effect directly in the trigger, not referencing an SVar). The Forge parser stores SVars on `CardFace.svars` but `AbilityFile` doesn't serialize them.
**How to avoid:** Two options: (A) Add `svars` field to `AbilityFile` schema to preserve them, or (B) keep SVars in trigger/ability params (the `Execute$` key) and let the engine resolve them at load time as it does now. Option B is simpler since the existing engine already resolves SVars. The migration tool should include `Execute$` and `SubAbility$` keys in `remaining_params` which end up in JSON.
**Warning signs:** Cards with complex trigger chains (Banishing Light, Brainstorm, Blade Splicer) may lose sub-ability references.

### Pitfall 3: Multi-Face Card Output Filenames
**What goes wrong:** Multi-face Forge card files are named like `delver_of_secrets_insectile_aberration.txt` but the output JSON should be named by the primary face: `delver_of_secrets.json`. Using the source filename would create mismatches with `json_loader.rs` which converts `delver_of_secrets.json` -> "Delver Of Secrets" for MTGJSON lookup.
**Why it happens:** The source filename includes both face names but `json_loader.rs` expects only the primary face name.
**How to avoid:** Use the `Name:` field from the first face of the parsed card (not the source filename) to generate the output filename. Apply `card_name_to_filename()` to the parsed name.

### Pitfall 4: Hand-Authored JSON Files Include Extra Fields
**What goes wrong:** The 8 hand-authored ability JSON files from Phase 23 include fields not in the Rust type definitions. For example, Rancor's trigger has an `effect` field, but `TriggerDefinition` only has `mode` and `params`. These extra fields are silently ignored by serde deserialization (no `deny_unknown_fields`).
**Why it happens:** The hand-authored files were written for human clarity, including fields that the Rust types don't model.
**How to avoid:** Since the migration tool overwrites all 8 files, the auto-generated versions will only contain fields from the Rust types. This is correct behavior -- the extra fields were never consumed by the engine. But verify that parity tests still pass after overwriting.

### Pitfall 5: Missing Full AtomicCards.json for Migration
**What goes wrong:** Only `data/mtgjson/test_fixture.json` (12 cards) is committed. The migration tool doesn't need MTGJSON data (it only reads Forge `.txt` files), but the **parity tests** and **JSON coverage gate** need MTGJSON to load cards via `CardDatabase::load_json()`. The test fixture only has 12 cards, not 78.
**Why it happens:** The full AtomicCards.json (~50MB) is not committed to the repo.
**How to avoid:** Either (A) extend `test_fixture.json` to include all 78 Standard cards, or (B) download AtomicCards.json at test time, or (C) use a subset fixture for parity tests that covers all 78 Standard cards. Option A is most practical -- the test fixture is ~11KB for 12 cards, so 78 cards would be ~70KB, still tiny.

### Pitfall 6: Flat vs Subdirectory Organization for 32,300 Files
**What goes wrong:** Writing 32,300 JSON files flat in `data/abilities/` makes directory listing slow and git operations sluggish.
**Why it happens:** The existing `json_loader.rs` only reads files directly in `abilities_dir` (one level, no recursion).
**How to avoid:** Keep flat for Phase 24 (matches existing loader behavior). If performance is an issue, subdirectories can be added in a future phase with loader updates. 32,300 files in one directory is manageable for modern filesystems and git handles it fine (git uses tree objects, not filesystem listings).

**Recommendation for discretionary items:**
- **Manifest format:** Plain text (`data/standard-cards.txt`), one card name per line. Simplest format, easiest to maintain.
- **Directory organization:** Flat `data/abilities/` -- matches existing `json_loader.rs` with zero changes. The loader reads one directory level.
- **JSON coverage gate card list:** Use the same 78-card manifest. Both gates read from `data/standard-cards.txt`.
- **Oracle text validation:** Keyword-presence heuristic -- check that keywords from oracle text (Flying, Trample, Lifelink, etc.) appear in the parsed card's keywords list. Log warnings for mismatches but don't block.

## Code Examples

### Migration Tool Core Loop
```rust
// Source: Existing crate types and parser
fn main() {
    let forge_dir = PathBuf::from("data/cardsfolder");
    let output_dir = PathBuf::from("data/abilities");

    let mut stats = MigrationStats::default();

    for entry in WalkDir::new(&forge_dir).into_iter().filter_map(|e| e.ok()) {
        if !entry.path().is_file() || entry.path().extension() != Some("txt".as_ref()) {
            continue;
        }

        let content = fs::read_to_string(entry.path())?;
        match parse_card_file(&content) {
            Ok(card_rules) => {
                let ability_file = card_rules_to_ability_file(&card_rules);
                let filename = card_name_to_filename(card_rules.name());
                let json = serde_json::to_string_pretty(&ability_file)?;
                fs::write(output_dir.join(format!("{filename}.json")), format!("{json}\n"))?;
                stats.converted += 1;
            }
            Err(e) => {
                stats.errors.push((entry.path().to_path_buf(), e.to_string()));
            }
        }
    }

    // Print summary to stdout
    println!("Converted: {}/{}", stats.converted, stats.total);
}
```

### CardRules to AbilityFile Conversion
```rust
// Source: Existing schema/mod.rs types
fn card_rules_to_ability_file(rules: &CardRules) -> AbilityFile {
    match &rules.layout {
        CardLayout::Single(face) => AbilityFile {
            schema: Some("schema.json".to_string()),
            abilities: face.abilities.clone(),
            triggers: face.triggers.clone(),
            statics: face.static_abilities.clone(),
            replacements: face.replacements.clone(),
            faces: vec![],
        },
        // All multi-face variants
        CardLayout::Transform(a, b) | CardLayout::Adventure(a, b)
        | CardLayout::Split(a, b) | CardLayout::Flip(a, b)
        | CardLayout::Meld(a, b) | CardLayout::Modal(a, b)
        | CardLayout::Omen(a, b) => AbilityFile {
            schema: Some("schema.json".to_string()),
            abilities: vec![],
            triggers: vec![],
            statics: vec![],
            replacements: vec![],
            faces: vec![face_to_abilities(a), face_to_abilities(b)],
        },
        CardLayout::Specialize(base, variants) => {
            let mut faces = vec![face_to_abilities(base)];
            faces.extend(variants.iter().map(face_to_abilities));
            AbilityFile {
                schema: Some("schema.json".to_string()),
                abilities: vec![],
                triggers: vec![],
                statics: vec![],
                replacements: vec![],
                faces,
            }
        }
    }
}

fn face_to_abilities(face: &CardFace) -> FaceAbilities {
    FaceAbilities {
        abilities: face.abilities.clone(),
        triggers: face.triggers.clone(),
        statics: face.static_abilities.clone(),
        replacements: face.replacements.clone(),
    }
}
```

### Cost Parser Enhancement
```rust
// Source: Forge card format documentation + existing AbilityCost variants
fn parse_cost(cost_str: &str) -> Option<AbilityCost> {
    let cost_str = cost_str.trim();
    if cost_str.is_empty() { return None; }

    // Tap only
    if cost_str == "T" { return Some(AbilityCost::Tap); }

    // Loyalty: AddCounter<N/LOYALTY> = +N, SubCounter<N/LOYALTY> = -N
    if let Some(amount) = parse_loyalty(cost_str) {
        return Some(AbilityCost::Loyalty { amount });
    }

    // Split into components (space-separated, respecting angle brackets)
    let components = split_cost_components(cost_str);
    let mut costs: Vec<AbilityCost> = Vec::new();
    let mut mana_parts: Vec<String> = Vec::new();

    for comp in &components {
        if comp == "T" {
            costs.push(AbilityCost::Tap);
        } else if let Some(amount) = parse_loyalty(comp) {
            costs.push(AbilityCost::Loyalty { amount });
        } else if comp.starts_with("Sac<") {
            let target = parse_angle_bracket_target(comp, "Sac");
            costs.push(AbilityCost::Sacrifice { target });
        } else if comp.starts_with("Discard<") {
            // Discard costs -- handle as a cost component
            costs.push(AbilityCost::Tap); // placeholder: extend later
        } else {
            // Assume mana: "3", "W", "R", etc.
            mana_parts.push(comp.clone());
        }
    }

    if !mana_parts.is_empty() {
        costs.insert(0, AbilityCost::Mana { cost: mana_parts.join(" ") });
    }

    match costs.len() {
        0 => None,
        1 => Some(costs.into_iter().next().unwrap()),
        _ => Some(AbilityCost::Composite { costs }),
    }
}

fn parse_loyalty(s: &str) -> Option<i32> {
    // AddCounter<2/LOYALTY> -> +2
    if let Some(rest) = s.strip_prefix("AddCounter<") {
        if let Some(num_str) = rest.strip_suffix("/LOYALTY>") {
            return num_str.parse::<i32>().ok();
        }
    }
    // SubCounter<1/LOYALTY> -> -1
    if let Some(rest) = s.strip_prefix("SubCounter<") {
        if let Some(num_str) = rest.strip_suffix("/LOYALTY>") {
            return num_str.parse::<i32>().ok().map(|n| -n);
        }
    }
    None
}
```

### Parity Test Structure
```rust
// Source: Existing CardDatabase load paths
use engine::database::CardDatabase;
use std::path::Path;

fn data_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data")
}

#[test]
fn parity_all_standard_cards() {
    let data = data_dir();
    let manifest = std::fs::read_to_string(data.join("standard-cards.txt")).unwrap();
    let card_names: Vec<&str> = manifest.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();

    // Load via Forge path
    let forge_db = CardDatabase::load(&data.join("standard-cards")).unwrap();

    // Load via JSON path
    let json_db = CardDatabase::load_json(
        &data.join("mtgjson/test_fixture.json"),
        &data.join("abilities"),
    ).unwrap();

    for name in &card_names {
        let forge_card = forge_db.get_by_name(name)
            .unwrap_or_else(|| panic!("Forge DB missing: {name}"));
        let json_card = json_db.get_by_name(name)
            .unwrap_or_else(|| panic!("JSON DB missing: {name}"));

        // Compare face fields structurally
        compare_card_rules(name, forge_card, json_card);
    }
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Raw Forge `.txt` as sole card format | MTGJSON metadata + ability JSON | Phase 21-23 | JSON format now proven with 8 cards; migration scales to all 32,300 |
| `AbilityCost::Tap` hardcoded | Typed `AbilityCost` enum with Mana, Tap, Loyalty, Sacrifice, Composite | Phase 21 (types), Phase 24 (parser) | Types exist but parser doesn't populate them yet |
| Manual card authoring | Automated migration from Forge | Phase 24 (this phase) | One-time batch conversion replaces manual effort |
| Single coverage gate (Forge) | Dual coverage gate (Forge + JSON) | Phase 24 | Safety net before Phase 25 removes Forge gate |

## Open Questions

1. **MTGJSON Test Fixture Expansion**
   - What we know: Current fixture has 12 cards. Parity tests need all 78 Standard cards in the fixture.
   - What's unclear: Whether to expand the fixture or use the full AtomicCards.json for parity tests.
   - Recommendation: Expand `test_fixture.json` to include all 78 Standard cards (~70KB). Keep it as a committed fixture file rather than depending on a large download. The migration tool doesn't need MTGJSON at all.

2. **SVar Preservation Strategy**
   - What we know: `CardFace.svars` contains SVar references (e.g., `TrigToken: "DB$ Token | TokenScript$ ..."`) that are used by the engine at runtime. `AbilityFile` has no `svars` field.
   - What's unclear: Whether SVars need to be in the ability JSON or if the engine resolves them from `CardFace.svars` which is populated separately.
   - Recommendation: The SVar issue needs investigation. The current JSON loading path (`json_loader.rs`) does NOT populate `CardFace.svars` from ability JSON -- it's always empty for JSON-loaded cards. For Phase 24 parity, SVars stored in ability params (`Execute$`, `SubAbility$` keys in `remaining_params`) may be sufficient. The `Effect::Other` fallback already preserves params. For cards where SVars contain sub-ability chains (Banishing Light, Brainstorm), the parity test should compare at the ability-definition level, not full SVar resolution.

3. **Cost Parser Scope**
   - What we know: Forge cost syntax is complex. Common patterns: `T`, `3 W`, `AddCounter<2/LOYALTY>`, `SubCounter<1/LOYALTY>`, `R Sac<1/CARDNAME>`, `R Discard<0/Hand>`, `exert`, `paylife<2>`, `Untap`.
   - What's unclear: Full exhaustive set of cost formats across 32,300 cards.
   - Recommendation: Parse the most common patterns (mana, tap, loyalty, sacrifice) correctly. For unrecognized cost components, fall back to `AbilityCost::Mana { cost: raw_string }` to preserve the data without losing information. This matches the `Effect::Other` fallback pattern.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in test + cargo integration tests |
| Config file | `Cargo.toml` [[test]] entries (auto-detected by cargo) |
| Quick run command | `cargo test --test parity` |
| Full suite command | `cargo test --all` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| MIGR-01 | All engine-supported cards converted to JSON | integration | `cargo test --test parity -- parity_all_standard_cards` | Wave 0 |
| MIGR-03 | Migration tool processes all 32,300 cards | integration (manual run) | `cargo run --bin migrate -- data/cardsfolder data/abilities` | Wave 0 |
| MIGR-05 | CI JSON coverage gate passes | integration | `cargo run --bin coverage-report -- --json data/abilities --ci` | Wave 0 |
| TEST-04 | Per-card parity tests (Forge vs JSON) | integration | `cargo test --test parity` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test -p engine && cargo test --test parity`
- **Per wave merge:** `cargo test --all`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `crates/engine/tests/parity.rs` -- covers MIGR-01, TEST-04
- [ ] `crates/engine/src/bin/migrate.rs` -- covers MIGR-03
- [ ] `data/standard-cards.txt` -- manifest for both coverage gates
- [ ] Extended `data/mtgjson/test_fixture.json` -- 78 Standard cards for JSON load path in parity tests

## Sources

### Primary (HIGH confidence)
- **Codebase inspection** -- `parser/ability.rs` (cost parsing stub at line 465-468), `schema/mod.rs` (`AbilityFile` structure), `database/json_loader.rs` (JSON loading pipeline), `database/card_db.rs` (both load paths), `game/coverage.rs` (coverage analysis functions), `bin/coverage_report.rs` (CI binary)
- **Existing ability JSON files** -- `data/abilities/` (8 hand-authored files showing target format)
- **Phase 23 SUMMARY** -- Confirmed JSON loading pipeline proven end-to-end with 10 integration tests
- **CI configuration** -- `.github/workflows/ci.yml` (current coverage gate: `cargo run --bin coverage-report -- data/standard-cards/ --ci`)

### Secondary (MEDIUM confidence)
- **Forge card format** -- Inspected real card files in `data/cardsfolder/` and `data/standard-cards/` to understand format complexity
- **MTGJSON data structure** -- Inspected `test_fixture.json` and `mtgjson.rs` types

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- All dependencies already in crate, no new libraries needed
- Architecture: HIGH -- Direct serialization of existing typed Rust structs to JSON; pattern proven by Phase 23
- Pitfalls: HIGH -- Identified from direct code inspection; cost parsing stub confirmed in source
- Parity testing: MEDIUM -- SVar preservation strategy needs validation during implementation
- Cost parser completeness: MEDIUM -- Common patterns identified but full Forge cost syntax may have edge cases

**Research date:** 2026-03-10
**Valid until:** 2026-04-10 (stable domain; no external library changes expected)
