# Phase 1 plan — Issue #6876 (stamp `TapCause` on every tap)

Phase-plan mode for `/engine-implementer`. Frozen design is charter
`/workspace/.claude/wf/issue-6876-tap-cause-charter.md` §1 — **not re-planned**.
This document is the Phase 1 implementation plan only. Do not implement from a
partial reading; every architectural section below is load-bearing.

- **Phase index:** 1 of 2
- **Goal:** typed `TapCause` on every `PermanentTapped`; translate the opponent-tap
  gate in `match_taps`; thread the in-flight additional-cost origin as a
  **parameter** into `pay_tap_creatures_selection` from
  `ability.context.additional_cost_payments`. No trigger gains a cost qualifier.
  #6876 remains live (Hill still wrongly-supported until Phase 2).
- **Prior phases:** none
- **Worktree HEAD at plan time:** `a9bf90be3` (`cursor/general-bug-4e55`). Charter
  `BASE_SHA 0f1a35b4de154ca969ca2e8f9ed6db85225c7dc3`.
- **Tilt:** down at plan time (`tilt` exit 127). Isolated probe target
  `CARGO_TARGET_DIR=/tmp/phase-plan-6876-p1` (same 254G disk as `/workspace`;
  reused, not deleted).

---

## Step 0 — Premise verification

Fetched `https://api.scryfall.com/cards/named?exact=Agent%20Maria%20Hill`
independently this round (oracle_id `a2d90efe-80f2-4069-b135-a8bd00ccd2b0`,
MSH 2):

> Whenever Agent Maria Hill becomes tapped to pay a teamwork cost, put a +1/+1
> counter on her and draw a card.

Legendary Creature — Human Spy Hero · `{W}` · 2/1.

Matches the issue body and the charter verbatim. **Premise PASSES.**

C1.5(e) alternative-cost class independently re-verified:
`https://api.scryfall.com/cards/named?exact=The%20Lady%20of%20Otaria`:

> You may tap three untapped Dwarves you control rather than pay this spell's
> mana cost. …

That is the Lady-of-Otaria / Ramosian Rally class the origin helper must yield
`None` for (not `Some(Other)`, not a stale earlier payment).

`AdditionalCostOrigin::Teamwork` and
`effective_teamwork_additional_cost_instances` already queue the Teamwork
additional cost. Teamwork keyword landing is **not** this phase.

---

## Step 1 — Applicable skills

| Skill | This phase |
|---|---|
| `/add-engine-variant` | **Mandatory.** `TapCause` and `TapCostKind` are new engine enums. Stage verdicts below. |
| `/add-trigger` | Phase 1 of that checklist is **not** a new `TriggerMode`. Event-emission retype + opponent-tap gate translation are this phase. Parser, equality gate, Hill integration: **`DEFERRED(phase 2)`**. |
| `/card-test` | Hill discriminating cast-pipeline test is **`DEFERRED(phase 2)`**. Phase 1's C1.2 flipped-firing test is a `match_taps` / replacement-resume unit table, not a Hill `GameRunner::cast` test. |
| `/oracle-parser` | **Must not touch.** Nom Compliance = N/A. |
| `/casting-stack-conditions` | Origin is **derived** at the existing tap-creatures payment site; no new `WaitingFor` / `PendingCast` field. Consulted as the map of that site, not as a checklist to extend. |
| `/add-engine-effect`, `/add-keyword`, `/add-static-ability`, `/add-replacement-effect`, `/add-interactive-effect`, `/add-frontend-component`, `/add-ai-feature-policy`, `/add-card-data-pipeline` | Do not apply. |

### `/add-trigger` checklist (every step present)

- **Phase 1 — Type Definition** (`TriggerMode` / `TriggerCondition` / `TriggerConstraint`): **N/A this phase** (existing `TriggerMode::Taps`). `TriggerDefinition.tap_cause`: **`DEFERRED(phase 2)`**.
- **Phase 2 — Event Emission** (`types/events.rs` + emit sites): **this phase** — retype `PermanentTapped.caused_by` → `cause: TapCause` and stamp every producer.
- **Phase 3 — Matcher** (`match_taps` opponent-tap gate): **this phase**, translation only. Equality gate (`trigger.tap_cause`): **`DEFERRED(phase 2)`**. Registry insert: already `TriggerMode::Taps` → `match_taps`; do not add a mode.
- **Phase 4 — Target Extraction:** N/A (Hill's execute does not retarget via this change).
- **Phase 5 — Parser** (`oracle_trigger.rs` `"to pay a teamwork cost"`): **`DEFERRED(phase 2)`**.
- **Phase 6 — Condition/Constraint:** N/A.
- **Phase 7 — Stack Resolution:** N/A.
- **Phase 8 — Tests:** C1.3/C1.5/C1.6 unit tables and the C1.2 resume stamp + flipped gate: this phase. Hill parser + matcher + integration (`/card-test` recipe): **`DEFERRED(phase 2)`**. APNAP / once-per-turn: N/A (no new trigger).

### `/card-test` checklist (Hill)

All six foot-guns and the `GameScenario` + `GameRunner::cast(..).resolve()` +
`CastOutcome` recipe for Agent Maria Hill: **`DEFERRED(phase 2)`**. Do not add a
Hill cast-pipeline test in Phase 1 — no card observes a tap cause until the
consumer lands. That absence is the infrastructure→consumer seam, not a skipped
checklist item.

### `/add-engine-variant` checklist

Complete stage verdicts are in **Variant Discoverability**. After approval:
CR annotations (grep-verified below), exhaustive `match` (compiler),
ability-scan / ability-rw **N/A** (`TapCause` lives on `GameEvent`, which those
walkers do not traverse), runtime status = live (stamped at every producer in
the same change — no `RUNTIME: TODO` stub), serialized-surface = protocol bump
70→71 / wire 53→54 (C1.4). No converter pairing (this is not a Forge/JSON
ability AST variant).

---

## Frozen design (inherited verbatim — do not change)

- **REFUSE** `TriggerMode::TapsToPayTeamwork`.
- **CREATE** `TapCause { AttackDeclaration, CostPayment(TapCostKind), Effect { source } }`.
- **CREATE** `TapCostKind { TapSymbol, TapCreatures { origin: Option<AdditionalCostOrigin> }, ManaShard(ConvokeMode), CrewFamily(CrewAction), Enlist, Harmonize }`.
- **Identity:** Hill fires **iff**
  `cause == TapCause::CostPayment(TapCostKind::TapCreatures { origin: Some(AdditionalCostOrigin::Teamwork) })`.
  Bound at tap time by the payment site; never re-derived at trigger time.
- Phase 1 stamps; Phase 2 consumes. No trigger in this phase gains a cost qualifier.

---

## Deferral allowlist (must not be omitted)

| Item | Attribution |
|---|---|
| `TriggerDefinition.tap_cause` | **`DEFERRED(phase 2)`** |
| `match_taps` equality gate | **`DEFERRED(phase 2)`** |
| Parser `"to pay a teamwork cost"` | **`DEFERRED(phase 2)`** |
| Coverage honesty / `trigger_details` | **`DEFERRED(phase 2)`** |
| Hill discriminating cast-pipeline test | **`DEFERRED(phase 2)`** |
| Second `PROTOCOL_VERSION` bump | **`DEFERRED(phase 2)`** as an allowlist row; Phase 2 charter then decides it is **not owed** (RA-1 / C2.5). Phase 1 must not take a second bump. |
| `PendingCast` origin field | **`DEFERRED(phase 2)`** as an allowlist row; **not owed** — withdrawn at C1.5. Phase 1 must not add it. `types/game_state.rs` stays out of scope. |

---

## Step 2 — Analogous trace

**Primary analogue — effect vs cost tap stamping (the retype's existing split):**

`crates/engine/src/game/effects/tap_untap.rs::process_one_tap`
→ constructs `ProposedEvent::Tap { object_id, applied }` (**drops `source_id`**)
→ `replacement::replace_event`
→ Execute arm stamps `GameEvent::PermanentTapped { caused_by: Some(source_id) }`
→ NeedsChoice parks on `PendingReplacement.proposed`
→ `crates/engine/src/game/engine_replacement.rs` resume arm stamps
`caused_by: None`.

Cost taps never enter that pipeline:
`crates/engine/src/game/restrictions.rs::tap_permanent_for_cost`
→ `PermanentTapped { caused_by: None }` for every cost.

**Secondary analogue — identity bound at produce time, consumed later:**

`ThisWayCause` (`types/ability.rs`) is stamped onto tracked-set members when the
producer action happens and later equality-gated by consumers. `TapCause` is
the same contract applied to "becomes tapped": the event carries the identity;
the trigger (Phase 2) compares it. Do **not** reuse `ThisWayCause` — it names
keyword *actions that move cards*, a different CR layer (608.2c vs 701.26).

**Tertiary analogue — proposed-event source recoverability:**

`ProposedEvent::Destroy { source: Option<ObjectId>, .. }` and
`ProposedEvent::Discard { source_id, .. }` already thread a causing object
through the replacement pause. `ProposedEvent::Tap` is the missing sibling.
Widening it is how C1.2 stamps `Effect { source }` at resume without a
`PendingReplacement` watermark.

**Teamwork origin analogue (already landed):**

`effective_teamwork_additional_cost_instances` (`casting_costs.rs`) queues
`AdditionalCostOrigin::Teamwork` as `Optional { Once, cost: TapCreatures { total_power_at_least } }`.
`handle_decide_additional_cost` records that origin **before** popping, then
`pay_additional_cost_with_source` suspends into `PayCost { TapCreatures }`.
Phase 1 **reads** that recording at `handle_tap_creatures_for_spell_cost`; it
does not invent a second home.

Full trace path for this phase:

`types/events.rs` (retype)
→ `types/proposed_event.rs` (C1.2 widen — **admitted**)
→ `game/effects/tap_untap.rs` (Effect stamp + proposed source)
→ `game/engine_replacement.rs` (resume Effect stamp)
→ `game/restrictions.rs` (`tap_permanent_for_cost` gains a `TapCause`)
→ ten `tap_permanent_for_cost` callers (C1.6)
→ `game/casting_costs.rs` (origin derivation + auto-tap land fallback)
→ `game/combat.rs` (AttackDeclaration)
→ `game/mana_sources.rs` / `ai_support/mod.rs` (TapSymbol literals)
→ `game/public_state.rs` (dirty only `Effect { source }`)
→ `game/trigger_matchers.rs` (opponent-tap gate translation)
→ wire constants (C1.4).

---

## Step 3 — Files this phase will touch

Literal paths. Standing classes (compiler-forced tests, shared registration,
comment-only protocol changelog lines) implied.

**Types**

- `crates/engine/src/types/events.rs` — define `TapCause` / `TapCostKind`; retype the field.
- `crates/engine/src/types/proposed_event.rs` — **admitted by C1.2** (measured: source is not recoverable at resume without this widen).
- `crates/engine/src/types/ability.rs` — `AbilityCost::contains_tap_creatures` only (recursive cost-tree predicate). No `TriggerDefinition.tap_cause`. No parser edits.

**Producers**

- `crates/engine/src/game/restrictions.rs`
- `crates/engine/src/game/effects/tap_untap.rs`
- `crates/engine/src/game/combat.rs`
- `crates/engine/src/game/engine_replacement.rs`
- `crates/engine/src/game/casting_costs.rs`
- `crates/engine/src/game/mana_sources.rs`
- `crates/engine/src/ai_support/mod.rs`

**Cost-payment callers (C1.6)**

- `crates/engine/src/game/engine_combat.rs`
- `crates/engine/src/game/engine_casting.rs`
- `crates/engine/src/game/costs.rs`
- `crates/engine/src/game/mana_abilities.rs`
- `crates/engine/src/game/engine.rs`

**Readers**

- `crates/engine/src/game/public_state.rs`
- `crates/engine/src/game/trigger_matchers.rs`

**Wire**

- `crates/lobby-broker/src/protocol.rs`
- `client/src/adapter/ws-adapter.ts`
- `client/src/network/protocol.ts`
- `scripts/check-protocol-version.mjs`
- `crates/server-core/src/protocol.rs`
- `client/src/adapter/types.ts` — **permissive, not required** (`PermanentTapped` arm is `{ object_id }` only).
- Tests (excluded from T2): `client/src/network/__tests__/protocol.test.ts`, `client/src/adapter/__tests__/p2p-adapter-multiplayer.test.ts`.

**Out of scope**

- `crates/engine/src/types/game_state.rs` (PendingCast field withdrawn).
- `crates/engine/src/parser/**` (Nom Compliance N/A).
- `crates/engine/src/game/ability_scan.rs` / `ability_rw.rs` (`GameEvent` is not walked).
- Lobby floors `MIN_LOBBY_PROTOCOL_FOR_*`, `LOBBY_PROTOCOL_VERSION`.

**Advisory (C1.0)** — rest-pattern readers of `PermanentTapped { .. }` / `{ object_id, .. }`. They compile through a field rename. Re-enter `SCOPE_PATHS` only with a compiler error as evidence. Measured examples: `game/log.rs`, `game/trigger_index.rs`, `game/triggers.rs` (`observe_object_taps`), `game/targeting.rs` (subject extract), `game/effects/mod.rs` (anaphor), `trigger_matchers.rs` (unrelated `{ object_id, .. }` arm). Animation TS string keys (`client/src/animation/types.ts`, `eventNormalizer.ts`) do not read the payload.

---

## Step 3.5 — Probe results (isolated `CARGO_TARGET_DIR=/tmp/phase-plan-6876-p1`)

Throwaway `crates/engine/tests/integration/probe6876_p1.rs` was compiled and run
against a real `GameScenario` board, then deleted after this plan was written.
Regenerate, do not treat line numbers as durable.

Command:

```
CARGO_TARGET_DIR=/tmp/phase-plan-6876-p1 cargo test -p phase-engine --test integration probe_ -- --nocapture --test-threads=1
```

### C1.5(a)(b)(d) — Teamwork after Casualty (Twin Strike oracle, real keywords)

Reached `WaitingFor::PayCost { kind: TapCreatures { mode: Aggregate(..) } }`
(positive reach-guard: `payer_tapped_after=true`). At that seam:

- `pending.additional_cost_queue` **empty** (once-instance already popped). **(a) holds.**
- `additional_cost_payments == [(Casualty, 0, 1), (Teamwork, 0, 1)]`;
  `.last() == Teamwork`. **(b) and (d) hold** — newest recorded payment is the
  in-flight tap cost; Casualty remains present and unselected.
- `obj.additional_cost` is still the **Casualty sacrifice** (face additional).
  Face additional is **not** the in-flight origin authority.
- `additional_cost_flow == None`.
- `state.pending_cast` is `None` at the `PayCost` prompt (the pending rides
  `CostResume::Spell`, not `state.pending_cast`). Derivation must read the
  boxed resume pending, which `handle_tap_creatures_for_spell_cost` already
  receives as `pending`.

### C1.5(e) — three windows that must not steal a tap-shaped origin

**Activation** (`Tap an untapped creature you control:`):

- `activation_ability_index == Some(0)`
- `activation_cost == None` (**TapCreatures stripped** by
  `surface_next_unpaid_interactive_activation_cost` before boxing)
- `pending.cost == ManaCost::NoCost`
- `alternative_mana_cost_paid == false`
- `payments == []`
- Reach-guard: helper creature actually tapped.

**Alternative** (Lady-of-Otaria class, simplified “tap an untapped creature
rather than pay mana”):

- `activation_ability_index == None`
- `pending.cost == ManaCost::NoCost`
- `alternative_mana_cost_paid == true` (Choice accept of a spell alternative)
- `payments == []`, queue empty
- Reach-guard: dwarf actually tapped.

**Required additional TapCreatures** (stamped `AdditionalCost::Required` on
face; the 7043 pay-directly bypass):

- `activation_ability_index == None`
- `pending.cost == Cost { shards: [], generic: 0 }` (**printed mana, not `NoCost`**)
- `alternative_mana_cost_paid == false`
- `payments == []` (**unrecorded**)
- `additional_cost_flow == None` (never published onto the pending in this bypass)
- `obj.additional_cost == Some(Required(TapCreatures { Count 1, You creature }))`
- Reach-guard: payer actually tapped.

These three plus the Teamwork row are the narrowing proof. A `.last()` that
ignored cost shape would not misfire on the empty-payment windows, but **would**
misfire if a required tap followed a recorded Casualty (last = Casualty, not
tap-shaped) or if an activation tap followed a recorded additional payment on
the same pending. The helper specified in C1.5(e) below is what closes those.

### C1.5(c) — `pay_tap_creatures_selection` callers (source enumeration, not a probe snapshot)

```
rg -n 'pay_tap_creatures_selection\(' crates/engine/src
```

Production: (1) `casting_costs::handle_tap_creatures_for_spell_cost` — **thread
origin**; (2) `engine.rs` `CostResume::Resolution` — **`origin: None`**.
Mana-ability tap does **not** call this function (`mana_abilities::tap_selected_creature_for_mana_cost`
is C1.6). Remaining hits are `costs.rs` `#[cfg(test)]`.

### C1.2 — `ProposedEvent::Tap` has no source; resume stamps `caused_by: None`

Probe: “Tap target creature.” + optional `ReplacementEvent::Tap` on the
opponent creature. Reached `WaitingFor::ReplacementChoice`.
`pending_replacement.proposed` printed as `Tap { object_id, fields: object_id+applied only }`.
Accept emitted `PermanentTapped { caused_by: None }`. Victim tapped.

Production constructor `process_one_tap` **has** `source_id` and stamps
`caused_by: Some(source_id)` on the Execute path, but **does not put it on
`ProposedEvent::Tap`**. Resume therefore cannot recover it without widening.
Analogues already carry source: `Destroy { source }`, `Discard { source_id }`.
`PendingReplacement` has no tap-source field — do not add one.

**C1.2 resolution: widen `ProposedEvent::Tap` with `source_id: ObjectId`
(required, not `Option`). Admit `types/proposed_event.rs`.** Every production
`ProposedEvent::Tap` is `process_one_tap` (regenerate:
`rg -n 'ProposedEvent::Tap \{' crates/engine/src` — production construction is
only `tap_untap.rs`; tests in `engine_replacement.rs` /
`replacement.rs`). This is the single intended new firing: opponent-tap gate
currently refuses resume taps as self-initiated; after stamping
`Effect { source }` it fires.

Existing unit test `tap_replacement_accepted_applies_tap`
(`engine_replacement.rs`) already reaches the resume arm; it must gain a
`source_id` in its `ProposedEvent::Tap` literal and assert the emitted cause.

### C1.6 — named site is `TapCreatures`, not `TapSymbol`

Springleaf Drum class (`{T}, Tap an untapped creature you control: Add one mana
of any color.`): the creature-tap window is `PayCostKind::TapCreatures { mode: Fixed }`
with `CostResume::ManaAbility`. Helper actually tapped. Assignment:
`TapCostKind::TapCreatures { origin: None }`. The `{T}` on the Drum itself is a
separate `tap_source` → `TapSymbol` call. `origin: None` is the activation /
mana-ability fact `Option<AdditionalCostOrigin>` exists to express; it is not
collapsible with `TapSymbol` and is not a Teamwork payment.

### C1.0 — forced vs rest

```
rg -n 'GameEvent::PermanentTapped\s*\{' crates/engine/
```

Non-test **constructions** (must name `cause:` after the retype):

| Site | Today | Stamp |
|---|---|---|
| `tap_untap.rs::process_one_tap` Execute | `caused_by: Some(source_id)` | `Effect { source: source_id }` |
| `engine_replacement.rs` resume | `caused_by: None` | `Effect { source: proposed.source_id }` after widen |
| `restrictions.rs::tap_permanent_for_cost` | `caused_by: None` | the `TapCause` parameter |
| `combat.rs::commit_attack_declaration` | `caused_by: None` | `AttackDeclaration` |
| `mana_sources.rs` land tap | `caused_by: None` | `CostPayment(TapSymbol)` |
| `ai_support/mod.rs` mana-probe literal | `caused_by: None` | `CostPayment(TapSymbol)` |
| `casting_costs.rs` auto-tap basic-land fallback | `caused_by: None` | `CostPayment(TapSymbol)` — **does not** go through `tap_permanent_for_cost`; stamp in place, do not reroute |

No hidden non-test construction. **C1.0 holds.** Rest-pattern readers are
advisory. Named `caused_by` destructures that **are** forced:
`public_state.rs`, `match_taps`. Test-module literals are the standing
compiler-forced class (`trigger_matchers.rs` tests, `targeting.rs` tests,
`delayed_trigger.rs` test, `effects/mod.rs` tests, `triggers.rs` tests,
`gran_gran_integration.rs`, `triggers_dedup_regression_tests.rs`, plus
`ProposedEvent::Tap` literals in `engine_replacement.rs` /
`replacement.rs` after the widen).

---

## C1.5(e) — narrowing mechanism (chosen; no `PendingCast` watermark)

Charter obligation: a payment window that recorded no payment of its own must
yield `None`, not a stale origin; unrecorded required additional TapCreatures
must yield `Some(Other)`, not `None`. No new pending field.

**Chosen helper** (lives next to the only production caller that may inspect a
pending): `casting_costs::in_flight_tap_creatures_origin(state, pending) -> Option<AdditionalCostOrigin>`,
called only from `handle_tap_creatures_for_spell_cost`. No other
`tap_permanent_for_cost` caller may inspect a pending cast.

### Predicates (probed)

Add `AbilityCost::contains_tap_creatures(&self) -> bool` in `types/ability.rs`,
recursive over `Composite` / `OneOf` / `PerCounter { base }` the same way
`consumes_source` already walks the cost tree. **Do not** touch
`oracle_trigger.rs::cost_contains_tap_creatures` (parser-private, Composite-only,
out of Phase 1). A small `additional_cost_contains_tap_creatures` private fn in
`casting_costs.rs` covers `AdditionalCost::{Required, Optional, Choice, Kicker}`.

**Do not** treat last `Other` as tap-shaped blindly: Choice-card additional can
record `Other` for a non-tap cost. `Other` is tap-shaped iff face
`obj.additional_cost`, `pending.additional_cost_flow`, or
`effective_conspire_additional_cost(state, player, pending.object_id)` contains
TapCreatures.

Static origin→shape (from `build_effective_additional_cost_queue` + keyword
definitions; regenerate via that function, not a remembered list):

| `AdditionalCostOrigin` | TapCreatures-shaped? |
|---|---|
| `Teamwork` | **always** (`effective_teamwork_additional_cost_instances`) |
| `Casualty`, `Bargain`, `Gift`, `Offspring`, `Squad`, `Replicate`, `Kicker` | **never** |
| `Other` | **not static** — inspect face / flow / conspire as above |

### Algorithm (order is load-bearing)

Activation pendings are also `ManaCost::NoCost` (probed). Alternative detection
by `NoCost` **must not** run first.

```
fn in_flight_tap_creatures_origin(state, pending) -> Option<AdditionalCostOrigin> {
    // 1. Activation-cost TapCreatures window: surface_next_unpaid_interactive
    //    strips TapCreatures from activation_cost before boxing. Additional-cost
    //    taps on an activated ability still have that leg on activation_cost
    //    (additional costs are paid first).
    if pending.activation_ability_index.is_some()
        && !pending.activation_cost.as_ref().is_some_and(AbilityCost::contains_tap_creatures)
    {
        return None;
    }

    // 2. Newest recorded payment whose origin is tap-creatures-shaped.
    if let Some(p) = pending.ability.context.additional_cost_payments.iter().rev()
        .find(|p| origin_is_tap_creatures_shaped(state, pending, p.origin))
    {
        return Some(p.origin);
    }

    // 3. Alternative-cost window (Lady of Otaria / timing-required non-mana alt).
    //    Probed accept path sets alternative_mana_cost_paid and pending.cost=NoCost.
    //    Timing-required non-mana alt (~6982) does NOT set the flag; those pendings
    //    are built with ManaCost::NoCost (~6952) and empty queue. Required face
    //    additional uses the printed mana cost (probed: Cost { generic: 0 }, not
    //    NoCost), so it cannot fall into this arm.
    if pending.activation_ability_index.is_none() && matches!(pending.cost, ManaCost::NoCost) {
        return None;
    }

    // 4. Unrecorded Required additional TapCreatures (7043 bypass / 1201 queue pop).
    Some(AdditionalCostOrigin::Other)
}
```

### Proof against the two windows the charter named

| Window | Arm taken | Result | Why not the other arms |
|---|---|---|---|
| Activation TapCreatures | 1 | `None` | `activation_ability_index` Some and `activation_cost` stripped. `NoCost` is true but must not be used (would also be None, but would mis-fire if an additional tap-shaped payment existed on the same pending — arm 1 refuses to walk payments once stripped). |
| Alternative tap (Lady of Otaria) | 3 | `None` | Not activation; payments empty so arm 2 misses; `cost == NoCost`. |
| Teamwork after Casualty | 2 | `Some(Teamwork)` | Not activation; newest tap-shaped payment is Teamwork (Casualty is never tap-shaped). Face additional is still Casualty — ignored. |
| Required additional tap, no prior payment | 4 | `Some(Other)` | Not activation; payments empty; `cost` is printed mana, not `NoCost`. |
| Required additional tap after recorded Casualty | 4 | `Some(Other)` | Arm 2 skips Casualty (not tap-shaped); not `NoCost`. |
| Additional tap on an activated ability (activation_cost still contains TapCreatures) | 2 | recorded origin | Arm 1 does **not** fire because the activation TapCreatures leg is still present. |

Representational note (must not collapse): `None` = no additional-cost payment
in flight (activation, alternative, mana-ability, resolution `CostResume`).
`Some(Other)` = generic object additional (Conspire, face Required/Optional
tap, Choice-card additional that is tap-shaped). Both refuse Hill; they are
different facts.

`CostResume::Resolution` never calls this helper — it passes `origin: None` at
the `pay_tap_creatures_selection` call site (C1.5(c)).

---

## Step 4 — Architectural sections

### Pattern Coverage

Assessed against the **charter's class attribution**, not Phase 1's own diff
(infrastructure covers zero Oracle cards by construction).

- **Producer class:** every tap-emitting site in the engine — the ten non-test
  `tap_permanent_for_cost` callers, plus effect tap, attacker-declaration tap,
  replacement-resume tap, mana-source taps, and the auto-tap land fallback that
  does not go through `tap_permanent_for_cost`. Every card that taps a permanent
  for any reason is in this population. None of their firings may change except
  C1.2.
- **Consumer class (charter, not this diff):** `becomes tapped to pay …` = **1
  card** (Hill); unqualified `becomes tapped` = **116 cards** that must stay
  cause-blind until Phase 2 (`tap_cause == None` there). Phase 1 must not add a
  qualifier. The architecture (`TapCause` on the event, later an
  `Option<TapCause>` equality gate) is what carries “build for the class.”

Stop-if-1 does **not** fire: the producer class is the whole tap-emitting
engine, not Hill.

### Sizing (this phase)

- **Units:** **one**. Typed-cause retype + lockstep stamp + opponent-tap gate
  translation + origin parameter, one skill-checklist pass (`/add-engine-variant`
  + the event-emission / matcher slices of `/add-trigger`). Not independently
  testable behaviors.
- **Discriminating test this phase can own:** C1.2's single flipped gate
  outcome (resume tap becomes `Effect { source }` and the opponent-tap trigger
  fires). Hill is **`DEFERRED(phase 2)`**.
- **Inter-unit edges:** infrastructure → Phase 2 consumer (`TriggerDefinition.tap_cause`
  + equality gate + parser). No intra-phase unit edge.
- **T1 (≥2 units):** **FALSE**.
- **T2 (≥13 scope paths):** **TRUE**. Unconditional 21 paths in the charter
  enumeration, plus `types/proposed_event.rs` admitted by C1.2 = **22**.
  `casting_costs.rs` auto-tap is already path 6. Tests excluded. Advisory rest
  readers excluded unless the compiler forces them.
- **Conjunction T1 ∧ T2:** **FALSE**. No further split.

### Building Blocks

Compose; do not duplicate.

| Existing | Use |
|---|---|
| `restrictions::tap_permanent_for_cost` | Single cost-tap authority. Gains a `TapCause` argument. |
| `casting_costs::pay_tap_creatures_selection` | Gains `origin: Option<AdditionalCostOrigin>`; stamps `TapCreatures { origin }` once. |
| `casting_costs::handle_tap_creatures_for_spell_cost` | Derives origin via `in_flight_tap_creatures_origin`; only production caller allowed to inspect a pending. |
| `SpellContext::record_additional_cost_instance_payment` / `additional_cost_payments` | Existing push-ordered Vec; in-flight = newest tap-shaped origin. |
| `AdditionalCostOrigin::Teamwork` + `effective_teamwork_additional_cost_instances` | Already queues the identity Hill will require in Phase 2. |
| `effective_conspire_additional_cost` | `Other`-shape inspector, not a new origin variant. |
| `surface_next_unpaid_interactive_activation_cost` TapCreatures strip | Activation discriminant (probed). |
| `valid_card_matches` | Opponent-tap gate still uses it; do not fork. |
| `ThisWayCause` | Pattern analogue only — do not reuse the type. |
| `ProposedEvent::Destroy { source }` / `Discard { source_id }` | Shape analogue for the Tap widen. |
| `ConvokeMode`, `CrewAction` | Nested parameters of `ManaShard` / `CrewFamily`. |
| `AbilityCost::consumes_source` walk | Template for `contains_tap_creatures`. |

**New helpers (justified):**

1. `TapCause` / `TapCostKind` — frozen; Stage 1 DOES_NOT_EXIST.
2. `AbilityCost::contains_tap_creatures` — cost-tree predicate, not a boolean
   flag on PendingCast.
3. `in_flight_tap_creatures_origin` — single derivation authority so every
   opener of `pay_additional_cost_with_source` is classified by the four-arm
   algorithm rather than by per-opener special cases. Regenerating the opener
   list (`rg -n 'pay_additional_cost_with_source\(|pay_additional_cost\(' crates/engine/src | rg -v tests`)
   is how C1.5(e) stays honest; new openers must still fall into one of the
   four arms.

### Logic Placement

| Piece | Where | Why |
|---|---|---|
| `TapCause` / `TapCostKind` | `types/events.rs` next to `PermanentTapped` | The identity lives on the event (engine owns game rules). Not a `TriggerMode`. |
| Proposed tap source | `types/proposed_event.rs` | Replacement pause must carry what resume cannot recover. |
| Cost stamp | `restrictions::tap_permanent_for_cost` | Single cost-tap authority (charter). |
| Origin derivation | `casting_costs.rs` | The only function that has the pending at the spell/activation tap-creatures site. |
| Effect stamp | `tap_untap.rs` + replacement resume | CR 701.26 effect tap. |
| Attack stamp | `combat.rs::commit_attack_declaration` | CR 508.1f — not a cost. |
| Opponent-tap gate | `trigger_matchers::match_taps` | Existing matcher; translate, don't fork. |
| Dirty-marking | `public_state.rs` | Display layer consumes engine-provided ids; only `Effect { source }` has one. |
| Protocol bump | authored constants | `caused_by` → `cause` is a rename+retype of a `GameEvent` payload field. |
| Parser | **untouched** | Phase 2. |
| `TriggerDefinition` | **untouched** | Phase 2. |

Frontend remains a display layer: TS `PermanentTapped` arm does not read
`caused_by` today and is not required to grow `cause`.

### Rust Idioms

- `TapCause` / `TapCostKind` are enums, not bools or string tags.
- `Option<AdditionalCostOrigin>` on `TapCreatures` — not `is_teamwork: bool`.
  `None` vs `Some(Other)` vs `Some(Teamwork)` are three facts.
- Exhaustive `match` on `TapCause` in `match_taps` (opponent-tap gate) and in
  `public_state` dirty-marking. No `_ =>` that would swallow a future variant.
- `AbilityCost::contains_tap_creatures` exhaustive like `consumes_source` so a
  new cost variant forces a look.
- `ProposedEvent::Tap.source_id: ObjectId` required, not `Option` — every
  production constructor has the source (`process_one_tap`).
- No `#[serde(default)]` / `alias = "caused_by"` on the retyped field — the
  bump is the break (C1.4).
- `TapCause` / `TapCostKind` should be `Copy` (`AdditionalCostOrigin`,
  `ConvokeMode`, `CrewAction`, `ObjectId` already are).

### Nom Compliance

**N/A.** This phase must not modify any file under `crates/engine/src/parser/`.

### Extension vs Creation

**Creation of `TapCause` / `TapCostKind`**, extending the existing
`PermanentTapped` event rather than adding a `TriggerMode`. That is the frozen
refusal of `TapsToPayTeamwork`. Nested parameterization (`CrewFamily(CrewAction)`,
`ManaShard(ConvokeMode)`, `TapCreatures { origin }`) is extension of those
already-landed enums, not sibling proliferation.

`ProposedEvent::Tap` is **extended** with `source_id`, matching Destroy/Discard.

### Analogous Trace

See Step 2. Named feature: **effect-tap `caused_by: Some(source_id)` vs
cost-tap `caused_by: None`**, plus **`ThisWayCause` produce-time identity**, plus
**`ProposedEvent::Destroy { source }`**.

### Variant Discoverability (`/add-engine-variant`)

Inventory consulted: `data/engine-inventory.json` (present in this worktree).
`GameEvent::PermanentTapped` fields are still `object_id`, `caused_by`. No
`TapCause` / `TapCostKind` entries.

**Stage 1 — 5-grep protocol**

```
rg -n 'TapCause|TapCostKind' crates/engine/src/types/     # no matches
rg -n 'TapCause|TapCostKind' crates/engine/src/parser/     # no matches
rg -n 'TapCause|TapCostKind' crates/engine/src/database/synthesis.rs  # no matches
rg -n 'negated: bool|invert|polar' crates/engine/src/types/ability.rs  # unrelated
```

Near-misses that are **different layers** (EXISTS_DIFFERENT_NAME does **not** apply):

- `ThisWayCause` — tracked-set producer *action* (CR 608.2c), not why a
  permanent became tapped (CR 701.26).
- `CostPaymentProhibition` — static “can't pay life / sacrifice”, not a tap cause.
- `AttackDeclarationRecord` — combat history snapshot, not the tap event.
- `PermanentTapped.caused_by: Option<ObjectId>` — the field being **replaced**,
  not a reusable identity type.

**Stage 1 verdict: `DOES_NOT_EXIST`** for both enums. Proceed.

**Stage 2 — Parameterization filter**

`AttackDeclaration` vs `CostPayment` vs `Effect` do **not** share an X /
OpponentX / TargetX name root, a comparator, or a scope axis. They are
orthogonal CR categories (508.1f vs 601.2h/702.x vs 701.26). Nested leaves are
already parameterized (`CrewAction`, `ConvokeMode`, `Option<AdditionalCostOrigin>`).
Adding `Harmonize` as a `CrewFamily` member would cross 702.180 (alternative
cost from graveyard) with 702.122/171/184 (crew-family keyword actions) —
refused. Enlist stays `CostPayment(Enlist)` per frozen design (702.154a tap as
the enlist cost).

**Stage 2 verdict: `EXTEND_OK`.**

**Stage 3 — Categorical boundary**

The `TapCause` axis is “why this permanent became tapped” (CR 701.26 + CR 603.2e
“becomes tapped”). `AttackDeclaration` is a sibling of `CostPayment` *because*
CR 508.1f says attacking is not a cost — that is within the tap-event section,
not a cross-section unification of life/P/T/zones. Nested `TapCostKind` leaves
stay in their keyword sections (702.51a Convoke, 702.122b Crew, 702.154a Enlist,
702.171c Saddle, 702.184a Station, 702.180a Harmonize, 701.67a Waterbend,
702.126a Improvise). `ManaShard(ConvokeMode::Delve)` is admitted on the type
because `ConvokeMode` already contains Delve (CR 702.66a exile, not tap) and
must be stated **unreachable at any tap site** (existing `unreachable!` at the
engine convoke arm).

**Stage 3 verdict: `WITHIN_SECTION`.**

**APPROVED:**

```
TapCause::{ AttackDeclaration, CostPayment(TapCostKind), Effect { source: ObjectId } }
TapCostKind::{ TapSymbol, TapCreatures { origin: Option<AdditionalCostOrigin> },
              ManaShard(ConvokeMode), CrewFamily(CrewAction), Enlist, Harmonize }
```

Runtime: live in the same change. Serialized: `GameEvent` + `ProposedEvent`
inside `GameState.pending_replacement` → C1.4 bump. ability-scan / ability-rw:
N/A.

### Verification Matrix

| Claim | Seam | Production entry | Test | Revert-failing assertion | Hostile / sibling | Coverage |
|---|---|---|---|---|---|---|
| C1.0 forced sites are the listed producers + test literals | `PermanentTapped {` constructions | n/a | compiler | a non-test construction outside SCOPE_PATHS fails the build or C1.0 | rest-pattern readers must still compile | n/a |
| C1.1 behavioral neutrality | all `TriggerMode::Taps` | existing suite | **unmodified** `test-engine`; `cost_zone_pipeline.rs` Taps observers | any test *relaxed* to stay green falsifies | 116 unqualified becomes-tapped cards stay cause-blind | Hill stays wrongly-supported (pre-existing, not a Phase 1 regression) |
| C1.2 resume is the one new firing | `engine_replacement` Tap resume + `match_taps` | `process_one_tap` → NeedsChoice → accept | (1) extend `tap_replacement_accepted_applies_tap` to assert `cause == Effect { source }` — **reach-guard that the resume arm ran**; (2) unit: opponent-filtered `Taps` trigger **fires** on that event (today refuses `None`) | (1) `caused_by: None` still emitted; (2) `match_taps` false | Execute path (no replacement) already stamped Effect — must not be the only path the test hits; decline-replacement must still prevent the tap (existing Prevented behavior) | n/a |
| C1.3 opponent-tap gate translates | `match_taps` | `process_triggers` | extend `tap_opponent_creature_*` table | Effect{source: your} fires; Effect{source: theirs} refuses; AttackDeclaration refuses; every CostPayment variant refuses; no-opponent-filter still ignores cause | `CostPayment(TapCreatures { Some(Teamwork) })` must **still fire** an unqualified Taps trigger (no equality gate yet) — this is C1.1, not Hill | n/a |
| C1.4 wire bump | protocol constants | handshake | `node scripts/check-protocol-version.mjs` exits 0; vitest name+value pins | leftover `_70_` or `v53` | lobby floors must be untouched | n/a |
| C1.5 origin derivation | `in_flight_tap_creatures_origin` | `handle_tap_creatures_for_spell_cost` | unit/table at the helper **and** a Teamwork+Casualty integration assertion of origin at the tap site (can observe via the stamped event once producers land) | last payment Teamwork; activation → None; alternative → None; required unrecorded → Some(Other) | Casualty-then-Teamwork (d); activation after a recorded additional on the same pending (arm 1); `None` vs `Some(Other)` not collapsed | n/a |
| C1.6 total over callers | `tap_permanent_for_cost` | ten non-test call sites | compile-forced signature + a table in comments or a unit that names the mapping; Springleaf Drum already proves TapCreatures not TapSymbol | a caller passing the wrong variant | `ManaShard(Delve)` unreachable — do not add a tap site that constructs it | n/a |
| Hill Teamwork-only firing | `match_taps` equality + parser | Hill | **`DEFERRED(phase 2)`** | — | — | **`DEFERRED(phase 2)`** |
| Parser “to pay a teamwork cost” | `oracle_trigger.rs` | parse_trigger_line | **`DEFERRED(phase 2)`** | — | unmodelled `to pay …` tail honesty **`DEFERRED(phase 2)`** | **`DEFERRED(phase 2)`** |

Every planned negative has a paired positive reach-guard: C1.2's replacement
choice; C1.5's actual tap; C1.3's existing `tap_opponent_creature_via_effect_fires`.

### Identity / Provenance Contract

| Slot | Value |
|---|---|
| Source phrase / rules concept | “becomes tapped” (CR 603.2e) **because of** a specific game action: attack declaration (CR 508.1f), paying a cost (CR 601.2h / keyword 702.x), or an effect (CR 701.26). Hill's “to pay a teamwork cost” is Phase 2's consumer of this identity. |
| Authority type | `TapCause` on `GameEvent::PermanentTapped`. Nested `TapCostKind::TapCreatures { origin: Option<AdditionalCostOrigin> }` for additional/activation/mana tap-creatures costs. |
| Id / value | `AttackDeclaration`; `CostPayment(kind)`; `Effect { source: ObjectId }` of the effect source (the spell/ability that instructed the tap). Origin for Teamwork is `Some(AdditionalCostOrigin::Teamwork)`. |
| Binding time | **Tap time** — when `PermanentTapped` is pushed. Origin is derived then from the recorded payment / window discriminants, never later. |
| Live vs snapshot | Cause is **latched on the event**. `Effect { source }` controller lookup at trigger-match time is **live** `state.objects.get` (today's `caused_by` behavior). **No new LKI.** If the source has left play, controller is `None` and the opponent-tap gate refuses — same fail-closed as today. Phase 2 equality does not re-query payments. |
| Storage | The event payload. Not `PendingCast`, not `PendingReplacement`, not `GameState` side tables. `ProposedEvent::Tap.source_id` is only the replacement-pause carrier for the Effect case. |
| Consuming function this phase | `match_taps` opponent-tap gate (controller of `Effect.source`; refuse AttackDeclaration and every CostPayment as self-initiated siblings). Phase 2: equality on `trigger.tap_cause`. |
| Invalidation | Event is ephemeral (processed, logged). No turn-end expiry. A replacement that **prevents** the tap (CR 603.2g) never emits `PermanentTapped`. |
| Multi-authority hostile fixture | Twin Strike: Casualty payment then Teamwork tap — stamp must be `TapCreatures { Some(Teamwork) }`, not Casualty. Activation tap after a recorded additional on the same pending — stamp `TapCreatures { None }`, not the additional's origin. Required additional tap with empty payments — `Some(Other)`, not `None`. Alternative tap with `NoCost` — `None`, not `Some(Other)`. |

---

## Claims this phase must establish (measurement recap)

- **C1.0** — advisory list is advisory; forced non-test constructions are the seven producer sites above. **Measured.**
- **C1.1** — no existing Taps firing stops. Instrument: unmodified engine suite + `cost_zone_pipeline.rs` observers. **Unprobed as a green-tree run this round (Tilt down); the implementation must collect it.** Do not relax tests.
- **C1.2** — widen `ProposedEvent::Tap`; resume stamps `Effect { source }`; opponent-tap gate flips from refuse to fire. **Measured: source not recoverable without the widen.**
- **C1.3** — translate the three base verdicts onto `TapCause`; AttackDeclaration and each CostPayment variant are self-initiated siblings. **Source-read; implement as an exhaustive table.**
- **C1.4** — 70→71 and 53→54 move together; lobby floors do not. **Source-read (constants still 70/53 at plan time).**
- **C1.5** — derive origin; no PendingCast field. **Legs (a)(b)(c)(d) and the three (e) windows probed.**
- **C1.6** — ten callers assigned; named site `TapCreatures { None }`; `ManaShard(Delve)` unreachable. **Enumerated + Drum probe.**

---

## CR citations (grep-verified against `docs/MagicCompRules.txt`)

Run before writing each annotation (already run this round):

```
grep -n "^701.26" docs/MagicCompRules.txt   # Tap and Untap; 701.26a To tap a permanent…
grep -n "^508.1f" docs/MagicCompRules.txt   # Tapping a creature when declared as an attacker isn't a cost
grep -n "^601.2h" docs/MagicCompRules.txt   # The player pays the total cost
grep -n "^601.2b" docs/MagicCompRules.txt   # Announces alternative/additional costs
grep -n "^603.2"  docs/MagicCompRules.txt   # Triggered abilities; 603.2e “becomes tapped”
grep -n "^118.9"  docs/MagicCompRules.txt   # Alternative costs
grep -n "^118.3"  docs/MagicCompRules.txt   # Can't pay a cost without the resources; already-tapped can't tap to pay
grep -n "^702.51a" docs/MagicCompRules.txt  # Convoke
grep -n "^702.66a" docs/MagicCompRules.txt  # Delve (exile, not tap — unreachable at tap sites)
grep -n "^702.122b" docs/MagicCompRules.txt # A creature crews a Vehicle when tapped to pay the cost
grep -n "^702.126a" docs/MagicCompRules.txt # Improvise
grep -n "^702.154a" docs/MagicCompRules.txt # Enlist
grep -n "^702.171c" docs/MagicCompRules.txt # A creature saddles as it's tapped to pay the cost
grep -n "^702.184a" docs/MagicCompRules.txt # Station
grep -n "^702.180" docs/MagicCompRules.txt  # Harmonize 702.180a
grep -n "^701.67" docs/MagicCompRules.txt   # Waterbend (keyword action; ConvokeMode::Waterbend taps)
grep -n "^702.194" docs/MagicCompRules.txt  # Teamwork 702.194a additional cost
grep -n "^603.2g" docs/MagicCompRules.txt   # Replaced/prevented events don't trigger
```

**Placement:** `TapCause` variants and each stamp site. Do not invent 701.x/702.x
from memory. `ConvokeMode::Waterbend` is a keyword **action** (701.67), not a
702 keyword ability — annotate `ManaShard(Waterbend)` with CR 701.67a.

---

## Step 5 — Implementation steps

### 1. Types — `crates/engine/src/types/events.rs`

Define (next to `PermanentTapped`):

```rust
/// CR 701.26 + CR 603.2e: why a permanent became tapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum TapCause {
    /// CR 508.1f: tapping as part of declaring attackers is not a cost.
    AttackDeclaration,
    /// CR 601.2h + keyword 702.x / 118.3: tapped to pay a cost.
    CostPayment(TapCostKind),
    /// CR 701.26: an effect instructed the tap. `source` is the effect source.
    Effect { source: ObjectId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum TapCostKind {
    /// `{T}` / intrinsic land tap / auto-tap mana fallback.
    TapSymbol,
    /// CR 601.2b TapCreatures additional/activation/mana cost.
    /// `origin: Some(Teamwork)` is Hill's identity (consumed in Phase 2).
    TapCreatures { origin: Option<AdditionalCostOrigin> },
    /// CR 702.51a Convoke / CR 701.67a Waterbend / CR 702.126a Improvise.
    /// `ConvokeMode::Delve` (CR 702.66a) is unreachable at any tap site.
    ManaShard(ConvokeMode),
    /// CR 702.122b Crew / CR 702.171c Saddle / CR 702.184a Station.
    CrewFamily(CrewAction),
    /// CR 702.154a Enlist.
    Enlist,
    /// CR 702.180a Harmonize.
    Harmonize,
}
```

Retype:

```rust
PermanentTapped {
    object_id: ObjectId,
    cause: TapCause,  // was caused_by: Option<ObjectId> with serde default/skip
}
```

**No** `serde(default)`, **no** `alias = "caused_by"`. Import `AdditionalCostOrigin`,
`ConvokeMode`, `CrewAction` as needed.

### 2. `crates/engine/src/types/proposed_event.rs` (C1.2 — admitted)

```rust
Tap {
    object_id: ObjectId,
    source_id: ObjectId, // NEW, required
    applied: HashSet<AppliedReplacementKey>,
}
```

Update rest matches that already bind `object_id` / `applied` only as needed
(they compile with `..`). Forced constructions: `process_one_tap`, the two test
literals. Do not add a field on `PendingReplacement`.

### 3. `crates/engine/src/types/ability.rs`

Add `AbilityCost::contains_tap_creatures` (exhaustive, recurse Composite / OneOf
/ PerCounter). Do not add `TriggerDefinition.tap_cause`.

### 4. Effect + resume producers

`tap_untap.rs::process_one_tap`: put `source_id` on `ProposedEvent::Tap`; Execute
arm stamps `cause: TapCause::Effect { source: source_id }`. Keep the CR 701.26a
+ CR 508.1f comment on can't-tap restrictions.

`engine_replacement.rs` resume:

```rust
ProposedEvent::Tap { object_id, source_id, .. } => {
    // …apply tap…
    events.push(GameEvent::PermanentTapped {
        object_id,
        cause: TapCause::Effect { source: source_id },
    });
}
```

Keep/adjust the CR 701.26a comment.

### 5. `tap_permanent_for_cost` + C1.6 assignments

Signature:

```rust
pub(crate) fn tap_permanent_for_cost(
    state: &mut GameState,
    id: ObjectId,
    events: &mut Vec<GameEvent>,
    cause: TapCause, // always CostPayment(_) at these sites
) -> Result<(), EngineError>
```

Stamp `cause` instead of `caused_by: None`. Assign **before** the signature
change (callers must compile):

| Caller | Variant | CR |
|---|---|---|
| `engine_combat::apply_attack_enlist` | `CostPayment(Enlist)` | 702.154a |
| `engine_casting::handle_harmonize_tap_choice` | `CostPayment(Harmonize)` | 702.180a |
| `costs::pay_ability_cost_inner` `AbilityCost::Tap` | `CostPayment(TapSymbol)` | 118.3 + 701.26a |
| `casting_costs::pay_tap_creatures_selection` | `CostPayment(TapCreatures { origin })` | 601.2h + 701.26a |
| `mana_abilities::tap_source` | `CostPayment(TapSymbol)` | 118.3 |
| `mana_abilities::tap_selected_creature_for_mana_cost` | `CostPayment(TapCreatures { origin: None })` | 601.2b — **C1.6 named site** |
| `engine.rs` convoke arm | `CostPayment(ManaShard(mode))`; `Delve => unreachable!` stays | 702.51a / 701.67a / 702.126a / 702.66a |
| `engine.rs` crew | `CostPayment(CrewFamily(Crew))` | 702.122b |
| `engine.rs` station | `CostPayment(CrewFamily(Station))` | 702.184a |
| `engine.rs` saddle | `CostPayment(CrewFamily(Saddle))` | 702.171c |

No caller except `pay_tap_creatures_selection` (via the origin parameter from
`handle_tap_creatures_for_spell_cost`) may inspect a pending cast.

### 6. Origin threading — `casting_costs.rs`

- Implement `in_flight_tap_creatures_origin` as specified.
- `pay_tap_creatures_selection(..., origin: Option<AdditionalCostOrigin>)` passes
  `TapCause::CostPayment(TapCostKind::TapCreatures { origin })` into
  `tap_permanent_for_cost`.
- `handle_tap_creatures_for_spell_cost` computes `origin` and passes it.
- `engine.rs` `CostResume::Resolution` passes `None`.
- `costs.rs` tests: pass `None`.
- Auto-tap basic-land fallback (~the `PermanentTapped` literal that does not
  call `tap_permanent_for_cost`): stamp `CostPayment(TapSymbol)` in place.

### 7. Remaining producers

- `combat.rs::commit_attack_declaration`: `AttackDeclaration` (CR 508.1f).
- `mana_sources.rs` land tap: `CostPayment(TapSymbol)`.
- `ai_support/mod.rs` mana-probe: `CostPayment(TapSymbol)`.

### 8. Readers

`match_taps` — translate, **do not** add equality (Phase 2):

```rust
GameEvent::PermanentTapped { object_id, cause } => {
    // valid_card_matches unchanged
    if requires_opponent {
        match cause {
            TapCause::Effect { source } => {
                // live lookup, no new LKI
                let cause_controller = state.objects.get(source).map(|o| o.controller);
                if Some(trigger_controller) != cause_controller { return false; }
            }
            TapCause::AttackDeclaration | TapCause::CostPayment(_) => return false,
        }
    }
    …
}
```

`public_state.rs`: dirty `object_id` always; dirty `source` only for
`TapCause::Effect { source }`. AttackDeclaration / CostPayment have no cause
object.

### 9. Protocol bump (C1.4) — all pins together, bare integers

| File | Change |
|---|---|
| `crates/lobby-broker/src/protocol.rs` | `PROTOCOL_VERSION = 71`; changelog `71 —` for `PermanentTapped.caused_by` → `cause: TapCause` and `ProposedEvent::Tap.source_id`; `assert_eq!(PROTOCOL_VERSION, 71)` |
| `client/src/adapter/ws-adapter.ts` | `PROTOCOL_VERSION = 71` + matching changelog |
| `client/src/network/protocol.ts` | `WIRE_PROTOCOL_VERSION = 54` + changelog lockstep with 71 |
| `scripts/check-protocol-version.mjs` | `EXPECTED_PROTOCOL_VERSION = 71`, `EXPECTED_WIRE_PROTOCOL_VERSION = 54` |
| `crates/server-core/src/protocol.rs` | **rename** `protocol_version_is_70_for_booster_pack_origin` → `protocol_version_is_71_for_tap_cause` (or similarly; the numeral **must** be 71). Body `assert_eq!(PROTOCOL_VERSION, 71)`. Refuse leftover `protocol_version_is_70`. Update the comment in `full_game_floor_is_current_only_*` that names the old fn. |
| `client/src/network/__tests__/protocol.test.ts` | title `v54`; `expect(WIRE_PROTOCOL_VERSION).toBe(54)` |
| `client/src/adapter/__tests__/p2p-adapter-multiplayer.test.ts` | `it` title names refused `v53` **before** admitted `v54`; `setupFrameAt(53)` before `setupFrameAt(54)`; comments that say 52/53 move to 53/54 |

**Do not** move `LOBBY_PROTOCOL_VERSION`, `MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK`,
`MIN_LOBBY_PROTOCOL_FOR_DEFAULT_SCORING`, `MIN_LOBBY_PROTOCOL_FOR_MATCH_TYPE`.
Leave authored constants as bare integers (no `PREV + 1`).

`client/src/adapter/types.ts` PermanentTapped arm: no required edit.

### 10. Tests this phase

- Update every `PermanentTapped { caused_by: … }` literal (compiler-forced).
  Self-initiated fixtures → a `CostPayment` sibling (e.g. `TapSymbol`) or
  `AttackDeclaration` as the test's comment describes; effect fixtures →
  `Effect { source }`.
- Update `ProposedEvent::Tap` literals with `source_id`.
- Extend C1.3 table (`tap_opponent_creature_via_effect_fires`,
  `tap_opponent_creature_self_initiated_does_not_fire`,
  `tap_own_creature_does_not_fire_opponent_trigger`,
  `tap_no_opponent_filter_ignores_caused_by`) plus AttackDeclaration and each
  CostPayment variant as hostile siblings of Effect.
- C1.2: `tap_replacement_accepted_applies_tap` asserts Effect stamp; add the
  flipped `match_taps` assertion with ReplacementChoice as reach-guard.
- C1.5: helper unit table (Teamwork+Casualty, activation None, alternative None,
  required Other) — prefer calling the helper if `pub(crate)` + `#[cfg(test)]`,
  or observe stamped events on the same `GameScenario` boards the probes used.
- C1.6: signature change is the compile gate; comment the Delve unreachable
  invariant at the convoke arm (already present).
- **Do not** add Agent Maria Hill `GameRunner::cast` tests.
  **`DEFERRED(phase 2)`**.

`restrictions.rs` unit tests that call `tap_permanent_for_cost` must pass a
cause (compiler-forced).

### 11. Format and verify

- `cargo fmt --all` directly (Tilt does not auto-format).
- If Tilt is up: `./scripts/tilt-wait.sh clippy test-engine card-data check-frontend test-frontend`.
  `test-frontend` is required (C1.4 vitest pins).
- If Tilt is down: isolated `CARGO_TARGET_DIR` for `cargo clippy -p phase-engine --all-targets -- -D warnings` and `cargo test -p phase-engine --test integration` is acceptable; do not fight a live Tilt lock.
- `node scripts/check-protocol-version.mjs` exits 0.
- Do **not** run `cargo clean`. Do **not** touch parser. Do **not** bump protocol a second time. Do **not** add `PendingCast.paying_additional_cost_origin`.

---

## What this phase must not do

- Parser work; `TriggerDefinition.tap_cause`; equality gate; coverage classifier
  / `trigger_details`; Hill discriminating test — all **`DEFERRED(phase 2)`**.
- `TriggerMode::TapsToPayTeamwork`.
- A `PendingCast` origin field (`types/game_state.rs` stays out).
- `#[serde(default)]` on `PermanentTapped.cause`.
- Moving lobby protocol floors.
- Rerouting auto-tap / land-tap / AI-probe literals through
  `tap_permanent_for_cost` unless a later compiler demand requires it — stamp
  `TapSymbol` in place.
- New LKI for `Effect.source`.
- Claiming #6876 fixed. The issue stays live until Phase 2.

---

## Held green across the seam

After Phase 1, no trigger definition carries a tap-cause qualifier (the field
does not exist yet). `match_taps` still ignores cost kind except for the
opponent-tap controller gate. Unqualified `Taps` triggers fire on every
`TapCause` except that gate. Hill remains wrongly-supported. Tree green except
the one intended C1.2 flip.
