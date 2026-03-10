# Phase 24: Card Migration - Context

**Gathered:** 2026-03-10
**Status:** Ready for planning

<domain>
## Phase Boundary

Convert all engine-supported cards from Forge `.txt` format to the new MTGJSON metadata + ability JSON format via an automated migration tool. Validate behavioral parity for the 78 curated Standard cards. Update CI coverage gate to validate JSON-loaded cards alongside the existing Forge gate.

</domain>

<decisions>
## Implementation Decisions

### Migration Tool Design
- Standalone Rust binary at `crates/engine/src/bin/migrate.rs`, invoked via `cargo run --bin migrate`
- Processes all 32,300 Forge card files in `data/cardsfolder/` (not just the 78 Standard cards)
- Reuses existing Forge parser pipeline: `parse_ability()` → `AbilityDefinition` (with typed `Effect` enum via `params_to_effect()`) → serialize to JSON via serde
- Overwrites the 8 existing hand-authored ability JSON files from Phase 23 — ensures consistency across all cards
- Outputs all generated files to `data/abilities/`

### Partial Conversion Handling
- Use `Effect::Other { api_type, params }` for effects/triggers the parser can't map to typed enum variants — engine dispatch already handles this fallback
- Generate ability JSON for ALL 32,300 cards, including those with unsupported mechanics (Unknown keywords, unregistered triggers) — `coverage.rs` still flags them as unsupported
- Validate generated output against MTGJSON oracle text as a heuristic smoke check — log warnings for cards where oracle text suggests abilities the tool didn't capture (advisory, not blocking)
- Summary + detailed log: print summary stats to stdout, write per-card detailed report to `migration-report.json` for drill-down

### Parity Testing Strategy
- Structural comparison: load each of the 78 Standard cards via both paths (Forge `.txt` and JSON), compare resulting `CardFace`/`CardRules` fields (abilities, triggers, statics, keywords, P/T, types, costs)
- All 78 Standard cards tested — these are the CI-gated cards
- For `Effect::Other` fields, compare `api_type` string and `params` HashMap to confirm roundtrip preserves behavior
- Parity tests live in an integration test module: `crates/engine/tests/parity.rs` (alongside Phase 22 rules tests), run via `cargo test --test parity`

### CI Gate Transition
- Add JSON coverage gate alongside existing Forge coverage gate — both must pass during Phase 24
- Phase 25 removes the Forge gate; JSON gate becomes the sole gate
- Introduce explicit Standard card manifest file (e.g., `data/standard-cards.txt`) listing the 78 card names — both gates read from this list, decoupling from the Forge directory structure
- All 32,300 generated ability JSON files committed to repo under `data/abilities/` (~16MB total, small enough for git)

### Claude's Discretion
- Which card list the JSON coverage gate uses (same 78-card list is the natural choice given the explicit manifest)
- Internal migration tool architecture (batch processing order, parallelism, error handling)
- Exact oracle text validation heuristics (which keywords to check, matching strategy)
- Manifest file format (plain text vs JSON)
- Whether to organize `data/abilities/` with subdirectories (a/, b/, c/) or keep flat

</decisions>

<specifics>
## Specific Ideas

- "Make the architecture clean as fuck" carries forward — migration tool should be clean, idiomatic Rust
- Reusing the parser pipeline means the migration tool is essentially a serialization adapter: Forge `.txt` → parser → typed Rust structs → JSON
- Effect::Other is the bridge for cards whose abilities aren't yet fully typed — enables incremental improvement without blocking migration
- Dual coverage gate provides safety net: if JSON-loaded cards differ from Forge-loaded cards, CI catches it before Phase 25 removes the Forge path

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `parser/ability.rs`: `parse_ability()` → `AbilityDefinition` with typed `Effect` via `params_to_effect()` — core conversion logic for the migration tool
- `parser/card_parser.rs`: Parses full Forge `.txt` card files into `CardFace` structs
- `database/json_loader.rs`: `load_json()` merges MTGJSON metadata + ability JSON into `CardRules` — the JSON loading path parity tests validate against
- `database/card_db.rs`: `CardDatabase::load()` for Forge path, `CardDatabase::load_json()` for JSON path — parity tests call both
- `game/coverage.rs`: `analyze_standard_coverage()` + `has_unimplemented_mechanics()` + `is_fully_covered()` — CI gate logic to extend
- `bin/coverage_report.rs`: CI binary that loads cards and reports coverage — needs JSON gate addition
- `schema/mod.rs`: `AbilityFile`, `FaceAbilities` — the JSON serialization types the migration tool writes

### Established Patterns
- `Effect::Other { api_type, params }` fallback for unmapped effects — already used by 8 smoke test cards
- `TriggerMode::Unknown(String)` for unrecognized triggers — same fallback pattern
- `remaining_params` on `AbilityDefinition` preserves unconsumed Forge parser params for compat
- `api_type()` and `params()` compat methods bridge typed enums to string-based dispatch

### Integration Points
- `data/cardsfolder/` (29 subdirectories, 32,300 `.txt` files) — migration tool input
- `data/abilities/` (currently 8 JSON + schema) — migration tool output, will grow to ~32,300 files
- `data/standard-cards/` (78 `.txt` files) — reference for parity tests, will be replaced by manifest
- `data/mtgjson/AtomicCards.json` — MTGJSON metadata, already committed and loaded by `mtgjson.rs`

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 24-card-migration*
*Context gathered: 2026-03-10*
