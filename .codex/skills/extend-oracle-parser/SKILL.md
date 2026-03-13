---
name: extend-oracle-parser
description: Use when adding new Oracle text patterns to the parser — new verb forms, phrase helpers, target patterns, subject handling, effect chain composition, or fixing Unimplemented fallbacks. Covers the parsing priority system, subject stripping, the try_parse intercept pattern, and all helper modules.
---

# Extending the Oracle Parser

The Oracle parser converts MTGJSON Oracle text into typed `AbilityDefinition` structs. It's the bridge between natural-language card text and the engine's typed effect system. This skill covers how the parser is structured, how to add new patterns, and how to avoid the most common pitfalls.

**Before you start:** Read `docs/parser-instructions.md` for the official contribution guide. This skill supplements that document with architectural detail and the full parsing priority system.

---

## Architecture Overview

```
Oracle text (from MTGJSON)
    ↓
strip_reminder_text() — remove parenthesized text
    ↓
normalize_self_refs() — card name → ~
    ↓
parse_oracle_text() — classify line by priority
    ├─ Keywords-only → keyword extraction
    ├─ "When/Whenever/At" → parse_trigger_line()      [oracle_trigger.rs]
    ├─ Contains ":" → parse activated ability           [oracle_cost.rs + oracle_effect.rs]
    ├─ is_static_pattern() → parse_static_line()       [oracle_static.rs]
    ├─ is_replacement_pattern() → parse_replacement()   [oracle_replacement.rs]
    ├─ Imperative verb → parse_effect_chain()           [oracle_effect.rs]
    └─ Fallback → Effect::Unimplemented
```

---

## Parsing Priority — `parse_oracle_text()` in `crates/engine/src/parser/oracle.rs`

Lines are classified in this exact order. **The first match wins.** Understanding this is critical when adding new patterns.

| Priority | Pattern | Router | Module |
|----------|---------|--------|--------|
| 1 | Keywords-only line (comma-separated keywords) | Keyword extraction | `oracle.rs` |
| 2 | `"Enchant {filter}"` | Skipped (handled externally) | — |
| 3 | `"Equip {cost}"` / `"Equip — {cost}"` | `try_parse_equip()` | `oracle.rs` |
| 4 | `"Choose one/two —"` (modal) | Bullet point parsing | `oracle.rs` |
| 5 | Planeswalker loyalty `[+N]/[-N]/[0]:` | `try_parse_loyalty_line()` | `oracle.rs` |
| 6 | Contains `":"` with cost prefix | Activated ability: cost + `parse_effect_chain()` | `oracle_cost.rs` |
| 7 | Starts with `"When"` / `"Whenever"` / `"At"` | `parse_trigger_line()` | `oracle_trigger.rs` |
| 8 | `is_static_pattern()` matches | `parse_static_line()` | `oracle_static.rs` |
| 9 | `is_replacement_pattern()` matches | `parse_replacement()` | `oracle_replacement.rs` |
| 10 | Card is Instant/Sorcery + looks like imperative | `parse_effect_chain()` | `oracle_effect.rs` |
| 11 | Roman numeral (saga chapter) | Skipped | — |
| 12 | Keyword cost line (kicker, etc.) | Skipped (MTGJSON handles) | — |
| 13 | Has ability word prefix (`"Landfall —"`) | Strip prefix, re-classify from priority 7 | `oracle.rs` |
| 14 | Looks like effect sentence (non-spell) | `parse_effect_chain()` | `oracle_effect.rs` |
| 15 | Fallback | `Effect::Unimplemented` | — |

### `is_static_pattern()` — `oracle.rs`

Detects static ability text via string matching:
- `"gets +"`, `"gets -"`, `"get +"`, `"get -"` — pump effects
- `"have "`, `"has "` — keyword granting
- `"can't be blocked"`, `"can't attack"`, `"can't block"` — restrictions
- `"enchanted "`, `"equipped "`, `"all creatures "` — scope prefixes
- `"enters with "`, `"cost {"` — ETB counters, cost modification
- And more — check the function for the full list

### `is_replacement_pattern()` — `oracle.rs`

Detects replacement effect text:
- `"as ~ enters"`, `"enters tapped"` — ETB replacements
- `"if damage would be dealt"` — damage prevention
- `"instead"` — generic replacement indicator

---

## The Effect Parsing Pipeline

### `parse_effect_chain()` — `crates/engine/src/parser/oracle_effect.rs`

Handles multi-sentence abilities by splitting on `. ` and `, then ` boundaries:

```
"Exile target creature. Its controller gains 3 life."
    ↓ split on ". "
["Exile target creature", "Its controller gains 3 life"]
    ↓ parse each via parse_effect_clause()
[ChangeZone { destination: Exile }, GainLife { amount: 3 }]
    ↓ chain via sub_ability
ChangeZone { ..., sub_ability: Some(GainLife { ... }) }
```

**Special compositional logic:**
- **SearchLibrary**: Injects `ChangeZone` sub_ability for the destination ("put it into your hand")
- **RevealHand**: Extracts card filter from follow-up sentence ("you may choose a nonland card")
- **Mana restrictions**: Absorbs "Spend this mana only to cast..." sentences as `ManaSpendRestriction` on the preceding `Effect::Mana` (e.g., Cavern of Souls, Unclaimed Territory). Uses `parse_mana_spend_restriction()` helper.
- **Shuffle**: Detected from ", then shuffle" suffix and appended to chain

### `parse_effect_clause()` — same file

The decision tree for a single clause. Processing order:

1. Strip leading duration: `"until end of turn, X"` → parse duration + recurse on X
2. Strip leading conditional: `"if X, Y"` → parse condition + recurse on Y
3. **Try subject-specific patterns** (BEFORE stripping):
   - `try_parse_subject_continuous_clause()` — "Target creature gets +1/+1"
   - `try_parse_subject_become_clause()` — "~ becomes a 3/3 creature"
   - `try_parse_subject_restriction_clause()` — "That creature can't block"
   - `try_parse_targeted_controller_gain_life()` — "Its controller gains life..."
4. Strip subject clause → recurse on predicate
5. Strip trailing duration: `"... this turn"` → attach duration
6. `parse_imperative_effect()` — bare verb matching

### `parse_imperative_effect()` — same file

Matches bare verb forms in priority order. Add new patterns here. Current verbs (in order):

`activate only` → `add {mana}` → `deals damage` → `destroy all/each` → `destroy target` → `exile` → `draw` → `counter` → `gain/lose life` → `gets +N/+M` → `scry/surveil/mill` → `tap/untap` → `sacrifice` → `discard` → `put counter` → `return/bounce` → `search` → `dig/look at top` → `fight` → `gain control` → `create token` → `explore/proliferate` → `shuffle` → `look at hand` → `reveal` → `copy` → `attach` → `"you may" prefix` → **fallback: Unimplemented**

---

## Subject Stripping — The Critical Design Decision

**The most important parser concept to understand.**

### What it does

`strip_subject_clause()` removes the grammatical subject ("target creature", "you", "that player") from a sentence, leaving just the predicate verb phrase.

```
"Target creature gets +2/+2"  →  "gets +2/+2"
"You draw three cards"         →  "draw three cards"
"Its controller gains 3 life"  →  "gains 3 life"  ← PROBLEM
```

### Why it's dangerous

Subject stripping **discards semantic information**. In the third example, we lose the fact that it's the *controller* who gains life, not the spell's caster. After stripping, "gains 3 life" defaults to the caster.

### The `try_parse_*` intercept pattern

**If the subject carries game-relevant information, intercept BEFORE `strip_subject_clause()`.**

The intercept functions run at step 3 of `parse_effect_clause()`, before subject stripping:

```rust
// Step 3: Try subject-specific patterns FIRST
if let Some(effect) = try_parse_targeted_controller_gain_life(text) {
    return Some(effect);  // Preserved: who gains life + amount source
}
// Step 4: THEN strip subject (information already lost at this point)
if let Some(predicate) = strip_subject_clause(text) {
    return parse_effect_clause(predicate, card_types);
}
```

**When to add a `try_parse_*` interceptor:**
- The subject determines WHO is affected (controller, owner, opponent)
- The subject determines WHAT is referenced (target's power, enchanted creature's toughness)
- The subject creates a dependency between two parts of the sentence

**When subject stripping is fine:**
- "You draw three cards" — the caster always draws
- "Destroy target creature" — target is in the verb phrase, not the subject

---

## Helper Modules

### `oracle_target.rs` — Target & Filter Parsing

**`parse_target(text) → Option<(TargetFilter, &str)>`**
Consumes "target ..." from text, returns the filter and remaining text.

- `"target creature"` → `Typed { card_type: Creature, ... }`
- `"target opponent"` → `Typed { controller: Opponent, ... }`
- `"target creature you control"` → adds controller filter
- `"target creature with power 2 or less"` → adds `FilterProp::PowerLE(2)`

**`parse_type_phrase(text) → TargetFilter`**
Parses complex type descriptions without the "target" prefix.

- Handles color prefixes: "white creature"
- "non" prefixes: "nonland permanent"
- "or" combinations: "creature or artifact" → distributes controller
- Power/toughness constraints: "with power 2 or less"
- Zone suffixes: "card from a graveyard" → `FilterProp::InZone { Graveyard }`, "card in your graveyard" → `InZone { Graveyard }` + `controller: You`

**`parse_zone_suffix(text) → Option<(FilterProp, Option<ControllerRef>, usize)>`**
Detects zone qualifiers after a type phrase. Handles possessive ("from your graveyard"), indefinite ("from a graveyard"), and direct ("from exile") forms for all non-battlefield zones. Skips optional "card"/"cards" before zone prepositions. When `InZone` is present in the filter, `find_legal_targets` searches ONLY that zone exclusively.

### `oracle_util.rs` — Shared Utilities

| Function | What it does | Use when |
|----------|-------------|----------|
| `parse_number(text)` | Parses digits AND English ("three", "a", "an") | Extracting counts from Oracle text |
| `parse_mana_symbols(text)` | Parses `{2}{W}{U}` cost syntax | Mana costs and mana production |
| `strip_reminder_text(text)` | Removes `(parenthesized text)` | Called before all parsing |
| `contains_possessive(text)` | Matches "your"/"their"/"its owner's" | Zone references: "into your hand" |
| `starts_with_possessive(text)` | Same, anchored at start | Subject detection |
| `contains_object_pronoun(text)` | Matches "it"/"them"/"that card"/"those cards" | Anaphoric references in compound effects |

### `oracle_phrase.rs` — Phrase Matching

**`match_phrase_variants(text, phrases) → bool`**
Shared backbone for phrase helpers. Normalizes and checks against a list of patterns.

All phrase helpers (`contains_possessive`, `contains_object_pronoun`, etc.) are built on this function. If you need a new phrase helper, implement it via `match_phrase_variants()` rather than duplicating normalization logic.

---

## The Possessive vs. Targeting Fork

**The single most important parser decision:**

```
"Look at your hand"              → contains_possessive → target: Controller
"Look at target opponent's hand" → parse_target → target: Typed { controller: Opponent }
```

Getting this wrong produces **silent** failures:
- Possessive forms that fall to `parse_target` → no target found → `Unimplemented`
- Targeting forms that match `contains_possessive` → skip targeting phase entirely → wrong player affected

**Rule of thumb:**
- "your/their/its" → `contains_possessive()` → no targeting needed
- "target X's" → `parse_target()` → requires targeting phase (goes on stack, player selects)

---

## Checklist — Adding a New Parser Pattern

### Phase 1 — Identify Where It Belongs

Determine which parser module handles your text:

- **Imperative verb** ("exile", "draw", "create") → `parse_imperative_effect()` in `oracle_effect.rs`
- **Subject + predicate** ("Target creature gets...") → `try_parse_*` or subject stripping in `oracle_effect.rs`
- **Trigger** ("When/Whenever/At") → `parse_trigger_line()` in `oracle_trigger.rs`
- **Static** ("has/gets/can't") → `parse_static_line()` in `oracle_static.rs`
- **Replacement** ("As enters", "instead") → `parse_replacement()` in `oracle_replacement.rs`
- **New gate in is_static/is_replacement_pattern** → `oracle.rs`

### Phase 2 — Add the Pattern

- [ ] **Write the parser test FIRST** — the pattern you're matching, the expected output:
  ```rust
  #[test]
  fn effect_your_new_pattern() {
      let e = parse_imperative_effect("your oracle text here");
      assert!(matches!(e, Effect::YourEffect { field: expected, .. }));
  }
  ```

- [ ] **Add the pattern match** in the appropriate function. Place it at the right priority position:
  - More specific patterns go BEFORE more general ones
  - Check existing order to avoid shadowing

- [ ] **Use existing helpers** — before writing string manipulation:
  - `parse_target()` for "target X" phrases
  - `parse_number()` for count extraction
  - `contains_possessive()` / `contains_object_pronoun()` for pronoun detection
  - `parse_type_phrase()` for card type descriptions
  - `strip_reminder_text()` should already be done by caller

### Phase 3 — Handle the Subject (if predicate pattern)

- [ ] **Decide: intercept or strip?**
  - Does the subject carry game-relevant info? → Write a `try_parse_*` function before `strip_subject_clause()`
  - Is the subject just "you" or boilerplate? → Let subject stripping handle it

### Phase 4 — Chain Composition (if multi-sentence)

- [ ] **Check `parse_effect_chain()` for special handling**
  If your pattern commonly appears in multi-sentence abilities with compositional behavior (like SearchLibrary → ChangeZone → Shuffle), add composition logic in `parse_effect_chain()`.

### Phase 5 — Routing (if new category)

- [ ] **`oracle.rs` — `is_static_pattern()` or `is_replacement_pattern()`**
  If your text is being routed to the wrong parser (e.g., a static ability falling through to effect parsing), add a detection pattern.

### Phase 6 — Tests & Verification

- [ ] Parser unit tests for each new pattern
- [ ] Snapshot test: `crates/engine/tests/oracle_parser.rs` — verify card abilities parse correctly
- [ ] `cargo coverage` — check that Unimplemented count decreased
- [ ] `cargo test -p engine && cargo clippy --all-targets -- -D warnings`

---

## Adding a New Phrase Helper

If you need to detect a new recurring phrase pattern:

1. Identify the phrase variants (e.g., "sacrifice a creature", "sacrifice a permanent", "sacrifices a")
2. Implement via `match_phrase_variants()` in `oracle_phrase.rs`
3. Export from the module and use in parsers
4. Add tests for all variants

**Do not** copy-paste string matching logic — use `match_phrase_variants()` to get normalization for free.

---

## `~` Normalization

Before parsing, card names and self-references are replaced with `~`:

```
"Put a +1/+1 counter on Ajani's Pridemate" → "Put a +1/+1 counter on ~"
"This creature gets +1/+1"                  → "~ gets +1/+1"
```

All parsers receive `~`-normalized text. `parse_target()` maps `~` → `TargetFilter::SelfRef` automatically.

---

## Common Mistakes

| Mistake | Consequence | Fix |
|---------|-------------|-----|
| Pattern too broad, shadows existing match | Existing cards break, wrong Effect produced | Place specific patterns before general ones; test existing patterns still work |
| Using `parse_target` for possessive forms | No target found → Unimplemented | Use `contains_possessive()` → `Controller` |
| Using `contains_possessive` for targeting forms | Targeting phase skipped, wrong player affected | Use `parse_target()` → full targeting |
| Not stripping reminder text | Parenthesized text breaks pattern matching | `strip_reminder_text()` is called by the entry point — verify your caller does it |
| Hardcoding amount as 1 instead of `parse_number()` | "Draw three cards" parses as draw 1 | Always use `parse_number()` for count extraction |
| Subject carries info but gets stripped | "Its controller gains life" → caster gains life | Add `try_parse_*` interceptor before `strip_subject_clause()` |
| Not checking `parse_effect_chain()` composition | "Search... put into hand... shuffle" parses as 3 separate abilities instead of chained | Add composition logic in `parse_effect_chain()` |
| Returning `Unimplemented` with misleading `name` | Coverage report miscategorizes the gap | Use the actual verb as `name`, full text as `description` |

---

## Self-Maintenance

After completing work using this skill:

1. **Verify references** with the check below
2. **Update the priority table** if parsing order changed
3. **Update the imperative verb order** if new verbs were added
4. **Add new phrase helpers** to the helper module table

### Verification

```bash
rg -q "fn parse_oracle_text" crates/engine/src/parser/oracle.rs && \
rg -q "fn is_static_pattern" crates/engine/src/parser/oracle.rs && \
rg -q "fn is_replacement_pattern" crates/engine/src/parser/oracle.rs && \
rg -q "fn parse_effect_chain" crates/engine/src/parser/oracle_effect.rs && \
rg -q "fn parse_effect_clause" crates/engine/src/parser/oracle_effect.rs && \
rg -q "fn parse_imperative_effect" crates/engine/src/parser/oracle_effect.rs && \
rg -q "fn strip_subject_clause" crates/engine/src/parser/oracle_effect.rs && \
rg -q "fn parse_target" crates/engine/src/parser/oracle_target.rs && \
rg -q "fn parse_type_phrase" crates/engine/src/parser/oracle_target.rs && \
rg -q "fn parse_number" crates/engine/src/parser/oracle_util.rs && \
rg -q "fn contains_possessive" crates/engine/src/parser/oracle_util.rs && \
rg -q "fn contains_object_pronoun" crates/engine/src/parser/oracle_util.rs && \
rg -q "fn parse_trigger_line" crates/engine/src/parser/oracle_trigger.rs && \
rg -q "fn parse_static_line" crates/engine/src/parser/oracle_static.rs && \
echo "✓ extend-oracle-parser skill references valid" || \
echo "✗ STALE — update skill references"
```
