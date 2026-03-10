# Phase 23: Unified Card Loader - Context

**Gathered:** 2026-03-10
**Status:** Ready for planning

<domain>
## Phase Boundary

Wire MTGJSON metadata + ability JSON into CardDatabase as the primary card loading path, proven end-to-end with sample cards. Produces `CardDatabase::load_json()` that merges both data sources into valid `CardFace`/`CardRules`, synthesizes implicit abilities per MTG rules, threads `scryfall_oracle_id` through to the frontend, and passes a smoke test game with 5-10 JSON-loaded cards.

</domain>

<decisions>
## Implementation Decisions

### Implicit Ability Synthesis
- Synthesized during `CardDatabase::load_json()` — cards arrive fully formed
- Basic lands: synthesize mana abilities per CR 305.6 (e.g., Forest → `{T}: Add {G}`)
- Equipment: synthesize Equip activated ability from `K:Equip:N` keyword per CR 702.6
- Planeswalkers: wire loyalty costs through `AbilityCost::Loyalty` instead of `remaining_params["PW_Cost"]`
- MTG Comprehensive Rules are the source of truth for all implicit ability behavior

### Multi-face Card Handling
- One ability JSON file per card with a `faces` array matching MTGJSON's side a/b ordering
- File named by primary card name (e.g., `delver_of_secrets.json` covers both Delver and Insectile Aberration)
- `CardLayout` variant determined from MTGJSON's `layout` field — single source of truth

### Scryfall Image Integration
- Add `scryfall_oracle_id: Option<String>` to `CardFace`
- Populated from MTGJSON `identifiers.scryfallOracleId` during JSON loading
- Serializes across WASM boundary via serde — frontend can use for direct Scryfall lookups
- Satisfies success criterion #3 and requirement MIGR-04

### Missing Ability File Handling
- `load_json()` always collects missing ability files as errors (same pattern as Forge loader's `errors()` method)
- Even during Phase 23 when most cards lack files — early visibility prevents forgotten validation
- Loader takes a card name list and reports which cards lack ability JSON files

### Cross-Validation Strategy
- Test-time only: a cross-validation test checks that every card with an ability JSON file matches its MTGJSON entry
- Catches missing/incomplete card definitions at `cargo test` time, not runtime
- No runtime validation overhead

### Smoke Test Card Selection
- Claude selects 5-10 cards from curated Standard set covering all archetypes:
  - Vanilla creature (no abilities)
  - Instant/sorcery (spell effects)
  - Multi-face card (Adventure, Transform, or MDFC)
  - Planeswalker (loyalty abilities)
  - Equipment (Equip keyword → synthesized ability)
  - Basic land (synthesized mana ability)
  - Rancor (Aura attachment + triggered return-to-hand — user-specified)
- Smoke test runs a game using only JSON-loaded cards — cast, resolve, combat

### Claude's Discretion
- Exact smoke test card selection (beyond Rancor) covering the required archetypes
- Internal implementation of the merge logic (how MTGJSON metadata + ability JSON combine into CardFace fields)
- How load_json() discovers ability files (directory scan vs. explicit name mapping)
- Whether to update the existing Forge-path `CardDatabase::load()` or keep it entirely separate
- Frontend changes (if any) to use scryfall_oracle_id for image lookups

</decisions>

<specifics>
## Specific Ideas

- "Make the architecture clean as fuck" carries forward — idiomatic Rust, maximum type safety
- MTG Comprehensive Rules are paramount — if rules dictate behavior, that's the implementation
- Rancor specifically requested for smoke test — exercises Aura + triggered ability synthesis
- Prioritize clean architecture over clever solutions — this loader will be used by Phase 24's migration tool

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `CardDatabase` (`database/card_db.rs`): Existing Forge loader with `cards: HashMap<String, CardRules>`, `face_index`, `errors` pattern — new `load_json()` parallels this structure
- `mtgjson.rs`: `AtomicCardsFile`, `AtomicCard`, `load_atomic_cards()`, `find_card()`, `parse_mtgjson_mana_cost()` — all ready to use
- `lightning_bolt.json`: Working ability JSON example with typed `Effect::DealDamage` — template for new cards
- `schema.json`: JSON Schema for ability files — validates new hand-authored cards
- `CardLayout` enum: Already handles Single, Split, Flip, Transform, Meld, Adventure, Modal, Omen, Specialize
- `layout_faces()` helper in `card_db.rs`: Extracts face references from any CardLayout variant

### Established Patterns
- `AbilityDefinition` with typed `Effect` + `remaining_params` compat bridge (Phase 21)
- `api_type()` and `params()` compat methods still used by effect dispatch — JSON-loaded cards must produce compatible values
- Serde `#[serde(tag = "type", content = "data")]` for discriminated unions
- `CardFace` has `Vec<AbilityDefinition>`, `Vec<TriggerDefinition>`, `Vec<StaticDefinition>`, `Vec<ReplacementDefinition>` — typed from Phase 21

### Integration Points
- `CardDatabase` module (`database/mod.rs`): Add `load_json()` as new public method
- `CardFace` struct (`types/card.rs`): Add `scryfall_oracle_id: Option<String>` field
- `server-core/deck_resolve.rs`: Uses `CardDatabase` — must work with JSON-loaded cards too
- `coverage.rs`: Uses `CardDatabase` + `has_unimplemented_mechanics()` — Phase 25 updates this to JSON format
- Frontend `scryfall.ts`: Currently name-based lookups — oracle ID enables `/cards/:id` path
- `engine-wasm`: CardDatabase not directly used in WASM (game state carries cards), but CardFace serializes across boundary

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 23-unified-card-loader*
*Context gathered: 2026-03-10*
