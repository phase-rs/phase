# MSH/MSC Wave 4 — Static Structure + New Filters — Implementation Plan

**Scope:** 3 cards on the `main` checkout, one coherent wave.
**Cards:** Dragon Man, Reformed Robot (Cluster C, CDA) · Wolverine, Claws Out (Cluster C, combat-damage assignment) · Ms. Marvel, Elastic Ally (Cluster H, new FilterProp).
**mtgish:** OUT OF BOUNDS. No step in this plan touches `mtgish/`, `crates/mtgish-import/`, or `data/mtgish-*`. No mtgish mirroring is required for any of these changes.
**Skills applied:** `/add-static-ability`, `/add-trigger`, `/add-engine-variant`, `/oracle-parser`.

---

## Headline result

All three cards have **`gap_count == 1`** in `client/public/coverage-data.json` — every sibling clause already parses. This wave is far smaller than the master wave plan assumed:

| Card | Master-plan assumption | Verified reality | New engine types |
|------|------------------------|------------------|------------------|
| Dragon Man | "likely new `QuantityRef` variant `MaxManaCost{…}`" | **Parser-only.** All primitives exist; `QuantityRef::Aggregate` is already zone-general | **0** |
| Wolverine | "check for `CombatDamageScope::AsThoughUnblocked`" | **Parser-only.** Full primitive exists & is tested; gap is gendered pronouns ("his"/"he") | **0** |
| Ms. Marvel | "new `FilterProp::PowerExceedsBase`" | **Confirmed:** one new `FilterProp` self-comparison variant | **1** |

So this wave adds **exactly one** engine enum variant (`FilterProp::PowerExceedsBase`); the other two cards are pure parser compositions over existing building blocks.

---

## Per-clause ground truth (from `coverage-data.json` `parse_details`)

**Dragon Man, Reformed Robot** (`supported=False`, `gap_count=1`)
- `keyword "Flying"` → **True** ✓
- `ability "static_structure"` → **False** ← *the only gap* — src `"~'s power is equal to the greatest mana value among noncreature permanents you control and noncreature cards in your graveyard"`
- `static "GraveyardCastPermission(Cast,unlimited)"` → **True** ✓ ("You may cast this card from your graveyard by discarding a card…")

**Wolverine, Claws Out** (`supported=False`, `gap_count=1`)
- `ability "static_structure"` → **False** ← *the only gap* — src `"You may have ~ assign his combat damage as though he weren't blocked."`
- `trigger "Attacks"` → **True** ✓ ("Whenever a Mutant you control attacks, double its power until end of turn.")

**Ms. Marvel, Elastic Ally** (`supported=False`, `gap_count=1`)
- `keyword "Reach"` → **True** ✓
- `trigger "ChangesZone"` (+ child `ability "Pump"`) → **True** ✓ ("When ~ enters, target creature gets +2/+0…")
- `trigger "Whenever a creature you control with power greater than its base power deals combat damage to a player…"` → **False** ← *the only gap*

---

# CR Annotations (grep-verified against `docs/MagicCompRules.txt`)

Every number below was confirmed by `grep -nE "^<num>" docs/MagicCompRules.txt`. Matched text pasted.

**CR 604.3** (CDA definition):
> 604.3. Some static abilities are characteristic-defining abilities. … Characteristic-defining abilities function in all zones. They also function outside the game and before the game begins.

**CR 208.2a** (the exact "[creature]'s power is equal to…" CDA wording):
> 208.2a The card may have a characteristic-defining ability that sets its power and/or toughness according to some stated condition. (See rule 604.3.) Such an ability is worded "[This creature's] [power or toughness] is equal to . . ." … If the ability needs to use a number that can't be determined, including inside a calculation, use 0 instead of that number.

*(The "use 0" clause is exactly what the existing `Aggregate` resolver implements via `.max().unwrap_or(0)` — quantity.rs:1785.)*

**CR 613.4a** (CDA P/T layer):
> 613.4a Layer 7a: Effects from characteristic-defining abilities that define power and/or toughness are applied. See rule 604.3.

**CR 613.4b** (base power / set layer — defines "base power"):
> 613.4b Layer 7b: Effects that set power and/or toughness to a specific number or value are applied. Effects that refer to the base power and/or toughness of a creature apply in this layer.

**CR 613.4c** (modify P/T — pumps/counters):
> 613.4c Layer 7c: Effects and counters that modify power and/or toughness (but don't set power and/or toughness to a specific number or value) are applied.

**CR 202.3** (mana value):
> 202.3. The mana value of an object is a number equal to the total amount of mana in its mana cost, regardless of color.

**CR 510.1c** (blocked creature combat-damage assignment — the rule the "as though it weren't blocked" effect modifies):
> 510.1c A blocked creature assigns its combat damage to the creatures blocking it. … If exactly one creature is blocking it, it assigns all its combat damage to that creature. …

**CR 510.1b** (unblocked assignment — the behavior Wolverine borrows while remaining blocked):
> 510.1b An unblocked creature assigns its combat damage to the player, planeswalker, or battle it's attacking. …

**CR 208.1** (power) — for the new FilterProp:
> 208.1. A creature card has two numbers separated by a slash printed in its lower right corner. The first number is its power … Power and toughness can be modified or set to particular values by effects.

> ⚠️ Recurring-hallucination guard: the master plan/casual memory referenced "509.1c"/"604.3b". 509.1c is *declare-blockers requirements*, **not** damage assignment; the correct damage rules are **510.1b/510.1c**. There is no 604.3b subpart governing this. Verified by grep.

---

# Card 1 — Dragon Man, Reformed Robot (CDA)

### Root cause
Oracle: `Flying` · **`Dragon Man's power is equal to the greatest mana value among noncreature permanents you control and noncreature cards in your graveyard.`** · graveyard-cast permission. Self-ref-normalized (CR 201.4): `~'s power is equal to the greatest mana value among noncreature permanents you control and noncreature cards in your graveyard.`

Falls to `Effect::unimplemented("static_structure")` for two parser reasons:
1. **No dispatch arm** converts the possessive-CDA form `~'s power is equal to <quantity>` into a `SetDynamicPower` CDA `StaticDefinition`. (`oracle_classifier.rs:266` lists `"power is equal to"` but the line is not turned into a CDA — it hits the `static_structure` strict-failure marker.)
2. The quantity `"the greatest mana value among <A> and <B>"` is a **two-filter conjunction spanning two zones**. The existing `tag("the greatest mana value among ")` (oracle_quantity.rs:246-247, 301) consumes a *single* trailing filter and emits one `Aggregate`; it has no "… **and** <second filter>" tail.

### Engine types — NONE NEW (add-engine-variant Stage 1 = EXISTS for every primitive)
- `ContinuousModification::SetDynamicPower { value: QuantityExpr }` — ability.rs:15507, **"Set power to a dynamically computed value (CDA, layer 7a)."** Applied at layers.rs:4002 (`obj.power = Some(val)`); resolved with full `GameState` at layer-eval time (layers.rs:3575-3648, `resolve_quantity`).
- `QuantityRef::Aggregate { function, property, filter }` — ability.rs:3765, CR 107.3e. `AggregateFunction::Max` (ability.rs:4214) + `ObjectProperty::ManaValue` (ability.rs:4236) exist.
- `QuantityExpr::Max { exprs }` — resolver fold at quantity.rs:963 (`.max().unwrap_or(0)`).
- **Zone-generality proof (decisive):** `Aggregate` resolves via `object_count_matching_ids` (quantity.rs:1759-1764) which scans `filter.extract_zones()` (quantity.rs:1361, default `[Battlefield]`). `extract_zones()` (ability.rs:10335-10371) returns `[Graveyard]` from `FilterProp::InZone{Graveyard}` and **unions zones across `TargetFilter::Or`**. Graveyard cards live in `state.objects` (`zone_object_ids(state, Graveyard)`); the extractor reads `obj.mana_cost.mana_value_with_x(obj.zone, …)` in any zone (quantity.rs:1771). **So `Aggregate{Max,ManaValue,…}` already aggregates over the graveyard.** The recon agent's "needs a new `ZoneCardAggregate`" was wrong — it trusted the stale `// battlefield objects` doc-comment and never traced `object_count_matching_ids`/`extract_zones`.

### Chosen representation (existing primitives only)
```
StaticDefinition {
  mode: Continuous,
  characteristic_defining: true,            // CR 604.3 / 208.2a — functions in all zones
  affected: Some(TargetFilter::SelfRef),
  modifications: [ SetDynamicPower { value: QuantityExpr::Max { exprs: [
    Ref(Aggregate{ Max, ManaValue,                         // battlefield arm
        Typed{ type_filters:[Non(Creature)], controller:Some(You) } }),          // no InZone → Battlefield
    Ref(Aggregate{ Max, ManaValue,                         // graveyard arm
        Typed{ type_filters:[Non(Creature)], properties:[InZone(Graveyard)], controller:Some(You) } }), // InZone → Graveyard
  ] } } ],
}
```
Toughness is the printed value (Oracle defines only power) — no toughness modification.

**Why `Max`-of-two-`Aggregate`s, not one `Aggregate` over a zone-spanning `Or`:** mirrors the documented disjoint-zone idiom on `QuantityExpr::Sum { exprs }` ("for each card in your hand **and** each foretold card in exile") — the parser already models "A and B across distinct zones" as two refs joined by a composition node. Each `Aggregate` then carries one unambiguous zone, so controller/owner semantics are clear per arm without relying on `Or`-filter controller rebinding across zones.

### Parser approach (nom combinators only — zero `contains`/`find`/`split_once`)
1. **`oracle_quantity.rs`** — extend the "greatest `<prop>` among" block (≈235-301): after the first filter, attempt `opt(preceded(tag(" and "), parse_target_filter))`; if a second filter parses, emit `QuantityExpr::Max{ exprs:[Aggregate(first), Aggregate(second)] }`, else the bare single `Aggregate`. Use existing `parse_target_filter`/static-subject filter combinators per arm so "noncreature permanents you control" and "noncreature cards in your graveyard" decode through the shared filter grammar (incl. `in your graveyard` → `InZone{Graveyard}`). Generalizes "greatest power/toughness/mana value among A and B" to the whole class.
2. **`oracle_static.rs` (+ `oracle_static/` dispatch)** — add/extend a CDA arm for `~'s <power|toughness> is equal to <cda_quantity>` → `StaticDefinition::continuous().characteristic_defining(true).affected(SelfRef).modifications([SetDynamicPower{ parse_cda_quantity(rhs) }])`. RHS routes through existing `parse_cda_quantity` (oracle_quantity.rs:619), which now yields the `Max`-of-`Aggregate`s expression. Sketch: strip self-possessive prefix, `alt((value(Power, tag("power")), value(Toughness, tag("toughness"))))`, `tag(" is equal to ")`, `parse_cda_quantity(rest)`. **First confirm** whether the existing "is equal to" → `parse_cda_quantity` path (oracle_target.rs:3745 / oracle_util.rs:508) already handles possessive-self CDAs — if so, Dragon Man collapses to edit #1 only. Keep the `static_structure` strict marker for genuinely-unhandled static forms.

### Registration-point checklist (/add-static-ability)
- [ ] Types: none. Layers: none (SetDynamicPower apply + dynamic resolution exist). Quantity resolver: none (`Aggregate`+`Max` resolved).
- [ ] Parser: `oracle_quantity.rs` "A and B" tail; `oracle_static` possessive-CDA dispatch; ensure classifier routes "…'s power is equal to…" to it and stops emitting `static_structure`.
- [ ] Frontend/AI: none (power renders from engine field; `*` display supported).
- [ ] Doc fix: correct the `QuantityRef::Aggregate` "battlefield objects" comment to "objects in the zones the filter declares (battlefield by default)".

### Discriminating test (add_real_card + rehydrate — NOT `from_oracle_text`)
Dragon Man on battlefield; control a MV-5 noncreature permanent; graveyard has a MV-7 noncreature card + a MV-9 *creature* card; `evaluate_layers()` → assert `power == 7` (creature card excluded; bf 5 < gy 7). Building-block assertions: (a) bf-only max 4, empty gy → 4; (b) empty board+gy → 0 (CR 208.2a); (c) add +1/+1 counter → power 8 (7 from 7a + 1 from 7e), proving the CDA is the base. Revert-failing: remove the parser arm → clause `Unimplemented`, creature keeps printed power.

---

# Card 2 — Wolverine, Claws Out (combat damage as though unblocked)

### Root cause
Oracle line 1: `You may have Wolverine assign his combat damage as though he weren't blocked.` → `You may have ~ assign his combat damage as though he weren't blocked.`

The **entire primitive already exists and is tested** (recon-confirmed): `ContinuousModification::AssignDamageAsThoughUnblocked` (ability.rs:15606, inventory CR 510.1c); `GameObject.assigns_damage_as_though_unblocked: bool` (game_object.rs:851); layer-6 apply (layers.rs:2853); combat resolution (combat_damage.rs:523-527 decision, 621-628 mode offer, 631-644 `assign_damage_as_though_unblocked`) offering `CombatDamageAssignmentMode::{Normal, AsThoughUnblocked}`; parser `parse_assign_damage_as_though_unblocked` (oracle_static/evasion.rs:1676-1702, dispatched dispatch.rs:986); Thorn Elemental tests (combat_damage.rs:1294-1368).

**The gap is gendered pronouns.** The tag is hard-coded neuter:
```
alt((tag("this creature"), tag("~"), tag("it")))               // subject
tag(" assign its combat damage as though it weren't blocked")  // its / it
```
Wolverine uses `his`/`he` → tag fails → falls to `static_structure`. Not a missing primitive; a pronoun-coverage gap in one combinator. (Apostrophe: card-data uses straight `'` in `weren't`, matching the tag — no normalization needed.)

### Engine types — NONE NEW (Stage 1 = EXISTS).

### Parser approach (nom; build for the gendered-character class)
Replace the literal `its`/`it` in `parse_assign_damage_as_though_unblocked` (and the attached-creature twin, evasion.rs:1708-1742) with shared pronoun combinators:
- possessive: `alt((tag("its"), tag("his"), tag("her"), tag("their")))`
- nominative: `alt((tag("it"), tag("he"), tag("she"), tag("they")))`
```
preceded(tag("you may have "), alt((tag("this creature"), tag("~"), tag("it"))))
tag(" assign "); possessive_pronoun; tag(" combat damage as though "); subject_pronoun; tag(" weren't blocked")
```
Place the shared pronoun combinator in `oracle_nom/` (or reuse existing). **First check** whether `parse_self_or_object_pronoun` (oracle_util.rs:828) or the `OBJECT_PRONOUNS`/`POSSESSIVES` constant sets (oracle_util.rs:761-798) already enumerate `his/her/he/she`; if so, route the combinator through them rather than re-listing. Do **not** attempt a global his→its normalization pass in this wave (broad blast radius — separate change).

### Registration-point checklist
- [ ] Types/layers/combat/frontend/AI: none. Parser: widen the two `evasion.rs` combinators; add parser unit tests for gendered forms.

### Discriminating test
1. **Parser unit** (evasion.rs): `parse_assign_damage_as_though_unblocked("you may have ~ assign his combat damage as though he weren't blocked")` → `Some(StaticDefinition{ modifications:[AssignDamageAsThoughUnblocked], affected:SelfRef })`; sibling `her`/`she`; regression `its`/`it`. Revert-failing: without widening, gendered form → `None`.
2. **Runtime** (add_real_card + rehydrate): Wolverine attacks, blocked by one creature → engine offers `AsThoughUnblocked`; choosing it assigns Wolverine's power to the defending player (mirror Thorn Elemental tests at combat_damage.rs:1294-1368 with Wolverine's real card so it drives the card-data parse path).

---

# Card 3 — Ms. Marvel, Elastic Ally (new FilterProp)

### Root cause
Oracle: `Reach` · ETB pump · **`Whenever a creature you control with power greater than its base power deals combat damage to a player, draw a card. This ability triggers only once each turn.`** Reach + ETB pump already parse. The `DamageDone` trigger event ("a creature you control deals combat damage to a player"), "draw a card", and `OncePerTurn` all exist. **Only unrecognized fragment:** the source filter `with power greater than its base power` (self-comparison: effective power > base power).

### Engine type — ONE NEW variant (via `/add-engine-variant` gate)
**Stage 1 — Existence (5-grep):** no `PowerExceedsBase`. Nearest: `FilterProp::ToughnessGTPower` (ability.rs:2568, same-object stat-vs-stat), `PowerGTSource` (2504, cross-object candidate-vs-source), `PtComparison{stat,scope,comparator,value}` (2495, stat vs *threshold value*), `Modified` (2598, has-counter/aura/equipment — **not** "power>base", includes toughness-only/no-pump modifiers). None expresses current-power > own-base-power → **DOES_NOT_EXIST.**

**Stage 2 — Parameterization:** same-object self-comparison cluster is `{ToughnessGTPower}` only; adding this makes **2** (`PowerGTSource` is cross-object, different axis). 2 < the 3+ sibling-cluster-smell threshold → **EXTEND_OK.** Future consolidation if a 3rd appears: `PtSelfComparison{ lhs:(stat,scope), comparator, rhs:(stat,scope) }` absorbing both. **Recommendation:** add dedicated `FilterProp::PowerExceedsBase` now (mirrors `ToughnessGTPower` 1:1; lowest risk). Parameterizing now would refactor `ToughnessGTPower`'s eval arm + zone-change-snapshot eval + parser arm — multi-site, concurrent-edit collision risk, marginal benefit at 2 siblings.

**Stage 3 — Categorical boundary:** axis = power vs base power, both CR 208 / 613.4b. Single section → **WITHIN_SECTION.**

> **APPROVED: `FilterProp::PowerExceedsBase`** (unit). Doc-comment: "CR 208.1 + CR 613.4b: matches a creature whose current (post-layer) power exceeds its base power (layer-7b baseline incl. CDA, before counters/pumps). Consolidation target if a 3rd same-object P/T self-comparison appears: `PtSelfComparison{lhs,comparator,rhs}`."

### Semantics — base vs effective
`obj.power` = effective (all layers) — game_object.rs:386. `obj.base_power` = layer-7b baseline (after CDA 7a + set 7b, before counters/modify 7c-7e) — game_object.rs:453, per CR 613.4b. So "power > base power" = pumped above base by counters/auras/anthems; for a CDA creature the CDA value is in `base_power`, a counter on top still satisfies — correct. Eval: `obj.power.unwrap_or(0) > obj.base_power.unwrap_or(0)` (mirrors `ToughnessGTPower`, filter.rs:3519-3524).

### Registration-point checklist (/add-trigger filter wiring)
- [ ] **`types/ability.rs`** — add `FilterProp::PowerExceedsBase` with CR-annotated doc-comment.
- [ ] **`game/filter.rs`** — eval arm in `matches_filter_prop` (≈2981-3632): `PowerExceedsBase => obj.power.unwrap_or(0) > obj.base_power.unwrap_or(0)`. **Also** add the arm in the zone-change-snapshot eval path (`ZoneChangeRecord` power/base_power, ≈3702-3777) for look-back correctness. No wildcard — let `cargo check -p engine` enumerate sites.
- [ ] **`parser/oracle_nom/filter.rs`** — `value(FilterProp::PowerExceedsBase, tag("power greater than its base power"))` in `parse_with_inner` (≈172-189), **before** `parse_pt_comparison` so the literal wins over generic numeric P/T parse.
- [ ] **Trigger plumbing** — none new (DamageDone + draw + OncePerTurn exist; prop lands in the source-creature `properties` vec).
- [ ] **Frontend/AI** — none. **engine-inventory** — auto-regenerated by `cargo engine-inventory`.
- [ ] **Exhaustiveness** — add arms in any other `FilterProp` match (serialization, coverage `is_data_carrying_*`) found via `cargo check -p engine`; no wildcard.

### Discriminating test (add_real_card + rehydrate)
Ms. Marvel out. Creature A (+1/+1 counter: power 3, base 2) and creature B (no mods: power 2, base 2) both deal combat damage to a player. Assert trigger fires for A (draw 1), not B; `OncePerTurn` caps at one draw. Building-block range: aura/anthem pump (7c) matches; -1/-1 counter (power<base) doesn't; CDA-no-counter (power==base) doesn't. Revert-failing: drop eval arm → always-false, no draw; drop parse arm → trigger `Unimplemented`, card unsupported.

---

# Architectural sections (mandatory)

### Pattern Coverage
- **Dragon Man:** "greatest `<prop>` among A and B" two-zone aggregate quantity + the possessive-CDA dispatch form `"[self]'s power is equal to <quantity>"` (CR 208.2a) — covers dozens of `*`/CDA creatures, not just Dragon Man.
- **Wolverine:** gendered-pronoun widening unlocks the whole "have [gendered character] assign his/her combat damage as though he/she weren't blocked" class and, via the shared combinator, any static currently hard-coding `its`/`it`.
- **Ms. Marvel:** `PowerExceedsBase` covers every "creature with power greater than its base power" filter (counters-/pump-matters template).

### Building Blocks (by name)
`SetDynamicPower`; `QuantityRef::Aggregate`; `QuantityExpr::Max`; `AggregateFunction::Max`; `ObjectProperty::ManaValue`; `TargetFilter::{Typed,Or,SelfRef}` + `TypeFilter::Non`; `FilterProp::InZone`; `object_count_matching_ids`+`extract_zones`; `parse_cda_quantity` + the "greatest…among" block; `parse_target_filter`; `parse_assign_damage_as_though_unblocked` + `AssignDamageAsThoughUnblocked` + `assigns_damage_as_though_unblocked`; `object_pt_value(obj,stat,Base)`; `matches_filter_prop`; `parse_with_inner`/`parse_pt_comparison`; `parse_self_or_object_pronoun`/`OBJECT_PRONOUNS`/`POSSESSIVES`. **Only new helper:** a shared gendered-pronoun nom combinator (removes the hard-coded neuter assumption across the static-parser family).

### Logic Placement
Dragon Man + Wolverine: parser-only (decode → existing typed AST). Ms. Marvel: type (ability.rs) + runtime eval (filter.rs, engine owns filter semantics) + parser decode (oracle_nom/filter.rs). No logic in transport/frontend.

### Rust Idioms
Typed enums, no bools (`PtValueScope`, `AggregateFunction`, `ObjectProperty`, `ControllerRef`, `QuantityExpr` nodes). Exhaustive `match`, no wildcard for the new `FilterProp` arm. `PowerExceedsBase` mirrors the established `ToughnessGTPower` sibling.

### Nom Compliance
All dispatch via `tag`/`alt`/`value`/`opt`/`preceded` and existing `parse_target_filter`/`parse_cda_quantity`/`parse_with_inner`. "A and B" tail = `opt(preceded(tag(" and "), parse_target_filter))`. Pronoun widening = `alt((tag…))`. `PowerExceedsBase` literal ordered before the generic numeric P/T combinator. No `contains`/`find`/`split_once`/`starts_with` for parsing.

### Extension vs Creation
Dragon Man + Wolverine extend existing parser branches (wider grammar, no new types/patterns). Ms. Marvel extends `FilterProp` with one sibling modeled on `ToughnessGTPower`. No new architecture.

### Analogous Traces
- Dragon Man: `parse_cda_quantity`/"greatest mana value among" (oracle_quantity.rs:246,301) → `Aggregate` (ability.rs:3765) → `object_count_matching_ids`+`extract_zones` (quantity.rs:1361 / ability.rs:10335) → `SetDynamicPower` (ability.rs:15507) → layer-7a apply (layers.rs:3575-3648,4002). Precedent: March of the Machines / Karn animate (oracle_static snapshot_tests.rs:127-223) using `SetPowerDynamic`/`ObjectManaValue{Recipient}`.
- Wolverine: `parse_assign_damage_as_though_unblocked` (evasion.rs:1676) → `AssignDamageAsThoughUnblocked` (ability.rs:15606) → layer apply (layers.rs:2853) → `assigns_damage_as_though_unblocked` (game_object.rs:851) → combat (combat_damage.rs:523-644). Reference: Thorn Elemental.
- Ms. Marvel: `parse_with_inner`/`parse_pt_comparison` (oracle_nom/filter.rs:172-251) → `FilterProp` (ability.rs:2280) → `matches_filter_prop`/`object_pt_value` (filter.rs:2885,2981-3632); modeled on `ToughnessGTPower` (filter.rs:3519).

### Variant Discoverability
`data/engine-inventory.json` consulted — confirms `AssignDamageAsThoughUnblocked`/CR 510.1c (line 5797), `Aggregate`, `SetDynamicPower`, `ManaValue`; confirms **no** `PowerExceedsBase`. `/add-engine-variant` run for the one new variant → APPROVED (Stage1 DOES_NOT_EXIST, Stage2 EXTEND_OK, Stage3 WITHIN_SECTION). Dragon Man + Wolverine variants are Stage1 EXISTS → wire to existing slots.

### Verification Matrix
| Claim | Changed seam | Production entry | Runtime test | Revert-failing | Sibling/negative | Coverage |
|------|------|------|------|------|------|------|
| Dragon Man CDA power | oracle_static CDA dispatch + oracle_quantity "A and B" | `evaluate_layers` | add_real_card, `power==7` | remove arm → printed power | bf-only=4; empty=0; +counter=8 | → supported |
| Wolverine unblocked assign | evasion.rs pronoun widening | combat damage step | parser unit + combat runtime | gendered → None/Unimplemented | `her`/`she` ✓; `its`/`it` regression | → supported |
| Ms. Marvel power>base | new `FilterProp` + filter.rs eval + parser arm | `process_triggers` DamageDone | add_real_card, A fires/B not, OncePerTurn=1 | drop eval → no draw | aura-pump ✓; -1/-1 ✗; CDA-no-counter ✗ | → supported |

No Oracle text is accepted while semantics remain deferred — each parser change wires a fully-evaluating primitive. Genuinely-unhandled static forms keep the `static_structure` strict-failure marker so coverage stays honest.

---

# Sequencing, collisions, and PR strategy

### File collisions within the wave
| File | Dragon Man | Wolverine | Ms. Marvel |
|------|:--:|:--:|:--:|
| `types/ability.rs` | — | — | ✎ |
| `parser/oracle_quantity.rs` | ✎ | — | — |
| `parser/oracle_static.rs` + `oracle_static/` | ✎ (CDA dispatch) | ✎ (evasion.rs) | — |
| `parser/oracle_nom/filter.rs` | — | — | ✎ |
| `game/filter.rs` | — | — | ✎ |

Only `oracle_static/` is touched by two cards, in **different files/functions** (Dragon Man CDA dispatch vs Wolverine `evasion.rs`). No shared-function collision. `types/ability.rs` touched only by Ms. Marvel.

### Sequencing (single `main` checkout, sequential — no concurrent edits)
1. **Wolverine** (smallest, isolated to `evasion.rs`) → 2. **Dragon Man** (oracle_quantity.rs + oracle_static dispatch) → 3. **Ms. Marvel** (types + filter.rs + oracle_nom/filter.rs). `cargo fmt --all` after edits. Verify via **Tilt** (`./scripts/tilt-wait.sh clippy test-engine card-data`), never raw cargo. Commit by **pathspec** only.

### One PR vs split — **Recommendation: ONE PR for the wave.**
- All three are single-clause fixes (`gap_count==1`) with their own discriminating tests; small review surface.
- The only shared-type file (`types/ability.rs`) is touched by exactly one card (Ms. Marvel) → no cross-card merge hazard.
- **Card-data regen is the dominating, wave-global cost:** parser fixes are inert until `card-data` regenerates; one regen + one CI/merge-queue cycle covers all three. Splitting triples regen+CI overhead for three tiny fixes.
- Fallback split line: if Ms. Marvel's `types/ability.rs` review stalls, peel **only Ms. Marvel** into its own PR and ship Dragon Man + Wolverine (pure parser, no shared files with Ms. Marvel) immediately.

---

# Risks & Open Questions
1. **Graveyard ownership (Dragon Man).** Confirm `controller: Some(You)` resolves for graveyard cards (control == owner). If owner semantics needed, use the established "in your graveyard" scope (`ZoneCardCount{scope:Controller}` convention) / `FilterProp::Owned`. Low risk; the graveyard-card test assertion catches it.
2. **Existing self-CDA dispatch reuse (Dragon Man).** If the "is equal to" → `parse_cda_quantity` path (oracle_target.rs:3745 / oracle_util.rs:508) already covers possessive-self CDAs, Dragon Man collapses to a single oracle_quantity.rs edit. Prefer that.
3. **Dedicated vs parameterized self-comparison (Ms. Marvel).** Plan recommends the dedicated `PowerExceedsBase` (2 siblings < threshold; mirrors `ToughnessGTPower`; avoids a multi-site refactor under concurrent edits). Reviewer may mandate `PtSelfComparison` instead — explicit decision point; categorical boundary (CR 208) holds either way.
4. **`Aggregate` doc-comment** says "battlefield objects" but the resolver is zone-general — correct it as part of Dragon Man (prevents the next agent repeating the recon error).
5. **Inert-until-regen.** All three are parser changes; `from_oracle_text` unit tests pass even with stale deployed data. Tests MUST use `add_real_card` + rehydrate to exercise the real card-data path; regenerate card-data before claiming "supported".
6. **No stop-and-return blockers.** No mtgish dependency, no missing infrastructure — all three implementable on `main` now.
