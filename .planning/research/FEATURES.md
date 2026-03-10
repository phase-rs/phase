# Feature Landscape: MTGJSON Integration, Ability Schema, and Test Suite

**Domain:** MTG game engine data layer migration and test infrastructure
**Researched:** 2026-03-10
**Confidence:** HIGH (MTGJSON docs verified, codebase analysis complete, XMage test patterns confirmed MIT)

## Context

Forge.rs v1.2 replaces the Forge-derived card data (GPL-licensed `.txt` files) with MTGJSON (MIT-licensed JSON) for card metadata and a custom typed JSON format for ability/trigger/effect definitions. This enables MIT relicensing of the entire project. The milestone also adds comprehensive rules correctness tests, using XMage's MIT-licensed test scenarios as reference material.

The engine already has 38 effect handlers, 137 trigger modes, 61 static ability types, and 45 replacement effects -- all driven by string-keyed registries. The new ability format must map cleanly to these existing registries without changing the engine's core architecture.

Current card data: 78 curated Standard cards in `data/standard-cards/` as Forge `.txt` files, plus 32,300+ Forge card definitions used for coverage analysis.

---

## Table Stakes

Features that are required for the v1.2 milestone to be considered complete. Missing any = milestone not delivered.

| Feature | Why Required | Complexity | Dependencies | Notes |
|---------|-------------|------------|--------------|-------|
| MTGJSON card metadata loading | Replaces Forge `.txt` for name, mana cost, types, P/T, colors, keywords, oracle text, layout | Med | None | `StandardAtomic.json` provides all 78 cards' metadata. Existing `mtgjson` Rust crate (v5.2.2, MIT) has `AtomicCard` struct with all needed fields. |
| MTGJSON layout mapping | Map MTGJSON's 24 `Layout` enum variants to engine's `CardLayout` enum (9 variants) | Low | MTGJSON loading | Direct mapping: `Normal`->`Single`, `Transform`->`Transform`, `ModalDfc`->`Modal`, `Adventure`->`Adventure`, `Split`->`Split`, `Flip`->`Flip`, `Meld`->`Meld`. Others not needed for Standard. |
| Custom ability JSON schema | Typed JSON format for abilities, triggers, statics, replacements that maps to engine's `AbilityDefinition`, `TriggerDefinition`, `StaticDefinition`, `ReplacementDefinition` | High | MTGJSON loading (for card identity) | Must encode the same information as Forge's pipe-delimited `$`-separated strings but in structured JSON. 78 cards x ~2-4 abilities each = ~200 ability definitions. |
| Ability JSON parser | Rust code to deserialize ability JSON into existing engine types | Med | Ability JSON schema | Serde deserialization directly into existing types. Parser must produce identical `AbilityDefinition`/`TriggerDefinition`/etc. to current Forge parser output. |
| Card format migration (78 cards) | Convert all curated Standard cards from `.txt` to MTGJSON metadata + ability JSON | High | MTGJSON loading, ability JSON schema | Each card needs: metadata from MTGJSON + hand-authored ability definitions in new JSON format. Forge parser can be used as migration reference. |
| Forge parser made optional | Retain Forge parser as dev-only tool (behind feature flag or separate binary), not required at runtime | Low | Card format migration | Keep `parser/` module but gate behind `#[cfg(feature = "forge-compat")]` or similar. Database module gets a new `load_json()` path. |
| Remove Forge data directory | Delete `data/cardsfolder/` from repository, replace with MTGJSON + ability JSON files | Low | Card format migration | `data/standard-cards/` (78 `.txt` files) replaced with JSON equivalents. |
| MIT/Apache-2.0 relicensing | Change project license once all GPL data dependencies are removed | Low | Remove Forge data directory | License file change + Cargo.toml `license` field. Cannot happen until Forge `.txt` files are out of the repo. |
| Game state test helpers | Setup functions to construct game states with specific cards, zones, life totals for deterministic testing | Med | Ability JSON parser | Extend existing `test_helpers.rs` pattern. Must not depend on external Forge database -- load from bundled JSON test fixtures. |
| Rules correctness test suite | Integration tests that set up game scenarios, execute actions, and assert outcomes match MTG comprehensive rules | High | Game state test helpers | XMage's MIT-licensed tests provide reference scenarios. Port test *logic* (not Java code) into Rust `#[test]` functions. |
| Coverage report migration | Update `coverage_report` binary and `coverage.rs` to work with new JSON card format | Low | Card format migration | Currently checks abilities against registries using Forge format strings. Must check ability JSON instead. |
| CI gate preservation | CI must still enforce 100% Standard-legal coverage with new format | Low | Coverage report migration | Same `--ci` exit code behavior, just reading JSON instead of `.txt`. |

---

## Differentiators

Features that add significant value beyond the minimum required migration.

| Feature | Value Proposition | Complexity | Notes |
|---------|-------------------|------------|-------|
| JSON Schema validation for ability format | Validates ability JSON files at build time, catches typos in effect names / trigger modes before runtime | Med | Generate JSON Schema from Rust types via `schemars` crate. Validates that `api_type` values match registered effect handlers, `mode` values match `TriggerMode` variants, etc. |
| Ability format documentation generator | Auto-generate docs from the ability schema showing all supported effects, triggers, statics, and their parameters | Low | Iterate registry at build time, output markdown. Helps card authors. |
| Forge-to-JSON migration tool | Automated converter from Forge `.txt` format to new JSON format, producing ability JSON from parsed Forge strings | Med | Reads `.txt`, parses with existing parser, outputs JSON. Handles ~90% of cards automatically; complex SVars may need manual review. |
| Scryfall UUID cross-reference | Store Scryfall IDs from MTGJSON `identifiers.scryfallId` field, enabling direct Scryfall API lookups without name-based search | Low | MTGJSON's `Identifiers` struct already has `scryfall_id`. Replaces current name-based Scryfall lookups with deterministic UUID lookups. |
| Snapshot-based regression tests | Serialize full `GameState` after N actions, compare against golden snapshots to detect regressions | Med | Catches unintended behavior changes across engine refactors. Must handle nondeterminism (use seeded RNG, already using ChaCha20Rng). |
| Card-specific test fixtures | Per-card test files that exercise each card's specific abilities in isolation | Med | Ensures every curated card works correctly, not just overall coverage. More granular than XMage-style scenario tests. |
| Property-based testing for core rules | Use `proptest` or `quickcheck` to fuzz game actions and verify invariants (life never negative via SBA, stack empties fully, etc.) | Med | Catches edge cases humans wouldn't write tests for. Engine's pure `apply(state, action)` reducer pattern makes this feasible. |
| MTGJSON auto-update script | Script to download latest `StandardAtomic.json` and check for new/rotated Standard cards | Low | Keeps card data current with Standard rotation without manual downloads. |
| Comprehensive rules reference tests | Tests derived from MTG Comprehensive Rules sections (e.g., Rule 613 layer ordering, Rule 704 SBA, Rule 509 blocking restrictions) | High | Documents which rules sections are implemented. Each test cites the specific comprehensive rule number. |

---

## Anti-Features

Features to explicitly NOT build in this milestone.

| Anti-Feature | Why Avoid | What to Do Instead |
|--------------|-----------|-------------------|
| Natural language ability parsing | Parsing oracle text (`text` field) to derive abilities automatically. MTGA does ~80% of this with a Python parser + CLIPS rules engine, but it took a team years. | Hand-author ability JSON for each card. 78 cards is manageable. Oracle text is display-only. |
| Full MTGJSON AllPrintings integration | AllPrintings.json is 544 MB with 32,000+ cards and printing metadata (artist, rarity, frame). Massive scope. | Use `StandardAtomic.json` only (~2-5 MB). Load metadata for the 78 curated cards. Expand to more sets later. |
| Backwards-compatible Forge parser support | Maintaining both `.txt` and JSON loading paths as equal citizens adds testing/maintenance burden. | Make JSON the primary path. Keep Forge parser behind a feature flag for migration/reference only. |
| Card image URL generation from MTGJSON | MTGJSON has Scryfall IDs but doesn't serve images. Building image URL generation adds complexity. | Keep existing Scryfall API integration (`scryfall.ts`). Use MTGJSON's `identifiers.scryfallId` only for cross-reference, not as image source. |
| Automated XMage test porting | Writing a Java-to-Rust transpiler for XMage tests is a rabbit hole. | Read XMage tests for *scenarios* (what game state to set up, what to assert). Write Rust tests by hand inspired by those scenarios. |
| Token definitions from MTGJSON | MTGJSON has `TokenCard` data but the engine already creates tokens via the `Token` effect handler with inline definitions. | Keep token creation as-is. Token definitions in ability JSON, not separate MTGJSON token data. |
| Multi-format legality checking | MTGJSON has `legalities` for all formats (Modern, Legacy, etc.). Building format validation is scope creep. | Curate Standard cards by hand. The 78-card list is the source of truth, not MTGJSON legality data. |
| Porting XMage card implementations | XMage has Java implementations for 28,000+ cards. Porting those is a different project. | Port only XMage *test scenarios* as reference. Engine abilities are defined in JSON, not ported from XMage Java. |

---

## Feature Dependencies

```
MTGJSON StandardAtomic.json download
  └──> MTGJSON card metadata loader (Rust, serde)
       ├──> Layout mapping (MTGJSON Layout -> CardLayout)
       ├──> Scryfall UUID cross-reference (identifiers.scryfallId)
       └──> Card identity (name, types, costs, colors, P/T)
            └──> Ability JSON files (keyed by card name)
                 ├──> Ability JSON schema definition
                 │    └──> JSON Schema validation (optional)
                 ├──> Ability JSON parser (serde -> AbilityDefinition, etc.)
                 │    └──> CardDatabase.load_json() (new loading path)
                 │         ├──> Coverage report migration
                 │         │    └──> CI gate (100% coverage)
                 │         └──> Game state test helpers
                 │              └──> Rules correctness test suite
                 │                   ├──> Card-specific test fixtures
                 │                   ├──> Comprehensive rules tests
                 │                   └──> Snapshot regression tests
                 └──> Card format migration (78 cards)
                      └──> Remove Forge data directory
                           └──> MIT relicensing

Forge parser (existing)
  └──> Forge-to-JSON migration tool (optional, accelerates migration)
       └──> Card format migration
```

---

## MVP Recommendation

### Priority 1: MTGJSON Integration + Ability Schema Definition

1. **MTGJSON metadata loading** -- Download `StandardAtomic.json`, add `mtgjson` crate dependency, write loader mapping `AtomicCard` fields to engine's `CardFace` fields (name, mana_cost, card_type, power, toughness, loyalty, defense, colors, keywords, oracle_text, layout)
2. **Ability JSON schema** -- Define the JSON structure that maps to `AbilityDefinition` (kind + api_type + params), `TriggerDefinition` (mode + params), `StaticDefinition` (mode + params), `ReplacementDefinition` (event + params), plus SVar references
3. **Ability JSON parser** -- Serde deserialization into existing engine types. Validate api_type against effect registry at load time

**Rationale:** The schema is the critical design decision. Everything else is plumbing once the schema is right. Design it to map 1:1 to existing engine types so no engine changes are needed.

### Priority 2: Card Migration

4. **Forge-to-JSON migration tool** -- Automated converter reads `.txt`, outputs JSON. Gets ~90% of 78 cards right automatically
5. **Manual ability JSON authoring** -- Hand-review and fix the ~10% that need adjustment. Verify each card loads and resolves correctly
6. **CardDatabase.load_json()** -- New loading path in `card_db.rs` that reads MTGJSON metadata + ability JSON instead of `.txt` files

**Rationale:** Migration is mechanical work but must be done carefully. The migration tool pays for itself even for 78 cards because it validates the schema against real data.

### Priority 3: Test Infrastructure

7. **Game state test helpers** -- Functions like `setup_game(cards_for_p1, cards_for_p2) -> GameState`, `add_to_zone(state, card_name, zone, player)`, `assert_life(state, player, expected)`, `assert_zone_count(state, zone, player, expected)`
8. **Rules correctness tests** -- Integration tests covering core mechanics: ETB triggers, combat damage, stack resolution, state-based actions, layer system, keyword interactions. Reference XMage MIT tests for scenario ideas
9. **Card-specific test fixtures** -- At least one test per card exercising its primary ability

**Rationale:** Tests must work with the new JSON format. Test helpers that load from JSON fixtures make tests self-contained and CI-friendly (no external Forge database dependency).

### Priority 4: Cleanup and Licensing

10. **Remove Forge data** -- Delete `data/cardsfolder/` and `data/standard-cards/` (`.txt` files)
11. **Feature-gate Forge parser** -- `#[cfg(feature = "forge-compat")]` on parser module
12. **MIT relicensing** -- Update LICENSE, Cargo.toml, README

### Defer:

- **JSON Schema validation** -- nice for third-party card authors but not blocking for 78 curated cards
- **Property-based testing** -- valuable but can be added incrementally after basic tests exist
- **MTGJSON auto-update script** -- Standard rotation happens infrequently; manual update is fine for now
- **Comprehensive rules reference tests** -- ambitious catalog; build incrementally as bugs surface

---

## Complexity Assessment

| Feature Area | Estimated Effort | Risk | Notes |
|-------------|-----------------|------|-------|
| MTGJSON integration | Low-Med | Low | Existing `mtgjson` crate handles deserialization. Mapping is straightforward. |
| Ability JSON schema | Med-High | Med | Design decision with long-term consequences. Must handle SVars, sub-ability chains, conditional execution, and all 38 effect types. |
| Card migration (78 cards) | Med | Low | Mechanical but tedious. Migration tool reduces effort. Verification is the real work. |
| Forge parser removal | Low | Low | Feature flag, not deletion. Parser code stays for reference. |
| Test infrastructure | High | Med | Building a good test harness takes iteration. XMage provides proven patterns to follow. |
| Rules correctness tests | High | Med | Combinatorial complexity of MTG rules means infinite possible tests. Must scope to highest-value scenarios. |
| MIT relicensing | Low | Low | Straightforward once Forge data is removed. No code changes needed. |

---

## Sources

- [MTGJSON Official Documentation](https://mtgjson.com/) -- Card data models, download files, format specifications
- [MTGJSON Card (Atomic) Data Model](https://mtgjson.com/data-models/card/card-atomic/) -- Complete field schema for atomic card data
- [MTGJSON Downloads](https://mtgjson.com/downloads/all-files/) -- StandardAtomic.json and other format-specific files
- [mtgjson Rust crate (docs.rs)](https://docs.rs/mtgjson/latest/mtgjson/) -- v5.2.2, MIT/Apache-2.0, `AtomicCard`, `SetCard`, `Layout` enum (24 variants)
- [XMage GitHub Repository](https://github.com/magefree/mage) -- MIT-licensed MTG rules engine with comprehensive test suite
- [XMage Testing Tools Wiki](https://github.com/magefree/mage/wiki/Development-Testing-Tools) -- `CardTestPlayerBase`, `addCard()`, `castSpell()`, `assertLife()` patterns
- Direct codebase analysis: `crates/engine/src/parser/` (Forge parser), `crates/engine/src/types/` (engine types), `crates/engine/src/game/effects/mod.rs` (38 effect handlers), `crates/engine/src/game/test_helpers.rs` (existing test infrastructure)
