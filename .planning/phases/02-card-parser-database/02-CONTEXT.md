# Phase 2: Card Parser & Database - Context

**Gathered:** 2026-03-07
**Status:** Ready for planning

<domain>
## Phase Boundary

Parse Forge's 32,300+ `.txt` card definition files into typed Rust structures, index them by name (and by individual face name for multi-face cards), and support all multi-face card types. Ability strings are structurally parsed (category, ApiType, parameters) but not semantically interpreted — that's Phase 4.

</domain>

<decisions>
## Implementation Decisions

### Parser Architecture
- Lenient parsing: unknown/unrecognized keys are silently skipped (matching Forge's switch fallthrough behavior)
- Single parse function with `match` on first character of key, then exact key match — direct Rust translation of Forge's `CardRules.Reader.parseLine`
- Card files use Forge's exact directory layout: `cardsfolder/{first_letter}/{card_name}.txt` with configurable root path
- Lines starting with `#` or empty lines are skipped (matching Forge)
- Line format: split on first `:` to get key and value

### Multi-Face Card Model
- `CardLayout` enum with face data as discriminated union: `Single(CardFace)`, `Split(CardFace, CardFace)`, `Transform(CardFace, CardFace)`, `Adventure(CardFace, CardFace)`, `Flip(CardFace, CardFace)`, `Meld(CardFace, CardFace)`, `Modal(CardFace, CardFace)`, `Specialize(CardFace, Vec<CardFace>)`
- Specialize included as a stub (parses but no runtime behavior) — Alchemy is out of scope per PROJECT.md but parser should handle all Forge card files
- `ALTERNATE` delimiter switches face context during parsing (matching Forge's `curFace` toggle)
- `AlternateMode` field maps to `CardLayout` variant (with `DoubleFaced` → `Transform` alias matching Forge's `smartValueOf`)

### CardFace Fields
- All Forge ICardFace fields: name, mana_cost (parsed ManaCost), card_type (parsed CardType), pt, loyalty, defense, oracle_text, non_ability_text, flavor_name
- Lists: abilities (A:), triggers (T:), static_abilities (S:), replacement_effects (R:), keywords (K:)
- SVars stored as `HashMap<String, String>` — raw values, resolution deferred to Phase 4 (ABIL-02)
- ManaCost and CardType as proper parsed Rust types with their own sub-parsers (PARSE-04 requirement)
- ManaCost handles: colored, generic, hybrid, phyrexian, X costs, snow, "no cost"
- CardType separates supertypes, card types, and subtypes

### Card Database & Loading
- Eager loading of all card files at startup (matching Forge's approach) — 32k small text files parse fast in Rust
- Primary index: `HashMap<String, CardRules>` with lowercased name keys for case-insensitive O(1) lookup
- Face-level index: separate `HashMap<String, CardFace>` for individual face lookup (e.g., search "Stomp" finds "Bonecrusher Giant // Stomp")
- Target: sub-10ms single-card lookup (easily met with HashMap)

### Ability String Parsing (ABIL-01 Scope)
- Parse the pipe-delimited `Key$ Value` format into a typed `AbilityDefinition` struct
- Typed `AbilityKind` enum: `Spell` (SP$), `Activated` (AB$), `Database` (DB$) — matching Forge's distinction
- Extract ApiType (DealDamage, Draw, ChangeZone, etc.) as a string field
- Store all `Key$Value` parameters as `HashMap<String, String>`
- Triggers (T:) and statics (S:) use `Mode$` as their type discriminator
- Replacement effects (R:) use `Event$` as their type discriminator
- No semantic interpretation of what effects do — that's Phase 4

### Testing Strategy
- Port Forge's parser and database tests to Rust (CardDb performance tests, card parsing correctness)
- Test against real Forge card files (Lightning Bolt, multi-face cards, edge cases)
- UI/mock-related card tests deferred to later phases (Phase 7)
- Forge parity is the guiding principle: match Forge's behavior, port their tests

### Claude's Discretion
- Internal error types and Result patterns for parse failures
- Exact module organization within the `engine` crate (parser module, database module)
- Whether CardRules wraps CardLayout or is the same struct
- Performance optimization details (parallel file loading, memory layout)

</decisions>

<specifics>
## Specific Ideas

- "We should be looking at what is the most idiomatic path that strictly aligns with clean architecture and looking at what Forge does" — Forge parity is the north star, improved only where Rust offers genuinely better patterns (e.g., discriminated unions instead of nullable arrays)
- Forge's `CardRules.Reader` is the reference implementation — the Rust parser should be a direct conceptual translation
- Forge's `CardSplitType` → Rust `CardLayout` enum is the canonical example of "same behavior, better types"
- Port Forge's unit tests precisely — we want parity

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `CardDefinition` stub in `crates/engine/src/types/card.rs` — will be replaced with full `CardFace`/`CardRules` structures
- `ManaColor` enum and `ManaPool` in `crates/engine/src/types/mana.rs` — can be extended for ManaCost parsing
- `serde` already in workspace dependencies — all new types get Serialize/Deserialize

### Established Patterns
- Cargo workspace with `crates/engine` and `crates/engine-wasm` — parser goes in `engine` crate
- Types module at `crates/engine/src/types/` — new card types extend this module
- All types derive `Debug, Clone, Serialize, Deserialize`

### Integration Points
- `crates/engine/src/types/mod.rs` re-exports all types — new card types added here
- `crates/engine-wasm` wraps engine types with tsify for TypeScript — card types will need WASM bindings eventually (Phase 7)
- Forge card files at `../forge/forge-gui/res/cardsfolder/` — available for testing

</code_context>

<deferred>
## Deferred Ideas

- Forge's UI/mock card tests (CardMockTestCase, CardEditionCollectionCardMockTestCase, etc.) — Phase 7 when UI integration happens
- Card edition/set metadata (CardEdition, PaperCard with art preferences) — not needed for Phase 2 card rules parsing
- DeckHints/DeckNeeds/DeckHas parsing — useful for deck builder (Phase 7), parse as raw strings for now
- AI hints (AI:RemoveDeck) — Phase 8 AI implementation

</deferred>

---

*Phase: 02-card-parser-database*
*Context gathered: 2026-03-07*
