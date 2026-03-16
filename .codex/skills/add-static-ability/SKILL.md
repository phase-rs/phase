---
name: add-static-ability
description: Use when adding or modifying static abilities, continuous effects, or layer system modifications — type-changing effects, power/toughness modification, keyword granting, color changes, CDAs, or any "as long as" / "enchanted creature" / "creatures you control" ability.
---

# Adding a Static Ability

Static abilities produce continuous effects that modify game objects through the layer system (MTG Rule 613). Unlike triggered or activated abilities, they don't use the stack — they simply exist while their source is on the battlefield.

**Before you start:** Trace how Changeling's `AddAllCreatureTypes` works from parser to layers. It's the best reference for type-changing effects: `synthesize_changeling_cda()` in `oracle_loader.rs` → `StaticDefinition` with `ContinuousModification::AddAllCreatureTypes` → `gather_active_continuous_effects()` → `apply_continuous_effect()` in `layers.rs`.

---

## MTG Rules Reference

| Rule | What it governs | Engine implication |
|------|----------------|-------------------|
| **613.1** | Continuous effects are applied in layer order | `Layer` enum with fixed evaluation order in `evaluate_layers()` |
| **613.1a-g** | Layer 1-7 definitions | Copy → Control → Text → Type → Color → Ability → P/T (sub-layers) |
| **613.7** | Within a layer, use timestamps (or dependency for some layers) | Layers 1-4, 6, 7a-b use dependency ordering; 7c-e use timestamp ordering |
| **604.1-3** | Characteristic-defining abilities (CDAs) | CDAs function in ALL zones, evaluated in Layer 7a (CharDef) |
| **613.6** | Dependencies between continuous effects | `order_with_dependencies()` handles this for applicable layers |

---

## Key Types

### `StaticDefinition` — `crates/engine/src/types/ability.rs`

```rust
pub struct StaticDefinition {
    pub mode: StaticMode,                        // Continuous, CantAttack, ReduceCost, etc.
    pub affected: Option<TargetFilter>,          // What this applies to (SelfRef, creatures you control, etc.)
    pub modifications: Vec<ContinuousModification>, // What changes to apply
    pub condition: Option<StaticCondition>,       // When this is active
    pub affected_zone: Option<Zone>,             // Future: zone constraint
    pub effect_zone: Option<Zone>,               // Future: where effect originates
    pub characteristic_defining: bool,           // True = CDA, functions in all zones
    pub description: Option<String>,             // Fallback text
}
```

Only `StaticMode::Continuous` is evaluated through the layer system. Other modes (`CantAttack`, `CantBlock`, `ReduceCost`, etc.) are checked directly in their respective game modules.

### `ContinuousModification` — `crates/engine/src/types/ability.rs`

Each variant carries its own data and knows its layer implicitly via `modification.layer()`:

| Variant | Layer | MTG Rule |
|---------|-------|----------|
| `AddType { core_type }` | Type (4) | 613.1d |
| `RemoveType { core_type }` | Type (4) | 613.1d |
| `AddSubtype { subtype: String }` | Type (4) | 613.1d |
| `RemoveSubtype { subtype: String }` | Type (4) | 613.1d |
| `AddAllCreatureTypes` | Type (4) | 613.1d — Changeling CDA |
| `SetColor { colors }` | Color (5) | 613.1e |
| `AddColor { color }` | Color (5) | 613.1e |
| `AddKeyword { keyword }` | Ability (6) | 613.1f |
| `RemoveKeyword { keyword }` | Ability (6) | 613.1f |
| `AddAbility { ability: String }` | Ability (6) | 613.1f |
| `RemoveAllAbilities` | Ability (6) | 613.1f |
| `SetPower { value }` | SetPT (7b) | 613.4b |
| `SetToughness { value }` | SetPT (7b) | 613.4b |
| `AddPower { value }` | ModifyPT (7c) | 613.4c |
| `AddToughness { value }` | ModifyPT (7c) | 613.4c |

### `Layer` Enum — `crates/engine/src/types/layers.rs`

Evaluation order (fixed):

```
Copy (1) → Control (2) → Text (3) → Type (4) → Color (5) → Ability (6)
    → CharDef (7a) → SetPT (7b) → ModifyPT (7c) → SwitchPT (7d) → CounterPT (7e)
```

- Layers 1-4, 6, 7a-b: **dependency-aware** ordering (topological sort)
- Layers 7c-e: **timestamp** ordering

### `StaticCondition` — `crates/engine/src/types/ability.rs`

When a static ability is conditionally active:

- `DevotionGE { colors, threshold }` — Active when devotion ≥ threshold (gods)
- `IsPresent { filter }` — Active when a permanent matching filter exists
- `DuringYourTurn` — Active only during controller's turn
- `ChosenColorIs { color }` — Active when the source's chosen color matches (ETB color-choice cards)
- `QuantityComparison { lhs, comparator, rhs }` — Active when a quantity expression comparison is satisfied (e.g., "hand size > life total"). Uses `QuantityExpr` which can be `Ref(QuantityRef)` or `Fixed(i32)`
- `None` — Always active

### `StaticMode` — `crates/engine/src/types/statics.rs`

Which system evaluates this static:

- **`Continuous`** → Layer system (`evaluate_layers()`)
- **`CantAttack`** / **`CantBlock`** / **`MustAttack`** / **`MustBlock`** → Combat validation
- **`CantBeTargeted`** → Targeting validation
- **`CantBeCast`** / **`CantBeActivated`** → Action validation
- **`ReduceCost`** / **`RaiseCost`** → Mana payment
- **`CantGainLife`** / **`CantLoseLife`** → Life change validation
- **`Panharmonicon`** → Trigger doubling
- **`IgnoreHexproof`** → Targeting override

---

## Layer Evaluation Flow

```
state.layers_dirty = true  (set after zone changes, control changes, etc.)
    ↓
evaluate_layers() in crates/engine/src/game/layers.rs:
    ↓
Step 1: Reset all battlefield objects to base characteristics
    power ← base_power, toughness ← base_toughness
    keywords ← base_keywords, color ← base_color
    ↓
Step 2: gather_active_continuous_effects()
    For each battlefield object:
        For each static_definition where mode == Continuous:
            Check condition (devotion, presence, during-your-turn)
            Get affected filter
            For each modification → create ActiveContinuousEffect {
                source, affected filter, modification, layer, timestamp,
                characteristic_defining
            }
    ↓
Step 3: For each Layer in order:
    Filter effects for this layer
    Order (dependency-aware or timestamp, depending on layer)
    For each effect:
        Find affected objects via filter matching
        apply_continuous_effect(state, obj, modification)
    ↓
Step 3b: Post-fixup for Changeling
    If any object gained Changeling keyword via AddKeyword in layer 6,
    expand all creature types (handles granted-Changeling case)
    ↓
Step 4: Apply counter-based P/T (layer 7e)
    +1/+1, -1/-1 counters modify power/toughness
    ↓
Step 5: Clear layers_dirty flag
```

### `apply_continuous_effect()` — `layers.rs`

The actual mutation switch. This is where new `ContinuousModification` variants must be handled:

```rust
match modification {
    AddPower { value } => obj.power = obj.power.map(|p| p + value),
    AddSubtype { subtype } => { /* push if not present */ },
    RemoveSubtype { subtype } => obj.card_types.subtypes.retain(|s| s != subtype),
    AddAllCreatureTypes => { /* expand from state.all_creature_types */ },
    // ... other variants
}
```

---

## Checklist — Adding a New Static Ability

### Phase 1 — Type Definition

- [ ] **`crates/engine/src/types/ability.rs` — `ContinuousModification` enum**
  Add a new variant if the modification isn't expressible with existing variants. Include any data the modification needs as typed fields.

- [ ] **`crates/engine/src/types/layers.rs` — `ContinuousModification::layer()` impl**
  Add a match arm returning the correct `Layer` for your new variant. Consult the MTG Comprehensive Rules §613 for which layer applies.

- [ ] **`crates/engine/src/types/ability.rs` — `StaticCondition` enum** (if new condition type)
  Add a variant if the static's activation condition isn't covered by existing conditions.

- [ ] **`crates/engine/src/types/statics.rs` — `StaticMode` enum** (if non-continuous)
  Only needed if the static doesn't go through layers (e.g., a new restriction type).

### Phase 2 — Layer Evaluation

- [ ] **`crates/engine/src/game/layers.rs` — `apply_continuous_effect()`**
  Add a match arm for your new `ContinuousModification` variant. This is where the actual object mutation happens.

- [ ] **`crates/engine/src/game/layers.rs` — `gather_active_continuous_effects()`** (if new condition)
  If you added a new `StaticCondition`, add evaluation logic in the condition-checking block.

- [ ] **`crates/engine/src/game/layers.rs` — post-fixup** (if interaction with keywords)
  If your modification interacts with keywords that might be granted in layer 6, check whether the post-fixup block (Changeling handling) needs extension.

### Phase 3 — Parser

- [ ] **`crates/engine/src/parser/oracle.rs` — `is_static_pattern()`**
  Add a pattern check so the Oracle parser routes your text to `parse_static_line()` instead of treating it as an effect. This is a simple string match — `lower.contains("your pattern")` or `lower.starts_with("your prefix")`.

- [ ] **`crates/engine/src/parser/oracle_static.rs` — `parse_static_line()`**
  Add a parsing branch that:
  1. Detects the Oracle text pattern
  2. Extracts the `TargetFilter` for affected objects
  3. Calls `parse_continuous_modifications()` or constructs modifications directly
  4. Returns `Some(StaticDefinition { mode: Continuous, affected, modifications, ... })`

- [ ] **`crates/engine/src/parser/oracle_static.rs` — `parse_continuous_modifications()`** (if extending)
  This helper parses "gets +N/+M" and "has keyword" patterns. Extend if your modification uses a new syntax.

### Phase 4 — Non-Continuous Statics (if applicable)

If your static uses a mode other than `Continuous`, it's evaluated outside the layer system:

- [ ] **`crates/engine/src/game/combat.rs`** — for `CantAttack`, `CantBlock`, `MustAttack`, `MustBlock`
- [ ] **`crates/engine/src/game/targeting.rs`** — for `CantBeTargeted`
- [ ] **`crates/engine/src/game/casting.rs`** — for `CantBeCast`, `ReduceCost`, `RaiseCost`
- [ ] **`crates/engine/src/game/mana_abilities.rs`** — for `CantBeActivated`
- [ ] **`crates/engine/src/game/triggers.rs`** — for `Panharmonicon`

### Phase 5 — Tests

- [ ] Parser test: Oracle text → correct `StaticDefinition` with expected modifications
- [ ] Layer test: create objects with static definitions, run `evaluate_layers()`, assert final characteristics
- [ ] Condition test: verify static is active/inactive based on condition (if conditional)
- [ ] Snapshot test: update `crates/engine/tests/oracle_parser.rs` if card parsing changed
- [ ] `cargo test -p engine && cargo clippy --all-targets -- -D warnings`

---

## Reference: Existing Static Patterns Worth Studying

| Pattern | Example | Parser location in `oracle_static.rs` |
|---------|---------|--------------------------------------|
| Self-buff | "~ gets +1/+1" | `parse_static_line()` — self-referential block |
| Enchant buff | "Enchanted creature gets +2/+2 and has flying" | Enchanted creature block |
| Lord effect | "Other Elf creatures you control get +1/+1" | Other-subtype-you-control block |
| Global effect | "All creatures get -1/-1" | All-creatures block |
| Conditional | "~ has indestructible as long as..." | As-long-as block |
| Turn-scoped | "During your turn, ~ has first strike" | During-your-turn block |
| CDA | Changeling → all creature types | `synthesize_changeling_cda()` in `oracle_loader.rs` |
| Type changing | "This land is the chosen type" | **Not yet implemented** — see `add-replacement-effect` skill |
| Cost reduction | "Spells you cast cost {1} less" | Cost reduction block |
| Restriction | "~ can't be blocked" | Can't-be-blocked block |

### The `parse_continuous_modifications()` Helper

This shared function extracts modifications from "gets +N/+M" and "has keyword" clauses:

```
Input: "gets +2/+1 and has flying and trample"
Output: [AddPower(2), AddToughness(1), AddKeyword(Flying), AddKeyword(Trample)]
```

It handles:
- `+N/+M` and `-N/-M` syntax → `AddPower` + `AddToughness`
- "has X" / "has X and Y" / "has X, Y, and Z" → `AddKeyword` for each
- Combined "gets +N/+M and has X" → both P/T and keyword modifications

---

## Common Mistakes

| Mistake | Consequence | Fix |
|---------|-------------|-----|
| Missing `is_static_pattern()` entry | Oracle text falls through to effect parser, produces `Unimplemented` | Add the pattern check in `oracle.rs` |
| Wrong layer for modification | Effect applies in wrong order relative to other effects | Consult MTG Rules §613 for correct layer |
| Missing `apply_continuous_effect()` arm | New modification variant compiles but no-ops at runtime | Add the match arm in `layers.rs` |
| `affected: None` instead of `Some(SelfRef)` | Effect applies to everything, not just the source | Set `affected` correctly |
| Forgetting `characteristic_defining: true` for CDAs | CDA doesn't function in non-battlefield zones | Set the flag for CDAs |
| Using `StaticMode::Continuous` for restrictions | Restriction evaluated through layers but needs direct checks | Use `CantAttack`/`CantBlock`/etc. modes |
| Not resetting to base values in step 1 | Modifications stack additively across evaluations | `evaluate_layers()` handles reset — just ensure base values are set correctly |
| Layer 7c modification with dependency ordering | P/T modifications should use timestamp, not dependency | The `Layer` impl handles this — just map to correct sub-layer |

---

## Self-Maintenance

After completing work using this skill:

1. **Verify references** with the check below
2. **Update if stale**: function names, file paths, or enum variants that moved
3. **Add new patterns**: new `parse_static_line()` branches or `ContinuousModification` variants

### Verification

```bash
rg -q "fn evaluate_layers" crates/engine/src/game/layers.rs && \
rg -q "fn gather_active_continuous_effects" crates/engine/src/game/layers.rs && \
rg -q "fn apply_continuous_effect" crates/engine/src/game/layers.rs && \
rg -q "fn parse_static_line" crates/engine/src/parser/oracle_static.rs && \
rg -q "fn parse_continuous_modifications" crates/engine/src/parser/oracle_static.rs && \
rg -q "fn is_static_pattern" crates/engine/src/parser/oracle.rs && \
rg -q "enum ContinuousModification" crates/engine/src/types/ability.rs && \
rg -q "struct StaticDefinition" crates/engine/src/types/ability.rs && \
rg -q "enum StaticCondition" crates/engine/src/types/ability.rs && \
rg -q "enum StaticMode" crates/engine/src/types/statics.rs && \
rg -q "enum Layer" crates/engine/src/types/layers.rs && \
rg -q "fn layer\b" crates/engine/src/types/layers.rs && \
echo "✓ add-static-ability skill references valid" || \
echo "✗ STALE — update skill references"
```
