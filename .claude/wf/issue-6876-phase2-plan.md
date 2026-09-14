# Phase 2 plan — Issue #6876 (cost-qualified tap trigger / Agent Maria Hill)

Phase-plan mode for `/engine-implementer`. Frozen design is charter
`/workspace/.claude/wf/issue-6876-tap-cause-charter.md` §1 — **not re-planned**.
This document is the Phase 2 implementation plan only. Do not implement from a
partial reading; every architectural section below is load-bearing.

- **Phase index:** 2 of 2
- **Goal:** Let a triggered ability require a specific tap cause, and parse
  Agent Maria Hill's Teamwork qualifier into it, so the trigger fires only on a
  Teamwork additional-cost tap — while any tap qualifier the parser does not
  recognize leaves the card honestly unsupported (`TriggerMode::Unknown`) rather
  than silently producing an unqualified `Taps` trigger.
- **Prior phase:** Phase 1 accepted at `PHASE_BASE_SHA 896c68c9c4b888e95ae902a313334ea2cf3b2d97`
  (typed `TapCause` on every `PermanentTapped`; opponent-tap gate translated;
  in-flight Teamwork origin derived and stamped; protocol 70→71 / 53→54).
  Workspace HEAD at plan time **matches** that SHA.
- **Run-level BASE_SHA:** `0f1a35b4de154ca969ca2e8f9ed6db85225c7dc3`
- **Worktree:** `/workspace` on `cursor/general-bug-4e55`
- **Tilt:** down at plan time (`tilt` exit 127). Isolated probe target
  `CARGO_TARGET_DIR=/tmp/phase-plan-6876-p2` (same 254G disk as `/workspace`;
  reused, not deleted; seeded from the Phase 1 cache then compiled).
- **#6876 is still live** until this phase lands — Hill still fires on any tap.

---

## Step 0 — Premise verification

Fetched independently this round:

`https://api.scryfall.com/cards/named?exact=Agent%20Maria%20Hill`
(oracle_id `a2d90efe-80f2-4069-b135-a8bd00ccd2b0`, MSH 2):

> Whenever Agent Maria Hill becomes tapped to pay a teamwork cost, put a +1/+1
> counter on her and draw a card.

Legendary Creature — Human Spy Hero · `{W}` · 2/1.

Matches GitHub issue #6876 and the charter verbatim. **Premise PASSES.**

Turn-constraint reach-guard, independently fetched:

`https://api.scryfall.com/cards/named?exact=Captain%20America%2C%20Living%20Legend`

> Vigilance
> Whenever a creature you control becomes tapped during your turn, if it's the
> first time that creature has become tapped this turn, untap it.

Scryfall class counts re-fetched this round (`total_cards`):

- `o:"becomes tapped to pay"` → **1** (Agent Maria Hill)
- `o:"becomes tapped"` → **117** → **116** unqualified

Matches charter §1.1. Pattern Coverage stop is assessed against that
attribution, not against this phase's own diff.

`AdditionalCostOrigin::Teamwork` and the Phase 1 stamp
`TapCause::CostPayment(TapCostKind::TapCreatures { origin: Some(Teamwork) })`
are landed infrastructure. This phase consumes them; it does not re-stamp
producers.

---

## Step 1 — Applicable skills

| Skill | This phase |
|---|---|
| `/add-trigger` | **Primary.** Additive `TriggerDefinition.tap_cause`; equality gate in `match_taps`; parser qualifier; Hill tests. **No new `TriggerMode`.** |
| `/oracle-parser` | `SimpleEvent::BecomesTapped` arm in `oracle_trigger.rs`. Nom on the first pass. |
| `/card-test` | Hill discriminating cast-pipeline test (Teamwork path) plus hostiles. |
| `/add-engine-variant` | **N/A / field-not-variant.** `tap_cause` is an additive field on existing `TriggerDefinition`, consuming Phase 1 `TapCause` / `TapCostKind`. No new enum variant. Stage checklist recorded below, not run as an extension gate. |
| `/add-engine-effect`, `/add-keyword`, `/add-static-ability`, `/add-replacement-effect`, `/add-interactive-effect`, `/add-frontend-component`, `/add-ai-feature-policy`, `/add-card-data-pipeline`, `/casting-stack-conditions` | Do not apply. |

### `/add-trigger` checklist (every step present)

- **Phase 1 — Type Definition** (`TriggerMode` / `TriggerCondition` / `TriggerConstraint`): **N/A** — existing `TriggerMode::Taps`. No new condition or constraint variant. **This phase** adds `TriggerDefinition.tap_cause: Option<TapCause>` (field, not a mode).
- **Phase 2 — Event Emission:** **N/A this phase** — Phase 1 already stamps `PermanentTapped.cause`. Do not re-stamp producers. Production test `teamwork_tap_emits_cost_payment_cause_with_teamwork_origin` already asserts the event cause (not Hill decline).
- **Phase 3 — Matcher:** **this phase** — equality gate in `match_taps`. Registry insert already `TriggerMode::Taps → match_taps` (`trigger_matchers.rs`); do **not** add a mode or a second matcher. Opponent-tap gate from Phase 1 stays; equality is a **separate** conjunct after subject match.
- **Phase 4 — Target Extraction:** **N/A** — Hill execute is `PutCounter` on `SelfRef` plus chained `Draw` for the controller. No new `extract_target_filter_from_effect` arm.
- **Phase 5 — Parser:** **this phase** — `SimpleEvent::BecomesTapped` arm: bind `"to pay a teamwork cost"`; refuse unmodelled `"to pay …"` tails. Reuse `parse_leading_turn_constraint` (do not rewrite it).
- **Phase 6 — Condition/Constraint Tracking:** **N/A** — no new `TriggerCondition` / `TriggerConstraint` / `GameState` ledger. Captain America's intervening-if and turn constraint already lower. LKI: **N/A** — a "becomes tapped" trigger (CR 603.2e) fires while the permanent is still on the battlefield; Hill has no intervening-if.
- **Phase 7 — Stack Resolution:** **N/A** — standard `resolve_ability_chain`. No new re-check.
- **Phase 8 — Tests:** parser (Hill + bargain + Captain America reach-guard + unqualified sibling) in `oracle_trigger_tests.rs`; matcher equality table next to existing `match_taps` tests; `/card-test` integration file. APNAP: **N/A** — Teamwork taps creatures **you** control (CR 702.194a); two players' Hills cannot both be the payment for one Teamwork spell. Once-per-turn: **N/A**.

### `/oracle-parser` checklist (every step present)

**9a. Adding a New Parser Pattern**

- **Phase 1 — Identify Where It Belongs:** Trigger → `crates/engine/src/parser/oracle_trigger.rs`, `try_parse_event` / `SimpleEvent::BecomesTapped` arm. Not imperative, static, replacement, or classifier routing.
- **Phase 2 — Add the Pattern:** Write parser tests first. Nom combinators from the first line (`tag` / `alt` / `value` / `opt` / `preceded` / `space0`). Existing helpers: `parse_leading_turn_constraint` (already on this arm). More specific (`to pay` qualifier) **before** the generic remaining-as-turn-constraint peel. **Never** `contains` / `find` / `split_once` for this dispatch.
- **Phase 3 — Handle the Subject:** **N/A** — `~` / card name already lowers to `TargetFilter::SelfRef` via `normalize_self_refs`. No `try_parse_*` interceptor. Probe confirmed `valid_card == Some(SelfRef)`.
- **Phase 4 — Chain Composition:** **N/A** — `"put a +1/+1 counter on her and draw a card"` already lowers (probe: top-level `Effect::PutCounter` `{ counter_type: P1P1, count: Fixed 1, target: SelfRef }`; runtime draw proves the `Draw` continuation). Do not touch `sequence.rs` / `parse_effect_chain`.
- **Phase 5 — Routing:** **N/A** — `has_trigger_prefix` already routes `Whenever … becomes tapped`. Do not edit `oracle_classifier.rs`.
- **Phase 6 — Tests & Verification:** verbatim Hill Oracle; bargain negative with Hill positive reach-guard; Captain America turn-constraint reach-guard; Night Market Lookout unqualified sibling. Runtime discriminating test via `/card-test` (not parser-shape-only). Snapshot tests: update `oracle_ir/snapshot_tests.rs` / `oracle_trigger_snapshot_tests.rs` **only if** insta reds (Hill is not in those snapshots today). `cargo coverage`: Hill stays **supported** (it is wrongly-supported today; this phase makes that support correct). Unimplemented count is **not** expected to drop — honesty is `TriggerMode::Unknown`, not `Effect::unimplemented`. Tilt-down verification: isolated `CARGO_TARGET_DIR` (see Step 5.11).

**9b. Adding a New Effect Type:** **N/A**

**9c. Adding a New Trigger Event:** **N/A** as a new `SimpleEvent` / `TriggerMode`. Parser-specific work is a **qualifier on the existing `BecomesTapped` arm**, wiring `tap_cause` (not `valid_source`). No new `parse_simple_event` `tag()` arm. Actor-side compound-subject matcher: **N/A**. Condition-scoped `"for the first time each turn"`: **N/A** (Captain America already uses `parse_leading_turn_constraint` + intervening-if extraction).

**9d. Adding a New Phrase Helper:** **N/A** as `oracle_util::match_phrase_variants`. New combinator `parse_modelled_to_pay_cause` lives next to `parse_leading_turn_constraint` in `oracle_trigger.rs` (same file as the arm that owns the tail).

**9e. Adding a New Replacement Pattern:** **N/A**

### `/card-test` checklist (Hill — all six foot-guns)

Recipe: `GameScenario` + `GameRunner::cast(..).resolve()` + `CastOutcome` for the **Teamwork payment path** (the discriminating positive). Hostiles that are not casts (attack, crew, `{T}`) drive through `GameRunner::act` / `activate` — never raw `stack::resolve_top`.

1. **No hand-written `TargetRef` vectors.** Teamwork payment uses `.pay_cost_with(&[hill])`. Effect-tap uses `.target_object(hill)`.
2. **No modal slots.** Hill and the Teamwork fixture are non-modal. Omit `.modes`.
3. **Hand baseline.** Teamwork / effect-tap paths use `CastOutcome::assert_hand_drawn` (stack-commit baseline). Attack / `{T}` / crew paths are not casts — assert `Plus1Plus1` counts and library/hand deltas after `advance_until_stack_empty`, never a hand-picked `let hand_before` around `handle_cast_spell`.
4. **Keywords not inline reminder text.** Teamwork spell built with `from_oracle_text_with_keywords(&["teamwork:1"], TEAMWORK_ORACLE)` (same recipe as `teamwork_tap_emits_cost_payment_cause_with_teamwork_origin`). Crew vehicle: `from_oracle_text_with_keywords(&["crew:1"], …)`. Do **not** feed `"Teamwork 1 (As an additional cost…)"` as the sole keyword source without the keyword name list.
5. **No AST-internal flag assertions in runtime tests.** Runtime asserts counters + draws + tapped-ness. `tap_cause` field assertions belong in **SHAPE** parser tests, labeled as such.
6. **No vacuous negatives.** Every "does not award +1/+1 / does not draw" row lives in the **same file** as the Teamwork positive that awards both. Bargain `Unknown` is paired with Hill `Taps` + `tap_cause == Some(Teamwork)`.

Verbatim Hill Oracle via `add_creature_from_oracle`. SpellCast optional defaults to **Decline** — the Teamwork **pay** path **must** `.accept_optional()`.

### `/add-engine-variant` — Stage verdict: **N/A / field-not-variant**

This phase does **not** type `pub enum Foo { … NewVariant }`.

- **Stage 1 (existence, recorded not re-run as an extension):** `TapCause` / `TapCostKind` **EXIST** in `crates/engine/src/types/events.rs` (Phase 1). `AdditionalCostOrigin::Teamwork` **EXISTS** in `types/ability.rs`. `TriggerMode::Taps` **EXISTS**. Refused sibling `TriggerMode::TapsToPayTeamwork` remains refused.
- **Stage 2 / Stage 3:** N/A — no proposed variant to parameterize or bound.
- After-approval items (exhaustive match, ability-scan, serialized surface): the **field** still forces `TriggerDefinition::new`, two exhaustive destructures, `TRIGGER_KEYS`, and the serde roundtrip literal — listed as standing-class admissions, not as a new design. Runtime status = live in the same change (parser binds + matcher reads). Serialized surface = additive `Option` with `#[serde(default, skip_serializing_if = "Option::is_none")]`. **No second protocol bump** (C2.5). No Forge converter pairing (string mapper already emits `TriggerDefinition::new(Taps)` → `tap_cause: None`, which C2.3 measures as permissive).

---

## Frozen design (inherited verbatim — do not change)

- **REFUSE** `TriggerMode::TapsToPayTeamwork`. A per-mechanic trigger mode is a card-shaped variant; the cause belongs on the event, not on the mode.
- **CREATE** (already landed in Phase 1) `TapCause { AttackDeclaration, CostPayment(TapCostKind), Effect { source } }`.
- **CREATE** (already landed) `TapCostKind { TapSymbol, TapCreatures { origin: Option<AdditionalCostOrigin> }, ManaShard(ConvokeMode), CrewFamily(CrewAction), Enlist, Harmonize }`.
- **Phase 2** — `TriggerDefinition.tap_cause`; `match_taps` equality gate; parser; Hill tests.
- **Identity contract** — Hill fires **iff**
  `cause == TapCause::CostPayment(TapCostKind::TapCreatures { origin: Some(AdditionalCostOrigin::Teamwork) })`.
  The authority is the event's stamped cause, bound at tap time by the payment
  site, never re-derived at trigger time.

---

## Deferral allowlist (nothing deferred past Phase 2)

Nothing from the full task is deferred past this phase. Explicitly **not owed**
(decided, not deferred):

| Item | Status |
|---|---|
| Second `PROTOCOL_VERSION` bump | **Not owed** (RA-1 / C2.5). Do not bump 71/54. |
| `ai_support/mod.rs` and the six `TriggerMode::Taps` readers at RA-2 | **Not owed** (C2.6). Cause-blind; same verdict before/after. |
| `coverage.rs` **classifier** edit (`build_trigger_item` / `check_trigger`) | **Not owed** unless C2.4(i) measurement contradicts. Probe confirms it does **not**. `trigger_details` **is** owed (C2.4(iii) judgment: add a `tap cause` row). |
| Any `TriggerMode` variant for Teamwork | **Refused** (frozen design). |
| `PendingCast` origin field | **Not owed** — withdrawn at C1.5. |

---

## Step 2 — Analogous trace

**Primary analogue — qualifier bound at parse time, equality-gated at match time:**

`TriggerDefinition.attack_target_filter: Option<AttackTargetFilter>`
(`types/ability.rs`, `#[serde(default, skip_serializing_if = "Option::is_none")]`)
→ parser writes `Some(Player)` / `None`
→ `matching_you_attack_pairs` / attack matchers equality-narrow
→ `trigger_details` renders key `"attack target"` and **omits** the key when `None`
  (`attack_target_filter_reaches_parse_details`, coverage.rs; #5507 sticky signatures).

`tap_cause` is the same contract for "becomes tapped": optional required identity
on the definition; `None` permissive; `Some(x)` equality against the event field
Phase 1 already stamps.

**Secondary analogue — tail-specific refusal, not blanket remainder reject:**

`SimpleEvent::DealtDamage` arm (`oracle_trigger.rs`): for newly opened cells,
`if !remaining.trim().is_empty() { return None; }` → `try_parse_event` `None` →
`parse_trigger_condition` fallback `TriggerMode::Unknown(condition.to_string())`.

**Must not copy that as an all-consuming remainder reject on `BecomesTapped`.**
Measured: that arm's `remaining` legitimately carries `" during your turn"`
(Captain America, Living Legend). `split_trigger` already moved the intervening-if
`", if …"` into effect text; `parse_leading_turn_constraint` peels the turn
phrase as a **leading** prefix. A blanket `remaining.is_empty()` would regress
that class (C2.4(ii)).

**Tertiary analogue — identity bound at produce time, consumed later:**

Phase 1 `ThisWayCause` discussion still holds: `PermanentTapped.cause` is the
authority. This phase **compares** it. Do not re-derive origin from
`additional_cost_payments` inside `match_taps`.

Full trace path for this phase:

`types/ability.rs` (`TriggerDefinition.tap_cause` + `::new`)
→ `parser/oracle_trigger.rs` (`BecomesTapped` bind / refuse)
→ `parser/oracle_trigger_tests.rs` (SHAPE)
→ `game/trigger_matchers.rs` (`match_taps` equality; registry already wired)
→ `game/coverage.rs` (`trigger_details` row; classifier unchanged)
→ `parser/audit_projection.rs` `TRIGGER_KEYS` (standing class)
→ `game/ability_scan.rs` / `game/quantity.rs` exhaustive destructures (standing class)
→ `client/src/adapter/types.ts` (C2.5 ownership; expected empty diff)
→ `tests/integration/issue_6876_agent_maria_hill.rs` + `main.rs` `mod`.

---

## Step 3 — Files this phase will touch

Literal paths. Standing classes (compiler-forced `TriggerDefinition` sites,
shared registration `tests/integration/main.rs`, comment-only) implied.

**Engine (charter scope)**

- `crates/engine/src/types/ability.rs` — additive field; `TriggerDefinition::new`; serde roundtrip literal.
- `crates/engine/src/game/trigger_matchers.rs` — equality gate in `match_taps` (**re-read this file; do not work from Phase 1 plan text**).
- `crates/engine/src/parser/oracle_trigger.rs` — `BecomesTapped` arm + `parse_modelled_to_pay_cause`.
- `crates/engine/src/game/coverage.rs` — `trigger_details` row; classifier **unchanged**.

**Frontend mirror (charter scope; C2.5 decides the diff)**

- `client/src/adapter/types.ts` — `trigger_definitions: unknown[]`. Expected TS diff **empty**. Open to confirm; do not invent a typed trigger body.

**Tests (T2-excluded)**

- `crates/engine/tests/integration/issue_6876_agent_maria_hill.rs` (new)
- `crates/engine/tests/integration/main.rs` — `mod issue_6876_agent_maria_hill;` (between `issue_6858_*` and `issue_688_*`)
- `crates/engine/src/parser/oracle_trigger_tests.rs`
- `crates/engine/src/game/trigger_matchers.rs` `#[cfg(test)]` equality table
- `crates/engine/src/game/coverage.rs` `#[cfg(test)]` `tap_cause_reaches_parse_details`

**Standing-class admissions (compiler / schema will name them; not new design)**

Measured at HEAD against `clash_result` / last-field discipline:

- `TriggerDefinition::new` (`ability.rs`) — add `tap_cause: None`
- serde roundtrip exhaustive literal `trigger_definition_roundtrip` (`ability.rs`)
- `scan_trigger_definition` exhaustive destructure (`ability_scan.rs`) — `tap_cause: _` with the same justification as `clash_result` / `taps_for_mana_produced` (event-shape discriminator; no `TargetFilter` / `QuantityExpr` payload)
- `trigger_definition_is_cast_stable_for_pre_cast` (`quantity.rs`) — `tap_cause: _` (same class)
- `parser/audit_projection.rs` `TRIGGER_KEYS` — add `"tap_cause"`
- Any further exhaustive `TriggerDefinition { … }` / destructure the compiler names after the field lands. **Do not pre-edit files the compiler does not name.**

**Not compiler-forced (do not admit to scope)**

- `ability_rw.rs` `legacy_trigger_definition` — field access of `execute` / `condition` / `valid_card` / `valid_source` only; no exhaustive destructure.
- `ai_support/shortcut_efficacy.rs` `trigger_event_unreachable_by_confined_action` — `Taps` falls through `_ => false` on **mode** only; still cause-blind; not in the RA-2 six; do not admit.
- The six RA-2 `TriggerMode::Taps` readers (C2.6).
- All wire paths.

**Out of scope by decision**

- `crates/engine/src/ai_support/mod.rs`
- `phase-ai/src/policies/self_untap_loop.rs`
- `engine/src/analysis/ability_graph.rs`
- `engine/src/game/mana_sources.rs`
- `engine/src/game/trigger_index.rs`
- `engine/src/database/forge/trigger.rs`
- `engine/src/types/triggers.rs` (string mapper)
- `lobby-broker` / `server-core` / `ws-adapter` / `network/protocol.ts` / `check-protocol-version.mjs` constants

---

## Step 3.5 — Probe results (isolated `CARGO_TARGET_DIR=/tmp/phase-plan-6876-p2`)

Throwaway `crates/engine/tests/integration/probe6876_p2.rs` was compiled and run
against a real `GameScenario` board, then **deleted**. Regenerate; do not treat
line numbers as durable.

Command:

```
CARGO_TARGET_DIR=/tmp/phase-plan-6876-p2 CARGO_INCREMENTAL=0 \
  cargo test -p phase-engine --test integration probe_ -- --nocapture --test-threads=1
```

Also: `node scripts/check-protocol-version.mjs` → exit 0 at HEAD (pins 71 / 54).

### C2.2 / C2.4 parser + classifier (production `parse_oracle_text` + `card_face_gaps`)

Reached the production `SimpleEvent::BecomesTapped` arm (positive marker: Hill
and Lookout both `TriggerMode::Taps`; Captain America constraint+condition set).

- **Hill** (verbatim Scryfall Oracle): `mode=Taps`, `valid_card=Some(SelfRef)`,
  `constraint=None`, top execute `Effect::PutCounter { counter_type: P1P1,
  count: Fixed 1, target: SelfRef }` (not `Unimplemented`). Qualifier `"to pay
  a teamwork cost"` is **dropped**.
- **Bargain** (`"becomes tapped to pay a bargain cost"`): **also `Taps`**. This
  is the honesty defect — an unmodelled `to pay …` tail currently yields an
  unqualified supported trigger.
- **Lookout** (unqualified sibling): `Taps`, `constraint=None`.
- **Captain America** (verbatim): `Taps` + `OnlyDuringYourTurn` +
  `FirstTimeObjectTappedThisTurn`. The turn-constraint peel **works**; a blanket
  remainder reject would break this.
- **`card_face_gaps(Hill) = []`**, **`card_face_gaps(Bargain) = []`**.
  `build_parse_details_for_face(Hill)`: `supported=true`, `label=Taps`,
  details `[("watches","self"), ("active in","battlefield")]` — **no tap-cause
  row**.
- **Synthetic `TriggerMode::Unknown("becomes tapped to pay a bargain cost")`:**
  gaps `["Trigger:becomes tapped to pay a bargain cost"]`. Classifier **does**
  red `Unknown` (Display of `Unknown(s)` is `s`; `check_trigger` labels
  `Trigger:{mode}`). Positive control that C2.4 honesty is parser-side:
  `TriggerMode::Taps` cannot be turned red.

**Load-bearing matcher fact:** Hill's `valid_card` is `Some(SelfRef)`, so
`match_taps` takes the **`valid_card.is_some()` branch** and currently returns
`true` after `valid_card_matches` (opponent-tap gate skipped because the
controller is not `Opponent`). An equality gate placed **only** on the
`valid_card.is_none()` else-branch would **not fix Hill**.

### C2.1 live defect (real board)

- **Attack** (`DeclareAttackers` Hill vs P1, then `advance_until_stack_empty`):
  `tapped=true`, counters `0→1`, hand `0→1`. Reach-guard: attack actually
  tapped Hill (CR 508.1f). **Live defect:** non-Teamwork tap awards both rewards.
- **Teamwork** (`cast` + `.accept_optional()` + `.pay_cost_with(&[hill])`):
  `tapped=true`, `plus1=1`, `hand_drawn=1`. Positive control that the execute
  chain (counter **and** draw) is live, so the attack negative cannot pass
  through an unparsed ability.

### C2.5 protocol

`node scripts/check-protocol-version.mjs` exits 0 against HEAD. Pins
`EXPECTED_PROTOCOL_VERSION = 71`, `EXPECTED_WIRE_PROTOCOL_VERSION = 54`.
`protocol_version_is_71_for_tap_cause` is the current require/refuse name.
Phase 2 must leave these literals untouched.

### C2.3 / C2.6

Matcher equality is **absent** at HEAD (read of `match_taps`; existing tests
`unqualified_taps_fires_on_teamwork_cost_payment` and
`tap_no_opponent_filter_ignores_caused_by` prove `None` is cause-blind).
RA-2 readers enumerated from source this round (see C2.6 below) — cause-blind
by construction; not re-run as a behavioral probe because none of them read a
field this phase adds.

---

## Step 4 — Architectural sections

### Pattern Coverage

Charter §1.1 class (this stop is assessed against the **charter**, not the
phase diff):

- **1** cost-qualified card in print: Agent Maria Hill (`becomes tapped to pay …`).
- **116** unqualified `becomes tapped` cards — must keep `tap_cause == None` and
  remain cause-blind (C2.3).

The card count of the qualified class is 1. **The stop does not fire:** the
architecture that carries the class burden is already chosen — `Option<TapCause>`
on `TriggerDefinition` plus equality against the event's stamped `TapCause`.
That is a general required-identity gate for every future `"becomes tapped to
pay a <modelled origin> cost"` printing, not a `TriggerMode::TapsToPayTeamwork`
one-card variant.

Unmodelled `"to pay …"` tails are a **second** class member: they must not
silently join the 116. Honesty is `try_parse_event` `None` → `Unknown`.

### Sizing (this phase)

**Units (one unit = one skill-checklist pass):** **1**

1. **Cost-qualified tap trigger** — `/add-trigger` lockstep (field + matcher +
   parser + coverage details + Hill discriminating test). Discriminating test:
   Teamwork payment awards +1/+1 and a card; a non-Teamwork tap of the same
   Hill awards neither (same file).

No infrastructure→consumer edge remains inside this phase (Phase 1 was the
infrastructure unit; this is the consumer).

**T1 (≥2 units): FALSE**

**Expected scope-path count (phase-fit rule):**

Non-test authored paths in the charter scope rule: **5**

1. `crates/engine/src/types/ability.rs`
2. `crates/engine/src/game/trigger_matchers.rs`
3. `crates/engine/src/parser/oracle_trigger.rs`
4. `crates/engine/src/game/coverage.rs`
5. `client/src/adapter/types.ts` (owned; **expected empty diff**)

Tests + `tests/integration/main.rs` excluded/grouped. Standing-class
compiler-forced `TriggerDefinition` sites **group with authored sources** / are
the implied standing class — they do not mint extra T2 units.

Content diffs expected: 4 engine files + standing-class admissions the compiler
names. TS: 0 lines.

**T2 (≥13 paths): FALSE** (5, or 4 if empty-diff `types.ts` is not counted as a
changed file — either way under 13).

**Conjunction T1 ∧ T2: FALSE.** No further split. This is the last phase.

### Building Blocks

Compose; do not duplicate.

| Building block | Role |
|---|---|
| `TapCause` / `TapCostKind` (`types/events.rs`) | Required identity values. **Do not redefine.** |
| `AdditionalCostOrigin::Teamwork` | Parser binds `origin: Some(Teamwork)`. **Do not add a sibling origin.** |
| `TriggerMode::Taps` + `match_taps` + registry insert | Existing event family. Equality is a conjunct, not a new matcher. |
| `parse_leading_turn_constraint` | Existing nom peel for `"during your/opponent's turn"`. Call it **after** modelled `to pay` (leftover) **or** on the original remaining when there is no `to pay` prefix. |
| `try_parse_event` `None` → `parse_trigger_condition` `Unknown` | Honesty path. Same as `DealtDamage` / `BecomesTargetAbility` F1, but **scoped to an unmodelled `to pay` prefix**, not to any remainder. |
| `valid_card_matches` | Subject filter (Hill: `SelfRef`). Equality does not replace this. |
| Opponent-tap `TapCause` match in `match_taps` | Phase 1 gate. Leave it. Equality is independent. |
| `trigger_details` / `fmt_attack_target_filter` | Analog for the `tap cause` row: omit when `None`; distinct labels per variant. |
| `TriggerDefinition::new` | Default `tap_cause: None` (permissive). String mappers already call this. |
| `GameScenario::add_creature_from_oracle` + `from_oracle_text_with_keywords` + `SpellCast::{accept_optional,pay_cost_with}` | `/card-test` / Phase 1 Teamwork recipe. |
| `nom::{tag, alt, value, opt, preceded}` + `space0` | Qualifier combinator. Import `space0` at **file top** (`oracle_trigger.rs` currently inlines `nom::character::complete::space0` at one site). |

**New helper (justified):** `parse_modelled_to_pay_cause` in `oracle_trigger.rs`
next to `parse_leading_turn_constraint`. It is the typed combinator for the
grammar `to pay ` × optional article × modelled origin name × ` cost`/`costs`.
Not a second copy of `parse_inner_condition`. Not `oracle_util::match_phrase_variants`
(this is a trigger-event tail, not a reusable Oracle phrase table).

### Logic Placement

| Piece | Where | Why |
|---|---|---|
| Required cause on the ability | `TriggerDefinition.tap_cause` in `types/ability.rs` | The engine owns trigger identity. Not a frontend flag. Not a `TriggerMode` sibling. |
| Equality | `match_taps` in `trigger_matchers.rs` | Single runtime authority for `Taps`. Do not re-check cause in `process_triggers` or the AI. |
| Grammar → `tap_cause` / honesty | `oracle_trigger.rs` `BecomesTapped` arm | Parser is the detector (nom). Classifier cannot red a well-formed `Taps`. |
| Parse-details firing qualifier | `coverage.rs` `trigger_details` | Triage overlay already renders `constraint` / `condition` / `attack_target_filter`. A cost-qualified trigger must not read as unqualified there. |
| Classifier red/green | **unchanged** | Keys on `Unknown(_)` + registry membership + unimplemented parts. Honesty is parser `Unknown`. |
| TS mirror | no edit | `unknown[]` absorbs the additive field (C2.5). |
| Origin at tap time | **already in Phase 1** | Matcher must not read `additional_cost_payments`. |

### Rust Idioms

- `Option<TapCause>` — not `requires_teamwork: bool`, not a second `TriggerMode`.
- Exhaustive `match` on `TapCause` / `TapCostKind` / `AdditionalCostOrigin` /
  `CrewAction` / `ConvokeMode` in `fmt_tap_cause` — compiler catches new Phase 1
  siblings.
- `TapCause` is `Copy` + `PartialEq`: `if let Some(required) = trigger.tap_cause { if cause != required { return false; } }`.
- `#[serde(default, skip_serializing_if = "Option::is_none")]` — additive, #5507.
- `TriggerDefinition::new` is the single defaulting constructor; string mappers
  stay permissive via `None`.
- Import `TapCause` next to `ClashResult` in `ability.rs` (`use crate::types::events::{ClashResult, PlayerActionKind, TapCause}`). Existing `events.rs` ↔ `ability.rs` cycle already carries `AdditionalCostOrigin` on `TapCostKind`; this field does not mint a new cycle.

### Nom Compliance

All detection/dispatch in the `BecomesTapped` arm and the new helper **is nom**.
If a draft uses `contains("to pay")` / `find` / `split_once` / `starts_with` for
this qualifier, **stop and rewrite**.

Exact combinators (first pass; compose axes, do not enumerate full-string
`tag("to pay a teamwork cost")` permutations):

```rust
use nom::character::complete::{alpha1, one_of, space0, space1};

/// CR 702.194a + CR 603.2: modelled "to pay [a/an] <origin> cost(s)" tail.
/// Only Teamwork is modelled today (the printed class). Unlisted origin names
/// must fail this combinator so the caller can refuse the `to pay` prefix.
fn parse_modelled_to_pay_cause(input: &str) -> OracleResult<'_, TapCause> {
    let (input, _) = tag("to pay ").parse(input)?;
    let (input, _) = opt(alt((tag("a "), tag("an ")))).parse(input)?;
    let (input, origin) = alt((value(
        AdditionalCostOrigin::Teamwork,
        tag("teamwork"),
    ),))
    .parse(input)?;
    let (input, _) = alt((tag(" cost"), tag(" costs"))).parse(input)?;
    Ok((
        input,
        TapCause::CostPayment(TapCostKind::TapCreatures {
            origin: Some(origin),
        }),
    ))
}
```

`BecomesTapped` arm (after `def.mode = Taps` and `def.valid_card = Some(subject)`):

```rust
// remaining is the suffix after tag("becomes tapped"|"become tapped")
// on already-lowercased `rest`.
let remaining_after_pay = match preceded(space0, parse_modelled_to_pay_cause).parse(remaining) {
    Ok((after, cause)) => {
        def.tap_cause = Some(cause);
        after
    }
    Err(_) => {
        if preceded(space0, tag("to pay ")).parse(remaining).is_ok() {
            // Unmodelled "to pay …" — refuse this event family.
            // CR 603.2: do not emit a different (unqualified) trigger event.
            return None;
        }
        remaining
    }
};
if let Some(constraint) = parse_leading_turn_constraint(remaining_after_pay) {
    def.constraint = Some(constraint);
}
```

**Must not:**

- `if !remaining.trim().is_empty() { return None; }` on this arm.
- Bind `AdditionalCostOrigin::Bargain` / `Kicker` / … because the enum exists.
  Bargain is **not** a tap cost; modelling it would emit a supported `Taps`
  trigger that never fires. C2.4's fixture is `"a bargain cost"` → `Unknown`.
- Consume leftover after modelled+turn as a second all-consuming refuse.
  Leftover after a successful modelled qualifier may still hold `" during your
  turn"` (unprinted class; still peel). Other leftover stays dropped the way
  today's unqualified arm drops unused remainder — that honesty expansion is
  not this phase.

`return None` inside the `SimpleEvent` match exits `try_parse_event`. Caller
`parse_trigger_condition` then sets `TriggerMode::Unknown(condition.to_string())`.

### Extension vs Creation

**Extension.** Existing `TriggerMode::Taps` + existing `TapCause` + existing
turn-constraint peel. New field on `TriggerDefinition` is the same pattern as
`attack_target_filter` / `clash_result`. New combinator is a nested prefix
dispatch on a tail the arm already owns. No new event, no new matcher, no new
WaitingFor.

### Analogous Trace

Traced `attack_target_filter` through `types/ability.rs` → attack parser →
attack matchers → `coverage.rs` `trigger_details` / `attack_target_filter_reaches_parse_details`.

Traced `SimpleEvent::DealtDamage` tail guard (`return None` → `Unknown`) as the
honesty shape, then **narrowed** it so `BecomesTapped` still peels
`parse_leading_turn_constraint`.

Traced Phase 1 `match_taps` opponent-tap gate and `teamwork_tap_emits_cost_payment_cause_with_teamwork_origin`
as the stamp this matcher compares.

### Variant Discoverability (`/add-engine-variant`)

**N/A / field-not-variant** — see Step 1. Inventory grep for a new `TriggerMode`
/ `TapCause` sibling is how we **refuse** `TapsToPayTeamwork`, not how we add
one. `cargo engine-inventory` is not required for an additive field.

### Verification Matrix

Every negative has a paired positive reach-guard in the same file. Hostile rows
are per-claim.

| Claim | Seam | Production entry | Test | Revert-failing assertion | Hostiles / siblings | Coverage |
|---|---|---|---|---|---|---|
| **C2.1** defect live at base; this phase fixes it | `match_taps` equality + parser bind | `process_triggers` on `PermanentTapped` | `issue_6876_agent_maria_hill.rs` — Teamwork `.cast().accept_optional().pay_cost_with(&[hill]).resolve()` **and** attack / effect-tap / crew / `{T}` in the **same file** | After Teamwork: `Plus1Plus1 == 1` and `assert_hand_drawn(P0, 1)` and Hill tapped. After attack (CR 508.1f): `Plus1Plus1 == 0` and no draw, Hill tapped (reach-guard that the tap happened). Revert of equality **or** parser bind flips the attack row back to +1/+1+draw (probe: that is HEAD). | Attack; crew (`CrewFamily`); `{T}` (`TapSymbol`); effect tap (`Effect { source }`); two Hills (only the tapped `SelfRef` source is rewarded); declined Teamwork (default `OptionalPolicy::Decline` — Hill **untapped**, 0 counters; paid path is the reach-guard); vigilance (attack does not tap; 0 counters; Teamwork path still taps). | Hill stays supported; parse-details gains `tap cause`. |
| **C2.2** parser binds frozen identity | `BecomesTapped` arm | `parse_trigger_line` / `parse_oracle_text` | `oracle_trigger_tests.rs` SHAPE: verbatim Hill Oracle | `mode == Taps` && `tap_cause == Some(CostPayment(TapCreatures { origin: Some(Teamwork) }))` && `valid_card == Some(SelfRef)` && execute is `PutCounter` P1P1 on SelfRef with chained Draw (walk `sub_ability`; not `Unimplemented`) | Unqualified Lookout: `tap_cause == None` (sibling). Captain America: `Taps` + `OnlyDuringYourTurn` + `FirstTimeObjectTappedThisTurn` (turn peel must survive). Optional unprinted `"… teamwork cost during your turn"` if cheap as a unit string: `tap_cause` Some **and** turn constraint. | Hill green, qualified. |
| **C2.3** equality; `None` permissive | `match_taps` | registry `Taps` | `trigger_matchers.rs` unit table. Hill-shaped: `valid_card: Some(SelfRef)`, `tap_cause: Some(Teamwork)`. **Must use the Some(valid_card) branch** (probe). | `true` iff event `cause` equals required. `TriggerDefinition::new(Taps)` (string-mapper default) fires on **every** `TapCause` including `AttackDeclaration` and Teamwork. | Against Teamwork-required: `TapCreatures { Kicker }`, `{ origin: None }`, `{ Some(Other) }`, `{ Bargain }`, `TapSymbol`, `CrewFamily(Crew)`, `AttackDeclaration` (**load-bearing, CR 508.1f**), `Effect { source }`, `Enlist`, `Harmonize`, `ManaShard(Convoke)`. First production branch: `if let Some(required) = trigger.tap_cause`. | Unqualified 116 unchanged. |
| **C2.4** unmodelled `to pay` honest; mechanism parser-side | `BecomesTapped` refuse + `check_trigger` | `card_face_gaps` / `build_parse_details_for_face` | Parser: bargain line `matches!(mode, Unknown(_))` **and not** `Taps`, paired with Hill success. Coverage: `card_face_gaps` on parsed bargain face nonempty `Trigger:…`; synthetic `Unknown` (already red at HEAD) remains red. Captain America still supported `Taps`. `tap_cause_reaches_parse_details`: `None` omits key `"tap cause"`; `Some(Teamwork)` emits a distinct value from `Some(TapSymbol)` / `AttackDeclaration`. | Bargain currently parses `Taps` with empty gaps (probe). After: `Unknown` + red gap. Classifier source **unchanged** (i) holds. | Bargain / any `to pay a <unmodelled> cost`. Do **not** use a fixture that is red for Unimplemented execute. | Classifier red/green unchanged. `trigger_details` **gains** the row. |
| **C2.5** no second wire bump; TS empty | protocol pins / `types.ts` | handshake | `node scripts/check-protocol-version.mjs` stays 0 without editing `EXPECTED_*`. `git diff client/src/adapter/types.ts` empty. Cite `p2p-adapter.ts`: "P2P guests never construct this adapter" / host `getHostAdapter()`. | Editing 71→72 or 54→55 fails the script. | If any C2.5 leg failed, wire paths would be admitted — probe says they do not. | N/A |
| **C2.6** other `Taps` readers cause-blind | RA-2 six + `shortcut_efficacy` | AI / index / forge | Source enumeration (below). No production edit. `test-ai` stays green. **No `cargo ai-gate`** unless a reader's verdict changes. | A reader that started matching on `tap_cause` would be a scope admission. | N/A — out of scope unless measurement fails. | N/A |

**Oracle accepted with deferred semantics?** No. Modelled Teamwork is fully live. Unmodelled `to pay` is **not** accepted as `Taps`; it is `Unknown` (coverage red). Unqualified `becomes tapped` remains live and cause-blind.

### Identity / Provenance Contract

| Axis | Contract |
|---|---|
| Source phrase | `"becomes tapped to pay a teamwork cost"` (CR 603.2e + CR 702.194a + CR 601.2h) |
| Selected authority | `PermanentTapped.cause: TapCause`, stamped at tap time by Phase 1 (`pay_tap_creatures_selection` / `tap_permanent_for_cost` with origin derived from `ability.context.additional_cost_payments`) |
| Id / value | `TapCause::CostPayment(TapCostKind::TapCreatures { origin: Some(AdditionalCostOrigin::Teamwork) })` |
| Binding time | **Tap / cost-payment time**, not trigger-scan time |
| Live vs snapshot | The event carries a **Copy** cause. Matcher equality is against that snapshot. Do not re-read in-flight payments when the trigger fires. |
| Storage | Event field `cause`. Trigger field `tap_cause` is the **required** value, parsed from Oracle, not copied from a particular event. |
| Consuming function | `match_taps` equality conjunct **after** subject match (both `valid_card` branches) |
| Invalidation | None beyond the event itself. A later origin change on the spell does not rewrite past `PermanentTapped` events. |
| Multi-authority hostile | Two Hills: only the object whose `object_id` matches `SelfRef` is rewarded. Teamwork+Kicker on one spell: Hill fires only if **her** tap event's origin is Teamwork (Phase 1 stamp), not because a sibling Casualty/Kicker payment exists on the same `PendingCast`. AttackDeclaration must not satisfy a cost-qualified trigger (CR 508.1f) even though the same object becomes tapped. |

---

## Claims this phase must establish (measurement recap)

Phrase as claims, not as restated HEAD facts. HEAD evidence is in Step 3.5.

- **C2.1** — At `PHASE_BASE_SHA`, a non-Teamwork tap of Hill awards +1/+1 and a card; after this phase it awards neither; the Teamwork path still awards both. One test file.
- **C2.2** — `"becomes tapped to pay a teamwork cost"` → `Taps` + `tap_cause == Some(CostPayment(TapCreatures { origin: Some(Teamwork) }))` via nom; unqualified sibling `tap_cause == None`; Captain America turn-constraint does not regress.
- **C2.3** — `Some(x)` fires on `x` only; `None` fires on every `TapCause` (116 cards + `TriggerDefinition::new` / string mappers). Hostiles include `AttackDeclaration` (CR 508.1f).
- **C2.4** — Unmodelled `to pay …` → `Unknown` + red `card_face_gaps`. Classifier path exhibited (synthetic Unknown already red). Captain America still supported. Classifier source unchanged. `trigger_details` gains a `tap cause` row; `None` omits it.
- **C2.5** — No protocol bump. `check-protocol-version.mjs` green on 71/54. TS diff empty. P2P guests construct no engine.
- **C2.6** — Six RA-2 readers (plus `shortcut_efficacy` mode-only) remain cause-blind; same verdict before/after. No `ai-gate`.

### C2.6 reader enumeration (source, this round)

| # | Reader | What it reads | After this field |
|---|---|---|---|
| 1 | `phase-ai/src/policies/self_untap_loop.rs::has_tap_payoff_trigger` | `active.definition.mode` set membership including `Taps` | Unchanged (mode only) |
| 2 | `engine/src/analysis/ability_graph.rs::trigger_axis` | `Taps \| TapAll => AxisKey::Tap` | Unchanged |
| 3 | `engine/src/game/mana_sources.rs::object_self_tap_harm_amount` | `mode == Taps`, then `valid_card`, then `execute` chain | Unchanged (does not read `tap_cause`; Hill's PutCounter/Draw may still count as self-tap harm — pre-existing, not this phase) |
| 4 | `engine/src/game/trigger_index.rs` | `Taps \| TapAll => TriggerEventKey::Taps` | Unchanged |
| 5 | `engine/src/database/forge/trigger.rs` | `"Taps" => Ok(TriggerMode::Taps)` then `TriggerDefinition::new(mode)` | `tap_cause: None` via `::new` (C2.3 permissive) |
| 6 | `engine/src/types/triggers.rs` | `"Taps" => TriggerMode::Taps` | Mode only; callers that `::new` get `None` |

`ai_support/mod.rs` beneficial predictor: `TapsForMana | ManaAbilityProduced` then `_ => false` — **no `Taps` arm**. Cannot over-predict Hill.

`shortcut_efficacy::trigger_event_unreachable_by_confined_action`: `Taps` is in the "confined action **does** produce" table; dispatch is mode-only. Cause-blind. Out of scope.

---

## CR citations (grep-verified against `docs/MagicCompRules.txt`)

Verify again before writing annotations. Numbers used in this plan:

| Number | File line (this round) | Text (summary) |
|---|---|---|
| **CR 701.26** | 3522 | Tap and Untap |
| **CR 701.26a** | 3524 | To tap a permanent, turn it sideways. Only untapped permanents can be tapped. |
| **CR 508.1f** | 2278 | The active player taps the chosen creatures. Tapping a creature when it's declared as an attacker **isn't a cost**. |
| **CR 601.2h** | 2476 | The player pays the total cost. |
| **CR 603.2** | 2565 | Whenever a game event matches a triggered ability's trigger event, that ability automatically triggers. |
| **CR 603.2a** | 2567 | Triggered abilities can trigger when it isn't legal to cast/activate. |
| **CR 603.2e** | 2576 | "Becomes tapped" triggers at the time the named event happens; does not trigger if it enters tapped. |
| **CR 603.4** | 2596 | Intervening-if (Captain America first-tap). Do not regress. |
| **CR 702.194a** | 5437 | Teamwork N = additional cost: tap any number of creatures you control with total power N or more. |
| **CR 702.122a / 702.122b** | 4890 / 4892 | Crew activated ability; a creature crews when tapped to pay that cost. |
| **CR 702.51a** | 4405 | Convoke (stamp already Phase 1; hostile sibling). |
| **CR 702.154a** | 5138 | Enlist. |
| **CR 702.171c** | 5279 | Saddle. |

Annotate: field (`603.2` + `603.2e` + `702.194a` / `601.2h`), equality (`603.2` + `603.2e`), AttackDeclaration refuse (`508.1f`), parser Teamwork bind (`702.194a`), parser refuse (`603.2` — do not emit a different trigger event). Do not invent 701.x / 702.x numbers.

---

## Step 5 — Implementation steps

### 1. Types — `crates/engine/src/types/ability.rs`

Add to `TriggerDefinition` after `room_door` (or with the other optional firing
qualifiers; keep serde skip-none):

```rust
/// CR 603.2 + CR 603.2e: optional required [`TapCause`] for `TriggerMode::Taps`.
/// `None` is the unqualified "becomes tapped" class and is cause-blind at
/// match time. `Some(x)` fires only when `PermanentTapped.cause == x`.
/// Agent Maria Hill: `Some(CostPayment(TapCreatures { origin: Some(Teamwork) }))`
/// (CR 702.194a + CR 601.2h). Bound from Oracle at parse time; compared to the
/// event stamp, never re-derived.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub tap_cause: Option<TapCause>,
```

Import `TapCause` from `crate::types::events`.

`TriggerDefinition::new`: `tap_cause: None`.

`trigger_definition_roundtrip`: `tap_cause: None` (or `Some` if the test wants
to prove skip-serializing — prefer `Some(AttackDeclaration)` **and** a `None`
omit test in coverage instead of overloading this literal).

### 2. Standing-class exhaustives (as the compiler names them)

- `ability_scan.rs` `scan_trigger_definition`: `tap_cause: _` next to
  `clash_result: _`. Comment: event-shape discriminator; no `TargetFilter` /
  `QuantityExpr`; a required cause can only **narrow** which `Taps` triggers
  fire, so it cannot inflate growing-class census.
- `quantity.rs` `trigger_definition_is_cast_stable_for_pre_cast`: `tap_cause: _`
  (same as `attack_target_filter: _` / `clash_result: _`).
- `parser/audit_projection.rs` `TRIGGER_KEYS`: `"tap_cause"` after `"room_door"`.
- Do not edit `ability_rw.rs` unless the compiler demands it (it should not).

### 3. Matcher — `crates/engine/src/game/trigger_matchers.rs`

**Re-read `match_taps`.** Current shape (HEAD): `valid_card.is_some()` branch
runs `valid_card_matches`, then the opponent-tap `TapCause` gate, then
**unconditional `true`**. Else branch: `*object_id == source_id`. **No equality.**

Insert the equality gate on **both** subject-success paths. Preferred
structure (do not duplicate the gate):

```rust
if let GameEvent::PermanentTapped { object_id, cause } = event {
    let subject_ok = if trigger.valid_card.is_some() {
        if !valid_card_matches(trigger, state, *object_id, source_context) {
            return false;
        }
        // existing requires_opponent / TapCause opponent-tap gate — unchanged
        // ...
        true
    } else {
        *object_id == source_id
    };
    if !subject_ok {
        return false;
    }
    // CR 603.2 + CR 603.2e: a cost-qualified "becomes tapped" trigger matches
    // only its required cause. None is the unqualified class (permissive).
    if let Some(required) = trigger.tap_cause {
        if *cause != required {
            return false;
        }
    }
    true
} else {
    false
}
```

Opponent-tap gate and equality gate are **independent conjuncts**. Hill:
`requires_opponent == false`; equality is the fix.

Registry: already `r.insert(TriggerMode::Taps, match_taps)`. Do not add a second
insert.

### 4. Parser — `crates/engine/src/parser/oracle_trigger.rs`

Imports (file top): `space0` beside `space1`; `TapCause`, `TapCostKind` beside
`ClashResult`.

Add `parse_modelled_to_pay_cause` next to `parse_leading_turn_constraint`.

Rewrite **only** the `SimpleEvent::BecomesTapped` arm as in Nom Compliance.
Keep the existing CR 603.2e comment on the turn peel; add CR 702.194a /
CR 603.2 on the `to pay` bind/refuse.

`lower_trigger_ir` clones `ir.partial_def` and overwrites `constraint` from
modifiers. It does **not** touch a new field — `tap_cause` on `partial_def`
survives. Do not add a modifiers slot.

`make_base()` uses `TriggerDefinition::new` → `tap_cause: None` automatically.

### 5. Coverage — `crates/engine/src/game/coverage.rs`

**Do not** change `build_trigger_item` / `check_trigger` / `trigger_has_unimplemented_parts`.

In `trigger_details`, after `attack_target_filter` / with `constraint` and
`condition` (firing qualifiers):

```rust
if let Some(cause) = trig.tap_cause {
    d.push(("tap cause".into(), fmt_tap_cause(cause).into()));
}
```

Key **`"tap cause"`**, not `"cause"` (would collide with unrelated labels).
Omit when `None` (#5507).

`fmt_tap_cause` exhaustive:

| Value | Label |
|---|---|
| `AttackDeclaration` | `"attack declaration"` |
| `Effect { .. }` | `"effect"` (no `ObjectId` in parse-details) |
| `CostPayment(TapSymbol)` | `"tap symbol"` |
| `CostPayment(TapCreatures { origin: None })` | `"tap creatures"` |
| `CostPayment(TapCreatures { origin: Some(Teamwork) })` | `"teamwork cost"` |
| `Some(Kicker)` | `"kicker cost"` |
| `Some(Casualty)` | `"casualty cost"` |
| `Some(Offspring)` | `"offspring cost"` |
| `Some(Squad)` | `"squad cost"` |
| `Some(Replicate)` | `"replicate cost"` |
| `Some(Bargain)` | `"bargain cost"` |
| `Some(Gift)` | `"gift cost"` |
| `Some(Other)` | `"additional cost"` |
| `ManaShard(Convoke/Waterbend/Improvise/Delve)` | `"convoke"` / `"waterbend"` / `"improvise"` / `"delve"` |
| `CrewFamily(Crew/Saddle/Station)` | `"crew"` / `"saddle"` / `"station"` |
| `Enlist` | `"enlist"` |
| `Harmonize` | `"harmonize"` |

Parser only **produces** `"teamwork cost"` today; the formatter still covers
the `TapCause` class so matcher/test fixtures and future origins render.

Add `tap_cause_reaches_parse_details` next to `attack_target_filter_reaches_parse_details`.

### 6. Frontend — `client/src/adapter/types.ts`

Open. Confirm `trigger_definitions: unknown[]`. **Make no edit** unless that
line is no longer `unknown[]` (if it is typed, stop and admit a TS mirror —
C2.5 failed). Expected diff empty.

### 7. Tests

**SHAPE — `oracle_trigger_tests.rs`**

- Verbatim Hill Oracle → `Taps` + `tap_cause` Teamwork + `SelfRef` + PutCounter/Draw chain (not Unimplemented).
- Lookout / existing `trigger_becomes_tapped` / `becomes_tapped_without_turn_phrase_has_no_constraint`: assert `tap_cause.is_none()` (keep existing asserts).
- Captain America test: keep constraint+condition; add `tap_cause.is_none()`.
- Bargain: `Whenever Bargain Probe becomes tapped to pay a bargain cost, draw a card.` → `matches!(Unknown(_))` and **not** `Taps`. Reach-guard: Hill test in the same module proves the arm still produces `Taps` when modelled.
- Do not paraphrase Hill (`her` / name normalization must run on the real line).

**Matcher — `trigger_matchers.rs` cfg(test)**

Hill-shaped `SelfRef` + `tap_cause: Some(Teamwork)` table (C2.3 hostiles).
Permissive `::new(Taps)` + `SelfRef` fires on `AttackDeclaration` **and** Teamwork
(paired). Do not weaken existing opponent-tap tests.

**Integration — `crates/engine/tests/integration/issue_6876_agent_maria_hill.rs`**

```rust
const HILL: &str = "Whenever Agent Maria Hill becomes tapped to pay a teamwork cost, put a +1/+1 counter on her and draw a card.";
const TEAMWORK: &str = "Teamwork 1 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 1 or more.)\nYou gain 1 life.";
```

Library: `with_library_top(P0, &["LibA", "LibB"])`.

Teamwork recipe (Phase 1 analog): `add_creature_from_oracle(P0, "Agent Maria Hill", 2, 1, HILL)`;
spell `add_spell_to_hand_from_oracle` + `from_oracle_text_with_keywords(&["teamwork:1"], TEAMWORK)` +
`with_mana_cost(ManaCost::generic(0))`; `.cast(spell).accept_optional().pay_cost_with(&[hill]).resolve()`.

Assert `CounterType::Plus1Plus1` and `assert_hand_drawn(P0, 1)` and Hill tapped.

Hostiles in the same file (each: 0 counters, 0 extra cards, with Teamwork as reach-guard):

| Hostile | Drive | First branch reached |
|---|---|---|
| Attack | `advance_to_combat` + `DeclareAttackers` + `advance_until_stack_empty` (Captain America integration analog) | `tap_cause` equality vs `AttackDeclaration` |
| Effect tap | Instant `"Tap target creature."`, `generic(0)`, `.target_object(hill)` | equality vs `Effect { source }` |
| Crew | Artifact Vehicle, `from_oracle_text_with_keywords(&["crew:1"], …)`, `GameAction::CrewVehicle { creature_ids: vec![hill] }` | equality vs `CrewFamily(Crew)` |
| `{T}` | **Fixture extra ability**, not Hill Oracle: `with_ability_definition(AbilityDefinition::new(Activated, Draw{1, Controller}).cost(AbilityCost::Tap))` then `runner.activate(hill, idx).resolve()` | equality vs `TapSymbol` |
| Two Hills | Two `add_creature_from_oracle` copies; pay with `[hill_a]` only | `valid_card` SelfRef — `hill_b` unrewarded |
| Declined Teamwork | omit `.accept_optional()` (default Decline) | Hill **untapped** (optional cost not paid) |
| Vigilance | `with_keyword(Keyword::Vigilance)` on Hill, then attack | no tap event (CR 702.20 / 508.1f does not tap) |

Bargain honesty is **parser/coverage**, not this runtime file (no printed card).

`mod issue_6876_agent_maria_hill;` in `tests/integration/main.rs`. **Never** a
new top-level file under `crates/engine/tests/`.

### 8. Protocol / AI / wire

**Do not bump** `PROTOCOL_VERSION` (71) or `WIRE_PROTOCOL_VERSION` (54).
Do not rename `protocol_version_is_71_for_tap_cause`.
Do not edit RA-2 readers.
Do not run `cargo ai-gate` unless C2.6 fails.

### 9. Format and verify

Always: `cargo fmt --all`.

Tilt is **down**. Do **not** use workspace `target/`. Isolated dir
`CARGO_TARGET_DIR=/tmp/phase-plan-6876-p2` (reuse; do not delete):

```
CARGO_TARGET_DIR=/tmp/phase-plan-6876-p2 CARGO_INCREMENTAL=0 cargo clippy -p phase-engine --all-targets -- -D warnings
CARGO_TARGET_DIR=/tmp/phase-plan-6876-p2 CARGO_INCREMENTAL=0 cargo test -p phase-engine --test integration issue_6876_agent_maria_hill -- --test-threads=1
CARGO_TARGET_DIR=/tmp/phase-plan-6876-p2 CARGO_INCREMENTAL=0 cargo test -p phase-engine --lib oracle_trigger_tests trigger_becomes_tapped captain_america tap_cause -- --test-threads=1
# plus match_taps / coverage unit tests similarly
node scripts/check-protocol-version.mjs
```

If Tilt is up at implementation time: `./scripts/tilt-wait.sh clippy test-engine card-data check-frontend test-ai` after `cargo fmt --all`. Still do not `cargo clean`.

`cargo coverage` (one-shot, not Tilt): Agent Maria Hill remains supported; parse-details for her trigger include `tap cause=teamwork cost`. Status moves from wrongly-unqualified-supported to correctly-qualified-supported — not a red→green flip.

Frontend: no UI behavior change; no browser pass required if `types.ts` is untouched. If a TS edit appears, that is a C2.5 failure — stop.

---

## What this phase must not do

- Add `TriggerMode::TapsToPayTeamwork` or any Teamwork-shaped mode.
- Blanket-reject `BecomesTapped` remainder (Captain America).
- Bind `Bargain` / `Kicker` / other `AdditionalCostOrigin` names into the
  combinator without a printed `"becomes tapped to pay a <that> cost"` card.
- Re-derive origin inside `match_taps` from `additional_cost_payments`.
- Put the equality gate only on `valid_card.is_none()` (would miss Hill).
- Bump protocol 71/54.
- Edit the six RA-2 readers or `ai_support/mod.rs`.
- Edit `build_trigger_item` / `check_trigger` unless C2.4(i) is contradicted.
- Feed Teamwork reminder text as the only keyword source.
- Add a top-level `crates/engine/tests/*.rs` binary.
- Implement from Phase 1 plan text of `match_taps` without re-reading the file.
- Leave Hill's execute as `Unimplemented` and call the attack negative a test.

---

## Held green across the seam

After Phase 1, every trigger's `tap_cause` is absent and `match_taps` ignores
causes except the opponent-tap gate, so unqualified `Taps` still fire (C1.1 /
C2.3 `None`). After Phase 2, those 116 keep `None` and still fire. The only
intended firing change is Hill (and any future modelled `to pay` bind). Coverage
honesty: Hill was wrongly green and stays green for a **correct** reason;
unmodelled `to pay` becomes red. Captain America stays green with constraint +
intervening-if.
