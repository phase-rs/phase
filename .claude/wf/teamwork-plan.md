# Implementation Plan — Teamwork (Marvel Super Heroes, MSH/MSC) + 17 spells

Plan only. No code written. Every CR number below was grep-verified against `docs/MagicCompRules.txt` (present locally).

---

## 0. Summary of the mechanic and the architectural verdict

**Teamwork N** = *"As an additional cost to cast this spell, you may tap any number of creatures you control with total power N or more."* It is an **optional additional cost** (CR 601.2b/f/h). If paid, the spell "was cast using teamwork," and conditional clauses on the card branch on that flag.

Teamwork is a structural **twin of Conspire** (`synthesize_conspire`), not of Kicker: both are an `AdditionalCost::Optional { cost: TapCreatures, .. }` plus effects gated on `SpellContext.additional_cost_paid`. The only thing Teamwork has that no existing mechanic has is a **total-power threshold** for the tap selection (Conspire taps an *exact count* of 2; Teamwork taps *any number* whose *summed power* ≥ N).

The "cast using teamwork" flag is **not** a new condition system. It is the existing `AbilityCondition::AdditionalCostPaid { source: Any }` (resolution-time) and `ModalSelectionCondition::AdditionalCostPaid { source: Any }` (modal upgrade), both of which already power "if this spell's additional cost was paid". The ENTIRE conditional-clause class for the 17 cards is already representable; the work is (a) one keyword variant, (b) one synthesis pass, (c) one new power-threshold tap-payment mode, and (d) parser surface-phrase recognition for "cast using teamwork" in the three condition sites that already exist for "kicked".

---

## 1. Pattern Coverage

**Class, not card.** Teamwork is a *named keyword mechanic*; the keyword + cost machinery covers **all 17 printed Teamwork spells** and any future Teamwork card with zero per-card code. The conditional-clause riders decompose into existing reusable classes:

| Conditional class | Cards | Existing machinery reused |
|---|---|---|
| **"choose both instead"** (modal cap upgrade) | Atlantis Attacks, Murdock's Crusade, HULK SMASH!, Go Nuts!, Widow's Bite (5) | `ModalSelectionConstraint::ConditionalMaxChoices { condition: ModalSelectionCondition::AdditionalCostPaid }` — already parses for "if this spell's additional cost was paid, choose both instead" (oracle.rs:10755 test) |
| **additive rider** ("also …", "draw a card", "it also deals 2 damage") | Beast Mode, Team Tactics, Repulsor Blast, Crossover Collaboration, Heroic Teamwork (5) | `sub_ability` gated by `AbilityCondition::AdditionalCostPaid { source: Any }` |
| **"instead" effect substitution** ("instead deals 4", "instead exile…and gain 3 life", "put any number…instead", "instead choose…") | Helicarrier Strike, Earth's Mightiest Heroes, Too Evil to Stay Dead, Cruel Alliance (4) | `AbilityCondition::AdditionalCostPaidInstead` / leading-`if`+`instead` override already used by Kicker cards (mod.rs:33376 "If it was kicked, ~ deals 5 damage instead") |
| **negated rider** ("unless this spell was cast using teamwork") | Timeline Inquiry, We Say Thee Nay! (2) | `AbilityCondition::Not { AdditionalCostPaid }` — already produced for "wasn't kicked, " / "wasn't bargained, " (conditions.rs:380) |
| **cast-permission** ("as though it had flash if cast using teamwork") | Quantum Reduction (1) | `SpellCastingOption::as_though_had_flash()` gated by a `ParsedCondition` (CR 601.3b) — needs the condition wired to the additional-cost-paid flag (see §6e) |

Card-count of the keyword + cost building block: 17 today, open-ended. No clause is a one-off; every rider is a member of an already-supported class.

**Source of truth.** All 17 cards are present in `client/public/card-data.json` with full Oracle text (verified). `keywords: []` for all of them — Teamwork is NOT in the MTGJSON keywords array; it survives only on the first Oracle line (`"Teamwork N (As an additional cost …)"`). This drives the parser design in §4.

---

## 2. Analogous trace (full file path)

**Primary trace — Conspire** (closest existing "optional additional cost that taps creatures + flag-gated effect"):

`types/keywords.rs` (`Keyword::Conspire`, FromStr ~:2284, tagged ~:2621)
→ `database/synthesis.rs::synthesize_conspire` (:2550) builds `AdditionalCost::Optional { cost: AbilityCost::TapCreatures { count: 2, filter }, .. }` + a flag-gated trigger; registered in `synthesize_all` (:8914)
→ `game/casting_costs.rs::effective_conspire_additional_cost` (:4403) offers the cost; the optional yes/no is `WaitingFor::OptionalCostChoice` → `handle_decide_additional_cost` (:127), which on `pay=true` calls `record_additional_cost_payment(Other, 1)` (:209, sets `SpellContext.additional_cost_paid`) then `pay_additional_cost_with_source` (:4064) which routes `TapCreatures` into `WaitingFor::PayCost { kind: TapCreatures, count, min_count: 0 }`
→ `game/engine.rs::apply` PayCost arm (:2340) → `engine_casting::handle_tap_creatures_for_spell_cost` (:136) → `casting_costs::handle_tap_creatures_for_spell_cost` (:1776) validates `chosen.len() == count`, taps, `finish_pending_cost_or_cast`
→ flag read at resolution by `AbilityCondition::additional_cost_paid_any()` (the conspire copy trigger condition, synthesis.rs:2633).

**Secondary trace — Kicker** (the surface-phrase condition parser + modal upgrade): `parser/oracle_effect/conditions.rs::strip_additional_cost_conditional` (:361, "was kicked"/"wasn't kicked"/"was bargained") and `parser/oracle_modal.rs::parse_modal_additional_cost_condition` (:520, "this spell's additional cost was paid" / "this spell was kicked").

**Power-threshold-keyword precedent — Crew / Conspire-filter / Renown / Firebending** (`u32`/power-parameterized keyword parsed from the Oracle line even when MTGJSON omits it): `parse_keyword_from_oracle` (oracle_keyword.rs:1025), `parse_firebending_keyword_line` (:411), and the standalone-recovery allowlist `parse_mtgjson_missing_standalone_keyword_line` (:374).

---

## 3. add-engine-variant gate — `Keyword::Teamwork(u32)`

Run BEFORE adding the variant (mandatory).

**Stage 1 — Existence (5-grep):** No `Teamwork` variant exists (`rg -n "Teamwork" crates/engine/src/types/` → 0). `Crew { power: u32, .. }` is power-threshold-keyed but is an *activated* ability (CR 702.122), not a cast-time additional cost — different rule section, not reusable. **DOES_NOT_EXIST → proceed.**

**Stage 2 — Parameterization filter:** Is `Teamwork(u32)` a sibling-cluster smell? The `Keyword` enum holds many parameterized-by-`u32` keywords (`Rampage(u32)`, `Absorb(u32)`, `Renown(u32)`, `Fabricate(u32)`, `Annihilator(u32)`, `Dredge(u32)`, `Modular(u32)`). These are NOT a unifiable cluster — each names a categorically distinct keyword ability (separate CR sections), and the engine's whole keyword design is "one variant per named keyword." Teamwork is genuinely orthogonal: a new named keyword. **EXTEND_OK.**

**Stage 3 — Categorical boundary:** The variant carries a single `u32` power threshold within the additional-cost rule section (CR 601.2). No cross-section conflation. **WITHIN_SECTION → APPROVED.**

**Verdict:** `APPROVED: Keyword::Teamwork(u32)` — the `u32` is the total-power threshold N. CR annotation: Teamwork is a Marvel-set mechanic and is **NOT in the Comprehensive Rules** (`grep -ni teamwork docs/MagicCompRules.txt` → 0 hits). Annotate against the general additional-cost rules, never a fabricated `702.x`:
```rust
/// Teamwork N (Marvel Super Heroes mechanic; not in the Comprehensive Rules).
/// CR 601.2b/f/h: An OPTIONAL additional cost to tap any number of creatures
/// you control with total power N or more. `u32` is the power threshold N.
Teamwork(u32),
```

**Second variant gate — the tap-payment mode.** §5 needs a way to express "tap creatures with total power ≥ N." This is a `PayCostKind` extension (`PayCostKind::TapCreaturesPower` or a power field). Run the gate there too (§5.1).

---

## 4. Parser — recognizing the `Teamwork N` keyword line (nom-compliant)

The line is `"Teamwork 1 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 1 or more.)"`. `strip_reminder_text` removes the parenthetical, leaving `"Teamwork 1"`. Because `keywords: []`, the empty-MTGJSON branch runs.

### 4a. `crates/engine/src/parser/oracle_keyword.rs`
- **`parse_teamwork_keyword_line(line: &str) -> Option<Keyword>`** — new combinator mirroring `parse_firebending_keyword_line` (:411) EXACTLY: `tag("teamwork ")` then `nom_primitives::parse_number`, `all_consuming` on the remainder, → `Keyword::Teamwork(n as u32)`. No `contains`/`split` — pure nom dispatch.
- **`parse_keyword_from_oracle`** (:1025) — add `if let Some(kw) = parse_teamwork_keyword_line(text) { return Some(kw); }` alongside the firebending/renown arms.
- **`parse_mtgjson_missing_standalone_keyword_line`** (:374) — add `Keyword::Teamwork(_) => Some(vec![keyword]),` to the recovery allowlist. **This is the load-bearing line** that lets the standalone `Teamwork N` line be recovered when MTGJSON omits the keyword (same reason `ForMirrodin`/`TotemArmor`/`BandsWithOther` are listed). Without it, the keyword line is dropped and synthesis never fires.

### 4b. `crates/engine/src/types/keywords.rs`
- **`Keyword` enum** — add `Teamwork(u32)` in the parameterized-numeric cluster (near `Renown(u32)`), with the CR annotation from §3.
- **`KeywordKind` enum + `From<&Keyword>`** (~:140 / :1123) — add `Teamwork` kind + the `Keyword::Teamwork(_) => KeywordKind::Teamwork` arm (exhaustive match; no wildcard).
- **`FromStr` parameterized block** (:1846) — add `"teamwork" => return Ok(Keyword::Teamwork(p.parse().unwrap_or(1))),` (mirrors `"renown"`/`"rampage"`). Covers any future MTGJSON `"Teamwork:N"` tagged form.
- **`keyword_from_tagged`** (~:2669) — add `"Teamwork" => Ok(Keyword::Teamwork(num(data)?)),` (mirrors the numeric tagged keywords) so persisted `card-data.json` round-trips.
- **`Display`** (if present) — add the `Teamwork(n)` formatting arm.

### 4c. Parser tests (oracle_keyword.rs inline)
- `extract_keyword_line_teamwork_without_mtgjson_keyword` — assert `extract_keyword_line("Teamwork 1 (…reminder…)", &[])` → `vec![Keyword::Teamwork(1)]` (mirrors `extract_keyword_line_umbra_armor_reachable_without_mtgjson_keyword` :2956).
- `parse_keyword_from_oracle_teamwork` — `Teamwork(3)` for `"teamwork 3"`.
- `FromStr` round-trip test in keywords.rs: `"teamwork:4".parse::<Keyword>()` → `Teamwork(4)`.

**Nom compliance:** every dispatch above is `tag()` + `parse_number` + `all_consuming`. No `contains`/`starts_with`/`find`/`split` for parsing.

---

## 5. Engine — the total-power tap-payment cost

### 5.1 add-engine-variant gate — `PayCostKind` / the cost shape

Two candidate designs; gate decides.

**Candidate A — extend `AbilityCost::TapCreatures { count, filter }` with a power axis.** Sibling-cluster check: `TapCreatures` is the only "tap N creatures" cost; "tap creatures with total power ≥ N" is a *different selection predicate* (sum-of-power threshold vs cardinality). Adding a `min_total_power: Option<u32>` field to `TapCreatures` would conflate two selection semantics in one variant and force every existing `TapCreatures { count, filter }` call site (cost_payability.rs, mana_abilities.rs, costs.rs, coverage.rs, synthesis.rs — ~15 sites) to reason about a power axis that is meaningless for Conspire. **REFUSE Candidate A** — wrong axis on a count-typed variant.

**Candidate B — new sibling `AbilityCost::TapCreaturesPower { min_total_power: u32, filter: TargetFilter }`.** Stage 1: does not exist. Stage 2: is this a sibling-cluster smell against `TapCreatures`? No — the two are categorically distinct *selection predicates* (cardinality vs aggregate-power), exactly the `Composite` vs `OneOf` precedent (ability.rs:5993 documents siblings as correct when the compositional/selection axis differs and lives in one CR section). Both are CR 601.2 additional costs. Stage 3: within CR 601.2. **APPROVED: `AbilityCost::TapCreaturesPower { min_total_power: u32, filter: TargetFilter }`** with:
```rust
/// Teamwork (Marvel Super Heroes). CR 601.2b/f/h: tap any number of creatures
/// matching `filter` whose summed power is at least `min_total_power`. Distinct
/// from `TapCreatures { count }` (cardinality predicate) — this is an
/// aggregate-power predicate over a variable-size selection.
TapCreaturesPower { min_total_power: u32, filter: TargetFilter },
```

**`PayCostKind` extension (the interactive round-trip).** The existing `WaitingFor::PayCost { count, min_count }` round-trip validates `chosen.len() == count` (casting_costs.rs:1785) and AI generates exactly-`count` combinations (candidates.rs:1601). Neither expresses a power-sum stop condition. APPROVED: add `PayCostKind::TapCreaturesPower { min_total_power: u32 }` (the threshold travels on the kind so the resolver and AI can validate the sum without re-deriving it). `WaitingFor::PayCost.choices` carries the eligible set; `count` = `choices.len()` (max selectable), `min_count` = 0 (decline already handled upstream by the Optional yes/no — see §5.4).

### 5.2 `crates/engine/src/types/ability.rs`
- Add `AbilityCost::TapCreaturesPower { min_total_power, filter }` (above) with `#[serde(default)]` on nothing new needed (both fields required; new variant is additive and old JSON never contains it).
- **`AbilityCost::categories`** (:6120) — add `AbilityCost::TapCreaturesPower { .. } => vec![CostCategory::TapsOtherCreatures],` (exhaustive; same category as `TapCreatures`).
- **`supports_cumulative_upkeep_payment`** (:6082) — falls into `_ => false` (correct; not a cumulative-upkeep cost). Verify no other exhaustive `AbilityCost` match needs an arm via `cargo check -p engine` (clippy/Tilt will surface E0004). Known sites to update by inspection: `costs.rs` (:976/:1126/:1264/:1333/:1434 — cost-reduction/clone passes), `printed_cards.rs:753`, `replacement.rs:465`, `engine_payment_choices.rs:914`, `cost_payability.rs` (payability + eligibility, :204/:391/:531), `coverage.rs` (:3429/:4655/:4706 unimplemented-leaf + label).

### 5.3 `crates/engine/src/types/game_state.rs`
- **`PayCostKind`** (:2423) — add `TapCreaturesPower { min_total_power: u32 }`.
- **`WaitingFor::PayCost`** — no struct change (reuse `choices`/`count`/`min_count`/`resume`).

### 5.4 `crates/engine/src/game/casting_costs.rs` — offer + payment

- **`pay_additional_cost_with_source`** (:4064) — add an arm for `AbilityCost::TapCreaturesPower { min_total_power, ref filter }`, mirroring the `TapCreatures` arm: build `eligible` via `matches_target_filter` over controller's untapped creatures matching `filter`. Eligibility error: if the **summed power of all eligible creatures** `< min_total_power`, return `ActionNotAllowed("creatures' total power can't reach the teamwork threshold")` — but note: because the cost is *optional*, the engine must NOT hard-error during the optional offer; the threshold-unreachable case simply means "paying is impossible," handled by the offer gate (below). Return `WaitingFor::PayCost { kind: PayCostKind::TapCreaturesPower { min_total_power }, choices: eligible, count: eligible.len(), min_count: 0, resume: Spell { .. } }`.

  **CR annotation:** `// CR 601.2b/f/h: Teamwork — tap any number of creatures with total power ≥ N as an optional additional cost.`

- **`handle_tap_creatures_power_for_spell_cost`** (NEW, mirrors `handle_tap_creatures_for_spell_cost` :1776) — validation:
  1. every `chosen` ∈ `legal_creatures` (re-validate eligibility);
  2. **sum the current power of `chosen`** (use the layer-evaluated power via `state.objects.get(id).power`/derived power getter — match how combat/`SourcePowerAtLeast` reads power; do NOT read base P/T) and require `sum >= min_total_power`, else `InvalidAction("total power N not reached")`;
  3. tap each chosen, emit `PermanentTapped`;
  4. record the payment: `pending.ability.context.record_additional_cost_payment(AdditionalCostOrigin::Other, 1)` then `finish_pending_cost_or_cast`. **NOTE:** unlike the Conspire path (where the Optional yes/no already recorded the flag in `handle_decide_additional_cost`), confirm during implementation whether the flag is already set by the upstream `pay=true` decision; if so, this step is a no-op and must NOT double-record. The single authority for "was teamwork paid" is `SpellContext.additional_cost_paid` set once at the Optional `pay=true` decision (casting_costs.rs:201-209). The power-validation handler must only TAP and finish — it must not re-set the flag. (This mirrors how `handle_tap_creatures_for_spell_cost` does NOT set the flag.)

  **CR annotation on the power-sum check:** `// CR 601.2h: pay the announced additional cost — selected creatures' total power must meet the teamwork threshold N (Marvel Super Heroes).`

- **PRIMARY offer path = the synthesized `obj.additional_cost`.** VERIFIED against code: at casting_costs.rs:2867, `obj_additional = obj.additional_cost.clone()`, and the `obj_additional.is_some()` arm (:2925) is what offers a *printed* keyword's additional cost. The comment at :2947-2951 confirms `effective_conspire_additional_cost` (:2947 arm) fires ONLY for the **statics-granted** Conspire case (Wort/Rassilon grant Conspire to OTHER spells); printed Conspire is caught by the `obj_additional.is_some()` arm. Teamwork is a **printed** keyword on every one of the 17 cards, so the `synthesize_teamwork` → `face.additional_cost` → `GameObject.additional_cost` chain (§7) IS the offer path. The Optional yes/no (`WaitingFor::OptionalCostChoice`) and `handle_decide_additional_cost` fire automatically off that.
- **`effective_teamwork_additional_cost` is OPTIONAL / OUT OF SCOPE.** No Marvel card *grants* Teamwork to another spell, so the granted-path analog (`effective_conspire_additional_cost`) is unnecessary for the 17 cards. **Do NOT add it** unless a granted-Teamwork card later exists — adding it now would be speculative infrastructure for a card that doesn't exist (violates "build for the class that exists, not hypothetical future requirements"). The `teamwork_tap_filter()` ("creatures you control": `TypedFilter::creature().controller(ControllerRef::You)`, NO shares-color clause, unlike Conspire) lives only in `synthesize_teamwork` (§7).

- **Offer gating (impossible-to-pay):** because the cost is optional, the offer is presentable even when unpayable (the player just declines). The hard requirement is that once the player chooses "pay," they MUST be able to reach N. The enforcement lives in `pay_additional_cost_with_source` (the `TapCreaturesPower` arm above): if total eligible power `< min_total_power`, the cost cannot be satisfied. Decide the exact gating point during implementation by tracing where Conspire's `eligible.len() < count` guard (casting_costs.rs:4089) interacts with the *optional* offer: for an `AdditionalCost::Optional`, an unpayable cost should leave `additional_cost_paid` false (decline-equivalent), NOT hard-error the cast. Confirm the optional-cost decision flow (`handle_decide_additional_cost`, :127) already short-circuits `pay=true` when the inner cost is unpayable — if it does, no new suppression is needed; if it does not, gate the `pay=true` branch on payability (`cost_payability` summed-power check from §5.7) and fall back to the decline path. Do NOT replicate Conspire's hard `ActionNotAllowed` for the optional case.

### 5.5 `crates/engine/src/game/engine.rs` — reducer dispatch
- PayCost arm (:2226-2367) — add `PayCostKind::TapCreaturesPower { min_total_power } => engine_casting::handle_tap_creatures_power_for_spell_cost(state, *player, *pending_cast.clone(), choices, &chosen, *min_total_power, &mut events)?` in BOTH the `CostResume::Spell|SpellCost` block and (only if a mana-ability path is ever needed — it is not for Teamwork) leave the `ManaAbility` block unreachable for this kind. Exhaustive `match kind` — add the arm, no wildcard.

### 5.6 `crates/engine/src/game/engine_casting.rs`
- Add the thin wrapper `handle_tap_creatures_power_for_spell_cost` forwarding to `casting_costs` (mirrors :136).

### 5.7 `crates/engine/src/game/cost_payability.rs`
- Add `AbilityCost::TapCreaturesPower { min_total_power, filter }` arms in the payability + eligibility matches (:204/:391/:531). Payable iff summed eligible power ≥ `min_total_power` (mirrors the `TapCreatures` count check but on summed power). Same source-exclusion logic (the spell isn't on the battlefield, so no `{T}` self-exclusion concern, but keep the structure parallel).

### 5.8 `crates/engine/src/game/costs.rs`
- The cost-clone/cost-reduction passes (:1333/:1434) that rebuild `TapCreatures` — add the `TapCreaturesPower` clone arm (identity rebuild; no cost reduction applies to a tap-cost).

---

## 6. Parser — the "cast using teamwork" conditional surface phrases (nom-compliant)

All three condition sites already handle "kicked"/"additional cost was paid"; add the Teamwork surface phrase as additional `alt()`/`split` anchors that produce the SAME typed conditions (`source: Any`, because Teamwork is a non-kicker optional additional cost). No new condition variants.

### 6a. `crates/engine/src/parser/oracle_effect/conditions.rs` — `strip_additional_cost_conditional` (:361)
- **Leading positive** — add to the existing `split_once_on` chain (:421): `.or_else(|_| split_once_on(lower, " was cast using teamwork, "))`. Cards: Repulsor Blast ("If this spell was cast using teamwork, it also deals…"), Earth's Mightiest Heroes / Too Evil to Stay Dead / Cruel Alliance ("…instead …"). Produces `AbilityCondition::additional_cost_paid_any()`.
- **Negated** — add to the `wasn't kicked` chain (:380): `.or_else(|_| split_once_on(lower, " wasn't cast using teamwork, "))` → `Not { additional_cost_paid_any() }`. (Surface form is "unless this spell was cast using teamwork" — see 6b for the `unless` shape.)

  CR annotation: `// CR 601.2f + CR 608.2c: "cast using teamwork" reads the optional-additional-cost-paid flag (Marvel Super Heroes Teamwork).`

### 6b. Trailing and "unless" forms
Several cards put the condition at the END:
- Beast Mode: "Also put a +1/+1 counter on that creature if this spell was cast using teamwork."
- Heroic Teamwork: "…each get +2/+1 until end of turn. If this spell was cast using teamwork, draw a card." (sentence-leading in the second sentence — handled by 6a after `parse_effect_chain` splits sentences)
- Team Tactics: "…gains double strike until end of turn. If this spell was cast using teamwork, that creature also gains trample…" (leading in 2nd sentence — 6a)
- Timeline Inquiry: "Draw three cards. Then discard a card unless this spell was cast using teamwork." (trailing `unless`)

Trace how the Kicker class handles trailing `if`/`unless`. The trailing-`if` peeler and `unless`-condition handling live in `conditions.rs` (the `strip_suffix(" instead")` / trailing-`if` machinery around :519-527 and the `unless` family). Add `" if this spell was cast using teamwork"` / `" unless this spell was cast using teamwork"` as recognized trailing anchors that route to `additional_cost_paid_any()` / `Not { additional_cost_paid_any() }`. **Implementation note:** verify whether `parse_effect_chain`'s sentence split already converts Beast Mode's trailing-`if` into a gated sub_ability via the existing trailing-conditional path; if the existing kicker trailing path is phrase-keyed on "kicked"/"bargained", generalize it to also accept "cast using teamwork" at the same site (do NOT add a Teamwork-only bespoke branch — extend the existing alt list).

### 6c. `crates/engine/src/parser/oracle_modal.rs` — `parse_modal_additional_cost_condition` (:520)
- Add to the leading `alt()` (:523): `tag("this spell was cast using teamwork")` → `ModalSelectionCondition::AdditionalCostPaid { source: Any, .. , min_count: 1 }` (identical shape to the existing "this spell's additional cost was paid" arm). Covers the 5 "Choose one. If this spell was cast using teamwork, choose both instead." cards. The base-sentence + `opt("you may ")` + `parse_modal_override_count` machinery (:475) already handles the "choose both" cap upgrade; only the condition phrase is new.

  CR annotation: `// CR 601.2b + CR 700.2a: Teamwork modal upgrade — additional-cost-paid gates the higher mode cap (Marvel Super Heroes).`

### 6d. "instead" override class (Helicarrier Strike, Cruel Alliance, Earth's Mightiest, Too Evil)
These are "If this spell was cast using teamwork, [it/instead] …" — leading-`if` + `instead`. The Kicker class already parses "If it was kicked, ~ deals 5 damage instead" (mod.rs:33376) and "If this spell was kicked, instead destroy…" (:33406) via the leading-`if` split (6a) feeding the `instead`-override/`AdditionalCostPaidInstead` path. Once 6a recognizes "was cast using teamwork, ", these route through the identical override machinery automatically — confirm with a parser test, no new code expected beyond 6a.

### 6e. Quantum Reduction — "cast as though it had flash if cast using teamwork" (CR 601.3b)
"You may cast this spell as though it had flash if it's cast using teamwork." This is a `SpellCastingOption::as_though_had_flash()` whose applicability is conditional on the additional-cost-paid flag. Trace how `casting_options` conditions are stored: `SpellCastingOption { kind: AsThoughHadFlash, condition: Option<ParsedCondition> }` (ability.rs:6436). The gap: `ParsedCondition` (cast-restriction conditions) has no "additional cost was paid" variant today (only `StaticCondition::AdditionalCostPaid` and `AbilityCondition::AdditionalCostPaid` exist). **Decision:** Quantum Reduction's flash-timing permission depends on a choice made DURING the same cast (CR 601.3b explicitly allows considering in-proposal choices). This is the one card needing infrastructure beyond the existing condition reuse.

- **Run the add-engine-variant gate for `ParsedCondition::AdditionalCostPaid`.** Stage 1: `ParsedCondition` has no additional-cost-paid member. Stage 2: not a sibling cluster (the `ParsedCondition` cast-restriction family is keyed on board/source state, not cast-time payments). Stage 3: within CR 601.2/601.3b. Likely **APPROVED: `ParsedCondition::AdditionalCostPaid`** (nullary, reads the in-flight `pending_cast.ability.context.additional_cost_paid`). Wire its evaluation at the `as_though_had_flash` timing-permission check (CR 601.3b) — find where `SpellCastingOption::condition` is evaluated during the flash-timing gate and add the `AdditionalCostPaid` arm reading the pending cast context.
- **If that runtime wiring proves to require cast-proposal re-entry the engine doesn't yet model**, mark Quantum Reduction's flash clause as the single deferred rider with a strict-failure marker (`Effect::unimplemented` is not applicable here since it's a casting option, not an effect — instead leave the `casting_option` condition unparsed so coverage stays honest), and ship the other 16 cards + Quantum Reduction's enchant/`-5/-0` body. Decide during implementation review; do NOT silently accept the flash clause as a no-op (that would let Quantum Reduction be cast at flash speed unconditionally — a correctness bug). This is the only sub-feature with a possible defer; everything else is fully reusable.

### 6f. Parser tests (conditions.rs / oracle_modal.rs / oracle.rs inline)
- `additional_cost_conditional_teamwork_leading` → `additional_cost_paid_any()` + residual body.
- `additional_cost_conditional_unless_teamwork` → `Not { additional_cost_paid_any() }`.
- `modal_teamwork_choose_both` → `ConditionalMaxChoices { condition: ModalSelectionCondition::AdditionalCostPaid { source: Any }, max_choices: 2, otherwise_max_choices: 1 }`.

---

## 7. Synthesis — keyword → cost + nothing else

### `crates/engine/src/database/synthesis.rs`
- **`synthesize_teamwork(face: &mut CardFace)`** (NEW, mirrors `synthesize_conspire` :2550 but simpler — Teamwork has NO synthesized trigger; the flag-gated effects are parsed from the card's own text):
  ```
  let n = first Keyword::Teamwork(n) on face.keywords;  // (multiple instances impossible for these 17 — single keyword)
  if face.additional_cost.is_none() {
      face.additional_cost = Some(AdditionalCost::Optional {
          cost: AbilityCost::TapCreaturesPower { min_total_power: n, filter: teamwork_tap_filter() },
          repeatability: Once,
      });
  }
  ```
  CR annotation: `// CR 601.2b/f/h: Teamwork N — optional additional cost to tap any number of creatures you control with total power N or more (Marvel Super Heroes).`
- **`teamwork_tap_filter() -> TargetFilter`** (NEW) = `TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))`. No shares-quality clause (unlike `conspire_tap_filter`).
- **Register in `synthesize_all`** (:8850 area, next to `synthesize_kicker`/`synthesize_conspire` :8914) — `synthesize_teamwork(face);`. Order: after keyword parsing, alongside the other additional-cost synthesizers. Idempotent (guards on `face.additional_cost.is_none()` — but note Conspire and Teamwork can't co-occur).
- **Multiple Teamwork instances:** none of the 17 cards print Teamwork twice; if a future card does, defer with `defer_synthesis(face, "teamwork_multiple_instances", …)` exactly like Conspire's CR 702.78b deferral (:2564) — the single-aggregate `additional_cost_paid` flag can't distinguish instances.

### Synthesis test (synthesis.rs inline)
- `synthesize_teamwork_sets_tap_creatures_power_cost` — build a `CardFace` with `Keyword::Teamwork(3)`, run `synthesize_teamwork`, assert `face.additional_cost == Some(Optional { cost: TapCreaturesPower { min_total_power: 3, filter: <you-control creatures> }, Once })`. Mirror `synthesize_conspire_sets_tap_creatures_cost_and_copy_trigger` (:19119).

---

## 8. AI legal-actions (the WaitingFor round-trip)

### `crates/engine/src/ai_support/candidates.rs`
- **`OptionalCostChoice`** (:1476) — the yes/no Teamwork offer is already an `OptionalCostChoice`; the existing arm generates both pay/decline. No change (Teamwork's `AdditionalCost::Optional` flows through the same path as Conspire/Kicker). Confirm the AI evaluates "pay teamwork" sensibly (it taps creatures for an upside) — acceptable default; not an `ai-gate` change since no policy/eval weight changes.
- **`PayCost` arm** (:1601) — the default `bounded_select_card_candidates(*player, choices, [*count])` generates exactly-`count` selections. For `PayCostKind::TapCreaturesPower`, exactly-`count` (tap ALL eligible) is a *legal* selection (its summed power ≥ N whenever the offer was made), so the AI never hangs. But "tap all" may over-tap. Add an arm:
  ```
  WaitingFor::PayCost { player, kind: PayCostKind::TapCreaturesPower { min_total_power }, choices, .. }
      => teamwork_tap_candidates(*player, choices, *min_total_power, state)
  ```
  generating a small set of minimal-power-cost subsets meeting the threshold (e.g., greedily smallest-power creatures summing to ≥ N), plus the "tap all" fallback so a legal action always exists. This mirrors how Sacrifice uses a `min..=count` range rather than `[count]`. Keep it bounded (no combinatorial blowup) — cap at a few candidate subsets.
  CR annotation: `// CR 601.2h: AI selects a minimal creature subset whose total power meets the teamwork threshold.`

No `cargo ai-gate` run required unless an eval/policy weight changes — this is legal-action enumeration only. (If review judges it materially shifts decisions, run `ai-gate` with paired-seed report.)

---

## 9. Multiplayer filter + frontend

### 9a. Frontend types — `client/src/adapter/types.ts`
- **`PayCostKind`** (:307) — add `| { type: "TapCreaturesPower"; min_total_power: number }`. (TypeScript discriminated union; tsify may auto-generate — check both the generated block and manual overrides per add-engine-effect Phase 5.)
- **`AbilityCost`** union — add the `TapCreaturesPower` variant if `AbilityCost` is surfaced to TS (only if referenced by FE; `OptionalCostChoice.cost: AdditionalCost` is shown — so the `AdditionalCost::Optional { cost }` carrying `TapCreaturesPower` must deserialize; add it).

### 9b. Frontend UI — `client/src/components/`
- The Teamwork tap selection renders via the existing `WaitingFor::PayCost` / board-tap path (Conspire already works — creature tapping is board-driven, not a modal). The new `min_total_power` lets the UI show "tap creatures with total power N or more" and enable a Confirm button once the running power sum ≥ N. Trace the existing `PayCost { TapCreatures }` FE handler (Conspire's board-tap selection + Confirm) and extend its done-gate from "count reached" to "summed power ≥ min_total_power" when `kind.type === "TapCreaturesPower"`. Any new FE-authored prompt text routes through `t()` (i18n `en/game.json` + all 6 non-English locales per the i18n parity hard-gate).
- The yes/no Teamwork offer uses the existing `OptionalCostChoice` modal (renders `cost: AdditionalCost`) — confirm it renders a `TapCreaturesPower`-carrying Optional without crashing (it shows the cost description; engine provides the text). No new modal.

### 9c. Multiplayer filter — `crates/server-core/src/filter.rs`
- Teamwork reveals NO hidden information (tapping your own creatures, public board state). `WaitingFor::PayCost.choices` are public battlefield ObjectIds. **No `filter.rs` change needed.** (Confirm `PayCost` is already broadcast/handled — it is, for Conspire.) `session.rs` routing unchanged.

### 9d. Frontend type-check gate
- Run `pnpm type-check` + `pnpm lint` before committing FE changes (no committed TS errors).

---

## 10. CR annotations to add (all grep-verified)

| Location | Annotation | Verified |
|---|---|---|
| `Keyword::Teamwork(u32)` | `CR 601.2b/f/h: optional additional cost (Marvel mechanic; not in CR)` | `601.2b/f/h` present (lines 2457/2466/2470); `teamwork` absent from CR ✓ |
| `synthesize_teamwork` | `CR 601.2b: announce optional additional cost` | ✓ |
| power-sum payment check | `CR 601.2h: pay the announced additional cost` | ✓ (line 2470 "player pays the total cost") |
| `effective_teamwork_additional_cost` | `CR 601.2f: cost locked in after announcement` | ✓ (line 2466) |
| modal cap upgrade | `CR 601.2b + CR 700.2a: mode choice with conditional cap` | `601.2b` ✓; `700.2a` to grep-verify before writing |
| "cast using teamwork" condition | `CR 601.2f + CR 608.2c: reads additional-cost-paid flag at resolution` | `608.2c` to grep-verify (it is the standard "if it was kicked" annotation already in conditions.rs) |
| Quantum Reduction flash (if shipped) | `CR 601.3b: cast as though it had flash, considering in-proposal additional-cost choice` | `601.3b` present (line 2480) ✓ |

**Mandatory before writing any annotation:** re-grep `700.2a` and `608.2c` (`grep -n "^700.2a" docs/MagicCompRules.txt`, `grep -n "^608.2c" docs/MagicCompRules.txt`). Do not trust this table's "to grep-verify" rows without the grep.

---

## 11. Verification Matrix (discriminating, cast-pipeline tests)

Tests live in `crates/engine/tests/` (integration) per the card-test skill: `GameScenario` + `GameRunner::cast(...).resolve()`, assert via `CastOutcome` deltas — **never** AST-shape-only, **never** hand-built `TargetRef` vectors. Each test must FAIL if the teamwork flag wiring is reverted.

| Behavioral claim | Changed seam | Production entry | Discriminating runtime test | Revert-fails assertion | Sibling / negative case |
|---|---|---|---|---|---|
| Teamwork keyword parses from empty-MTGJSON line | `parse_mtgjson_missing_standalone_keyword_line` allowlist + `parse_teamwork_keyword_line` | card load → `face.keywords` | parser test: `Teamwork(1)` recovered | drop allowlist arm → `keywords` empty, synthesis no-ops | line with no number → not a keyword line |
| Paying teamwork sets the flag and the rider fires | `handle_tap_creatures_power_for_spell_cost` + Optional decision | cast Beast Mode, pay teamwork (tap a power-1 creature), resolve | `cast_teamwork_beast_mode_paid_adds_counter` — target creature has +1/+1 counter AND +2/+2/trample | revert flag-record → no counter | **decline** teamwork → +2/+2/trample but NO counter (proves the gate, not unconditional) |
| Decline path = "not cast using teamwork" | `OptionalCostChoice` decline | cast Beast Mode, decline | `cast_teamwork_declined_no_rider` — no counter | n/a (negative is the point) | confirm creatures NOT tapped |
| Power threshold enforced | power-sum check in handler | cast Heroic Teamwork (N=3), attempt to tap only power-2 worth | engine rejects (InvalidAction); tapping power≥3 succeeds → draw a card | revert sum check → under-power tap accepted | tapping 1×3-power creature OR 3×1-power creatures both satisfy |
| Modal "choose both instead" upgrade | `parse_modal_additional_cost_condition` + modal cap eval | cast Widow's Bite paid → choose both modes | `cast_teamwork_widows_bite_choose_both` — both deathtouch AND −2/−2 applied | revert modal phrase arm → cap stays 1, only one mode | declined → exactly one mode selectable |
| "instead" substitution | leading-if + instead override (6a/6d) | cast Helicarrier Strike paid → 4 damage | `cast_teamwork_helicarrier_paid_deals_4` (declined → 2) | revert "cast using teamwork" split → 2 damage even when paid | declined → 2 damage |
| Negated "unless" rider | `Not { additional_cost_paid_any }` | cast Timeline Inquiry paid → NO discard | `cast_teamwork_timeline_paid_no_discard` (declined → discard 1) | revert negated split → discards even when paid | declined → discard occurs |
| Quantum Reduction flash (if shipped) | `ParsedCondition::AdditionalCostPaid` at flash-timing gate | cast at instant speed only when teamwork paid | `cast_teamwork_quantum_flash_only_when_paid` | revert → castable at flash unconditionally (BUG) | declined on opp turn → cast rejected |

**Coverage honesty:** With the `Teamwork(u32)` variant + synthesis, the keyword line parses to a real variant (not `Keyword::Unknown`) → coverage classifies it supported (coverage.rs:3127/3749). `AdditionalCost::Optional { cost: TapCreaturesPower }` classifies as `"AdditionalCost:Optional"` and `TapCreaturesPower` is not `Unimplemented` → honest green (coverage.rs:3398/3429). The flag-gated riders parse to real `Effect`/`AbilityCondition` (no `Effect::unimplemented`). **The ONLY possibly-deferred surface is Quantum Reduction's flash clause** (§6e) — if deferred, leave its casting-option condition unparsed so the card stays partially-unsupported in coverage (honest), never a silent no-op.

**No Oracle text accepted-but-deferred** except the Quantum Reduction flash decision, which is explicitly gated in §6e.

---

## 12. Registration points in lockstep (add-engine-effect + add-keyword checklist)

One coherent change set; nothing half-wired:

1. `types/keywords.rs` — `Keyword::Teamwork(u32)` + `KeywordKind` + `FromStr` + `keyword_from_tagged` + `Display`.
2. `types/ability.rs` — `AbilityCost::TapCreaturesPower` + `categories` + every exhaustive `AbilityCost` match (find via `cargo check -p engine`).
3. `types/game_state.rs` — `PayCostKind::TapCreaturesPower`.
4. `parser/oracle_keyword.rs` — `parse_teamwork_keyword_line` + `parse_keyword_from_oracle` + standalone-recovery allowlist + tests.
5. `parser/oracle_effect/conditions.rs` — leading/trailing/`unless` "cast using teamwork" anchors → existing `additional_cost_paid_any` / `Not` + tests.
6. `parser/oracle_modal.rs` — modal "this spell was cast using teamwork" arm + test.
7. `database/synthesis.rs` — `synthesize_teamwork` + `teamwork_tap_filter` + `synthesize_all` registration + test.
8. `game/casting_costs.rs` — `pay_additional_cost_with_source` arm (`TapCreaturesPower`) + `handle_tap_creatures_power_for_spell_cost` + optional-cost payability gate (§5.4). (`effective_teamwork_additional_cost` is OUT OF SCOPE — printed Teamwork flows through the synthesized `obj.additional_cost` path; see §5.4.)
9. `game/engine.rs` + `game/engine_casting.rs` — PayCost reducer arm + wrapper.
10. `game/cost_payability.rs` + `game/costs.rs` — payability/eligibility/clone arms.
11. `ai_support/candidates.rs` — `PayCost { TapCreaturesPower }` minimal-subset candidate arm.
12. `client/src/adapter/types.ts` — `PayCostKind` + `AbilityCost` variant; FE PayCost done-gate by power-sum; i18n keys in all 7 locales.
13. (If Quantum Reduction flash shipped) `types/ability.rs` `ParsedCondition::AdditionalCostPaid` + its flash-timing evaluation.
14. Integration tests per §11; snapshot updates if any card's parsed abilities change (`cargo coverage` to confirm the 17 cards drop their unsupported markers).

**Verification cadence (Tilt-first):** `cargo fmt --all` (direct, always) → if Tilt up, `./scripts/tilt-wait.sh --timeout 240 clippy test-engine card-data`; else direct `cargo clippy --all-targets -- -D warnings` + `cargo test -p engine`. `cargo coverage` (direct one-shot) to confirm the 17 cards. `pnpm type-check`/`pnpm lint` for the FE diff. Do NOT touch `mtgish/`, `crates/mtgish-import/`, or `data/mtgish-*` (dormant — out of scope).

---

## 13. Logic-placement & idiom summary

- **Engine owns all logic.** Keyword variant, cost variant, payment validation, condition evaluation — all in `crates/engine`. FE only renders the offer + tap selection + power-sum done-gate (display, not derivation: the engine provides `min_total_power`; the FE never computes game state).
- **Typed enums, no bools.** `Keyword::Teamwork(u32)`, `AbilityCost::TapCreaturesPower { min_total_power, filter }`, `PayCostKind::TapCreaturesPower { min_total_power }` — all carry typed data; the flag is the existing `SpellContext.additional_cost_paid` bool (already the engine's single authority for "optional additional cost paid").
- **Exhaustive matches.** Every new variant forces arms in `categories`, `cost_payability`, `costs`, `coverage`, the PayCost reducer, and AI candidates — found via `cargo check -p engine`, no wildcard fallbacks.
- **Single authority for the flag.** "Was teamwork paid" is read ONLY through `AbilityCondition::AdditionalCostPaid` / `ModalSelectionCondition::AdditionalCostPaid` / (`ParsedCondition::AdditionalCostPaid` if Quantum ships), all backed by `SpellContext.additional_cost_paid` set once at the Optional `pay=true` decision. The power-validation handler taps and finishes — it never re-sets the flag.
- **Extend, don't proliferate.** Reused `AdditionalCost::Optional`, `OptionalCostChoice`, `WaitingFor::PayCost`, `AbilityCondition::AdditionalCostPaid`, `ModalSelectionCondition::AdditionalCostPaid`, `ConditionalMaxChoices`, `AdditionalCostPaidInstead`. The only NEW types are `Keyword::Teamwork(u32)`, `AbilityCost::TapCreaturesPower`, `PayCostKind::TapCreaturesPower`, and (conditionally) `ParsedCondition::AdditionalCostPaid` — each gated through add-engine-variant and justified by a categorical-distinctness argument (power-sum selection predicate ≠ cardinality predicate).
