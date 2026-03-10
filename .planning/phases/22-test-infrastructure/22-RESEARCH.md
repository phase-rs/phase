# Phase 22: Test Infrastructure - Research

**Researched:** 2026-03-10
**Domain:** Rust test harness design, snapshot testing, game engine test patterns
**Confidence:** HIGH

## Summary

Phase 22 builds a self-contained `GameScenario` test harness and rules correctness test suite for the engine crate. The codebase already has 626 passing unit tests across 75 `#[cfg(test)]` modules, but they share a critical weakness: duplicated setup helpers (at least 5 variants of `setup_game_at_main_phase` and `create_creature` across `engine.rs`, `casting.rs`, `combat_damage.rs`, `sba.rs`, `layers.rs`) and the existing `test_helpers.rs` depends on the Forge filesystem database (silently returning `None` when unavailable). The solution is a `GameScenario` builder in a shared test utilities module that constructs `GameState` + `GameObject` instances inline with no filesystem dependencies, plus integration tests organized by mechanic in `crates/engine/tests/rules/`.

The `insta` crate (v1, `json` feature) is already a dev-dependency and used for one snapshot test (`ability_schema`). The snapshot strategy uses focused `GameSnapshot` projections (not raw `GameState` serialization) to avoid brittleness from RNG state, timestamps, and internal bookkeeping. All core engine types already derive `Serialize`/`Deserialize`, so snapshot infrastructure requires no type modifications.

**Primary recommendation:** Build `GameScenario` as a mutable builder in `crates/engine/src/game/scenario.rs` (accessible via `#[cfg(test)]`), with convenience methods returning `ObjectId` directly. Place integration tests in `crates/engine/tests/rules/{etb,combat,stack,sba,layers,keywords,targeting}.rs`. Use `GameSnapshot` struct for focused projections with `insta::assert_json_snapshot!` for regression anchors.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Mutable method-chaining builder: `GameScenario::new()` with `.add_creature()`, `.at_phase()`, `.build()` etc.
- `add_*` methods return `ObjectId` directly (mutable borrow pattern, not fluent chaining on scenario itself)
- Accessor methods for assertions -- `result.zone(id)`, `result.life(P0)`, `result.battlefield_count(P0)` -- compose with standard `assert_eq!`/`assert!`
- No custom assertion helpers (e.g., no `.assert_zone()`) -- keep it idiomatic Rust
- Two execution modes: `build_and_run(actions)` for simple cases + step-by-step `game.act(action)` for complex interaction tests needing intermediate assertions
- Cards constructed inline via scenario builder -- no fixture files, no filesystem dependency
- Convenience methods for common cards: `.add_vanilla(P0, 2, 2)`, `.add_bolt_to_hand(P0)`, `.add_basic_land(P0, ManaColor::Green)`
- Fluent named methods for ~15 common keywords: `.flying()`, `.deathtouch()`, `.trample()`, `.haste()`, `.defender()`, etc.
- Generic `.with_keyword(Keyword::X)` fallback for rare keywords
- Ability chaining: `.with_ability(Effect::DealDamage { ... })`, `.with_static(StaticMode::...)`, `.as_instant()`, `.as_enchantment()`
- One module per mechanic: `crates/engine/tests/rules/{etb,combat,stack,sba,layers,keywords,targeting}.rs`
- 3-5 scenarios per mechanic (more for complex mechanics like combat + keyword interactions)
- XMage MIT-licensed tests as comprehensive reference -- port key scenarios to GameScenario format, add own edge cases for gaps
- Comprehensive coverage of XMage scenarios unless there's excellent reason to skip specific ones
- MTG Comprehensive Rules numbers in doc comments: `/// CR 704.5a: lethal damage SBA`
- Focused projections via a `GameSnapshot` struct -- zones, life totals, stack, relevant object fields, and fired events
- NOT full GameState serialization (avoids brittleness from RNG state, counters, timestamps)
- Events included in snapshots -- ordered list of GameEvents that fired during the action sequence
- Golden master snapshots only -- most rules tests use `assert_eq!`/`assert!` for specific checks; 5-10 snapshot tests per mechanic capture complex action sequences as regression anchors
- Snapshot files colocated per insta convention (`tests/rules/snapshots/` auto-discovered)
- `test_helpers.rs` (Forge DB filesystem dependency) replaced entirely -- not preserved
- All existing tests that use `spawn_creature()` / `forge_db()` migrated to GameScenario
- Success criterion #4: zero silent skips -- no tests that pass by doing nothing when files are missing

### Claude's Discretion
- Exact `GameSnapshot` projection fields (beyond zones, life, stack, events)
- Which ~15 keywords get named convenience methods
- Internal implementation of the builder (how ObjectIds are tracked, how build() constructs GameState)
- Which specific XMage test files to reference per mechanic
- How to handle `WaitingFor` state in step-by-step execution mode

### Deferred Ideas (OUT OF SCOPE)
None -- discussion stayed within phase scope
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| TEST-01 | A self-contained GameScenario test harness provides add_card(), set_phase(), act(), and assertion helpers with no external filesystem dependencies | GameScenario builder pattern, inline card construction via `create_object()` + property setting, `GameState::new_two_player()` as foundation |
| TEST-02 | Scenario-based rules correctness tests cover core mechanics: ETB triggers, combat, stack resolution, state-based actions, layer system, keyword interactions | Integration test modules in `crates/engine/tests/rules/`, XMage MIT tests as reference scenarios, existing inline tests show patterns for each mechanic |
| TEST-03 | insta snapshot tests capture GameState after action sequences to detect unintended engine changes across commits | `insta` v1 with `json` feature already in dev-deps, `GameSnapshot` projection struct, `assert_json_snapshot!` for focused snapshots |
</phase_requirements>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| insta | 1 (features: `json`) | Snapshot testing with JSON serialization | Already in dev-deps, industry standard for Rust snapshot tests |
| serde + serde_json | 1 | Serialize GameSnapshot for insta | Already workspace deps, all game types derive Serialize |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| (none needed) | - | - | All required libs already present |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| insta JSON snapshots | insta YAML snapshots | YAML is better for diffs but JSON matches the codebase's serialization convention |
| Custom GameSnapshot | Full GameState serialization | Full state is brittle (RNG, timestamps, internal counters change); projection is stable |
| Integration tests dir | Inline #[cfg(test)] | Integration tests in `tests/` can use the scenario builder as a real external consumer; inline tests can't share helpers across modules easily |

**Installation:**
```bash
# No new packages needed -- insta already in dev-deps
# Verify: cargo test -p engine -- scenario (after implementation)
```

## Architecture Patterns

### Recommended Project Structure
```
crates/engine/
├── src/
│   └── game/
│       ├── scenario.rs          # GameScenario builder + GameSnapshot (cfg(test))
│       ├── test_helpers.rs      # REPLACED (currently Forge DB dependent)
│       └── mod.rs               # Add pub mod scenario
├── tests/
│   ├── rules/
│   │   ├── mod.rs               # Common imports, re-export scenario
│   │   ├── etb.rs               # ETB trigger tests
│   │   ├── combat.rs            # Combat damage tests
│   │   ├── stack.rs             # Stack resolution tests
│   │   ├── sba.rs               # State-based action tests
│   │   ├── layers.rs            # Layer system tests
│   │   ├── keywords.rs          # Keyword interaction tests
│   │   ├── targeting.rs         # Targeting/fizzle tests
│   │   └── snapshots/           # insta auto-discovered snapshot dir
│   └── rules.rs                 # Integration test entry point (mod rules;)
```

### Pattern 1: GameScenario Builder
**What:** Mutable builder that constructs a GameState with predefined board state, phase, turn, and card objects. Returns `ObjectId` from `add_*` methods for later reference.
**When to use:** Every rules test uses this to set up initial game state.
**Example:**
```rust
// Source: Derived from codebase analysis of GameState, create_object, GameObject
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;

#[test]
fn etb_trigger_fires_when_creature_enters() {
    let mut scenario = GameScenario::new();
    let soul_warden = scenario
        .add_creature(P0, "Soul Warden", 1, 1)
        .with_trigger(TriggerMode::ChangesZone, /* params */)
        .on_battlefield();
    scenario.at_phase(Phase::PreCombatMain);

    let result = scenario.build_and_run(vec![
        // Cast a creature to trigger ETB
    ]);

    assert_eq!(result.life(P0), 21); // Soul Warden gained 1 life
}
```

### Pattern 2: Step-by-Step Execution
**What:** Build game state, then execute actions one at a time with intermediate assertions.
**When to use:** Complex interactions where intermediate state matters (e.g., stack ordering, priority passing).
**Example:**
```rust
// Source: Derived from codebase analysis of apply() and WaitingFor
#[test]
fn stack_resolves_lifo() {
    let mut scenario = GameScenario::new();
    // ... setup ...
    let mut game = scenario.build();

    let result1 = game.act(GameAction::CastSpell { /* bolt 1 */ });
    assert_eq!(game.state().stack.len(), 1);

    let result2 = game.act(GameAction::CastSpell { /* bolt 2 */ });
    assert_eq!(game.state().stack.len(), 2);

    // Pass priority to resolve -- bolt 2 resolves first (LIFO)
    game.pass_until_resolve();
    // Assert bolt 2 resolved first
}
```

### Pattern 3: GameSnapshot for insta
**What:** A focused projection struct that extracts stable, test-relevant fields from GameState + events.
**When to use:** Golden master regression tests (5-10 per mechanic).
**Example:**
```rust
// Source: insta docs (https://insta.rs/docs/quickstart/)
#[test]
fn snapshot_combat_with_first_strike() {
    let mut scenario = GameScenario::new();
    // ... setup combat ...
    let result = scenario.build_and_run(actions);

    let snapshot = result.snapshot();
    insta::assert_json_snapshot!("combat_first_strike_kills_before_regular", snapshot);
}
```

### Pattern 4: CardBuilder (Fluent Keyword Chaining)
**What:** A temporary builder returned by `add_creature()` etc. that allows `.flying().deathtouch().trample()` chaining before finalizing.
**When to use:** Setting up creatures with multiple keywords/abilities.
**Design consideration:** The `add_*` methods return `ObjectId` (per user decision), so keyword chaining must happen on an intermediate builder. The recommended approach:
```rust
// Two-phase: add returns ObjectId, modify via scenario
let bear = scenario.add_creature(P0, "Bear", 2, 2);
scenario.give_keywords(bear, &[Keyword::Flying, Keyword::Deathtouch]);

// OR: convenience methods that chain on scenario itself
let id = scenario.add_creature(P0, "Knight", 3, 2)
    .flying()
    .first_strike();
// Here add_creature returns a CardBuilder that auto-records ObjectId
// CardBuilder::drop or .done() writes back to scenario
// ObjectId retrievable via .id() on CardBuilder
```

**Recommendation for reconciling "returns ObjectId" with keyword chaining:** Use a `CardBuilder<'a>` that holds `&mut GameScenario` + `ObjectId`. Methods like `.flying()` mutate the object through the scenario and return `&mut Self`. A `.id()` method or `Into<ObjectId>` impl extracts the ObjectId. This pattern satisfies both decisions: `add_*` ultimately yields `ObjectId`, and keyword chaining works fluidly.

### Anti-Patterns to Avoid
- **Full GameState in snapshots:** Brittle -- RNG seed, timestamps, next_object_id all change. Use GameSnapshot projection instead.
- **Filesystem-dependent test helpers:** The whole point is eliminating `forge_db()` / `spawn_creature()`. Never reference card database files.
- **Custom assertion macros:** The user explicitly decided against `.assert_zone()` style helpers. Use standard `assert_eq!`/`assert!` with accessor methods.
- **Parsing Forge strings for test cards:** The `parse_test_ability("SP$ DealDamage | ...")` pattern couples tests to the Forge parser. Use typed `Effect::DealDamage { ... }` directly.
- **Silent skip on missing data:** Tests must not `if forge_db().is_none() { return; }`. Every test must unconditionally exercise its assertions.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Snapshot diffs | Custom diff logic | `insta` crate | Handles snapshot creation, review workflow (`cargo insta review`), CI integration |
| JSON snapshot serialization | Custom serializers | `insta::assert_json_snapshot!` with `serde` | Already have Serialize on all types; insta handles formatting and comparison |
| Object ID allocation | Manual ID tracking | Wrap `GameState::next_object_id` via `create_object()` | Already correct; don't re-implement ID allocation |
| Zone bookkeeping | Direct HashMap manipulation | Use existing `create_object()` and `add_to_zone()` | Zone bookkeeping has edge cases (player zones vs shared zones) |
| Combat state setup | Manual CombatState construction | Scenario builder method that mirrors `combat_damage.rs::setup_combat` | Complex nested struct with `AttackerInfo`, blocker assignments, etc. |

**Key insight:** The engine already has well-tested low-level primitives (`create_object`, `add_to_zone`, `move_to_zone`, `apply`). The scenario builder wraps these -- it does NOT bypass them. This ensures tests exercise the same code paths as production.

## Common Pitfalls

### Pitfall 1: Summoning Sickness in Tests
**What goes wrong:** Creatures created on the battlefield can't attack because `entered_battlefield_turn == Some(current_turn)` and `turn_number` wasn't advanced.
**Why it happens:** `create_object` doesn't set `entered_battlefield_turn`; the test forgets to set it or advance the turn.
**How to avoid:** The scenario builder's `add_creature()` should set `entered_battlefield_turn = Some(turn_number.saturating_sub(1))` by default (matching the pattern in `test_helpers.rs::spawn_creature`). Provide `.with_summoning_sickness()` to override.
**Warning signs:** Tests involving combat where attackers can't be declared.

### Pitfall 2: Phase/WaitingFor Mismatch
**What goes wrong:** Actions fail because `WaitingFor` doesn't match the phase. E.g., trying to cast a spell when `WaitingFor::DeclareAttackers`.
**Why it happens:** Setting `state.phase = Phase::PreCombatMain` without also setting `state.waiting_for = WaitingFor::Priority { player }`.
**How to avoid:** The scenario builder's `at_phase()` method MUST set both `phase` and `waiting_for` together. Also set `priority_player` and `active_player` consistently.
**Warning signs:** `EngineError::InvalidAction` or `NotYourPriority` in tests that look correct.

### Pitfall 3: CardType Not Set
**What goes wrong:** Creature-specific logic doesn't fire because `card_types.core_types` is empty.
**Why it happens:** `GameObject::new` initializes `card_types: CardType::default()` which has empty vectors.
**How to avoid:** `add_creature()` must push `CoreType::Creature`, `add_basic_land()` must push `CoreType::Land`, etc. Every convenience method sets the correct card type.
**Warning signs:** SBAs don't destroy creatures with lethal damage, creatures can't attack/block.

### Pitfall 4: Base vs Computed Characteristics
**What goes wrong:** Layer tests fail because `base_power`/`base_toughness` aren't set, so layer evaluation resets computed values.
**Why it happens:** The layer system reads `base_power` and writes to `power`. If `base_power` is `None`, the reset step sets `power` to `None`.
**How to avoid:** `add_creature(P0, name, p, t)` must set BOTH `power`/`toughness` AND `base_power`/`base_toughness`. See the existing `layers.rs::make_creature` helper.
**Warning signs:** Creatures with correct power at creation lose it after `evaluate_layers()`.

### Pitfall 5: Snapshot Brittleness from Object IDs
**What goes wrong:** Snapshot tests fail when unrelated changes alter object creation order, changing ObjectIds.
**Why it happens:** `ObjectId` is an incrementing counter. Adding a card earlier in setup shifts all subsequent IDs.
**How to avoid:** Use `insta` redactions to replace ObjectIds with stable placeholders in snapshots: `{ "**.object_id" => "[id]" }`. Or design `GameSnapshot` to use names/indices rather than raw IDs where possible.
**Warning signs:** Unrelated test changes cause snapshot failures in other tests.

### Pitfall 6: Priority Player vs Active Player Confusion
**What goes wrong:** Actions are rejected or assigned to the wrong player.
**Why it happens:** `active_player` (whose turn it is) and `priority_player` (who can currently act) are different concepts. During opponent's combat, active_player != priority_player.
**How to avoid:** Scenario builder defaults both to P0. Tests that need opponent's turn must set both correctly.
**Warning signs:** `WrongPlayer` errors in multi-player action sequences.

## Code Examples

Verified patterns from codebase analysis:

### Creating a Creature on the Battlefield (Current Pattern)
```rust
// Source: crates/engine/src/game/combat_damage.rs lines 486-506
fn create_creature(
    state: &mut GameState,
    owner: PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        owner,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(power);
    obj.toughness = Some(toughness);
    obj.entered_battlefield_turn = Some(1); // Not summoning sick
    id
}
```

### Setting Up Combat State (Current Pattern)
```rust
// Source: crates/engine/src/game/combat_damage.rs lines 508-530
fn setup_combat(
    state: &mut GameState,
    attackers: Vec<ObjectId>,
    blocker_assignments: Vec<(ObjectId, Vec<ObjectId>)>,
) {
    let mut combat = CombatState {
        attackers: attackers
            .iter()
            .map(|&id| AttackerInfo {
                object_id: id,
                defending_player: PlayerId(1),
            })
            .collect(),
        ..Default::default()
    };
    for (attacker_id, blockers) in blocker_assignments {
        for &blocker_id in &blockers {
            combat.blocker_to_attacker.insert(blocker_id, attacker_id);
        }
        combat.blocker_assignments.insert(attacker_id, blockers);
    }
    state.combat = Some(combat);
}
```

### Setting Up Main Phase (Current Duplicated Pattern)
```rust
// Source: crates/engine/src/game/engine.rs lines 765-774 (duplicated in casting.rs lines 485-495)
fn setup_game_at_main_phase() -> GameState {
    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state
}
```

### insta Snapshot Test (Existing Pattern)
```rust
// Source: crates/engine/src/schema/mod.rs lines 55-58
#[test]
fn ability_schema_snapshot() {
    let schema = generate_schema();
    insta::assert_json_snapshot!("ability_schema", schema);
}
```

### Applying an Action (Core Engine Pattern)
```rust
// Source: crates/engine/src/game/engine.rs line 35
pub fn apply(state: &mut GameState, action: GameAction) -> Result<ActionResult, EngineError> {
    // Returns ActionResult { events: Vec<GameEvent>, waiting_for: WaitingFor }
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| `forge_db()` + `spawn_creature()` with filesystem | Inline `GameScenario` builder with no dependencies | This phase | Tests run in CI without Forge data directory |
| Duplicated `setup_game_at_main_phase()` in 5+ files | Single `GameScenario::new().at_phase()` | This phase | DRY, consistent behavior |
| Duplicated `create_creature()` helpers in 4+ files | `scenario.add_creature(P0, name, p, t)` | This phase | Single source of truth for creature setup |
| No integration tests directory | `crates/engine/tests/rules/` with per-mechanic modules | This phase | Clear organization, easy to add new tests |
| Silent test skips when DB unavailable | All tests unconditionally assert | This phase | Zero false positives in CI |

**Deprecated/outdated:**
- `test_helpers.rs` with `forge_db()`, `load_card()`, `spawn_creature()`: Replaced entirely. The 3 tests in its own `#[cfg(test)]` module are also removed.

## Open Questions

1. **CardBuilder Lifetime Design**
   - What we know: `add_creature()` must return `ObjectId` (locked decision), keyword chaining is desired (locked decision).
   - What's unclear: Whether `CardBuilder<'a>` holding `&mut GameScenario` will cause borrow checker issues when adding multiple cards.
   - Recommendation: Implement `CardBuilder` that borrows `&mut GameState` (not `&mut GameScenario`) or use a pattern where `CardBuilder` consumes and returns `&mut GameScenario`. Alternative: have `add_creature()` return `ObjectId` immediately and use separate `.give_keywords(id, &[...])` methods on the scenario. Test ergonomics during implementation to find the cleanest approach.

2. **WaitingFor Handling in Step-by-Step Mode**
   - What we know: The engine's `apply()` returns `ActionResult { events, waiting_for }`. Many WaitingFor states require specific follow-up actions.
   - What's unclear: How deeply the scenario wrapper should abstract WaitingFor transitions. E.g., should `game.pass_until_resolve()` auto-handle all priority passes?
   - Recommendation: Provide `game.act(action) -> ActionResult` for raw control, plus convenience methods like `game.pass_both_players()` (passes priority for both players once), `game.resolve_top()` (passes until stack top resolves). Keep these thin wrappers around `apply()` calls.

3. **Which ~15 Keywords Get Named Convenience Methods**
   - What we know: The Keyword enum has 100+ variants. Only the most commonly tested ~15 should get fluent `.flying()` etc.
   - Recommendation: Based on existing test usage and combat mechanics coverage: `flying`, `first_strike`, `double_strike`, `trample`, `deathtouch`, `lifelink`, `vigilance`, `haste`, `reach`, `defender`, `menace`, `indestructible`, `hexproof`, `flash`, `wither`. These cover all combat keywords tested in `combat_damage.rs` and `keywords` interactions.

4. **XMage Test File Selection per Mechanic**
   - What we know: XMage has `Mage.Tests/src/test/java/org/mage/test/` with subdirectories for `combat/`, `cards/abilities/keywords/`, etc.
   - Recommendation per mechanic:
     - **ETB**: `cards/abilities/keywords/` (various ETB trigger tests)
     - **Combat**: `combat/FirstStrikeTest.java`, `combat/LifelinkInCombatTest.java`, `combat/DamageDistributionTest.java`, `cards/abilities/keywords/DeathtouchTest.java`
     - **Stack**: `serverside/StackOrderTest.java` or equivalent
     - **SBA**: Covered by combat and damage tests; reference CR 704.5 rules directly
     - **Layers**: Reference CR 613 directly; XMage has limited dedicated layer tests
     - **Keywords**: `cards/abilities/keywords/` directory

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | cargo test (built-in) + insta 1.x (snapshot) |
| Config file | `crates/engine/Cargo.toml` (dev-dependencies) |
| Quick run command | `cargo test -p engine -- rules` |
| Full suite command | `cargo test --all` |

### Phase Requirements to Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| TEST-01 | GameScenario creates board state without filesystem | unit | `cargo test -p engine -- scenario -x` | No -- Wave 0 |
| TEST-01 | Scenario builder add_creature/add_land returns valid ObjectId | unit | `cargo test -p engine -- scenario::tests -x` | No -- Wave 0 |
| TEST-02 | ETB triggers fire on creature entry | integration | `cargo test -p engine --test rules -- etb -x` | No -- Wave 0 |
| TEST-02 | Combat damage resolves correctly (basic) | integration | `cargo test -p engine --test rules -- combat -x` | No -- Wave 0 |
| TEST-02 | Stack resolves LIFO | integration | `cargo test -p engine --test rules -- stack -x` | No -- Wave 0 |
| TEST-02 | SBAs check lethal damage, zero life, zero toughness | integration | `cargo test -p engine --test rules -- sba -x` | No -- Wave 0 |
| TEST-02 | Layer system applies in correct order | integration | `cargo test -p engine --test rules -- layers -x` | No -- Wave 0 |
| TEST-02 | Keyword interactions (deathtouch+trample, etc.) | integration | `cargo test -p engine --test rules -- keywords -x` | No -- Wave 0 |
| TEST-03 | insta snapshots capture post-action GameState projections | integration | `cargo test -p engine --test rules -- snapshot -x` | No -- Wave 0 |
| TEST-03 | Snapshot changes detected on cargo test | integration | `cargo test -p engine -- --check` | No -- Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test -p engine -- rules`
- **Per wave merge:** `cargo test --all`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `crates/engine/src/game/scenario.rs` -- GameScenario builder + GameSnapshot
- [ ] `crates/engine/tests/rules.rs` -- Integration test entry point
- [ ] `crates/engine/tests/rules/mod.rs` -- Common imports
- [ ] `crates/engine/tests/rules/etb.rs` -- ETB trigger tests
- [ ] `crates/engine/tests/rules/combat.rs` -- Combat tests
- [ ] `crates/engine/tests/rules/stack.rs` -- Stack resolution tests
- [ ] `crates/engine/tests/rules/sba.rs` -- SBA tests
- [ ] `crates/engine/tests/rules/layers.rs` -- Layer system tests
- [ ] `crates/engine/tests/rules/keywords.rs` -- Keyword interaction tests
- [ ] `crates/engine/tests/rules/targeting.rs` -- Targeting/fizzle tests

## Sources

### Primary (HIGH confidence)
- Codebase analysis of `crates/engine/src/game/test_helpers.rs` -- current filesystem-dependent helpers
- Codebase analysis of `crates/engine/src/game/engine.rs` -- `apply()` function, duplicated `setup_game_at_main_phase()`
- Codebase analysis of `crates/engine/src/game/combat_damage.rs` -- combat test patterns, `create_creature()`, `setup_combat()`
- Codebase analysis of `crates/engine/src/game/layers.rs` -- layer test patterns, `make_creature()` with base/computed P/T
- Codebase analysis of `crates/engine/src/game/sba.rs` -- SBA test patterns
- Codebase analysis of `crates/engine/src/types/` -- GameState, GameObject, Keyword, Phase, GameAction, GameEvent, WaitingFor structures
- Codebase analysis of `crates/engine/src/schema/mod.rs` -- existing insta snapshot usage
- Codebase analysis of `crates/engine/Cargo.toml` -- insta v1 with json feature confirmed

### Secondary (MEDIUM confidence)
- [insta documentation](https://insta.rs/docs/quickstart/) -- snapshot testing patterns, redactions, JSON snapshots
- [XMage GitHub](https://github.com/magefree/mage) -- MIT-licensed test reference, combat test directory structure

### Tertiary (LOW confidence)
- XMage test file specifics -- directory structure confirmed but individual test scenarios not deeply analyzed

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- insta already in dev-deps, all types already Serialize, no new dependencies needed
- Architecture: HIGH -- builder pattern directly mirrors existing duplicated test helpers; consolidation path is clear
- Pitfalls: HIGH -- identified from actual patterns in 5+ existing test modules with duplicated/inconsistent setup
- XMage reference details: MEDIUM -- directory structure confirmed, individual test scenarios need review during implementation

**Research date:** 2026-03-10
**Valid until:** 2026-04-10 (stable domain -- Rust testing patterns don't change rapidly)
