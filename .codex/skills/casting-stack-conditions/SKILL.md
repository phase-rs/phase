---
name: casting-stack-conditions
description: Use when modifying the casting flow, stack resolution, condition systems, WaitingFor/GameAction state machine, or adding optional costs, new casting steps, or conditional ability resolution.
---

# Casting, Stack & Condition Systems

Reference for the spell casting pipeline, stack resolution, sub_ability chaining, and all condition systems. Use this to understand how spells go from hand → stack → resolution, and where to extend for new casting features.

**Before you start:** Trace `Ward` for a casting-time interaction, or `TriggerCondition::LifeGainedThisTurn` for a condition that's checked at both trigger and resolution time.

---

## Casting Flow — `crates/engine/src/game/casting.rs`

Entry point: `handle_cast_spell(state, player, card_id, events)`

```
CastSpell action
  │
  ▼
1. Find card in hand (or command zone for Commander)
  │
  ▼
2. Get ability: obj.abilities[0].clone()
   (vanilla permanents get placeholder Unimplemented)
   NOTE: Modal spells (obj.modal.is_some()) skip to step 2b
  │
  ▼
2b. Modal detection → WaitingFor::ModeChoice
   Player selects modes → handle_select_modes() builds chained ResolvedAbility
   → proceeds to targeting (walks sub_ability chain for first target filter)
  │
  ▼
3. Validate timing
   ├─ Instant / Flash → anytime
   └─ Sorcery-speed → main phase + empty stack + active player
  │
  ▼
4. Commander color identity check (Commander only)
  │
  ▼
5. Calculate mana cost (base + commander tax if from command zone)
  │
  ▼
6. Build ResolvedAbility from AbilityDefinition
   └─ Recursively converts sub_ability chain via build_resolved_from_def()
  │
  ▼
7. Handle targeting
   ├─ Auras: extract Enchant keyword filter
   └─ Others: extract_target_filter_from_effect()
       ├─ 0 legal targets → error
       ├─ 1 legal target → auto-assign, proceed
       └─ >1 → WaitingFor::TargetSelection
  │
  ▼
8. pay_and_push()
   ├─ X in cost → WaitingFor::ManaPayment
   ├─ Build `SpellMeta` from spell's types/subtypes (for restriction-aware mana spending)
   ├─ auto_tap_lands() → can_pay_for_spell() → pay_cost_with_demand(pool, cost, demand, spell)
   ├─ Move card to Zone::Stack
   ├─ Record commander cast if applicable
   └─ stack::push_to_stack() creates StackEntry
  │
  ▼
9. Return WaitingFor::Priority
```

Key functions: `handle_cast_spell()`, `pay_and_push()`, `handle_select_targets()`, `handle_cancel_cast()`, `build_resolved_from_def()`

---

## Stack Model

### PendingCast — `crates/engine/src/types/game_state.rs`

```rust
pub struct PendingCast {
    pub object_id: ObjectId,
    pub card_id: CardId,
    pub ability: ResolvedAbility,
    pub cost: ManaCost,
}
```

Holds state between targeting and payment steps during casting.

### StackEntry — `crates/engine/src/types/game_state.rs`

```rust
pub struct StackEntry {
    pub id: ObjectId,
    pub source_id: ObjectId,
    pub controller: PlayerId,
    pub kind: StackEntryKind,
}

pub enum StackEntryKind {
    Spell { card_id: CardId, ability: ResolvedAbility },
    ActivatedAbility { source_id: ObjectId, ability: ResolvedAbility },
    TriggeredAbility {
        source_id: ObjectId,
        ability: ResolvedAbility,
        condition: Option<TriggerCondition>,  // intervening-if
    },
}
```

**Note:** Only `TriggeredAbility` carries a `condition` today. Spells and activated abilities have no condition field.

### Stack Resolution — `crates/engine/src/game/stack.rs::resolve_top()`

1. Pop top entry from stack
2. **Intervening-if:** If `TriggeredAbility` with `condition`, call `check_trigger_condition()` — if fails, fizzle
3. Extract `ResolvedAbility` from entry kind
4. **Fizzle check:** If targets exist, validate them; all illegal → fizzle (move to graveyard)
5. Execute: `effects::resolve_ability_chain(state, ability, events, 0)`
6. Post-resolution: move spell to graveyard (or battlefield for permanents)

---

## Sub-Ability Chain Resolution — `crates/engine/src/game/effects/mod.rs`

`resolve_ability_chain(state, ability, events, depth)`:

1. **Safety:** `depth > 20` → `ChainTooDeep` error
2. **Resolve current:** `resolve_effect(state, ability, events)` unless `Unimplemented`
3. **Process sub_ability (if present):**
   - If state entered interactive `WaitingFor` (Scry/Dig/Surveil/Reveal/Search) → save sub_ability as `state.pending_continuation`, return
   - Otherwise → propagate parent targets to sub if sub has none → recurse

**Target propagation:** Parent targets flow to sub_abilities automatically. "Exile target creature. Its controller gains life" → sub_ability receives creature target from parent `ChangeZone`.

**Continuation pattern:** For interactive effects, `state.pending_continuation` holds the remaining chain. When player responds, `engine.rs::apply()` picks up the continuation.

---

## Condition Systems

Three existing condition types, each for a different ability category:

| System | Enum | Used by | Check location |
|--------|------|---------|---------------|
| **TriggerCondition** (`types/ability.rs`) | `LifeGainedThisTurn { minimum }` | `StackEntryKind::TriggeredAbility { condition }` | Trigger discovery AND resolution (intervening-if) |
| **ReplacementCondition** | `UnlessControlsSubtype { subtypes }` | Replacement effects | Replacement applicability check |
| **StaticCondition** | `DevotionGE`, `IsPresent`, `DuringYourTurn`, etc. | Static abilities | Layers system evaluation |

### ReplacementMode (optional replacements)

```rust
pub enum ReplacementMode {
    Mandatory,
    Optional { decline: Option<Box<AbilityDefinition>> },
}
```

Used for "you may" on replacement effects.

---

## WaitingFor / GameAction State Machine

Casting-relevant states:

| WaitingFor | Triggered by | Responded with |
|------------|-------------|---------------|
| `Priority { player }` | Normal priority | `CastSpell`, `ActivateAbility`, `PassPriority` |
| `TargetSelection { player, pending_cast, legal_targets }` | Multiple legal targets during cast | `SelectTargets { targets }` |
| `ModeChoice { player, modal, pending_cast }` | Modal spell ("Choose one —") detected via `obj.modal` | `SelectModes { indices }` |
| `ManaPayment { player }` | X in mana cost | Mana declaration |

The `apply()` function in `engine.rs` matches `(WaitingFor, GameAction)` pairs to route to handlers.

### Adding a new WaitingFor state requires:

1. `types/game_state.rs` — `WaitingFor` variant + `GameAction` variant
2. `engine.rs::apply()` — Match arm for `(WaitingFor::New, GameAction::NewResponse)` pair
3. `phase-ai/legal_actions.rs` — Generate legal responses for AI
4. `client/adapter/types.ts` — TypeScript discriminated union variant
5. Frontend component (if interactive choice needed)

---

## "You May" in Parser

Currently in `oracle_effect.rs`, "you may" is stripped naively:

```rust
if lower.starts_with("you may ") {
    return parse_effect(&text[8..]);
}
```

**The optionality is lost** — effects become mandatory. No `optional` flag exists on `AbilityDefinition` or `ResolvedAbility`.

---

## Common Pitfalls

| Pitfall | Consequence |
|---------|-------------|
| Data on `PendingCast` not propagated to `StackEntry` | Data lost after casting completes |
| New `WaitingFor` without `GameAction` handler in `engine.rs` | Game hangs — response never processed |
| New `WaitingFor` without AI `legal_actions` | AI hangs on the choice |
| New field on `StackEntry` without `#[serde(default)]` | Deserialization breaks for in-progress games |
| Condition check only at resolution, not at trigger time | Violates MTG intervening-if rules (triggers only) |
| New interactive state without `pending_continuation` support | Sub_ability chain breaks when player choice interrupts resolution |
| Modifying `abilities[0]` selection in casting.rs | Changes which ability goes on stack for ALL spells |
| Modal spell targeting only checks first mode | Modes after the first with targets will skip targeting — use `find_first_target_filter_in_chain()` |

---

## Verification

```bash
rg -q "fn handle_cast_spell" crates/engine/src/game/casting.rs && \
rg -q "fn pay_and_push" crates/engine/src/game/casting.rs && \
rg -q "fn resolve_top" crates/engine/src/game/stack.rs && \
rg -q "fn resolve_ability_chain" crates/engine/src/game/effects/mod.rs && \
rg -q "struct PendingCast" crates/engine/src/types/game_state.rs && \
rg -q "struct StackEntry" crates/engine/src/types/game_state.rs && \
rg -q "enum StackEntryKind" crates/engine/src/types/game_state.rs && \
rg -q "enum TriggerCondition" crates/engine/src/types/ability.rs && \
rg -q "enum WaitingFor" crates/engine/src/types/game_state.rs && \
echo "✓ casting-stack-conditions skill references valid" || \
echo "✗ STALE — update skill references"
```
