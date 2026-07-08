# Implementation Plan — Teamwork conditional-rider parser fix (revised)

**Task #8.** MSC "Teamwork" spells drop their `if … cast using teamwork` conditional in three
shapes the parser does not yet handle:

1. **Trailing rider** — `<effect> if this spell was cast using teamwork` (Beast Mode), plus the
   inverted `<effect> instead if this spell was cast using teamwork` (We Say Thee Nay!). Plus the
   clause-leading **"Also "** filler conjunction (Beast Mode only).
2. **Conditional Dig-selection alternative** — `If this spell was cast using teamwork, put any
   number of creature cards from among them onto the battlefield instead` (**Earth's Mightiest
   Heroes**). The teamwork-gated alternative reuses the preceding Dig's top-N source but swaps the
   selection parameters. **This is the blocking gap pulled into scope by this revision.**
3. **Conditional flash self-grant** — `You may cast this spell as though it had flash if it's cast
   using teamwork` (Quantum Reduction), present-tense `it's`.

This plan fixes shapes (1) and (2) fully (both are existing building-block analogs and harden the
whole class), and makes a **bounded, justified deferral** of shape (3)'s flash permission while
keeping it honest (coverage stays red; no over-permissive grant).

Applicable skill: **`/oracle-parser`** (parser-only, AST shape change). No new engine effect, no
new enum variant on the implemented path, **no new runtime** (the `AbilityCondition` evaluator,
the `else_ability` fallback, and Teamwork payment recording all already exist). Inert until
card-data regen+redeploy but provable via `from_oracle_text` + cast-pipeline tests.

---

## ⚠️ Root-cause correction (revision delta — read this first)

The prior revision brief hypothesized that EMH was misparsed by the
`AdditionalCostPaidInstead` **fold** (`conditions.rs` `strip_additional_cost_conditional`,
~:585-606) "collapsing base and instead when they are the same effect kind differing only by a
parameter." **Code+AST tracing disproves that hypothesis.** EMH never reaches that fold:

- EMH's ground-truth AST (`client/public/card-data.json`, key `earth's mightiest heroes`) is a
  **single** `Effect::Dig { count: 8, keep_count: 4294967295 (u32::MAX), up_to: true, filter:
  Creature, destination: Battlefield, rest_destination: Graveyard, reveal: true }` with
  `condition: null`, **plus** a `SwallowedClause { detector: "Condition_If" }` parse warning.
- A run through the `AdditionalCostPaidInstead` fold would have produced an
  `AbilityCondition::AdditionalCostPaidInstead` on a sub-ability (exactly what **Cruel Alliance**
  shows — verified below). EMH has **no such condition** and a swallow warning — proof the fold
  did not fire.

**Actual root cause (verified by tracing).** EMH's three sentences lower as:
1. `Reveal the top eight cards of your library.` → `Effect::Dig { count: 8, keep_count: None }`
   (`imperative.rs:2039` → `lower_search_and_creation_ast` `imperative.rs:2311-2325`).
2. `You may put a creature card from among them onto the battlefield.` → a
   `ContinuationAst::DigFromAmong` that **patches** the preceding Dig (`sequence.rs:2519-2611`),
   setting `keep_count = Some(1)`.
3. `If this spell was cast using teamwork, put any number of creature cards from among them onto
   the battlefield instead.` → **should** route to the conditional Dig-alternative path
   (`try_parse_dig_instead_alternative`, `conditions.rs:2495`, dispatched at `mod.rs:18156`), which
   builds a teamwork-gated alternative Dig (`keep_count = u32::MAX`) wrapped as
   `SpecialClause::DigInsteadAlt` with the base Dig stashed in `else_ability` (lowered at
   `lower.rs:361-372`). **But** `try_parse_dig_instead_alternative` builds its condition from a
   four-arm chain (`conditions.rs:2586-2589`) — `parse_additional_cost_instead_condition_fragment`
   (kicked/bargained/beheld/additional-cost/evidence/gift) → `try_nom_condition_as_ability_condition`
   → `parse_condition_text` → `parse_control_count_as_ability_condition` — **none of which
   recognize "this spell was cast using teamwork."** The `?` on that chain returns `None`, so the
   DigInsteadAlt path bails and sentence 3 falls through to the unconditional `DigFromAmong` patch,
   which overwrites `keep_count = u32::MAX` **with no condition**. The leading "if …" is then
   flagged as a swallowed clause.

So EMH is a **same class of bug** as the trailing-rider cards (Beast Mode / We Say Thee Nay!): the
teamwork phrase is not recognized by the condition recognizer that the relevant instead/rider path
consults. The unifying fix is to make the shared `parse_cast_using_teamwork_condition_text`
recognizer (introduced by the trailing-rider work) available at the **Dig-instead-alternative**
recognition site too — one `.or_else` arm. **The `AdditionalCostPaidInstead` fold is not touched.**

---

## Evidence gathered (all verified against the codebase)

### Class size — verified from `client/public/card-data.json`
**17 teamwork cards.** Distribution of the `cast using teamwork` clause:

| Shape | Cards | Status |
|---|---|---|
| **Leading** `If this spell was cast using teamwork, <body>` (non-instead) | Crossover Collaboration, Heroic Teamwork, Repulsor Blast, Team Tactics | ✅ already handled — `strip_additional_cost_conditional` leading peel (conditions.rs:467-482) |
| **Leading + instead** (non-Dig effect kinds) | Cruel Alliance (ChangeZone → `AdditionalCostPaidInstead` sub-ability ✔ verified), Helicarrier Strike (DealDamage → `AdditionalCostPaid{Teamwork}` sub-ability ✔ verified), Too Evil to Stay Dead (Return) | ✅ already handled — leading peel + sibling/`AdditionalCostPaidInstead` |
| **Modal** `… choose both instead` | Atlantis Attacks, Go Nuts!, HULK SMASH!, Murdock's Crusade, Widow's Bite | ✅ already handled — `parse_modal_additional_cost_condition` (oracle_modal.rs:521-546) |
| **Conditional Dig-selection alternative** `If teamwork, put any number from among them … instead` | **Earth's Mightiest Heroes** | ❌ **GAP — this fix (shape 2)** |
| **Trailing rider** `<effect> if teamwork` / `<effect> instead if teamwork` | **Beast Mode**, **We Say Thee Nay!** | ❌ **GAP — this fix (shape 1)** |
| **Trailing `unless`** `Then discard a card unless this spell was cast using teamwork` | Timeline Inquiry | ⚠️ separate negated-unless shape (see Out-of-scope) |
| **Conditional flash self-grant** (present tense `it's`) | **Quantum Reduction** | ⚠️ **larger gap — deferred honest** |

**Correction vs prior revision:** EMH was previously listed under "Leading + instead — already
handled." It is **not** handled; it is the only Dig-shaped teamwork card and is the blocking gap.
The other three "Leading + instead" cards are non-Dig and verified handled (ASTs below).

### Verified ASTs (instead-class cards)
- **Cruel Alliance** (`ChangeZone Exile`): base + `sub_ability { effect: ChangeZone Exile (no
  cmc filter), sub_ability: GainLife 3, condition: AdditionalCostPaidInstead, sub_link:
  SequentialSibling }`. ✅ handled (and **does not** touch `try_parse_dig_instead_alternative`).
- **Helicarrier Strike** (`DealDamage 2`): base + `sub_ability { effect: DealDamage 4 to
  ParentTarget, condition: AdditionalCostPaid{origin: Teamwork}, sub_link: SequentialSibling }`.
  ✅ handled via the leading peel (`conditions.rs:467-482`, which already recognizes teamwork for
  the **leading** form), routed before `try_parse_generic_instead_clause` returns `None`.
- **We Say Thee Nay!** (`Counter StackSpell unless pays {2}`): base + `sub_ability { effect:
  Counter ParentTarget, unless_pay {4}, condition: **null** ❌, sub_link: SequentialSibling }`.
  Oracle: `… Counter that spell unless its controller pays {4} instead if this spell was cast
  using teamwork.` The `{4}` tax fires on **every** cast. The fix must make the existing
  sub-ability's condition `AdditionalCostPaid{Teamwork}` (see Verification Matrix).
- **Earth's Mightiest Heroes**: single `Dig { keep_count: u32::MAX, up_to: true, condition: null }`
  + `SwallowedClause{Condition_If}`. ❌ — the teamwork-gated "any number instead" applies
  unconditionally (free upgrade). Root cause above.

### Dispatch order (verified — `crates/engine/src/parser/oracle_effect/mod.rs`, chunk loop)
1. `try_parse_dig_instead_alternative` → `SpecialClause::DigInsteadAlt` — **mod.rs:18156** (EMH).
2. `try_parse_generic_instead_clause` (`build_instead_def`, forward + inverted) — **mod.rs:18306**.
3. `strip_additional_cost_conditional` leading peel — **mod.rs:18338** (Helicarrier leading form).
4. `strip_suffix_conditional` trailing rider — **mod.rs:18468** (Beast Mode + We Say Thee Nay!).

This order is what makes both fixes correctly scoped:
- **EMH** is captured at (1), **before** the leading peel at (3) can mis-route sentence 3 into the
  unconditional `DigFromAmong` patch.
- **We Say Thee Nay!** (inverted `… instead if teamwork`) naturally **defers** at (2) —
  `build_instead_def`'s condition chain (conditions.rs:2462-2464) does not recognize teamwork, so
  it returns `None` — does not match the leading peel at (3) (no leading `if … teamwork,`), and is
  peeled at (4) by `strip_suffix_conditional` with the new teamwork recognizer, attaching the
  condition to the residual `Counter … pays {4} instead` sibling. **No change to `build_instead_def`
  is required or made.**

### Building blocks confirmed present (reuse — do NOT reinvent)
- `AbilityCondition::additional_cost_paid_origin(AdditionalCostOrigin::Teamwork)` — builder at
  **ability.rs:13337**; `AdditionalCostOrigin::Teamwork` at **ability.rs:6488**. Single authority
  for "Teamwork additional cost was paid."
- Runtime eval — **effects/mod.rs:6387** (`AdditionalCostPaid { origin: Some(Teamwork), … }` →
  `instance_payment_count(Teamwork) >= 1`), recorded at **casting_costs.rs:4556**. Recognized as a
  self-referential cost condition at **effects/mod.rs:1854**.
- Conditional-def `else_ability` fallback — evaluated at **effects/mod.rs:1650 / 2878 / 3667 /
  4977**. A top-level def with `condition: Some(..)` + `else_ability: Some(base)` runs the base
  when the condition is false. **This is exactly the `DigInsteadAlt` lowering** (`lower.rs:361-372`:
  `new_def = alt_def; new_def.else_ability = Some(base_dig)`). The combination (DigInsteadAlt +
  `AdditionalCostPaid{Teamwork}`) reuses both already-proven paths — **zero new runtime.**
- Existing `DigInsteadAlt` analog — **Follow the Lumarets** ("If you gained life this turn, you may
  instead reveal two …") proves the conditional-Dig-alternative + `else_ability` shape end-to-end.
- Trailing-condition host — `strip_suffix_conditional` (conditions.rs:1831), dispatched at
  **mod.rs:18468**; trailing kicker recognizer `parse_was_kicked_condition_text` (conditions.rs:1944)
  is the exact analog for the new trailing-teamwork recognizer.
- Leading sequence-connector strip — `strip_leading_sequence_connector` (lower.rs:5513).
- Modal teamwork phrase set — oracle_modal.rs:529-546.

---

## Architectural sections

### Pattern Coverage
- **Shared teamwork-phrase combinator** dedups the phrase set across the modal, leading, trailing,
  and Dig-instead recognizers (one nom combinator, three+ callers); adds the present-tense `it's`
  variant uniformly.
- **Trailing teamwork rider** (`strip_suffix_conditional`): fixes **Beast Mode** and **We Say Thee
  Nay!**, and is the trailing analog of the existing trailing-kicker recognizer — covering the
  whole `<effect> if/instead-if [spell] was {kicked|cast using teamwork}` trailing-rider class.
- **Conditional Dig-selection alternative** (`try_parse_dig_instead_alternative`): adding the
  teamwork condition arm fixes **EMH** and generalizes the existing DigInsteadAlt recognizer to the
  teamwork additional-cost gate (same class as Follow the Lumarets, but gated on a cost-paid flag).
- **"Also " leading connector**: covers every Oracle clause opening with the additive conjunction
  "Also" (CR 608.2c) — a general filler class.
- Class size: 17 teamwork cards; this fix closes the 3 remaining genuinely-broken parses (Beast
  Mode, We Say Thee Nay!, EMH) and defers 1 (Quantum Reduction) + 1 separate shape (Timeline
  Inquiry, `unless`).

### Building Blocks
- Reuse `AbilityCondition::additional_cost_paid_origin(Teamwork)` (ability.rs:13337) — no new
  condition.
- Reuse `strip_suffix_conditional` host (mirror `parse_was_kicked_condition_text`).
- Reuse `try_parse_dig_instead_alternative` + `DigInsteadAlt` + `else_ability` (no new path).
- Reuse `strip_leading_sequence_connector` for the connector strip.
- **New shared combinator** `parse_cast_using_teamwork_phrase` (oracle_nom/condition.rs) +
  **new helper** `parse_cast_using_teamwork_condition_text` (conditions.rs) — justified: the phrase
  set is currently duplicated (modal inline; leading peel uses a `split_once_on` string). Factoring
  it into one nom combinator is the DRY home all four recognizers call. The text helper returns
  `Option<AbilityCondition> = Some(additional_cost_paid_origin(Teamwork))`; each caller wraps/uses
  it in its own slot (modal `ModalSelectionCondition`, trailing/Dig `AbilityCondition`) — layers
  stay separate.

### Logic Placement
- Trailing-rider recognition → **parser** (`conditions.rs`, `strip_suffix_conditional`).
- Dig-instead condition arm → **parser** (`conditions.rs`, `try_parse_dig_instead_alternative`).
- "Also " connector → **parser** (`lower.rs`, `strip_leading_sequence_connector`).
- Shared phrase combinator → **parser** (`oracle_nom/condition.rs`).
- All produce typed `AbilityCondition`; **no runtime change** (evaluator + payment recording +
  `else_ability` fallback already exist).
- Flash gate (Quantum Reduction) → deferred; only honest action is to ensure no over-permissive
  grant (already returns `None`; add a guard test).

### Rust Idioms
- nom combinators only: `alt()` + `tag()` per axis, composed sequentially; `all_consuming` for the
  trailing/Dig text recognizers; `value()`. No `find`/`split`/`contains`/`starts_with` for dispatch.
- Typed `AdditionalCostOrigin::Teamwork` (no bool, no teamwork-specific sibling condition —
  parameterized through the existing `origin` axis).
- Exhaustive matching preserved; the Dig-instead fix is a single `.or_else` arm (no match changes).

### Nom Compliance (mandatory — parser files change)
- `parse_cast_using_teamwork_phrase(input) -> OracleResult<'_, ()>`:
  ```text
  preceded(
    alt(( tag("this spell was "), tag("it was "), tag("it's ") )),   // subject + tense axis
    tag("cast using teamwork"),
  )
  ```
  Two axes, two `alt`/`tag` calls chained — no permutation enumeration. Input is already-lowercased
  (all callers pass lower text).
- `parse_cast_using_teamwork_condition_text(text) -> Option<AbilityCondition>` mirrors
  `parse_was_kicked_condition_text` (conditions.rs:1944): `nom_parse_lower(&lower, |i|
  all_consuming(parse_cast_using_teamwork_condition).parse(i))`, where
  `parse_cast_using_teamwork_condition` calls the shared phrase combinator and returns
  `AbilityCondition::additional_cost_paid_origin(AdditionalCostOrigin::Teamwork)`.
- Trailing recognizer: wire `parse_cast_using_teamwork_condition_text` into `strip_suffix_conditional`
  next to the kicker call.
- **Dig-instead arm**: append `.or_else(|| parse_cast_using_teamwork_condition_text(cond_text))` to
  the condition chain in `try_parse_dig_instead_alternative` (conditions.rs:2586-2589), placed
  **last** (after `parse_control_count_as_ability_condition`) so it only fires when no other arm
  matches — the teamwork phrase is disjoint from all four existing arms, so ordering is purely
  defensive.
- Modal path: replace the inline `alt((tag("this spell was cast using teamwork"), tag("it was cast
  using teamwork")))` (oracle_modal.rs:529) with a call to the shared combinator (adds `it's` free);
  keep the `ModalSelectionCondition::AdditionalCostPaid { origin: Some(Teamwork), … }` wrapping.
- "Also " strip: add `tag("Also, ")`, `tag("Also ")`, `tag("also, ")`, `tag("also ")` as sibling
  `alt` arms in `strip_leading_sequence_connector`.

### Extension vs Creation
Extends four existing patterns (trailing-condition recognizer, leading-connector strip, modal
teamwork recognizer, conditional Dig-instead recognizer) and adds one shared combinator that
*removes* duplication. No new pattern, no new effect, no new AST variant on the implemented path,
no new runtime.

### Analogous Trace
1. **Trailing-kicker rider** (exact analog for shape 1): `mod.rs:18468`
   (`strip_suffix_conditional`) → `conditions.rs:1871/1944` (`parse_was_kicked_condition_text` →
   `parse_was_kicked_condition` → `additional_cost_paid_any()`) → `ability.rs:13322` (builder) →
   `effects/mod.rs:6387` (`AdditionalCostPaid` eval) → `casting_costs.rs:4556` (payment recording).
   The teamwork rider rides the identical path, swapping `additional_cost_paid_any()` for
   `additional_cost_paid_origin(Teamwork)`.
2. **Follow the Lumarets / conditional Dig-instead** (exact analog for shape 2): `mod.rs:18156`
   (`try_parse_dig_instead_alternative`) → `conditions.rs:2495` (gate on previous Dig; split
   conditional; `parse_dig_from_among` body; condition chain) → `SpecialClause::DigInsteadAlt`
   (`mod.rs:18181`) → `lower.rs:361-372` (`new_def.else_ability = Some(base_dig)`) →
   `effects/mod.rs:1650/6387` (condition eval + `else_ability` fallback). EMH rides this path
   unchanged except the new teamwork condition arm.

### Variant Discoverability
- **Implemented path:** no new enum variant. `AbilityCondition::AdditionalCostPaid`,
  `AdditionalCostOrigin::Teamwork`, and `SpecialClause::DigInsteadAlt` all already exist (verified
  ability.rs:6488/13337; conditions.rs:2495; lower.rs:361). `/add-engine-variant` gate **not
  triggered**.
- **Deferred path (Quantum Reduction flash):** representing the predicate in `ParsedCondition`
  *would* require a new variant — **explicitly out of scope**, so the gate is intentionally not run.

### Verification Matrix

| Claim | Changed seam | Production entry | Test (add) | Revert-fail assertion | Negative / sibling |
|---|---|---|---|---|---|
| Beast Mode sentence-2 body parses to the real effect | `strip_leading_sequence_connector` (lower.rs) + `strip_suffix_conditional` (conditions.rs) | `from_oracle_text` | `beast_mode_teamwork_counter_rider`: a clause carries `Effect::AddCounter{+1/+1, target=that-creature}` with `condition == AdditionalCostPaid{origin:Some(Teamwork)}`, and **no** `Effect::Unimplemented` in the chain | Without "Also " strip → `Unimplemented{name:"also"}`; without trailing recognizer → `condition == None` | sentence-1 `+2/+2 & trample` clause unchanged |
| Trailing teamwork condition extracted & typed | `strip_suffix_conditional` (conditions.rs) | `from_oracle_text` | `strip_suffix_conditional_extracts_teamwork_gate` (unit): `"Draw a card if this spell was cast using teamwork"` → `(Some(AdditionalCostPaid{origin:Teamwork}), "Draw a card")`; assert `it was` and `it's` variants also parse | Returns `(None, original)` | non-teamwork trailing rider (kicker) unaffected |
| **We Say Thee Nay!**: `{4}` counter-unless is **gated**, not unconditional | `strip_suffix_conditional` (conditions.rs) — inverted `… instead if teamwork` defers from `build_instead_def` (mod.rs:18306 → None) to the trailing rider (mod.rs:18468) | `from_oracle_text` | `we_say_thee_nay_counter_tax_gated`: the `Counter ParentTarget` **sub-ability** (the existing `unless_pay {4}` sibling) **gains** `condition` referencing `AdditionalCostPaid{origin:Some(Teamwork)}` (bare or `ConditionInstead`-wrapped); assert it is **not null** | With null condition → the `{4}` tax fires on every cast (over-strict) | base `Counter … unless pays {2}` clause unchanged (`condition == None`) |
| **EMH**: teamwork-gated "any number" alternative is conditional | `try_parse_dig_instead_alternative` (conditions.rs:2586-2589, new `.or_else` arm) | `from_oracle_text` | `emh_dig_any_number_gated_on_teamwork`: top-level `Effect::Dig{keep_count:Some(u32::MAX), up_to:true}` with `condition == AdditionalCostPaid{origin:Some(Teamwork)}` **and** `else_ability == Some(Dig{keep_count:Some(1)})`; `rest_destination == Some(Graveyard)` on **both** branches; **no** `SwallowedClause{Condition_If}` warning | Revert the arm → single `Dig{keep_count:u32::MAX, condition:null}` + swallow warning (the live free-upgrade bug) | Cruel Alliance (`AdditionalCostPaidInstead` sub-ability) + Helicarrier (`AdditionalCostPaid{Teamwork}` sub-ability) + a kicker-instead card AST **unchanged** |
| EMH applies correct count iff teamwork paid | resolution (unchanged eval + `else_ability` fallback) | cast-pipeline `GameRunner::cast(...).resolve()` | `emh_any_number_with_teamwork` / `…_one_without`: cast EMH WITH teamwork tap → may put any number of revealed creatures to battlefield; WITHOUT → may put exactly one | Without the gate → any-number unconditionally (free upgrade) | rest of revealed cards go to graveyard in both branches |
| Modal path still recognizes teamwork (+ new `it's`) | `parse_modal_additional_cost_condition` (oracle_modal.rs) | `from_oracle_text` | extend existing modal teamwork test to include `it's` | shared-combinator regression breaks modal + trailing + Dig tests | non-teamwork modal (kicker) unaffected |
| Quantum Reduction emits **no** unconditional flash grant | `parse_self_flash_option` (oracle_casting.rs) | `from_oracle_text` | `quantum_reduction_flash_not_unconditional`: parse contains **no** instant-speed / `CastFromZone`-flash permission with `condition: None` | A bad parse would expose a null-condition flash/CastFromZone | enchant + `-5/-0 lose abilities` body parses normally |

**Coverage-honesty statement.** The trailing-rider and Dig-instead fixes make Beast Mode, We Say
Thee Nay!, and EMH *more* covered with fully-typed, runtime-evaluable ASTs — no Oracle text is
accepted with deferred semantics. **Quantum Reduction stays RED:** its flash permission is not
representable in `ParsedCondition`, so `parse_self_flash_option` returns `None` for the teamwork
predicate (the existing honest-refusal path, oracle_casting.rs:244-255); the SwallowedClause /
`Condition_If` detector continues to flag it. We add a guard test asserting **no** unconditional
flash grant leaks. We do **not** ship a green-but-wrong flash grant.

**mtgish-safety statement.** Zero edits to `mtgish/`, `crates/mtgish-import/`, or `data/mtgish-*`.
No new `Effect`, `AbilityCondition`, `AdditionalCostOrigin`, or `SpecialClause` variant (all
already exist), so no mtgish mirroring is implicated. Changes are confined to parser files
(`conditions.rs`, `lower.rs`, `oracle_modal.rs`, `oracle_nom/condition.rs`) plus tests.

### Blast-radius bounding (mandatory — EMH + We Say Thee Nay! fixes)
**EMH fix (teamwork arm in `try_parse_dig_instead_alternative`):**
- Cards flowing through `try_parse_dig_instead_alternative`: any clause that (a) follows an
  `Effect::Dig`, (b) parses as a `DigFromAmong` body with a prefix/suffix "instead", and (c) has a
  recognized leading condition. Existing members: **Follow the Lumarets** ("if you gained life this
  turn") and any kicker-Dig-instead. EMH is the **only** teamwork member.
- The new arm is appended **last** in the four-arm condition chain and returns `Some` **only** for
  the exact teamwork phrase. Follow the Lumarets / kicker conditions match arms 1-3 first → reach
  the teamwork arm never → **unchanged**. Non-Dig instead cards (Cruel Alliance, Too Evil,
  Helicarrier) never enter this function → **unchanged**. Net behavior change: **EMH only.**

**We Say Thee Nay! fix (teamwork recognizer in `strip_suffix_conditional`):**
- The inverted `… instead if teamwork` form defers from `build_instead_def` (returns `None` on
  unrecognized teamwork — **no change to `build_instead_def`**), does not match the leading peel,
  and is peeled by the trailing rider. The trailing recognizer fires only on the exact teamwork
  phrase; kicker/bargain trailing riders are unaffected (separate `parse_was_kicked_condition_text`
  arm). Helicarrier (leading `If teamwork, … instead`) is matched at the leading peel (mod.rs:18338,
  which already recognizes teamwork) **before** the trailing rider, and `build_instead_def` is not
  modified — **Helicarrier unchanged** (regression test asserts its `AdditionalCostPaid{Teamwork}`
  sub-ability is intact).
- Regression tests (mandatory): assert **Cruel Alliance** (`AdditionalCostPaidInstead` sub-ability),
  **Too Evil to Stay Dead**, **Helicarrier Strike** (`AdditionalCostPaid{Teamwork}` sub-ability),
  and one representative **kicker-instead** card parse **identically** to current `card-data.json`.

### CR annotations (all grep-verified against `docs/MagicCompRules.txt`)
- **CR 601.2b** (line 2457) — announcing intent to pay alternative/additional costs as the spell is
  cast. ✔
- **CR 601.2f** (line 2466) — determining total cost including additional costs. ✔ → annotate the
  trailing-teamwork recognizer and the Dig-instead teamwork arm.
- **CR 608.2c** (line 2789) — later text modifies earlier text (the trailing conditional; the
  conditional Dig-selection alternative; the additive "Also" connector). ✔ → annotate the trailing
  recognizer, the Dig-instead arm, and the "Also " connector arm.
- **CR 702.8a** (Flash; previously verified) — reference in the Quantum Reduction deferral note.

No new CR number is required for the Dig-instead conditional: it is the same CR 601.2b/f + 608.2c
pairing already used by the leading/trailing teamwork recognizers and the existing DigInsteadAlt
comment (conditions.rs:2481-2494 cites CR 608.2c).

---

## Step-by-step implementation

### Step 1 — Shared teamwork-phrase combinator (DRY)
**File:** `crates/engine/src/parser/oracle_nom/condition.rs`
Add `pub(crate) fn parse_cast_using_teamwork_phrase(input: &str) -> OracleResult<'_, ()>`:
`preceded(alt((tag("this spell was "), tag("it was "), tag("it's "))), tag("cast using
teamwork"))`, `value((), …)`. CR annotation: `// CR 601.2b/f: subject+tense axes for the Teamwork
additional-cost-paid phrase, shared by the modal, leading, trailing, and Dig-instead recognizers.`
Input is already-lowercased.

### Step 2 — Trailing recognizer + Dig-instead arm (`conditions.rs`)
**File:** `crates/engine/src/parser/oracle_effect/conditions.rs`
- Add `fn parse_cast_using_teamwork_condition(input) -> OracleResult<'_, AbilityCondition>`:
  call `parse_cast_using_teamwork_phrase`, return
  `AbilityCondition::additional_cost_paid_origin(AdditionalCostOrigin::Teamwork)`.
- Add `pub(super) fn parse_cast_using_teamwork_condition_text(text) -> Option<AbilityCondition>`
  mirroring `parse_was_kicked_condition_text` (1944): `nom_parse_lower(&lower, |i|
  all_consuming(parse_cast_using_teamwork_condition).parse(i))`.
- Wire it into `strip_suffix_conditional` next to the kicker call (after ~1871/1873):
  `if let Some(cond) = parse_cast_using_teamwork_condition_text(condition_core) { return
  (Some(cond), effect_text); }`. Confirm `condition_text_is_rehomeable` does **not** exclude it
  (`NON_REHOMEABLE_CONDITION_PREFIXES`, ~1776, contains none of `this spell`/`it was`/`it's`).
- **Dig-instead arm:** in `try_parse_dig_instead_alternative` (2586-2589), append to the condition
  chain: `.or_else(|| parse_cast_using_teamwork_condition_text(cond_text))` **before** the `?`.
  CR annotation: `// CR 601.2b/f + CR 608.2c: a teamwork-gated "put … from among them … instead"
  alternative reuses the preceding Dig's source; the base selection runs from else_ability when
  Teamwork wasn't paid.`

### Step 3 — "Also " leading connector
**File:** `crates/engine/src/parser/oracle_effect/lower.rs` (`strip_leading_sequence_connector`,
~5513). Add `tag("Also, ")`, `tag("Also ")`, `tag("also, ")`, `tag("also ")` arms. CR annotation:
`// CR 608.2c: "Also" is an additive sequence connector at clause start (Beast Mode); strip like
"then"/"and".` Position-0 only; mid-sentence "also" (`strip_trailing_additive_adverb`) is unaffected.

### Step 4 — Refactor modal path onto the shared combinator
**File:** `crates/engine/src/parser/oracle_modal.rs` (529-546). Replace the inline
`alt((tag("this spell was cast using teamwork"), tag("it was cast using teamwork")))` with a call
to `parse_cast_using_teamwork_phrase`; keep the `ModalSelectionCondition::AdditionalCostPaid {
origin: Some(AdditionalCostOrigin::Teamwork), … }` return unchanged (adds `it's` for free).

### Step 5 — Quantum Reduction flash guard (honest, no grant)
**File:** `crates/engine/src/parser/oracle_casting.rs` — **no production change required**;
`parse_self_flash_option` already returns `None` for the unrecognized teamwork predicate
(`parse_restriction_condition(...)?`, ~252). Add only the **guard test**
(`quantum_reduction_flash_not_unconditional`) asserting no unconditional flash/`CastFromZone`
permission is emitted. If implementation reveals a fallback path that *does* emit a bad
`CastFromZone{condition:null}`, the fix is to make that path refuse (return the unparsed clause to
the swallow detector) — **not** emit a grant. Document the deferral inline (CR 702.8a / CR 601.2b)
pointing at the follow-up.

### Step 6 — Tests
Add the matrix tests. Use `GameScenario` + `GameRunner::cast(...).resolve()` for runtime tests
(per `card-test`); assert via `CastOutcome` deltas, not hand-built `TargetRef` vectors. Beast Mode
and EMH are inline-keyword cards (`Teamwork N`) — construct via `from_oracle_text_with_keywords` /
`add_real_card` + rehydrate so the Teamwork cost is wired and the data-path is exercised (per
`parser_fix_inert_until_data_regen`). For EMH's runtime test, the discriminating assertions are the
count cap (any-number vs one) keyed on the Teamwork tap, and graveyard placement of the rest in
both branches. Add the four blast-radius regression assertions (Cruel Alliance / Too Evil /
Helicarrier / kicker-instead unchanged).

### Step 7 — Verification cadence
`cargo fmt --all` (direct). Parser combinator gate on the touched parser files
(`check-parser-combinators.sh`; manually grep diffs for `.rfind(`/`.split(` per
`parser_gate_blind_spots`). Then Tilt: `clippy`, `test-engine`, `card-data` (via `tilt logs` /
`./scripts/tilt-wait.sh`, not direct cargo). No frontend changes.

---

## Out-of-scope (explicit, justified)
- **Quantum Reduction conditional flash grant** — deferred (CONFIRMED honest, not a
  `no_default_deferral` violation). The flash-permission slot is `ParsedCondition`, which has no
  additional-cost-paid variant, **and** the gate evaluates at *cast time*: a `ParsedCondition`
  predicate is checked **pre-announcement** (casting.rs:3441) where Teamwork `payment_count == 0`,
  while payment is recorded **post-announcement** (casting_costs.rs:2521). That structural
  cast-time/payment-time circular dependency makes a correct fix a multi-day cast-flow state-machine
  rework = genuinely MASSIVE. Deferred honestly: `parse_self_flash_option` returns `None` via `?`
  (oracle_casting.rs:252) — no over-permissive grant, no `CastFromZone{condition:null}` leak,
  coverage stays red. **Tracked follow-up.**
- **Timeline Inquiry** `Then discard a card unless this spell was cast using teamwork` — a *negated
  trailing-unless* shape (`Not(AdditionalCostPaid{Teamwork})`), routed through `strip_unless_*` /
  `try_nom_condition_as_unless`, not `strip_suffix_conditional`. Different host; out of scope. Small
  follow-up.
