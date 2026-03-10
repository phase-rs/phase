# Phase 21: Schema & MTGJSON Foundation - Context

**Gathered:** 2026-03-10
**Status:** Ready for planning

<domain>
## Phase Boundary

Define the typed ability JSON schema, refactor the engine to use typed ability structs (replacing Vec<String>), load card metadata from MTGJSON, and validate the schema via schemars. This phase delivers the foundational types and data loading that all subsequent phases build on.

</domain>

<decisions>
## Implementation Decisions

### Ability JSON Schema — Typed Enums
- All four definition types get fully typed Rust enums: `Effect` (abilities), `TriggerMode` (triggers), `StaticMode` (statics), `ReplacementEvent` (replacements)
- Each enum variant has typed fields (e.g., `Effect::DealDamage { damage: i32, target: TargetSpec }`) — no `HashMap<String, String>` params
- JSON deserializes directly into typed structs via serde — no Forge string intermediary
- Sub-ability chaining and SVar-equivalent behavior handled via the typed enum structure (implementation detail for researcher/planner)

### Vec<AbilityDefinition> Refactor (Included in Phase 21)
- `CardFace.abilities: Vec<String>` → `Vec<AbilityDefinition>` (and same for triggers, statics, replacements)
- `GameObject` fields updated to match
- ~13 files that currently parse ability strings at runtime updated to use typed structs directly
- Forge parser updated to produce `AbilityDefinition` directly (already returns it from `parse_ability()`, just needs to store it instead of the raw string)
- This refactor IS the schema work — a schema the engine can't consume is academic

### Licensing — No Forge Format Coupling
- The new JSON format uses our own typed schema, NOT Forge's pipe-delimited string format (`SP$ ... | Param$ Value`)
- Effect handler names ("DealDamage", "Draw", etc.) are descriptive/functional and part of our engine code — not a licensing concern
- Ability definitions authored from oracle text/MTG rules; Forge output used only for validation (GPL contamination avoidance)

### MTGJSON Data Handling
- AtomicCards.json (all cards, ~50MB) committed to repo under `data/mtgjson/`
- MIT-licensed, always available for `cargo test`, no network dependency
- Custom Rust types for MTGJSON deserialization (~50 lines, no mtgjson crate dependency)
- Two-file merge model: MTGJSON provides metadata (name, cost, types, P/T, colors, oracle text, scryfallOracleId), ability JSON provides behavior (effects, triggers, statics, replacements), merged into CardFace at load time

### Card File Organization
- One ability JSON file per card: `data/abilities/lightning_bolt.json`
- Snake_case filenames derived from card name
- Typed enums provide structural sharing at the type level; per-card files are self-contained
- Directory structure: `data/mtgjson/AtomicCards.json` + `data/abilities/*.json`

### Schema Validation
- `schemars` crate with `#[derive(JsonSchema)]` on all ability/trigger/static/replacement types
- Schema auto-generated from Rust types — always in sync
- `insta` snapshot test ensures schema hasn't changed unexpectedly
- Validation at test time only (`cargo test` loads all ability JSONs and validates against schema)
- No runtime validation overhead; no build.rs complexity
- Schema file committed to repo at `data/abilities/schema.json` with `$schema` reference in ability files for editor autocompletion

### Claude's Discretion
- Exact field names and enum variant naming conventions
- Cost representation type design (reuse existing `ManaCost` or new type)
- Target specification type design
- Sub-ability chaining representation (inline nesting vs. named references)
- MTGJSON field mapping details (which fields are essential vs. optional)
- Multi-face card handling in ability JSON (single file with multiple faces vs. separate treatment)

</decisions>

<specifics>
## Specific Ideas

- "Make the architecture clean as fuck" — user explicitly wants maximum type safety and idiomatic Rust
- Typed Effect enum chosen over HashMap<String, String> despite 200+ variants — AI can generate these
- The refactor that was originally deferred ("Vec<AbilityDefinition> to avoid touching ~13 files") is now included because emitting Forge-compatible strings perpetuates licensing risk

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `AbilityDefinition`, `TriggerDefinition`, `StaticDefinition`, `ReplacementDefinition` — already exist with serde derives, need typed enum fields replacing `HashMap<String, String>`
- `ManaCost`, `CardType`, `Zone`, `Keyword`, `ManaColor` — existing typed enums/structs reusable in the new ability schema
- `CardLayout` enum — already handles all multi-face types (Single, Split, Flip, Transform, Meld, Adventure, Modal, Omen, Specialize)
- `ResolvedAbility` — has `sub_ability: Option<Box<ResolvedAbility>>` for chaining, pattern to follow

### Established Patterns
- Serde `#[serde(tag = "type", content = "data")]` for discriminated unions across WASM boundary
- `fn pointer` effect/trigger/static registries built per `apply()` call — dispatch by string key currently, will change to pattern match on typed enum
- Effect handlers in `game/effects/mod.rs` register ~30+ effect types — each handler will get typed params instead of HashMap extraction

### Integration Points
- `CardDatabase::load()` — needs new `load_json()` method for MTGJSON + ability JSON path
- `CardFace` struct — fields change from `Vec<String>` to typed definition vectors
- `GameObject` struct — same field changes, ~13 files reference these
- `parser/ability.rs` — `parse_ability()` already returns `AbilityDefinition`, needs to populate typed Effect enum for Forge compat path
- Effect handler signatures — currently take `ResolvedAbility` with HashMap params, will take typed params

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 21-schema-mtgjson-foundation*
*Context gathered: 2026-03-10*
