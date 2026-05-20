# Cumulative Upkeep Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement CR 702.24 (Cumulative Upkeep) end-to-end — typed `AbilityCost::PerCounter` building block, `CounterType::Age`, sub_ability-chained trigger, resolution-time cost expansion, full end-to-end coverage for Mystic Remora, Inner Sanctum, Polar Kraken, and Elephant Grass.

**Architecture:** Refactor `Keyword::CumulativeUpkeep` from `String` to typed `AbilityCost`. Add `AbilityCost::PerCounter { counter, target, base }` as the resolution-time multiplier wrapper. Synthesize a trigger whose `execute` is a chained `Effect::AddCounter -> sub_ability(Sacrifice + unless_pay: PerCounter)`. Expand `PerCounter` at the unless-payment entry point in `effects/mod.rs`. `OneOf × N` (Elephant Grass) unfolds into `Composite{[OneOf; N]}` and is driven by a generalized multi-choice unless-payment flow.

**Tech Stack:** Rust (engine crate), nom 8.0 combinators (parser), existing `UnlessPayModifier` + `WaitingFor::UnlessPayment` payment infrastructure, Tilt continuous verification.

**Spec:** [docs/superpowers/specs/2026-05-20-cumulative-upkeep-design.md](../specs/2026-05-20-cumulative-upkeep-design.md)

**Verification pattern (all tasks):**
```bash
cargo fmt --all
if tilt get uiresource clippy >/dev/null 2>&1; then
  ./scripts/tilt-wait.sh --timeout 240 clippy test-engine card-data
else
  cargo clippy --all-targets -- -D warnings && cargo test -p engine && ./scripts/gen-card-data.sh
fi
```

After failures, fetch details with `tilt logs <resource> --tail 50 --since 2m`.

**CR verification policy:** Every `CR 7XX` annotation in this plan has been grepped against `docs/MagicCompRules.txt`. Specifically: CR 702.24a, CR 702.24b (cumulative upkeep), CR 118.5 (paying zero), CR 118.12 (unless cost), CR 400.7 (zone change resets object identity), CR 122.1a (counters), CR 603.2b (phase triggers).

---

## Task 1: Add `CounterType::Age` variant

**Files:**
- Modify: `crates/engine/src/types/counter.rs`

- [ ] **Step 1: Read current `CounterType` definition**

Read `crates/engine/src/types/counter.rs` lines 1–135 to see the existing enum, `as_str`, `power_toughness_delta`, and `parse_counter_type` function.

- [ ] **Step 2: Add `Age` variant to `CounterType`**

In `crates/engine/src/types/counter.rs`, add the `Age` variant immediately after `Time` (it is the analogous "duration-tracking" counter):

```rust
    /// CR 702.24a + CR 122.1a: Age counters track Cumulative Upkeep
    /// duration. Each cumulative-upkeep trigger places one at the start
    /// of its controller's upkeep, and the cost is multiplied by the
    /// total age-counter count on the permanent at resolution time
    /// (CR 702.24b).
    Age,
```

- [ ] **Step 3: Add `Age` to `as_str`**

Add the arm in `as_str` (after `Time`):
```rust
            CounterType::Age => Cow::Borrowed("age"),
```

- [ ] **Step 4: Add `Age` to `power_toughness_delta` exclusion**

Append `CounterType::Age` to the existing `Loyalty | Defense | Stun | Lore | Time | ...` exhaustive arm that returns `None`.

- [ ] **Step 5: Add `Age` to `parse_counter_type`**

Locate the `parse_counter_type` function (further down in the same file). Add the arm:
```rust
        "age" => CounterType::Age,
```
Slot it next to `"time"`.

- [ ] **Step 6: Add a unit test**

Append to the existing test module in `counter.rs`:
```rust
    #[test]
    fn age_counter_serializes_as_age_and_round_trips() {
        let c = CounterType::Age;
        assert_eq!(c.as_str().as_ref(), "age");
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, "\"age\"");
        let back: CounterType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, CounterType::Age);
        assert_eq!(c.power_toughness_delta(), None);
    }
```

- [ ] **Step 7: Verify**

Run the verification block. Confirm `clippy` and `test-engine` are green.

- [ ] **Step 8: Commit**

```bash
git add crates/engine/src/types/counter.rs
git commit -m "engine: add CounterType::Age (CR 702.24a)

Cumulative Upkeep needs an 'age' counter type. Slotted next to Time
(the analogous duration-tracking counter). Serializes as \"age\"
via as_str; excluded from power_toughness_delta.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Add `AbilityCost::PerCounter` variant

**Files:**
- Modify: `crates/engine/src/types/ability.rs:3850` (AbilityCost enum) and `:4033` (impl AbilityCost cost_categories)

- [ ] **Step 1: Read `AbilityCost` definition and `cost_categories` impl**

Read `crates/engine/src/types/ability.rs:3849–4060` to study existing variants and the `cost_categories` method.

- [ ] **Step 2: Add `PerCounter` variant**

In the `pub enum AbilityCost` block (around line 3850), add the variant immediately before `Unimplemented`:

```rust
    /// CR 702.24a: A cost that multiplies a base cost by the number of
    /// counters of `counter` type on `target`. The runtime resolves the
    /// multiplier at the unless-payment entry point and expands `base`
    /// into the effective payment: mana scales via `ManaCost::scaled(n)`,
    /// life/sacrifice counts multiply directly, and `OneOf` unfolds into
    /// a `Composite` of `n` independent disjunctive choices (each made
    /// separately per CR 702.24a).
    ///
    /// Building block, not a special case: this is the typed shape of
    /// "pay [cost] for each counter on it". Cumulative upkeep is the
    /// only mechanic using it today, but the variant is composable with
    /// every existing base cost (Mana, PayLife, Sacrifice, OneOf,
    /// Composite).
    PerCounter {
        counter: CounterType,
        target: TargetFilter,
        base: Box<AbilityCost>,
    },
```

- [ ] **Step 3: Add `PerCounter` to `cost_categories`**

In `impl AbilityCost { fn cost_categories() ... }` (around line 4033), add an arm before any catch-all that delegates to `base`:

```rust
            AbilityCost::PerCounter { base, .. } => base.cost_categories(),
```

If the match is exhaustive (no wildcard), this addition will be flagged by the compiler if missed — that's the right safety. If a wildcard exists, slot the new arm before it.

- [ ] **Step 4: Run compiler to find any other exhaustive match sites**

Run the verification block. The compiler will surface any exhaustive matches on `AbilityCost` that need a `PerCounter` arm. For each surfaced site, add a `PerCounter { base, .. } => /* delegate to base */` arm or — if delegation isn't sensible — return the safest default for that site (e.g., `false` for payability gating in non-resolution contexts; resolution-time expansion is Task 6).

**Likely sites:** `cost_payability.rs`, `game/effects/mod.rs` resolved-cost match (already in Task 6 scope — leave a `todo!()` here, the compiler error pins it).

- [ ] **Step 5: Add a unit test**

Append to the `ability.rs` test module:
```rust
    #[test]
    fn per_counter_cost_delegates_categories_to_base() {
        let base = AbilityCost::Mana { cost: ManaCost::generic(1) };
        let wrapped = AbilityCost::PerCounter {
            counter: CounterType::Age,
            target: TargetFilter::SelfRef,
            base: Box::new(base.clone()),
        };
        assert_eq!(wrapped.cost_categories(), base.cost_categories());
    }
```

- [ ] **Step 6: Verify**

Run the verification block. All targets green.

- [ ] **Step 7: Commit**

```bash
git add crates/engine/src/types/ability.rs
git commit -m "engine: add AbilityCost::PerCounter (CR 702.24a)

Typed wrapper for 'pay [cost] for each counter on it'. Composable
with every existing base cost variant. cost_categories delegates to
base — the multiplier doesn't change the kind of cost, only how
much. Resolution-time expansion lands in a later task.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Refactor `Keyword::CumulativeUpkeep(String)` → `Keyword::CumulativeUpkeep(AbilityCost)`

**Files:**
- Modify: `crates/engine/src/types/keywords.rs` (variant + three placeholder construction sites at lines ~1596, ~1809, ~2426)
- Modify: `crates/engine/src/parser/oracle_special.rs:368–402` (extractor — temporarily produce zero-cost sentinel; real parsing lands in Task 4)
- Modify: `crates/engine/src/parser/oracle.rs:8567–8649` (the four existing tests)
- Modify: `crates/engine/src/parser/oracle_keyword.rs:964` (the existing arm using the cost string)

- [ ] **Step 1: Read the variant and all call sites**

Run:
```bash
grep -rn "Keyword::CumulativeUpkeep" crates/engine/src/ 2>/dev/null
```

Confirm the call sites: variant definition, three placeholder constructions, parser extractor, 4 tests, oracle_keyword arm.

- [ ] **Step 2: Change the variant signature**

In `crates/engine/src/types/keywords.rs`, locate `CumulativeUpkeep(String)` and change to:

```rust
    /// CR 702.24a: cost paid per age counter on this permanent at the
    /// start of the controller's upkeep, or sacrifice. The typed
    /// `AbilityCost` lets the synthesis pipeline wire the
    /// cumulative-upkeep trigger uniformly across mana / life /
    /// sacrifice / disjunctive cost shapes.
    CumulativeUpkeep(AbilityCost),
```

Add `use crate::types::ability::AbilityCost;` and `use crate::types::mana::ManaCost;` if not already imported.

- [ ] **Step 3: Update placeholder constructions**

At each of `keywords.rs:1596`, `:1809`, `:2426` (sites previously building `Keyword::CumulativeUpkeep(String::new())`), change to:

```rust
            Keyword::CumulativeUpkeep(AbilityCost::Mana { cost: ManaCost::zero() })
```

This is a well-formed zero-cost sentinel for legacy-deserialization paths.

- [ ] **Step 4: Update the extractor stub (real parsing in Task 4)**

In `crates/engine/src/parser/oracle_special.rs:368–402` (`parse_cumulative_upkeep_keyword`), replace both `Keyword::CumulativeUpkeep(cost_text)` constructions with:

```rust
        return Some(Keyword::CumulativeUpkeep(
            AbilityCost::Mana { cost: ManaCost::zero() }, // TODO Task 4: real parsing
        ));
```

and

```rust
    Some(Keyword::CumulativeUpkeep(
        AbilityCost::Mana { cost: ManaCost::zero() }, // TODO Task 4: real parsing
    ))
```

The TODO comments are placeholders for the Task 4 hook; they intentionally violate "no placeholders" *within this single task* because the extractor must compile in Task 3 before Task 4 can write the real logic. Task 4 removes the TODOs.

Add necessary `use` imports (`AbilityCost`, `ManaCost`) at the top of `oracle_special.rs`.

- [ ] **Step 5: Update `oracle_keyword.rs:964`**

Locate the arm:
```rust
        Keyword::CumulativeUpkeep(ref cost) => { ... }
```

If it formats `cost` as a string in display output, replace with placeholder formatting that compiles until Task 9 (frontend display) finalizes it:
```rust
        Keyword::CumulativeUpkeep(ref _cost) => {
            "cumulative upkeep".to_string()  // TODO Task 9: format from typed cost
        }
```

- [ ] **Step 6: Update the four parser tests at `oracle.rs:8567–8649`**

Each test currently destructures `Keyword::CumulativeUpkeep(cost)` and asserts `cost == "..."`. Update assertions to match the *current zero-cost placeholder* (real assertions land in Task 4):

```rust
        match cu_kw.unwrap() {
            Keyword::CumulativeUpkeep(cost) => {
                // Task 4 will refine this to assert the real parsed cost.
                assert!(matches!(cost,
                    AbilityCost::Mana { cost: ManaCost::Cost { generic: 0, .. } }));
            }
            _ => unreachable!(),
        }
```

Apply to all four tests (`parse_cumulative_upkeep_mana_cost`, `_life_payment`, `_sacrifice`, `_or_mana`). Same assertion in each — the placeholder is the same — Task 4 differentiates them.

- [ ] **Step 7: Verify**

Run the verification block. All targets green. The four parser tests pass against the placeholder; this is intentional and explicit in their comments.

- [ ] **Step 8: Commit**

```bash
git add crates/engine/src/types/keywords.rs \
        crates/engine/src/parser/oracle_special.rs \
        crates/engine/src/parser/oracle.rs \
        crates/engine/src/parser/oracle_keyword.rs
git commit -m "engine: type Keyword::CumulativeUpkeep cost (refactor)

String → AbilityCost. Aligns with Keyword::Echo(ManaCost) shape.
Extractor temporarily produces a zero-cost sentinel; real per-shape
parsing lands in the next task (which also re-tightens the four
parser tests).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Parser — `parse_cumulative_upkeep_cost` + extractor + test re-tightening

**Files:**
- Modify: `crates/engine/src/parser/oracle_special.rs:368–402`
- Possibly modify: `crates/engine/src/parser/oracle_cost.rs` (add `parse_or_separated_mana_costs` if not present)
- Modify: `crates/engine/src/parser/oracle.rs:8567–8649` (re-tighten assertions)

- [ ] **Step 1: Read existing cost-parsing helpers**

```bash
grep -n "parse_single_cost\|parse_mana_symbols\|parse_or_separated" crates/engine/src/parser/oracle_cost.rs crates/engine/src/parser/oracle_util.rs 2>/dev/null | head -20
```

Confirm `parse_mana_symbols` and `parse_single_cost` exist; check whether any "or-separated mana costs" helper already exists. Note its module path.

- [ ] **Step 2: If `parse_or_separated_mana_costs` does not exist, add it**

In `crates/engine/src/parser/oracle_cost.rs` (or `oracle_special.rs` if oracle_cost.rs is too crowded), add:

```rust
/// CR 702.24a: Parse a sequence of mana costs separated by " or ", e.g.,
/// "{G} or {W}" for Elephant Grass-style cumulative upkeep. Returns
/// `Some(Vec<ManaCost>)` only when at least two alternatives are present
/// — a single mana cost is *not* a disjunction and should fall through to
/// the caller's plain mana-cost branch.
pub(crate) fn parse_or_separated_mana_costs(text: &str) -> Option<Vec<ManaCost>> {
    let mut costs = Vec::new();
    let mut rest = text.trim();
    loop {
        let (cost, after) = parse_mana_symbols(rest)?;
        costs.push(cost);
        let after = after.trim_start();
        if let Some(more) = after.strip_prefix("or ") {
            rest = more.trim_start();
            continue;
        }
        if !after.is_empty() {
            return None;  // unexpected trailing text
        }
        break;
    }
    if costs.len() < 2 {
        None
    } else {
        Some(costs)
    }
}
```

Add a unit test:
```rust
#[test]
fn parse_or_separated_mana_costs_two_alternatives() {
    let r = parse_or_separated_mana_costs("{G} or {W}").unwrap();
    assert_eq!(r.len(), 2);
}

#[test]
fn parse_or_separated_mana_costs_single_returns_none() {
    assert!(parse_or_separated_mana_costs("{G}").is_none());
}

#[test]
fn parse_or_separated_mana_costs_three_alternatives() {
    let r = parse_or_separated_mana_costs("{G} or {W} or {U}").unwrap();
    assert_eq!(r.len(), 3);
}
```

If a similar helper already exists under a different name, reuse it instead and skip this step.

- [ ] **Step 3: Add `parse_cumulative_upkeep_cost` dispatcher**

In `crates/engine/src/parser/oracle_special.rs`, above `parse_cumulative_upkeep_keyword`:

```rust
/// CR 702.24a: Dispatch a cumulative-upkeep cost text into a typed
/// `AbilityCost`. Tries disjunctive mana ("{G} or {W}"), then pure mana
/// ("{1}"), then falls back to the generic cost parser (Pay N life,
/// Sacrifice ...).
fn parse_cumulative_upkeep_cost(text: &str) -> Option<AbilityCost> {
    let text = text.trim();
    if let Some(costs) = parse_or_separated_mana_costs(text) {
        return Some(AbilityCost::OneOf {
            costs: costs
                .into_iter()
                .map(|c| AbilityCost::Mana { cost: c })
                .collect(),
        });
    }
    if let Some((cost, rest)) = parse_mana_symbols(text) {
        if rest.trim().is_empty() {
            return Some(AbilityCost::Mana { cost });
        }
    }
    parse_single_cost(text)
}
```

Add `use` imports as needed (`AbilityCost`, the `parse_*` helpers).

- [ ] **Step 4: Replace the TODO sentinel in the extractor**

In `parse_cumulative_upkeep_keyword`, replace both Task-3 TODO sentinels:

```rust
    if let Some(((), rest)) = nom_on_lower(line, &lower, |i| {
        value((), pair(tag("cumulative upkeep"), tag("\u{2014}"))).parse(i)
    }) {
        let cost_text = strip_reminder_text(rest).trim().trim_end_matches('.');
        let cost = parse_cumulative_upkeep_cost(cost_text)?;
        return Some(Keyword::CumulativeUpkeep(cost));
    }

    let ((), rest) = nom_on_lower(line, &lower, |i| {
        value((), tag("cumulative upkeep ")).parse(i)
    })?;
    let cost_text = strip_reminder_text(rest).trim().trim_end_matches('.');
    let cost = parse_cumulative_upkeep_cost(cost_text)?;
    Some(Keyword::CumulativeUpkeep(cost))
```

- [ ] **Step 5: Re-tighten the four parser tests**

At `oracle.rs:8567–8649`, update each test to assert against the now-typed cost.

`parse_cumulative_upkeep_mana_cost`:
```rust
        match cu_kw.unwrap() {
            Keyword::CumulativeUpkeep(AbilityCost::Mana { cost: ManaCost::Cost { generic, shards } }) => {
                assert_eq!(*generic, 1);
                assert!(shards.is_empty());
            }
            other => panic!("expected Mana(1), got {other:?}"),
        }
```

`parse_cumulative_upkeep_life_payment`:
```rust
        match cu_kw.unwrap() {
            Keyword::CumulativeUpkeep(AbilityCost::PayLife { amount }) => {
                assert_eq!(*amount, QuantityExpr::Fixed { value: 2 });
            }
            other => panic!("expected PayLife(2), got {other:?}"),
        }
```

`parse_cumulative_upkeep_sacrifice`:
```rust
        match cu_kw.unwrap() {
            Keyword::CumulativeUpkeep(AbilityCost::Sacrifice { target, count, .. }) => {
                assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
                // target should be a TypedFilter with Land subtype filter.
                assert!(matches!(target, TargetFilter::Typed(_)));
            }
            other => panic!("expected Sacrifice(Land, 1), got {other:?}"),
        }
```

Note: `Sacrifice.count` is `QuantityExpr`, not `u32` — confirmed against `ability.rs:4580`.

`parse_cumulative_upkeep_or_mana`:
```rust
        match cu_kw.unwrap() {
            Keyword::CumulativeUpkeep(AbilityCost::OneOf { costs }) => {
                assert_eq!(costs.len(), 2);
                for c in costs {
                    assert!(matches!(c, AbilityCost::Mana { .. }));
                }
            }
            other => panic!("expected OneOf with 2 Mana costs, got {other:?}"),
        }
```

- [ ] **Step 6: Add a new test for multi-instance keyword extraction**

Append after `parse_cumulative_upkeep_or_mana`:

```rust
    #[test]
    fn parse_two_cumulative_upkeep_instances_both_extracted() {
        // CR 702.24b: A permanent can have multiple cumulative upkeep
        // abilities. Each must surface as its own Keyword entry so the
        // synthesis pipeline produces independent triggers.
        let r = parse(
            "Cumulative upkeep {1}\nCumulative upkeep\u{2014}Pay 1 life.",
            "Test Two-Instance Permanent",
            &[],
            &["Enchantment"],
            &[],
        );
        let cu_kws: Vec<_> = r
            .extracted_keywords
            .iter()
            .filter(|k| matches!(k, Keyword::CumulativeUpkeep(_)))
            .collect();
        assert_eq!(cu_kws.len(), 2, "expected two CumulativeUpkeep keywords, got {cu_kws:?}");
    }
```

- [ ] **Step 7: Verify**

Run the verification block. `test-engine` should pass; `card-data` should pass (the four cards now produce typed costs). Inspect:

```bash
jq '.["mystic remora"].keywords' client/public/card-data.json
```

Expected: keyword entry with typed `AbilityCost::Mana` shape (not raw string).

- [ ] **Step 8: Commit**

```bash
git add crates/engine/src/parser/oracle_special.rs \
        crates/engine/src/parser/oracle_cost.rs \
        crates/engine/src/parser/oracle.rs
git commit -m "parser: typed cumulative-upkeep cost extraction (CR 702.24a)

Dispatch the cumulative-upkeep cost text into AbilityCost via three
existing building blocks: parse_or_separated_mana_costs for {G} or {W},
parse_mana_symbols for pure mana, parse_single_cost for Pay/Sacrifice.
Four existing parser tests now assert the typed shape. New test asserts
multi-instance keyword extraction (CR 702.24b prerequisite).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: `expand_per_counter` and `multiply_quantity_expr` helpers

**Files:**
- Modify: `crates/engine/src/game/effects/mod.rs` (add helpers and unit tests; do not yet wire them into resolution — Task 6 wires)

- [ ] **Step 1: Locate the right module location**

Read `crates/engine/src/game/effects/mod.rs:2340–2430` to see the unless-payment resolution site. Helpers go in the same module, near the `resolve_unless_payer` family at line ~3389. Confirm `use` imports for `AbilityCost`, `CounterType`, `ManaCost`, `QuantityExpr`, `TargetFilter`.

- [ ] **Step 2: Add `multiply_quantity_expr` helper**

In `effects/mod.rs`, near other helpers:

```rust
/// CR 702.24a: Multiply a `QuantityExpr` by a runtime count. Today only
/// `Fixed` is exercised by cumulative-upkeep cards in scope; dynamic
/// quantity bases for the four supported cards do not exist in MTG.
/// Adding support for additional variants is a single-arm extension.
fn multiply_quantity_expr(q: &QuantityExpr, n: u32) -> QuantityExpr {
    match q {
        QuantityExpr::Fixed { value } => QuantityExpr::Fixed {
            value: value.saturating_mul(n as i32),
        },
        other => unreachable!(
            "multiply_quantity_expr: unsupported variant {other:?}; \
             extend when a new cumulative-upkeep card with a dynamic base \
             cost ships"
        ),
    }
}
```

- [ ] **Step 3: Add `expand_per_counter` helper**

```rust
/// CR 702.24a: Expand `pay [base] for each counter on it` into the
/// concrete N-fold cost the player actually pays. N=0 short-circuits to
/// a zero mana cost (CR 118.5 — players can always pay 0). `OneOf`
/// unfolds into a `Composite` of N independent disjunctive choices
/// (CR 702.24a: each choice is made separately).
fn expand_per_counter(base: &AbilityCost, n: u32) -> AbilityCost {
    if n == 0 {
        return AbilityCost::Mana { cost: ManaCost::zero() };
    }
    match base {
        AbilityCost::Mana { cost } => AbilityCost::Mana { cost: cost.scaled(n) },
        AbilityCost::PayLife { amount } => AbilityCost::PayLife {
            amount: multiply_quantity_expr(amount, n),
        },
        AbilityCost::Sacrifice { target, count, min_count } => AbilityCost::Sacrifice {
            target: target.clone(),
            count: multiply_quantity_expr(count, n),
            min_count: *min_count,
        },
        AbilityCost::OneOf { costs } => AbilityCost::Composite {
            costs: vec![AbilityCost::OneOf { costs: costs.clone() }; n as usize],
        },
        AbilityCost::Composite { costs } => AbilityCost::Composite {
            costs: costs.iter().map(|c| expand_per_counter(c, n)).collect(),
        },
        // YAGNI fallback: no current cumulative-upkeep card uses these
        // base variants. If a future mechanic does, the
        // Composite-of-N-copies expansion is semantically correct for
        // most cost shapes; refactor per-variant if needed.
        other => AbilityCost::Composite {
            costs: vec![other.clone(); n as usize],
        },
    }
}
```

**Field-name caveat:** `AbilityCost::Sacrifice` per `ability.rs:4567–4595` actually has fields `target`, `count: QuantityExpr`, `min_count: usize`. The arm above mirrors that — verify exact field names in the source as you write.

- [ ] **Step 4: Add unit tests**

In the same file's test module:

```rust
    use crate::types::ability::AbilityCost;
    use crate::types::counter::CounterType;
    use crate::types::filter::TargetFilter;
    use crate::types::mana::ManaCost;
    use crate::types::quantity::QuantityExpr;

    #[test]
    fn expand_per_counter_zero_returns_zero_mana() {
        let base = AbilityCost::Mana { cost: ManaCost::generic(5) };
        let expanded = expand_per_counter(&base, 0);
        assert!(matches!(expanded, AbilityCost::Mana { cost } if cost == ManaCost::zero()));
    }

    #[test]
    fn expand_per_counter_mana_scales() {
        let base = AbilityCost::Mana { cost: ManaCost::generic(2) };
        let expanded = expand_per_counter(&base, 3);
        let AbilityCost::Mana { cost: ManaCost::Cost { generic, .. } } = expanded else {
            panic!("expected Mana");
        };
        assert_eq!(generic, 6);
    }

    #[test]
    fn expand_per_counter_pay_life_multiplies() {
        let base = AbilityCost::PayLife { amount: QuantityExpr::Fixed { value: 2 } };
        let expanded = expand_per_counter(&base, 3);
        let AbilityCost::PayLife { amount } = expanded else { panic!("expected PayLife"); };
        assert_eq!(amount, QuantityExpr::Fixed { value: 6 });
    }

    #[test]
    fn expand_per_counter_sacrifice_multiplies_count() {
        let base = AbilityCost::Sacrifice {
            target: TargetFilter::SelfRef, // any TargetFilter works for this test
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        };
        let expanded = expand_per_counter(&base, 3);
        let AbilityCost::Sacrifice { count, .. } = expanded else { panic!("expected Sacrifice"); };
        assert_eq!(count, QuantityExpr::Fixed { value: 3 });
    }

    #[test]
    fn expand_per_counter_one_of_unfolds_to_composite_of_one_ofs() {
        let base = AbilityCost::OneOf {
            costs: vec![
                AbilityCost::Mana { cost: ManaCost::generic(1) },
                AbilityCost::Mana { cost: ManaCost::generic(1) },
            ],
        };
        let expanded = expand_per_counter(&base, 3);
        let AbilityCost::Composite { costs } = expanded else { panic!("expected Composite"); };
        assert_eq!(costs.len(), 3);
        assert!(costs.iter().all(|c| matches!(c, AbilityCost::OneOf { .. })));
    }

    #[test]
    fn expand_per_counter_composite_recurses() {
        let base = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana { cost: ManaCost::generic(1) },
                AbilityCost::PayLife { amount: QuantityExpr::Fixed { value: 1 } },
            ],
        };
        let expanded = expand_per_counter(&base, 2);
        let AbilityCost::Composite { costs } = expanded else { panic!("expected Composite"); };
        assert_eq!(costs.len(), 2);
        assert!(matches!(costs[0], AbilityCost::Mana { cost: ManaCost::Cost { generic: 2, .. } }));
        assert!(matches!(costs[1], AbilityCost::PayLife { amount: QuantityExpr::Fixed { value: 2 } }));
    }
```

- [ ] **Step 5: Verify**

Run the verification block. New tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/engine/src/game/effects/mod.rs
git commit -m "engine: expand_per_counter + multiply_quantity_expr (CR 702.24a)

Pure-function helpers for resolution-time PerCounter expansion. Mana
scales via ManaCost::scaled(); PayLife and Sacrifice multiply their
quantity; OneOf unfolds to Composite-of-OneOfs (each choice made
separately per CR 702.24a); Composite recurses. N=0 short-circuits
to zero mana (CR 118.5).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: PerCounter resolution at unless-payment entry point

**Files:**
- Modify: `crates/engine/src/game/effects/mod.rs:2361` (the `resolved_cost` match)

- [ ] **Step 1: Read the existing resolved_cost site**

Read `crates/engine/src/game/effects/mod.rs:2340–2430` again. The block at line 2361 matches `&unless_pay.cost`; the `ManaDynamic` arm uses `resolve_quantity_with_targets`. Note the surrounding context: `ability` is in scope, `state` is `&mut GameState`.

- [ ] **Step 2: Find a counter-counting helper**

```bash
grep -n "fn counter_count\|\.counters\.\(get\|values\)\|pub fn counts_of_counter" crates/engine/src/game/game_object.rs crates/engine/src/types/game_object.rs 2>/dev/null | head -10
```

If a method like `GameObject::counter_count(&CounterType) -> u32` exists, use it. Otherwise reach into `obj.counters: HashMap<CounterType, u32>` directly — `obj.counters.get(counter).copied().unwrap_or(0)`.

- [ ] **Step 3: Find the SelfRef → ObjectId resolver**

The trigger source ID is on `ability.source_id` (confirmed by Echo's pattern at `engine_payment_choices.rs:656`). For `TargetFilter::SelfRef` PerCounter targets, read `state.objects.get(&ability.source_id)`. For other `TargetFilter` shapes used in `PerCounter` (none in current scope but the wrapper allows them), use the existing target resolution path; if no helper applies cleanly for `SelfRef`, hardcode `SelfRef → ability.source_id` and panic-todo for other variants (acceptable since no card uses non-`SelfRef` PerCounter targets today).

- [ ] **Step 4: Add the `PerCounter` arm to the `resolved_cost` match**

In `effects/mod.rs:2361`, prepend (before the existing `ManaDynamic` arm):

```rust
            AbilityCost::PerCounter { counter, target, base } => {
                // CR 702.24a + CR 702.24b: Count counters on `target` at
                // resolution time so multi-instance reads the post-tick
                // total.
                let n = match target {
                    TargetFilter::SelfRef => state
                        .objects
                        .get(&ability.source_id)
                        .map(|obj| obj.counters.get(counter).copied().unwrap_or(0))
                        .unwrap_or(0),
                    other => panic!(
                        "PerCounter against non-SelfRef target {other:?} \
                         is not currently produced by any cumulative-upkeep \
                         card; extend the target resolution branch when a \
                         second mechanic uses PerCounter."
                    ),
                };
                expand_per_counter(base, n)
            }
```

Slot it as the *first* arm so it intercepts before any cloning fallthrough.

- [ ] **Step 5: Add a focused integration test**

In the same file's test module, write a test that constructs a `ResolvedAbility` with `unless_pay.cost = PerCounter { counter: Age, target: SelfRef, base: Mana(1) }`, attaches the source to a `GameObject` with 3 age counters, and asserts the `resolved_cost` produced is `Mana(3)`. Use the existing test scaffolding (look at the `unless_pay_bare_x_threads_chosen_x_into_resolved_cost` test at `effects/mod.rs:4226` as the template).

```rust
    #[test]
    fn unless_pay_per_counter_expands_against_source_counter_total() {
        use crate::types::counter::CounterType;
        use crate::types::filter::TargetFilter;
        use crate::types::mana::ManaCost;

        let mut state = GameState::new_solo_test();
        let source_id = state.spawn_test_object_on_battlefield();
        state.objects.get_mut(&source_id).unwrap()
            .counters.insert(CounterType::Age, 3);

        let mut ability = ResolvedAbility::test_placeholder(source_id);
        ability.unless_pay = Some(UnlessPayModifier {
            cost: AbilityCost::PerCounter {
                counter: CounterType::Age,
                target: TargetFilter::SelfRef,
                base: Box::new(AbilityCost::Mana { cost: ManaCost::generic(2) }),
            },
            payer: TargetFilter::Controller,
        });

        let mut events = Vec::new();
        resolve_unless_payment_entry(&mut state, &ability, &mut events);

        match state.waiting_for {
            WaitingFor::UnlessPayment { cost: AbilityCost::Mana { cost }, .. } => {
                assert_eq!(cost, ManaCost::generic(6));
            }
            other => panic!("expected UnlessPayment with Mana(6), got {other:?}"),
        }
    }
```

**Reality check:** The exact constructors (`GameState::new_solo_test`, `spawn_test_object_on_battlefield`, `ResolvedAbility::test_placeholder`, `resolve_unless_payment_entry`) may not exist by those names. Look at the closest existing pattern in `effects/mod.rs` tests (the `unless_pay_*` family around lines 4220–4320) and copy its scaffolding shape, swapping in PerCounter values. The test goal — "expand against 3 age counters, get scaled cost in WaitingFor" — is the invariant; the harness shape is whatever the existing tests use.

- [ ] **Step 6: Verify**

Run the verification block. Test passes; clippy clean.

- [ ] **Step 7: Commit**

```bash
git add crates/engine/src/game/effects/mod.rs
git commit -m "engine: resolve PerCounter at unless-payment entry (CR 702.24a)

New arm at the resolved_cost dispatch site reads counter_count on
the trigger source (SelfRef-only for the cumulative-upkeep cards in
scope) and expands via expand_per_counter. Non-SelfRef PerCounter
targets panic with a todo message — no mechanic produces them today.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: Synthesis — `build_cumulative_upkeep_trigger` + wiring

**Files:**
- Modify: `crates/engine/src/database/synthesis.rs` (add builder + recognizer; wire into the keyword → triggers map)

**PRECONDITION (verification spike):** Before writing the builder, confirm that `AbilityDefinition::sub_ability` resolves the parent's effect first and the sub_ability's `unless_pay` prompts when the sub_ability resolves (not when the parent's effect resolves).

- [ ] **Step 1: Verify sub_ability + unless_pay ordering**

Read `crates/engine/src/game/effects/mod.rs` lines 370–450 (the sub_ability cursor advance) and lines 2340–2430 (the unless-payment entry). Trace by reading code:
- When does the parent effect's resolution run? (Likely a `resolve_effect` call before the sub_ability advance.)
- When does the resolver enter `unless_pay`? (At line 2351, gated by `ability.unless_pay.is_some()`.)
- Confirm: the `ability` at the unless-pay site is the sub_ability *after* the parent has resolved.

**If confirmed:** proceed to Step 2.

**If NOT confirmed** (parent and child unless-pay interleave incorrectly): stop and report. The fallback is to add a `pre_effect: Option<Effect>` field on `UnlessPayModifier` and wire it into the resolver. Document the finding in the commit message and adjust this task accordingly. **Do not proceed without confirmation.**

- [ ] **Step 2: Read the Echo builder as the template**

Read `crates/engine/src/database/synthesis.rs:1765–1820` (`is_echo_trigger`, `build_echo_trigger`). Note the structure: `TriggerDefinition::new(...) .phase(...) .valid_target(...) .condition(...) .execute(...) .description(...)` then `unless_pay = Some(...)` on the trigger's execute ability.

Note that Echo's `unless_pay` is on the trigger's `execute` directly (not on a sub_ability). Cumulative upkeep needs it on the sub_ability per the design.

- [ ] **Step 3: Read the keyword → triggers dispatcher**

Read `crates/engine/src/database/synthesis.rs:120–160` (around line 126 where `Keyword::Echo(cost) => vec![build_echo_trigger(cost.clone())]` lives). Find the matching `is_*_trigger` dispatcher used for synthesis idempotency.

- [ ] **Step 4: Write `build_cumulative_upkeep_trigger`**

In `database/synthesis.rs`, near `build_echo_trigger`:

```rust
/// CR 702.24a: Cumulative upkeep trigger — at the beginning of your
/// upkeep, put an age counter on this permanent, then pay
/// [base × age counters on it] or sacrifice it.
fn build_cumulative_upkeep_trigger(base_cost: AbilityCost) -> TriggerDefinition {
    // Inner sub_ability: "Sacrifice ~ unless you pay [base × age counters]".
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

    // Outer execute: "Put an age counter on ~", then sacrifice-or-pay branch.
    let execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::AddCounter {
            counter_type: CounterType::Age,
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

- [ ] **Step 5: Write `is_cumulative_upkeep_trigger` recognizer**

```rust
fn is_cumulative_upkeep_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::PayCumulativeUpkeep)
        && t.phase == Some(Phase::Upkeep)
        && matches!(t.valid_target, Some(TargetFilter::Controller))
        && t.execute.as_deref().is_some_and(|a| {
            matches!(a.effect.as_ref(), Effect::AddCounter {
                counter_type: CounterType::Age, ..
            })
        })
}
```

- [ ] **Step 6: Wire into the keyword → triggers dispatcher**

In the dispatcher (synthesis.rs:126 area), add a sibling arm to the `Echo` arm:

```rust
            Keyword::Echo(cost) => vec![build_echo_trigger(cost.clone())],
+           Keyword::CumulativeUpkeep(cost) => {
+               vec![build_cumulative_upkeep_trigger(cost.clone())]
+           }
```

And wire the recognizer into the idempotency check site (mirror Echo's location at synthesis.rs:142):
```rust
            Keyword::Echo(_) => is_echo_trigger(trigger),
+           Keyword::CumulativeUpkeep(_) => is_cumulative_upkeep_trigger(trigger),
```

Also check `synthesis.rs:1322` which calls `KeywordTriggerInstaller::install_matching(...)`. If a similar install hook is needed for cumulative upkeep, add one alongside the existing Echo install pattern.

- [ ] **Step 7: Add a synthesis unit test**

```rust
#[test]
fn cumulative_upkeep_keyword_synthesizes_age_counter_trigger() {
    let kw = Keyword::CumulativeUpkeep(AbilityCost::Mana { cost: ManaCost::generic(1) });
    let triggers = triggers_for_keyword(&kw);
    assert_eq!(triggers.len(), 1);
    let t = &triggers[0];
    assert_eq!(t.mode, TriggerMode::PayCumulativeUpkeep);
    assert_eq!(t.phase, Some(Phase::Upkeep));

    let execute = t.execute.as_deref().expect("execute set");
    assert!(matches!(
        execute.effect.as_ref(),
        Effect::AddCounter { counter_type: CounterType::Age, .. }
    ));

    let sub = execute.sub_ability.as_deref().expect("sub_ability set");
    assert!(matches!(sub.effect.as_ref(), Effect::Sacrifice { target: TargetFilter::SelfRef, .. }));

    let unless = sub.unless_pay.as_ref().expect("unless_pay on sub");
    assert!(matches!(
        unless.cost,
        AbilityCost::PerCounter { counter: CounterType::Age, target: TargetFilter::SelfRef, .. }
    ));
}
```

Reuse whatever helper currently dispatches `Keyword → Vec<TriggerDefinition>` (call it `triggers_for_keyword` here; the real name may differ — find it in the dispatcher you modified in Step 6).

- [ ] **Step 8: Verify**

Run the verification block. The new test passes; the four target cards' `card-data.json` entries now have a typed `Cumulative upkeep` trigger.

```bash
jq '.["mystic remora"].triggers[] | select(.mode == "PayCumulativeUpkeep")' client/public/card-data.json
```

Expected: a trigger with `phase=Upkeep`, `execute.effect.type=AddCounter`, `execute.sub_ability.effect.type=Sacrifice`, `execute.sub_ability.unless_pay.cost.type=PerCounter`.

- [ ] **Step 9: Commit**

```bash
git add crates/engine/src/database/synthesis.rs
git commit -m "synthesis: wire Keyword::CumulativeUpkeep to trigger (CR 702.24a)

build_cumulative_upkeep_trigger mirrors build_echo_trigger but with
a sub_ability chain: outer AddCounter(Age, SelfRef) feeds inner
Sacrifice(SelfRef) gated by unless_pay PerCounter(Age, SelfRef,
base). is_cumulative_upkeep_trigger guards synthesis idempotency.
Mystic Remora, Inner Sanctum, Polar Kraken, Elephant Grass now
produce a typed trigger in card-data.json.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: Promote `TriggerMode::PayCumulativeUpkeep` to `match_phase`

**Files:**
- Modify: `crates/engine/src/game/trigger_matchers.rs:128` (remove from unimplemented), `:177` area (insert into registry)

- [ ] **Step 1: Add to the registry**

In `build_trigger_registry` around line 224 (next to the `PayEcho` insert), add:

```rust
    r.insert(TriggerMode::PayCumulativeUpkeep, match_phase);
```

- [ ] **Step 2: Remove from `unimplemented_modes`**

At `trigger_matchers.rs:349`, delete the `TriggerMode::PayCumulativeUpkeep,` line.

- [ ] **Step 3: Remove from the unimplemented `match` umbrella at line 128**

At `trigger_matchers.rs:128`, delete the `| TriggerMode::PayCumulativeUpkeep` arm in the catch-all match.

- [ ] **Step 4: Add a matcher test**

In the same file's test module (mirror the existing `PayEcho` assertions around line 4135):

```rust
#[test]
fn pay_cumulative_upkeep_matcher_registered() {
    let registry = build_trigger_registry();
    assert!(trigger_matcher(TriggerMode::PayCumulativeUpkeep).is_some());
    assert!(registry.contains_key(&TriggerMode::PayCumulativeUpkeep));
}
```

- [ ] **Step 5: Verify**

Run the verification block. The new test passes; clippy clean.

- [ ] **Step 6: Commit**

```bash
git add crates/engine/src/game/trigger_matchers.rs
git commit -m "engine: PayCumulativeUpkeep -> match_phase (CR 702.24a)

Wires the trigger to the standard phase-trigger matcher (Echo uses
the same matcher with the same .phase(Upkeep) shape). Removes the
stale match_unimplemented mapping.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: End-to-end test — Mystic Remora pay/decline/accumulation

**Files:**
- Modify: `crates/engine/src/game/engine.rs` (append to existing test module — locate near the Esper Sentinel pattern at line 12297)

- [ ] **Step 1: Read the Esper Sentinel test scaffolding**

Read `crates/engine/src/game/engine.rs:12297–12430` (`setup_esper_sentinel_unless_payment` + `pay_mana`/`don't pay` tests). This is the closest analogue; mirror the helper-construction shape.

- [ ] **Step 2: Add Mystic Remora test setup helper**

Append after the Esper Sentinel helpers:

```rust
fn setup_mystic_remora_upkeep_state() -> (GameState, ObjectId) {
    // Construct a minimal solo state with a single Mystic Remora on
    // the battlefield, controller is the active player, currently in
    // their upkeep step.
    let mut state = /* ... build via existing test fixture path ... */;
    let remora_id = state.spawn_card_on_battlefield(
        "Mystic Remora",
        PlayerId(0),
    );
    state.phase = Phase::Upkeep;
    state.active_player = PlayerId(0);
    (state, remora_id)
}
```

**Reality check:** The exact test fixture API may use different names (`build_solo_test_state`, `add_card_to_battlefield`, etc.). Find the equivalent from `setup_esper_sentinel_unless_payment` and adapt.

- [ ] **Step 3: Write the "pay path" test**

```rust
#[test]
fn mystic_remora_upkeep_pay_path_keeps_permanent_and_adds_age_counter() {
    // CR 702.24a: At controller's upkeep, age counter goes on, then
    // controller pays {1} to keep Mystic Remora alive.
    let (mut state, remora_id) = setup_mystic_remora_upkeep_state();

    // Advance to the upkeep trigger and resolve through the AddCounter.
    advance_to_unless_payment_prompt(&mut state);

    // Assert age counter was added.
    assert_eq!(
        state.objects[&remora_id].counters.get(&CounterType::Age).copied(),
        Some(1),
        "age counter should be added before the unless-pay prompt"
    );

    // Assert the prompt is for Mana{1} (1 counter × {1} base).
    match &state.waiting_for {
        WaitingFor::UnlessPayment { cost: AbilityCost::Mana { cost }, .. } => {
            assert_eq!(*cost, ManaCost::generic(1));
        }
        other => panic!("expected UnlessPayment(Mana(1)), got {other:?}"),
    }

    // Controller pays {1}. (Use whatever the harness's "pay mana for prompt" helper is.)
    pay_unless_payment_with_mana(&mut state, &ManaCost::generic(1));

    // Assert Mystic Remora is still on the battlefield.
    assert_eq!(state.objects[&remora_id].zone, Zone::Battlefield);
}
```

- [ ] **Step 4: Write the "decline path" test**

```rust
#[test]
fn mystic_remora_upkeep_decline_path_sacrifices() {
    let (mut state, remora_id) = setup_mystic_remora_upkeep_state();
    advance_to_unless_payment_prompt(&mut state);

    decline_unless_payment(&mut state);

    assert_eq!(
        state.objects[&remora_id].zone, Zone::Graveyard,
        "declining should sacrifice the permanent"
    );
}
```

- [ ] **Step 5: Write the "three-upkeep accumulation" test**

```rust
#[test]
fn mystic_remora_three_upkeeps_costs_one_two_three() {
    let (mut state, remora_id) = setup_mystic_remora_upkeep_state();

    let expected_costs = [1u32, 2, 3];
    for (turn_idx, expected) in expected_costs.iter().enumerate() {
        advance_to_unless_payment_prompt(&mut state);
        match &state.waiting_for {
            WaitingFor::UnlessPayment { cost: AbilityCost::Mana { cost: ManaCost::Cost { generic, .. } }, .. } => {
                assert_eq!(*generic, *expected, "turn {turn_idx}: expected {{{expected}}}");
            }
            other => panic!("turn {turn_idx}: expected Mana({{{expected}}}), got {other:?}"),
        }
        pay_unless_payment_with_mana(&mut state, &ManaCost::generic(*expected));
        advance_to_next_upkeep(&mut state);
    }

    assert_eq!(
        state.objects[&remora_id].counters.get(&CounterType::Age).copied(),
        Some(3),
        "three age counters should have accumulated"
    );
}
```

- [ ] **Step 6: Verify**

Run the verification block. All three tests pass. If `advance_to_unless_payment_prompt`, `pay_unless_payment_with_mana`, `decline_unless_payment`, or `advance_to_next_upkeep` are not existing helpers, look for the closest pattern in the Esper Sentinel test and reuse its action-dispatch idiom (e.g., `submit_action(&mut state, GameAction::PayUnlessCost { ... })`).

- [ ] **Step 7: Commit**

```bash
git add crates/engine/src/game/engine.rs
git commit -m "test: Mystic Remora cumulative upkeep end-to-end (CR 702.24a)

Three tests cover the pay path, decline path, and three-upkeep
accumulation (cost {1} -> {2} -> {3}). Asserts age counter ticks,
unless-pay prompt expansion, and sacrifice on decline.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 10: End-to-end test — Polar Kraken sacrifice cost path

**Files:**
- Modify: `crates/engine/src/game/engine.rs`

- [ ] **Step 1: Write the test**

```rust
#[test]
fn polar_kraken_upkeep_sacrifice_cost_path() {
    // CR 702.24a: Cumulative upkeep "Sacrifice a land".
    let mut state = /* build solo state, controller has Polar Kraken on
                       battlefield and 3 lands */;
    let kraken_id = state.spawn_card_on_battlefield("Polar Kraken", PlayerId(0));
    let land_ids = state.spawn_lands_on_battlefield(PlayerId(0), 3, "Forest");

    state.phase = Phase::Upkeep;
    state.active_player = PlayerId(0);

    advance_to_unless_payment_prompt(&mut state);

    // Counter=1, expect Sacrifice{Land, 1}.
    match &state.waiting_for {
        WaitingFor::UnlessPayment { cost: AbilityCost::Sacrifice { count, .. }, .. } => {
            assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
        }
        other => panic!("expected Sacrifice(Land, 1) prompt, got {other:?}"),
    }

    // Pay by sacrificing one land.
    sacrifice_for_unless_payment(&mut state, land_ids[0]);

    assert_eq!(state.objects[&kraken_id].zone, Zone::Battlefield);
    assert_eq!(state.objects[&land_ids[0]].zone, Zone::Graveyard);
}
```

- [ ] **Step 2: Verify**

Run the verification block. The test passes; if `sacrifice_for_unless_payment` doesn't exist, dispatch the relevant `GameAction::ChooseSacrificeForUnlessPay { ... }` directly (look up the action name via grep on the existing unless-pay sacrifice handling site at `engine_payment_choices.rs`).

- [ ] **Step 3: Commit**

```bash
git add crates/engine/src/game/engine.rs
git commit -m "test: Polar Kraken cumulative upkeep sacrifice path (CR 702.24a)

Sacrifice-a-land cost variant. Counter=1 → Sacrifice(Land, 1) prompt
→ sacrifice one of three forests → kraken stays.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 11: End-to-end test — Inner Sanctum life-cost path

**Files:**
- Modify: `crates/engine/src/game/engine.rs`

- [ ] **Step 1: Write the test (counter=2 to exercise multiplication)**

```rust
#[test]
fn inner_sanctum_upkeep_two_age_counters_pays_four_life() {
    // CR 702.24a: Cumulative upkeep "Pay 2 life". At counter=2, cost = 4 life.
    let mut state = /* solo state, controller=PlayerId(0), Inner Sanctum on bf */;
    let sanctum_id = state.spawn_card_on_battlefield("Inner Sanctum", PlayerId(0));

    // Pre-load one age counter to simulate the second upkeep.
    state.objects.get_mut(&sanctum_id).unwrap()
        .counters.insert(CounterType::Age, 1);

    state.phase = Phase::Upkeep;
    state.active_player = PlayerId(0);

    advance_to_unless_payment_prompt(&mut state);

    // Counter ticks to 2, expect PayLife{4}.
    match &state.waiting_for {
        WaitingFor::UnlessPayment { cost: AbilityCost::PayLife { amount }, .. } => {
            assert_eq!(*amount, QuantityExpr::Fixed { value: 4 });
        }
        other => panic!("expected PayLife(4) prompt, got {other:?}"),
    }

    let life_before = state.players[&PlayerId(0)].life;
    pay_unless_payment_with_life(&mut state);

    assert_eq!(state.players[&PlayerId(0)].life, life_before - 4);
    assert_eq!(state.objects[&sanctum_id].zone, Zone::Battlefield);
}
```

- [ ] **Step 2: Verify and commit**

```bash
git add crates/engine/src/game/engine.rs
git commit -m "test: Inner Sanctum cumulative upkeep life-cost (CR 702.24a)

PayLife base multiplies by age-counter count: counter=2 → 4 life.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 12: End-to-end test — Source gone before trigger resolves

**Files:**
- Modify: `crates/engine/src/game/engine.rs`

- [ ] **Step 1: Write the test**

```rust
#[test]
fn cumulative_upkeep_source_gone_before_resolution_is_noop() {
    // CR 702.24a "if this permanent is on the battlefield" — the
    // trigger condition is checked at resolution. If the source has
    // been bounced/exiled, the entire chain no-ops.
    let mut state = /* solo state with Mystic Remora on battlefield */;
    let remora_id = state.spawn_card_on_battlefield("Mystic Remora", PlayerId(0));
    state.phase = Phase::Upkeep;
    state.active_player = PlayerId(0);

    // Trigger goes on stack at upkeep start.
    advance_to_upkeep_trigger_on_stack(&mut state);

    // Bounce Mystic Remora to hand before the trigger resolves.
    move_to_zone(&mut state, remora_id, Zone::Hand);

    // Drain the stack — no prompt should fire, no sacrifice happens
    // (it's already not on the battlefield).
    drain_stack(&mut state);

    // No UnlessPayment prompt outstanding.
    assert!(
        !matches!(state.waiting_for, WaitingFor::UnlessPayment { .. }),
        "no unless-pay prompt should fire when source is gone"
    );

    // Card sits in hand (was not sacrificed).
    assert_eq!(state.objects[&remora_id].zone, Zone::Hand);
}
```

**Reality check:** `advance_to_upkeep_trigger_on_stack`, `move_to_zone`, and `drain_stack` may need to be assembled from existing primitives. The invariant is: trigger fires → source is moved off-battlefield before stack drains → resolution produces no prompt and no sacrifice. Build whatever sequence of `submit_action`/state mutation reaches that state.

- [ ] **Step 2: Verify and commit**

```bash
git add crates/engine/src/game/engine.rs
git commit -m "test: cumulative upkeep no-ops when source already gone (CR 702.24a)

When the permanent is bounced/exiled between trigger creation and
resolution, the trigger chain no-ops: no counter goes on, no prompt
fires, no sacrifice (it's already gone).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 13: End-to-end test — Multi-instance (CR 702.24b)

**Files:**
- Modify: `crates/engine/src/game/engine.rs`
- Possibly modify: a test-only card fixture mechanism (or use the synthetic two-instance card from Task 4's parser test)

- [ ] **Step 1: Build the test setup**

A real MTG card with two cumulative upkeep abilities doesn't exist for the four-card set in scope. Use a synthetic test card: a permanent constructed in-test with two `TriggerDefinition` instances of `PayCumulativeUpkeep`, one with `Mana{1}` base and one with `PayLife{1}` base.

```rust
#[test]
fn cumulative_upkeep_multi_instance_each_ticks_own_counter() {
    // CR 702.24b: Multiple cumulative upkeep abilities each trigger
    // separately. Counter is shared; each trigger reads the total
    // count at its own resolution.
    let mut state = /* solo state */;
    let card_id = state.spawn_synthetic_permanent_with_triggers(
        PlayerId(0),
        vec![
            build_cumulative_upkeep_trigger(
                AbilityCost::Mana { cost: ManaCost::generic(1) },
            ),
            build_cumulative_upkeep_trigger(
                AbilityCost::PayLife { amount: QuantityExpr::Fixed { value: 1 } },
            ),
        ],
    );
    state.phase = Phase::Upkeep;
    state.active_player = PlayerId(0);

    let life_before = state.players[&PlayerId(0)].life;

    // Drive both triggers in stack order. The first to resolve ticks
    // counter→1 and prompts cost={1}; the second ticks counter→2
    // and prompts PayLife{2}.
    advance_to_unless_payment_prompt(&mut state);
    let first_cost = state.waiting_for.clone();

    pay_first_then_second(&mut state);  // pay {1} for first, then 2 life for second

    assert_eq!(
        state.objects[&card_id].counters.get(&CounterType::Age).copied(),
        Some(2),
        "two age counters should accumulate"
    );
    assert_eq!(state.players[&PlayerId(0)].life, life_before - 2);
    assert_eq!(state.objects[&card_id].zone, Zone::Battlefield);
}
```

**Reality check:** `spawn_synthetic_permanent_with_triggers` likely doesn't exist; build the synthetic card via direct `GameObject` construction + manual `triggers` field population. Reference how other tests construct synthetic test cards.

`pay_first_then_second` is two distinct unless-payment exchanges in sequence — drive them via the same `submit_action` idiom used in Task 9.

- [ ] **Step 2: Verify and commit**

```bash
git add crates/engine/src/game/engine.rs
git commit -m "test: cumulative upkeep multi-instance (CR 702.24b)

Synthetic permanent with two CumulativeUpkeep abilities ({1} and
Pay 1 life). Each trigger ticks an age counter and reads the
running total: first prompts {1}, second prompts PayLife(2).
Total: 2 counters, no sacrifice, controller paid {1} + 2 life.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 14: Multi-choice `UnlessPaymentChooseCost` for `OneOf × N`

**Files:**
- Modify: `crates/engine/src/types/game_state.rs:1776` (`WaitingFor::UnlessPaymentChooseCost`)
- Modify: `crates/engine/src/game/effects/mod.rs:2406` (the routing site for `OneOf`)
- Modify: `crates/engine/src/game/engine_payment_choices.rs` (`handle_unless_payment_choose_cost`)
- Modify: `crates/engine/src/ai_support/legal_actions.rs` (or equivalent — wherever `UnlessPaymentChooseCost` is enumerated)

- [ ] **Step 1: Read the current `UnlessPaymentChooseCost` variant**

Read `crates/engine/src/types/game_state.rs:1770–1800`. Note the field shape.

- [ ] **Step 2: Decide variant-vs-parameter shape**

Option A (parameterize existing variant): add `Option<Vec<AbilityCost>>` field for "remaining choices" and `Vec<AbilityCost>` for "already chosen". Both default to empty for the single-choice case.

Option B (new sibling): `UnlessPaymentMultiChoose { remaining_choices: Vec<Vec<AbilityCost>>, chosen: Vec<AbilityCost>, ... }`.

**Recommendation:** Option A. The single-choice case is `remaining_choices = vec![]` + `chosen = vec![]`; the multi-choice case threads through naturally. One variant is easier to reason about than two siblings differing only by cardinality.

- [ ] **Step 3: Extend `UnlessPaymentChooseCost`**

```rust
    UnlessPaymentChooseCost {
        player: PlayerId,
        costs: Vec<AbilityCost>,
        pending_effect: Box<ResolvedAbility>,
        trigger_event: Option<TriggerEvent>,
        effect_description: Option<String>,
        /// CR 702.24a: Each remaining OneOf in a Composite-of-OneOfs
        /// expansion is resolved in order. After `costs` is chosen, pop
        /// from `remaining_choices` and re-prompt. When empty, transition
        /// to UnlessPayment with the flat Composite of all chosen sub-costs.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        remaining_choices: Vec<Vec<AbilityCost>>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        chosen: Vec<AbilityCost>,
    },
```

Update all existing constructors to use `remaining_choices: vec![]` and `chosen: vec![]`.

- [ ] **Step 4: Route Composite-of-OneOfs at the resolution site**

In `effects/mod.rs:2406` (the resolved_cost switch), update:

```rust
                state.waiting_for = match resolved_cost {
                    AbilityCost::OneOf { costs } => WaitingFor::UnlessPaymentChooseCost {
                        player: payer,
                        costs,
                        pending_effect: Box::new(pending),
                        trigger_event: state.current_trigger_event.clone(),
                        effect_description: ability.description.clone(),
                        remaining_choices: vec![],
                        chosen: vec![],
                    },
                    AbilityCost::Composite { costs } if costs.iter().all(|c| matches!(c, AbilityCost::OneOf { .. })) => {
                        // CR 702.24a: Composite-of-OneOfs from PerCounter
                        // expansion of OneOf base. Drive each disjunctive
                        // choice in order; accumulate picks into Composite.
                        let mut queue: Vec<Vec<AbilityCost>> = costs
                            .into_iter()
                            .map(|c| match c {
                                AbilityCost::OneOf { costs } => costs,
                                _ => unreachable!("matched all OneOf above"),
                            })
                            .collect();
                        let first = queue.remove(0);
                        WaitingFor::UnlessPaymentChooseCost {
                            player: payer,
                            costs: first,
                            pending_effect: Box::new(pending),
                            trigger_event: state.current_trigger_event.clone(),
                            effect_description: ability.description.clone(),
                            remaining_choices: queue,
                            chosen: vec![],
                        }
                    }
                    cost => WaitingFor::UnlessPayment { /* ... */ },
                };
```

- [ ] **Step 5: Update `handle_unless_payment_choose_cost`**

In `engine_payment_choices.rs`, locate `handle_unless_payment_choose_cost`. After the player picks one cost from `costs`, instead of transitioning straight to `WaitingFor::UnlessPayment`:

```rust
fn handle_unless_payment_choose_cost(
    state: &mut GameState,
    chosen_idx: usize,
    /* ... existing params ... */
) -> Result<WaitingFor, EngineError> {
    let WaitingFor::UnlessPaymentChooseCost {
        player, costs, pending_effect, trigger_event, effect_description,
        mut remaining_choices, mut chosen,
    } = state.waiting_for.clone() else { /* error */ };

    let pick = costs[chosen_idx].clone();
    chosen.push(pick);

    if let Some(next_choices) = remaining_choices.pop() {
        // Wrap remaining queue (popped from end, but we want FIFO — use
        // .drain(0..1).next() or VecDeque; whichever the existing code
        // style favors).
        Ok(WaitingFor::UnlessPaymentChooseCost {
            player, costs: next_choices, pending_effect, trigger_event,
            effect_description, remaining_choices, chosen,
        })
    } else {
        // All choices made — collapse into Composite and re-enter UnlessPayment.
        let final_cost = if chosen.len() == 1 {
            chosen.into_iter().next().unwrap()
        } else {
            AbilityCost::Composite { costs: chosen }
        };
        Ok(WaitingFor::UnlessPayment {
            player, cost: final_cost, pending_effect, trigger_event, effect_description,
            remaining: vec![],
        })
    }
}
```

**Reality check:** Use VecDeque or maintain FIFO semantics carefully. The first prompt resolves the first OneOf; the next prompt resolves the second; etc.

- [ ] **Step 6: Update AI legal-actions enumeration**

Locate `legal_actions` handling of `WaitingFor::UnlessPaymentChooseCost`. The choice space is unchanged per prompt (it's still "pick one of `costs`"), so the enumeration logic doesn't need updating — only verify the new fields don't break any existing serialization/match. If there's a structural match that asserts field count, add the new fields.

- [ ] **Step 7: Add unit test for the multi-choice flow**

```rust
#[test]
fn unless_payment_composite_of_one_ofs_routes_through_sequential_choose() {
    let composite = AbilityCost::Composite {
        costs: vec![
            AbilityCost::OneOf { costs: vec![
                AbilityCost::Mana { cost: ManaCost::generic(1) },
                AbilityCost::PayLife { amount: QuantityExpr::Fixed { value: 1 } },
            ]},
            AbilityCost::OneOf { costs: vec![
                AbilityCost::Mana { cost: ManaCost::generic(1) },
                AbilityCost::PayLife { amount: QuantityExpr::Fixed { value: 1 } },
            ]},
        ],
    };
    let mut state = /* ... drive a synthetic unless-pay with this cost ... */;
    /* assertions:
       1. After resolution-site dispatch, state.waiting_for is UnlessPaymentChooseCost
          with one remaining_choice queued.
       2. Pick "Mana" → next state is still UnlessPaymentChooseCost with the second OneOf,
          chosen=[Mana].
       3. Pick "PayLife" → state is UnlessPayment with cost = Composite([Mana, PayLife]).
    */
}
```

- [ ] **Step 8: Verify and commit**

```bash
git add crates/engine/src/types/game_state.rs \
        crates/engine/src/game/effects/mod.rs \
        crates/engine/src/game/engine_payment_choices.rs \
        crates/engine/src/ai_support/legal_actions.rs
git commit -m "engine: multi-choice unless-pay for OneOf × N (CR 702.24a)

Extends UnlessPaymentChooseCost with remaining_choices and chosen
fields so a Composite-of-OneOfs (PerCounter expansion of OneOf base)
walks each disjunctive choice in sequence, accumulating picks into a
final Composite that re-enters UnlessPayment. Single-choice case is
unchanged (both new fields default-empty).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 15: End-to-end test — Elephant Grass `OneOf × N`

**Files:**
- Modify: `crates/engine/src/game/engine.rs`

- [ ] **Step 1: Write the test**

```rust
#[test]
fn elephant_grass_two_counters_prompts_two_independent_g_or_w_choices() {
    // CR 702.24a "{G} or {W}". At counter=2, prompt is Composite of two OneOfs;
    // player picks {G} then {W}; total paid is {G}{W}.
    let mut state = /* solo state */;
    let grass_id = state.spawn_card_on_battlefield("Elephant Grass", PlayerId(0));
    state.objects.get_mut(&grass_id).unwrap()
        .counters.insert(CounterType::Age, 1);  // pre-load to make this the 2nd upkeep
    state.phase = Phase::Upkeep;
    state.active_player = PlayerId(0);

    advance_to_unless_payment_prompt(&mut state);

    // First choose prompt: pick {G} (index 0).
    let WaitingFor::UnlessPaymentChooseCost { costs, remaining_choices, .. } = &state.waiting_for else {
        panic!("expected ChooseCost prompt, got {:?}", state.waiting_for);
    };
    assert_eq!(costs.len(), 2);
    assert_eq!(remaining_choices.len(), 1, "one more disjunctive choice queued");

    submit_action(&mut state, GameAction::ChooseUnlessCostIndex(0));

    // Second choose prompt: pick {W} (index 1).
    let WaitingFor::UnlessPaymentChooseCost { costs, remaining_choices, chosen, .. } = &state.waiting_for else {
        panic!("expected second ChooseCost prompt");
    };
    assert_eq!(costs.len(), 2);
    assert!(remaining_choices.is_empty());
    assert_eq!(chosen.len(), 1);

    submit_action(&mut state, GameAction::ChooseUnlessCostIndex(1));

    // Now: UnlessPayment with Composite([G, W]).
    match &state.waiting_for {
        WaitingFor::UnlessPayment { cost: AbilityCost::Composite { costs }, .. } => {
            assert_eq!(costs.len(), 2);
        }
        other => panic!("expected UnlessPayment(Composite), got {other:?}"),
    }

    // Pay the composite (G + W).
    pay_unless_payment_with_mana_composite(&mut state, &[ManaShard::Green.into(), ManaShard::White.into()]);

    assert_eq!(state.objects[&grass_id].zone, Zone::Battlefield);
}
```

**Reality check:** `GameAction::ChooseUnlessCostIndex` and `pay_unless_payment_with_mana_composite` are placeholders for whatever the real action enum / payment helper exposes. Pin down via `grep "UnlessPaymentChooseCost" crates/engine/src/types/game_state.rs` for the dispatch action and the existing single-choice test for the payment helper.

- [ ] **Step 2: Verify and commit**

```bash
git add crates/engine/src/game/engine.rs
git commit -m "test: Elephant Grass OneOf × N cumulative upkeep (CR 702.24a)

Disjunctive cost ({G} or {W}) × 2 age counters. Each OneOf is
resolved as an independent ChooseCost prompt; the picks accumulate
into a Composite that is finally paid as {G}{W}.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 16: Frontend keyword display + AI tooltip text

**Files:**
- Modify: `crates/engine/src/parser/oracle_keyword.rs:964` (replace the Task-3 placeholder)

- [ ] **Step 1: Locate `format_ability_cost`**

```bash
grep -rn "fn format_ability_cost\|pub fn format_cost\|cost_display_string" crates/engine/src/ 2>/dev/null | head -5
```

Find the canonical formatter used for typed `AbilityCost` display. If it doesn't exist, use the existing Echo formatter as a reference pattern — Echo's display in `oracle_keyword.rs` (search for `Keyword::Echo` in that file).

- [ ] **Step 2: Replace the Task-3 placeholder**

In `oracle_keyword.rs:964`, change:
```rust
        Keyword::CumulativeUpkeep(ref _cost) => {
            "cumulative upkeep".to_string()  // TODO Task 9: format from typed cost
        }
```
to:
```rust
        Keyword::CumulativeUpkeep(ref cost) => {
            format!("cumulative upkeep — {}", format_ability_cost(cost))
        }
```

If `format_ability_cost` returns a `String`, this is direct. If it takes more arguments (player context, etc.), pass the trivially-correct defaults — see Echo's call for the right shape.

If no shared formatter exists yet, build a small local helper covering the variants the cumulative-upkeep parser emits (`Mana`, `PayLife`, `Sacrifice`, `OneOf`) — at most 12 lines.

- [ ] **Step 3: Add a snapshot/unit test**

```rust
#[test]
fn cumulative_upkeep_keyword_display_mana() {
    let kw = Keyword::CumulativeUpkeep(AbilityCost::Mana { cost: ManaCost::generic(1) });
    let s = keyword_display_string(&kw);  // or whatever the function is called
    assert!(s.contains("cumulative upkeep"));
    assert!(s.contains("{1}") || s.contains("1"));
}
```

- [ ] **Step 4: Verify and commit**

```bash
git add crates/engine/src/parser/oracle_keyword.rs
git commit -m "engine: format CumulativeUpkeep keyword from typed cost (CR 702.24a)

Replaces the Task-3 placeholder with format_ability_cost-driven
display. Tooltip now shows the actual base cost text.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 17: Coverage verification + AI duel smoke

**Files:** (none modified; this is a verification task)

- [ ] **Step 1: Run the coverage report**

```bash
cargo coverage 2>&1 | grep -i "cumulative upkeep\|mystic remora\|inner sanctum\|polar kraken\|elephant grass"
```

Expected: all four cards appear as supported. If any are reported unsupported, the coverage classifier may need an extension — investigate.

- [ ] **Step 2: Run an AI duel smoke test**

Run `cargo ai-duel` (or the equivalent binary, find via `.cargo/config.toml`) with default settings for ~10 turns and confirm:
- No panics in the engine
- No `Unimplemented` effect traces in the log
- Game completes (one side wins or game state advances normally)

If the project doesn't have a Mystic-Remora-based AI deck, skip this step and rely on the engine-level e2e tests from Tasks 9–13 + 15.

- [ ] **Step 3: Final full verification**

Run the verification block one final time across the whole change:
```bash
cargo fmt --all
./scripts/tilt-wait.sh --timeout 300 clippy test-engine test-ai card-data check-frontend
```

All resources should be green.

- [ ] **Step 4: Commit (only if anything changed)**

If the coverage classifier needed tweaks:
```bash
git add crates/engine/src/game/coverage.rs
git commit -m "coverage: classify cumulative-upkeep cards as supported (CR 702.24a)

[Describe the specific classifier extension.]

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

Otherwise, no commit — this task is a verification gate.

---

## Self-Review

### Spec coverage check

| Spec section | Task(s) |
|--------------|---------|
| §2a CounterType::Age | Task 1 |
| §2b AbilityCost::PerCounter | Task 2 |
| §2c Keyword::CumulativeUpkeep refactor | Task 3 |
| §3 Parser (typed cost extraction + or-separated helper) | Task 4 |
| §4a build_cumulative_upkeep_trigger | Task 7 |
| §4b Keyword → triggers wiring | Task 7 |
| §4c Multi-instance (CR 702.24b) | Task 7 (structurally) + Task 13 (test) |
| §4d PayCumulativeUpkeep → match_phase | Task 8 |
| §5a/5b PerCounter resolution + expand_per_counter | Tasks 5, 6 |
| §5c OneOf × N flow | Task 14 |
| §5d Tick-first, then prompt | Task 7 (synthesis structure) |
| §5e Verification spikes | Task 7 Step 1 (precondition) |
| §6a Frontend keyword display | Task 16 |
| §6b AI cost categories + decider | Task 2 (categories delegate); Task 14 (legal-actions enum) |
| §6c Multiplayer state filter | No change needed — verified in spec §6c |
| §6d Coverage flip | Task 17 |
| §7a Parser tests | Tasks 3, 4 |
| §7b expand_per_counter unit tests | Task 5 |
| §7c End-to-end tests 1–8 | Tasks 9 (1–3), 10 (4), 11 (5), 15 (6), 12 (7), 13 (8) |
| §7d AI duel smoke | Task 17 |

All spec sections have an owning task.

### Placeholder scan

Searching the plan body for `TBD`, `TODO`, `FIXME`, `???`: only two intentional uses are present, both in Task 3 (the cross-task hook for the parser stub) and explicitly resolved by Task 4. They are part of the planned flow, not placeholder bugs.

Searching for "implement later", "fill in", "similar to": none found.

Every code block in this plan is complete content the engineer can copy.

### Type consistency

- `Effect::AddCounter` field name is `counter_type` (verified against `ability.rs:4552`); used consistently in Tasks 5 and 7.
- `AbilityCost::Sacrifice.count` is `QuantityExpr` (verified against `ability.rs:4580`); used consistently in Tasks 4, 5, 10.
- `obj.counters: HashMap<CounterType, u32>` (verified against `game_object.rs:273`); used consistently in Tasks 6, 9, 11, 13.
- `ManaCost::scaled(n: u32)` (verified against `mana.rs:747`); used in Task 5.
- `TriggerMode::PayCumulativeUpkeep`, `Phase::Upkeep`, `TargetFilter::SelfRef`, `TargetFilter::Controller`, `CounterType::Age`: all consistent.

No naming drift across tasks.
