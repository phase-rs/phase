# Cumulative Upkeep (CR 702.24) — Design

**Status:** Approved, ready for plan-writing
**Date:** 2026-05-20
**Owner:** engine team
**CR coverage:** 702.24a, 702.24b; supporting CR 118.5, CR 118.12, CR 400.7

## 1. Goal, Non-Goals, Success Criteria

### Goal

Implement CR 702.24 (Cumulative Upkeep) end-to-end. At the start of the
controller's upkeep, each cumulative upkeep ability on a permanent:

1. Puts an age counter on the permanent (CR 702.24a step 1).
2. Prompts its controller for `[cost] × N` where N is the resulting total
   age-counter count on that permanent (CR 702.24a step 2).
3. Sacrifices the permanent if the controller declines (CR 702.24a step 3).

### In scope

- All four documented cost shapes:
  - Mana: `Cumulative upkeep {1}` (Mystic Remora).
  - Pay-life: `Cumulative upkeep—Pay 2 life.` (Inner Sanctum).
  - Sacrifice: `Cumulative upkeep—Sacrifice a land.` (Polar Kraken).
  - Alternative: `Cumulative upkeep {G} or {W}` (Elephant Grass).
- Multi-instance correctness (CR 702.24b): each cumulative-upkeep trigger
  fires separately and reads the total counter pool at its own resolution
  time.
- Multiplayer (state filter is unchanged; per-player payment prompts route
  via the existing unless-payment infrastructure).
- AI: opaque payment heuristic (pay if affordable; otherwise let it die —
  the post-expansion concrete cost flows through the existing AI
  unless-pay decider with no new code).
- Frontend: display the cumulative upkeep prompt with the resolved N×cost.

### Out of scope

- Refactoring Echo / Vanishing / Fading. The user scoped this work to
  cumulative upkeep only.
- A general "X for each Y on Z" cost shape beyond what `PerCounter`
  exposes. We don't pre-build for hypothetical mechanics that don't exist
  in MTG today.
- Adding new cumulative-upkeep cards to the database; we ship the mechanic
  and the four cards already covered by the existing parser tests.
- AI evaluation tuning for cumulative-upkeep cards.

### Success criteria

- Mystic Remora played in a duel: at controller's upkeep on turn 2 an age
  counter is placed, controller is prompted to pay `{1}`; declining
  sacrifices it; paying keeps it. Turn 3 prompt is `{2}`. Turn 4 is `{3}`.
- A synthetic two-instance card (test-only) ticks two counters per upkeep
  and prompts each cost against the post-tick total, satisfying CR
  702.24b.
- AI duels remain stable (no panics), and the coverage report counts
  Mystic Remora, Inner Sanctum, Polar Kraken, and Elephant Grass as
  supported.

---

## 2. Type System Additions

Three changes to typed enums. The `CounterType` and `AbilityCost` changes
are additive. The `Keyword::CumulativeUpkeep` change is a refactor from a
raw `String` to a typed `AbilityCost`.

### 2a. `CounterType::Age` (additive)

```rust
// crates/engine/src/types/counter.rs
pub enum CounterType {
    // existing variants ...
    /// CR 702.24a: Age counters track Cumulative Upkeep duration. Each
    /// cumulative-upkeep trigger places one at the start of its
    /// controller's upkeep, and the cost is multiplied by the total
    /// age-counter count on the permanent at resolution time (CR 702.24b).
    Age,
}
```

- Serializes as `"age"` via `CounterType::as_str()`.
- Excluded from `power_toughness_delta()` — age counters do not modify P/T.

### 2b. `AbilityCost::PerCounter` (additive)

```rust
// crates/engine/src/types/ability.rs — in AbilityCost
/// CR 702.24a: A cost that multiplies a base cost by the number of
/// counters of `counter` type on `target`. The runtime resolves the
/// multiplier at the unless-payment entry point and expands `base` into
/// the effective payment: mana scales via `ManaCost::scaled(n)`,
/// life/sacrifice counts multiply directly, and `OneOf` unfolds into a
/// `Composite` of `n` independent disjunctive choices (each made
/// separately per CR 702.24a).
///
/// Building block, not a special case: this is the typed shape of "pay
/// [cost] for each counter on it". Cumulative upkeep is the only mechanic
/// using it today, but the variant is composable with every existing base
/// cost (Mana, PayLife, Sacrifice, OneOf, Composite).
PerCounter {
    counter: CounterType,
    target: TargetFilter,
    base: Box<AbilityCost>,
},
```

- `cost_categories()` delegates to `base.cost_categories()` — the
  multiplier doesn't change what kind of cost this is, only how much.
- Payability checks (`can_pay_*`) recurse on `base` with the
  runtime-resolved count; if N = 0, the expansion is trivially payable as
  `ManaCost::zero()` / no-op (CR 118.5).

### 2c. `Keyword::CumulativeUpkeep` retyped (refactor)

```rust
// crates/engine/src/types/keywords.rs
- CumulativeUpkeep(String),
+ /// CR 702.24a: cost paid per age counter on this permanent at the start
+ /// of the controller's upkeep, or sacrifice. The typed `AbilityCost`
+ /// lets the synthesis pipeline wire the cumulative-upkeep trigger
+ /// uniformly across mana / life / sacrifice / disjunctive cost shapes.
+ CumulativeUpkeep(AbilityCost),
```

- The four existing parser tests at `oracle.rs:8567–8649` update from
  string-equality to typed-cost matches.
- Round-trip serialization changes for this keyword (previously a raw
  string). No on-disk artifacts depend on the old form — only four
  in-repo tests reference it.
- `Keyword::CumulativeUpkeep(String::new())` placeholder constructions
  (`keywords.rs:1596`, `:1809`, `:2426`) become
  `Keyword::CumulativeUpkeep(AbilityCost::Mana { cost: ManaCost::zero() })`
  — a well-formed zero-cost sentinel for legacy-deserialization paths.

---

## 3. Parser

The keyword extractor at `parser/oracle_special.rs:368`
(`parse_cumulative_upkeep_keyword`) currently captures the cost as a raw
`String`. It is refactored to produce a typed `AbilityCost` using
existing parser building blocks.

### 3a. Refactored extractor

```rust
// CR 702.24a: Parse "Cumulative upkeep—[cost]" or "Cumulative upkeep {mana}".
pub(super) fn parse_cumulative_upkeep_keyword(line: &str) -> Option<Keyword> {
    let lower = line.to_lowercase();

    // Em-dash variant: "Cumulative upkeep—Pay 2 life." / "—Sacrifice a land."
    if let Some(((), rest)) = nom_on_lower(line, &lower, |i| {
        value((), pair(tag("cumulative upkeep"), tag("\u{2014}"))).parse(i)
    }) {
        let cost_text = strip_reminder_text(rest).trim().trim_end_matches('.');
        let cost = parse_cumulative_upkeep_cost(cost_text)?;
        return Some(Keyword::CumulativeUpkeep(cost));
    }

    // Space variant: "Cumulative upkeep {1}" / "Cumulative upkeep {G} or {W}"
    let ((), rest) = nom_on_lower(line, &lower, |i| {
        value((), tag("cumulative upkeep ")).parse(i)
    })?;
    let cost_text = strip_reminder_text(rest).trim().trim_end_matches('.');
    let cost = parse_cumulative_upkeep_cost(cost_text)?;
    Some(Keyword::CumulativeUpkeep(cost))
}
```

### 3b. Cost text → `AbilityCost`

A single helper (in `oracle_cost.rs` or a new sibling) dispatches by cost
shape, reusing existing combinators:

```rust
fn parse_cumulative_upkeep_cost(text: &str) -> Option<AbilityCost> {
    // Disjunctive: "{G} or {W}" — try parse_or_separated_mana_costs first.
    if let Some(costs) = parse_or_separated_mana_costs(text) {
        return Some(AbilityCost::OneOf {
            costs: costs.into_iter().map(|c| AbilityCost::Mana { cost: c }).collect(),
        });
    }
    // Pure mana: "{1}", "{2}{U}" — use parse_mana_symbols.
    if let Some((cost, "")) = parse_mana_symbols(text) {
        return Some(AbilityCost::Mana { cost });
    }
    // Non-mana via existing parse_single_cost: "Pay 2 life", "Sacrifice a land".
    parse_single_cost(text)
}
```

Three reused building blocks (no new string-matching):

- `parse_mana_symbols` — already used by the Escape keyword parser at
  `oracle_special.rs:342`.
- `parse_single_cost` — the existing cost dispatcher in `oracle_cost.rs`
  that already handles `Pay N life` (`AbilityCost::PayLife`), `Sacrifice
  ⟨filter⟩` (`AbilityCost::Sacrifice`), tap/discard/exile, etc.
- `parse_or_separated_mana_costs` — if it doesn't exist yet, we add it as
  a small `nom` combinator: `separated_list1(tag(" or "),
  parse_mana_symbols)`. (Verified to not exist via the existing
  `parse_cumulative_upkeep_or_mana` test passing on a raw string — we're
  upgrading it now.)

### 3c. Existing parser tests

The four tests at `oracle.rs:8567–8649` update from string-equality
assertions to typed-cost matches:

```rust
// parse_cumulative_upkeep_mana_cost
assert!(matches!(cu_kw.unwrap(),
    Keyword::CumulativeUpkeep(
        AbilityCost::Mana { cost: ManaCost::Cost { generic: 1, .. } }
    )));

// parse_cumulative_upkeep_life_payment
assert!(matches!(cu_kw.unwrap(),
    Keyword::CumulativeUpkeep(
        AbilityCost::PayLife { amount: QuantityExpr::Fixed { value: 2 } }
    )));

// parse_cumulative_upkeep_sacrifice
assert!(matches!(cu_kw.unwrap(),
    Keyword::CumulativeUpkeep(
        AbilityCost::Sacrifice { target: TargetFilter::Typed(_), count: 1 }
    )));

// parse_cumulative_upkeep_or_mana
let Keyword::CumulativeUpkeep(AbilityCost::OneOf { costs }) = cu_kw.unwrap()
    else { panic!() };
assert_eq!(costs.len(), 2);
```

### 3d. What the parser does NOT do

The parser produces only the base `AbilityCost`. The `PerCounter` wrapper
is constructed at **synthesis time** (Section 4), not at parse time,
because the parser produces card-static data while the wrapper expresses
runtime resolution semantics that belong to the trigger definition.

---

## 4. Synthesis (the trigger)

Mirrors `build_echo_trigger` at `database/synthesis.rs:1795`, with the
two-step "add age counter, then pay-or-sacrifice" structure expressed via
`sub_ability` chaining.

### 4a. New trigger builder

```rust
// crates/engine/src/database/synthesis.rs

/// CR 702.24a: Cumulative upkeep trigger — at the beginning of your
/// upkeep, put an age counter on this permanent, then pay
/// [base × age counters] or sacrifice it.
fn build_cumulative_upkeep_trigger(base_cost: AbilityCost) -> TriggerDefinition {
    // Inner step: "Sacrifice ~ unless you pay [base × age counters on ~]".
    let mut sacrifice_branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Sacrifice {
            target: TargetFilter::SelfRef,
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
    );
    sacrifice_branch.unless_pay = Some(UnlessPayModifier {
        cost: AbilityCost::PerCounter {
            counter: CounterType::Age,
            target: TargetFilter::SelfRef,
            base: Box::new(base_cost),
        },
        payer: TargetFilter::Controller,
    });

    // Outer step: "Put an age counter on ~", then sacrifice-or-pay branch.
    let execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::AddCounter {
            counter: CounterType::Age,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SelfRef,
        },
    )
    .sub_ability(sacrifice_branch);

    TriggerDefinition::new(TriggerMode::PayCumulativeUpkeep)
        .phase(Phase::Upkeep)
        .valid_target(TargetFilter::Controller)
        .execute(execute)
        .description(
            "CR 702.24a: At the beginning of your upkeep, put an age \
             counter on this permanent, then sacrifice it unless you pay \
             its upkeep cost for each age counter on it.".to_string(),
        )
}
```

Two assumptions to verify during planning, not blocking design approval:

- `Effect::AddCounter` exact name and field names. The planner-agent
  reconciles against the canonical effect variant; if it's
  `Effect::PutCounters` or similar, the synthesis name switches with no
  design impact.
- `sub_ability` chain semantics: the parent effect resolves first, then
  the sub-ability resolves, and `unless_pay` on the sub-ability prompts
  before the sub-effect resolves. This is the unverified linchpin of the
  trigger flow — see Section 5e.

### 4b. Wiring into the keyword → triggers map

```rust
// Dispatcher around synthesis.rs:126
  Keyword::Echo(cost) => vec![build_echo_trigger(cost.clone())],
+ Keyword::CumulativeUpkeep(cost) => vec![build_cumulative_upkeep_trigger(cost.clone())],
```

Add `is_cumulative_upkeep_trigger` recognizer alongside `is_echo_trigger`
at `synthesis.rs:1765` so the synthesis idempotency check prevents
double-installation.

### 4c. Multi-instance correctness (CR 702.24b)

If a card has multiple `Keyword::CumulativeUpkeep` entries, the
keyword-to-triggers mapping produces one `TriggerDefinition` per keyword.
Each fires independently at upkeep; each puts its own age counter; each
reads the current total at its own resolution time. This is structurally
correct without special-casing — it falls out of the
`vec![one_trigger_per_keyword]` shape plus the resolution-time counter
read in `PerCounter`. A synthetic two-instance test card asserts this
end-to-end (Section 7c, test 8).

### 4d. Removing the `match_unimplemented` mapping

`TriggerMode::PayCumulativeUpkeep` is currently in the
`match_unimplemented` list at `trigger_matchers.rs:128`. Replace with:

```rust
// In build_trigger_registry (trigger_matchers.rs:177)
r.insert(TriggerMode::PayCumulativeUpkeep, match_phase);
```

`match_phase` is the right matcher because the trigger has
`.phase(Phase::Upkeep)` — exactly the shape Echo uses. The corresponding
entry is removed from the `unimplemented_modes` array.

---

## 5. Resolution

How `PerCounter` expands at the unless-payment entry point, and how each
cost-shape branch routes through existing payment infrastructure.

### 5a. Expansion site

In `effects/mod.rs:2361`, the `resolved_cost` block currently handles
only `ManaDynamic`. Add a `PerCounter` arm at the same site, before the
existing `ManaDynamic` arm:

```rust
let resolved_cost = match &unless_pay.cost {
    AbilityCost::PerCounter { counter, target, base } => {
        // CR 702.24a: Count counters on `target` at resolution time
        // (so multi-instance CR 702.24b reads the post-tick total).
        let target_obj = crate::game::targeting::resolve_self_or_target(
            state, target, ability,
        );
        let n = target_obj
            .and_then(|id| state.objects.get(&id))
            .map(|obj| obj.counter_count(counter))
            .unwrap_or(0);
        expand_per_counter(base, n)
    }
    AbilityCost::ManaDynamic { quantity } => { /* existing */ }
    other => other.clone(),
};
```

`counter_count` is the existing object-level counter query (used by, e.g.,
Vanishing). The targeting helper resolves `SelfRef` to the trigger's
source.

**Source-already-gone behavior.** If the source has left the battlefield
between trigger fire and resolution, two engine paths converge on the
correct "do nothing" outcome and the design relies on either being
sufficient:

- The outer `Effect::AddCounter` resolves against `SelfRef`. If the
  source is no longer a battlefield permanent, the existing
  `SelfRef`-target resolution path for counter-add no-ops (per CR
  702.24a "if this permanent is on the battlefield" — built in via
  every `SelfRef`-targeted effect's existence check).
- Even if the engine still descends into `sub_ability`, the `PerCounter`
  resolution reads `counter_count` from the source ID; counters are
  tied to the battlefield-instance (CR 400.7), so the post-zone-change
  object reports `n = 0`. The CR 118.5 zero-cost branch at line 2387
  short-circuits, and the `Effect::Sacrifice { target: SelfRef }`
  fallback no-ops because the source is no longer on the battlefield.

The plan-step spike (Section 8b.2) confirms which path actually fires
in practice; if only one is exercised the other is harmless redundancy.
Test 7 (Section 7c) asserts the end-to-end outcome, not the specific
internal path.

### 5b. `expand_per_counter`

```rust
/// CR 702.24a: Expand "pay [base] for each counter on it" into the
/// concrete N-fold cost the player actually pays.
fn expand_per_counter(base: &AbilityCost, n: u32) -> AbilityCost {
    if n == 0 {
        // CR 118.5: zero-cost short-circuit handled by caller.
        return AbilityCost::Mana { cost: ManaCost::zero() };
    }
    match base {
        AbilityCost::Mana { cost } => AbilityCost::Mana { cost: cost.scaled(n) },
        AbilityCost::PayLife { amount } => AbilityCost::PayLife {
            amount: multiply_quantity_expr(amount, n),
        },
        AbilityCost::Sacrifice { target, count } => AbilityCost::Sacrifice {
            target: target.clone(),
            count: count * n,
        },
        AbilityCost::OneOf { costs } => {
            // CR 702.24a: "each choice is made separately for each age
            // counter, then either the entire set of costs is paid, or
            // none of them is paid".
            AbilityCost::Composite {
                costs: vec![AbilityCost::OneOf { costs: costs.clone() }; n as usize],
            }
        }
        AbilityCost::Composite { costs } => AbilityCost::Composite {
            costs: costs.iter().map(|c| expand_per_counter(c, n)).collect(),
        },
        // Other variants: not produced by the cumulative-upkeep parser
        // today. We expand them as Composite-of-N copies for the
        // general "pay X for each counter" semantics.
        other => AbilityCost::Composite {
            costs: vec![other.clone(); n as usize],
        },
    }
}
```

Reuse: `ManaCost::scaled(n)` already exists at `mana.rs:747`.
`multiply_quantity_expr` is a small helper — for the cumulative-upkeep
cards in scope it's only ever called on `QuantityExpr::Fixed`, so the
implementation is trivial; the non-`Fixed` arm panics with a descriptive
message until a future card needs it.

### 5c. The `OneOf × N` flow (Elephant Grass)

After expansion, an `OneOf`-base cumulative upkeep at N=3 becomes:

```
Composite {
    costs: [
        OneOf { [{G}, {W}] },
        OneOf { [{G}, {W}] },
        OneOf { [{G}, {W}] },
    ]
}
```

The existing unless-payment handler at `effects/mod.rs:2406` switches on
the resolved cost. For this `Composite`-of-`OneOf`s shape we need
sequential `UnlessPaymentChooseCost` prompts for each inner `OneOf`, then
a single `UnlessPayment` for the resulting flat `Composite` of chosen
sub-costs.

**Approach 5c-i (chosen):** Generalize `WaitingFor::UnlessPaymentChooseCost`
to handle a list of choices. The current variant carries one
`costs: Vec<AbilityCost>`. Add a sibling variant — or a parameterizing
ledger field — carrying `remaining_choices: Vec<Vec<AbilityCost>>` so
each choice is resolved in order, accumulating the picks into a final
`Composite` that re-enters `WaitingFor::UnlessPayment`. The exact
variant-vs-parameter shape is a planning decision (see Section 8b.3).

### 5d. The "tick first, then prompt" flow

The trigger's `execute` is the outer `Effect::AddCounter`. Resolution
order on `AbilityDefinition` is **parent effect → sub_ability**, so:

1. Trigger fires at upkeep, enters the stack, resolves.
2. `Effect::AddCounter { counter: Age, count: 1, target: SelfRef }`
   resolves — age counter added.
3. Resolver descends into `sub_ability` (the `Sacrifice + unless_pay`
   branch).
4. The unless-payment site at `effects/mod.rs:2351` resolves `PerCounter`
   against the post-tick counter total.
5. Prompt fires; on pay → branch consumed, no sacrifice. On decline →
   the `Effect::Sacrifice { target: SelfRef }` resolves.

CR 702.24b is satisfied: each instance's trigger ticks its own counter
and reads the running total. Multi-instance ordering follows the
standard APNAP / controller-chooses rules already implemented for
"at the beginning of your upkeep" trigger ordering.

### 5e. Verification spikes (deferred to planning)

Two assumptions in this section are pinned down during planning, not
before design approval:

1. **`Effect::AddCounter` shape.** Confirm the variant name and field
   names. If the on-battlefield placement requires a `Permanent` target
   rather than `SelfRef`, switch to whatever the resolver expects.
2. **`sub_ability` + `unless_pay` ordering linchpin.** The chain-based
   approach assumes `unless_pay` on a sub-ability prompts as the
   sub-ability resolves, after the parent effect has already mutated
   state. If this does not hold today, the fallback is to introduce a
   `pre_effect: Option<Effect>` field on `UnlessPayModifier` — a smaller
   surgery than restructuring the trigger.

---

## 6. Frontend, AI, Multiplayer

### 6a. Frontend

The frontend is a display layer — it renders the resolved cost the engine
emits.

1. **The unless-payment prompt.** When the engine sets
   `WaitingFor::UnlessPayment` (or the new multi-choice sibling for
   `OneOf × N`), the existing prompt overlay renders `cost` and
   `effect_description`. The engine's `effect_description` for the
   cumulative-upkeep sub_ability reads
   `"Sacrifice <card name> unless you pay <resolved_cost_pretty>"`,
   built by the existing prompt-text formatter from the expanded cost
   (e.g., `{3}` at N=3), not the raw `PerCounter`. No new frontend
   component required.
2. **The card tooltip / static text.** The keyword extractor at
   `oracle_keyword.rs:1082` produces the display string for keywords.
   Add an arm:
   ```rust
   Keyword::CumulativeUpkeep(cost) => {
       format!("Cumulative upkeep — {}", format_ability_cost(cost))
   }
   ```
   `format_ability_cost` is the existing typed-cost printer used
   elsewhere; it handles `Mana`, `PayLife`, `Sacrifice`, `OneOf`. No
   frontend code changes.
3. **Age-counter rendering.** Rendered via the generic
   `CounterType::as_str()` path — the existing battlefield counter badge
   displays `"age"` next to the card. No new visual asset.

### 6b. AI

1. **Cost-category classification.** `AbilityCost::cost_categories()` for
   `PerCounter` delegates to `base.cost_categories()`. AI heuristics that
   filter by category (e.g. "this ability sacrifices a permanent") see
   through the wrapper without per-mechanic logic.
2. **Pay-vs-let-die heuristic.** The existing AI unless-pay decider sees
   the concrete expanded cost (`Mana { scaled }`, `PayLife { multiplied
   }`, …) — no AI changes required. The AI pays Mystic Remora early
   then naturally drops it when {4} or {5} starts exceeding its value.
3. **AI legal-actions enumeration.** `legal_actions()` exposes the
   unless-payment choices to the AI policy via the same path as Esper
   Sentinel. The new multi-choice waiting-for variant (if implemented
   per 5c-i) gets a matching arm in `legal_actions`.

### 6c. Multiplayer / state filtering

`PerCounter` is a public cost on a public ability — no hidden
information. `filter_state_for_player` does not change. Age counters are
visible on the battlefield, the trigger and its prompt are visible to
all players, and the controller's payment choice is visible. The
`WaitingFor::UnlessPayment` filter at `game_state.rs:2460` already
handles the variant; the new multi-choice sibling gets the same
treatment as `UnlessPaymentChooseCost`.

### 6d. Coverage report

`game/coverage.rs:7235` already lists `"cumulative upkeep"`. After this
change, Mystic Remora, Inner Sanctum, Polar Kraken, and Elephant Grass
flip from "unimplemented" to "supported" because their abilities have
non-`Unimplemented` parsed forms and a real trigger matcher backing
them.

---

## 7. Tests

Tests cover the building blocks (parser + cost expansion) and the
end-to-end flow (trigger fires → counter ticks → prompt → pay/decline →
outcome).

### 7a. Parser unit tests (update existing)

The four tests at `oracle.rs:8567–8649` flip from `String`-equality to
typed-cost matches (assertions shown in 3c). No new parser cards needed.

One new parser test: a synthetic two-instance card asserts that two
`Keyword::CumulativeUpkeep` entries are extracted side-by-side (not
collapsed), the prerequisite for multi-instance trigger fan-out.

### 7b. `expand_per_counter` unit tests

In `effects/mod.rs` tests (or a new `effects/per_counter_test.rs`
sibling):

- `expand_per_counter(Mana{2}, 3)` → `Mana{6}` (verifies `ManaCost::scaled`).
- `expand_per_counter(PayLife{2}, 3)` → `PayLife{6}`.
- `expand_per_counter(Sacrifice{Land, 1}, 3)` → `Sacrifice{Land, 3}`.
- `expand_per_counter(OneOf{[{G}, {W}]}, 3)` → `Composite{[OneOf, OneOf, OneOf]}`.
- `expand_per_counter(anything, 0)` → `Mana{zero}` (CR 118.5 short-circuit).
- `expand_per_counter(Composite{[Mana{1}, PayLife{1}]}, 2)` →
  `Composite{[Mana{2}, PayLife{2}]}` (recursive case).

Pure-function tests, fast, deterministic, no game state needed.

### 7c. End-to-end trigger tests

In `game/engine.rs` tests, following the
`setup_esper_sentinel_unless_payment` pattern at line 12297:

1. **Mystic Remora — pay path.** 1-card state with `Cumulative upkeep
   {1}`. Advance to controller's turn 2 upkeep. Assert: trigger fires,
   age counter added (count=1), `WaitingFor::UnlessPayment { cost:
   Mana{1}, ... }`. Pay; assert no sacrifice, no `Sacrificed` event.
2. **Mystic Remora — decline path.** Same setup. Decline payment.
   Assert: sacrifice event, permanent moves to graveyard.
3. **Mystic Remora — three upkeeps in a row.** Pay turn 2, turn 3, turn
   4. Assert costs are `{1}`, `{2}`, `{3}` respectively (post-tick
   counter total each time).
4. **Polar Kraken — sacrifice cost path.** Setup with extra lands on
   battlefield. Trigger fires, counter ticks to 1, prompt is
   `Sacrifice{Land, 1}`. Sacrifice a land; assert kraken stays, one
   land in graveyard.
5. **Inner Sanctum — life cost.** Trigger fires at counter=2, prompt is
   `PayLife{4}`. Pay; assert 4 life lost, permanent stays.
6. **Elephant Grass — `OneOf × N`.** At counter=2, prompt is
   `Composite{[OneOf{G|W}, OneOf{G|W}]}`. Choose `{G}` then `{W}`.
   Assert final paid cost is `{G}{W}`, permanent stays.
7. **Source already left the battlefield.** Trigger created, source
   bounced/exiled between creation and resolution. At resolution:
   `PerCounter` resolves with N=0, zero-cost short-circuit, no
   sacrifice.
8. **Multi-instance (CR 702.24b).** Synthetic card with two
   `CumulativeUpkeep` keywords (`{1}` and `Pay 1 life`). At upkeep both
   triggers go on stack. Resolve first: counter→1, pay `{1}`. Resolve
   second: counter→2, pay 2 life. Assert: counters end at 2, no
   sacrifice, controller paid `{1}` + 2 life.

### 7d. AI duel smoke test

One scripted duel where one side fields a Mystic Remora deck. Assert no
panics across ~10 turns. Detailed AI evaluation tuning is out of scope.

### 7e. Tests we are NOT writing

- No frontend visual test. The frontend renders engine state via the
  existing prompt overlay; if engine state is right, the UI is right.
- No multiplayer-specific test. `filter_state_for_player` is unchanged;
  the multi-instance test indirectly covers trigger ordering.
- No tests for `PerCounter` against unfamiliar `base` variants (e.g.
  `Exile`, `Discard`). No cumulative-upkeep card uses them; the fallback
  `Composite-of-N-copies` arm in `expand_per_counter` is YAGNI-tolerated
  with an inline comment.

---

## 8. Edge Cases & Open Questions

### 8a. Edge cases the design handles

| Case | Behavior | Where it lives |
|------|----------|----------------|
| Source leaves battlefield before trigger resolves | N=0 → CR 118.5 short-circuit, no sacrifice | `expand_per_counter` zero branch + existing zero-cost short-circuit |
| Source's age counters removed (Hex Parasite, Vampire Hexmage) between upkeeps | Next trigger reads current counter total, prompts smaller cost | Resolution-time `counter_count` read in `PerCounter` |
| Card returns from graveyard / re-enters battlefield | New object instance, counters reset to 0 — CR 400.7 | Built-in via `GameObject`-instance counter storage |
| Controller change between trigger creation and resolution | Trigger remains tied to `payer: Controller`, re-resolves at prompt time | Existing `resolve_unless_payer` logic |
| Multiple permanents with cumulative upkeep in the same upkeep | Each fires its own trigger; APNAP / controller-orders stack | Existing trigger-stacking infrastructure |
| Cumulative upkeep on a non-controller's turn (e.g., copied / cloned) | `phase: Upkeep` + `valid_target: Controller` only fires on current controller's upkeep | Existing trigger gating, no change |

### 8b. Open questions deferred to plan/implementation

1. **`Effect::AddCounter` exact name and field shape.** Reconciled at
   planning against the canonical effect variant.
2. **`sub_ability` + `unless_pay` ordering linchpin.** Section 5d
   assumes parent-effect → sub_ability resolution order with
   sub-ability `unless_pay` prompting at sub-ability resolution time
   (after parent counter-tick mutation has landed). The plan-step spike
   confirms this against `effects/mod.rs`. Fallback: `pre_effect:
   Option<Effect>` field on `UnlessPayModifier`.
3. **Multi-choice `UnlessPaymentChooseCost` shape.** Sibling variant vs.
   parameterized form — a parameterize-vs-sibling decision made at the
   type-system edit.
4. **`multiply_quantity_expr` scope.** Handles `QuantityExpr::Fixed`
   today. Future cards with dynamic base costs grow the helper;
   `unreachable!()` panic with descriptive message until then.
5. **The four-card coverage flip.** Plan should include a verification
   step that runs `cargo coverage` and confirms count 4/4.

### 8c. Things we explicitly chose NOT to do

- **Refactor Echo to use `PerCounter`.** Echo is a one-shot "this turn"
  payment gated by `obj.echo_due`, not per-counter. Shapes differ
  despite surface similarity.
- **Add `pre_effect` on `UnlessPayModifier` proactively.** Only if the
  `sub_ability`-order spike (8b.2) shows the chain-based approach
  doesn't work.
- **Generalize `PerCounter` to `PerN { quantity, base }`.** Tempting,
  but no current MTG mechanic uses that shape outside cumulative
  upkeep, and the typed-counter form is more discoverable. Refactor
  only if a second consumer appears.
- **Frontend animation for age-counter add.** The existing counter-add
  animation already fires for any counter type.

### 8d. Design-principle consistency check

- Engine owns all logic: parser, synthesis, resolution, AI, coverage all
  in `engine` / `phase-ai`. ✓
- Frontend is a display layer: only typed-cost formatting and existing
  prompt overlay. ✓
- Build for the class: `PerCounter` is a typed building block; four
  cards covered; `OneOf × N` falls out naturally. ✓
- Parameterize, don't proliferate: `PerCounter` is one variant covering
  one CR section (CR 702.24); not unified across rule sections. ✓
- Rules-correct: every decision annotated against CR 702.24a/b, CR
  118.5, CR 400.7. ✓
- Single authority for costs: cost expansion happens at the
  unless-payment site in `effects/mod.rs`, not at call sites. ✓
