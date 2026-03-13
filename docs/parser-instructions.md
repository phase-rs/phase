# Oracle Parser — Architecture & Contribution Guide

The oracle parser converts MTG card text (from MTGJSON) into typed `AbilityDefinition` structs
that the engine can execute. This document describes the philosophy, structure, and the correct
way to extend it.

---

## Core Philosophy

**The parser is a one-way translation layer.** It takes natural-language Oracle text and produces
a typed data model. All game logic lives in `crates/engine/src/game/` — the parser only produces
data structures, never executes game rules.

1. **Parse intent, not syntax.** Oracle text for the same concept (e.g. "exile target creature")
   can appear in many grammatical forms. The parser must handle all of them and produce the same
   typed output.

2. **Information must not be silently lost.** If Oracle text encodes a semantic distinction (e.g.
   "its controller" vs "you"), that distinction must be preserved in the typed output — never
   discarded by generic subject-stripping.

3. **Unrecognized text → `Effect::Unimplemented`, never panic.** The parser is best-effort. Unknown
   patterns fall through cleanly; the engine skips `Unimplemented` effects without crashing.

4. **Follow the existing type patterns.** The data model already has parallel enum families for
   amounts (`DamageAmount`), players (`GainLifePlayer`), targets (`TargetFilter`), etc. New
   semantic distinctions belong in the type layer, not as ad-hoc boolean flags.

---

## Architecture

```
oracle.rs               Entry point: parse_oracle_text()
oracle_effect.rs        Effect / ability parsing (the main file)
oracle_target.rs        Target filter parsing (TargetFilter)
oracle_cost.rs          Cost parsing (AbilityCost)
oracle_trigger.rs       Trigger condition parsing
oracle_static.rs        Static ability parsing
oracle_replacement.rs   Replacement effect parsing
oracle_util.rs          Shared utilities (parse_number, parse_mana_production, …)
```

### Parse pipeline for a spell ability

```
parse_oracle_text()
  └── parse_effect_chain(text)          # splits "Sentence 1. Sentence 2." into chain
        └── parse_effect_clause(sent)   # handles one sentence
              ├── strip_leading_duration()   # "until end of turn, …"
              ├── strip_leading_conditional() # "if X, Y"
              ├── try_parse_subject_*_clause() # subject-specific clauses (continuous, become…)
              ├── try_parse_targeted_controller_*() # ← NEW: subject-preserving helpers
              ├── strip_subject_clause()  # strips "its controller", "you", etc. → recurse
              └── parse_imperative_effect()  # bare verb phrases: "draw", "exile", "gain"
```

---

## Subject Stripping — The Key Design Decision

`strip_subject_clause` removes subjects like "you", "target creature", "its controller" and
recurses on the predicate. This simplifies parsing for most effects — but **it discards semantic
information**.

**Rule:** If the subject encodes game-relevant information (i.e. it changes *who* the effect
applies to), you **must** intercept the text *before* `strip_subject_clause` is called, using a
dedicated `try_parse_*` helper that preserves the subject's meaning.

### Example: "Its controller gains life equal to its power"

❌ Wrong approach — letting `strip_subject_clause` handle it:
```
"Its controller gains life equal to its power"
    → strip_subject_clause strips "Its controller"
    → parse "gains life equal to its power"
    → GainLife { amount: Fixed(1), player: Controller }  ← BUG: wrong player, wrong amount
```

✅ Correct approach — intercept before stripping:
```rust
// In parse_effect_clause, BEFORE strip_subject_clause:
if let Some(clause) = try_parse_targeted_controller_gain_life(text) {
    return clause;
}
```
```rust
fn try_parse_targeted_controller_gain_life(text: &str) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    if !lower.starts_with("its controller ") { return None; }
    // … parse amount and player, preserving semantic context
    Some(parsed_clause(Effect::GainLife {
        amount: LifeAmount::TargetPower,
        player: GainLifePlayer::TargetedController,
    }))
}
```

---

## Adding a New Effect Type

### Step 1 — Add the variant to `Effect` in `types/ability.rs`

Follow existing patterns:
- Use enum fields for variants that carry distinct data (e.g. `DamageAmount`, `LifeAmount`).
- **Never use boolean flags** as a substitute for a proper enum variant. Boolean flags create
  undefined combinations and obscure intent.
- Mark optional fields `#[serde(default)]` so old card-data.json files are still deserializable.
- Add the variant name to `effect_variant_name()` and a dispatch arm to `resolve_effect()`.

```rust
// Good: mutually exclusive cases in the type system
pub enum LifeAmount {
    Fixed(i32),
    TargetPower,
}

// Bad: boolean flag alongside a numeric field
GainLife { amount: i32, use_target_power: bool }  // ← DON'T DO THIS
```

### Step 2 — Handle the effect in `game/effects/`

Create or extend an effect handler in `crates/engine/src/game/effects/`:
- One `resolve_*` function per logical operation.
- Never access card data or parse text in effect handlers — only process the typed `ResolvedAbility`.
- Register the new effect variant in `game/effects/mod.rs::resolve_effect()`.

### Step 3 — Add the parser logic in `oracle_effect.rs`

- Add a pattern match in `parse_imperative_effect()` for bare verb forms.
- If the subject matters, add a `try_parse_*` helper called **before** `strip_subject_clause()`.
- Use `strip_prefix()` over manual index arithmetic to avoid clippy warnings.
- Return `Effect::Unimplemented { name, description }` for patterns that are recognized but
  not yet implemented rather than panicking or silently returning a wrong effect.

### Step 4 — Write parser tests

Every new parser pattern must have a test in `oracle_effect.rs`:
```rust
#[test]
fn effect_its_controller_gains_life_equal_to_power() {
    let e = parse_effect("Its controller gains life equal to its power");
    assert!(matches!(
        e,
        Effect::GainLife {
            amount: LifeAmount::TargetPower,
            player: GainLifePlayer::TargetedController,
        }
    ));
}
```

---

## Sub-Ability Chains

`parse_effect_chain` splits Oracle text on ". " boundaries and links each clause as a
`sub_ability`. At runtime, `game/effects/mod.rs::resolve_ability_chain` walks this chain.

**Target propagation:** When a parent ability has targets but the sub-ability does not, the engine
propagates the parent's targets to the sub-ability. This allows sub-effects like "its controller
gains life" (in the Swords to Plowshares chain) to access the targeted creature without
duplicating target information in the data model.

This means:
- Parser sub-abilities do **not** need to store their own target lists.
- Effect handlers may receive targets from the parent ability even when `ability.targets` was
  empty in the raw `AbilityDefinition`.

---

## Handling "Equal to X" Amounts

Several effects have amounts derived from game state (power, toughness, CMC, etc.) rather than
fixed integers. Model these as enum variants, following `DamageAmount` and `LifeAmount`:

| Oracle phrase                       | Type / variant           |
|-------------------------------------|--------------------------|
| "N damage" / "N life"               | `Fixed(i32)`             |
| "damage/life equal to its power"    | `TargetPower`            |
| "that much damage"                  | `Variable("that much")`  |

When parsing "equal to its power" / "equal to its toughness", always return the enum variant —
never `Fixed(0)` as a sentinel value.

---

## Self-Reference Normalization (`~`)

Before any condition or effect text is parsed, `normalize_self_refs` replaces the card's own name
and phrases like "this creature", "this enchantment", "this artifact" with `~` (tilde). This
normalization happens in the trigger parser (`oracle_trigger.rs`) but the effect parser also
receives `~`-normalized text when parsing trigger effects.

**Rule:** Any parser function that checks for self-references must recognize `~` alongside explicit
phrases like "this creature" or "it". `parse_target` in `oracle_target.rs` handles `~` → `SelfRef`
at the root level, so any effect that delegates to `parse_target` automatically gets this behavior.

```
"put a +1/+1 counter on Ajani's Pridemate"
  → normalize_self_refs → "put a +1/+1 counter on ~"
  → try_parse_put_counter → PutCounter { target: SelfRef }  ✅
```

---

## Trigger Parser — Subject + Event Decomposition

`oracle_trigger.rs` parses trigger conditions into `TriggerDefinition` structs. The parser uses a
**subject + event decomposition** pattern:

```
parse_trigger_line(text, card_name)
  └── normalize_self_refs()              # card name / "this creature" → ~
  └── split_trigger()                    # split "condition, effect" at first ", "
  └── parse_trigger_condition(condition) # decompose into subject + event
        ├── try_parse_phase_trigger()     # "At the beginning of..."
        ├── try_parse_player_trigger()    # "you gain life", "you cast a spell"
        └── parse_trigger_subject()       # "~", "another creature you control", "a creature"
            └── try_parse_event()         # "enters", "dies", "attacks", "deals damage"
                └── try_parse_counter_trigger()  # "counter is put on ~"
  └── parse_trigger_constraint()         # "triggers only once each turn"
```

### Adding a new trigger event

1. Add a pattern in `try_parse_event()` matching the event verb (e.g. `"leaves the battlefield"`).
2. Set the appropriate `TriggerMode`, `origin`/`destination` zones, and wire the subject into
   `valid_card` or `valid_source`.
3. Add parser tests in the `tests` module.

### Adding a new trigger subject

1. Add a pattern in `parse_trigger_subject()` (e.g. `"each creature"`, `"a nontoken creature"`).
2. Use `parse_type_phrase()` from `oracle_target.rs` for type/controller/property parsing.
3. Compose with `FilterProp::Another` for exclusion patterns ("another creature").

### Trigger constraints

`TriggerConstraint` models rate-limiting on triggers:

| Oracle text | Variant |
|------------|---------|
| "This ability triggers only once each turn." | `OncePerTurn` |
| "This ability triggers only once." | `OncePerGame` |
| "only during your turn" | `OnlyDuringYourTurn` |

Parsed from the full trigger text in `parse_trigger_constraint()`. The runtime enforces constraints
in `process_triggers()` using `(ObjectId, trigger_index)` tracking sets on `GameState`.

---

## Common Pitfalls

| Pitfall | Correct approach |
|---------|-----------------|
| Manual index arithmetic `&text[n..]` | Use `strip_prefix()` / clippy will flag this |
| `unwrap()` on parse results | Return `None` or `Effect::Unimplemented` instead |
| Losing subject context via `strip_subject_clause` | Add `try_parse_*` before the strip call |
| Boolean flags on effect types | Use an enum variant |
| `parse_number("equal to its power")` → `unwrap_or(1)` | Detect the "equal to" pattern first |
| Hardcoding `amount: 1` as default when text is unparseable | Prefer `Unimplemented` so the gap is visible in coverage reports |
| Not recognizing `~` as self-reference in effect parsers | Always check for `~` alongside "this creature", "it", etc. — `parse_target` handles this |
| Monolithic condition parsing | Use subject+event decomposition — add subjects and events independently |
