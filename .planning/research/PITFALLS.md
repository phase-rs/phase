# Domain Pitfalls

**Domain:** MTGJSON integration, custom ability format, test infrastructure, and MIT relicensing for an existing MTG rules engine
**Researched:** 2026-03-10
**Overall Confidence:** HIGH (based on thorough codebase analysis of 54 test modules, 38 effect handlers, 137 trigger modes, 78 curated cards, plus external domain research)

## Critical Pitfalls

Mistakes that cause rewrites, regressions, or block the milestone entirely.

---

### Pitfall 1: Raw Forge Strings Parsed at Runtime Throughout the Engine

**What goes wrong:** The engine stores abilities as `Vec<String>` in Forge's pipe-delimited format (`"SP$ DealDamage | NumDmg$ 3"`) on both `CardFace` and `GameObject`. These raw strings are parsed on-the-fly by `parse_ability()` during gameplay -- at cast time (`casting.rs:60, 208`), mana ability detection (`mana_abilities.rs:15, 43`), planeswalker activation (`planeswalker.rs:170`), sub-ability chaining (`effects/mod.rs:129`), trigger resolution (`triggers.rs:176`), and coverage analysis (`coverage.rs`). The format is NOT just a load-time concern; it is woven through the entire game loop across at least 20 call sites.

**Why it happens:** A natural assumption is "replace the parser, swap the card files, done." But `GameObject.abilities` is `Vec<String>` containing raw Forge notation that gets re-parsed every time an ability is cast, activated, or chained. Replacing the data source without changing this runtime parsing will either require the new format to still emit Forge-compatible strings (defeating the purpose) or require touching every call site simultaneously.

**Consequences:**
- If you change `CardFace` to use a new format but `GameObject.abilities` still stores Forge strings, runtime parsing breaks
- If you change `GameObject.abilities` to typed data, every file that calls `parse_ability()` at runtime must be updated simultaneously
- Partial migration (some cards new format, some old) creates two code paths through the entire engine
- The `AbilityDefinition` type already exists as a clean intermediate representation, but it's constructed transiently during parse_ability() and immediately destructured -- never stored on the game object

**Prevention:**
1. Map every call site of `parse_ability`, `parse_trigger`, `parse_static`, `parse_replacement` in the game loop (identified: `casting.rs`, `engine.rs`, `mana_abilities.rs`, `planeswalker.rs`, `effects/mod.rs`, `effects/effect.rs`, `triggers.rs`, `deck_loading.rs`, `coverage.rs`)
2. Design the new ability format to serialize directly to `AbilityDefinition`, `TriggerDefinition`, `StaticDefinition`, `ReplacementDefinition` -- these existing Rust types ARE the schema
3. Two-phase approach: FIRST make `GameObject` store `Vec<AbilityDefinition>` instead of `Vec<String>` (parse once at load time, store typed data), THEN swap the card data source. The first step eliminates runtime parsing regardless of data source.
4. Never store raw format strings on runtime objects -- parse to typed structures at deck-load time only

**Detection:** Any test that exercises casting, activating, or resolving abilities will fail immediately if format migration is incomplete. The 78 curated Standard cards' CI gate provides an integration safety net.

**Phase that must address this:** FIRST phase of the milestone. All other work depends on clean runtime types.

---

### Pitfall 2: New Ability Schema Fails to Cover the Full Engine Surface Area

**What goes wrong:** The custom JSON ability schema is designed to cover common cases but misses edge cases in the existing engine. The engine has:
- 38 registered effect handlers (DealDamage through Mana -- see `effects/mod.rs:46-86`)
- 137+ trigger modes (ChangesZone through ElementalBend -- see `types/triggers.rs`)
- 61+ static ability modes (Continuous, CantAttack, Ward, Protection, etc. -- see `static_abilities.rs:37-84` plus 30+ stubs)
- 45+ replacement effect event types
- Complex features: sub-ability chaining via SVar references (`SubAbility$`, `Execute$`), conditional execution (`ConditionCompare$`, `ConditionPresent$`, `ConditionSVarCompare$`), target inheritance (`Defined$ Targeted`), and parameterized keywords (`Ward:1`, `Protection from red`)

**Why it happens:** Schema design often starts with clean examples (Lightning Bolt, Grizzly Bears). Complex cards use features that are harder to model:
- SVars as a general-purpose key-value store for cross-ability data sharing
- Ability chains where a trigger's `Execute$` parameter points to an SVar containing another ability definition
- Filter expressions embedded in params (`ValidTgts$ Creature.YouCtrl+nonBlack`)
- Multiple abilities per card face (a card can have 3+ activated abilities)
- Keywords with varied parameter formats (`K:Ward:1`, `K:Cycling:2 R`, `K:Protection from red`)

**Consequences:**
- Cards that worked before migration silently break because their ability chain can't be expressed in the new schema
- The CI coverage gate catches this late -- after the schema is already designed and cards partially migrated
- Redesigning the schema after partial migration means re-converting all migrated cards

**Prevention:**
1. Before designing the schema, audit every unique parameter key and SVar pattern across all 78 curated Standard cards
2. The schema should be a **direct JSON serialization of the existing Rust types**: `AbilityDefinition { kind, api_type, params }`, `TriggerDefinition { mode, params }`, `StaticDefinition { mode, params }`, `ReplacementDefinition { event, params }`. This is not a new invention -- it's `serde_json::to_value()` of what already works
3. Run an automated conversion of all 78 cards (Forge `.txt` -> parse -> serialize to JSON) and verify 100% round-trip fidelity BEFORE committing to the schema
4. The `HashMap<String, String>` params pattern is ugly but powerful -- resist the temptation to replace it with strongly-typed per-effect-type structs in this milestone (that's a future optimization)

**Detection:** Write a test that loads every curated card in both formats and asserts identical `AbilityDefinition` structures.

**Phase that must address this:** Schema design phase, BEFORE any card migration begins.

---

### Pitfall 3: MTGJSON Provides Card Metadata, Not Ability Mechanics

**What goes wrong:** MTGJSON provides card metadata (name, manaCost, types, power/toughness, colors, keywords list, oracle text, layout, side, otherFaceIds, legalities, set codes, identifiers) but does NOT provide machine-readable ability definitions. The `text` field is human-readable oracle text ("Lightning Bolt deals 3 damage to any target"). The `keywords` array contains keyword names without parameters (["Flying", "Trample"] not ["Ward:1"]). There is NO equivalent to Forge's `A:SP$ DealDamage | NumDmg$ 3`.

**Why it happens:** MTGJSON is a card catalog database, not a game engine data source. Its purpose is card information (printings, legalities, prices, rulings), not executable game rules.

**Consequences:**
- MTGJSON supplies the "card shell" (metadata): name, mana cost, types/subtypes/supertypes, colors, P/T, loyalty, defense, keyword names, oracle text, layout, side, face relationships, legalities, set codes, UUIDs
- MTGJSON CANNOT supply the "card brain" (mechanics): ability definitions, trigger definitions, static ability definitions, replacement effect definitions, SVar data, ability parameters, sub-ability chains, condition logic
- The custom ability format must be independently authored for every card -- the scope is larger than "integrate MTGJSON"

**Prevention:**
1. Clearly separate the two data sources in architecture: MTGJSON for card metadata, custom JSON for ability mechanics
2. Budget the effort correctly: each of the 78 cards needs hand-authored (or machine-converted) ability definitions
3. Use the existing Forge parser as a one-time conversion tool: parse `.txt` files -> serialize `AbilityDefinition` structs to JSON -> validate -> commit as the new format
4. Do NOT attempt to parse oracle text into ability definitions -- that is an NLP problem far beyond the scope of this milestone

**Detection:** If card loading produces objects with empty `abilities` / `triggers` / `static_abilities` vectors, MTGJSON data was used without companion ability data.

**Phase that must address this:** Architecture/design phase. This shapes the entire data pipeline.

---

### Pitfall 4: GPL Contamination from Forge Card Definition Content

**What goes wrong:** The project wants to relicense as MIT. Forge is GPL-licensed. The `.txt` card definitions in `data/cardsfolder/` are part of Forge's GPL codebase. If the new custom ability JSON files are mechanically derived from Forge's `.txt` files (even through an automated conversion), there is a legal argument they are derivative works of GPL content.

**Why it happens:** The ability definitions in Forge's `.txt` files contain creative expression in how game mechanics are decomposed into the `SP$/AB$/DB$` system with specific parameter choices, sub-ability chains, and SVar structures. While the underlying game rules (MTG Comprehensive Rules) are uncopyrightable facts, and the merger doctrine may apply (there are limited ways to express "deal 3 damage"), Forge's specific encoding choices in their DSL are potentially copyrightable creative expression.

**Consequences:**
- If converted JSON files are considered derivative works of GPL code, the entire project cannot be MIT-licensed
- Clean-room implementation from Comprehensive Rules is legally safest but labor-intensive
- Partial contamination (some files derived, some clean) risks the whole license

**Prevention:**
1. **Strongest approach:** Write ability definitions independently using MTG Comprehensive Rules and MTGJSON oracle text as specifications. Do not read or convert Forge `.txt` files.
2. **Pragmatic approach:** The ability schema maps to generic concepts (deal N damage, draw N cards, target type T). Forge's format is one of very few ways to express MTG game rules programmatically. The merger doctrine provides that when expression merges with idea (because there are limited ways to express the idea), the expression is not protected. For simple cards this is strong; for complex multi-ability chains this is weaker.
3. **Hybrid approach:** Use Forge conversion for validation only -- write definitions from oracle text, then compare against Forge output to verify correctness. The Forge data validates, it does not source.
4. **Mandatory:** Remove `data/cardsfolder/` from the repository entirely. Its presence implies GPL content in the project. Retain the Forge parser only behind an optional Cargo feature flag (`forge-compat`) for development validation.
5. **Document the process:** Keep records showing ability definitions were authored from game rules and oracle text.

**Detection:** Git history showing mechanical conversion from `.txt` to `.json` without intermediate human authoring step would be evidence of derivation.

**Phase that must address this:** Must be addressed before removing Forge data. Legal analysis should inform the approach for the card migration phase.

---

### Pitfall 5: Regression in 78 Standard Cards During Migration

**What goes wrong:** A card that worked correctly before migration breaks silently after conversion to the new format. The bug may be subtle -- a missing parameter, a different ability chain order, a condition that doesn't evaluate correctly. The existing CI gate checks "coverage" (all handlers registered) but does NOT test that cards play correctly.

**Why it happens:** The coverage check in `coverage.rs` only verifies that effect handlers, trigger matchers, and static ability handlers exist for each card's mechanics. It does not verify that abilities resolve correctly during gameplay. A card could pass coverage (all handlers registered) but produce wrong game state when played (wrong damage amount, missing trigger, incorrect targeting).

**Consequences:**
- Cards appear "supported" but have subtle behavioral regressions
- These bugs surface during playtesting, not CI
- Players lose trust in the engine's correctness

**Prevention:**
1. Create gameplay-level integration tests BEFORE migrating any card data. Each test: set up game state -> play a card -> assert resulting state changes (life totals, zone contents, P/T modifications, etc.)
2. Run the same gameplay tests against both old Forge format and new custom format to verify identical outcomes (A/B comparison)
3. Use deterministic RNG seeds (`ChaCha20Rng`) to make tests exactly reproducible
4. Minimum test coverage: one card per effect type actually used in Standard (not all 38, just those in the 78 cards), plus one card per trigger mode, plus one card per static ability mode
5. Test infrastructure phase MUST precede card migration phase

**Detection:** A/B comparison tests between old and new format for every curated card. Any deviation is a regression.

**Phase that must address this:** Test infrastructure must be built BEFORE card migration starts.

---

## Moderate Pitfalls

### Pitfall 6: Frontend card-data.json Dependency on Forge Format

**What goes wrong:** The frontend uses a precompiled `client/public/card-data.json` generated by the `card_data_export` binary from Forge data. This serializes `CardFace` objects. If `CardFace` changes structure, the JSON format changes, and frontend TypeScript types need to match.

**Prevention:**
- Change the export format incrementally -- add new fields alongside old ones during transition
- The WASM bridge uses `tsify` for type generation, but `card-data.json` is a separate pipeline from `card_data_export` binary
- Update TypeScript types in the same PR as Rust `CardFace` changes
- Consider whether `card-data.json` should contain ability data at all (the frontend doesn't execute abilities -- it only displays card info)

### Pitfall 7: SVar System is a General-Purpose Extension Mechanism

**What goes wrong:** SVars (`HashMap<String, String>`) serve multiple purposes in Forge cards: sub-ability definitions referenced by `SubAbility$`/`Execute$`, AI hints (`RemAIDeck`, `AILogic`), picture URLs, conditional values, and arbitrary metadata. The new ability format needs to handle everything SVars do, or the sub-ability chaining system breaks.

**Prevention:**
- Audit all SVar keys used in the 78 Standard cards and categorize: sub-abilities, AI hints, metadata, display values
- The new format should have explicit fields for sub-abilities (nested ability definitions) rather than the indirect SVar reference system -- this eliminates an entire class of "SVar not found" bugs
- AI hints need a separate home: either in the card schema under an `ai_hints` field or in the forge-ai crate's configuration
- The current `resolve_ability_chain()` function (`effects/mod.rs:105-159`) traverses SVars by name to find sub-abilities. Nested ability definitions would make this traversal unnecessary.

### Pitfall 8: Test Suite Accidentally Uses XMage Patterns Instead of Forge.rs Patterns

**What goes wrong:** XMage is MIT-licensed with extensive card tests, making it an attractive reference. However, XMage tests are Java/JUnit with XMage-specific helper classes (`CardTestPlayerBase`, `addCard()`, `setChoice()`). Copying XMage test patterns verbatim creates tests that don't match Forge.rs's architecture (Rust `#[test]`, `GameState::new_two_player()`, `create_object()`).

**Prevention:**
- XMage's MIT license permits using their tests as reference for SCENARIOS (what game state to set up, what outcome to expect), not for code structure
- Write tests using Forge.rs idioms: `#[test]`, `GameState::new_two_player(seed)`, `create_object()`, direct state assertion
- XMage test organization by mechanic (`abilities/`, `triggers/`, `damage/`, `protection/`) is a good model for Rust test module organization
- Each test should reference the specific MTG Comprehensive Rule being tested, not the XMage test it was inspired by
- Focus on the MTG Comprehensive Rules as the specification document, not any other engine's implementation

### Pitfall 9: MTGJSON Card Name Matching Breaks for Multi-Face Cards

**What goes wrong:** MTGJSON uses `//` delimiters for multi-face card names (e.g., "Bonecrusher Giant // Stomp") while Forge uses separate face entries. The current engine indexes by primary face name (lowercase). MTGJSON's `faceName` field provides individual face names, but the main `name` field is the combined name. The `side` field ("a" or "b") identifies which face.

**Prevention:**
- Use `faceName` (not `name`) when matching MTGJSON data to engine cards
- MTGJSON's `otherFaceIds` field links faces via UUID -- use for reliable cross-referencing
- Test name matching for all multi-face types present in Standard: Adventure (Lovestruck Beast // Heart's Desire), Transform, Modal DFC
- The engine's `CardLayout` enum (Single, Split, Transform, Adventure, Modal, etc.) must map correctly to MTGJSON's `layout` field ("normal", "split", "transform", "adventure", "modal_dfc", etc.) -- different string values for the same concept

### Pitfall 10: Database Module Assumes Filesystem Loading of .txt Files

**What goes wrong:** `CardDatabase::load()` uses `walkdir` to traverse directories of `.txt` files, calls `parse_card_file()` on each, and builds HashMap indexes. The new system loads from JSON. Simply changing the file extension is not enough -- the entire load path assumes Forge `.txt` parsing.

**Prevention:**
- Create `CardDatabase::from_cards(cards: Vec<CardRules>)` constructor that accepts pre-parsed cards from any source
- Create separate loader functions: `load_forge_txt(path)` and `load_json(path)` that both produce `Vec<CardRules>`
- The `CardDatabase` struct itself (HashMap lookup by name, face index) is format-agnostic -- only the `load` function is Forge-specific
- Put `load_forge_txt` behind a `forge-compat` feature flag

### Pitfall 11: Keyword Parameters Lost in Translation

**What goes wrong:** Some keywords have parameters: `Ward:1`, `Protection from red`, `Hexproof from black`, `Cycling {2}`. Forge encodes these in the keyword string (`K:Ward:1`). MTGJSON's `keywords` array only contains keyword names without parameters (["Ward"] not ["Ward:1"]). The engine's `parse_keywords()` function expects the Forge format.

**Prevention:**
- The custom ability format (not MTGJSON) must carry keyword parameters
- Consider two approaches: (a) keep keywords as parameterized strings and preserve `parse_keywords()`, or (b) encode parameterized keywords as typed Keyword enum variants in the JSON schema
- Option (a) is simpler and preserves existing code; option (b) is cleaner but requires more changes
- Test every parameterized keyword in the 78 Standard cards (audit which ones actually appear)

---

## Minor Pitfalls

### Pitfall 12: WASM Binary Size Increase from Embedded Card Data

**What goes wrong:** If the 78 Standard cards' JSON definitions are embedded in the WASM binary via `include_str!`, the binary size increases. The current release profile optimizes for size (`opt-level = 'z'`) and the WASM binary is only 19 KB.

**Prevention:**
- Keep card data as a separate JSON file loaded at runtime (like the current `card-data.json`)
- Don't embed card data in the WASM binary
- Card data should be loaded via JavaScript/TypeScript and passed to the WASM init function

### Pitfall 13: Test Helpers Depend on External Forge Directory

**What goes wrong:** `test_helpers.rs` loads from `../../forge/forge-gui/res/cardsfolder/` which requires a local Forge checkout. Tests using these helpers silently return `None` when the directory is absent (CI-safe but not CI-tested). The new test infrastructure must be self-contained.

**Prevention:**
- New test infrastructure must use embedded test data checked into the repo (the 78 cards in `data/standard-cards/` or their JSON equivalents)
- Tests MUST NOT silently skip -- a test that returns early without asserting anything is worse than a failing test
- Replace `OnceLock<Option<CardDatabase>>` pattern with `OnceLock<CardDatabase>` that loads from the repo's own data directory
- CI must actually exercise card-specific tests, not just compile them

### Pitfall 14: Forge Parser Retained as Dev Tool But Gradually Bitrot

**What goes wrong:** If the Forge parser is kept as an optional dev tool but the canonical format is now custom JSON, the parser will gradually diverge as Rust types evolve. It becomes dead code.

**Prevention:**
- Put behind a Cargo feature flag (`forge-compat`) from day one
- Accept that it will bitrot and set a definite removal milestone (e.g., remove in v1.3)
- Do not maintain two parsers indefinitely

---

## Phase-Specific Warnings

| Phase Topic | Likely Pitfall | Mitigation |
|-------------|---------------|------------|
| Runtime Type Migration | `GameObject.abilities` stores raw Forge strings parsed at 20+ runtime sites (#1) | Convert to `Vec<AbilityDefinition>` on load; eliminate all runtime `parse_ability()` calls |
| Schema Design | Schema can't express all 38 effects + SVar chains (#2) | Schema = JSON serialization of existing Rust types; audit all param keys in 78 cards first |
| MTGJSON Integration | Expecting ability data from MTGJSON (#3) | MTGJSON is metadata only; ability data must come from custom format |
| MTGJSON Integration | Multi-face card name mismatch (#9) | Use `faceName` + `otherFaceIds`, not combined `name` field |
| MTGJSON Integration | Keyword parameters not in MTGJSON (#11) | Custom format must carry parameterized keywords |
| Card Data Migration | Mechanical conversion from Forge `.txt` creates GPL derivative (#4) | Author from game rules + oracle text; use Forge output only for validation comparison |
| Card Data Migration | Regression in 78 cards (#5) | Build test infrastructure BEFORE migration; A/B comparison tests |
| Card Data Migration | SVar system not fully replaced (#7) | Audit all SVar keys; use nested ability definitions instead of SVar references |
| Test Infrastructure | Tests skip silently when data unavailable (#13) | Self-contained embedded test data; no silent skips |
| Test Infrastructure | XMage patterns don't match Forge.rs architecture (#8) | Reference XMage scenarios but implement in Rust idioms; cite Comprehensive Rules |
| License Change | Forge content remains in repo (#4) | Delete `data/cardsfolder/`; check git history; feature-flag parser |
| Frontend Update | `card-data.json` format changes break TypeScript (#6) | Update TS types in same PR as Rust changes |
| Database Module | `CardDatabase::load()` assumes `.txt` files (#10) | Create format-agnostic constructor; feature-flag `.txt` loader |
| WASM Build | Embedded card data inflates binary (#12) | Load card data at runtime via JS, not baked into WASM |

---

## Sources

**Codebase analysis (HIGH confidence):**
- `crates/engine/src/game/effects/mod.rs` -- 38 effect handlers, `build_registry()`, `resolve_ability_chain()` with runtime `parse_ability()` calls
- `crates/engine/src/types/triggers.rs` -- 137+ trigger modes
- `crates/engine/src/game/static_abilities.rs` -- 61+ static ability modes (37 registered + 30 stubs)
- `crates/engine/src/game/game_object.rs` -- `abilities: Vec<String>` storing raw Forge notation on runtime objects
- `crates/engine/src/game/casting.rs:60, 208` -- runtime `parse_ability()` at cast time
- `crates/engine/src/game/mana_abilities.rs:15, 43` -- runtime parsing for mana ability detection
- `crates/engine/src/game/planeswalker.rs:170` -- runtime parsing for loyalty activation
- `crates/engine/src/game/triggers.rs:176` -- runtime parsing during trigger resolution
- `crates/engine/src/game/deck_loading.rs:132-146` -- parsing triggers/statics/replacements at load time
- `crates/engine/src/game/coverage.rs` -- coverage check limitations (handler presence, not correctness)
- `crates/engine/src/game/test_helpers.rs` -- external Forge directory dependency
- `crates/engine/src/database/card_db.rs` -- filesystem/walkdir/.txt coupling
- `crates/engine/src/types/ability.rs` -- `AbilityDefinition`, `TriggerDefinition`, `StaticDefinition`, `ReplacementDefinition` as clean intermediate types
- `data/standard-cards/` -- 78 curated Standard cards in Forge `.txt` format

**External research (MEDIUM confidence):**
- [MTGJSON Card (Atomic) Data Model](https://mtgjson.com/data-models/card/card-atomic/) -- confirms no ability definition fields, only oracle text + keyword list
- [MTGJSON Card (Set) Data Model](https://mtgjson.com/data-models/card/card-set/) -- layout, side, otherFaceIds, faceName fields
- [MTGJSON License](https://mtgjson.com/license/) -- MIT license confirmed
- [XMage GitHub Repository](https://github.com/magefree/mage) -- MIT license confirmed; test organization by mechanic category
- [XMage Testing Tools Wiki](https://github.com/magefree/mage/wiki/Development-Testing-Tools) -- test structure and practices
- [Forge GitHub Repository](https://github.com/Card-Forge/forge) -- GPL license confirmed

**Legal analysis (LOW confidence -- not legal advice):**
- [ABA: Limited Copyright Protection for Playing Cards](https://www.americanbar.org/groups/intellectual_property_law/publications/landslide/2020-21/january-february/limited-copyright-protection-playing-cards/) -- merger doctrine for game rules
- [GNU GPL FAQ](https://www.gnu.org/licenses/gpl-faq.en.html) -- derivative work definition
- [Copyleft Guide: Derivative Works](https://copyleft.org/guide/comprehensive-gpl-guidech5.html) -- statute and case law analysis
