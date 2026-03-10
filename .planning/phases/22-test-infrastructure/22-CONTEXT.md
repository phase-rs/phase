# Phase 22: Test Infrastructure - Context

**Gathered:** 2026-03-10
**Status:** Ready for planning

<domain>
## Phase Boundary

Build a self-contained `GameScenario` test harness and rules correctness test suite that runs in CI with no filesystem dependencies. Developers can set up board states, execute actions, and assert outcomes using a fluent builder API. Core mechanics get dedicated test modules with XMage MIT scenarios ported as reference. Old filesystem-dependent test helpers are replaced entirely.

</domain>

<decisions>
## Implementation Decisions

### Scenario Builder API
- Mutable method-chaining builder: `GameScenario::new()` with `.add_creature()`, `.at_phase()`, `.build()` etc.
- `add_*` methods return `ObjectId` directly (mutable borrow pattern, not fluent chaining on scenario itself)
- Accessor methods for assertions — `result.zone(id)`, `result.life(P0)`, `result.battlefield_count(P0)` — compose with standard `assert_eq!`/`assert!`
- No custom assertion helpers (e.g., no `.assert_zone()`) — keep it idiomatic Rust
- Two execution modes: `build_and_run(actions)` for simple cases + step-by-step `game.act(action)` for complex interaction tests needing intermediate assertions

### Test Card Construction
- Cards constructed inline via scenario builder — no fixture files, no filesystem dependency
- Convenience methods for common cards: `.add_vanilla(P0, 2, 2)`, `.add_bolt_to_hand(P0)`, `.add_basic_land(P0, ManaColor::Green)`
- Fluent named methods for ~15 common keywords: `.flying()`, `.deathtouch()`, `.trample()`, `.haste()`, `.defender()`, etc.
- Generic `.with_keyword(Keyword::X)` fallback for rare keywords
- Ability chaining: `.with_ability(Effect::DealDamage { ... })`, `.with_static(StaticMode::...)`, `.as_instant()`, `.as_enchantment()`

### Rules Test Organization
- One module per mechanic: `crates/engine/tests/rules/{etb,combat,stack,sba,layers,keywords,targeting}.rs`
- 3-5 scenarios per mechanic (more for complex mechanics like combat + keyword interactions)
- XMage MIT-licensed tests as comprehensive reference — port key scenarios to GameScenario format, add own edge cases for gaps
- Comprehensive coverage of XMage scenarios unless there's excellent reason to skip specific ones
- MTG Comprehensive Rules numbers in doc comments: `/// CR 704.5a: lethal damage SBA`

### Snapshot Strategy
- Focused projections via a `GameSnapshot` struct — zones, life totals, stack, relevant object fields, and fired events
- NOT full GameState serialization (avoids brittleness from RNG state, counters, timestamps)
- Events included in snapshots — ordered list of GameEvents that fired during the action sequence
- Golden master snapshots only — most rules tests use `assert_eq!`/`assert!` for specific checks; 5-10 snapshot tests per mechanic capture complex action sequences as regression anchors
- Snapshot files colocated per insta convention (`tests/rules/snapshots/` auto-discovered)

### Old Test Helpers
- `test_helpers.rs` (Forge DB filesystem dependency) replaced entirely — not preserved
- All existing tests that use `spawn_creature()` / `forge_db()` migrated to GameScenario
- Success criterion #4: zero silent skips — no tests that pass by doing nothing when files are missing

### Claude's Discretion
- Exact `GameSnapshot` projection fields (beyond zones, life, stack, events)
- Which ~15 keywords get named convenience methods
- Internal implementation of the builder (how ObjectIds are tracked, how build() constructs GameState)
- Which specific XMage test files to reference per mechanic
- How to handle `WaitingFor` state in step-by-step execution mode

</decisions>

<specifics>
## Specific Ideas

- "Make the architecture clean as fuck" carries forward from Phase 21 — test harness API should be equally clean and idiomatic
- XMage tests as comprehensive reference, not just inspiration — port their scenarios thoroughly
- Fluent keyword API (`.flying().deathtouch()`) over verbose `.with_keyword(Keyword::X)` for common cases
- CR rule numbers in test doc comments serve as rules documentation — tests ARE the spec

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `GameState::new_two_player(seed)` — existing 2-player game init, likely the foundation for `GameScenario::build()`
- `create_object()` in `game/zones.rs` — creates GameObjects in zones, reusable in builder
- `parse_keywords()` in `game/keywords.rs` — converts keyword strings to typed `Keyword` values
- `apply(&mut state, action) -> ActionResult` — the core engine function the harness wraps
- `insta` crate already in deps — used for schema snapshots in `schema/mod.rs`
- Typed `Effect`, `TriggerMode`, `StaticMode`, `ReplacementEvent` enums from Phase 21 — test cards can use these directly

### Established Patterns
- Inline `#[cfg(test)]` modules in 75 files — existing tests use local `setup_game_at_main_phase()` helpers
- Duplicated setup helpers in `engine.rs` and `casting.rs` — consolidation opportunity
- `GameAction` / `GameEvent` / `WaitingFor` discriminated unions — test assertions match on these
- Serde derives on all core types — serialization for snapshots is built-in

### Integration Points
- `crates/engine/tests/` — integration test directory for the new rules test modules
- `test_helpers.rs` — to be replaced (currently Forge DB dependent, silently skips)
- `GameState` fields: `battlefield`, `players[].hand`, `graveyard`, `stack`, `exile` — accessor methods wrap these
- `ActionResult { events, waiting_for }` — events feed into snapshot projection

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 22-test-infrastructure*
*Context gathered: 2026-03-10*
