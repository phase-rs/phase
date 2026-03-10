# Project Research Summary

**Project:** Forge.rs v1.2 — Data Source Migration & Test Infrastructure
**Domain:** MTG game engine data layer migration (Forge GPL -> MTGJSON MIT) with comprehensive test suite
**Researched:** 2026-03-10
**Confidence:** HIGH

## Executive Summary

Forge.rs v1.2 is a licensing and infrastructure milestone, not a gameplay feature milestone. The goal is to replace all GPL-licensed Forge card data (`.txt` files, `data/cardsfolder/`) with MIT-licensed MTGJSON metadata plus a new custom JSON ability format, enabling MIT/Apache-2.0 relicensing of the entire project. The engine's core architecture — the `apply(state, action) -> ActionResult` reducer, effect/trigger/static/replacement registries, WASM bridge, AI, and all downstream components — remains completely unchanged. Only the input pipeline (how card data enters the engine) and validation pipeline (how correctness is verified) change.

The recommended approach is layered: MTGJSON supplies card metadata (name, types, mana cost, P/T, colors, keywords, oracle text, layout) while a hand-authored custom JSON format supplies ability mechanics (ability definitions, triggers, static abilities, replacements, SVars). These two sources merge into the existing `CardFace` type the rest of the engine already consumes. The custom ability JSON schema is not a new invention — it is the direct JSON serialization of the existing `AbilityDefinition`, `TriggerDefinition`, `StaticDefinition`, and `ReplacementDefinition` Rust types. The test infrastructure, modeled on XMage's MIT-licensed scenario patterns, must be built before any card migration begins.

The primary risk is the runtime parsing spread: `GameObject.abilities` currently stores raw Forge pipe-delimited strings that are re-parsed by `parse_ability()` at 20+ call sites throughout the game loop (casting, triggers, mana detection, planeswalker activation, sub-ability chaining). This must be addressed before card migration: the JSON loader must either emit Forge-compatible strings (safe, preserves all runtime code) or the runtime representation must change to `Vec<AbilityDefinition>` (cleaner long-term, touches ~13 source files). The secondary risk is GPL contamination: ability JSON files derived mechanically from Forge `.txt` files may be considered derivative works — the safest approach is authoring from MTG Comprehensive Rules and oracle text, using Forge output only for validation comparison.

## Key Findings

### Recommended Stack

The v1.2 stack changes are minimal and additive — all new dependencies are dev-only with zero WASM binary impact. The net WASM binary size change is -15KB from feature-gating `walkdir`. The key decision is custom MTGJSON deserialization types (~50 lines) rather than the `mtgjson` crate (v5.2.2), which is missing the `defense` field needed for Battle cards and pulls in `chrono`, `semver`, and `uuid` as unnecessary transitive dependencies.

See `.planning/research/STACK.md` for full rationale and dependency manifest.

**Core technologies:**
- `serde_json` (existing): MTGJSON deserialization — already in workspace, standard Rust JSON library
- Custom MTGJSON types (~50 lines, new): 15-field subset struct avoiding 3 unnecessary transitive deps from the `mtgjson` crate; covers all fields needed including `defense` for Battle cards
- `schemars` 1.2.1 (dev-only): JSON Schema generation from Rust ability types — schema auto-updates when types change, MIT licensed, released Feb 2026
- `insta` 1.46.3 (dev-only): Snapshot testing — `GameState` already implements `Serialize`, zero adaptation cost; `assert_json_snapshot!` replaces brittle assertion chains
- `test-case` 3.3.1 (dev-only): Parameterized test cases — `#[test_case(...)]` generates named sub-tests; ideal for "same mechanic, different cards" patterns
- `walkdir` (feature-gated behind `forge-compat`): Forge parser retained for migration tooling only, excluded from all release/WASM builds

### Expected Features

See `.planning/research/FEATURES.md` for the full feature dependency graph and complexity assessments.

**Must have (table stakes for v1.2 completion):**
- MTGJSON card metadata loading (`StandardAtomic.json`) — replaces Forge `.txt` for name, types, mana cost, P/T, colors, layout, oracle text
- Custom ability JSON schema — typed JSON format mapping to `AbilityDefinition`, `TriggerDefinition`, `StaticDefinition`, `ReplacementDefinition`
- Ability JSON parser — serde deserialization into existing engine types with load-time registry validation
- Card format migration — all 78 Standard cards converted from `.txt` to MTGJSON + ability JSON
- Forge parser made optional behind `forge-compat` feature flag
- Remove Forge data directory from the repository (`data/cardsfolder/`, `data/standard-cards/`)
- MIT/Apache-2.0 relicensing (after Forge data is fully removed)
- Game state test helpers — self-contained `GameScenario` builder with no external filesystem dependencies
- Rules correctness test suite — scenario-based tests exercising core mechanics
- Coverage report migration to JSON format with CI gate (100% Standard coverage) preserved

**Should have (differentiators worth including in v1.2):**
- JSON Schema validation for ability format — catches authoring errors at build time via `schemars`, useful for future card authors
- Forge-to-JSON migration tool — automated converter accelerates the 78-card migration and validates the schema against real data
- Scryfall UUID cross-reference — use `identifiers.scryfallOracleId` from MTGJSON for more reliable frontend image lookups
- Snapshot-based regression tests — `insta` snapshots of `GameState` after action sequences to catch unintended engine changes

**Defer (v2+):**
- Property-based testing with `proptest` — valuable but builds on basic test infrastructure that must exist first
- Comprehensive rules reference tests (per Comprehensive Rule number) — ambitious; build incrementally as bugs surface
- MTGJSON auto-update script — Standard rotation is infrequent; manual update acceptable
- Full Standard expansion (~2,000 cards) — architecture scales but expansion is a separate milestone
- Natural language ability parsing from oracle text — NLP problem far beyond this milestone's scope

### Architecture Approach

The architecture preserves all engine internals and changes only the input pipeline. MTGJSON provides card metadata via a new `crates/engine/src/mtgjson/` module; the custom ability JSON provides behavior via a new `crates/engine/src/abilities/` module; these two sources merge in a new `CardDatabase::load_json()` method that produces the same `CardFace` type the engine already consumes. Everything downstream of `CardFace` — the `apply()` function, all effect handlers, the WASM bridge, the AI, and the frontend — is untouched.

See `.planning/research/ARCHITECTURE.md` for component boundaries, detailed type mappings, multi-face card handling, and the build-order dependency graph.

**Major components:**
1. `crates/engine/src/mtgjson/` (new) — deserializes MTGJSON AtomicCards into `CardFace` metadata fields; handles layout/side/faceName for multi-face cards; provides `parse_mtgjson_mana_cost()` for the `{2}{R}` format (different from Forge's `2 R`)
2. `crates/engine/src/abilities/` (new) — deserializes custom ability JSON into `AbilityDefinition`, `TriggerDefinition`, `StaticDefinition`, `ReplacementDefinition`; validates api_type/mode values against registries at load time
3. `CardDatabase::load_json()` (modified) — merges MTGJSON metadata + ability JSON into `CardFace`; becomes the primary loading path; existing `load()` (Forge `.txt`) feature-gated behind `forge-compat`
4. `crates/engine/src/testing/` (new) — `GameScenario` builder with `add_card()`, `set_phase()`, `act()`, and assertion helpers; all test data embedded in repo, no external filesystem dependencies
5. `crates/engine/tests/` (new) — scenario-based integration tests organized as `rules/`, `effects/`, `cards/`, `keywords/`

### Critical Pitfalls

See `.planning/research/PITFALLS.md` for all 14 pitfalls with phase-specific warnings and detection strategies.

1. **Runtime Forge string parsing at 20+ call sites** — `GameObject.abilities` stores raw pipe-delimited strings re-parsed during every cast, trigger, mana detection, and sub-ability chain. The new format must address this before migration: either emit Forge-compatible strings from the JSON loader (safe, preserves all runtime code) or change `GameObject.abilities` to `Vec<AbilityDefinition>` (parse once at load time). The strategy must be decided first — it determines whether migration touches 0 or ~13 source files.

2. **Ability schema failing to cover the full engine surface** — 38 effect handlers, 137 trigger modes, 61 static ability modes, 45 replacement event types, SVar sub-ability chaining, conditional execution, and parameterized keywords must all be expressible. The safe approach: the schema IS a JSON serialization of the existing Rust types, not a new design. Validate against all 78 cards via round-trip testing before committing.

3. **GPL contamination from mechanical Forge conversion** — ability JSON files created by automated conversion from Forge `.txt` may be derivative works of GPL code. Author ability definitions from MTG Comprehensive Rules and oracle text; use Forge parser output only as a validation comparator, not a source. Keep records showing the authoring process.

4. **MTGJSON provides metadata only, not ability mechanics** — `text` is human-readable oracle text; `keywords` contains names without parameters. Budget 78 hand-authored ability definitions, not just "integrate MTGJSON." The custom format must be independently authored for every card.

5. **Regression in 78 Standard cards during migration** — the coverage gate checks handler presence, not behavioral correctness. Cards can pass CI while producing wrong game state. Build the `GameScenario` test harness and A/B comparison tests before migrating any cards.

## Implications for Roadmap

The dependency graph from ARCHITECTURE.md makes the phase ordering unambiguous. The runtime type decision must be made first (it shapes everything downstream); schema must be validated against real cards before migration; tests must exist before cards are migrated; GPL data must be fully removed before relicensing.

### Phase 1: Runtime Type Decision & MTGJSON + Ability Schema Foundation
**Rationale:** The most critical pitfall (#1) is the 20+ runtime call sites for `parse_ability()`. Choosing the migration strategy (emit Forge-compatible strings vs. store `Vec<AbilityDefinition>`) must happen before schema design because it determines what the JSON loader produces. MTGJSON loader and ability schema design are parallelizable with each other but both must precede card migration. Audit all 78 cards' SVar keys and param patterns before finalizing the schema.
**Delivers:** Decided and documented runtime type strategy; `crates/engine/src/mtgjson/` module with MTGJSON types and loader; `parse_mtgjson_mana_cost()` for `{2}{R}` format; custom ability JSON schema with schemars export; schema validated by round-trip testing all 78 existing Forge cards
**Addresses:** MTGJSON card metadata loading, ability JSON schema, MTGJSON layout mapping
**Avoids:** Pitfall #1 (runtime parsing), Pitfall #2 (schema gaps), Pitfall #3 (expecting ability data from MTGJSON), Pitfall #9 (multi-face name mismatch), Pitfall #11 (keyword parameters lost)

### Phase 2: Unified Card Loading Pipeline
**Rationale:** With the schema validated and the runtime strategy decided, `CardDatabase::load_json()` is straightforward integration work. This phase wires the two data sources into the existing engine pipeline and confirms the approach works end-to-end with a few test cards before the full 78-card migration.
**Delivers:** `CardDatabase::load_json(mtgjson_path, abilities_dir)` merging MTGJSON metadata + ability JSON into `CardFace`; multi-face card handling (Adventure, Transform, Modal); `CardDatabase::load()` feature-gated behind `forge-compat`; smoke test confirming 5-10 cards load and resolve correctly via the new path
**Uses:** Custom MTGJSON types, serde, `StandardAtomic.json` checked into repo
**Implements:** Unified CardDatabase v2 component
**Avoids:** Pitfall #10 (database assumes .txt), Pitfall #6 (frontend card-data.json breakage handled incrementally)

### Phase 3: Test Infrastructure
**Rationale:** Tests must exist before the full 78-card migration begins (Pitfall #5). Rules tests (combat, priority, SBA, layer system) are independent of the new card loading pipeline and can be written immediately in parallel with Phase 2. The `GameScenario` harness replaces the current `test_helpers.rs` which silently skips tests when the external Forge directory is absent.
**Delivers:** `crates/engine/src/testing/` module with `GameScenario` builder; initial rules test suite covering core mechanics (ETB triggers, combat, stack resolution, state-based actions, layer system, keyword interactions); all tests self-contained and CI-runnable with no external dependencies; `cargo test` passes with no silent skips
**Uses:** `insta` 1.46.3 for snapshot assertions, `test-case` 3.3.1 for parameterized scenarios, XMage scenarios as reference for test ideas (not code)
**Implements:** Test harness component
**Avoids:** Pitfall #5 (regressions during migration), Pitfall #8 (XMage patterns instead of Rust idioms), Pitfall #13 (silent test skips)

### Phase 4: Card Migration (78 Cards)
**Rationale:** With the loader, schema, and test harness in place, each card can be migrated and immediately validated. The Forge-to-JSON migration tool accelerates the process. GPL contamination is avoided by authoring ability definitions from oracle text, using Forge output for validation only. Cards are migrated one at a time with a per-card test confirming identical behavior.
**Delivers:** All 78 Standard cards in `data/cards/` as JSON (curated `standard_cards.json` for metadata + per-card ability JSON); passing A/B comparison tests for every card confirming behavioral parity with Forge format; CI gate preserved (100% Standard coverage)
**Addresses:** All table-stakes migration features; Forge-to-JSON migration tool as a differentiator; card-specific test fixtures
**Avoids:** Pitfall #4 (GPL contamination), Pitfall #5 (regressions), Pitfall #7 (SVar system not fully replaced), Pitfall #11 (keyword parameters)

### Phase 5: Cleanup, Licensing, and Polish
**Rationale:** Once all 78 cards load correctly from the new format and tests pass, the Forge data can be safely removed and the project relicensed. This phase is non-code-heavy but legally significant — the license change cannot happen until the last GPL file is gone from the repository (including git history consideration).
**Delivers:** `data/cardsfolder/` and `data/standard-cards/` deleted; Forge parser behind `forge-compat` feature flag; LICENSE and Cargo.toml updated to MIT/Apache-2.0; WASM binary verified -15KB smaller; frontend `card-data.json` export pipeline updated; `coverage.rs` reading JSON format; CI gate verified green
**Addresses:** MIT relicensing (table stakes), Forge parser made optional, coverage report migration, CI gate preservation
**Avoids:** Pitfall #4 (GPL contamination), Pitfall #6 (frontend breakage), Pitfall #12 (WASM size), Pitfall #14 (parser bitrot)

### Phase Ordering Rationale

- Phase 1 must come first: the runtime type strategy decision shapes every other phase. Schema design and MTGJSON loader development can overlap but both must precede the full loading pipeline.
- Phase 2 requires Phase 1's schema and loader foundations to be in place. A few cards should be proven end-to-end before Phase 4 begins.
- Phase 3 (test infrastructure) can overlap significantly with Phase 2. Rules tests don't depend on the new card loading pipeline at all. The `GameScenario` harness must exist before Phase 4 starts.
- Phase 4 is the bulk of the mechanical work. Card-specific tests can be written alongside each card migration, validating behavioral parity.
- Phase 5 is strictly last — license change cannot happen until all GPL data is removed and verified gone from the repository.

### Research Flags

Phases likely needing deeper research during planning:
- **Phase 1 (Runtime Type Decision):** The 20+ `parse_ability()` call sites need careful mapping before choosing the migration strategy. Direct codebase inspection of `casting.rs`, `mana_abilities.rs`, `planeswalker.rs`, `triggers.rs`, `effects/mod.rs` required to enumerate every affected site and estimate the change scope for the `Vec<AbilityDefinition>` approach.
- **Phase 1 (SVar Key Audit):** All SVar keys across the 78 Standard cards need cataloging before schema design. Categories: sub-abilities (`Execute$`, `SubAbility$`), AI hints (`RemAIDeck`, `AILogic`), display metadata, conditional values. This determines whether nested typed sub-ability definitions are needed in v1.2.
- **Phase 4 (GPL Contamination):** The legal analysis has LOW confidence. Recommend reviewing MTG open-source community precedents (MTGA Deck Tracker, Cockatrice approaches) or consulting legal counsel before committing to the conversion approach.

Phases with standard patterns (can skip research-phase during planning):
- **Phase 2 (MTGJSON Loader):** Well-documented MTGJSON v5 API, serde patterns are standard Rust. The custom types are ~50 lines of straightforward deserialization.
- **Phase 3 (Test Infrastructure):** XMage patterns are well-documented and directly applicable. `GameScenario` builder follows established Rust test helper patterns.
- **Phase 5 (Cleanup):** Mechanical tasks — delete files, update Cargo.toml, feature flags. No research needed.

## Confidence Assessment

| Area | Confidence | Notes |
|------|------------|-------|
| Stack | HIGH | All decisions verified against official docs (MTGJSON, docs.rs, crate changelogs). `mtgjson` crate limitations confirmed by inspecting struct definitions. No speculative choices. |
| Features | HIGH | Feature list derived from direct codebase analysis of all 78 Standard cards, 38 effect handlers, 137 trigger modes. MTGJSON field coverage verified against official schema. |
| Architecture | HIGH | Engine internals traced directly. The "merge into CardFace" approach confirmed by the existing type system. MTGJSON multi-face handling (layout/side/faceName) verified against official docs. WASM bridge and AI confirmed unchanged. |
| Pitfalls | HIGH (engineering) / LOW (legal) | Runtime parsing pitfall confirmed by direct inspection of call sites with file paths and line numbers. GPL analysis is LOW confidence — legal domain, not engineering. |

**Overall confidence:** HIGH (engineering decisions), LOW (GPL contamination legal analysis)

### Gaps to Address

- **Runtime type migration strategy:** Whether to store `Vec<AbilityDefinition>` on `GameObject` or emit Forge-compatible strings from the new loader is the most consequential decision of the milestone. Both work; the former is cleaner long-term but touches ~13 source files; the latter is safer for v1.2 and defers the refactor. Must be decided explicitly in Phase 1 planning, not discovered during implementation.
- **SVar key audit:** Before finalizing the ability schema, all SVar keys across the 78 Standard cards need cataloging. Categories: sub-abilities, AI hints, display metadata, conditional values. This determines schema design for sub-ability chaining.
- **GPL derivative work legal analysis:** The merger doctrine argument is plausible for simple cards but weaker for complex multi-ability chains. LOW confidence. Consider legal consultation or clean-room authoring for complex cards.
- **Keyword parameter inventory:** Which of the 78 cards use parameterized keywords (Ward:N, Protection from X, Cycling {cost})? Determines whether the ability format needs a dedicated parameterized keyword section or can preserve the existing keyword string format.
- **WASM card data loading:** Whether card JSON is embedded in the WASM binary or loaded at runtime via JavaScript affects the initialization path. Recommendation: load at runtime to keep WASM binary small (current binary is 19KB; embedding 78-card JSON would expand it significantly).

## Sources

### Primary (HIGH confidence)
- Forge.rs codebase — direct analysis of `crates/engine/src/` including parser/, database/, types/, game/ modules; all 38 effect handlers, 137 trigger modes, 61 static ability modes, 45 replacement effects, `test_helpers.rs`, `card_db.rs`
- [MTGJSON Card (Atomic) Data Model](https://mtgjson.com/data-models/card/card-atomic/) — official documentation, verified field schema confirming no machine-readable ability definitions
- [MTGJSON Downloads](https://mtgjson.com/downloads/all-files/) — file formats and sizes confirmed; `StandardAtomic.json` verified as Standard-legal subset
- [XMage GitHub Repository](https://github.com/magefree/mage) — MIT license confirmed, test organization by mechanic verified

### Secondary (MEDIUM confidence)
- [schemars documentation](https://graham.cool/schemars/) — v1.2.1, serde compatibility verified on docs.rs
- [insta snapshot testing](https://insta.rs/) — v1.46.3, `GameState` serialization compatibility confirmed
- [test-case crate](https://docs.rs/test-case/latest/test_case/) — v3.3.1, parameterized test macro syntax verified
- [mtgjson Rust crate docs](https://docs.rs/mtgjson/latest/mtgjson/) — v5.2.2, confirmed missing `defense` field and `chrono`/`semver`/`uuid` transitive deps
- [XMage Testing Tools Wiki](https://github.com/magefree/mage/wiki/Development-Testing-Tools) — test structure and `CardTestPlayerBase` patterns

### Tertiary (LOW confidence)
- GPL derivative work legal analysis — based on merger doctrine / ABA references; not legal advice; legal outcome uncertain

---
*Research completed: 2026-03-10*
*Ready for roadmap: yes*
