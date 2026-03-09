# Phase 18: Select Candidates to Support and Implement Stubbed Mechanics - Research

**Researched:** 2026-03-09
**Domain:** Rust game engine mechanics implementation (MTG keywords, effects, triggers, static abilities)
**Confidence:** HIGH

## Summary

Phase 18 involves implementing stubbed game mechanics across the engine. The codebase already has a mature architecture: all 200+ keywords are parsed into typed Rust enums, trigger/static/replacement registries exist with stub handlers, and 15 effect handlers are implemented. The primary work is adding game logic to existing stubs -- not creating new infrastructure.

Card frequency analysis of the 32,274-card Forge database reveals clear priority tiers. Flying (3,039 cards), Trample (919), Vigilance (651), Haste (615), Flash (586) are already implemented. The highest-impact remaining combat keywords are Fear (stubbed, used on ~200 cards via Intimidate/Fear), Horsemanship, and Skulk. For effects, Mill (153 cards), Scry (84), Dig (312), PumpAll (399), DamageAll (170), and DestroyAll (154) are the most impactful missing handlers.

**Primary recommendation:** Follow the CONTEXT.md tier structure exactly. Combat keywords are trivial additions to validate_blockers(). Effect handlers follow the established pattern in effects/mod.rs. Integration tests using real Forge card definitions from /Users/matt/dev/forge/ will prove the full pipeline works.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Rank all stubbed mechanics by Standard-legal card frequency (analyze card-data.json or Forge DB)
- Tiebreaker: prefer simpler mechanics when frequency is similar
- Dependency-aware: always include prerequisite mechanics
- Combat keywords get top priority since combat damage infrastructure already exists
- Tier 1 (Plan 1): All combat keywords -- Flying, Reach, Menace, Vigilance, Defender, Haste, Hexproof, Shroud, Flash, Fear, Intimidate, Skulk, Shadow, Horsemanship, Indestructible
- Tier 2 (Plan 2-3): Common non-combat mechanics by Standard frequency -- Scry, Mill, Surveil, Ward, Protection, Proliferate, Explore, Cycling, Kicker, ETB variants, block/attack restrictions
- Tier 3 (Plan 4+ if architecturally clean): Complex subsystems -- Transform/DFCs, Morph, Phasing, Day/Night
- Architecture fit is the gating criterion, not complexity alone
- MTG comprehensive rules as primary authority
- Forge Java source as reference only, not authoritative
- Unit tests: 2-5 inline Rust tests per mechanic
- Integration tests: Load real card definitions from Forge database
- Create reusable test helper utility for loading cards from Forge DB
- Mechanic coverage report: script or test counting Standard cards with support
- Card-level UI warning: visual indicator on cards with unimplemented mechanics

### Claude's Discretion
- Exact mechanic ranking from card frequency analysis (this research provides it)
- How to batch mechanics within plans for engineering efficiency
- Whether card-level warning lives engine-side or client-side
- Test helper architecture and API design
- Coverage report format and output location
- Whether specific subsystem mechanics pass architecture assessment

### Deferred Ideas (OUT OF SCOPE)
None -- discussion stayed within phase scope
</user_constraints>

## Current Implementation Status

### Already Implemented Keywords (with game logic)

| Keyword | Where | Cards |
|---------|-------|-------|
| Flying | combat.rs (blocking restriction) | 3,039 |
| Reach | combat.rs (can block flying) | 372 |
| Menace | combat.rs (must be blocked by 2+) | 354 |
| Vigilance | combat.rs (doesn't tap to attack) | 651 |
| Defender | combat.rs (can't attack) | 306 |
| Haste | combat.rs (no summoning sickness) | 615 |
| Shadow | combat.rs (shadow blocking rules) | ~30 |
| First Strike | combat_damage.rs (first damage step) | 358 |
| Double Strike | combat_damage.rs (deals damage twice) | 109 |
| Trample | combat_damage.rs (excess to player) | 919 |
| Deathtouch | combat_damage.rs + sba.rs (lethal from any) | 302 |
| Lifelink | combat_damage.rs (gain life equal to damage) | 335 |
| Indestructible | sba.rs (survives lethal damage) | 97 |
| Flash | casting.rs (cast at instant speed) | 586 |
| Hexproof | targeting.rs (can't be targeted by opponents) | 88 |
| Shroud | targeting.rs (can't be targeted by anyone) | ~30 |

### Already Implemented Effect Handlers (15 total)

DealDamage, Draw, ChangeZone, Pump, Destroy, Counter, Token, GainLife, LoseLife, Tap, Untap, AddCounter, RemoveCounter, Sacrifice, DiscardCard

### Already Implemented Trigger Matchers (~30)

ChangesZone, ChangesZoneAll, DamageDone (+ variants), SpellCast (+ variants), Attacks, AttackersDeclared (+ variants), Blocks, BlockersDeclared, Countered, CounterAdded (+ variants), CounterRemoved (+ variants), Taps (+ variants), Untaps (+ variants), LifeGained, LifeLost (+ variants), Drawn, Discarded (+ variants), Sacrificed (+ variants), Destroyed, TokenCreated (+ variants), TurnBegin, Phase, BecomesTarget (+ variants), LandPlayed, SpellCopy, ManaAdded

### Stubbed Systems

- **57 unimplemented trigger modes** (including Milled, Cycled, Explored, Surveil, Scry, AttackerBlocked, etc.)
- **47 stubbed static ability modes** (including CantBeBlocked, Ward, Protection, Indestructible as static, etc.)
- **21 stubbed replacement event types** (including Mill, Scry, Explore, Proliferate, Transform, etc.)

## Architecture Patterns

### Recommended Project Structure

No new directories needed. All work fits into existing structure:

```
crates/engine/src/
├── game/
│   ├── combat.rs          # Add evasion checks (Fear, Intimidate, Skulk, Horsemanship)
│   ├── combat_damage.rs   # Add Wither/Infect damage handling
│   ├── casting.rs         # Flash already works; add Cycling/Kicker activation
│   ├── effects/
│   │   ├── mod.rs         # Register new effect handlers
│   │   ├── mill.rs        # NEW: Mill effect handler
│   │   ├── scry.rs        # NEW: Scry effect handler
│   │   ├── pump.rs        # Extend for PumpAll
│   │   ├── deal_damage.rs # Extend for DamageAll
│   │   └── destroy.rs     # Extend for DestroyAll
│   ├── static_abilities.rs # Promote stubs to real handlers
│   ├── triggers.rs        # Add match arms for new trigger modes
│   ├── replacement.rs     # Promote stubs for relevant replacement types
│   └── sba.rs             # Add SBAs for new mechanics (poison counters, etc.)
└── types/
    └── (no changes needed -- keywords, triggers already parsed)
```

### Pattern 1: Adding a Combat Evasion Keyword

**What:** Adding a blocking restriction check in `validate_blockers()`
**When to use:** Fear, Intimidate, Skulk, Horsemanship
**Example:**
```rust
// In combat.rs validate_blockers(), after the existing Flying/Shadow checks:

// Fear: can only be blocked by artifact or black creatures
if attacker.has_keyword(&Keyword::Fear) {
    let is_artifact = blocker.card_types.core_types.contains(&CoreType::Artifact);
    let is_black = blocker.color.contains(&ManaColor::Black);
    if !is_artifact && !is_black {
        return Err(format!(
            "{:?} cannot block {:?} (fear: must be artifact or black)",
            blocker_id, attacker_id
        ));
    }
}

// Intimidate: can only be blocked by artifact or creatures sharing a color
if attacker.has_keyword(&Keyword::Intimidate) {
    let is_artifact = blocker.card_types.core_types.contains(&CoreType::Artifact);
    let shares_color = attacker.color.iter().any(|c| blocker.color.contains(c));
    if !is_artifact && !shares_color {
        return Err(format!(
            "{:?} cannot block {:?} (intimidate: must be artifact or share color)",
            blocker_id, attacker_id
        ));
    }
}

// Skulk: can't be blocked by creatures with greater power
if attacker.has_keyword(&Keyword::Skulk) {
    let attacker_power = attacker.power.unwrap_or(0);
    let blocker_power = blocker.power.unwrap_or(0);
    if blocker_power > attacker_power {
        return Err(format!(
            "{:?} cannot block {:?} (skulk: blocker power {} > attacker power {})",
            blocker_id, attacker_id, blocker_power, attacker_power
        ));
    }
}

// Horsemanship: like flying, only blocked by horsemanship
if attacker.has_keyword(&Keyword::Horsemanship)
    && !blocker.has_keyword(&Keyword::Horsemanship)
{
    return Err(format!(
        "{:?} cannot block {:?} (horsemanship)",
        blocker_id, attacker_id
    ));
}
```

### Pattern 2: Adding a New Effect Handler

**What:** Create a new effect module and register it
**When to use:** Mill, Scry, Surveil, PumpAll, DamageAll, DestroyAll, GainControl, Dig
**Example:**
```rust
// In effects/mill.rs (NEW FILE):
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let count: u32 = ability.params.get("NumCards")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    // Determine target player
    let target_player = resolve_target_player(ability, state);

    let player = state.players.iter_mut()
        .find(|p| p.id == target_player)
        .ok_or_else(|| EffectError::InvalidTarget)?;

    // Move top N cards from library to graveyard
    for _ in 0..count {
        if let Some(&card_id) = player.library.last() {
            zones::move_to_zone(state, card_id, Zone::Graveyard, events);
        }
    }
    Ok(())
}

// Register in effects/mod.rs build_registry():
registry.insert("Mill".to_string(), mill::resolve);
```

### Pattern 3: Promoting a Stubbed Static Ability

**What:** Replace `handle_stub` with a real handler that returns meaningful StaticEffects
**When to use:** Ward, Protection, CantBeBlocked, block/attack restrictions
**Example:**
```rust
// In static_abilities.rs, replace the stub for "Ward":
fn handle_ward(
    _state: &GameState,
    params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    let mode = params.get("Mode").cloned().unwrap_or_else(|| "Ward".to_string());
    vec![StaticEffect::RuleModification { mode }]
}
// Then the targeting system checks for Ward and prompts the opponent to pay
```

### Pattern 4: Integration Test with Real Forge Cards

**What:** Load card from Forge database, create game scenario, verify behavior
**When to use:** Every mechanic needs at least one integration test
**Example:**
```rust
// Test helper module (new):
pub fn load_forge_card(name: &str) -> Option<CardRules> {
    let forge_path = Path::new("../../forge/forge-gui/res/cardsfolder/");
    if !forge_path.exists() {
        return None; // Skip in CI without Forge
    }
    let db = CardDatabase::load(forge_path).ok()?;
    db.get_by_name(name).cloned()
}

// Integration test:
#[test]
fn serra_angel_has_flying_and_vigilance() {
    let card = load_forge_card("Serra Angel");
    if card.is_none() { return; } // Skip if no Forge DB
    let card = card.unwrap();
    let face = card.front_face();
    let keywords = parse_keywords(&face.keywords);
    assert!(keywords.contains(&Keyword::Flying));
    assert!(keywords.contains(&Keyword::Vigilance));
}
```

### Anti-Patterns to Avoid

- **Don't add new enum variants to Keyword**: All keywords are already parsed. New mechanics add game logic only, not new types.
- **Don't create separate keyword handler systems**: Use the existing combat.rs / targeting.rs / sba.rs / layers.rs integration points.
- **Don't rebuild the trigger registry at startup**: The build_trigger_registry() function is called per process_triggers() invocation. Adding match arms is sufficient.
- **Don't duplicate evasion logic**: All blocking restrictions go in validate_blockers(). All damage modifications go in combat_damage.rs. Don't scatter keyword checks.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Card loading for tests | Custom test file reader | `CardDatabase::load()` | Already handles recursive directory walking, face indexing, error recovery |
| Keyword presence check | Manual Vec::contains | `obj.has_keyword()` | Uses discriminant matching (handles parameterized variants) |
| Zone movement | Manual state mutation | `zones::move_to_zone()` | Handles battlefield tracking, player zone lists, events |
| Damage application | Direct life/damage mutation | `replacement::replace_event()` pipeline | Handles replacement effects, prevention, deathtouch flags |
| Layer evaluation | Manual P/T recalculation | `layers::evaluate_layers()` | Handles all 7 layers, counter-based mods, dependency ordering |

## Common Pitfalls

### Pitfall 1: Keywords Already Implemented But Listed as Tier 1
**What goes wrong:** CONTEXT.md lists Flying, Reach, Menace, Vigilance, Defender, Haste, Hexproof, Shroud, Flash, Indestructible, Shadow as Tier 1 combat keywords, but these are ALREADY IMPLEMENTED.
**Why it happens:** The phase scope was defined before detailed code analysis.
**How to avoid:** Plan 1 should only implement the 4 actually-stubbed combat keywords: Fear, Intimidate, Skulk, Horsemanship. Verify each "new" keyword doesn't already have logic before implementing.
**Warning signs:** Test already passes without any code changes.

### Pitfall 2: Forge Card Path Varies by Environment
**What goes wrong:** Integration tests hardcode a Forge card path that doesn't exist in CI.
**Why it happens:** Forge card database lives at `/Users/matt/dev/forge/forge-gui/res/cardsfolder/` locally but doesn't exist in CI.
**How to avoid:** Integration tests must use `#[ignore]` or check path existence and skip gracefully. The existing `load_real_forge_cards` test in card_db.rs shows this pattern.
**Warning signs:** CI failures on tests that pass locally.

### Pitfall 3: Confusing Keyword Parsing with Keyword Behavior
**What goes wrong:** Assuming a keyword "works" because it's parsed into a Keyword enum variant.
**Why it happens:** All 200+ keywords parse correctly. But only ~16 have actual game logic.
**How to avoid:** For each keyword, trace the code path: parsing (keywords.rs) -> game object (keywords vec) -> game logic (combat.rs / sba.rs / targeting.rs / layers.rs). A keyword without game logic just sits in the Vec doing nothing.
**Warning signs:** Cards with the keyword behave identically to vanilla creatures.

### Pitfall 4: Forgetting to Update has_potential_attackers()
**What goes wrong:** Adding blocking restrictions (Fear, etc.) without updating the defender-side check for whether blocking is possible.
**Why it happens:** `has_potential_attackers()` is used by auto-pass logic. If a creature can't be blocked due to Fear but this isn't checked, AI/auto-pass might incorrectly skip combat.
**How to avoid:** When adding evasion keywords, also check if `has_potential_attackers()` needs updates (it likely doesn't for blocking-side restrictions, but verify).

### Pitfall 5: Effect Handler Registration Order
**What goes wrong:** Adding a new effect handler but forgetting to register it in `build_registry()`.
**Why it happens:** The effect system uses a runtime registry, not compile-time dispatch.
**How to avoid:** Every new effect module needs a corresponding `registry.insert()` in `effects/mod.rs::build_registry()`. Add a test that checks registry length matches expected count.

### Pitfall 6: Wither/Infect Damage Through Replacement Pipeline
**What goes wrong:** Implementing Wither/Infect as simple if-checks in combat_damage.rs, bypassing replacement effects.
**Why it happens:** It seems simpler to check keywords in apply_combat_damage().
**How to avoid:** Wither and Infect modify HOW damage is dealt (as -1/-1 counters instead of marked damage). This should work through the existing `replace_event()` pipeline or be added at the damage application point carefully. Check: does damage still trigger Lifelink? (Yes, per MTG rules.)

## Tier Analysis (Research Recommendation)

### Tier 1: Combat Keywords (Plan 1) -- 4 new keywords

All actually-new (the rest are already implemented):

| Keyword | Cards | MTG Rule | Implementation |
|---------|-------|----------|----------------|
| Fear | ~200 | 702.36 | Add check in validate_blockers: only artifact/black can block |
| Intimidate | ~200 | 702.13 | Add check in validate_blockers: only artifact/share-color can block |
| Skulk | ~30 | 702.120 | Add check in validate_blockers: blocker power must be <= attacker power |
| Horsemanship | ~20 | 702.30 | Add check in validate_blockers: only horsemanship can block horsemanship |

Estimated effort: Small. All are simple boolean checks in a single function.

### Tier 2: Effect Handlers (Plan 2) -- New effect modules

| Effect | Cards Using | Priority | Notes |
|--------|------------|----------|-------|
| Mill | 153 | HIGH | Move top N from library to graveyard |
| Scry | 84 | HIGH | Look at top N, reorder/bottom |
| PumpAll | 399 | HIGH | Pump matching creatures (extend pump.rs) |
| DamageAll | 170 | HIGH | Deal damage to matching creatures/players |
| DestroyAll | 154 | HIGH | Board wipe matching creatures |
| ChangeZoneAll | 207 | MEDIUM | Bounce/exile all matching |
| Dig | 312 | MEDIUM | Reveal top N, pick some, rest to bottom/grave |
| GainControl | 119 | MEDIUM | Change controller of target permanent |

### Tier 2b: Keyword Mechanics as Effects (Plan 3)

| Mechanic | Cards | MTG Rule | Implementation |
|----------|-------|----------|----------------|
| Ward | 180 | 702.21 | Counter spell/ability unless controller pays Ward cost; add to targeting.rs |
| Protection | 63+ | 702.16 | Can't be damaged/enchanted/blocked/targeted by quality; multiple integration points |
| Wither | ~45 | 702.79 | Damage to creatures is -1/-1 counters instead; modify combat_damage.rs |
| Infect | ~45 | 702.89 | Damage to creatures as -1/-1, to players as poison; needs poison counter system |
| Prowess | 79 | 702.107 | Trigger: +1/+1 until EOT when you cast noncreature spell; add trigger matcher |

### Tier 2c: Static Ability Promotions (Plan 3)

| Static Mode | Cards | Implementation |
|-------------|-------|----------------|
| CantBeBlocked | 331 | Promote from stub; check in validate_blockers |
| CantAttack | 117 | Already has real handler; extend filter matching |
| CantBlock | 134 | Already has real handler; extend filter matching |
| MustAttack | 94 | Already has real handler; enforce in attacker declaration |
| CastWithFlash | 50 | Already has real handler; check in casting timing |

### Tier 3: Complex Subsystems (Plan 4+) -- Architecture Assessment

| Subsystem | Cards | Architecture Assessment | Recommendation |
|-----------|-------|------------------------|----------------|
| Cycling | 304 | CLEAN: Alternative activation from hand -- pay cost, discard self, draw. Fits as activated ability handler. | INCLUDE |
| Kicker | 236 | MODERATE: Optional additional cost during casting. Needs cost modification in casting.rs + tracking kicked state on stack entry. | INCLUDE if time permits |
| Flashback | 199 | MODERATE: Cast from graveyard with alternative cost. Needs graveyard-cast path in casting.rs. | INCLUDE if time permits |
| Transform/DFC | ~125 | COMPLEX: Needs back-face rendering, state toggle, day/night tracking. CardLayout::Transform exists in parser but no game state toggle. | DEFER unless architecture path is obvious |
| Morph | 153 | COMPLEX: Face-down casting at 3 generic, turn face-up for morph cost. Needs face-down state, separate casting path. | DEFER |
| Phasing | ~30 | MODERATE: Phase in/out during untap step. Needs phased-out zone or flag. | DEFER (low card count) |

### Coverage Report Approach

A Rust test or script that:
1. Loads all Forge cards from the database
2. For each card, checks if ALL its keywords have game logic (not just parsed)
3. Checks if ALL its effect types (from abilities) have registered handlers
4. Checks if ALL its trigger modes have registered matchers (not stub)
5. Reports: full support / partial support / no support
6. Outputs percentage and lists of unsupported mechanics

Best placed as an `#[ignore]` integration test in `engine/src/database/` that runs when Forge cards are available.

### Card-Level UI Warning Recommendation

**Engine-side approach** (recommended): Add a method on `GameObject` that checks if any of its keywords are in a known "unimplemented" set. The WASM bridge can expose this as a boolean flag on the serialized object.

```rust
impl GameObject {
    pub fn has_unimplemented_mechanics(&self) -> bool {
        // Check keywords against implemented set
        // Check ability api_types against effect registry
        // Check trigger modes against trigger registry
    }
}
```

This is better than client-side because:
- Engine knows exactly what it implements
- No keyword list duplication between Rust and TypeScript
- Updates automatically as mechanics are implemented

## Code Examples

### Reusable Test Helper for Forge Cards

```rust
// crates/engine/src/game/test_helpers.rs (NEW)
use crate::database::CardDatabase;
use crate::game::game_object::GameObject;
use crate::game::keywords::parse_keywords;
use crate::types::card::CardRules;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;
use std::path::Path;
use std::sync::OnceLock;

static FORGE_DB: OnceLock<Option<CardDatabase>> = OnceLock::new();

fn forge_db() -> Option<&'static CardDatabase> {
    FORGE_DB.get_or_init(|| {
        let path = Path::new("../../forge/forge-gui/res/cardsfolder/");
        if path.exists() {
            CardDatabase::load(path).ok()
        } else {
            None
        }
    }).as_ref()
}

pub fn load_card(name: &str) -> Option<&'static CardRules> {
    forge_db()?.get_by_name(name)
}

/// Create a creature on the battlefield from a Forge card definition.
pub fn spawn_creature(
    state: &mut GameState,
    name: &str,
    owner: PlayerId,
) -> Option<ObjectId> {
    let card = load_card(name)?;
    let face = card.front_face();
    let id = super::zones::create_object(
        state,
        CardId(state.next_object_id),
        owner,
        face.name.clone(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id)?;
    obj.card_types = face.card_type.clone();
    obj.keywords = parse_keywords(&face.keywords);
    if let Some(ref pt) = face.pt {
        obj.power = Some(pt.power);
        obj.toughness = Some(pt.toughness);
        obj.base_power = Some(pt.power);
        obj.base_toughness = Some(pt.toughness);
    }
    obj.entered_battlefield_turn = Some(state.turn_number.saturating_sub(1));
    Some(id)
}
```

### Test: Serra Angel Attacks Without Tapping (Vigilance)

```rust
#[test]
fn serra_angel_vigilance_integration() {
    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.active_player = PlayerId(0);

    let angel = match test_helpers::spawn_creature(&mut state, "Serra Angel", PlayerId(0)) {
        Some(id) => id,
        None => return, // Forge DB not available
    };

    state.combat = Some(CombatState::default());
    let mut events = Vec::new();
    declare_attackers(&mut state, &[angel], &mut events).unwrap();

    // Serra Angel has Vigilance -- should NOT be tapped after attacking
    assert!(!state.objects[&angel].tapped);
}
```

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in `#[test]` + `#[cfg(test)]` modules |
| Config file | Cargo.toml (workspace test settings) |
| Quick run command | `cargo test -p engine -- <test_name>` |
| Full suite command | `cargo test --all` |

### Phase Requirements -> Test Map

Since Phase 18 has no formal requirement IDs, tests map to mechanics:

| Mechanic | Test Type | Automated Command | Exists? |
|----------|-----------|-------------------|---------|
| Fear (blocking restriction) | unit | `cargo test -p engine -- combat::tests::fear` | Wave 0 |
| Intimidate (blocking restriction) | unit | `cargo test -p engine -- combat::tests::intimidate` | Wave 0 |
| Skulk (blocking restriction) | unit | `cargo test -p engine -- combat::tests::skulk` | Wave 0 |
| Horsemanship (blocking restriction) | unit | `cargo test -p engine -- combat::tests::horsemanship` | Wave 0 |
| Mill effect handler | unit | `cargo test -p engine -- effects::mill::tests` | Wave 0 |
| Scry effect handler | unit | `cargo test -p engine -- effects::scry::tests` | Wave 0 |
| Ward (targeting cost) | unit | `cargo test -p engine -- targeting::tests::ward` | Wave 0 |
| Protection (DEBT) | unit | `cargo test -p engine -- targeting::tests::protection` | Wave 0 |
| Integration (real cards) | integration | `cargo test -p engine -- --ignored` | Wave 0 |
| Coverage report | integration | `cargo test -p engine -- coverage_report --ignored` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test -p engine -- <module>::tests`
- **Per wave merge:** `cargo test --all`
- **Phase gate:** Full suite green + coverage report showing improvement

### Wave 0 Gaps
- [ ] Test helper module: `crates/engine/src/game/test_helpers.rs`
- [ ] Integration test file: `crates/engine/tests/mechanic_integration.rs` (or inline #[ignore] tests)
- [ ] Coverage report test: `crates/engine/tests/coverage_report.rs`
- [ ] Effect module files: `effects/mill.rs`, `effects/scry.rs`, etc.

## State of the Art

| Old Approach | Current Approach | Impact |
|--------------|------------------|--------|
| Fear/Intimidate as distinct mechanics | Fear is obsolete (pre-M10), Intimidate replaced it, both superseded by Menace | Implement all three for card coverage |
| Protection as single keyword | Protection from X (parameterized: color, type, quality) | Already parsed as `Keyword::Protection(ProtectionTarget)` |
| Damage prevention shields | Replacement effects via `replace_event()` pipeline | Use existing replacement infrastructure |

## Open Questions

1. **PutCounter vs AddCounter effect naming**
   - What we know: Cards use "PutCounter" (637 cards) but registry has "AddCounter"
   - What's unclear: Whether PutCounter is an alias or distinct behavior
   - Recommendation: Investigate card-data.json; likely PutCounter should map to AddCounter handler

2. **Mana effect handler**
   - What we know: 1,937 cards use "Mana" as api_type (for producing mana)
   - What's unclear: Whether the existing tap-for-mana handling in engine.rs covers this
   - Recommendation: Check if `handle_tap_land_for_mana` covers this or if a separate effect handler is needed

3. **"Effect" and "Charm" api_types**
   - What we know: 553 cards use "Effect", 517 use "Charm" -- these are Forge's generic wrapper types
   - What's unclear: What they actually resolve to
   - Recommendation: Analyze specific cards using these; likely multi-modal or indirect resolution

## Sources

### Primary (HIGH confidence)
- Direct code analysis of all engine game modules (combat.rs, combat_damage.rs, targeting.rs, sba.rs, effects/mod.rs, triggers.rs, static_abilities.rs, replacement.rs, layers.rs, keywords.rs, casting.rs)
- Frequency analysis of client/public/card-data.json (32,274 cards)
- Forge card database at /Users/matt/dev/forge/forge-gui/res/cardsfolder/

### Secondary (MEDIUM confidence)
- MTG comprehensive rules (from training data, 2025) for mechanic behavior descriptions

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - no external dependencies needed, all Rust engine work
- Architecture: HIGH - patterns are established, new mechanics follow existing conventions
- Pitfalls: HIGH - based on direct code analysis, not speculation
- Tier rankings: HIGH - based on actual frequency data from 32K card database

**Research date:** 2026-03-09
**Valid until:** Indefinite (engine architecture is stable, card frequency doesn't change)
