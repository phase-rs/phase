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
| **Compose combinators by dimension** — N independent axes = a sum of per-axis `alt()`/`opt()` calls inside one sequence, never a flat `alt` of full-string `tag`s (the product). Variation in the *middle* or an *optional* segment still factors: `recognize((tag(..), alt(..), tag(..), opt(..), tag(..)))`. Smell: a flat `alt` whose arms share a long common prefix **and** suffix. | `oracle_nom/condition.rs` multi-axis composition; PATTERNS.md §8b |
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

### Parse Pipeline — Document-Level Two-Phase (parse → IR → lower)

`parse_oracle_text()` (oracle.rs, near the bottom of the file) is the public
entry point and a **thin wrapper** over two phases: `parse_oracle_ir()` (IR
production) followed by `lower_oracle_ir()` (IR lowering). Diagnostics flow
through `OracleDocIr.diagnostics` → `ParsedAbilities.parse_warnings`.

```
Oracle text (from MTGJSON)
    ↓
parse_oracle_ir()               — oracle.rs: the priority router (see §3)
    ├─ normalize_card_name_refs()   — card name / "this creature" → ~ (once, at entry)
    ├─ pre-parsers (before the line loop): Saga chapters [oracle_saga.rs],
    │     Attraction visit lines [oracle_attraction.rs], Class levels
    │     [oracle_class.rs], Leveler LEVEL blocks [oracle_level.rs],
    │     Spacecraft "N+ |" thresholds [oracle_spacecraft.rs], Strive cost
    ├─ per line: strip_reminder_text(), then classify by priority slot (§3)
    ↓
OracleDocIr                     — oracle_ir/doc.rs: Vec<OracleItemIr> + diagnostics.
    │                             Core variants carry typed IR (EffectChainIr,
    │                             TriggerIr, StaticIr, ReplacementIr); PreLowered
    │                             variants carry already-assembled engine types.
    ↓
lower_oracle_ir()               — oracle.rs: exhaustive match on each OracleItemIr
    ↓                             (core IR → dedicated lowering fn; PreLowered → identity)
ParsedAbilities                 — abilities / triggers / statics / replacements /
                                  keywords / casting options + parse_warnings
```

Per-line classification inside `parse_oracle_ir` (simplified; §3 has the full slot table):

```
    ├─ Keywords-only            → keyword extraction
    ├─ "When/Whenever/At"       → parse_trigger_line()        [oracle_trigger.rs]
    ├─ Contains ":"             → activated ability parsing     [oracle_cost.rs + oracle_effect/]
    ├─ is_static_pattern()      → parse_static_line()          [oracle_classifier.rs → oracle_static/]
    ├─ is_replacement_pattern() → parse_replacement_line()     [oracle_classifier.rs → oracle_replacement.rs]
    ├─ Imperative verb          → parse_effect_chain()         [oracle_effect/]
    ├─ dispatch_line_nom()      → parse_effect_chain_with_context() [oracle_dispatch.rs → oracle_effect/]
    └─ Fallback                 → Effect::Unimplemented
```

### IR Layer — `oracle_ir/`

| Sub-module | Purpose |
|-----------|---------|
| `ast.rs` | All parser AST types (`ParsedEffectClause`, `ClauseAst`, modal/loyalty AST — moved here from `oracle_effect/types.rs`, `oracle_modal.rs`, `oracle.rs`) |
| `doc.rs` | Document-level IR: `OracleDocIr`, `OracleItemIr` |
| `context.rs` | `ParseContext` for stateful parsing (subject, actor, card_name, host_self_reference, …) |
| `diagnostic.rs` | `OracleDiagnostic` — structured parse warnings |
| `effect_chain.rs` | `EffectChainIr` + lowering for spell/ability effect chains |
| `trigger.rs` | `TriggerIr` + lowering |
| `static_ir.rs` | `StaticIr` + lowering |
| `replacement.rs` | `ReplacementIr` + lowering |

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
| `error.rs` | `OracleError` / `OracleResult` type aliases, `oracle_err` error constructor. Parse-failure authority is `Effect::unimplemented(name, fragment)` (`types/ability.rs`) — never hand-construct `Effect::Unimplemented { .. }` literals |
| `context.rs` | Re-export shim for `ParseContext` (canonical home: `oracle_ir/context.rs`) |
| `bridge.rs` | `nom_on_lower`, `nom_on_lower_required`, `nom_parse_lower` — mixed-case bridging |
| `enchant.rs` | "Enchant {filter}" attachment-restriction combinators |
| `return_as_aura.rs` | "return ~ ... attached to" / return-as-Aura combinators |

### Two-Phase Parse/Lower Architecture (clause level)

Within the effect branch, the parser uses the same two-phase approach at clause
granularity: **parse → AST → lower → Effect**.

```
parse_effect_clause()                    — entry point (oracle_effect/mod.rs)
  → clause_shell::peel_clause()          — strip structural slots first (see below)
  → parse_clause_ast()                   — classify sentence shape → ClauseAst
  → lower_clause_ast()                   — convert AST to Effect
    → lower_subject_predicate_ast()      — for SubjectPredicate clauses
    → lower_imperative_clause()          — for Imperative clauses
      → parse_imperative_effect()        — try special cases, then delegate
        → parse_imperative_family_ast()  — classify verb family (imperative.rs)
        → lower_imperative_family_ast()  — convert to Effect
```

### Clause Shell — Structural Slot Peeling (`clause_shell.rs`)

**This is the destination architecture for structural slots.** Phase 2 of the
no-text-swallowing refactor (see `data/parser-swallow-progress.md`) inverts
slot-consumption responsibility: instead of every body parser recognizing AND
consuming surrounding structural slots inline (and silently dropping them when
it forgets — the swallowing bug class), `peel_clause(text)` recursively strips
slot-bearing prefixes/suffixes off the clause, accumulating them into a
`ClauseContext` (synthesized attributes). The bare imperative remainder is
handed to the existing body parsers, and the context is applied back onto the
parsed result (`apply_optional`, `duration()`, `condition()`), so no recognized
slot can be dropped.

**Slots live in the shell today:**
- **Optional** — `"you may [verb]"` → `ClauseContext.optional` (CR 608.2d), with
  a specialized-phrase blocklist (`is_specialized_you_may_phrase`) for "you may"
  constructions whose dedicated body parsers need the full surface form
  (alt-costs, impulse-draw permission, causative "have", retarget, Dig
  keep-from-among, specialized reveals).
- **Duration** — trailing duration suffix → `ClauseContext.duration`, delegating
  to `strip_trailing_duration` so the suffix table stays single-source (with an
  `is_specialized_duration_carrier` guard for parsers that need the suffix as a
  disambiguation signal, e.g. impulse-draw vs `CastFromZone`).
- **Leading condition** — `"if [cond], [effect]"` → `ClauseContext.condition`,
  delegating to `strip_leading_general_conditional` (which routes through
  `parse_inner_condition`).

**Call site:** `parse_effect_clause()` in `oracle_effect/mod.rs` (single peel
point; `oracle_trigger.rs` and `oracle_effect/subject.rs` reference the peel
in comments where their behavior depends on it). If the peeled variant lands
in `Unimplemented`, the original text is retried — the shell is conservative.

**Rule for new work:** when adding handling for a new *structural* slot type
(durations, optional clauses, conditions, and future siblings like APNAP order
or activation limits), migrate it into the shell rather than adding another
per-callsite `strip_*` pass: add a field on `ClauseContext`, a branch in
`peel_inner`, an `apply_*` method, and remove the linear `strip_*` calls at the
body-parser call sites. Do not add new inline slot consumption to body parsers.

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

Lines in `parse_oracle_ir()` (oracle.rs) are classified slot by slot. **First
match wins — the binding order is the SOURCE ORDER of the slots in the line
loop, NOT the numeric labels.** The labels are historical and non-monotonic
(`8b (early)` runs before `0`; loyalty `11` runs before the `3x` keyword-line
slots). When inserting a new slot, place it by evaluation position and grep
`// Priority` in oracle.rs to see the live order; the table below mirrors that
grep in evaluation order (one row per `// Priority <label>:` comment — the CI
gate `scripts/check-skill-doc.sh` asserts the row count matches the code).

Before the line loop, `parse_oracle_ir` runs **pre-parsers** that consume whole
line ranges: Saga chapters, Attraction visit lines, Class level sections (early
return), Leveler LEVEL blocks, Spacecraft `N+ |` thresholds, and the Strive
cost scan. Consumed line indices are skipped by the loop.

Unlabeled handlers interleaved between labeled slots are shown as `—` rows.

| Label | Pattern | Router | Module |
|-------|---------|--------|--------|
| `14` | Line empty after reminder-text + "X can't be 0." stripping (stamps `min_x_value` on the previous ability first) | skip | `oracle.rs` |
| `8b (early)` | "As an additional cost to cast this spell …" (non-Defiler) — must precede static classifiers that match embedded "this spell costs {N} less" tails | `parse_additional_cost_line()` → `result.additional_cost` | `oracle_casting.rs` |
| `0` | Semicolon-separated keyword line ("Defender; reach"); colon guard excludes activated abilities | per-part keyword extraction | `oracle.rs` |
| `1` | Modal block: "Choose one —" header + mode lines, or Spree + `+` lines (consumes multiple lines) | `parse_oracle_block()` + `lower_oracle_block()` | `oracle_modal.rs` |
| — | "Equip {cost}" / "Equip — {cost}" (not "Equipped …"); "Crew N" with trailing cadence sentence | `try_parse_equip()`, `parse_crew_keyword()` | `oracle.rs` |
| `1b` | Keyword-only line (guard: "{kw} abilities you activate cost {N} less" is a static, not a keyword line) | `extract_keyword_line()` | `oracle_keyword.rs` |
| `2` | "Enchant {filter}" | skip (handled externally) | — |
| — | Commander-permission / deck-construction copy-limit sentences (skip); named equip "<Name> — Equip {cost}" | `try_parse_equip()` | `oracle.rs` |
| `11` | Planeswalker loyalty `+N:` / `−N:` / `0:` / `[+N]:` (runs here despite the label) | `try_parse_loyalty_line()` | `oracle.rs` |
| — | Granted-quoted statics: `is_granted_static_line()` ("enchanted/equipped/all/… has/gains \"…\"" ), incl. compound can't-win/lose split | `parse_static_line_with_graveyard_keyword_continuation()` | `oracle_static/` |
| `3b` | "To solve — {condition}" (CR 719.1) | `parse_solve_condition()` → `result.solve_condition` | `oracle_special.rs` |
| `3c` | "Channel — {cost}, Discard this card: {effect}" | activated-ability build | `oracle.rs` |
| `3d` | "Boast — {cost}: {effect}" (implicit attacked-this-turn + once-per-turn restrictions, CR 702.142a) | activated-ability build | `oracle.rs` |
| `3e` | "Exhaust — {cost}: {effect}" (implicit activate-only-once, CR 702.177a) | activated-ability build | `oracle.rs` |
| `3e2` | "Power-up — {cost}: {effect}" (activate only once per game, MV cost reduction if entered this turn, CR 602.5b) | activated-ability build | `oracle.rs` |
| `3f` | "Forecast — {cost}: {effect}" (hand-only, your upkeep, once per turn, CR 702.57a-b) | activated-ability build | `oracle.rs` |
| `4` | Activated ability — contains `":"` with cost-like prefix | `find_activated_colon()` + `parse_activated_ability_definition()` | `oracle_cost.rs` + `oracle_effect/` |
| `5-pre` | Trigger-framed "… enters with [counters] on it" — CR 614.1c replacement despite When/Whenever framing | `parse_replacement_line()` | `oracle_replacement.rs` |
| `5-6` | Triggered abilities — `has_trigger_prefix()` (When/Whenever/At); compound triggers produce multiple `TriggerDefinition`s (CR 603.2) | `parse_trigger_line()` | `oracle_trigger.rs` |
| `6b` | Ability-word-prefixed activated/trigger lines ("Threshold — {T}: …", "Heroic — Whenever …") — must precede static/replacement gates | strip word + re-route | `oracle.rs` |
| `6c-defiler` | Defiler cycle: "As an additional cost to cast [color] permanent spells, you may pay N life. Those spells cost {C} less…" — static, not self-cost | `parse_defiler_cost_reduction()` | `oracle_special.rs` |
| `6c-altcost` | "You may pay X rather than pay the mana cost for [filter] spells you cast" (CR 118.9; Fist of Suns class) | `parse_spells_alternative_cost()` | `oracle_static/cost_mod.rs` |
| `6c-altcost-b` | "You may cast [filter] by paying {X} rather than paying their mana costs" (Primal Prayers) | `parse_cast_spells_alternative_cost_multi()` | `oracle_static/cost_mod.rs` |
| `6c-altcost-c` | "You may collect evidence N rather than pay …" (Conspiracy Unraveler class) | `parse_collect_evidence_alt_cost()` | `oracle_static/cost_mod.rs` |
| `6c-altcost-d` | "For each {C} in a cost, you may pay 2 life rather than pay that mana" (K'rrik class, CR 107.4f) | static-line parse | `oracle_static/` |
| `6c-altcost-e` | "You may [cost] rather than pay [keyword] cost[s]" (New Perspectives / Heart of Kiran class) | `parse_alternative_keyword_cost()` | `oracle_static/cost_mod.rs` |
| `6d` | Compound "enters tapped and doesn't untap during your untap step" — decomposed into ETB-tapped replacement (CR 614.1c) + CantUntap static (CR 502.3) | both parsers run | `oracle.rs` |
| `7` | Static/continuous patterns — `is_static_pattern()`; spell lines with explicit durations and damage verbs are deferred to `9`; copy-replacement lines route to the replacement parser first | `parse_static_line_multi()` family | `oracle_classifier.rs` → `oracle_static/` |
| `8` | Replacement patterns — `is_replacement_pattern()`; one paragraph can yield multiple ETB replacements | `parse_replacement_line()` | `oracle_classifier.rs` → `oracle_replacement.rs` |
| `8c` | Leyline clause "If this card is in your opening hand, you may begin the game with it on the battlefield" (CR 103.6) | `parse_begin_game_clause()` | `oracle.rs` |
| `8c-strive` | Strive lines — skip (cost extracted by the pre-loop scan) | skip | `oracle.rs` |
| — | Casting restrictions ("Cast this spell only …"), spell casting options, die-roll tables (`try_parse_die_roll_table`, consumes header + table lines), Suspend/Specialize/Harmonize/Mayhem keyword-cost extraction | various | `oracle_casting.rs`, `oracle_special.rs`, `oracle_keyword.rs` |
| `8f` | Kicker / Multikicker / Replicate cost lines — before the spell catch-all so they don't become Unimplemented | keyword extraction | `oracle.rs` |
| `9` | Card is Instant/Sorcery → imperative spell body | `parse_effect_chain()` | `oracle_effect/` |
| — | Flashback-equal-to-mana-cost, Commander ninjutsu, Escape em-dash, Cumulative upkeep keyword extraction | keyword extraction | `oracle.rs` / `oracle_keyword.rs` |
| `12` | Roman numeral chapters (saga) | skip (pre-parsed) | — |
| `13` | Keyword cost lines (`is_keyword_cost_line`) — extract parameterized keyword (e.g. "Morph {2}{B}") then skip | `parse_keyword_from_oracle()` | `oracle_keyword.rs` |
| `13b` | Kicker/Multikicker leftovers | skip (handled by keywords) | — |
| `13c` | Vehicle tier lines "N+ \| keyword(s)" | skip | `oracle_classifier.rs` |
| `13d` | "Activate only…" constraint line | skip | — |
| `13e` | "X can't be 0." annotation → `min_x_value` on previous ability | defensive fallback | `oracle.rs` |
| `14` | Ability word prefix ("Landfall —") — strip, map known words to typed conditions, re-classify the body | `strip_ability_word_with_name()` + `ability_word_to_condition()` | `oracle.rs` |
| `14a` | Nom fallback dispatch — try effect, trigger, static, and replacement sub-parsers | `dispatch_line_nom()` | `oracle_dispatch.rs` |
| `15` | Final fallback | `Effect::Unimplemented` with diagnostic trace | — |

### `is_static_pattern()` — `oracle_classifier.rs`
Gates Priority `7`. Returns false for `target`-leading lines, then matches
`STATIC_CONTAINS_PATTERNS` (word-boundary scan: "gets +", "have ", "can't
attack", "nonland ", "you may spend mana as though", …), `STATIC_PREFIX_PATTERNS`,
and `is_static_compound_pattern()` (graveyard/top-of-library/exile cast
permissions, "spells can't be cast", flash grants, …). Check the constants in
`oracle_classifier.rs` for the full lists.

### `is_replacement_pattern()` — `oracle_classifier.rs`
Gates Priority `8`. Matches `REPLACEMENT_CONTAINS_PATTERNS` ("would ",
"prevent all", "enters tapped/untapped/prepared", "enter as a copy of",
"become a copy of"), trailing " enter tapped/untapped", counter-prohibition
phrases (CR 614.17), and `is_replacement_compound_pattern()` (as-enters-choose,
enters/escapes + counter, tapped-for-mana + instead, madness discard).

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

Imperative family dispatch — `ImperativeFamilyAst` (oracle_ir/ast.rs). The enum
has three layers of variants: direct families, `Structured(ImperativeAst)`
wrapping the structured families (`Numeric`, `Targeted`, `SearchCreation`,
`HandReveal`, `Choose`, `Utility`), and a tail of keyword-action leaves
(`Explore`, `Connive`, `Investigate`, `Learn`, `Manifest`, `Proliferate`,
`Populate`, `Goad`, `RollDie`, `VentureIntoDungeon`, …):

| Family | Sub-parser | Verb patterns |
|--------|-----------|---------------|
| `CostResource` | `parse_cost_resource_ast()` | add mana, pay life, deal damage |
| `ZoneCounter` | `parse_zone_counter_ast()` | destroy, exile, counter, put counter |
| `Structured(Numeric)` | `parse_numeric_imperative_ast()` | draw, gain life, lose life, pump, scry, surveil, mill |
| `Structured(Targeted)` | `parse_targeted_action_ast()` | tap, untap, sacrifice, discard, return, fight, gain control |
| `Structured(SearchCreation)` | `parse_search_and_creation_ast()` | search library, dig, create token, copy token |
| `Structured(HandReveal)` | `parse_hand_reveal_ast()` | look at hand, reveal hand, reveal top |
| `Structured(Choose)` | `parse_choose_ast()` | target-only, named choice, reveal hand filter |
| `Structured(Utility)` | `parse_utility_imperative_ast()` | prevent, regenerate, copy, transform, attach |
| `Shuffle` | `parse_shuffle_ast()` | shuffle, shuffle into library |
| `Put` | `parse_put_ast()` | put into/on top of |
| `YouMay` | "you may" prefix | Wraps inner effect (generic optionals are peeled earlier by `clause_shell`) |

Approximate dispatch order in `parse_imperative_family_ast()`: keyword-grant
intercepts → CostResource → ZoneCounter → Numeric → Targeted → SearchCreation →
Utility → Shuffle → HandReveal → Choose → keyword-action leaves → Put → YouMay.
Read the function for the binding order before inserting a new family.

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
├── mod.rs                — Orchestrator: parse_effect_chain(), parse_effect_clause(), compound detection
├── conditions.rs         — Leading condition splitting, AbilityCondition extraction, condition bridges
├── imperative.rs         — Imperative verb family parsing: parse_*_ast() + lower_*_ast()
├── lower.rs              — Clause lowering helpers: strip_trailing_duration / strip_leading_duration
│                           (the live duration suffix/prefix tables), damage-player scopes
├── search.rs             — Search/seek parsing helpers: search filters, seek details, destinations
├── subject.rs            — Subject-predicate parsing: try_parse_subject_predicate_ast()
├── sequence.rs           — Clause boundary splitting and continuation absorption
├── token.rs              — Token creation: "create a 1/1 white Spirit token with flying"
├── animation.rs          — Animation/become: "becomes a 3/3 creature with flying"
├── become_copy_except.rs — Shared ", except <body>" clause for copy effects (CR 707.9 + CR 613.1a)
├── counter.rs            — Counter mechanics: put/remove/move/double counters
└── mana.rs               — Mana production and spend restrictions
```

AST type definitions (`ClauseAst`, `ImperativeFamilyAst`, `ParsedEffectClause`,
etc.) live in `oracle_ir/ast.rs` — the former `oracle_effect/types.rs` was
moved there as part of the IR layer.

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

| Module | Purpose | Invoked at Slot |
|--------|---------|-----------------|
| `oracle_classifier.rs` | Shared line-classification helpers: trigger prefixes (`has_trigger_prefix`), **`is_static_pattern()` (~line 398)**, **`is_replacement_pattern()`**, granted-static and vehicle-tier detection. Called by `oracle.rs`, `oracle_dispatch.rs`, and class parsing. | Gates for `5-6`, `7`, `8`, `13c` |
| `oracle_dispatch.rs` | Nom fallback dispatch for effect/static/replacement candidates before `Unimplemented`. | `14a` |
| `clause_shell.rs` | Structural-slot peeling (`peel_clause` / `ClauseContext`) — see §2. Destination for all new structural slot handling (optional, duration, leading condition today). | Inside `parse_effect_clause()` |
| `oracle_special.rs` | Router-adjacent helpers for solve conditions, Defiler two-line statics, die-roll tables, static self-ref normalization, and keyword-line parsing (Escape/Harmonize/Cumulative Upkeep). | `3b`, `6c-defiler`, die-roll slot |
| `oracle_trigger.rs` | Trigger parsing: subject + event decomposition, constraint parsing (OncePerTurn, OncePerGame). Uses `parse_trigger_subject()` → `try_parse_event()` pipeline. | `5-6` |
| `oracle_static/` | **Directory** — static ability parsing split into submodules (see below). Entry: `parse_static_line()` / `parse_static_line_multi()` in `mod.rs` / `shared.rs`, internally two-phase (`parse_static_line_ir` → `lower_static_ir`). | `7` |
| `oracle_replacement.rs` | Replacement effects: priority-ordered pattern matching (as-enters-choose before shock-land before fast-land, etc.), builder pattern with `ReplacementDefinition::new()`. | `8`, `5-pre` |
| `oracle_condition.rs` | Restriction conditions: source/control/graveyard/hand/event conditions for "Cast only if..." / "Activate only if..." patterns. | Used by `4` and casting-restriction lines |
| `oracle_cost.rs` | Ability cost parsing: mana costs, tap/sacrifice/discard costs, `parse_single_cost()` for individual cost components. | `4` |
| `oracle_keyword.rs` | Keyword extraction: comma-separated keyword lists, parameterized keywords (ward, kicker), keyword grants. | `0`, `1b`, `13` + keyword-cost slots |
| `oracle_casting.rs` | Casting options/restrictions: additional costs ("As an additional cost"), alternative costs, timing restrictions (flash, sorcery speed), `scan_timing_restrictions()`. | `8b (early)` + casting-restriction slot |
| `oracle_modal.rs` | Modal spell parsing: "Choose N" headers, bullet mode collection, `parse_oracle_block()` for block-level parsing. | `1` |
| `oracle_vote.rs` | Council's-dilemma / Will-of-the-Council vote blocks (CR 701.38): "each player votes for A or B" + per-vote effects. | Within `9` and trigger bodies |
| `oracle_separate_piles.rs` | Pile-separation shape (CR 700.3): "separates ... into two piles" three-sentence form. | Within `9` |
| `oracle_class.rs` | Class card parsing (level-gated abilities). | Special pre-parse |
| `oracle_level.rs` | Leveler card parsing (LEVEL N-M power/toughness ranges). | Special pre-parse |
| `oracle_saga.rs` | Saga chapter parsing (roman numeral → chapter effects). | Special pre-parse |
| `oracle_attraction.rs` | Attraction visit abilities and numbered visit lines (CR 717.5 + CR 702.159a). | Special pre-parse |
| `oracle_spacecraft.rs` | Spacecraft pipe-delimited threshold lines "N+ \| body" → charge-counter-gated statics/triggers/abilities (CR 721). | Special pre-parse |

### The `oracle_static/` Directory Split

The former single-file `oracle_static.rs` is now a directory. `mod.rs` owns the
public `parse_static_line()` (two-phase: `parse_static_line_ir` →
`lower_static_ir`), a shared `prelude`, and the re-export surface. Submodules:

| Sub-module | What lives here |
|-----------|----------------|
| `dispatch.rs` | `parse_static_line_inner` — the priority-ordered static-pattern dispatch |
| `shared.rs` | `parse_static_line_multi`, compound-line splitting, cross-submodule helpers |
| `anthem.rs` | P/T modification statics: "get +1/+1", dynamic/base P/T, "where X is" binding |
| `keyword_grant.rs` | Keyword/ability grants: `parse_continuous_modifications()`, quoted-ability grants, graveyard keyword grants |
| `evasion.rs` | Combat statics: can't-block/attack splits, block exceptions, must-attack |
| `restriction.rs` | Casting/activation prohibitions: `strip_casting_prohibition_subject()`, cast-and-activate-only-during |
| `cost_mod.rs` | Cost modification statics: alternative costs, cost payment prohibitions |
| `type_change.rs` | Type-changing statics: "is a", "becomes", additive type clauses |
| `cda.rs` | Characteristic-defining abilities |
| `grammar.rs` | Shared static-line grammar combinators |
| `static_helpers.rs` | Misc static construction helpers |
| `loyalty.rs` | Loyalty-related static helpers |
| `mana_transform.rs` | Mana-type transformation statics (retain-unspent-mana, etc.) |

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
| `oracle_nom/duration.rs` | Duration phrase combinators (`parse_duration`, `parse_optional_duration`, `parse_cast_snapshot_suffix`) | Parsing inline duration phrases in nom pipelines — but see the duration doctrine below for where NEW duration patterns go |
| `oracle_nom/condition.rs` | `parse_condition` (prefix + inner), `parse_inner_condition` (**single authority** for all game-state conditions) | Parsing "if/unless/as long as" — ALWAYS delegate here |
| `oracle_nom/filter.rs` | Zone filters, controller filters, property filters ("tapped", "attacking", "with flying", "with a +1/+1 counter") | Parsing object property constraints |
| `oracle_nom/error.rs` | `OracleError` / `OracleResult` type aliases, `oracle_err` (error constructor for hand-rolled combinators). For "parser couldn't handle this", use `Effect::unimplemented(name, fragment)` from `types/ability.rs` — the single authority; literal `Effect::Unimplemented { .. }` construction is gated for new code | Error handling at parser dispatch boundaries |
| `oracle_nom/bridge.rs` | `nom_on_lower` (run nom on lowercase, map consumed bytes back to original-case remainder), `nom_on_lower_required` (Result variant), `nom_parse_lower` (discard remainder) | Bridging mixed-case Oracle text to lowercase nom combinators |
| `oracle_nom/context.rs` | `ParseContext` (subject, quantity_ref, card_name, in_trigger, in_replacement) | Threading parse state across combinator boundaries |
| `oracle_util.rs` | `TextPair` (dual original/lowercase slices with `strip_prefix`/`strip_suffix`), `parse_number` wrapper, mana symbol parsing, `strip_reminder_text`, `normalize_card_name_refs`, possessive/pronoun matching (`contains_possessive`, `contains_object_pronoun`, `starts_with_possessive`), `match_phrase_variants`, `merge_or_filters`, `SELF_REF_TYPE_PHRASES`, `SELF_REF_PARSE_ONLY_PHRASES` | Case-bridging structural ops, shared string utilities, phrase matching |
| `oracle_target.rs` | `parse_target` (full target extraction), `parse_type_phrase` (type descriptions without "target"), `parse_player_reference`, `parse_event_context_ref`, `parse_zone_suffix` | High-level target/filter extraction from Oracle text |
| `oracle_quantity.rs` | **Frozen legacy fall-through** — `parse_quantity_ref` (semantic interpretation), `parse_cda_quantity` (CDAs), `parse_for_each_clause` ("for each [filter]") | Existing call sites only. Do NOT add new `QuantityRef` recognition here — it goes in `oracle_nom/quantity.rs` (see doctrine below) |

### Where New Grammar Goes — Single-Authority Doctrine

Each grammar axis has exactly one home for NEW pattern recognition. Adding a
pattern anywhere else creates a second authority that will drift:

- **Durations** — one grammar. Two duration tables exist today:
  `oracle_nom/duration.rs::parse_duration` (the nom combinator grammar) and
  `oracle_effect/lower.rs::strip_trailing_duration` (+ `strip_leading_duration`)
  — the suffix/prefix tables that the clause shell and the static/effect
  parsers actually run against, and the richer of the two ("for the rest of
  the game", "until ~ leaves the battlefield", mid-clause durations).
  Consolidation into `oracle_nom/duration.rs` is planned; **until that port
  happens, new duration patterns go in `strip_trailing_duration`'s table in
  `oracle_effect/lower.rs`** (and `strip_leading_duration` for leading forms),
  so the clause shell picks them up for free. Do not add a third recognizer.
- **`QuantityRef` recognition** — new dynamic-quantity phrases go in
  `oracle_nom/quantity.rs` (`parse_quantity_ref` combinator and friends),
  never in `oracle_quantity.rs`, which is frozen legacy fall-through.
- **Type phrases / targets** — new type-phrase and target work composes the
  `oracle_nom/target.rs` combinators. `oracle_target.rs` remains the
  high-level extraction surface, but its building blocks are the nom
  combinators — extend those, not bespoke string logic.
- **Conditions** — `parse_inner_condition` in `oracle_nom/condition.rs` is the
  single condition recognizer (output type: `StaticCondition`). Its output is
  adapted into other condition layers by three bridges that MUST be kept
  exhaustive when `StaticCondition` gains variants:
  `static_condition_to_trigger_condition` (`oracle_trigger.rs`),
  `static_condition_to_ability_condition` (`oracle_effect/conditions.rs`), and
  `static_condition_to_restriction_condition` (`oracle_condition.rs`). A new
  `StaticCondition` variant that is silently unconvertible in a bridge is a
  swallow bug waiting to surface.
- **Structural slots** (optional, duration, leading condition, future siblings)
  — `clause_shell.rs` (see §2). New slot types migrate into the shell.

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
- Structural slot (optional / duration / leading condition) → `clause_shell.rs` (see §2)
- Duration phrase → `strip_trailing_duration` table in `oracle_effect/lower.rs` (see §7 doctrine)
- Dynamic quantity → `oracle_nom/quantity.rs` (never `oracle_quantity.rs`)
- Trigger → `oracle_trigger.rs`
- Static → `oracle_static/` (dispatch in `dispatch.rs`, category submodules per §6)
- Replacement → `oracle_replacement.rs`
- Routing gate → `is_static_pattern()` / `is_replacement_pattern()` in `oracle_classifier.rs` (~line 398)

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
- [ ] Update `is_static_pattern()` or `is_replacement_pattern()` in `oracle_classifier.rs` if text is routed to the wrong parser

**Phase 6 — Tests & Verification**
- [ ] Parser unit tests for each new pattern
- [ ] Snapshot tests: `oracle_ir/snapshot_tests.rs` (IR + lowered parity, insta), plus per-module `snapshot_tests.rs` in `oracle_static/`
- [ ] `cargo coverage` — Unimplemented count should decrease
- [ ] Verify per CLAUDE.md § "Canonical verification pattern" — `cargo fmt --all`, then if `tilt get uiresource clippy >/dev/null 2>&1`: `./scripts/tilt-wait.sh --timeout 240 clippy test-engine card-data`; else: `cargo clippy --all-targets -- -D warnings` + `cargo test -p engine` + `./scripts/gen-card-data.sh`.

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

# Top clustered warning patterns by likely shared fix.
cargo run -p engine --bin coverage-report -- data --brief \
  --write-warning-patterns /tmp/parser-warning-patterns.json >/tmp/coverage.json
jq -r '
  [.[] | select(.category=="swallowed-clause")]
  | sort_by(-.otherwise_supported_cards, -.card_count)
  | .[0:25][]
  | "\(.otherwise_supported_cards) otherwise / \(.card_count) cards / \(.single_gap_cards) single | \(.pattern) | \(.example_cards|join(", "))"
' /tmp/parser-warning-patterns.json

# Drill down into one exact warning pattern. This uses the same clustering
# function as parser-warning-patterns.json and includes support status,
# gap count, warning text, parsed labels, and gap details.
cargo run -p engine --bin coverage-report -- data \
  --warning-category swallowed-clause \
  --warning-pattern 'Replacement_Instead: instead' \
  --warning-limit 20 >/tmp/warning-drilldown.json

# Drill down into a broader detector family when exact-pattern slices are too narrow.
cargo run -p engine --bin coverage-report -- data \
  --warning-detector Replacement_Instead \
  --warning-limit 20 >/tmp/warning-drilldown.json

# Include the full parse_details tree and exported CardFace JSON when needed.
cargo run -p engine --bin coverage-report -- data \
  --warning-detector DynamicQty \
  --warning-full \
  --warning-limit 5 >/tmp/warning-drilldown-full.json
```

**Detector class prefixes** (one row per detector in `swallow_check.rs`):

| Prefix | What it flags |
|---|---|
| `Condition_If` | "if <condition>" present in text but no `condition`/`constraint`/`if_clause` slot in AST |
| `Condition_Unless` | "unless …" not bound to `unless_filter` / `unless_*` slot |
| `Condition_AsLongAs` | "as long as …" not bound to a conditional static |
| `DynamicQty` | "for each / equal to / the number of / twice / half" present but AST has only `Fixed` quantity values — the canonical **count parsed but routed downstream as Fixed** bug class |
| `Duration_ThisTurn` / `_UntilEndOfTurn` / `_NextTurn` | duration phrase present but no `duration` slot populated |
| `Optional_YouMay` / `_MayHave` | "you may …" / "may have it …" not bound to the optional flag |
| `Replacement_Instead` | " instead" present but no replacement definition emitted or the detector has a false positive because the AST represented the replacement through another supported structure |
| `ActivateOnlyDuring` / `ActivateLimit` | activation timing/limit phrase not bound to a restriction slot |
| `APNAP` | "starting with you" / "in turn order" not bound to order metadata |
| `target-fallback:` | secondary class — `parse_target` couldn't classify a noun phrase, or a downstream chomping loop encountered an unmatched filter suffix |

Current `card-data.json` stores parse warnings as structured diagnostics, not legacy strings:

```json
{ "type": "SwallowedClause", "detector": "Replacement_Instead", "description": "...", "line_index": 0 }
```

Use `--warning-detector <detector>` for broad-family triage and `--warning-pattern '<detector>: <normalized excerpt>'` for exact shared-fix slices. A high `supported_cards` count in the drilldown means the warning is likely detector noise or an already-parsed semantic that `swallow_check.rs` does not recognize yet; inspect `parsed_labels` before adding parser behavior.

**Workflow:**
1. Start with `parser-warning-patterns.json` sorted by `otherwise_supported_cards`; this finds the largest likely false-positive or minor-chomp groups.
2. Run `coverage-report --warning-pattern ...` or `--warning-detector ...` and inspect `supported`, `gap_count`, `parsed_labels`, and `gap_details` before editing parser code.
3. Classify the pattern: detector false positive, parsed primary effect with missing modifier, or real parser gap.
4. When fixing a real swallow, identify the dispatch site that *recognized* the marker but failed to either capture or chomp it. The fix is almost always at one of two places: the upstream recognition (route through the right `try_parse_*` interceptor) or the downstream chomping loop (add a missing arm). The peek-vs-chomp pitfall in §10 is the recurring root cause.
5. After fixing, regenerate (`./scripts/gen-card-data.sh`) and rerun the same drilldown; warnings should drop by exactly the affected class size unless other detectors were un-muted.
6. **Suppression rule** — `swallow_check.rs` may skip detectors when a card already has stronger parser failures; fixing one issue can un-mute additional detector warnings on the same cards.

**Companion Python audit:** `scripts/swallow_audit.py` runs the same heuristics over `coverage-data.json` independently of the Rust runtime. Use it for cross-checking, or for exploring novel detector classes before promoting them to `swallow_check.rs`.

---

## 12. Self-Maintenance

After completing work using this skill:

1. **Verify references** by running `./scripts/check-skill-doc.sh`
2. **Update the priority table** (§3) if slots were added/removed/renamed in `parse_oracle_ir` — the gate compares the table against the `// Priority <label>:` comments
3. **Update the AST family tables** (§4b) if new families or continuations were added
4. **Update the deep dive** (§5) if new sub-modules were added to `oracle_effect/`
5. **Update the module catalog** (§6) if new `oracle_*.rs` modules or `oracle_static/` submodules were added

### Verification Gate — `scripts/check-skill-doc.sh`

The verification script lives at `scripts/check-skill-doc.sh` and **runs in CI**
(rust-lint job, alongside the parser combinator gate), so this document cannot
silently drift from the source tree. It asserts three invariant families:

1. **Paths** — every parser file/directory documented here exists.
2. **Anchor symbols** — the load-bearing functions named in this document
   (`parse_oracle_ir`, `lower_oracle_ir`, `peel_clause`, `parse_inner_condition`,
   the three condition bridges, `strip_trailing_duration`, …) still live in the
   documented files.
3. **Priority table sync** — the §3 table's labeled-row count equals the number
   of `// Priority <label>:` slot comments in `oracle.rs`, and every label that
   appears in code appears in the table. Cosmetic doc edits don't trip it;
   adding, removing, or renaming a slot without updating §3 does.

```bash
./scripts/check-skill-doc.sh   # exit 0 = doc in sync; non-zero lists drift
```

When the gate fails: update the relevant section here (not the script's
expectations) unless the code change itself was wrong. When documenting a new
slot, give it a `| \`label\` |` row in §3; unlabeled interleaved handlers use
`| — |` rows, which the gate ignores.
