# Phase 25: Forge Removal & Relicensing - Research

**Researched:** 2026-03-10
**Domain:** Rust feature gating, license management, dispatch refactoring, codebase scrubbing
**Confidence:** HIGH

## Summary

Phase 25 is the culmination of the v1.2 milestone: removing all GPL-licensed Forge data from the repository, feature-gating the Forge parser and compat bridge code behind `forge-compat`, refactoring all remaining string-based dispatch to typed pattern matching, and relicensing the project as MIT/Apache-2.0. The codebase has been thoroughly prepared by Phases 21-24: typed enums exist for all four definition types (Effect, TriggerMode, StaticMode, ReplacementEvent), JSON loading works end-to-end, and the CI coverage gate already validates against JSON data.

The primary technical challenge is the dispatch refactoring -- converting ~13 sites that use `api_type()`, `mode_str()`, `event_str()`, and `params()` compat bridge methods into typed pattern matching on enums. The effects module (`game/effects/mod.rs`) is already fully converted. The remaining sites are: static_abilities.rs (2 call sites using `mode_str()`), replacement.rs (2 call sites using `event_str()`), combat.rs (2 using `mode_str()`), layers.rs (1 using `mode_str()`), counter.rs (1 using `mode_str()`), coverage.rs (4+ using `api_type()`/`mode_str()`), mana_abilities.rs (1 using `api_type()`), casting.rs (2 using `params()`), planeswalker.rs (1 using `params()`), and various effect handlers (20+ using `api_type()` for GameEvent::EffectResolved). The data deletion is straightforward (78 tracked .txt files in data/standard-cards/), and the license file creation follows Rust ecosystem conventions exactly.

**Primary recommendation:** Execute in three waves: (1) dispatch refactoring + feature gating, (2) data deletion + coverage binary adaptation + CI update, (3) license files + documentation scrub.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Feature gate named `forge-compat` in engine crate's Cargo.toml
- **Gated behind `forge-compat`:**
  - Parser modules (`parser/*.rs`) -- Forge .txt card definition parser
  - `migrate.rs` binary -- migration tool (already served its purpose)
  - `CardDatabase::load()` -- Forge loading path
  - Compat bridge methods on typed enums: `api_type()`, `params()`, `mode_str()`, `event_str()` on Effect, TriggerMode, StaticMode, ReplacementEvent, and AbilityDefinition
- **Deleted entirely:**
  - `parity.rs` integration tests -- migration validated, no longer needed
- **Not gated (always compiled):**
  - `CardDatabase::load_json()` -- sole default loading path
  - Typed enums themselves (Effect, TriggerMode, etc.) -- these are the primary API
  - All game logic, dispatch, and handler code (refactored to typed pattern matching)
- All effect/trigger/static/replacement dispatch refactored from string-based HashMap lookup to typed pattern matching on enums
- ~13 handler registration sites migrated to `match` on typed variants
- Two separate files: `LICENSE-MIT` and `LICENSE-APACHE` (Rust ecosystem convention)
- Copyright holder: "phase.rs contributors"
- `NOTICE` file for MTGJSON MIT license attribution
- `license = "MIT OR Apache-2.0"` in all Cargo.toml files (workspace root + all crates)
- No per-file SPDX license headers
- Full scrub of ALL Forge references from ungated code
- Single atomic commit: `git rm -r data/standard-cards/` + .gitignore update
- `.gitignore` change: remove `!data/standard-cards/` exemption
- Keep tracked: `!data/standard-cards.txt` (manifest), `!data/mtgjson/`, `!data/abilities/`
- Git history rewrite explicitly out of scope

### Claude's Discretion
- Exact dispatch refactoring approach (whether to convert all ~13 sites at once or incrementally)
- Order of operations between feature gating, data deletion, and license file creation
- How to handle any remaining `remaining_params` usage in ungated code after compat methods are gated
- Coverage report binary adaptations for JSON-only world
- PROJECT.md key decisions table updates

### Deferred Ideas (OUT OF SCOPE)
None -- discussion stayed within phase scope
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| MIGR-02 | data/cardsfolder/ and data/standard-cards/ are removed from the repository; Forge parser is feature-gated behind forge-compat | Feature gating pattern documented; 78 tracked files in data/standard-cards/ identified; `required-features` for migrate binary; `#[cfg(feature)]` for parser/load modules |
| LICN-01 | Project relicensed as MIT/Apache-2.0 dual license after all GPL-coupled data is removed | LICENSE-MIT + LICENSE-APACHE file structure; Cargo.toml `license` field syntax; NOTICE file for MTGJSON attribution; 7 Cargo.toml files to update |
| LICN-02 | PROJECT.md constraints and key decisions updated to reflect MTGJSON + own ability format (removing Forge format dependency) | Specific PROJECT.md sections identified: Constraints, Key Decisions table, Active requirements, Context paragraph |
| LICN-03 | Coverage report (coverage.rs) reads JSON format and CI gate (100% Standard coverage) is preserved | coverage_report.rs dual-mode structure mapped; coverage.rs `api_type()`/`mode_str()` calls identified; CI workflow steps documented |
</phase_requirements>

## Standard Stack

### Core
| Library/Tool | Version | Purpose | Why Standard |
|-------------|---------|---------|--------------|
| Rust `cfg(feature)` | stable | Conditional compilation for forge-compat gate | Rust's built-in feature system, zero-cost at compile time |
| Cargo `required-features` | stable | Gate `migrate` binary behind forge-compat | Standard Cargo manifest field, skips binary when feature disabled |
| `license = "MIT OR Apache-2.0"` | SPDX 2.1 | Cargo.toml license field | Rust ecosystem standard (matches rust-lang/rust itself) |

### Supporting
| Tool | Purpose | When to Use |
|------|---------|-------------|
| `git rm -r` | Remove tracked data/standard-cards/ directory | Single atomic commit for data deletion |
| `cargo test --all` | Verify all tests pass without forge-compat | Post-refactoring validation |
| `cargo build --target wasm32-unknown-unknown` | Verify WASM build succeeds without forge-compat | Success criterion #5 |
| `cargo clippy --all-targets -- -D warnings` | Verify no dead code warnings from feature gating | Post-refactoring lint check |

## Architecture Patterns

### Feature Gate Structure in engine/Cargo.toml

```toml
[features]
forge-compat = []

[[bin]]
name = "migrate"
path = "src/bin/migrate.rs"
required-features = ["forge-compat"]
```

Source: [Cargo Targets documentation](https://doc.rust-lang.org/cargo/reference/cargo-targets.html), [Features documentation](https://doc.rust-lang.org/cargo/reference/features.html)

The `required-features` field on `[[bin]]` entries causes Cargo to skip building the binary entirely when the feature is not enabled. This is cleaner than wrapping the entire binary source in `#[cfg(feature)]`.

### Pattern 1: Module-Level Feature Gating

**What:** Gate entire parser module behind `forge-compat`
**When to use:** When an entire module tree should be excluded

```rust
// In parser/mod.rs
#[cfg(feature = "forge-compat")]
pub mod ability;
#[cfg(feature = "forge-compat")]
pub mod card_parser;
#[cfg(feature = "forge-compat")]
pub mod card_type;
#[cfg(feature = "forge-compat")]
pub mod mana_cost;

#[cfg(feature = "forge-compat")]
pub use card_parser::parse_card_file;

// Keep ParseError always available (used by non-gated code)
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("missing required field: {0}")]
    MissingField(String),
    #[error("missing ability kind (SP$/AB$/DB$)")]
    MissingAbilityKind,
    #[error("invalid mana cost shard: {0}")]
    InvalidManaCostShard(String),
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),
}
```

### Pattern 2: Method-Level Feature Gating

**What:** Gate compat bridge methods on types that remain ungated
**When to use:** Effect, AbilityDefinition, etc. stay compiled but their compat methods are gated

```rust
impl Effect {
    #[cfg(feature = "forge-compat")]
    pub fn api_type(&self) -> &str {
        // ... existing string-returning compat method
    }

    // to_params() is more nuanced -- see analysis below
}
```

### Pattern 3: Replace String Registry with Typed Match

**What:** Convert `HashMap<String, Handler>` to `match` on typed enum variant
**When to use:** static_abilities, replacement, coverage dispatch sites

**Before (string-based):**
```rust
// replacement.rs -- current
if let Some(handler) = registry.get(&repl_def.event_str()) {
    // ...
}
```

**After (typed match):**
```rust
// replacement.rs -- refactored
if let Some(handler) = registry.get(&repl_def.event) {
    // ...
}
// And build_replacement_registry uses ReplacementEvent keys instead of String keys
```

### Anti-Patterns to Avoid
- **Gating too aggressively:** Do NOT gate the typed enums themselves (Effect, TriggerMode, etc.) -- they ARE the primary API. Only gate the compat bridge methods.
- **Forgetting test code:** Tests that call `api_type()`, `mode_str()`, `from_raw()` etc. need to either be refactored or gated. Test code in `#[cfg(test)]` blocks still compiles by default.
- **Breaking GameEvent::EffectResolved:** The `api_type: String` field in `GameEvent::EffectResolved` is used by the frontend for animation/logging. This needs a replacement strategy -- derive from `Effect` variant name directly, not via the compat method.

## Dispatch Refactoring Analysis

### Sites Requiring Refactoring (Production Code)

**Category 1: Registry key type changes**

| File | Current | Target | Complexity |
|------|---------|--------|------------|
| `game/static_abilities.rs:37` | `HashMap<String, StaticAbilityHandler>` | `HashMap<StaticMode, StaticAbilityHandler>` | LOW -- StaticMode already implements Hash+Eq |
| `game/static_abilities.rs:338` | `def.mode_str() != mode` (string compare) | `def.mode != expected_mode` (enum compare) | LOW -- also update `check_static_ability` signature to take `StaticMode` |
| `game/replacement.rs:1034` | `IndexMap<String, ReplacementHandlerEntry>` | `IndexMap<ReplacementEvent, ReplacementHandlerEntry>` | LOW -- ReplacementEvent implements Hash+Eq |
| `game/replacement.rs:1237,1259` | `registry.get(&repl_def.event_str())` | `registry.get(&repl_def.event)` | LOW -- direct field access |
| `game/combat.rs:154,384` | `sd.mode_str() == "CantBeBlocked"` | `sd.mode == StaticMode::Other("CantBeBlocked".to_string())` or define variant | LOW -- but note CantBeBlocked is a `StaticMode::Other` since it's not in the core enum |
| `game/layers.rs:119` | `mode: def.mode_str()` | `mode: def.mode.to_string()` or change field type | LOW -- `ActiveContinuousEffect.mode` field type may change |
| `game/effects/counter.rs:36` | `sd.mode_str() == "CantBeCountered"` | `sd.mode == StaticMode::Other("CantBeCountered".to_string())` | LOW |

**Category 2: `api_type()` in effect handlers (GameEvent::EffectResolved)**

20+ effect handler files emit `GameEvent::EffectResolved { api_type: ability.api_type().to_string(), ... }`. These need a non-compat way to get the effect name string. Options:
- Add a standalone `fn effect_name(effect: &Effect) -> &str` function (not a method on Effect, but ungated utility)
- Use `Effect`'s `Display` impl or `Debug` name
- Use `std::any::type_name` (not suitable)
- **Best approach:** Keep a standalone function or use `Effect::variant_name()` method that is NOT gated (it's useful for logging/events, not a compat bridge)

**Category 3: `params()` in casting/planeswalker/mana_abilities**

| File | Current | Purpose | Refactoring |
|------|---------|---------|-------------|
| `game/casting.rs:96,234` | `ability_def.params()` | Build ResolvedAbility.params HashMap | Medium -- ResolvedAbility still uses params HashMap for SubAbility chain resolution |
| `game/planeswalker.rs:170` | `ability_def.params()` | Build ResolvedAbility.params HashMap | Same as casting |
| `game/mana_abilities.rs:14` | `ability_def.api_type() == "Mana"` | Check if ability is mana ability | LOW -- match on `matches!(ability_def.effect, Effect::Mana { .. })` |
| `game/mana_abilities.rs:63` | `ability_def.params()` | Get Produced/Amount params | Medium -- extract directly from Effect::Mana variant fields |

**Category 4: coverage.rs**

| Call Site | Current | Refactoring |
|-----------|---------|-------------|
| `coverage.rs:49` | `def.api_type()` | Use `is_known_effect` with variant name, or match directly on `def.effect` |
| `coverage.rs:68` | `stat.mode_str()` | `static_registry.contains_key(&stat.mode)` after registry uses StaticMode keys |
| `coverage.rs:110` | `def.api_type()` | Same as line 49 |
| `coverage.rs:158` | `def.api_type()` | Same as line 49 |
| `coverage.rs:202` | `def.mode_str()` | Same as line 68 |

### The `params()` Problem

The `params()` method on `AbilityDefinition` reconstructs a `HashMap<String, String>` from the typed `Effect` fields plus `remaining_params`. This is used in two ways:

1. **ResolvedAbility construction** (casting.rs, planeswalker.rs): `ResolvedAbility.params` is a `HashMap<String, String>` used by the SubAbility/Execute chain in `resolve_ability_chain()` and by condition checking. This is the most complex dependency.

2. **Mana ability resolution** (mana_abilities.rs): Reads `Produced` and `Amount` params.

**Recommended approach for `params()`:** The `to_params()` method on `Effect` is the core converter (Effect -> HashMap). It does NOT depend on compat strings -- it's a legitimate serialization. The compat issue is `AbilityDefinition::params()` which calls `effect.to_params()` plus merges `remaining_params`.

The cleanest path: keep `Effect::to_params()` ungated (it's useful for ResolvedAbility construction), gate only `AbilityDefinition::params()`, and have the callers construct the params HashMap themselves from `effect.to_params()` + `remaining_params`.

### The `api_type` in GameEvent Problem

`GameEvent::EffectResolved { api_type: String }` is emitted by every effect handler. The `api_type` string is consumed by the frontend for animation matching and game log display. This is NOT a Forge compat issue -- it's a legitimate need for a human-readable effect name.

**Recommended approach:** Create a non-gated utility function `pub fn effect_variant_name(effect: &Effect) -> &str` that returns the same strings as `api_type()` but exists independently of the compat bridge. This is a simple rename/extraction -- the function body is identical to the existing `api_type()` match block. Effect handlers then call `effect_variant_name(&ability.effect)` instead of `ability.api_type()`.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Binary gating | `#[cfg(feature)]` on main() | `required-features` in Cargo.toml | Cargo handles it natively, cleaner |
| License file content | Write from scratch | Copy from rust-lang/rust repo | Standard MIT/Apache-2.0 templates, well-tested |
| Forge reference search | Manual grep | `rg -l "forge\|Forge" --type rust` | Comprehensive, less error-prone |

## Common Pitfalls

### Pitfall 1: Orphaned `use` Imports After Feature Gating
**What goes wrong:** Gating `parser` modules creates dead `use crate::parser::*` imports in ungated code, causing compile errors.
**Why it happens:** Multiple files import from `parser::ability::parse_ability` -- particularly `coverage.rs`, `game/effects/mod.rs` (SubAbility chain), and test modules.
**How to avoid:** Search all `use crate::parser` imports. Gate those imports. Verify `cargo build` without the feature.
**Warning signs:** `unresolved import` errors after adding feature gates.

**Known import sites to handle:**
- `game/effects/mod.rs:1` -- `use crate::parser::ability::parse_ability` (used in SubAbility chain -- this is production code, needs alternative or gating of the chain)
- `game/coverage.rs:10` -- `use crate::parser::ability::parse_ability` (used for SVar checking)
- Test files throughout

### Pitfall 2: SubAbility Chain Uses Parser
**What goes wrong:** `resolve_ability_chain()` in `game/effects/mod.rs` calls `parse_ability()` to parse SVar strings. This is production game logic, not just compat code.
**Why it happens:** SVars store sub-ability definitions as Forge-format strings even in the JSON world (the migration preserved these strings).
**How to avoid:** The SubAbility chain's `parse_ability()` usage must remain ungated because it's called during game resolution. The parser module must be split: the `parse_ability` function needs to remain available (or the SVar format needs to change). Alternatively, gate only the card-level parser (`parse_card_file`) and keep `parse_ability` ungated.
**Critical decision:** `parse_ability()` vs `parse_card_file()` -- only `parse_card_file()` should be gated.

### Pitfall 3: Static Registry Uses String Keys for Unregistered Modes
**What goes wrong:** `build_static_registry()` uses `String` keys. Many modes like "CantBeBlocked", "Ward", "Protection" are registered with their Display string. The `StaticMode` enum only has 16 core variants; many modes are `StaticMode::Other("ModeName")`.
**Why it happens:** The StaticMode enum was designed with a limited set of core variants plus `Other(String)` fallback.
**How to avoid:** When converting to `HashMap<StaticMode, Handler>`, construct keys as `StaticMode::Other("CantBeBlocked".to_string())` for non-core variants. Or accept that `StaticMode::from_str("CantBeBlocked")` produces `StaticMode::Other("CantBeBlocked")` and use that. The `Display`/`FromStr` roundtrip already handles this correctly.

### Pitfall 4: CI Workflow Has Both Forge and JSON Coverage Steps
**What goes wrong:** The CI workflow runs BOTH `cargo run --bin coverage-report -- data/standard-cards/ --ci` (Forge) and `cargo run --bin coverage-report -- --json data/ --ci` (JSON). After Phase 25, the Forge step will fail because data/standard-cards/ is deleted.
**Why it happens:** CI was set up with both paths during the transition period.
**How to avoid:** Remove the Forge coverage step from CI and make the JSON step the sole coverage gate. Also, the coverage-report binary's text mode (non-`--json`) calls `CardDatabase::load()` which will be gated -- need to handle this.

### Pitfall 5: coverage_report.rs Binary Has Dual Mode
**What goes wrong:** The `coverage-report` binary supports both text mode (Forge) and `--json` mode. After gating `CardDatabase::load()`, the text mode code path won't compile.
**Why it happens:** The binary was built with both modes during migration.
**How to avoid:** Either (a) gate the text mode behind `forge-compat` and make `--json` the default, or (b) refactor the binary to always use JSON mode (remove text mode entirely since it's no longer needed).
**Recommended:** Remove text mode entirely -- it served its purpose. Make JSON the only mode (drop the `--json` flag, just pass data path).

### Pitfall 6: `from_raw()` Constructor on ResolvedAbility
**What goes wrong:** `ResolvedAbility::from_raw()` creates instances from api_type strings. It's used extensively in tests.
**Why it happens:** It was a transitional constructor for the Phase 21 migration.
**How to avoid:** Gate `from_raw()` behind `forge-compat`. Update test code to use `ResolvedAbility::new()` with typed `Effect` values. This is the correct long-term API anyway.

## Code Examples

### Feature Gate in Cargo.toml
```toml
# crates/engine/Cargo.toml
[features]
forge-compat = []

[[bin]]
name = "migrate"
path = "src/bin/migrate.rs"
required-features = ["forge-compat"]
```

### Gating a Method
```rust
// In types/ability.rs
impl Effect {
    /// Compat bridge: returns Forge api_type string.
    #[cfg(feature = "forge-compat")]
    pub fn api_type(&self) -> &str {
        match self {
            Effect::DealDamage { .. } => "DealDamage",
            // ... all variants
        }
    }
}

/// Non-gated utility: returns human-readable effect variant name.
/// Used for GameEvent::EffectResolved and logging.
pub fn effect_variant_name(effect: &Effect) -> &str {
    match effect {
        Effect::DealDamage { .. } => "DealDamage",
        // ... same match arms
        Effect::Other { api_type, .. } => api_type,
    }
}
```

### Typed Replacement Registry
```rust
// In game/replacement.rs -- refactored
pub fn build_replacement_registry() -> IndexMap<ReplacementEvent, ReplacementHandlerEntry> {
    let mut registry = IndexMap::new();
    registry.insert(ReplacementEvent::DamageDone, ReplacementHandlerEntry {
        matcher: damage_done_matcher,
        applier: damage_done_applier,
    });
    registry.insert(ReplacementEvent::Moved, ReplacementHandlerEntry {
        matcher: moved_matcher,
        applier: moved_applier,
    });
    // ... etc
    registry
}
```

### Typed Static Registry
```rust
// In game/static_abilities.rs -- refactored
pub fn build_static_registry() -> HashMap<StaticMode, StaticAbilityHandler> {
    let mut registry: HashMap<StaticMode, StaticAbilityHandler> = HashMap::new();
    registry.insert(StaticMode::Continuous, handle_continuous);
    registry.insert(StaticMode::CantAttack, handle_rule_mod);
    // Non-core modes use Other variant:
    registry.insert(StaticMode::Other("CantBeBlocked".into()), handle_cant_be_blocked);
    registry.insert(StaticMode::Other("Ward".into()), handle_ward);
    // ... etc
    registry
}
```

### LICENSE-MIT Content
```
MIT License

Copyright (c) 2024-2026 phase.rs contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

### Cargo.toml License Field
```toml
# In workspace root Cargo.toml
[workspace.package]
license = "MIT OR Apache-2.0"

# In each crate Cargo.toml
[package]
license.workspace = true
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| `HashMap<String, Handler>` registries | `HashMap<TypedEnum, Handler>` | Phase 21 (typed enums) -> Phase 25 (typed dispatch) | Eliminates string comparison, enables exhaustive matching |
| `api_type()` compat bridge | `effect_variant_name()` standalone fn | Phase 25 | Decouples from compat layer while preserving GameEvent strings |
| Forge `.txt` + JSON dual loading | JSON-only default | Phase 25 | Clean separation -- Forge code is dev-only behind feature gate |
| GPL implicit (Forge data) | MIT/Apache-2.0 explicit | Phase 25 | Project can be freely distributed and used |

## Files Requiring Changes

### Feature Gating Targets
| File | Change Type | Details |
|------|-------------|---------|
| `crates/engine/Cargo.toml` | Add `[features]` section | New `forge-compat = []` feature + `required-features` on migrate binary |
| `crates/engine/src/parser/mod.rs` | Gate modules | `#[cfg(feature = "forge-compat")]` on ability, card_parser, card_type, mana_cost modules and re-exports |
| `crates/engine/src/database/card_db.rs` | Gate `load()` method | `#[cfg(feature = "forge-compat")]` on the `load()` method |
| `crates/engine/src/types/ability.rs` | Gate compat methods | `api_type()`, `params()` on Effect, AbilityDefinition, ResolvedAbility |
| `crates/engine/src/types/ability.rs` | Gate `from_raw()` | Constructor used in tests only |

### Dispatch Refactoring Targets
| File | Line(s) | Change |
|------|---------|--------|
| `game/static_abilities.rs` | 37-121 | Registry: `String` -> `StaticMode` keys |
| `game/static_abilities.rs` | 338 | `mode_str()` -> direct `mode` enum comparison |
| `game/replacement.rs` | 1034-1114 | Registry: `String` -> `ReplacementEvent` keys |
| `game/replacement.rs` | 1237, 1259 | `event_str()` -> direct `event` field access |
| `game/combat.rs` | 154, 384 | `mode_str() == "X"` -> `mode == StaticMode::Other("X".into())` |
| `game/layers.rs` | 119 | `mode_str()` -> `mode.to_string()` or change struct field |
| `game/effects/counter.rs` | 36 | `mode_str() == "CantBeCountered"` -> typed enum compare |
| `game/mana_abilities.rs` | 14 | `api_type() == "Mana"` -> `matches!(effect, Effect::Mana { .. })` |
| `game/mana_abilities.rs` | 63 | `params()` -> extract from Effect::Mana fields |
| `game/casting.rs` | 96, 234 | `params()` -> `effect.to_params()` + `remaining_params` |
| `game/planeswalker.rs` | 170 | `params()` -> same pattern as casting |
| `game/coverage.rs` | 49, 68, 110, 158, 202 | All compat method calls -> typed alternatives |
| 20+ effect handlers | `api_type()` calls | -> `effect_variant_name()` standalone fn |

### Data + License
| File | Change Type |
|------|-------------|
| `data/standard-cards/` (78 files) | `git rm -r` |
| `.gitignore` | Remove `!data/standard-cards/` line |
| `LICENSE-MIT` | Create new |
| `LICENSE-APACHE` | Create new |
| `NOTICE` | Create new (MTGJSON attribution) |
| `Cargo.toml` (root) | Add `license = "MIT OR Apache-2.0"` to `[workspace.package]` |
| `crates/engine/Cargo.toml` | Add `license.workspace = true` |
| `crates/engine-wasm/Cargo.toml` | Add `license.workspace = true` |
| `crates/phase-ai/Cargo.toml` | Add `license.workspace = true` |
| `crates/server-core/Cargo.toml` | Add `license.workspace = true` |
| `crates/phase-server/Cargo.toml` | Add `license.workspace = true` |
| `client/src-tauri/Cargo.toml` | Add `license.workspace = true` |

### Documentation Scrub
| File | Changes |
|------|---------|
| `CLAUDE.md` | Remove "Originally derived from the open-source MTG Forge project" language; update crate descriptions (parser, card format references); update CI section |
| `.planning/PROJECT.md` | Update Constraints, Key Decisions table, Active requirements, Context paragraph |
| `.github/workflows/ci.yml` | Remove Forge coverage step, update JSON coverage to be default |
| `crates/engine/src/bin/coverage_report.rs` | Remove text/Forge mode, make JSON the default |

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in test + cargo test |
| Config file | Cargo.toml (workspace) |
| Quick run command | `cargo test -p engine` |
| Full suite command | `cargo test --all` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| MIGR-02 | Forge parser gated behind forge-compat | build | `cargo build -p engine` (no features) | Wave 0 -- verify compile |
| MIGR-02 | data/standard-cards/ removed | smoke | `test ! -d data/standard-cards/` | Wave 0 |
| MIGR-02 | Forge parser still works with feature | build | `cargo build -p engine --features forge-compat` | Wave 0 |
| LICN-01 | LICENSE-MIT exists | smoke | `test -f LICENSE-MIT` | Wave 0 |
| LICN-01 | LICENSE-APACHE exists | smoke | `test -f LICENSE-APACHE` | Wave 0 |
| LICN-01 | All Cargo.toml have license field | grep | `rg 'license' crates/*/Cargo.toml Cargo.toml` | Wave 0 |
| LICN-03 | Coverage binary works JSON-only | integration | `cargo run --bin coverage-report -- data/ --ci` | Existing (adapted) |
| LICN-03 | All tests pass without forge-compat | test | `cargo test --all` | Existing |
| LICN-03 | WASM builds without forge-compat | build | `cargo build --package engine-wasm --target wasm32-unknown-unknown` | Existing |

### Sampling Rate
- **Per task commit:** `cargo test -p engine && cargo clippy --all-targets -- -D warnings`
- **Per wave merge:** `cargo test --all && cargo build --package engine-wasm --target wasm32-unknown-unknown`
- **Phase gate:** Full suite green + WASM build + coverage-report JSON mode

### Wave 0 Gaps
None -- existing test infrastructure covers all phase requirements. The refactoring itself is validated by the existing test suite continuing to pass.

## Open Questions

1. **SubAbility Chain Parser Dependency**
   - What we know: `resolve_ability_chain()` calls `parse_ability()` to parse SVar strings at game resolution time
   - What's unclear: Whether `parse_ability()` can be cleanly separated from the Forge card-level parser
   - Recommendation: Gate only `parse_card_file()` and the file-walking loader, keep `parse_ability()` ungated since it's called during gameplay. The parser module split should be: `ability.rs` stays available, `card_parser.rs`/`card_type.rs`/`mana_cost.rs` get gated.

2. **`params()` / `to_params()` Boundary**
   - What we know: `Effect::to_params()` converts typed fields to HashMap. `AbilityDefinition::params()` merges that with `remaining_params`.
   - What's unclear: Whether gating `AbilityDefinition::params()` is sufficient or if callers need a replacement
   - Recommendation: Keep `Effect::to_params()` ungated. Have callers build params from `effect.to_params()` + `def.remaining_params` directly. Or keep a non-compat version of the merge.

3. **Static Registry Mode Coverage**
   - What we know: `StaticMode` enum has 16 core variants but the registry has 60+ entries, most using `StaticMode::Other("Name")`
   - What's unclear: Whether promoting more variants into the enum would be cleaner
   - Recommendation: NOT in scope for Phase 25. Use `StaticMode::Other("Name".into())` as registry keys -- the `PartialEq` + `Hash` impls handle this correctly. Promoting variants is future cleanup.

## Sources

### Primary (HIGH confidence)
- Codebase analysis: `crates/engine/src/game/effects/mod.rs`, `static_abilities.rs`, `replacement.rs`, `combat.rs`, `layers.rs`, `coverage.rs`, `casting.rs`, `mana_abilities.rs`, `planeswalker.rs`
- Codebase analysis: `crates/engine/src/types/ability.rs`, `statics.rs`, `replacements.rs`, `triggers.rs`
- Codebase analysis: `crates/engine/src/bin/coverage_report.rs`, `migrate.rs`
- Codebase analysis: `.github/workflows/ci.yml`
- [Cargo Features documentation](https://doc.rust-lang.org/cargo/reference/features.html) -- feature definition and cfg usage
- [Cargo Targets documentation](https://doc.rust-lang.org/cargo/reference/cargo-targets.html) -- required-features field
- [Rust API Guidelines - Necessities](https://rust-lang.github.io/api-guidelines/necessities.html) -- MIT/Apache-2.0 dual license convention

### Secondary (MEDIUM confidence)
- [Conditional Compilation in Rust](https://doc.rust-lang.org/reference/conditional-compilation.html) -- official reference
- [Feature-Gating Functions](https://www.slingacademy.com/article/feature-gating-functions-for-conditional-compilation-in-rust/) -- patterns and examples

### Tertiary (LOW confidence)
- None -- all findings verified against codebase and official Rust documentation

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- Rust's built-in feature system, well-documented
- Architecture: HIGH -- all dispatch sites enumerated from codebase analysis with line numbers
- Pitfalls: HIGH -- identified from actual code dependencies, not hypothetical
- License: HIGH -- Rust ecosystem convention, verified against official guidelines

**Research date:** 2026-03-10
**Valid until:** 2026-04-10 (stable domain, no external dependency changes expected)
