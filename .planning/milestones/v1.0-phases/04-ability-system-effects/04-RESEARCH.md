# Phase 4: Ability System & Effects - Research

**Researched:** 2026-03-07
**Domain:** MTG ability resolution, effect execution, targeting, cost payment
**Confidence:** HIGH

## Summary

Phase 4 bridges parsed card data (AbilityDefinition with api_type + params HashMap) to actual game state mutations. The existing codebase has strong foundations: CastSpell/ActivateAbility actions are already defined (currently rejected), mana payment with auto-pay exists, stack push/resolve works (currently just moves zones), and the ability parser correctly produces AbilityDefinition structs with SP$/AB$/DB$ kinds.

The core work is: (1) an effect handler registry that dispatches on api_type strings like "DealDamage", "Draw", "Pump", (2) a casting flow that validates timing, pays costs, selects targets, and pushes to stack, (3) target validation with fizzle checking on resolution, (4) SVar resolution for sub-ability chaining (SubAbility$, Execute$, conditions), and (5) the 15 most important effect handlers.

**Primary recommendation:** Build a `HashMap<String, fn>` effect registry that maps api_type strings to handler functions. Each handler takes `(&mut GameState, &ResolvedAbility, &mut Vec<GameEvent>)` and reads parameters from the AbilityDefinition's params HashMap at resolve time. Sub-abilities chain linearly via SubAbility$ -> SVar lookup -> parse -> resolve next link.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Follow Forge's implementation as reference, adapted to idiomatic Rust with clean architecture
- All 15 top effect types implemented as full handlers: Draw, DealDamage, ChangeZone, Pump, Destroy, Counter, Token, GainLife, LoseLife, Tap, Untap, AddCounter, RemoveCounter, Sacrifice, DiscardCard
- Sub-ability chaining uses a linear chain model (matching Forge's SpellAbility.getSubAbility() pattern) -- each link can independently check conditions, no tree/DAG needed
- Per-link condition checking enables conditional behavior within the linear chain
- Multi-step WaitingFor flow: CastSpell action starts the process, engine returns WaitingFor::TargetSelection or WaitingFor::CostPayment as needed, each step is a separate action/response cycle
- Auto-pay and auto-target: when there's exactly one legal target and mana payment is unambiguous, skip WaitingFor steps and resolve immediately
- Full MTG timing rules: sorcery-speed spells only during your main phase with empty stack; instants and flash cards at any priority point
- Both spell casting (from hand) and activated abilities (ActivateAbility action) supported in Phase 4
- Unified target enum: TargetRef::Object(ObjectId) | TargetRef::Player(PlayerId) -- enables targeting both creatures and players
- Full MTG fizzle rules (rule 608.2b): if ALL targets are illegal on resolution, spell fizzles; if some targets are still legal, spell resolves with only legal ones
- Basic hexproof and shroud checking during target validation; protection deferred to Phase 5

### Claude's Discretion
- SVar resolution approach (lazy lookup vs pre-linking) -- Claude picks based on Forge's SpellAbility.getSVar() pattern and correctness
- Condition system scope -- Claude determines which condition types to implement based on ABIL-05 requirements
- Execute$ branching model -- Claude implements based on Forge's actual behavior
- Effect parameter mapping approach (resolve-time HashMap extraction vs typed param structs) -- based on Forge's getParam() pattern and Rust idioms
- Target filter specification approach (string-based matching vs typed filter structs) -- based on Forge's TargetChoices/CardProperty pattern
- Internal module organization for the ability/effect system
- Cost parser architecture (how non-mana costs like tap, sacrifice, discard, life integrate with existing mana payment)
- StackEntry expansion (how resolved ability data attaches to stack entries for effect execution)
- Token creation implementation details
- Counter spell mechanics (how Counterspell removes a spell from the stack)

### Deferred Ideas (OUT OF SCOPE)
None -- discussion stayed within phase scope
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| ABIL-02 | SVar resolution (SubAbility$, Execute$, ReplaceWith$) | SVar lookup pattern from Forge's AbilityFactory; lazy resolution via card face svars HashMap; linear chain model |
| ABIL-03 | Cost parser (mana costs, tap, sacrifice, discard, life payment) | Forge's Cost.java parsing pattern; space-delimited cost parts with `<>` params; integration with existing mana_payment module |
| ABIL-04 | Target system with legality validation and rechecks on resolution | TargetRef enum, ValidTgts$ string matching, fizzle rules (608.2b), hexproof/shroud checking |
| ABIL-05 | Condition system (ConditionPresent$, ConditionCompare$) | Forge's SpellAbilityCondition patterns; per-link condition checking in sub-ability chains; GE/LE/EQ comparisons |
| ABIL-06 | All 202 effect type handlers via registry (Phase 4 covers top 15) | HashMap<String, HandlerFn> registry; each handler reads params from AbilityDefinition; verified patterns from Forge effects |
| ABIL-07 | Sub-ability chaining | Linear chain via SubAbility$ key referencing SVars; each link independently resolves with own targets/conditions |
</phase_requirements>

## Architecture Patterns

### Recommended Project Structure
```
crates/engine/src/
├── game/
│   ├── engine.rs           # Extended with CastSpell/ActivateAbility arms
│   ├── stack.rs             # Extended resolve_top() with effect execution
│   ├── casting.rs           # NEW: casting flow, timing validation, cost payment orchestration
│   ├── targeting.rs         # NEW: target validation, legality checks, fizzle logic
│   └── effects/
│       ├── mod.rs           # Effect registry, dispatch, ResolvedAbility struct
│       ├── draw.rs          # DrawEffect handler
│       ├── deal_damage.rs   # DealDamageEffect handler
│       ├── change_zone.rs   # ChangeZoneEffect handler
│       ├── pump.rs          # PumpEffect handler
│       ├── destroy.rs       # DestroyEffect handler
│       ├── counter.rs       # CounterEffect handler
│       ├── token.rs         # TokenEffect handler
│       ├── life.rs          # GainLife + LoseLife handlers
│       ├── tap_untap.rs     # Tap + Untap handlers
│       ├── counters.rs      # AddCounter + RemoveCounter handlers
│       ├── sacrifice.rs     # SacrificeEffect handler
│       └── discard.rs       # DiscardEffect handler
├── types/
│   ├── game_state.rs        # Extended WaitingFor, StackEntry, StackEntryKind
│   ├── actions.rs           # Extended with target selection / cost payment response actions
│   └── ability.rs           # Extended with runtime types (ResolvedAbility, TargetRef, etc.)
```

### Pattern 1: Effect Handler Registry
**What:** A HashMap mapping api_type strings ("DealDamage", "Draw", etc.) to handler functions. Mirrors Forge's ApiType enum -> Effect class mapping but uses dynamic dispatch via function pointers.
**When to use:** Every time resolve_top() processes a spell/ability on the stack.
**Example:**
```rust
// Effect handler function signature
type EffectHandler = fn(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError>;

// Registry built once at startup
fn build_registry() -> HashMap<String, EffectHandler> {
    let mut registry = HashMap::new();
    registry.insert("DealDamage".to_string(), deal_damage::resolve as EffectHandler);
    registry.insert("Draw".to_string(), draw::resolve as EffectHandler);
    registry.insert("Pump".to_string(), pump::resolve as EffectHandler);
    // ... 12 more handlers
    registry
}
```

### Pattern 2: ResolvedAbility Struct (Runtime Ability Data)
**What:** A struct that holds the parsed AbilityDefinition plus runtime state: chosen targets, controller, source card, and the sub-ability chain. This is what the effect handler receives.
**When to use:** Created during casting/activation, stored on StackEntry, consumed during resolution.
**Example:**
```rust
pub struct ResolvedAbility {
    pub api_type: String,
    pub params: HashMap<String, String>,
    pub targets: Vec<TargetRef>,
    pub source_id: ObjectId,
    pub controller: PlayerId,
    pub sub_ability: Option<Box<ResolvedAbility>>,
    // SVars available for this ability chain (from card face)
    pub svars: HashMap<String, String>,
}
```

### Pattern 3: Casting Flow State Machine
**What:** Multi-step casting flow using WaitingFor variants. CastSpell action triggers validation, then the engine returns WaitingFor::TargetSelection or WaitingFor::ManaPayment as needed. Auto-pay/auto-target shortcuts bypass intermediate steps.
**When to use:** Every spell cast or ability activation.
**Example flow:**
```
1. Player submits CastSpell { card_id }
2. Engine validates timing (sorcery speed check)
3. Engine checks if targets needed:
   a. No targets needed -> skip to step 5
   b. Exactly one legal target -> auto-select, skip to step 5
   c. Multiple legal targets -> return WaitingFor::TargetSelection
4. Player submits SelectTargets { targets }
5. Engine checks cost:
   a. Auto-pay succeeds -> skip to step 7
   b. Need player input -> return WaitingFor::ManaPayment (reuse existing)
6. Player taps lands / submits mana
7. Cost paid -> push to stack -> return WaitingFor::Priority
```

### Pattern 4: SVar Resolution (Lazy Lookup)
**What:** When resolving a sub-ability chain, look up SVar names in the card face's svars HashMap at resolve time. The SubAbility$ param value is an SVar name; the SVar value is a full ability string (e.g., "DB$ Draw | NumCards$ 1"). Parse it with the existing parse_ability() function, then resolve recursively.
**Why lazy:** Matches Forge's pattern where SpellAbility.getSVar() delegates to the card. Pre-linking would require building the full chain at parse time, which conflicts with dynamic SVar values and conditions.
**Example:**
```rust
// After resolving an effect, check for sub-ability
if let Some(sub_name) = ability.params.get("SubAbility") {
    if let Some(svar_value) = ability.svars.get(sub_name) {
        let sub_def = parse_ability(svar_value)?;
        let sub_resolved = ResolvedAbility::from_definition(sub_def, &ability.svars, ...);
        // Check conditions on this link
        if check_conditions(&sub_resolved, state) {
            resolve_ability(state, &sub_resolved, events)?;
        }
    }
}
```

### Pattern 5: Target Validation with ValidTgts$ String Matching
**What:** Target filters specified via ValidTgts$ strings like "Any", "Creature", "Creature.nonBlack", "Player", "Card" (for stack targeting). The validator parses these strings into filter predicates that check card types, properties, and controller relationships.
**Recommendation:** Use string-based matching (not typed filter structs) for Phase 4. This matches Forge's ValidTgts pattern and keeps the system extensible. A `is_valid_target(target: &TargetRef, filter: &str, state: &GameState, source_controller: PlayerId) -> bool` function handles the matching.
**Common ValidTgts values in the top 15 effects:**
- `Any` -- any creature or player (DealDamage: Lightning Bolt)
- `Creature` -- any creature (Pump: Giant Growth, Destroy: Doom Blade)
- `Creature.nonBlack` -- creature with property filter (Destroy: Doom Blade)
- `Card` with `TargetType$ Spell` -- spell on stack (Counter: Counterspell)
- `Player` -- target player (Draw: some draw effects)

### Anti-Patterns to Avoid
- **Pre-linking the full ability chain at parse time:** SVars may reference other SVars, and conditions are evaluated at resolve time. Build the chain lazily.
- **Typed param structs per effect:** The 15+ effects each have different parameters. Forge uses getParam() on a generic map. Trying to create typed structs for each effect's params adds massive boilerplate with no benefit. Use `params.get("NumDmg")` at resolve time.
- **Separate execution paths for spells vs activated abilities:** They share the same effect resolution system. The only difference is the cost (mana vs tap+mana) and the source (card in hand vs permanent on battlefield).
- **Complex WaitingFor state machines:** Keep the multi-step flow simple. Each WaitingFor variant should carry just enough context to validate the next action. Store pending cast state on the GameState if needed, not in the WaitingFor enum.

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| No new dependencies | - | All effect handling is pure Rust logic | Engine is intentionally dependency-light |

### Supporting
Already in use:
| Library | Version | Purpose |
|---------|---------|---------|
| serde | existing | Serialization of new types (TargetRef, StackEntryKind variants) |
| thiserror | existing | EffectError, CastingError types |
| rand/rand_chacha | existing | Token creation needs ID generation |

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Mana payment | Custom payment logic | Existing `mana_payment::pay_cost()` | Already handles hybrid, phyrexian, generic, X costs |
| Zone movement | Manual collection manipulation | Existing `zones::move_to_zone()` | Handles all zone bookkeeping and events |
| Object creation | Manual ID allocation | Existing `zones::create_object()` | Handles ID allocation, zone placement |
| Stack operations | Manual Vec operations | Existing `stack::push_to_stack()` | Handles events |
| Ability string parsing | New parser | Existing `parser::ability::parse_ability()` | Already handles SP$/AB$/DB$ with params |

**Key insight:** Phase 3 built substantial infrastructure. Phase 4 extends it rather than building parallel systems. The effect handlers are the truly new code; everything else should compose existing functions.

## Common Pitfalls

### Pitfall 1: Fizzle Rule Complexity
**What goes wrong:** Implementing "spell fizzles if target is illegal" without handling the partial-target case.
**Why it happens:** Simple implementations check "is target legal?" and fizzle if no. But MTG rule 608.2b says: only if ALL targets are illegal does the spell fizzle. If a spell has 2 targets and one becomes illegal, it still resolves using the legal target.
**How to avoid:** Track targets as Vec<TargetRef>, check each on resolution, filter to legal-only, fizzle only if resulting list is empty.
**Warning signs:** Tests that only use single-target spells. Add at least one multi-target test.

### Pitfall 2: Timing Validation Edge Cases
**What goes wrong:** Only checking "is it your main phase?" for sorcery-speed spells, forgetting the "stack must be empty" requirement.
**Why it happens:** MTG rules 307.1: sorceries can only be cast during a main phase of your turn, when the stack is empty, and you have priority.
**How to avoid:** Sorcery-speed check = `is_main_phase && stack.is_empty() && is_active_player`. Instant-speed = `has_priority` (any time you have priority).
**Warning signs:** Tests where sorcery-speed spells can be cast in response to other spells.

### Pitfall 3: SVar Resolution Loops
**What goes wrong:** SVar A references SVar B which references SVar A, causing infinite recursion.
**Why it happens:** Card definitions theoretically could have circular references (though none exist in Forge's corpus).
**How to avoid:** Cap sub-ability chain depth at a reasonable limit (e.g., 10 links). Forge doesn't explicitly guard this but its chain lengths are typically 2-4.
**Warning signs:** Stack overflow in tests with complex sub-ability chains.

### Pitfall 4: Forgetting to Move Spell Card After Resolution
**What goes wrong:** Spell resolves its effects but the card object stays on the stack.
**Why it happens:** The existing resolve_top() already handles this (moves to battlefield or graveyard), but when adding effect execution, it's tempting to restructure and lose the zone movement.
**How to avoid:** Effect execution happens BEFORE the zone movement in resolve_top(). The flow is: pop from stack -> execute effects -> move to appropriate zone.
**Warning signs:** Cards stuck on the stack after resolution, or moved to graveyard before effects execute.

### Pitfall 5: Counter Spell Targeting the Stack
**What goes wrong:** Counterspell targets spells on the stack, but the standard target validation checks battlefield objects.
**Why it happens:** Most targets are creatures/players on the battlefield. Counter spells need to target StackEntry items, which are a different concept.
**How to avoid:** TargetRef::Object(ObjectId) works for stack items too since StackEntry has an id field that is an ObjectId. The TargetType$ Spell param tells the validator to look at the stack, not the battlefield. Check Forge's CounterEffect.buildSpellAbility() which sets zone to Stack.
**Warning signs:** Counterspell can't find valid targets even when spells are on the stack.

### Pitfall 6: Auto-Pay Breaking X Costs
**What goes wrong:** Auto-pay for X costs defaults to X=0, making spells like Fireball useless.
**Why it happens:** The existing pay_cost() treats X as "always satisfiable with X=0". For auto-pay, X spells should NOT auto-pay -- they always need player input for the X value.
**How to avoid:** Check if the cost contains ManaCostShard::X; if so, always prompt for mana payment (skip auto-pay). Or treat X as a separate parameter that must be specified in the CastSpell action.
**Warning signs:** X-cost spells always resolve with X=0.

## Code Examples

### Effect Handler: DealDamage
```rust
// Mirrors Forge's DamageDealEffect.resolve()
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let num_dmg: u32 = ability.params.get("NumDmg")
        .ok_or(EffectError::MissingParam("NumDmg"))?
        .parse()
        .map_err(|_| EffectError::InvalidParam("NumDmg"))?;

    for target in &ability.targets {
        match target {
            TargetRef::Object(obj_id) => {
                if let Some(obj) = state.objects.get_mut(obj_id) {
                    obj.damage_marked += num_dmg;
                    events.push(GameEvent::DamageDealt {
                        source_id: ability.source_id,
                        target: target.clone(),
                        amount: num_dmg,
                    });
                }
            }
            TargetRef::Player(player_id) => {
                if let Some(player) = state.players.iter_mut().find(|p| p.id == *player_id) {
                    player.life -= num_dmg as i32;
                    events.push(GameEvent::LifeChanged {
                        player_id: *player_id,
                        amount: -(num_dmg as i32),
                    });
                }
            }
        }
    }
    Ok(())
}
```

### Effect Handler: Draw
```rust
// Mirrors Forge's DrawEffect.resolve()
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let num_cards: u32 = ability.params.get("NumCards")
        .map(|s| s.parse().unwrap_or(1))
        .unwrap_or(1);

    // Defined$ or controller draws
    let player_id = ability.controller;
    let player = state.players.iter().find(|p| p.id == player_id)
        .ok_or(EffectError::PlayerNotFound)?;

    for _ in 0..num_cards {
        // Draw = move top of library to hand
        if let Some(&top_card) = player.library.first() {
            zones::move_to_zone(state, top_card, Zone::Hand, events);
            events.push(GameEvent::CardDrawn {
                player_id,
                object_id: top_card,
            });
        }
    }
    Ok(())
}
```

### Casting Flow
```rust
// In engine.rs apply() match
(WaitingFor::Priority { player }, GameAction::CastSpell { card_id, targets }) => {
    casting::handle_cast_spell(state, *player, card_id, targets, events)?
}

// In casting.rs
pub fn handle_cast_spell(
    state: &mut GameState,
    player: PlayerId,
    card_id: CardId,
    initial_targets: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // 1. Find card in hand
    let object_id = find_card_in_hand(state, player, card_id)?;

    // 2. Validate timing
    validate_timing(state, object_id, player)?;

    // 3. Build ability from card data
    let ability_def = get_spell_ability(state, object_id)?;

    // 4. Handle targeting (auto-target if exactly one legal target)
    let targets = resolve_targets(state, &ability_def, player, initial_targets)?;

    // 5. Pay cost (auto-pay if unambiguous)
    let cost = get_spell_cost(state, object_id)?;
    pay_mana_cost(state, player, &cost)?;

    // 6. Move to stack, push entry
    let resolved = ResolvedAbility::new(ability_def, targets, object_id, player, &svars);
    let entry = StackEntry {
        id: object_id,
        source_id: object_id,
        controller: player,
        kind: StackEntryKind::Spell { card_id, ability: resolved },
    };
    stack::push_to_stack(state, entry, events);

    // 7. Reset priority pass count, give priority to active player
    state.priority_pass_count = 0;
    Ok(WaitingFor::Priority { player: state.active_player })
}
```

### Sub-Ability Chain Resolution
```rust
// After resolving an effect, follow the chain
pub fn resolve_ability_chain(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    registry: &HashMap<String, EffectHandler>,
    depth: u32,
) -> Result<(), EffectError> {
    if depth > 10 {
        return Err(EffectError::ChainTooDeep);
    }

    // Execute this link's effect
    if let Some(handler) = registry.get(&ability.api_type) {
        handler(state, ability, events)?;
    }

    // Follow SubAbility$ chain
    if let Some(sub_name) = ability.params.get("SubAbility") {
        if let Some(svar_value) = ability.svars.get(sub_name) {
            let sub_def = parse_ability(svar_value)?;
            let sub_resolved = ResolvedAbility::from_definition(
                sub_def, &ability.svars, ability.source_id, ability.controller,
            );
            // Per-link condition check
            if check_conditions(&sub_resolved, state) {
                resolve_ability_chain(state, &sub_resolved, events, registry, depth + 1)?;
            }
        }
    }

    Ok(())
}
```

### Counter Effect (Counterspell mechanics)
```rust
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            // Find and remove the targeted spell from the stack
            if let Some(pos) = state.stack.iter().position(|e| e.id == *obj_id) {
                let countered = state.stack.remove(pos);
                // Move countered spell to graveyard (not exile, unless specified)
                zones::move_to_zone(state, *obj_id, Zone::Graveyard, events);
                events.push(GameEvent::SpellCountered {
                    object_id: *obj_id,
                    countered_by: ability.source_id,
                });
            }
        }
    }
    Ok(())
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| resolve_top() only moves zones | resolve_top() executes effects then moves zones | Phase 4 | Core functionality addition |
| CastSpell/ActivateAbility rejected | Full casting flow with timing/targets/costs | Phase 4 | Spells actually work |
| StackEntryKind::Spell { card_id } | StackEntryKind::Spell { card_id, ability: ResolvedAbility } | Phase 4 | Stack entries carry effect data |
| abilities: Vec<String> on GameObject | Parsed AbilityDefinition lookup from CardDB | Phase 4 | Runtime ability resolution |

## Key Design Decisions (Recommendations for Claude's Discretion)

### SVar Resolution: Lazy Lookup (Recommended)
**Rationale:** Forge's SpellAbility.getSVar() performs lazy lookup against the card's state. Pre-linking would require building full chains at parse time, which breaks when SVars reference other SVars or when conditions need resolve-time evaluation. Lazy lookup is simpler, matches Forge, and defers errors to resolution (where they're actionable).

### Effect Parameter Mapping: HashMap at Resolve Time (Recommended)
**Rationale:** Forge's effects use `sa.getParam("NumDmg")` -- a generic string lookup. Creating typed param structs per effect would require 15+ struct definitions and parsing logic. The HashMap approach is simpler, extensible (new params just appear), and matches Forge. Parse values to typed primitives (u32, i32) at the point of use.

### Target Filter: String-Based Matching (Recommended)
**Rationale:** Forge uses ValidTgts$ strings like "Creature.nonBlack" with a generic validation system. For Phase 4, implement a basic matcher supporting: type checks (Creature, Instant, Sorcery), "Any" (creature or player), "Card" (anything on stack), and simple property filters (.nonBlack, .YouCtrl). This covers the success criteria cards (Lightning Bolt "Any", Counterspell "Card", Giant Growth "Creature").

### Cost Parser: Space-Delimited with Type Tags (Recommended)
**Rationale:** Forge's Cost.java parses "T Discard<1/Card> 2 B" as space-delimited parts where "T" = tap, "Discard<1/Card>" = discard cost, and "2 B" = mana. For Phase 4, implement: mana cost (reuse existing parser), "T" (tap source), and leave sacrifice/discard costs as stubs that can be filled when needed by specific cards. The existing ManaCost system handles the mana portion.

### Condition System Scope: ConditionPresent$ and ConditionCompare$ (Recommended)
**Rationale:** These are the two most common condition types in Forge's card corpus. ConditionPresent$ checks "do cards matching X exist in zone Y?" and ConditionCompare$ checks quantity comparisons (GE2 = greater-or-equal 2, EQ0 = equals 0). Together they cover spell mastery, threshold-like checks, and "enters tapped unless" patterns. Other condition types (ConditionDefined$, ConditionLifeTotal$) can be added later.

## Open Questions

1. **How should StackEntry carry the ResolvedAbility?**
   - What we know: StackEntryKind::Spell currently only has { card_id }. Needs to carry the full resolved ability with targets and params.
   - What's unclear: Whether to embed ResolvedAbility directly or reference it via an index/ID.
   - Recommendation: Embed directly in StackEntryKind. The struct is already Clone + Serialize. No need for indirection.

2. **How to handle "Defined$" (non-targeting) effects?**
   - What we know: Forge's effects use both targets (ValidTgts$) and defined references (Defined$ You, Defined$ Self, Defined$ Targeted). "Defined$" means "this is determined, not chosen."
   - What's unclear: Exact scope of Defined$ values needed for top 15 effects.
   - Recommendation: Implement Defined$ You (controller), Defined$ Self (source card), Defined$ Targeted (targets from parent ability). These cover the success criteria cards.

3. **How should the CastSpell action evolve?**
   - What we know: Current CastSpell carries card_id + targets. But targets may not be known at cast time (need WaitingFor::TargetSelection).
   - Recommendation: Keep CastSpell { card_id } for the initial action (no targets yet). Add a new GameAction::SelectTargets { targets: Vec<TargetRef> } for the target selection response. The engine stores pending cast state to link them.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in test framework (cargo test) |
| Config file | Cargo.toml (existing) |
| Quick run command | `cargo test --lib -p engine` |
| Full suite command | `cargo test` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| ABIL-02 | SVar resolution for SubAbility$, Execute$ chains | unit | `cargo test --lib -p engine svar` | No - Wave 0 |
| ABIL-03 | Cost parser for mana, tap, sacrifice, discard | unit | `cargo test --lib -p engine cost_parser` | No - Wave 0 |
| ABIL-04 | Target validation with fizzle checking | unit | `cargo test --lib -p engine targeting` | No - Wave 0 |
| ABIL-05 | Condition system (ConditionPresent$, ConditionCompare$) | unit | `cargo test --lib -p engine condition` | No - Wave 0 |
| ABIL-06 | Effect handlers (15 types) | unit | `cargo test --lib -p engine effects` | No - Wave 0 |
| ABIL-07 | Sub-ability chaining | integration | `cargo test --lib -p engine sub_ability_chain` | No - Wave 0 |
| SC-1 | Lightning Bolt cast -> deal 3 damage | integration | `cargo test --lib -p engine lightning_bolt` | No - Wave 0 |
| SC-2 | Counterspell counters spell on stack | integration | `cargo test --lib -p engine counterspell` | No - Wave 0 |
| SC-3 | Giant Growth gives +3/+3 to creature | integration | `cargo test --lib -p engine giant_growth` | No - Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test --lib -p engine`
- **Per wave merge:** `cargo test`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `crates/engine/src/game/effects/mod.rs` -- effect registry and dispatch tests
- [ ] `crates/engine/src/game/casting.rs` -- casting flow tests (timing, cost, targets)
- [ ] `crates/engine/src/game/targeting.rs` -- target validation and fizzle tests
- [ ] Integration tests for Lightning Bolt, Counterspell, Giant Growth scenarios

## Sources

### Primary (HIGH confidence)
- Forge Java source at `/Users/matt/dev/forge` -- ability factory, effect implementations, card definitions
- Existing codebase at `/Users/matt/dev/forge.rs/crates/engine/src/` -- all current types, engine, parser code
- Forge card definitions (Lightning Bolt, Counterspell, Giant Growth, Execute, Reckless Scholar, Ravaging Blaze)

### Secondary (MEDIUM confidence)
- MTG Comprehensive Rules 608.2b (fizzle rules) -- from training knowledge, well-established rule
- MTG Comprehensive Rules 307.1 (sorcery timing) -- from training knowledge

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- no new dependencies, extending existing code
- Architecture: HIGH -- patterns directly derived from Forge's implementation + existing codebase patterns
- Pitfalls: HIGH -- verified against Forge's actual handling of edge cases
- Effect handlers: HIGH -- based on reading actual Forge effect source code

**Research date:** 2026-03-07
**Valid until:** 2026-04-07 (stable domain, no external dependencies)
