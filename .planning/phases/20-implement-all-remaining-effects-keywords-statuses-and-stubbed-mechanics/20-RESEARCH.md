# Phase 20: Implement All Remaining Effects, Keywords, Statuses, and Stubbed Mechanics - Research

**Researched:** 2026-03-09
**Domain:** MTG rules engine mechanic coverage (Rust), WASM bridge, React frontend interactive choices
**Confidence:** HIGH

## Summary

Phase 20 is the engine completeness phase. The codebase has a well-established registry-based architecture across four mechanic registries (effects, triggers, static abilities, replacement effects) with clear patterns for adding new handlers. The current state has 23 effect handlers, ~37 trigger matchers (with ~100+ stubbed), ~18 real static ability handlers (with ~47 stubbed), and ~13 real replacement handlers (with ~21 stubbed). The foundation systems (mana abilities, equipment/aura attachment, interactive WaitingFor choices) are the highest-priority gaps because they block the largest number of Standard-legal cards.

The architecture is pure-reducer (`apply(state, action) -> ActionResult`), function-pointer registries rebuilt per call, discriminated unions for all state transitions, and inline `#[cfg(test)]` modules. Every new mechanic follows this exact pattern. The biggest complexity is in the new WaitingFor variants (ScryChoice, DigChoice, SurveilChoice) which require engine-side state storage, WASM bridge updates, and frontend UI components.

**Primary recommendation:** Follow the foundation-first batching from CONTEXT.md. Start with mana abilities + equipment/aura + WaitingFor interactive choices (Plan 1), then layer in planeswalker/transform/DFC, then promote the large number of stubs across all four registries, then add coverage CI gating.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- 100% Standard-legal card coverage as the hard target
- Best-effort Pioneer coverage (high-frequency Pioneer mechanics included)
- Coverage report CI gate: Standard-legal subset of Forge card data checked into repo, coverage-report binary runs in CI, 0 unsupported Standard cards = pass
- Coverage report runs at phase start (baseline) and phase end (final validation), not per-plan
- Mana abilities: implement full MTG rule 605 (mana abilities resolve instantly, don't use the stack, special action resolution)
- Equipment/Aura attachment: equip/enchant mechanics, attachment validation, unattach on creature death, SBA enforcement
- WaitingFor interactive choices: proper WaitingFor::ScryChoice, DigChoice, SurveilChoice for player decisions instead of auto-choose
- Scry UI: per-card top/bottom buttons (MTGA-style), not drag-to-reorder
- Transform/DFCs: face switching, CardLayout::Transform integration, UI shows hover-to-peek back face (like MTGA)
- Planeswalker loyalty: loyalty counter activation, once-per-turn restriction, damage-to-loyalty redirection
- Day/Night & Daybound/Nightbound: global GameState field, turn-based triggers, werewolf transformation
- Morph/Manifest/Disguise: face-down 2/2 representation, turn face-up triggers, hidden information in multiplayer
- Foundation-first approach: mana abilities + equipment/aura + WaitingFor system first (bundled)
- Then planeswalker loyalty + transform/DFCs
- Then static abilities (promote stubs to real handlers)
- Then trigger matchers
- Then effect handlers
- Then replacement effects
- Then complex subsystems (day/night, morph/manifest)
- Then coverage CI gate
- Full AI evaluation for interactive choices (not simplified heuristics)
- MTG comprehensive rules as primary authority; Forge Java as reference, not authoritative
- Unit tests: 2-5 inline Rust tests per mechanic; Integration tests: real Forge card definitions via test_helpers

### Claude's Discretion
- Exact plan count and grouping within plans (informed by dependency analysis)
- Choice UI pattern (generic modal vs per-mechanic) for Dig/Explore/Surveil
- Which Pioneer mechanics fall out naturally vs need explicit work
- AI evaluation depth for each choice type
- Exact ordering within each plan's batch

### Deferred Ideas (OUT OF SCOPE)
None -- discussion stayed within phase scope
</user_constraints>

## Standard Stack

### Core (Rust Engine)
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| `rpds` | (existing) | Persistent data structures for immutable game state | Already in use, structural sharing |
| `serde` + `serde_json` | (existing) | Serialization for WASM boundary | All types derive Serialize/Deserialize |
| `indexmap` | (existing) | Ordered replacement registry | Used in replacement.rs already |
| `petgraph` | (existing) | Dependency-aware layer ordering | Used in layers.rs already |
| `rand` + `rand_chacha` | (existing) | Deterministic RNG | Seeded RNG for reproducible games |
| `tempfile` | (existing) | Test temp directories | Coverage test infrastructure |

### Frontend (React)
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| React | (existing) | UI components | New choice modals |
| Zustand | (existing) | State management | gameStore WaitingFor handling |
| Framer Motion | (existing) | Animations | Choice card animations |
| Tailwind CSS v4 | (existing) | Styling | Choice UI styling |

No new dependencies needed. This phase is entirely about extending existing registries and patterns.

## Architecture Patterns

### Current Registry Pattern (ALL new mechanics follow this)

Each of the four mechanic registries follows the same fn-pointer-in-HashMap pattern:

```
Registry Type         | Key Type              | Handler Signature
--------------------- | --------------------- | -----------------
Effect handlers       | String (api_type)     | fn(&mut GameState, &ResolvedAbility, &mut Vec<GameEvent>) -> Result<(), EffectError>
Trigger matchers      | TriggerMode (enum)    | fn(&GameEvent, &HashMap<String,String>, ObjectId, &GameState) -> bool
Static ability        | String (mode)         | fn(&GameState, &HashMap<String,String>, ObjectId) -> Vec<StaticEffect>
Replacement effects   | String (event type)   | ReplacementHandlerEntry { matcher, applier }
```

### Pattern: Adding a New Effect Handler
```rust
// 1. Create effect module: crates/engine/src/game/effects/{name}.rs
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Parse params from ability.params
    // Modify state
    // Push events
    Ok(())
}

// 2. Register in effects/mod.rs build_registry():
registry.insert("EffectName".to_string(), effect_name::resolve);

// 3. Add inline tests in the module
```

### Pattern: Promoting a Stub Static Ability
```rust
// In static_abilities.rs:
// 1. Move mode from stubs array to explicit handler
// 2. Implement real handler function
// 3. Register: registry.insert("ModeName".to_string(), handle_mode_name);

fn handle_indestructible(
    _state: &GameState,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Indestructible".to_string(),
    }]
}
```

### Pattern: Adding New WaitingFor Variant
```rust
// 1. Add variant to WaitingFor enum in types/game_state.rs:
WaitingFor::ScryChoice {
    player: PlayerId,
    cards: Vec<ObjectId>,
    // cards to decide top/bottom placement
}

// 2. Add handler in engine.rs apply() match:
(WaitingFor::ScryChoice { player }, GameAction::SelectCards { cards }) => { ... }

// 3. Add TypeScript type in client/src/adapter/types.ts:
| { type: "ScryChoice"; data: { player: PlayerId; cards: ObjectId[] } }

// 4. Add GameAction variant if needed
// 5. Add frontend component to render the choice
```

### Pattern: Mana Abilities (Rule 605 -- NEW SYSTEM)
Mana abilities are special actions that don't use the stack. They need:
```
1. Detection: Check if an activated ability produces mana (has "Mana" in api_type or produces ManaAdded event)
2. Resolution: Execute immediately (no stack push, no priority pass)
3. Integration: In casting.rs, before auto_tap_lands, allow mana ability activation
4. GameState: No new fields needed -- mana goes directly into player.mana_pool
```

### Pattern: Equipment/Aura Attachment
```
1. GameAction::Equip { equipment_id, target_id }
2. WaitingFor::EquipTarget { player, equipment_id, valid_targets }
3. Validate: target must be creature, controller must own equipment
4. State: set equipment.attached_to = target, target.attachments.push(equipment)
5. SBA: equipment falls off when target dies (existing check_unattached_auras, extend)
6. Effect: Equip ability = activated ability with tap cost + mana cost
```

### Pattern: Planeswalker Loyalty Abilities
```
1. GameAction::ActivateAbility -- check if source is planeswalker
2. Loyalty cost: modify counter instead of mana payment
3. Once-per-turn tracking: new field on GameObject or state tracking
4. Damage redirect: in combat_damage and deal_damage, check if target is planeswalker
5. SBA 704.5i: Planeswalker with 0 loyalty goes to graveyard
```

### Anti-Patterns to Avoid
- **Don't hand-roll mana ability stack interaction**: Mana abilities NEVER use the stack per Rule 605. Don't add them to the stack and skip priority -- implement them as a completely separate code path.
- **Don't duplicate attachment logic**: Equipment and Aura attachment share validation logic. Use a shared `attach` helper, not separate code paths.
- **Don't auto-resolve interactive choices**: The CONTEXT explicitly says AI should make proper decisions for Scry/Dig/Surveil, not simplified heuristics. Always emit a WaitingFor for the choice.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Coverage analysis | Custom card scanner | Existing `coverage.rs` + `coverage_report.rs` | Already has all four registry checks |
| Filter matching | New filter parser | Existing `filter::object_matches_filter` and `filter::player_matches_filter` | Shared across effects, triggers, statics |
| Sub-ability chains | Manual recursion | Existing `resolve_ability_chain` in effects/mod.rs | Handles SubAbility/Execute with depth limit |
| Layer evaluation | Manual P/T calculation | Existing `layers::evaluate_layers` | Full 7-layer system with dependency ordering |
| Forge card parsing | Manual text parsing | Existing `parser::ability::{parse_ability, parse_trigger, parse_static}` | Robust Forge format parser |

**Key insight:** The engine already has the infrastructure. Phase 20 is about adding handlers to existing registries, not building new systems (except mana abilities and attachment).

## Common Pitfalls

### Pitfall 1: Mana Abilities Must Not Use the Stack
**What goes wrong:** Implementing mana abilities as activated abilities that go on the stack causes rules violations. "T: Add {G}" should resolve instantly.
**Why it happens:** The engine's existing ActivateAbility path always pushes to the stack.
**How to avoid:** Create a separate `handle_mana_ability` function in casting.rs or a new `mana_abilities.rs` module that bypasses the stack entirely. Check ability definition for mana-producing indicator before routing.
**Warning signs:** If `state.stack` grows when tapping a land for mana beyond the current auto_tap_lands shortcut.

### Pitfall 2: WaitingFor Variants Must Be Exhaustive in apply()
**What goes wrong:** Adding a new WaitingFor variant without handling all possible GameActions in that state causes panics or silent failures.
**Why it happens:** The `apply` function uses a match on `(&state.waiting_for, action)` pairs. Missing arms lead to EngineError or unreachable states.
**How to avoid:** For each new WaitingFor variant, define ALL valid actions (including CancelCast if applicable) and add exhaustive match arms in engine.rs.
**Warning signs:** The wildcard `_` match at the bottom of apply() catching new states.

### Pitfall 3: WASM Bridge Type Synchronization
**What goes wrong:** Adding new Rust types (WaitingFor variants, GameEvent variants, GameAction variants) without updating the TypeScript types in `client/src/adapter/types.ts` causes runtime deserialization failures.
**Why it happens:** The WASM bridge uses serde-wasm-bindgen serialization. TypeScript types are manually maintained to match Rust serde output.
**How to avoid:** For EVERY new enum variant or struct field added to Rust types, add the corresponding TypeScript type immediately. Consider adding a test that serializes all variants and checks they parse on the JS side.
**Warning signs:** `undefined` in UI when processing new WaitingFor or GameEvent types.

### Pitfall 4: Attachment Cascading SBA
**What goes wrong:** Equipment/aura removal when host creature dies can trigger cascading SBA issues if not handled in the right order.
**Why it happens:** The SBA fixpoint loop in sba.rs runs up to 9 iterations. If attachments create new invalid states (aura on dead creature), those must be caught in subsequent iterations.
**How to avoid:** The existing `check_unattached_auras` already handles auras. Extend it to also cover equipment. Ensure the fixpoint loop naturally handles cascading attachment deaths.
**Warning signs:** Orphaned attachments remaining on battlefield after host creature dies.

### Pitfall 5: DFC/Transform State on Zone Change
**What goes wrong:** A transformed card that dies and returns from graveyard might retain its transformed state incorrectly.
**Why it happens:** Rule 711.8 says a DFC that changes zones becomes front face up, but the `transformed` flag on GameObject might persist.
**How to avoid:** In `zones::move_to_zone`, reset `transformed` flag to false when entering any zone except battlefield (or when leaving battlefield).
**Warning signs:** Cards in graveyard or hand showing their back face.

### Pitfall 6: Coverage Report Not Filtering Standard-Legal Cards
**What goes wrong:** The coverage report counts ALL Forge cards (including Un-sets, Alchemy, Commander-only), making 100% coverage impossible.
**Why it happens:** `analyze_standard_coverage` in coverage.rs doesn't filter by set legality -- it scans everything.
**How to avoid:** Check Standard-legal card subset into the repo (per CONTEXT decision). The CI gate should run the coverage report only against this curated subset, not the full Forge database.
**Warning signs:** Coverage percentage far below expectations despite implementing all Standard mechanics.

## Code Examples

### Adding a New Effect Handler (e.g., Surveil)
```rust
// crates/engine/src/game/effects/surveil.rs
use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let amount: usize = ability.params
        .get("Amount")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    let player = &state.players[ability.controller.0 as usize];
    let top_cards: Vec<ObjectId> = player.library.iter()
        .rev()
        .take(amount)
        .copied()
        .collect();

    // Emit WaitingFor::SurveilChoice instead of auto-resolving
    // Engine returns this and frontend shows the choice UI
    // (Actual implementation stores pending state and returns WaitingFor)
    Ok(())
}
```

### Adding a New WaitingFor + Engine Handler
```rust
// In types/game_state.rs WaitingFor enum:
ScryChoice {
    player: PlayerId,
    cards: Vec<ObjectId>,
},

// In engine.rs apply() match:
(WaitingFor::ScryChoice { player }, GameAction::SelectCards { cards }) => {
    // cards = ObjectIds to put on top (remainder go to bottom)
    // Reorder library accordingly
    // Return to priority
    WaitingFor::Priority { player: *player }
}
```

### Promoting a Stub Trigger Matcher
```rust
// In triggers.rs -- move from unimplemented_modes to real matcher:
r.insert(TriggerMode::AttackerBlocked, match_attacker_blocked);

fn match_attacker_blocked(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::BlockersDeclared { assignments } = event {
        // Check if source_id is among the blocked attackers
        assignments.iter().any(|(_, attacker)| *attacker == source_id)
    } else {
        false
    }
}
```

## State of the Art

### Current Engine Registry Counts
| Registry | Total Entries | Real Handlers | Stubs/No-ops | Real % |
|----------|-------------|---------------|-------------|--------|
| Effect handlers | 23 | 23 | 0 | 100% |
| Trigger matchers | ~137 | ~37 | ~100 | 27% |
| Static abilities | ~66 | ~18 | ~47 | 27% |
| Replacement effects | 35 | 13 | 22 | 37% |

### Key Missing Systems (Foundational)
| System | Status | Impact |
|--------|--------|--------|
| Mana abilities (Rule 605) | Not implemented -- lands use auto_tap shortcut | Blocks every card with "T: Add {M}" on non-basic lands |
| Equipment attach/equip | Fields exist (attached_to, attachments) but no equip action | Blocks all Equipment cards |
| Aura enchant targeting | Partial -- SBA handles detachment, no enchant-on-cast | Blocks Aura targeting during cast |
| Interactive Scry | Simplified (all to bottom) | Incorrect behavior for Scry |
| Interactive Dig | Simplified (first N to hand) | Incorrect behavior for Dig |
| Planeswalker loyalty abilities | loyalty field exists, no activation mechanic | Blocks all Planeswalker cards |
| Transform/DFC | transformed field exists, no face-switching logic | Blocks all Transform/DFC cards |
| Day/Night | No global state field | Blocks Daybound/Nightbound cards |
| Morph/Manifest | face_down field exists, no play-face-down action | Blocks Morph/Manifest cards |

### Key Missing Effect Handlers
Based on Forge card data frequency analysis, the most-needed missing effect handlers for Standard coverage would typically include: `Surveil`, `Explore`, `Fight`, `Bounce` (return to hand), `Exile and Return`, `Attach`, `CopySpell`, `ChooseCard`, `Sacrifice` (forced opponent sacrifice), `Fateseal`, `Proliferate`, `Amass`. The exact list should be determined by running the coverage report at phase start (per CONTEXT decision).

### Key Missing Trigger Matchers (High Frequency for Standard)
- `AttackerBlocked` / `AttackerBlockedOnce` -- creatures with "when blocked" triggers
- `Cycled` -- cycling trigger support
- `TurnFaceUp` / `Transformed` -- DFC/morph triggers
- `Attached` / `Unattach` -- equipment/aura event triggers
- `DayTimeChanges` -- day/night cycle triggers

### Key Missing Static Ability Real Handlers (High Frequency)
Currently stubbed but high-impact for Standard:
- `Flying`, `Trample`, `Deathtouch`, `Lifelink`, `Vigilance`, `Menace`, `Reach` -- These are in the stubs list but are actually used through keywords.rs/combat.rs already (the static registry stubs are for static-granted versions, not keyword versions)
- `Indestructible` -- needs real SBA integration (partially done via has_indestructible keyword check)
- `FlashBack` -- flashback casting from graveyard
- `ETBReplacement` -- enters-the-battlefield replacement effects
- `CantBeCountered` -- spell uncounterability

## Dependency Analysis for Plan Ordering

```
Plan 1: Foundation (mana abilities + equipment/aura + WaitingFor system)
  ├─ Mana abilities: no dependencies, highest impact
  ├─ Equipment/Aura: needs SBA extension, attachment logic
  └─ WaitingFor choices: needs new WaitingFor variants, engine handlers, WASM bridge, frontend components

Plan 2: Planeswalker + Transform/DFC
  ├─ Planeswalker loyalty: depends on counter system (exists), needs SBA for 0 loyalty
  ├─ Transform/DFC: depends on CardLayout::Transform (exists), needs face-switching in engine
  └─ UI: hover-to-peek back face for DFCs

Plan 3: Static abilities (promote stubs)
  ├─ Depends on: combat.rs keyword checks (partial), layers.rs (exists)
  └─ Promotes: ~47 stubs to real handlers (many are rule-modification mode)

Plan 4: Trigger matchers (promote stubs)
  ├─ Depends on: GameEvent variants (most exist already)
  └─ Promotes: ~100 stub matchers to real logic

Plan 5: Effect handlers (add missing)
  ├─ Depends on: WaitingFor system (Plan 1) for interactive effects
  └─ Adds: Surveil, Explore, Fight, Bounce, etc.

Plan 6: Replacement effects (promote stubs)
  ├─ Depends on: ProposedEvent variants (most exist)
  └─ Promotes: ~21 stubs to real handlers

Plan 7: Complex subsystems (Day/Night, Morph/Manifest/Disguise)
  ├─ Day/Night: needs global GameState field, turn-based trigger
  └─ Morph: needs face-down play action, turn-up trigger, hidden info extension

Plan 8: Coverage CI gate
  ├─ Depends on: all previous plans
  └─ Standard card subset checked in, coverage-report runs in CI
```

**Estimated plan count: 8** (matching the batching order from CONTEXT.md)

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust inline `#[cfg(test)]` + cargo test |
| Config file | Cargo.toml workspace (existing) |
| Quick run command | `cargo test -p engine -- {test_name}` |
| Full suite command | `cargo test --all` |

### Phase Requirements -> Test Map
Since phase requirement IDs are TBD, mapping based on CONTEXT.md systems:

| System | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| Mana abilities | Land tap resolves instantly without stack | unit | `cargo test -p engine -- test_mana_ability -x` | Wave 0 |
| Equipment equip | Equip attaches equipment to creature | unit | `cargo test -p engine -- test_equip -x` | Wave 0 |
| Scry choice | ScryChoice WaitingFor emitted, player selects top/bottom | unit | `cargo test -p engine -- test_scry_choice -x` | Wave 0 |
| Planeswalker loyalty | Loyalty ability costs counter, once per turn | unit | `cargo test -p engine -- test_planeswalker -x` | Wave 0 |
| Transform DFC | Card flips between faces | unit | `cargo test -p engine -- test_transform -x` | Wave 0 |
| Static promotions | Promoted statics return correct effects | unit | `cargo test -p engine -- test_static -x` | Partial (3 promoted) |
| Trigger promotions | Promoted triggers fire on correct events | unit | `cargo test -p engine -- test_trigger -x` | Partial (37 real) |
| Coverage CI gate | 0 unsupported Standard cards | integration | `cargo run --bin coverage_report -- <path>` | Exists (extend) |

### Sampling Rate
- **Per task commit:** `cargo test -p engine -- {relevant_test_prefix} -x`
- **Per wave merge:** `cargo test --all`
- **Phase gate:** Full suite green + coverage report shows 100% Standard

### Wave 0 Gaps
- [ ] Standard-legal card subset data file -- needs to be curated and checked into repo
- [ ] Integration tests for mana abilities with real Forge cards (extend test_helpers.rs)
- [ ] Integration tests for equipment with real Forge cards
- [ ] Frontend test for ScryChoice component (Vitest)

## Open Questions

1. **Standard-legal card subset curation**
   - What we know: Coverage report exists, scans all Forge cards. CONTEXT says Standard subset checked into repo.
   - What's unclear: Which Standard sets to include, format of the subset file, whether to use Forge set codes or a manual list.
   - Recommendation: Create a script that filters Forge card data by set legality (using Forge's Edition files or a hardcoded Standard-legal set list). Output a directory of .txt files for CI.

2. **Exact count of missing effect handlers for Standard**
   - What we know: 23 effect handlers exist. Coverage report can tell us which api_types are missing.
   - What's unclear: Exact count until we run the coverage report against Standard-legal cards.
   - Recommendation: Run baseline coverage report as first task of Plan 1 to get exact numbers. This will inform plan sizing for Plans 3-6.

3. **Mana ability detection heuristic**
   - What we know: Rule 605 defines mana abilities. Forge cards encode this in ability text.
   - What's unclear: Whether Forge's ability format has an explicit "mana ability" marker or if we need to detect it from the effect type (e.g., api_type containing "Mana" or producing ManaAdded).
   - Recommendation: Check Forge's ability format for mana ability indicators. Likely the presence of "Mana" in Cost$ or api_type "Mana" / "ProduceMana".

4. **AI evaluation for interactive choices**
   - What we know: forge-ai crate has evaluation + card hints + game tree search. CONTEXT says full AI evaluation, not simplified heuristics.
   - What's unclear: Whether existing AI infrastructure can handle new WaitingFor types without significant extension.
   - Recommendation: The AI already handles MulliganDecision and other choices. New WaitingFor types need corresponding action selection in forge-ai. Check the AI action selection code.

## Sources

### Primary (HIGH confidence)
- `crates/engine/src/game/effects/mod.rs` -- Effect registry pattern, 23 handlers, resolve_ability_chain
- `crates/engine/src/game/static_abilities.rs` -- Static registry, 18 real + 47 stubs, check_static_ability
- `crates/engine/src/game/triggers.rs` -- Trigger registry, 37 real + ~100 stubs, process_triggers
- `crates/engine/src/game/replacement.rs` -- Replacement registry, 13 real + 22 stubs, replace_event pipeline
- `crates/engine/src/game/engine.rs` -- Main apply() reducer, WaitingFor dispatch
- `crates/engine/src/game/coverage.rs` -- Coverage analysis, has_unimplemented_mechanics
- `crates/engine/src/game/game_object.rs` -- GameObject fields (attached_to, face_down, transformed, loyalty)
- `crates/engine/src/types/game_state.rs` -- WaitingFor enum, GameState fields
- `crates/engine/src/types/actions.rs` -- GameAction enum
- `crates/engine/src/types/keywords.rs` -- 100+ keyword variants, FromStr parsing
- `crates/engine/src/types/triggers.rs` -- 137+ trigger mode variants
- `crates/engine/src/game/sba.rs` -- State-based actions (6 checks, fixpoint loop)
- `crates/engine/src/game/casting.rs` -- Spell casting, auto_tap_lands, pay_and_push
- `crates/engine/src/game/mana_payment.rs` -- Mana payment, can_pay, pay_cost, produce_mana
- `crates/engine/src/game/layers.rs` -- 7-layer system with dependency ordering
- `client/src/adapter/types.ts` -- TypeScript types matching Rust serde output

### Secondary (MEDIUM confidence)
- MTG Comprehensive Rules (Rule 605 mana abilities, Rule 704.5 SBA, Rule 613 layers, Rule 711 DFCs)
- Forge Java source code (reference implementation for card ability format)

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- no new dependencies, all existing patterns
- Architecture: HIGH -- registry pattern thoroughly verified across 4 registries, 1500+ lines of existing code
- Pitfalls: HIGH -- identified from direct code inspection of engine.rs, casting.rs, sba.rs
- Dependency ordering: HIGH -- derived from code analysis of actual dependencies between systems
- Coverage system: HIGH -- coverage.rs and coverage_report.rs binary fully inspected

**Research date:** 2026-03-09
**Valid until:** 2026-04-09 (stable architecture, no external dependencies)
