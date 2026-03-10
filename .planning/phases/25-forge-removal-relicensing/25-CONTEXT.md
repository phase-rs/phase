# Phase 25: Forge Removal & Relicensing - Context

**Gathered:** 2026-03-10
**Status:** Ready for planning

<domain>
## Phase Boundary

Remove all GPL-licensed Forge data from the repository, feature-gate the Forge parser and all Forge compatibility code behind `forge-compat`, refactor engine dispatch from string-based to typed pattern matching, scrub all Forge references from ungated code, and relicense the project as MIT/Apache-2.0 dual license.

</domain>

<decisions>
## Implementation Decisions

### Feature Gate Boundary
- Feature gate named `forge-compat` in engine crate's Cargo.toml
- **Gated behind `forge-compat`:**
  - Parser modules (`parser/*.rs`) — Forge .txt card definition parser
  - `migrate.rs` binary — migration tool (already served its purpose)
  - `CardDatabase::load()` — Forge loading path
  - Compat bridge methods on typed enums: `api_type()`, `params()`, `mode_str()`, `event_str()` on Effect, TriggerMode, StaticMode, ReplacementEvent, and AbilityDefinition
- **Deleted entirely:**
  - `parity.rs` integration tests — migration validated, no longer needed
- **Not gated (always compiled):**
  - `CardDatabase::load_json()` — sole default loading path
  - Typed enums themselves (Effect, TriggerMode, etc.) — these are the primary API
  - All game logic, dispatch, and handler code (refactored to typed pattern matching)

### Dispatch Refactoring
- All effect/trigger/static/replacement dispatch refactored from string-based HashMap lookup to typed pattern matching on enums
- This removes the dependency on compat bridge methods (`api_type()`, `params()`) from ungated code
- ~13 handler registration sites migrated to `match` on typed Effect/TriggerMode/StaticMode/ReplacementEvent variants
- Included in Phase 25 as the natural completion of the typed enum migration from Phase 21

### License File Structure
- Two separate files: `LICENSE-MIT` and `LICENSE-APACHE` (Rust ecosystem convention)
- Copyright holder: "phase.rs contributors"
- `NOTICE` file for MTGJSON MIT license attribution (Apache-2.0 convention for third-party notices)
- `license = "MIT OR Apache-2.0"` in all Cargo.toml files (workspace root + all crates)
- No per-file SPDX license headers — Cargo.toml + LICENSE files sufficient

### Forge Reference Cleanup
- Full scrub of ALL Forge references from ungated code (comments, doc strings, test names, method names)
- Project documentation (CLAUDE.md, PROJECT.md) scrubbed — project is phase.rs, not a Forge derivative
- Brief origin mention acceptable ("originally inspired by open-source MTG projects") but no "Forge" by name in ungated code/docs
- Code behind `forge-compat` feature gate left as-is — Forge references are appropriate in the Forge compatibility layer

### Data Directory Transition
- Single atomic commit: `git rm -r data/standard-cards/` + .gitignore update
- `.gitignore` change: remove `!data/standard-cards/` exemption
- Keep tracked: `!data/standard-cards.txt` (manifest), `!data/mtgjson/`, `!data/abilities/`
- `data/cardsfolder/` already gitignored via `data/*` glob — no action needed (126MB exists locally only)
- Git history rewrite explicitly out of scope (REQUIREMENTS.md decision)

### Claude's Discretion
- Exact dispatch refactoring approach (whether to convert all ~13 sites at once or incrementally)
- Order of operations between feature gating, data deletion, and license file creation
- How to handle any remaining `remaining_params` usage in ungated code after compat methods are gated
- Coverage report binary adaptations for JSON-only world
- PROJECT.md key decisions table updates

</decisions>

<specifics>
## Specific Ideas

- "Make the architecture clean as fuck" carries forward — this is the final cleanup that makes the project fully independent
- Full scrub means phase.rs stands on its own — no heritage debt in ungated code
- The dispatch refactoring is the culmination of the typed enum work from Phase 21 — string dispatch was always transitional
- `forge-compat` gate name is self-documenting: anyone enabling it knows exactly what they're opting into

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- Typed enums (Effect, TriggerMode, StaticMode, ReplacementEvent) — already fully defined with ~200+ variants, ready for pattern matching
- `CardDatabase::load_json()` — already the primary JSON loading path, tested end-to-end
- `coverage_report.rs` — already reads `standard-cards.txt` manifest (decoupled from Forge directory structure)
- `data/abilities/` — 32,300+ migrated JSON files, the sole card data source post-Phase 25

### Established Patterns
- `Effect::Other { api_type, params }` fallback for unmapped effects — stays as-is (no compat method needed for this variant)
- `#[cfg(feature = "...")]` — Rust's standard conditional compilation, but NO existing `[features]` section in Cargo.toml — needs creation from scratch
- Crate names already rebranded: `phase-ai`, `phase-server` (not `forge-*`)

### Integration Points
- `crates/engine/Cargo.toml` — needs `[features]` section with `forge-compat` gate
- `game/effects/mod.rs` — effect handler registry, primary dispatch refactoring target
- `game/triggers.rs` — trigger handler registry
- `game/static_abilities.rs` — static ability evaluation
- `game/replacement.rs` — replacement effect registry
- `types/ability.rs` — compat bridge methods to gate
- `database/card_db.rs` — `load()` method to gate
- `bin/migrate.rs` — binary to gate
- `tests/parity.rs` — to delete
- Root `Cargo.toml` — license field
- All crate `Cargo.toml` files — license field

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 25-forge-removal-relicensing*
*Context gathered: 2026-03-10*
