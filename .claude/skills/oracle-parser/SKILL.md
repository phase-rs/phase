---
name: oracle-parser
description: "Use when doing any parser work — adding new Oracle text patterns, verb forms, phrase helpers, target patterns, subject handling, effect chain composition, fixing Unimplemented fallbacks, or understanding the parser architecture. This is the SINGLE SOURCE OF TRUTH for all oracle parser knowledge. Covers the nom combinator mandate, parsing priority system, AST type system, all helper modules, and contribution checklists."
---

# Oracle Parser — Single Source of Truth

The Oracle parser converts MTGJSON Oracle text into typed `AbilityDefinition` structs that the engine executes. This skill is the **authoritative reference** for all parser work.

> **CR Verification Rule:** Every CR number you write MUST be verified by grepping `docs/MagicCompRules.txt` BEFORE adding it to code. See Section 8.

---

## 1. Non-Negotiable Rules

These rules are defined in CLAUDE.md and are enforced without exception. Violations will be caught in review and must be fixed before merge.

### ⚠ RULE ZERO: Nom Combinators Are Mandatory — No Exceptions

**All new parser code MUST use nom combinators from the very first line written.** This is the single most important rule in the parser codebase. It has been violated repeatedly and is now enforced as a hard gate.

**NEVER write any of these for parsing dispatch:**
- `find()`, `split_once()`, `contains()`, `starts_with()` — for dispatch routing
- `if lower.starts_with("destroy ")` — use `tag("destroy ").parse(lower)` instead
- `if lower.contains("target")` — use `scan_at_word_boundaries` or a nom combinator instead
- `text.find(' ').map(|i| &text[..i])` — use nom `take_till` or `take_while`

**ALWAYS write:**
- `tag("destroy ").parse(lower)?` — for known prefix dispatch
- `alt((tag("destroy "), tag("exile "))).parse(lower)?` — for multi-option dispatch
- `nom_on_lower(text, lower, parser_fn)` — for mixed-case text bridging to nom
- `nom_on_lower_required(text, lower, parser_fn)?` — same with `?` propagation
- `nom_parse_lower(lower, parser_fn)` — when remainder is unused
- `scan_at_word_boundaries(text, combinator)` — for multi-position phrase matching

**Nom combinator reference:** See `nom_combinators.md` in this skill directory for the complete list of nom parsers and combinators organized by module. Consult this when choosing which combinator to use.

**Copy-paste patterns + the enforcement gate:** When you need to translate a string-method idiom into combinators, open `crates/engine/src/parser/oracle_nom/PATTERNS.md` — it indexes every common shape (strip prefix, strip suffix, optional trailing clause, alternatives, word-boundary scan, delimiter split, contains-check, peek-without-consume) with copy-pasteable code. The pre-commit hook `scripts/check-parser-combinators.sh` actively rejects new lines containing `.strip_prefix(...)`, `.strip_suffix(...)`, `.contains("...")`, `.starts_with("...")`, `.ends_with("...")`, `.split_once(...)`, `.find("...")`, or `.trim_end_matches("...")` against string literals inside `crates/engine/src/parser/`. If a use is genuinely structural (post-tokenization punctuation cleanup, `TextPair` dual-string stripping, runtime char scans) annotate the line `// allow-noncombinator: <one-line reason>` per PATTERNS.md §9. Existing offenders are grandfathered; new code is gated.

**The only acceptable uses of `starts_with`/`strip_prefix` in parser code:**
- `TextPair::strip_prefix` for dual-string case-bridging operations (this is structural, not dispatch)
- Runtime array loops or char-level scanners
- Dynamic (non-literal) prefixes that can't be known at compile time

**If you catch yourself writing string matching for parsing, STOP and rewrite with combinators before proceeding.** There is no "convert later" — write it correctly the first time. This rule exists because every past violation required a review round-trip to fix.

**Example — the wrong way vs. the right way:**

```rust
// ❌ WRONG — string matching for dispatch
fn try_parse_destroy(lower: &str) -> Option<Effect> {
    if lower.starts_with("destroy ") {
        let rest = &lower["destroy ".len()..];
        // ... parse target from rest
    }
    None
}

// ✅ RIGHT — nom combinator from the first line
fn try_parse_destroy(lower: &str) -> Option<Effect> {
    let (rest, _) = tag("destroy ").parse(lower).ok()?;
    // ... parse target from rest using parse_target_phrase(rest)
}
```

**See:** `oracle_casting.rs` for verb dispatch via `tag().parse()`, `oracle_trigger.rs` for `alt()` dispatch.

### Other Non-Negotiable Rules

Each rule below is defined in CLAUDE.md. One-sentence principle + codebase example.

| Rule | Example Location |
|------|-----------------|
| **Never match verbatim Oracle text** — decompose every phrase into typed building blocks (grammar + helpers + enums). A verbatim string match handles exactly one card and poisons the architecture. | Contrast: typed `QuantityRef`/`Comparator` vs. literal string |
| **Compose combinators by dimension** — N independent dimensions = N chained `alt()` calls, not N! branches. | `oracle_nom/condition.rs` multi-axis composition |
| **Nest by prefix dispatch** — shared prefixes use `preceded(tag(...), sub_combinator)` to eliminate redundant matching. | `oracle_trigger.rs` phase trigger nesting |
| **Word-boundary scanning** — try a combinator at each word boundary via scanning loop, not `contains()` chains. | `oracle_casting.rs::scan_timing_restrictions`, `oracle_trigger.rs::scan_for_phase` |
| **`parse_inner_condition` is the single authority** for all game-state conditions. Trigger/static parsers MUST delegate to it. | `oracle_nom/condition.rs::parse_inner_condition` |
| **No boolean flags** — parameterize with typed enums (`ControllerRef`, `Comparator`, `Option<T>`). | `types/ability.rs` effect variants |
| **No raw `i32` for amounts** — use `QuantityExpr` on all new effects. | `QuantityExpr::Fixed` vs `QuantityExpr::Ref` |
| **Separate abstraction layers** — `QuantityRef` contains only dynamic references. Constants belong in `QuantityExpr::Fixed`. | `QuantityExpr` wrapping `QuantityRef` |
| **`parse_number` vs `parse_number_or_x`** — use `_or_x` when X resolves to 0 (costs, P/T, counters). Use `parse_number` when X should remain as `Variable("X")` (effect quantities). | `oracle_nom/primitives.rs` |
| **All imports at file top** — never inline `use nom::*` inside function bodies. | Project-wide convention |
| **CR annotations mandatory** — with grep verification. See Section 8. | `docs/MagicCompRules.txt` |

### Self-Review Checklist

Ask these four questions after every parser change:

1. Did I duplicate logic that an existing helper already handles?
2. Is this inline extraction something that should use a shared building block?
3. Would this logic work for 50 cards, or just the one I'm looking at?
4. Did I extend the general pattern, or write a special case?

If any answer is wrong, **stop and refactor before moving on.**

---

## 2. Architecture Overview

### Parse Pipeline

```
Oracle text (from MTGJSON)
    ↓
strip_reminder_text()           — remove (parenthesized text)
    ↓
normalize_self_refs()           — card name / "this creature" → ~
    ↓
parse_oracle_text()             — classify line by priority (see §3)
    ├─ Keywords-only            → keyword extraction
    ├─ "When/Whenever/At"       → parse_trigger_line()        [oracle_trigger.rs]
    ├─ Contains ":"             → activated ability parsing     [oracle_cost.rs + oracle_effect/]
    ├─ is_static_pattern()      → parse_static_line()          [oracle_classifier.rs → oracle_static.rs]
    ├─ is_replacement_pattern() → parse_replacement_line()     [oracle_classifier.rs → oracle_replacement.rs]
    ├─ Imperative verb          → parse_effect_chain()         [oracle_effect/]
    ├─ dispatch_line_nom()      → parse_effect_chain_with_context() [oracle_dispatch.rs → oracle_effect/]
    └─ Fallback                 → Effect::Unimplemented
```

### Nom Combinator Layer — `oracle_nom/`

All parser branches delegate atomic parsing to shared nom 8.0 combinators:

| Sub-module | Purpose |
|-----------|---------|
| `primitives.rs` | Numbers, mana symbols, colors, counters, P/T, roman numerals, word-boundary guards |
| `target.rs` | Target phrase combinators, controller suffix, combat status |
| `quantity.rs` | Quantity expression combinators, "for each" patterns |
| `duration.rs` | Duration phrase combinators ("until end of turn", etc.) |
| `condition.rs` | Condition phrase combinators ("if", "unless", "as long as") |
| `filter.rs` | Filter property combinators (zone, type, controller, "with") |
| `error.rs` | `OracleResult` type, `parse_or_unimplemented` error boundary |
| `context.rs` | `ParseContext` for stateful parsing |
| `bridge.rs` | `nom_on_lower`, `nom_on_lower_required`, `nom_parse_lower` — mixed-case bridging |

### Two-Phase Parse/Lower Architecture

The parser uses a two-phase approach: **parse → AST → lower → Effect**.

```
parse_effect_clause()                    — entry point (oracle_effect/mod.rs)
  → parse_clause_ast()                   — classify sentence shape → ClauseAst
  → lower_clause_ast()                   — convert AST to Effect
    → lower_subject_predicate_ast()      — for SubjectPredicate clauses
    → lower_imperative_clause()          — for Imperative clauses
      → parse_imperative_effect()        — try special cases, then delegate
        → parse_imperative_family_ast()  — classify verb family (imperative.rs)
        → lower_imperative_family_ast()  — convert to Effect
```

### Parser Dispatch Architecture

- **Nom combinators** handle ALL parsing dispatch — atomic, structural, sentence-level verb dispatch, and top-level routing.
- **`TextPair`** provides dual-string case-bridging (subject-predicate decomposition, clause AST classification). `TextPair::strip_prefix` is correct for these structural operations.
- **`oracle_classifier.rs`** owns reusable line-classification helpers such as trigger-prefix, static-pattern, and replacement-pattern detection. `oracle.rs` remains the priority router that calls them.
- **`oracle_special.rs`** owns the router-adjacent special helpers for solve conditions, Defiler two-line statics, die-roll tables, self-reference normalization for static parsing, and keyword-line parsers like Escape/Harmonize/Cumulative Upkeep.
- **`oracle_effect/conditions.rs`** owns leading-condition splitting and ability-condition helpers. `oracle_effect/mod.rs` remains the clause/effect orchestrator and re-exports `split_leading_conditional`.
- **`oracle_effect/search.rs`** owns search/seek filter parsing helpers. `oracle_effect/mod.rs` re-exports the stable search helper surface used by imperative and continuation parsing.
- **New parser code** MUST use nom combinators. `starts_with`/`strip_prefix` for parsing dispatch is NOT acceptable (see Rule Zero).

---

## 3. Parsing Priority System

Lines in `parse_oracle_text()` are classified in this exact order. **First match wins.**

| Priority | Pattern | Router | Module |
|----------|---------|--------|--------|
| 1 | Keywords-only line (comma-separated) | Keyword extraction | `oracle.rs` |
| 2 | `"Enchant {filter}"` | Skipped (external) | — |
| 3 | `"Equip {cost}"` / `"Equip — {cost}"` | `try_parse_equip()` | `oracle.rs` |
| 4 | `"Choose one/two —"` (modal) | Bullet parsing | `oracle_modal.rs` |
| 5 | Planeswalker loyalty `[+N]/[-N]/[0]:` | `try_parse_loyalty_line()` | `oracle.rs` |
| 6 | Contains `":"` with cost prefix | Activated ability | `oracle_cost.rs` |
| 7 | Starts with `"When"` / `"Whenever"` / `"At"` | `parse_trigger_line()` | `oracle_trigger.rs` |
| 8 | `is_static_pattern()` matches | `parse_static_line()` | `oracle_static.rs` |
| 9 | `is_replacement_pattern()` matches | `parse_replacement_line()` | `oracle_replacement.rs` |
| 10 | Card is Instant/Sorcery + imperative | `parse_effect_chain()` | `oracle_effect/` |
| 11 | Roman numeral (saga chapter) | Skipped | — |
| 12 | Keyword cost line (kicker, etc.) | Skipped (MTGJSON handles) | — |
| 13 | Ability word prefix (`"Landfall —"`) | Strip prefix, re-classify from P7 | `oracle.rs` |
| 14a | `dispatch_line_nom()` — effect candidates | `parse_effect_chain_with_context()` | `oracle.rs` |
| 15 | Fallback | `Effect::Unimplemented` | — |

### `is_static_pattern()` detects:
`"gets +"`, `"gets -"`, `"get +"`, `"get -"`, `"have "`, `"has "`, `"can't be blocked"`, `"can't attack"`, `"can't block"`, `"enchanted "`, `"equipped "`, `"all creatures "`, `"enters with "`, `"cost {"`, and more. Check the function for the full list.

### `is_replacement_pattern()` detects:
`"as ~ enters"`, `"enters tapped"`, `"if damage would be dealt"`, `"instead"`.

---

## 4. Core Concepts

### 4a. Subject Stripping — The Key Design Decision

`strip_subject_clause()` removes subjects like "you", "target creature", "its controller" and recurses on the predicate. This simplifies parsing but **discards semantic information**.

**Rule:** If the subject encodes game-relevant information, intercept with a `try_parse_*` helper *before* stripping.

**When to intercept:** Subject determines WHO is affected, WHAT is referenced, or creates a sentence-internal dependency.
**When stripping is fine:** "You draw three cards" (caster always draws), "Destroy target creature" (target is in verb phrase).

```
"Its controller gains life equal to its power"
    ❌ strip_subject_clause → loses "its controller" → GainLife { player: Controller }  BUG
    ✅ try_parse_targeted_controller_gain_life() → GainLife { player: TargetedController, amount: TargetPower }
```

The `try_parse_*` intercept pattern is used in:
- `try_parse_subject_predicate_ast()` in `subject.rs` — for subject-verb clauses
- `lower_imperative_clause()` in `mod.rs` — for imperative clauses with semantic subjects

### 4b. ClauseAst Type System

Top-level sentence classification — `ClauseAst`:

| Variant | Shape | Example |
|---------|-------|---------|
| `Imperative` | Bare verb, no subject | "draw three cards" |
| `SubjectPredicate` | Subject + verb | "target creature gets +2/+2" |
| `Conditional` | Wrapped conditional | "if you control a creature, draw a card" |

Predicate types — `PredicateAst`:

| Variant | Detected by | Example |
|---------|------------|---------|
| `Continuous` | "gets/get", "has/have" | "gets +2/+2 and has flying" |
| `Become` | "becomes" | "becomes a 3/3 creature" |
| `Restriction` | "can't", "cannot" | "can't attack or block" |
| `ImperativeFallback` | None of the above | Falls back to imperative parsing |

Imperative family dispatch — `ImperativeFamilyAst`:

| Family | Sub-parser | Verb patterns |
|--------|-----------|---------------|
| `CostResource` | `parse_cost_resource_ast()` | add mana, pay life, deal damage |
| `ZoneCounter` | `parse_zone_counter_ast()` | destroy, exile, counter, put counter |
| `Numeric` | `parse_numeric_imperative_ast()` | draw, gain life, lose life, pump, scry, surveil, mill |
| `Targeted` | `parse_targeted_action_ast()` | tap, untap, sacrifice, discard, return, fight, gain control |
| `SearchCreation` | `parse_search_and_creation_ast()` | search library, dig, create token, copy token |
| `HandReveal` | `parse_hand_reveal_ast()` | look at hand, reveal hand, reveal top |
| `Choose` | `parse_choose_ast()` | target-only, named choice, reveal hand filter |
| `Shuffle` | `parse_shuffle_ast()` | shuffle, shuffle into library |
| `Put` | `parse_put_ast()` | put into/on top of |
| `Utility` | `parse_utility_imperative_ast()` | prevent, regenerate, copy, transform, attach |
| `YouMay` | "you may" prefix | Wraps inner effect |

Dispatch order: CostResource → ZoneCounter → Numeric → Targeted → SearchCreation → Explore/Proliferate → Shuffle → HandReveal → Choose → Put → YouMay.

### 4c. Clause Splitting & Continuations

`split_clause_sequence(text)` splits multi-sentence text on `.` (Sentence), `, then` (Then), and certain `,` boundaries. Respects parentheses and possessive apostrophes.

**Continuation absorption** — a follow-up clause modifies a preceding effect:

| Pattern | Continuation | What it does |
|---------|-------------|-------------|
| Search → "put into your hand" | `SearchDestination` | Appends ChangeZone sub_ability |
| RevealHand → "choose a nonland card" | `RevealHandFilter` | Patches card filter |
| Mana → "spend this mana only..." | `ManaRestriction` | Patches spend restriction |
| Counter → "that spell loses all abilities" | `CounterSourceStatic` | Patches source_static |
| Token → "suspect it" | `SuspectLastCreated` | Appends Suspect sub_ability |

Key functions: `parse_followup_continuation_ast()`, `parse_intrinsic_continuation_ast()`, `continuation_absorbs_current()`, `apply_clause_continuation()` — all in `oracle_effect/sequence.rs`.

### 4d. QuantityExpr / QuantityRef

```rust
pub enum QuantityExpr {
    Ref { qty: QuantityRef },   // dynamic — resolved from game state at runtime
    Fixed { value: i32 },       // literal constant
}
```

`QuantityRef` contains ONLY dynamic references (HandSize, LifeTotal, ObjectCount, TargetPower, Variable, etc.). Constants belong in `QuantityExpr::Fixed` — never put `Fixed(i32)` inside `QuantityRef`.

| Oracle phrase | Mapping |
|---------------|---------|
| "3 damage" | `QuantityExpr::Fixed { value: 3 }` |
| "damage equal to its power" | `QuantityExpr::Ref { qty: TargetPower }` |
| "X damage" | `QuantityExpr::Ref { qty: Variable { name: "X" } }` |
| "for each creature you control" | `QuantityExpr::Ref { qty: ObjectCount { filter } }` |

### 4e. Self-Reference Normalization

Before parsing, `normalize_self_refs()` replaces the card's name and phrases like "this creature" with `~`. The canonical phrase list lives in `oracle_util.rs` as `SELF_REF_TYPE_PHRASES` — update the constant, not each consumer.

`parse_target()` handles both `~` and type phrases → `TargetFilter::SelfRef` automatically. Any parser function checking self-references gets this for free via `parse_target`.

---

## 5. Deep Dive — `oracle_effect/` Directory

```
oracle_effect/
├── mod.rs          — Orchestrator: parse_effect_chain(), parse_effect_clause(), compound detection
├── conditions.rs   — Leading condition splitting and AbilityCondition extraction helpers
├── imperative.rs   — Imperative verb family parsing: parse_*_ast() + lower_*_ast()
├── search.rs       — Search/seek parsing helpers: search filters, seek details, destinations
├── subject.rs      — Subject-predicate parsing: try_parse_subject_predicate_ast()
├── sequence.rs     — Clause boundary splitting and continuation absorption
├── token.rs        — Token creation: "create a 1/1 white Spirit token with flying"
├── animation.rs    — Animation/become: "becomes a 3/3 creature with flying"
├── counter.rs      — Counter mechanics: put/remove/move/double counters
├── mana.rs         — Mana production and spend restrictions
└── types.rs        — All AST type definitions (ClauseAst, ImperativeFamilyAst, etc.)
```

### Subject-Predicate Parsing — `subject.rs`

`try_parse_subject_predicate_ast()` parses sentences with explicit subjects.

Subject resolution via `parse_subject_application()`:

| Subject text | Result |
|-------------|--------|
| "target creature" | Explicit target with TargetFilter |
| "all creatures", "each creature" | Mass filter |
| "~", "it", "this creature" | SelfRef |
| "enchanted creature" | EnchantedCreature |
| "equipped creature" | EquippedCreature |
| "defending player" | DefendingPlayer |
| "creatures you control" | Typed filter with controller: You |

Predicate hierarchy: `try_parse_subject_continuous_clause()` → `try_parse_subject_become_clause()` → `try_parse_subject_restriction_clause()` → fallback to `strip_subject_clause()` + imperative.

### Imperative Family Verb Patterns

**Numeric** (`parse_numeric_imperative_ast`): draw N, gain N life, lose N life, gets +X/+Y, scry N, surveil N, mill N. Also used by `try_parse_for_each_effect()` via `with_for_each_quantity()`.

**ZoneCounter** (`parse_zone_counter_ast`): destroy target/all, exile target/all, counter target spell, put N counters on target (delegates to `counter.rs`), remove N counters.

**Targeted** (`parse_targeted_action_ast`): tap/untap target, sacrifice, discard N, return to hand/battlefield, fight, gain control of.

**CostResource** (`parse_cost_resource_ast`): add {mana} (delegates to `mana.rs`), pay N life, pay {mana}, deal damage.

**SearchCreation** (`parse_search_and_creation_ast`): search your library, look at top N (dig), create token (delegates to `token.rs`), token copy.

**Token** (`token.rs`): Parses count → P/T → supertypes → colors → types → name → keywords → "where X is" expressions.

**Animation** (`animation.rs`): Parses "becomes a 3/3 [colors] [types] [keywords]" → `AnimationSpec` → `Vec<ContinuousModification>`.

**Counter** (`counter.rs`): `try_parse_put_counter`, `try_parse_remove_counter`, `try_parse_move_counters`, `try_parse_multiply_counter`, `try_parse_double_effect`.

**Mana** (`mana.rs`): `try_parse_add_mana_effect` (fixed symbols, colorless, any color, chosen color), `parse_mana_spend_restriction`, `try_parse_activate_only_condition`.

### Compound Action Detection — `mod.rs`

- `try_split_targeted_compound()` — "verb target X and verb2 it": uses `parse_target()` remainder to find split, inherits parent target via `replace_target_with_parent()`
- `try_parse_compound_shuffle()` — "shuffle X and Y into libraries": two ChangeZone effects
- `try_parse_for_each_effect()` — "draw a card for each creature": delegates to `parse_numeric_imperative_ast()` + `with_for_each_quantity()` + `thread_for_each_subject()`
- `parse_damage_player_scope()` / `parse_damage_each_player_scope()` — shared damage-player routing helpers. Use these for exact `each player` / `each opponent` / `each foe` damage phrases before falling back to `DamageAll`. Keep this semantic split in `oracle_effect/mod.rs`; do not push it into `parse_target()`, which remains object/filter-oriented.

### Special-Case Matchers in `parse_effect_clause()`

| Matcher | Pattern | Effect |
|---------|---------|--------|
| `try_parse_damage_prevention_disabled()` | "damage can't be prevented" | GenericEffect + DamagePreventionDisabled |
| `try_parse_still_a_type()` | "it's still a land" | GenericEffect + AddType |
| `try_parse_for_each_effect()` | "draw a card for each creature" | Numeric AST + for-each quantity |
| `try_parse_equal_to_quantity_effect()` | "mill cards equal to hand size" | Effect with QuantityExpr |

---

## 6. Other Parser Modules

| Module | Purpose | Invoked at Priority |
|--------|---------|-------------------|
| `oracle_classifier.rs` | Shared line-classification helpers: trigger prefixes, static/replacement detection, special routing heuristics. Called by `oracle.rs`, `oracle_dispatch.rs`, and class parsing. | Priority router support |
| `oracle_dispatch.rs` | Nom fallback dispatch for effect/static/replacement candidates before `Unimplemented`. | P14a |
| `oracle_special.rs` | Router-adjacent helpers for solve conditions, Defiler two-line statics, die-roll tables, static self-ref normalization, and keyword-line parsing (Escape/Harmonize/Cumulative Upkeep). | Priority router support |
| `oracle_trigger.rs` | Trigger parsing: subject + event decomposition, constraint parsing (OncePerTurn, OncePerGame). Uses `parse_trigger_subject()` → `try_parse_event()` pipeline. | P7 |
| `oracle_static.rs` | Static ability parsing: turn-condition handling (prefix "During your turn" and suffix "during your turn"), continuous modifications via `parse_continuous_modifications()`. | P8 |
| `oracle_replacement.rs` | Replacement effects: priority-ordered pattern matching (as-enters-choose before shock-land before fast-land, etc.), builder pattern with `ReplacementDefinition::new()`. | P9 |
| `oracle_condition.rs` | Restriction conditions: source/control/graveyard/hand/event conditions for "Cast only if..." / "Activate only if..." patterns. | Used by P6, P8 |
| `oracle_cost.rs` | Ability cost parsing: mana costs, tap/sacrifice/discard costs, `parse_single_cost()` for individual cost components. | P6 |
| `oracle_keyword.rs` | Keyword extraction: comma-separated keyword lists, parameterized keywords (ward, kicker), keyword grants. | P1, P12 |
| `oracle_casting.rs` | Casting options/restrictions: additional costs ("As an additional cost"), alternative costs, timing restrictions (flash, sorcery speed), `scan_timing_restrictions()`. | Pre-P1 |
| `oracle_modal.rs` | Modal spell parsing: "Choose N" headers, bullet mode collection, loyalty ability dispatch, `parse_oracle_block()` for block-level parsing. | P4, P5 |
| `oracle_class.rs` | Class card parsing (level-gated abilities). | Special pre-parse |
| `oracle_level.rs` | Leveler card parsing (LEVEL N-M power/toughness ranges). | Special pre-parse |
| `oracle_saga.rs` | Saga chapter parsing (roman numeral → chapter effects). | Special pre-parse |

### Event-Context References

`parse_event_context_ref()` in `oracle_target.rs` handles trigger-event anaphoric references:

| Oracle phrase | TargetFilter variant |
|---------------|---------------------|
| "that spell's controller" | `TriggeringSpellController` |
| "that player" | `TriggeringPlayer` |
| "that source" / "that permanent" | `TriggeringSource` |
| "defending player" | `DefendingPlayer` |

**Must be checked BEFORE standard `parse_target()` for trigger-based effects.**

### The Possessive vs. Targeting Fork

**Critical decision point — silent failure when wrong:**

```
"Look at your hand"              → contains_possessive → target: Controller
"Look at target opponent's hand" → parse_target → target: Typed { controller: Opponent }
```

- Possessive forms that fall to `parse_target` → no target found → `Unimplemented`
- Targeting forms matched by `contains_possessive` → targeting phase skipped → wrong player affected

---

## 7. Building Block Reference

**Search these modules BEFORE writing any new utility.** Duplicating what already exists is a defect.

| Module | What Lives Here | Use When |
|--------|----------------|----------|
| `oracle_nom/primitives.rs` | Numbers (digits, English words, articles), mana symbols/costs, colors, counter types, P/T modifiers, roman numerals, `parse_article_number` (word-boundary guard — prevents "another" → "a"), `scan_at_word_boundaries`, `scan_contains` | Parsing any atomic Oracle text element |
| `oracle_nom/target.rs` | Target phrase combinators, controller suffix, color prefix, combat status, self-reference, event-context refs | Parsing "target X" or type descriptions in nom pipelines |
| `oracle_nom/quantity.rs` | Quantity expressions, quantity refs, "equal to" patterns, "for each" patterns | Parsing counts and dynamic amounts in nom pipelines |
| `oracle_nom/duration.rs` | Duration phrases ("until end of turn", "for as long as ~", "until your next turn") | Parsing effect durations |
| `oracle_nom/condition.rs` | `parse_condition` (prefix + inner), `parse_inner_condition` (**single authority** for all game-state conditions) | Parsing "if/unless/as long as" — ALWAYS delegate here |
| `oracle_nom/filter.rs` | Zone filters, controller filters, property filters ("tapped", "attacking", "with flying", "with a +1/+1 counter") | Parsing object property constraints |
| `oracle_nom/error.rs` | `OracleResult` type alias, `parse_or_unimplemented` (nom `VerboseError` → `Effect::Unimplemented` with diagnostic trace), `option_to_nom` (adapt Option → nom alt chain) | Error handling at parser dispatch boundaries |
| `oracle_nom/bridge.rs` | `nom_on_lower` (run nom on lowercase, map consumed bytes back to original-case remainder), `nom_on_lower_required` (Result variant), `nom_parse_lower` (discard remainder) | Bridging mixed-case Oracle text to lowercase nom combinators |
| `oracle_nom/context.rs` | `ParseContext` (subject, quantity_ref, card_name, in_trigger, in_replacement) | Threading parse state across combinator boundaries |
| `oracle_util.rs` | `TextPair` (dual original/lowercase slices with `strip_prefix`/`strip_suffix`), `parse_number` wrapper, mana symbol parsing, `strip_reminder_text`, `normalize_card_name_refs`, possessive/pronoun matching (`contains_possessive`, `contains_object_pronoun`, `starts_with_possessive`), `match_phrase_variants`, `merge_or_filters`, `SELF_REF_TYPE_PHRASES`, `SELF_REF_PARSE_ONLY_PHRASES` | Case-bridging structural ops, shared string utilities, phrase matching |
| `oracle_target.rs` | `parse_target` (full target extraction), `parse_type_phrase` (type descriptions without "target"), `parse_player_reference`, `parse_event_context_ref`, `parse_zone_suffix` | High-level target/filter extraction from Oracle text |
| `oracle_quantity.rs` | `parse_quantity_ref` (semantic interpretation), `parse_cda_quantity` (CDAs), `parse_for_each_clause` ("for each [filter]") | Semantic quantity interpretation from Oracle text |

**Damage-player routing** (`oracle_effect/mod.rs`) — exact player-set phrases in damage effects have a dedicated helper path:

| Helper | Purpose | Use When |
|--------|---------|----------|
| `parse_damage_player_scope()` | Parse the player noun for damage phrases: `player`, `opponent`, `foe` | Reusing the noun parse across simple and compound damage clauses |
| `parse_damage_each_player_scope()` | Parse exact `each player/opponent/foe` with punctuation-only tails allowed | Routing `DealDamage` text to `DamageEachPlayer` instead of `DamageAll` |

Rule: if the Oracle text is a damage effect that names a set of players, resolve that at the effect layer with these helpers. Do not teach `parse_target()` that `each opponent` is a player-damage target, because that would blur the object-target/filter boundary and reintroduce object-vs-player bugs.

### Sub-Ability Chains & Target Propagation

`parse_effect_chain()` splits on `. ` boundaries and links clauses as `sub_ability`. At runtime, `resolve_ability_chain()` walks the chain. When a parent ability has targets but the sub-ability does not, targets propagate automatically. Sub-abilities do NOT need their own target lists.

---

## 8. CR Annotation Protocol

**MANDATORY for any code implementing MTG game rules. Non-optional.**

### Verification — Before Writing ANY CR Number

```bash
# REQUIRED — run these BEFORE writing the annotation:
grep -n "^701.21" docs/MagicCompRules.txt   # Verify keyword action number
grep -n "^702.122" docs/MagicCompRules.txt  # Verify keyword ability number
grep -n "^704.5a" docs/MagicCompRules.txt   # Verify SBA rule
```

**If you cannot find the rule number, do NOT write the annotation.** Flag it as "needs manual verification" instead. 701.x and 702.x numbers are arbitrary sequential assignments — LLMs consistently hallucinate them.

**A wrong CR number is worse than no CR number. It creates false confidence that code was verified against the wrong rule.**

### Format

```rust
// CR 704.5a: A player with 0 or less life loses the game.
/// Checks state-based actions (CR 704).
// CR 702.2c + CR 702.19b: Deathtouch with trample assigns lethal (1).
// CR 704.3 / CR 800.4: SBAs may have ended the game during auto-advance.
```

- Prefix: Always `CR`. Never `Rule`, `MTG Rule`, or bare numbers.
- Description is mandatory — bare `CR 704.5a` with no explanation is not acceptable.
- `+` for interacting rules, `/` for alternative/overlapping rules.
- Only annotate game logic, not boilerplate/plumbing.

---

## 9. Checklists

### 9a. Adding a New Parser Pattern

**Phase 1 — Identify Where It Belongs**
- Imperative verb/family → the relevant `parse_*_ast()` in `imperative.rs`
- Subject + predicate → `try_parse_subject_*` in `subject.rs`
- Token creation → `token.rs`
- Animation/become → `animation.rs`
- Counter mechanics → `counter.rs`
- Mana production → `mana.rs`
- Continuation/absorption → `sequence.rs`
- Trigger → `oracle_trigger.rs`
- Static → `oracle_static.rs`
- Replacement → `oracle_replacement.rs`
- Routing gate → `is_static_pattern()` / `is_replacement_pattern()` in `oracle.rs`

**Phase 2 — Add the Pattern**
- [ ] Write the parser test FIRST
- [ ] Use nom combinators from the first line (Rule Zero)
- [ ] Use existing helpers — `parse_target()`, `parse_number()`, `contains_possessive()`, `parse_type_phrase()`
- [ ] More specific patterns go BEFORE more general ones

**Phase 3 — Handle the Subject**
- [ ] Does the subject carry game-relevant info? → add `try_parse_*` interceptor
- [ ] Otherwise, subject stripping is fine

**Phase 4 — Chain Composition**
- [ ] Check continuation system in `sequence.rs`
- [ ] Check `parse_effect_chain()` for special chaining

**Phase 5 — Routing**
- [ ] Update `is_static_pattern()` or `is_replacement_pattern()` if text is routed to the wrong parser

**Phase 6 — Tests & Verification**
- [ ] Parser unit tests for each new pattern
- [ ] Snapshot test: `crates/engine/tests/oracle_parser.rs`
- [ ] `cargo coverage` — Unimplemented count should decrease
- [ ] `cargo test -p engine && cargo clippy --all-targets -- -D warnings`

### 9b. Adding a New Effect Type

Cross-reference the `/add-engine-effect` skill for the full 8-phase lifecycle (types → handler → targeting → parser → interactive → multiplayer → frontend → AI → tests).

### 9c. Adding a New Trigger Event

Cross-reference the `/add-trigger` skill. Parser-specific: add pattern in `try_parse_event()`, wire subject into `valid_card`/`valid_source`, add tests.

**Simple-verb events** (e.g., `stations`, `crews a vehicle`, `saddles a mount`, `becomes saddled`): add a `SimpleEvent::*` variant in `parse_simple_event`, then a `tag(...)` arm in the appropriate `alt()` group. Compound events (e.g., `saddles a mount or crews a vehicle`) MUST precede their singular components so the compound matches first. Dispatch sets `def.mode` + `valid_card` (or `valid_source` for pronoun-context subjects).

**Actor-side compound-subject matchers**: when a trigger's subject filter may include non-source creatures (e.g., "Tiana or another legendary creature you control crews a Vehicle"), the runtime matcher MUST consult `trigger.valid_card` against the event's actor list (e.g., `event.creatures`) via `matches_target_filter` from `game/filter.rs`. See `match_crews` / `match_saddles` / `match_saddles_or_crews` + the shared `match_actor_against_filter` helper in `trigger_matchers.rs` for the canonical pattern.

**Condition-scoped constraint recognition**: trigger-frequency qualifiers like `"for the first time each turn"` must be detected against the post-`split_trigger` condition text only, NOT the full Oracle text — otherwise any card whose EFFECT text coincidentally contains the phrase is silently constrained. The phrase is then stripped from `condition_text` before dispatch so verbatim handlers (e.g., `"whenever you cycle another card"`) hit unchanged, and the constraint is applied as a fallback in `parse_trigger_line` only when no stronger text-based constraint (`OnlyDuringYourMainPhase`, `OncePerTurn` via explicit text) was set. See the condition-scoped assignment block in `parse_trigger_line` for the canonical pattern.

### 9d. Adding a New Phrase Helper

1. Identify phrase variants
2. Implement via `match_phrase_variants()` in `oracle_util.rs`
3. Export from module
4. Add tests for all variants

### 9e. Adding a New Replacement Pattern

1. Add `parse_*` function matching the Oracle text
2. Insert at correct priority in `parse_replacement_line()` — before any overlapping pattern
3. Add parser tests

---

## 10. Common Pitfalls

| Mistake | Consequence | Fix |
|---------|-------------|-----|
| `starts_with("verb ")` for dispatch | Bypasses nom, no structured errors | `tag("verb ").parse(lower)` or `nom_on_lower` |
| `&text[N..]` hardcoded byte offset | Off-by-one, mixed-case breakage | `nom_on_lower` calculates remainder automatically |
| `find()` / `split_once()` / `contains()` for parsing | Bypasses nom architecture | Use nom combinators — Rule Zero |
| Reimplementing number/color/mana parsing | Duplicates existing combinators | Delegate to `oracle_nom::primitives` |
| `tag("a")` without word boundary | "another" falsely matches as "a" | Use `parse_article_number` |
| `parse_number` for X-cost values | X not converted to 0 | Use `parse_number_or_x` |
| Hardcoding `amount: 1` when unparseable | Gap invisible in coverage | Return `Effect::Unimplemented` |
| Boolean flags on effect types | Undefined combinations, obscured intent | Use enum variant |
| Losing subject via `strip_subject_clause` | "Its controller gains life" → wrong player | Add `try_parse_*` interceptor |
| Pattern too broad, shadows existing | Existing cards break | Specific before general; test existing patterns |
| `parse_target` for possessive forms | No target found → Unimplemented | Use `contains_possessive` → Controller |
| `contains_possessive` for targeting forms | Targeting skipped → wrong player | Use `parse_target` → typed filter |
| Monolithic condition parsing | Fragile, card-specific | Use subject+event decomposition |
| Splitting on " and " naively | Breaks compound effects | Use `try_split_targeted_compound` |
| Putting `Fixed(i32)` inside `QuantityRef` | Wrong abstraction layer | `QuantityRef` = dynamic only; `Fixed` in `QuantityExpr` |
| Editing `mod.rs` when sub-module is right | Bloats orchestrator | Token → `token.rs`, mana → `mana.rs`, counters → `counter.rs`, leading conditions → `conditions.rs` |
| `unwrap()` on parse results | Parser panics on unknown text | Return `None` or `Effect::Unimplemented` |
| Not recognizing `~` as self-reference | Self-targeting fails | `parse_target` handles both `~` and type phrases |
| Inline `use nom::*` in function bodies | CLAUDE.md prohibition | All imports at file top |
| `Unimplemented` with misleading `name` | Coverage miscategorizes gap | Actual verb as `name`, full text as `description` |
| **Peek-vs-chomp** — upstream `scan_*` / detector reads marker text without consuming, downstream loop re-encounters and warns or drops it | "Swallow:*" warning emitted even though semantic was captured upstream; or qualifier text silently dropped on routing | Either single-pass read-and-chomp in the upstream helper, OR add a matching consume-without-record arm in the downstream dispatch loop. See `scan_distinct_names_clause` (peek) ↔ `parse_search_filter_suffixes` "with different name[s]" (chomp) for the canonical pair. |

---

## 11. Diagnostics — Swallow Detectors & `parse_warnings`

The parser must never silently discard Oracle text. Every clause must either be represented in the parsed AST OR cause the line to fail and yield `Effect::Unimplemented` carrying the original phrase. **Anything in between is a parser lie.**

The `crates/engine/src/parser/swallow_check.rs` module audits each card's parsed `ParsedAbilities` against its original Oracle text and emits a `parse_warning` for every marker phrase that has no AST representation. Findings surface in the coverage report via `CardFace::parse_warnings` (also written into each card's entry in `client/public/card-data.json`).

**Reading current swallow gaps:**

```bash
# Count total active warnings
jq -r '[.[] | .parse_warnings // [] | .[]] | length' client/public/card-data.json

# Top warning classes by frequency
jq -r '[.[] | .parse_warnings // [] | .[]] | map(split(":")[0:2] | join(":")) |
       group_by(.) | map({prefix: .[0], count: length}) | sort_by(.count) | reverse' \
       client/public/card-data.json

# Sample cards in a specific class
jq -r '[.[] | .parse_warnings // [] | .[] | select(startswith("Swallow:DynamicQty"))] | .[0:5]' \
       client/public/card-data.json
```

**Detector class prefixes** (one row per detector in `swallow_check.rs`):

| Prefix | What it flags |
|---|---|
| `Swallow:Condition_If` | "if <condition>" present in text but no `condition`/`constraint`/`if_clause` slot in AST |
| `Swallow:Condition_Unless` | "unless …" not bound to `unless_filter` / `unless_*` slot |
| `Swallow:Condition_AsLongAs` | "as long as …" not bound to a conditional static |
| `Swallow:DynamicQty` | "for each / equal to / the number of / twice / half" present but AST has only `Fixed` quantity values — the canonical **count parsed but routed downstream as Fixed** bug class |
| `Swallow:Duration_ThisTurn` / `_UntilEndOfTurn` / `_NextTurn` | duration phrase present but no `duration` slot populated |
| `Swallow:Optional_YouMay` / `_MayHave` | "you may …" / "may have it …" not bound to the optional flag |
| `Swallow:Replacement_Instead` | " instead" present but no replacement definition emitted |
| `Swallow:ActivateOnlyDuring` / `:ActivateLimit` | activation timing/limit phrase not bound to a restriction slot |
| `Swallow:APNAP` | "starting with you" / "in turn order" not bound to order metadata |
| `target-fallback:` | secondary class — `parse_target` couldn't classify a noun phrase, or a downstream chomping loop encountered an unmatched filter suffix |

**Workflow:**
1. When fixing a swallow, identify the dispatch site that *recognized* the marker but failed to either capture or chomp it. The fix is almost always at one of two places: the upstream recognition (route through the right `try_parse_*` interceptor) or the downstream chomping loop (add a missing arm). The peek-vs-chomp pitfall in §10 is the recurring root cause.
2. After fixing, regenerate (`./scripts/gen-card-data.sh`) and recount; warnings should drop by exactly the affected class size.
3. **Suppression rule** — `swallow_check.rs` skips all detectors when ANY ability on the card was already `Effect::Unimplemented` (the gap is reported there); only partial-parse cards run the full detector suite. So a "5-card drop" in `Swallow:DynamicQty` may also un-mute additional detectors on the same cards.

**Companion Python audit:** `scripts/swallow_audit.py` runs the same heuristics over `coverage-data.json` independently of the Rust runtime. Use it for cross-checking, or for exploring novel detector classes before promoting them to `swallow_check.rs`.

---

## 12. Self-Maintenance

After completing work using this skill:

1. **Verify references** with the script below
2. **Update the priority table** (§3) if parsing order changed
3. **Update the AST family tables** (§4b) if new families or continuations were added
4. **Update the deep dive** (§5) if new sub-modules were added to `oracle_effect/`
5. **Update the module catalog** (§6) if new `oracle_*.rs` modules were added

### Verification Script

```bash
rg -q "fn parse_oracle_text" crates/engine/src/parser/oracle.rs && \
rg -q "fn is_static_pattern" crates/engine/src/parser/oracle_classifier.rs && \
rg -q "fn is_replacement_pattern" crates/engine/src/parser/oracle_classifier.rs && \
rg -q "fn dispatch_line_nom" crates/engine/src/parser/oracle_dispatch.rs && \
rg -q "fn parse_effect_chain" crates/engine/src/parser/oracle_effect/mod.rs && \
rg -q "fn parse_effect_clause" crates/engine/src/parser/oracle_effect/mod.rs && \
rg -q "fn parse_imperative_effect" crates/engine/src/parser/oracle_effect/mod.rs && \
rg -q "fn split_leading_conditional" crates/engine/src/parser/oracle_effect/conditions.rs && \
rg -q "fn strip_leading_general_conditional" crates/engine/src/parser/oracle_effect/conditions.rs && \
rg -q "fn parse_search_library_details" crates/engine/src/parser/oracle_effect/search.rs && \
rg -q "fn parse_seek_details" crates/engine/src/parser/oracle_effect/search.rs && \
rg -q "fn parse_search_destination" crates/engine/src/parser/oracle_effect/search.rs && \
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
rg -q "fn parse_replacement_line" crates/engine/src/parser/oracle_replacement.rs && \
rg -q "fn parse_inner_condition" crates/engine/src/parser/oracle_nom/condition.rs && \
rg -q "pub fn parse_number" crates/engine/src/parser/oracle_nom/primitives.rs && \
rg -q "pub fn parse_number_or_x" crates/engine/src/parser/oracle_nom/primitives.rs && \
rg -q "pub fn parse_color" crates/engine/src/parser/oracle_nom/primitives.rs && \
rg -q "pub fn parse_mana_cost" crates/engine/src/parser/oracle_nom/primitives.rs && \
rg -q "fn parse_or_unimplemented" crates/engine/src/parser/oracle_nom/error.rs && \
rg -q "pub type OracleResult" crates/engine/src/parser/oracle_nom/error.rs && \
rg -q "pub fn nom_on_lower" crates/engine/src/parser/oracle_nom/bridge.rs && \
rg -q "pub fn scan_at_word_boundaries" crates/engine/src/parser/oracle_nom/primitives.rs && \
test -f crates/engine/src/parser/oracle_keyword.rs && \
test -f crates/engine/src/parser/oracle_casting.rs && \
test -f crates/engine/src/parser/oracle_modal.rs && \
test -f crates/engine/src/parser/oracle_class.rs && \
test -f crates/engine/src/parser/oracle_level.rs && \
test -f crates/engine/src/parser/oracle_saga.rs && \
echo "✓ oracle-parser skill references valid" || \
echo "✗ STALE — update skill references"
```
