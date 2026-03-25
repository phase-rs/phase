---
name: extend-oracle-parser
description: Use when adding new Oracle text patterns to the parser — new verb forms, phrase helpers, target patterns, subject handling, effect chain composition, or fixing Unimplemented fallbacks. Covers the parsing priority system, subject stripping, the try_parse intercept pattern, and all helper modules.
---

# Extending the Oracle Parser

The Oracle parser converts MTGJSON Oracle text into typed `AbilityDefinition` structs. It's the bridge between natural-language card text and the engine's typed effect system. This skill covers how the parser is structured, how to add new patterns, and how to avoid the most common pitfalls.

**Before you start:** Read `docs/parser-instructions.md` for the official contribution guide. This skill supplements that document with architectural detail and the full parsing priority system.

> **CR Verification Rule:** Every CR number in annotations MUST be verified by grepping `docs/MagicCompRules.txt` before writing. Do NOT rely on memory — 701.x and 702.x numbers are arbitrary sequential assignments that LLMs consistently hallucinate. Run `grep -n "^701.21" docs/MagicCompRules.txt` (etc.) for every number. If you cannot find it, do not write the annotation.

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
    ├─ Contains ":" → parse activated ability           [oracle_cost.rs + oracle_effect/]
    ├─ is_static_pattern() → parse_static_line()       [oracle_static.rs]
    ├─ is_replacement_pattern() → parse_replacement()   [oracle_replacement.rs]
    ├─ Imperative verb → parse_effect_chain()           [oracle_effect/]
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
| 4 | `"Choose one/two —"` (modal) | Bullet point parsing | `oracle_modal.rs` |
| 5 | Planeswalker loyalty `[+N]/[-N]/[0]:` | `try_parse_loyalty_line()` | `oracle.rs` |
| 6 | Contains `":"` with cost prefix | Activated ability: cost + `parse_effect_chain()` | `oracle_cost.rs` |
| 7 | Starts with `"When"` / `"Whenever"` / `"At"` | `parse_trigger_line()` | `oracle_trigger.rs` |
| 8 | `is_static_pattern()` matches | `parse_static_line()` | `oracle_static.rs` |
| 9 | `is_replacement_pattern()` matches | `parse_replacement()` | `oracle_replacement.rs` |
| 10 | Card is Instant/Sorcery + looks like imperative | `parse_effect_chain()` | `oracle_effect/` |
| 11 | Roman numeral (saga chapter) | Skipped | — |
| 12 | Keyword cost line (kicker, etc.) | Skipped (MTGJSON handles) | — |
| 13 | Has ability word prefix (`"Landfall —"`) | Strip prefix, re-classify from priority 7 | `oracle.rs` |
| 14 | Looks like effect sentence (non-spell) | `parse_effect_chain()` | `oracle_effect/` |
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

## Deep Dive: The `oracle_effect/` Directory

The `oracle_effect/` directory is the heart of the parser. It was split from a single file into 9 sub-modules for maintainability:

```
oracle_effect/
├── mod.rs          — Main orchestrator: parse_effect_chain(), parse_effect_clause(), compound detection
├── imperative.rs   — Imperative verb family parsing: parse_*_ast() and lower_*_ast() helpers
├── subject.rs      — Subject-predicate sentence parsing: try_parse_subject_predicate_ast()
├── sequence.rs     — Clause boundary splitting and continuation absorption
├── token.rs        — Token creation parsing: "create a 1/1 white Spirit token with flying"
├── animation.rs    — Animation/become effects: "becomes a 3/3 creature with flying"
├── counter.rs      — Counter mechanics: "put N +1/+1 counters on target"
├── mana.rs         — Mana production and spend restrictions: "add {W}{U}"
└── types.rs        — All AST type definitions (ClauseAst, ImperativeFamilyAst, etc.)
```

### AST Type System — `oracle_effect/types.rs`

The parser uses a two-phase architecture: **parse → AST → lower → Effect**. This separates sentence classification from Effect construction.

**Top-level clause classification — `ClauseAst`:**

| Variant | Shape | Example |
|---------|-------|---------|
| `Imperative { text }` | Bare verb, no subject | "draw three cards" |
| `SubjectPredicate { subject, predicate }` | Subject + verb | "target creature gets +2/+2" |
| `Conditional { clause }` | Wrapped conditional | "if you control a creature, draw a card" |

**Predicate types — `PredicateAst`:**

| Variant | Detected by | Example |
|---------|------------|---------|
| `Continuous` | "gets/get", "has/have" | "gets +2/+2 and has flying" |
| `Become` | "becomes" | "becomes a 3/3 creature" |
| `Restriction` | "can't", "cannot" | "can't attack or block" |
| `ImperativeFallback` | None of the above | Falls back to imperative parsing |

**Imperative family dispatch — `ImperativeFamilyAst`:**

| Variant | Sub-parser | Verb patterns |
|---------|-----------|---------------|
| `Structured(ImperativeAst)` | Multiple sub-types below | Most verbs |
| `CostResource(CostResourceImperativeAst)` | `parse_cost_resource_ast()` | "add {mana}", "pay N life", "activate only if" |
| `ZoneCounter(ZoneCounterImperativeAst)` | `parse_zone_counter_ast()` | "destroy", "exile", "counter", "put counter" |
| `Shuffle(ShuffleImperativeAst)` | `parse_shuffle_ast()` | "shuffle", "shuffle into library" |
| `Put(PutImperativeAst)` | `parse_put_ast()` | "put into/on top of" |
| `Explore` | Direct match | "explore" |
| `Proliferate` | Direct match | "proliferate" |
| `YouMay { text }` | "you may" prefix | Wraps inner effect |

**Structured imperative sub-categories — `ImperativeAst`:**

| Variant | Sub-parser | Effect patterns |
|---------|-----------|----------------|
| `Numeric(NumericImperativeAst)` | `parse_numeric_imperative_ast()` | Draw, GainLife, LoseLife, Pump, Scry, Surveil, Mill |
| `Targeted(TargetedImperativeAst)` | `parse_targeted_action_ast()` | Tap, Untap, Sacrifice, Discard, Return, ReturnToBattlefield, ReturnToZone, Fight, GainControl |
| `SearchCreation(SearchCreationImperativeAst)` | `parse_search_and_creation_ast()` | SearchLibrary, Dig, Token, CopyTokenOf |
| `HandReveal(HandRevealImperativeAst)` | `parse_hand_reveal_ast()` | LookAtHand, RevealHand, RevealTop |
| `Choose(ChooseImperativeAst)` | `parse_choose_ast()` | TargetOnly, NamedChoice, RevealHandFilter |
| `Utility(UtilityImperativeAst)` | `parse_utility_imperative_ast()` | Prevent, Regenerate, Copy, Transform, Attach |

**Supporting AST types:**

| Type | Purpose | Used by |
|------|---------|---------|
| `TokenDescription` | Token metadata (name, P/T, colors, types, keywords, count) | Token creation |
| `AnimationSpec` | Animation parameters (P/T, colors, keywords, types) | Become effects |
| `SearchLibraryDetails` | Search filter, count, reveal flag | Search library |
| `SubjectApplication` | Subject → TargetFilter mapping (affected + optional explicit target) | Subject-predicate parsing |
| `ContinuationAst` | Follow-up phrase that modifies preceding effect | Sequence absorption |
| `ClauseBoundary` | Sentence (`.`), Then (`, then`), Comma (`,`) | Clause splitting |
| `ParsedEffectClause` | Parsing result: effect + duration + sub_ability | All clause parsing |

### Clause Splitting & Continuations — `oracle_effect/sequence.rs`

**`split_clause_sequence(text) -> Vec<ClauseChunk>`** splits multi-sentence text into independent clauses:

- Splits on `.` (Sentence), `, then` (Then), and certain `,` boundaries (Comma)
- Respects parentheses and quotes (won't split inside them)
- Preserves possessive apostrophes (e.g., "player's")

**Continuation absorption** is the mechanism where a follow-up clause modifies a preceding effect rather than creating a new sub_ability:

| Pattern | Continuation type | What it does |
|---------|------------------|-------------|
| Search → "put into your hand" | `SearchDestination` | Appends ChangeZone sub_ability to SearchLibrary |
| RevealHand → "choose a nonland card" | `RevealHandFilter` | Patches existing RevealHand's card filter |
| Mana → "spend this mana only..." | `ManaRestriction` | Patches ManaSpendRestriction on Mana effect |
| Counter → "that spell loses all abilities" | `CounterSourceStatic` | Patches source_static on Counter effect |
| Token → "suspect it" | `SuspectLastCreated` | Appends Suspect sub_ability |

Key functions:
- `parse_followup_continuation_ast(text, previous_effect)` — Detects continuations based on the previous effect type
- `parse_intrinsic_continuation_ast(text, effect)` — Detects continuations within the same clause
- `continuation_absorbs_current(continuation, effect) -> bool` — Determines if continuation patches vs. chains
- `apply_clause_continuation(defs, continuation)` — Applies the continuation to the ability chain

### Subject-Predicate Parsing — `oracle_effect/subject.rs`

**`try_parse_subject_predicate_ast(text) -> Option<ClauseAst>`** parses sentences with explicit subjects.

**Subject parsing via `parse_subject_application(subject)`:**

| Subject text | Result |
|-------------|--------|
| "target creature" | Explicit target with TargetFilter |
| "all creatures", "each creature" | Mass filter (affects all matching) |
| "~", "it", "this creature" | SelfRef |
| "enchanted creature" | EnchantedCreature filter |
| "equipped creature" | EquippedCreature filter |
| "defending player" | DefendingPlayer filter |
| "creatures you control" | Typed filter with controller: You |

**Predicate parsing hierarchy:**
1. `try_parse_subject_continuous_clause()` — "gets +X/+Y", "has keyword" → GenericEffect with ContinuousModification
2. `try_parse_subject_become_clause()` — "becomes a 3/3 creature" → GenericEffect with animation modifications
3. `try_parse_subject_restriction_clause()` — "can't attack or block" → AddRestriction effect
4. Fallback: `strip_subject_clause()` + reparse as imperative

### Imperative Family Parsing — `oracle_effect/imperative.rs`

**`parse_imperative_family_ast(text, lower) -> Option<ImperativeFamilyAst>`** is the master dispatcher. It tries all families in this order:

1. CostResource → 2. ZoneCounter → 3. Numeric → 4. Targeted → 5. SearchCreation → 6. Explore/Proliferate → 7. Shuffle → 8. HandReveal → 9. Choose → 10. Put → 11. YouMay

Each `parse_*_ast()` returns an AST node, and each `lower_*_ast()` converts it to an `Effect`. **Detailed family reference:**

#### `parse_numeric_imperative_ast()` — Numeric effects
| Pattern | AST → Effect |
|---------|-------------|
| "draw N" | Draw { count } → Effect::Draw |
| "gain N life" | GainLife { amount } → Effect::GainLife |
| "lose N life" | LoseLife { amount } → Effect::LoseLife |
| "gets +X/+Y" | Pump { power, toughness } → Effect::Pump |
| "scry N" | Scry { count } → Effect::Scry |
| "surveil N" | Surveil { count } → Effect::Surveil |
| "mill N" | Mill { count } → Effect::Mill |

#### `parse_zone_counter_ast()` — Zone changes and counters
| Pattern | AST → Effect |
|---------|-------------|
| "destroy target/all" | Destroy → Effect::Destroy / DestroyAll |
| "exile target/all" | Exile → Effect::ChangeZone / ChangeZoneAll |
| "counter target" | Counter → Effect::Counter |
| "put N counters on" | PutCounter → Effect::PutCounter (via `counter.rs`) |
| "remove N counters from" | RemoveCounter → Effect::RemoveCounter (via `counter.rs`) |

#### `parse_targeted_action_ast()` — Targeted actions
| Pattern | AST → Effect |
|---------|-------------|
| "tap {target}" | Tap → Effect::TapUntap { tap: true } |
| "untap {target}" | Untap → Effect::TapUntap { tap: false } |
| "sacrifice {target}" | Sacrifice → Effect::Sacrifice |
| "discard N" | Discard → Effect::Discard |
| "return {target}" | Return → Effect::ChangeZone (to hand) |
| "return {target} to battlefield" | ReturnToBattlefield → Effect::ChangeZone (to battlefield) |
| "fight {target}" | Fight → Effect::Fight |
| "gain control of {target}" | GainControl → Effect::GainControl |

#### `parse_cost_resource_ast()` — Mana and cost patterns
| Pattern | AST → Effect |
|---------|-------------|
| "add {mana}" | Mana → Effect::Mana (via `mana.rs`) |
| "pay N life" | Pay { Life } → Effect::Pay |
| "pay {mana}" | Pay { Mana } → Effect::Pay |
| "activate only if" | ActivateOnlyIf → Effect::Unimplemented (placeholder) |
| Damage patterns | Damage → Effect::DealDamage (via `try_parse_damage()` in `mod.rs`) |

#### `parse_search_and_creation_ast()` — Search and tokens
| Pattern | AST → Effect |
|---------|-------------|
| "search your library" | SearchLibrary → Effect::SearchLibrary |
| "look at the top N" | Dig → Effect::Dig |
| "create a/N {token desc}" | Token → Effect::Token (via `token.rs`) |
| "token that's a copy of" | CopyTokenOf → Effect::CopyTokenOf |

#### `parse_shuffle_ast()` — Shuffle variants
| Pattern | AST → Effect |
|---------|-------------|
| "shuffle" (bare) | ShuffleLibrary → Effect::Shuffle |
| "shuffle {noun} into library" | ChangeZoneToLibrary → Effect::ChangeZone + Shuffle |
| "shuffle {possessive} graveyard" | ChangeZoneAllToLibrary → Effect::ChangeZoneAll + Shuffle |

#### Other families
- **`parse_hand_reveal_ast()`**: LookAtHand, RevealHand, RevealTop
- **`parse_choose_ast()`**: TargetOnly (choose-as-targeting), NamedChoice (creature type/color/etc.), RevealHandFilter
- **`parse_put_ast()`**: Mill (put top N into graveyard), ZoneChange, TopOfLibrary
- **`parse_utility_imperative_ast()`**: Prevent, Regenerate, Copy, Transform, Attach

### Token Parsing — `oracle_effect/token.rs`

Parses "create a 1/1 white Spirit creature token with flying" into `TokenDescription`:

1. Count prefix: "a" → 1, "two" → 2, "X" → Variable
2. P/T prefix: "1/1", "X/X" (variable), "*/*" (star)
3. Supertypes: "legendary"
4. Colors: "white", "blue", etc.
5. Type + subtype: "Spirit creature", "Treasure artifact"
6. Name clause: optional named tokens (e.g., "named Shard")
7. Keywords: "with flying and vigilance"
8. "where X is" expressions → variable P/T or count
9. "for each ... this way" → TrackedSetSize count

### Animation Parsing — `oracle_effect/animation.rs`

Parses "becomes a 3/3 creature with flying" into `AnimationSpec` → `Vec<ContinuousModification>`:

- Fixed P/T: "3/3" → SetPower(3), SetToughness(3)
- Colors: "white", "red and green" → SetColor
- Types: "creature", "artifact creature" → AddType/AddSubtype
- Keywords: "with flying, vigilance" → AddKeyword
- "loses all other abilities" → RemoveAllAbilities

### Counter Parsing — `oracle_effect/counter.rs`

- `try_parse_put_counter(lower, text)` — "put N +1/+1 counter(s) on {target}"
- `try_parse_remove_counter(lower)` — "remove N counter(s) from {target}"
- Counter type normalization: "+1/+1" → "P1P1", "-1/-1" → "M1M1"

### Mana Parsing — `oracle_effect/mana.rs`

- `try_parse_add_mana_effect(text)` — "add {W}{U}", "add {C}", "add one mana of any color"
  - Handles: Fixed symbols, Colorless, AnyOneColor, AnyCombination, ChosenColor
- `parse_mana_spend_restriction(lower)` — "spend this mana only to cast creature spells"
- `try_parse_activate_only_condition(text)` — "activate only if you control a Plains"

### Compound Action Detection — `oracle_effect/mod.rs`

**`try_split_targeted_compound(text)`** — Detects `"verb target X and verb2 it"`:
- Uses `parse_target()` remainder to find the split point
- If second clause has anaphoric pronoun ("it", "them"), inherits parent target via `replace_target_with_parent()`

**`try_parse_compound_shuffle(text)`** — Special case for `"shuffle X and Y into libraries"`:
- Creates two ChangeZone effects (primary + sub_ability)

### Special-Case Matchers in `parse_effect_clause()` — `oracle_effect/mod.rs`

These run before the AST pipeline:

| Matcher | Pattern | Effect |
|---------|---------|--------|
| `try_parse_damage_prevention_disabled()` | "damage can't be prevented this turn" | GenericEffect + DamagePreventionDisabled |
| `try_parse_still_a_type()` | "it's still a land" | GenericEffect + AddType |
| `try_parse_for_each_effect()` | "draw a card for each creature" | Effect with QuantityExpr::Ref |
| `try_parse_equal_to_quantity_effect()` | "mill cards equal to hand size" | Effect with QuantityExpr |

---

## The Two-Phase Parse/Lower Architecture

Single clauses go through an explicit parse/lower split:

```rust
parse_effect_clause()                    // mod.rs — entry point
  → parse_clause_ast()                   // mod.rs — classify sentence shape
  → lower_clause_ast()                   // mod.rs — convert AST to Effect
    → lower_subject_predicate_ast()      // mod.rs — for SubjectPredicate clauses
    → lower_imperative_clause()          // mod.rs — for Imperative clauses
      → parse_imperative_effect()        // mod.rs — try special cases, then delegate
        → parse_imperative_family_ast()  // imperative.rs — classify verb family
        → lower_imperative_family_ast()  // imperative.rs — convert to Effect
```

---

## Subject Stripping — The Critical Design Decision

**The most important parser concept to understand.**

### What it does

`strip_subject_clause()` (in `oracle_effect/subject.rs`) is now a fallback helper, not the primary sentence model. The parser first tries to build a `SubjectPredicate` AST via `try_parse_subject_predicate_ast()` (also in `subject.rs`); only the fallback path strips the grammatical subject and lowers the remainder as an imperative.

```
"Target creature gets +2/+2"  →  SubjectPredicate AST → Continuous predicate
"You draw three cards"         →  SubjectPredicate fails → strip "You" → imperative "draw three cards"
"Its controller gains 3 life"  →  INTERCEPTED by try_parse_targeted_controller_gain_life()
```

### Why it's dangerous

Subject stripping **discards semantic information**. "Its controller gains 3 life" would lose the fact that it's the *controller* who gains life, not the spell's caster.

### The `try_parse_*` intercept pattern

**If the subject carries game-relevant information, preserve it before falling back to `strip_subject_clause()`.**

`try_parse_targeted_controller_gain_life()` (in `subject.rs`) runs in `lower_imperative_clause()` (in `mod.rs`) before fallback subject stripping.

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

**`parse_type_phrase(text) → TargetFilter`**
Parses complex type descriptions without the "target" prefix. Handles color prefixes, "non" prefixes, "or" combinations, power/toughness constraints, and zone suffixes.

**`parse_zone_suffix(text) → Option<(FilterProp, Option<ControllerRef>, usize)>`**
Detects zone qualifiers after a type phrase. When `InZone` is present in the filter, `find_legal_targets` searches ONLY that zone exclusively.

### `oracle_util.rs` — Shared Utilities & Phrase Matching

| Function | What it does | Use when |
|----------|-------------|----------|
| `parse_number(text)` | Parses digits AND English ("three", "a", "an") | Extracting counts from Oracle text |
| `parse_mana_symbols(text)` | Parses `{2}{W}{U}` cost syntax | Mana costs and mana production |
| `strip_reminder_text(text)` | Removes `(parenthesized text)` | Called before all parsing |
| `contains_possessive(text)` | Matches "your"/"their"/"its owner's" | Zone references: "into your hand" |
| `starts_with_possessive(text)` | Same, anchored at start | Subject detection |
| `contains_object_pronoun(text)` | Matches "it"/"them"/"that card"/"those cards" | Anaphoric references in compound effects |
| `match_phrase_variants(text, phrases)` | Shared backbone for all phrase helpers | Building new phrase matchers |

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

---

## Checklist — Adding a New Parser Pattern

### Phase 1 — Identify Where It Belongs

- **Imperative verb/family** → the relevant `parse_*_ast()` in `oracle_effect/imperative.rs`
- **Subject + predicate** → `try_parse_subject_*` in `oracle_effect/subject.rs`
- **Token creation** → `oracle_effect/token.rs`
- **Animation/become** → `oracle_effect/animation.rs`
- **Counter mechanics** → `oracle_effect/counter.rs`
- **Mana production** → `oracle_effect/mana.rs`
- **Continuation/absorption** → `oracle_effect/sequence.rs`
- **Trigger** → `parse_trigger_line()` in `oracle_trigger.rs`
- **Static** → `parse_static_line()` in `oracle_static.rs`
- **Replacement** → `parse_replacement()` in `oracle_replacement.rs`
- **Routing gate** → `is_static_pattern()` / `is_replacement_pattern()` in `oracle.rs`

### Phase 2 — Add the Pattern

- [ ] **Write the parser test FIRST**
- [ ] **Add the pattern match** — more specific patterns go BEFORE more general ones
- [ ] **Use existing helpers** — `parse_target()`, `parse_number()`, `contains_possessive()`, `parse_type_phrase()`

### Phase 3 — Handle the Subject (if predicate pattern)

- [ ] **Decide: intercept or strip?** — Does the subject carry game-relevant info? → add `try_parse_*` in `subject.rs`

### Phase 4 — Chain Composition (if multi-sentence)

- [ ] **Check continuation system in `sequence.rs`** — for follow-up clauses that modify preceding effects
- [ ] **Check `parse_effect_chain()` in `mod.rs`** — for special chaining behavior

### Phase 5 — Routing (if new category)

- [ ] **`oracle.rs` — `is_static_pattern()` or `is_replacement_pattern()`** — if text is routed to the wrong parser

### Phase 6 — Tests & Verification

- [ ] Parser unit tests for each new pattern
- [ ] Snapshot test: `crates/engine/tests/oracle_parser.rs`
- [ ] `cargo coverage` — check that Unimplemented count decreased
- [ ] `cargo test -p engine && cargo clippy --all-targets -- -D warnings`

---

## Adding a New Phrase Helper

1. Identify the phrase variants
2. Implement via `match_phrase_variants()` in `oracle_util.rs`
3. Export from the module and use in parsers
4. Add tests for all variants

---

## `~` Normalization

Before parsing, card names and self-references are replaced with `~`. All parsers receive `~`-normalized text. `parse_target()` maps `~` → `TargetFilter::SelfRef` automatically.

---

## Common Mistakes

| Mistake | Consequence | Fix |
|---------|-------------|-----|
| Pattern too broad, shadows existing match | Existing cards break, wrong Effect produced | Place specific patterns before general ones; test existing patterns still work |
| Using `parse_target` for possessive forms | No target found → Unimplemented | Use `contains_possessive()` → `Controller` |
| Using `contains_possessive` for targeting forms | Targeting phase skipped, wrong player affected | Use `parse_target()` → full targeting |
| Hardcoding amount as 1 instead of `parse_number()` | "Draw three cards" parses as draw 1 | Always use `parse_number()` for count extraction |
| Subject carries info but gets stripped | "Its controller gains life" → caster gains life | Preserve via `try_parse_*` in `subject.rs` |
| Added pattern to wrong AST family | Text lowers to the wrong `Effect` or misses continuation | Register in the smallest matching `parse_*_ast()` in `imperative.rs` |
| Not checking continuation system | "Search... put into hand... shuffle" parses as 3 separate abilities | Add continuation logic in `sequence.rs` |
| Editing `mod.rs` when sub-module is the right place | Bloats the orchestrator | Token → `token.rs`, mana → `mana.rs`, counters → `counter.rs` |
| Returning `Unimplemented` with misleading `name` | Coverage report miscategorizes the gap | Use the actual verb as `name`, full text as `description` |

---

## Self-Maintenance

After completing work using this skill:

1. **Verify references** with the check below
2. **Update the priority table** if parsing order changed
3. **Update the AST family tables** if new imperative families or continuation absorptions were added
4. **Update the deep dive section** if new sub-modules were added to `oracle_effect/`

### Verification

```bash
rg -q "fn parse_oracle_text" crates/engine/src/parser/oracle.rs && \
rg -q "fn is_static_pattern" crates/engine/src/parser/oracle.rs && \
rg -q "fn is_replacement_pattern" crates/engine/src/parser/oracle.rs && \
rg -q "fn parse_effect_chain" crates/engine/src/parser/oracle_effect/mod.rs && \
rg -q "fn parse_effect_clause" crates/engine/src/parser/oracle_effect/mod.rs && \
rg -q "fn parse_imperative_effect" crates/engine/src/parser/oracle_effect/mod.rs && \
rg -q "fn strip_subject_clause" crates/engine/src/parser/oracle_effect/subject.rs && \
rg -q "fn try_parse_subject_predicate_ast" crates/engine/src/parser/oracle_effect/subject.rs && \
rg -q "fn try_parse_targeted_controller_gain_life" crates/engine/src/parser/oracle_effect/subject.rs && \
rg -q "fn parse_imperative_family_ast" crates/engine/src/parser/oracle_effect/imperative.rs && \
rg -q "fn parse_numeric_imperative_ast" crates/engine/src/parser/oracle_effect/imperative.rs && \
rg -q "fn parse_zone_counter_ast" crates/engine/src/parser/oracle_effect/imperative.rs && \
rg -q "fn split_clause_sequence" crates/engine/src/parser/oracle_effect/sequence.rs && \
rg -q "fn parse_followup_continuation_ast" crates/engine/src/parser/oracle_effect/sequence.rs && \
rg -q "fn try_parse_token" crates/engine/src/parser/oracle_effect/token.rs && \
rg -q "fn parse_animation_spec" crates/engine/src/parser/oracle_effect/animation.rs && \
rg -q "fn try_parse_put_counter" crates/engine/src/parser/oracle_effect/counter.rs && \
rg -q "fn try_parse_add_mana_effect" crates/engine/src/parser/oracle_effect/mana.rs && \
rg -q "fn parse_target" crates/engine/src/parser/oracle_target.rs && \
rg -q "fn parse_type_phrase" crates/engine/src/parser/oracle_target.rs && \
rg -q "fn parse_number" crates/engine/src/parser/oracle_util.rs && \
rg -q "fn contains_possessive" crates/engine/src/parser/oracle_util.rs && \
rg -q "fn contains_object_pronoun" crates/engine/src/parser/oracle_util.rs && \
rg -q "fn match_phrase_variants" crates/engine/src/parser/oracle_util.rs && \
rg -q "fn parse_trigger_line" crates/engine/src/parser/oracle_trigger.rs && \
rg -q "fn parse_static_line" crates/engine/src/parser/oracle_static.rs && \
echo "✓ extend-oracle-parser skill references valid" || \
echo "✗ STALE — update skill references"
```
