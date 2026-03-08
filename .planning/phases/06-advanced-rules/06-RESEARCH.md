# Phase 6: Advanced Rules - Research

**Researched:** 2026-03-07
**Domain:** MTG replacement effects (CR 614-616), seven-layer continuous effects (CR 613), static abilities
**Confidence:** HIGH

## Summary

Phase 6 implements two major MTG rules subsystems: replacement effects and the seven-layer continuous effect system. Both are among the most complex areas of the MTG comprehensive rules and require careful architectural decisions.

The replacement pipeline intercepts game events before they happen, allowing effects like "if a creature would die, exile it instead" to modify or redirect events. This requires a `ProposedEvent` abstraction layer between intent and execution, with player choice when multiple replacements apply. The layer system evaluates continuous effects in strict order (copy, control, text, type, color, ability, P/T) with dependency-aware ordering within each layer.

The existing codebase provides strong foundations: `StaticDefinition` and `ReplacementDefinition` types already exist in the parser, the `WaitingFor`/`GameAction` state machine pattern is battle-tested through 5 phases, and the registry-per-call pattern from effects/triggers maps directly to replacement and static ability handlers. All 347 existing tests pass, so regression detection is straightforward.

**Primary recommendation:** Build replacement effects first (they are simpler to validate in isolation), then layer evaluation (which depends on replacement for some edge cases like "as enters the battlefield" effects). Use petgraph for intra-layer dependency toposort. Use indexmap for deterministic candidate ordering.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Central `replace_event()` function called from every mutation site (damage, zone moves, draw, life change, counters, token creation, discard, tap/untap)
- All mutation sites get interception hooks -- not a minimal subset
- Follow Forge's `ReplacementHandler` pattern and CR 616 flow
- Prevention effects (CR 615) are a subtype of replacement -- same pipeline, same once-per-event tracking, same choice logic
- When multiple replacements apply, engine sets `WaitingFor::ReplacementChoice` and returns -- consistent with target/attacker/blocker choice patterns
- Pending replacement context stored on `GameState` for WaitingFor round-trip
- Full re-evaluation of candidate list after each replacement applies (CR 616.1)
- Once-per-event enforcement per CR 616.1f
- Each replacement application emits a `ReplacementApplied` GameEvent for tracing/game log
- Nested replacements supported, depth cap of 16
- HashMap<String, ReplacementHandler> built per call -- same pattern as effect/trigger registries
- Replacement handlers can emit side-effect events that feed into the trigger pipeline
- Separate `ProposedEvent` enum with exhaustive typed variants
- Applied-replacements tracking (HashSet) lives on ProposedEvent itself
- After pipeline completes, ProposedEvent converts to actual mutations + GameEvents
- All 7 layers implemented: copy (1), control (2), text (3), type (4), color (5), ability (6), P/T (7a-7e)
- petgraph toposort for intra-layer dependency ordering; cycle fallback to timestamp ordering per CR 613.8
- Global monotonic u64 counter on GameState for timestamp tracking (CR 613.7)
- Base + computed fields on GameObject -- base_power/base_toughness vs computed power/toughness/keywords/etc
- Keyword grants from static abilities computed during layer 6 evaluation
- `static_definitions: Vec<StaticDefinition>` and `replacement_definitions: Vec<ReplacementDefinition>` on GameObject
- Active effects determined by zone-filtered scan (zone == Battlefield, plus special cases)
- Inside a layer, dependency ordering takes precedence over timestamp ordering
- Source/condition-based effects stop applying when source/condition is no longer true

### Claude's Discretion
- Layer evaluation timing strategy (on-demand before checks vs immediate on state change)
- EventId approach (monotonic counter vs structural identity) for once-per-event tracking
- 'Instead' replacement handling (event nullified flag vs return enum from pipeline)
- Self-replacement effect routing ('as enters' vs 'if would' -- same pipeline or separate pre-hook)
- Deterministic fallback key design when Forge/MTG do not define an ordering tie-break
- Exact `WaitingFor` and `GameAction` variant shapes for replacement-choice prompts
- Exact split of canonical coverage vs additional handlers across plans

### Deferred Ideas (OUT OF SCOPE)
- Any UI-facing replacement ordering UX (belongs with Phase 7 platform/UI work)
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| REPL-01 | Replacement effect pipeline intercepting events before they resolve | ProposedEvent enum, replace_event() function, mutation site hooks at all 14 callsites |
| REPL-02 | Per-event application tracking (each replacement modifies an event only once) | HashSet on ProposedEvent tracking ReplacementId, CR 616.1f enforcement |
| REPL-03 | Player choice when multiple replacements apply | WaitingFor::ReplacementChoice, PendingReplacement on GameState, GameAction::ChooseReplacement |
| REPL-04 | All 45 replacement effect handlers | Registry pattern matching effects/triggers, Forge's 35 ReplacementType variants as reference |
| STAT-01 | Seven-layer continuous effect evaluation per Rule 613 | Layer enum (1-7e), per-layer evaluation, base vs computed fields on GameObject |
| STAT-02 | Timestamp ordering within layers | Global monotonic u64 timestamp counter on GameState (CR 613.7) |
| STAT-03 | Intra-layer dependency detection | petgraph toposort with cycle fallback to timestamp per CR 613.8 |
| STAT-04 | All 61 static ability type handlers | Registry pattern, Forge's ~65 StaticAbilityMode variants as reference |
</phase_requirements>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| petgraph | 0.6 | Dependency graph + toposort for layer ordering | Standard Rust graph library; toposort handles cycle detection automatically |
| indexmap | 2.x | Deterministic-order maps for candidate lists and fallback ordering | Insertion-order iteration prevents nondeterministic behavior from HashMap |

### Supporting (already in project)
| Library | Version | Purpose |
|---------|---------|---------|
| serde | 1.x | Serialization for new types (ProposedEvent, Layer, PendingReplacement) |
| thiserror | 2.x | Error types for replacement/layer errors |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| petgraph | Manual toposort | petgraph handles cycle detection; manual is error-prone for dependency DAGs |
| indexmap | Sorted Vec | indexmap is cleaner API for map-like access with deterministic ordering |

**Installation:**
```bash
# Add to crates/engine/Cargo.toml [dependencies]
petgraph = "0.6"
indexmap = "2"
```

## Architecture Patterns

### Recommended Project Structure
```
crates/engine/src/
├── game/
│   ├── replacement.rs       # replace_event() pipeline, handler type, registry builder
│   ├── replacement/         # Per-event-type replacement handlers (45 total)
│   │   ├── mod.rs           # ReplacementMatcher type, build_registry()
│   │   ├── damage.rs        # DamageDone replacement
│   │   ├── zone_change.rs   # Moved replacement
│   │   ├── draw.rs          # Draw replacement
│   │   ├── destroy.rs       # Destroy replacement
│   │   ├── life.rs          # GainLife, LifeReduced
│   │   ├── counter.rs       # AddCounter, RemoveCounter
│   │   └── ...              # Remaining handlers
│   ├── layers.rs            # evaluate_layers(), Layer enum, ordering logic
│   └── static_abilities.rs  # Static ability handler registry + Continuous mode application
├── types/
│   ├── proposed_event.rs    # ProposedEvent enum with all typed variants
│   └── layers.rs            # Layer enum, ContinuousEffect struct
```

### Pattern 1: ProposedEvent Pipeline (REPL-01)
**What:** Every game mutation passes through `replace_event()` before executing.
**When to use:** ALL mutation sites.

```rust
/// Unique identity for a replacement definition on a specific object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReplacementId {
    pub source: ObjectId,
    pub index: usize,
}

/// Result of running the replacement pipeline.
pub enum ReplacementResult {
    /// Event was not modified or was modified but should proceed.
    Execute(ProposedEvent),
    /// Event was fully prevented (e.g., damage prevention).
    Prevented,
    /// Player choice needed -- engine should return WaitingFor.
    NeedsChoice(PlayerId),
}

pub fn replace_event(
    state: &mut GameState,
    proposed: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> ReplacementResult {
    let registry = build_replacement_registry();
    let mut current = proposed;
    let mut depth: u16 = 0;

    loop {
        if depth >= 16 {
            break;
        }

        let candidates = find_applicable_replacements(state, &current, &registry);
        // Filter already-applied
        let candidates: Vec<_> = candidates.into_iter()
            .filter(|c| !current.already_applied(c))
            .collect();

        if candidates.is_empty() {
            break;
        }

        if candidates.len() == 1 {
            let id = candidates[0];
            current.mark_applied(id);
            let apply_result = apply_replacement(state, current, &id, &registry, events);
            match apply_result {
                ApplyResult::Modified(next) => current = next,
                ApplyResult::Prevented => return ReplacementResult::Prevented,
            }
            events.push(GameEvent::ReplacementApplied { /* ... */ });
        } else {
            // Store context for WaitingFor round-trip
            let affected_player = current.affected_player();
            state.pending_replacement = Some(PendingReplacement {
                proposed: current,
                candidates,
                depth,
            });
            return ReplacementResult::NeedsChoice(affected_player);
        }

        depth += 1;
    }

    ReplacementResult::Execute(current)
}
```

### Pattern 2: ProposedEvent Enum (REPL-01, REPL-02)
**What:** Typed event variants carrying mutable fields for replacement modification.
**When to use:** Created at each mutation site before the mutation actually occurs.

```rust
#[derive(Debug, Clone)]
pub enum ProposedEvent {
    ZoneChange {
        object_id: ObjectId,
        from: Zone,
        to: Zone,
        cause: Option<ObjectId>,
        applied: HashSet<ReplacementId>,
    },
    Damage {
        source_id: ObjectId,
        target: TargetRef,
        amount: u32,
        is_combat: bool,
        applied: HashSet<ReplacementId>,
    },
    Draw {
        player_id: PlayerId,
        count: u32,
        applied: HashSet<ReplacementId>,
    },
    LifeGain {
        player_id: PlayerId,
        amount: u32,
        applied: HashSet<ReplacementId>,
    },
    LifeLoss {
        player_id: PlayerId,
        amount: u32,
        applied: HashSet<ReplacementId>,
    },
    AddCounter {
        object_id: ObjectId,
        counter_type: String,
        count: u32,
        applied: HashSet<ReplacementId>,
    },
    RemoveCounter {
        object_id: ObjectId,
        counter_type: String,
        count: u32,
        applied: HashSet<ReplacementId>,
    },
    CreateToken {
        owner: PlayerId,
        name: String,
        applied: HashSet<ReplacementId>,
    },
    Discard {
        player_id: PlayerId,
        object_id: ObjectId,
        applied: HashSet<ReplacementId>,
    },
    Tap {
        object_id: ObjectId,
        applied: HashSet<ReplacementId>,
    },
    Untap {
        object_id: ObjectId,
        applied: HashSet<ReplacementId>,
    },
    Destroy {
        object_id: ObjectId,
        source: Option<ObjectId>,
        applied: HashSet<ReplacementId>,
    },
    Sacrifice {
        object_id: ObjectId,
        player_id: PlayerId,
        applied: HashSet<ReplacementId>,
    },
}

impl ProposedEvent {
    pub fn already_applied(&self, id: &ReplacementId) -> bool {
        self.applied_set().contains(id)
    }
    pub fn mark_applied(&mut self, id: ReplacementId) {
        self.applied_set_mut().insert(id);
    }
    pub fn affected_player(&self) -> PlayerId { /* varies by variant */ }
    // Helper to access the applied HashSet regardless of variant
    fn applied_set(&self) -> &HashSet<ReplacementId> { /* match on self */ }
    fn applied_set_mut(&mut self) -> &mut HashSet<ReplacementId> { /* match on self */ }
}
```

### Pattern 3: Layer Evaluation (STAT-01, STAT-02, STAT-03)
**What:** Recompute all continuous effects through the seven-layer system.
**When to use:** Before any characteristic-dependent check.

**Recommendation (Claude's Discretion -- timing):** On-demand evaluation before SBAs, target legality checks, and combat damage. Track a `layers_dirty` flag on GameState, set when battlefield changes occur. This avoids redundant recomputation while ensuring correctness.

**Recommendation (Claude's Discretion -- "instead" handling):** Return enum from pipeline (`ReplacementResult::Prevented` for full prevention, `ReplacementResult::Execute(modified)` for modification). Cleaner than a nullified flag.

**Recommendation (Claude's Discretion -- EventId):** Use monotonic u64 counter. Simpler than structural identity, no hash collision risk, trivially deterministic.

**Recommendation (Claude's Discretion -- "as enters" routing):** Same pipeline with self-replacement effects getting first-pass priority. Forge does this with LKI copies. Our pipeline handles it by checking self-replacement effects first in the candidate list.

**Recommendation (Claude's Discretion -- deterministic fallback):** Sort by `(timestamp, source_object_id.0, definition_index)` as the tiebreaker triple. Fully deterministic, stable across runs.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    Copy,       // 1
    Control,    // 2
    Text,       // 3
    Type,       // 4
    Color,      // 5
    Ability,    // 6
    CharDef,    // 7a - characteristic-defining abilities
    SetPT,      // 7b - set power/toughness
    ModifyPT,   // 7c - modify (+N/+N)
    SwitchPT,   // 7d - switch power/toughness
    CounterPT,  // 7e - counters (applied implicitly)
}

impl Layer {
    pub fn all() -> &'static [Layer] {
        &[
            Layer::Copy, Layer::Control, Layer::Text, Layer::Type,
            Layer::Color, Layer::Ability, Layer::CharDef, Layer::SetPT,
            Layer::ModifyPT, Layer::SwitchPT, Layer::CounterPT,
        ]
    }

    /// Layers that can have dependency ordering per Forge/CR.
    pub fn has_dependency_ordering(&self) -> bool {
        matches!(self,
            Layer::Copy | Layer::Control | Layer::Text | Layer::Type |
            Layer::Ability | Layer::CharDef | Layer::SetPT
        )
    }
}

pub fn evaluate_layers(state: &mut GameState) {
    // Step 1: Reset all computed characteristics to base values
    for obj_id in state.battlefield.clone() {
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            obj.power = obj.base_power;
            obj.toughness = obj.base_toughness;
            obj.keywords = obj.base_keywords.clone();
            obj.color = obj.base_color.clone();
            // card_types reset handled similarly
        }
    }

    // Step 2: Gather active continuous effects
    let effects = gather_active_continuous_effects(state);

    // Step 3: Apply through each layer
    for layer in Layer::all() {
        let layer_effects: Vec<_> = effects.iter()
            .filter(|e| e.applies_in_layer(*layer))
            .cloned()
            .collect();

        if layer_effects.is_empty() {
            continue;
        }

        let ordered = if layer.has_dependency_ordering() {
            order_with_dependencies(&layer_effects, state)
        } else {
            order_by_timestamp(&layer_effects)
        };

        for effect in &ordered {
            apply_continuous_effect(state, effect, *layer);
        }
    }

    // Step 4: Apply counter-based P/T modifications (layer 7e)
    apply_counter_pt(state);
}
```

### Pattern 4: Base vs Computed Fields on GameObject (STAT-01)
**What:** Separate printed/base values from computed values produced by layer evaluation.

```rust
// Added to GameObject struct:
pub base_power: Option<i32>,       // Printed power (set at creation from CardFace)
pub base_toughness: Option<i32>,   // Printed toughness
pub base_keywords: Vec<Keyword>,   // Printed keywords
pub base_color: Vec<ManaColor>,    // Printed colors
// Existing power/toughness/keywords/color become computed (output of layer evaluation)

pub static_definitions: Vec<StaticDefinition>,       // From CardFace S: lines
pub replacement_definitions: Vec<ReplacementDefinition>, // From CardFace R: lines
pub timestamp: u64,                // Monotonic timestamp for layer ordering
```

### Pattern 5: Replacement Handler Registry (REPL-04)
**What:** Function pointer registry for replacement handlers, matching effect/trigger pattern.

```rust
/// Signature for replacement matchers: returns true if this replacement applies to the event.
pub type ReplacementMatcher = fn(
    event: &ProposedEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool;

/// Signature for replacement appliers: modifies the proposed event.
pub type ReplacementApplier = fn(
    event: ProposedEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> ApplyResult;

pub struct ReplacementHandlerEntry {
    pub matcher: ReplacementMatcher,
    pub applier: ReplacementApplier,
}

pub fn build_replacement_registry() -> HashMap<String, ReplacementHandlerEntry> {
    let mut registry = HashMap::new();
    registry.insert("DamageDone".to_string(), ReplacementHandlerEntry {
        matcher: damage::matches,
        applier: damage::apply,
    });
    registry.insert("Moved".to_string(), ReplacementHandlerEntry {
        matcher: zone_change::matches,
        applier: zone_change::apply,
    });
    // ... remaining 43 handlers
    registry
}
```

### Anti-Patterns to Avoid
- **Mutating game state during replacement evaluation:** Replacements modify the ProposedEvent, not the game state directly. State mutation only happens after the pipeline completes.
- **Forgetting to re-evaluate candidates after each replacement:** CR 616.1 requires this. A replacement that didn't match the original event might match the modified one.
- **Using `power`/`toughness` fields as both base and computed:** Must separate base (printed) from computed (layer output) or layer evaluation will compound effects incorrectly.
- **Incremental layer evaluation without full reset:** Partial updates are extremely bug-prone. Always reset to base, then reapply all layers.
- **Using HashMap for candidate ordering:** Nondeterministic iteration causes test flakiness and AI inconsistency. Use IndexMap or sorted Vec.
- **Hardcoding layer effects instead of using registries:** Follow the established pattern from effects/triggers.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Topological sort for layer dependencies | Manual DFS-based toposort | petgraph `toposort()` | Cycle detection built-in, well-tested |
| Deterministic map ordering | HashMap with post-sort | IndexMap with insertion-order iteration | Cleaner API, no sort step needed |
| Replacement choice orchestration | New callback side-channel | Existing WaitingFor/GameAction state machine | Proven pattern through 5 phases |
| Zone bookkeeping during replaced events | Manual vector manipulation | Existing `zones::move_to_zone` / `create_object` | Already correct and tested |
| Static/replacement string parsing | New parser for S:/R: grammar | Existing `parse_static()` / `parse_replacement()` | Already implemented and tested |
| Event identity tracking | Complex structural hashing | Monotonic u64 counter (ReplacementId) | Simple, deterministic, collision-free |

**Key insight:** The replacement pipeline and layer system are complex enough on their own. Use libraries for graph operations and keep identity tracking simple with monotonic counters.

## Common Pitfalls

### Pitfall 1: Replacement Effect Loops
**What goes wrong:** Replacement A modifies event to trigger replacement B, which modifies it back, causing infinite recursion.
**Why it happens:** CR 616.1f exists specifically to prevent this -- each replacement can only apply once per event.
**How to avoid:** HashSet of applied ReplacementIds on ProposedEvent, checked before each application. Depth cap of 16 as safety net.
**Warning signs:** Tests that hang or stack overflow during replacement processing.

### Pitfall 2: Layer Dependency vs Timestamp
**What goes wrong:** Two effects with a dependency relationship are applied in timestamp order instead of dependency order.
**Why it happens:** CR 613.8 says dependency supersedes timestamp, but easy to sort by timestamp only.
**How to avoid:** Build petgraph dependency graph per layer, toposort first. Only use timestamp as tiebreaker for independent effects. On cycle, fall back to timestamp per CR 613.8.
**Warning signs:** Tests where effect A depends on effect B produce different results than expected.

### Pitfall 3: Stale Computed Characteristics
**What goes wrong:** SBAs or combat use stale power/toughness that don't reflect current continuous effects.
**Why it happens:** `evaluate_layers()` not called before characteristic-dependent checks.
**How to avoid:** Call `evaluate_layers()` before SBA checks, target legality, combat damage. Use `layers_dirty` flag.
**Warning signs:** Creatures surviving with 0 toughness when a -1/-1 effect should kill them.

### Pitfall 4: "As Enters" vs "If Would" Confusion
**What goes wrong:** Self-replacement effects ("as enters the battlefield") don't have correct priority over external replacements.
**Why it happens:** CR 614.12 requires these use the card's projected battlefield characteristics.
**How to avoid:** Self-replacement effects (on the entering permanent itself) get first-pass priority. Forge creates an LKI copy for this.
**Warning signs:** Clone/Arcane Adaptation choices not visible to other replacement effects.

### Pitfall 5: Mutation Sites Missing Replacement Hooks
**What goes wrong:** Some mutations bypass `replace_event()`, causing replacement effects to silently not fire.
**Why it happens:** Not all 14 mutation callsites updated.
**How to avoid:** Audit every call to `zones::move_to_zone`, `damage_marked +=`, `player.life -=`, draw logic, counter modifications, tap/untap, token creation, discard, destroy, sacrifice.
**Warning signs:** Replacements working for spell effects but not combat damage (or vice versa).

### Pitfall 6: Not Re-evaluating Candidates After Each Replacement
**What goes wrong:** A replacement that should apply to the modified event is skipped, or one that no longer applies still fires.
**Why it happens:** Building candidate list once instead of re-evaluating per CR 616.1.
**How to avoid:** Loop: find candidates -> choose one -> apply -> find candidates again with modified event. Stop when no candidates remain.
**Warning signs:** Tests with chained replacements (A modifies event so B now applies) fail.

### Pitfall 7: Wrong Chooser for Multiple Replacements
**What goes wrong:** Active player makes choice instead of affected player/controller.
**Why it happens:** Using `state.active_player` instead of `proposed_event.affected_player()`.
**How to avoid:** CR 616.1 specifies the affected player (or controller of affected object) chooses. Derive from ProposedEvent variant.
**Warning signs:** Two-player tests where non-active player's creature has competing replacements.

### Pitfall 8: Nondeterministic HashMap Ordering
**What goes wrong:** Test results vary between runs, AI evaluates differently for same state.
**Why it happens:** `HashMap` iteration order is arbitrary in Rust.
**How to avoid:** Use IndexMap for candidate lists and any map used in ordering decisions.
**Warning signs:** Flaky tests that sometimes pass and sometimes fail.

## Code Examples

### Mutation Site Inventory (for replacement hooks)
Every one of these callsites needs a `replace_event()` wrapper:

| Mutation | Current Location | ProposedEvent Variant |
|----------|-----------------|----------------------|
| Zone change | `zones::move_to_zone()` | ZoneChange |
| Damage to creature | `combat_damage.rs`, `deal_damage.rs` | Damage |
| Damage to player | `combat_damage.rs`, `deal_damage.rs` | Damage |
| Draw card | `draw.rs` | Draw |
| Gain life | `life.rs` (resolve_gain) | LifeGain |
| Lose life | `life.rs` (resolve_lose) | LifeLoss |
| Add counter | `counters.rs` (resolve_add) | AddCounter |
| Remove counter | `counters.rs` (resolve_remove) | RemoveCounter |
| Create token | `token.rs` | CreateToken |
| Discard | `discard.rs` | Discard |
| Tap | `tap_untap.rs` (resolve_tap) | Tap |
| Untap | `tap_untap.rs` (resolve_untap), `turns.rs` (untap step) | Untap |
| Destroy | `destroy.rs` | Destroy |
| Sacrifice | `sacrifice.rs` | Sacrifice |

### Forge Replacement Types (Reference for REPL-04)
From `ReplacementType.java` -- 35 types:
```
DamageDone, Moved, Destroy, Draw, DrawCards, GainLife, LifeReduced,
AddCounter, RemoveCounter, Tap, Untap, Counter, CreateToken,
Attached, BeginPhase, BeginTurn, DealtDamage, DeclareBlocker,
Explore, GameLoss, GameWin, Learn, LoseMana, Mill, PayLife,
ProduceMana, Proliferate, Scry, Transform, TurnFaceUp,
AssembleContraption, Cascade, CopySpell, PlanarDiceResult,
Planeswalk, RollDice, RollPlanarDice, SetInMotion, AssignDealDamage
```

### Forge Layer Definitions (Reference for STAT-01)
From `StaticAbilityLayer.java`:

| Layer | Forge Name | CR | Dependency-eligible |
|-------|-----------|-----|---------------------|
| 1 | COPY | 613.2 | Yes |
| 2 | CONTROL | 613.3 | Yes |
| 3 | TEXT | 613.4 | Yes |
| 4 | TYPE | 613.5 | Yes |
| 5 | COLOR | 613.6 | No |
| 6 | ABILITIES | 613.7 | Yes |
| 7a | CHARACTERISTIC | 613.8 | Yes |
| 7b | SETPT | 613.8 | Yes |
| 7c | MODIFYPT | 613.8 | No |
| 7d | SWITCHPT (commented) | 613.8 | N/A |
| 8 | RULES (Forge extension) | N/A | No |

Note: Only layers 1-7 needed for CR compliance. Layer 8 is Forge-specific.

### Forge Static Ability Modes (Reference for STAT-04)
From `StaticAbilityMode.java` -- ~65 modes. Split into:
- **Continuous** (~15 param combinations): The core layer-evaluated mode. Params like AddPower, AddToughness, AddKeyword, AddAbility, AddType, SetPower, SetToughness, SetColor, RemoveKeyword, etc.
- **Rule-modification** (~50 modes): CantAttack, CantBlock, CantDraw, CantTarget, ReduceCost, RaiseCost, SetCost, CantGainLife, CantLoseLife, CantBeCast, CantBeActivated, CastWithFlash, IgnoreHexproof, MustAttack, MustBlock, DisableTriggers, Panharmonicon, etc.

### Dependency Graph Example
```rust
use petgraph::algo::toposort;
use petgraph::graph::DiGraph;

fn order_with_dependencies(
    effects: &[ActiveContinuousEffect],
    state: &GameState,
) -> Vec<ActiveContinuousEffect> {
    // Sort by deterministic fallback first
    let mut effects = effects.to_vec();
    effects.sort_by_key(|e| (e.timestamp, e.source_id.0, e.def_index));

    let mut graph: DiGraph<usize, ()> = DiGraph::new();
    let nodes: Vec<_> = (0..effects.len()).map(|i| graph.add_node(i)).collect();

    for i in 0..effects.len() {
        for j in 0..effects.len() {
            if i != j && depends_on(&effects[i], &effects[j], state) {
                // j must apply before i
                graph.add_edge(nodes[j], nodes[i], ());
            }
        }
    }

    match toposort(&graph, None) {
        Ok(order) => order.into_iter().map(|n| effects[graph[n]].clone()).collect(),
        Err(_cycle) => effects, // Cycle: fall back to timestamp ordering per CR 613.8
    }
}

/// Effect A depends on effect B if:
/// 1. Applying B could change whether A's affected set changes, OR
/// 2. Applying B could change what A does to the affected set
fn depends_on(
    a: &ActiveContinuousEffect,
    b: &ActiveContinuousEffect,
    state: &GameState,
) -> bool {
    // Example: if B changes types and A's filter is type-based,
    // then A depends on B
    // Implementation varies per layer
    false // placeholder
}
```

### Rules Checkpoint Integration
```rust
fn rules_checkpoint(state: &mut GameState, events: &mut Vec<GameEvent>) {
    // Evaluate layers before SBA checks
    if state.layers_dirty {
        evaluate_layers(state);
        state.layers_dirty = false;
    }

    sba::check_state_based_actions(state, events);
    triggers::process_triggers(state, events);
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Direct mutations in effect handlers | All mutations go through defined pathways | Project inception | Makes replacement interception feasible -- wrap each pathway |
| Hardcoded effect handlers | Registry-based handler lookup per call | Phase 4 | Replacement/static handlers follow identical pattern |
| Single power/toughness fields | Base + computed separation needed | Phase 6 | Layer evaluation requires this split to avoid compounding |

## Open Questions

1. **Layer evaluation performance with many permanents**
   - What we know: Full re-evaluation is correct. Forge does full re-evaluation. Current battlefield sizes are small.
   - What's unclear: At scale (many permanents with many statics), will per-checkpoint evaluation be fast enough?
   - Recommendation: Start with full re-evaluation (correctness first). Add `layers_dirty` flag to skip when no continuous effects are active. Profile if needed.

2. **Exact handler count mapping**
   - What we know: Forge has 35 replacement types and ~65 static modes. Requirements say 45 and 61.
   - What's unclear: Which specific handlers map to which Forge types, and how parameterization increases the count.
   - Recommendation: Start with the most impactful handlers in early plans, expand coverage in later plans. The registry pattern makes adding handlers trivial.

3. **WaitingFor::ReplacementChoice exact shape**
   - What we know: Must include player, must enable continuation with chosen replacement index.
   - What's unclear: Whether to include candidate descriptions in WaitingFor for future UI display.
   - Recommendation: Include minimal data (player, candidate count). Full descriptions are a UI concern (deferred).

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in test framework (cargo test) |
| Config file | Cargo.toml (already configured) |
| Quick run command | `cargo test -p engine` |
| Full suite command | `cargo test` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| REPL-01 | replace_event intercepts zone change (die -> exile) | unit | `cargo test -p engine replacement -- --test-threads=1` | Wave 0 |
| REPL-01 | replace_event intercepts damage | unit | `cargo test -p engine replacement -- --test-threads=1` | Wave 0 |
| REPL-02 | Once-per-event prevents double application | unit | `cargo test -p engine replacement::tests::once_per_event` | Wave 0 |
| REPL-03 | Multiple replacements trigger WaitingFor choice | unit | `cargo test -p engine replacement::tests::multiple_choice` | Wave 0 |
| REPL-04 | Canonical handlers (DamageDone, Moved, Destroy, Draw) | unit | `cargo test -p engine replacement` | Wave 0 |
| STAT-01 | Layer evaluation produces correct P/T from lord effect | unit | `cargo test -p engine layers::tests::lord_buff` | Wave 0 |
| STAT-02 | Timestamp ordering: newer effect applied after older | unit | `cargo test -p engine layers::tests::timestamp` | Wave 0 |
| STAT-03 | Dependency: type-granting effect before type-dependent buff | unit | `cargo test -p engine layers::tests::dependency` | Wave 0 |
| STAT-04 | Continuous handlers: AddPower, AddKeyword apply correctly | unit | `cargo test -p engine static_abilities` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test -p engine`
- **Per wave merge:** `cargo test`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `crates/engine/src/types/proposed_event.rs` -- ProposedEvent enum
- [ ] `crates/engine/src/game/replacement.rs` -- replacement pipeline + core tests
- [ ] `crates/engine/src/game/layers.rs` -- layer evaluation engine + core tests
- [ ] petgraph + indexmap dependencies in Cargo.toml

## Sources

### Primary (HIGH confidence)
- Forge source: `ReplacementHandler.java` -- replacement pipeline flow, candidate evaluation, LKI handling
- Forge source: `ReplacementType.java` -- 35 replacement event types with handler class mapping
- Forge source: `StaticAbilityLayer.java` -- seven-layer definitions with dependency-eligible layer list
- Forge source: `StaticAbilityMode.java` -- ~65 static ability modes
- Forge source: `StaticAbilityContinuous.java` -- continuous effect application per layer with param handling
- Existing codebase: `types/ability.rs` -- StaticDefinition, ReplacementDefinition already defined
- Existing codebase: `parser/ability.rs` -- parse_static(), parse_replacement() already implemented
- Existing codebase: `game/triggers.rs` -- registry pattern, TriggerMatcher fn signature, build_trigger_registry()
- Existing codebase: `game/effects/mod.rs` -- EffectHandler fn signature, build_registry(), registry-per-call pattern
- Existing codebase: `game/engine.rs` -- WaitingFor state machine, apply() dispatch
- Existing codebase: `game/zones.rs` -- move_to_zone(), create_object() mutation primitives
- Existing codebase: `game/game_object.rs` -- GameObject struct (fields to extend)
- Existing codebase: `types/game_state.rs` -- GameState struct (fields to extend)

### Secondary (MEDIUM confidence)
- MTG Comprehensive Rules 613 (layers), 614-616 (replacement effects) -- referenced through Forge's implementation
- petgraph docs (docs.rs/petgraph) -- toposort API, DiGraph API
- indexmap docs (github.com/indexmap-rs/indexmap) -- insertion-order guarantees

### Tertiary (LOW confidence)
- None

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- petgraph and indexmap are standard Rust ecosystem libraries, no alternatives needed
- Architecture: HIGH -- patterns directly extend established codebase conventions (registries, WaitingFor, fn pointer handlers)
- Pitfalls: HIGH -- derived from Forge's actual implementation complexity and MTG comprehensive rules
- Handler counts: MEDIUM -- exact mapping of 45 replacement + 61 static handlers needs validation during implementation

**Research date:** 2026-03-07
**Valid until:** 2026-04-07 (stable domain -- MTG rules change infrequently)
