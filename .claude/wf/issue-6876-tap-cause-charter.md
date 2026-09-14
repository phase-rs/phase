# Revised Phase Charter — Issue #6876 (Agent Maria Hill, cost-qualified tap trigger)

`BASE_SHA 0f1a35b4de154ca969ca2e8f9ed6db85225c7dc3` (the run's pre-charter base; HEAD has since
advanced to the charter's own commit).
Charter mode. **Revision round — decision revision.** Design frozen; not re-planned.

Absorbed this round: the **C1.5 blocker** (the previously-proposed
`PendingCast.paying_additional_cost_origin` field is withdrawn — three existing authorities were
re-measured and the origin is derived from one of them), **Corrections 1–5**, and the residual
assumption behind C2.4 (the honesty mechanism is parser-side, not classifier-side). Prior rounds'
absorptions (**B1**, **B2**, **R1**, **R2**, earlier Corrections 1–3, RA-1, RA-2) are retained.

---

## 0. Step 0 — premise verification (survives into charter mode; re-run this round)

Scryfall `cards/named?exact=Agent Maria Hill`, fetched at this round:

> `Whenever Agent Maria Hill becomes tapped to pay a teamwork cost, put a +1/+1 counter on her and draw a card.`
> Legendary Creature — Human Spy Hero · `{W}` · MSH

Matches the issue body verbatim. `AdditionalCostOrigin::Teamwork` already exists
(`types/ability.rs`), and `effective_teamwork_additional_cost_instances`
(`game/casting_costs.rs`) already queues the Teamwork cost stamped with that origin, so the
Teamwork additional cost is landed infrastructure, not part of this task. **Premise PASSES.**

CR numbers used below, each grep-verified against `docs/MagicCompRules.txt`:
`701.26` (Tap and Untap), `701.26a`, `508.1f` ("Tapping a creature when it's declared as an
attacker isn't a cost"), `601.2h`, `603.2`, `603.2a`, `702.51a` (Convoke), `702.122b` ("A
creature *crews a Vehicle* when it's tapped to pay the cost"), `702.154a` (Enlist),
`702.171c` ("A creature *saddles* a permanent as it's tapped to pay the cost").

CR 508.1f is the categorical warrant for the frozen shape: attacker-declaration tapping is
*not* a cost, so `AttackDeclaration` is a **sibling** of `CostPayment`, never a `TapCostKind`.
CR 702.122b / 702.171c define crew and saddle as *tapped to pay a cost*, which is what puts
`CrewFamily(CrewAction)` under `CostPayment`.

---

## 1. Frozen design (unchanged — restated so each phase plan inherits it verbatim)

- **REFUSE** `TriggerMode::TapsToPayTeamwork`. A per-mechanic trigger mode is a card-shaped
  variant; the cause belongs on the event, not on the mode.
- **CREATE** `TapCause { AttackDeclaration, CostPayment(TapCostKind), Effect { source } }`.
- **CREATE** `TapCostKind { TapSymbol, TapCreatures { origin: Option<AdditionalCostOrigin> },
  ManaShard(ConvokeMode), CrewFamily(CrewAction), Enlist, Harmonize }`.
- **Phase 1** — retype `PermanentTapped.caused_by` → `cause: TapCause`; stamp every producer;
  **derive** the in-flight additional-cost origin at the tap-creatures payment site from the
  payment the decision already recorded, and **thread it as a parameter** into
  `pay_tap_creatures_selection`; translate the opponent-tap gate in `match_taps`.
- **Phase 2** — `TriggerDefinition.tap_cause`; `match_taps` equality gate; parser; Hill tests.
- **Identity contract** — Hill fires **iff** `cause == TapCause::CostPayment(TapCostKind::TapCreatures
  { origin: Some(AdditionalCostOrigin::Teamwork) })`. The authority is the event's stamped cause,
  bound at tap time by the payment site, never re-derived at trigger time.
- Two phases, infrastructure then consumer.

The only element of this list that moved this round is the Phase 1 origin mechanism: the frozen
`TapCostKind::TapCreatures { origin }` shape and the identity contract are untouched — what changed
is *where the origin comes from* (an existing recorded payment, not a new `PendingCast` field).

### 1.1 Class attribution (Correction 3 — the population each phase serves)

Phase-plan mode assesses its Pattern Coverage stop against this attribution, not against a phase's
own diff.

- **Producer side (Phase 1)** — the class is *every tap-emitting site in the engine*: the ten
  non-test callers of `restrictions::tap_permanent_for_cost`, plus the effect tap, the
  attacker-declaration tap, the replacement-resume tap, and the mana-source taps. Every card that
  taps a permanent for any reason is in this population; the retype is infrastructure for all of
  them, and none of their firings may change (C1.1).
- **Consumer side (Phase 2)** — Oracle-text population, measured against Scryfall this round
  (regenerate: `curl -sG --data-urlencode 'q=o:"becomes tapped to pay"' https://api.scryfall.com/cards/search`
  and the same for `o:"becomes tapped"`, reading `total_cards`):
  - `becomes tapped to pay …` — **1 card**, Agent Maria Hill. This is the whole cost-qualified
    class in print today. Phase 2 does not get to claim a larger consumer population, and the
    architecture (a general `TapCause` on the event, an `Option<TapCause>` equality gate) is what
    carries the "build for the class" burden instead of the card count.
  - unqualified `becomes tapped` — **116 cards** (117 total minus Hill). This is the population
    C1.1 and C2.3 must leave firing exactly as it fires today: 116 cards keep `tap_cause == None`
    and must remain cause-blind.

---

## 2. Residual assumptions — decided

### RA-1. Does Phase 2 owe its own `PROTOCOL_VERSION` bump? → **No. Phase 2 carries no wire scope.**

Decided against a second bump, and the decision is claimed (**C2.5**), not asserted. Three
measured facts stand behind it:

1. `client/src/adapter/types.ts` types `GameObject.trigger_definitions` as `unknown[]`. The
   client provably never decodes a trigger body, so an additive field on `TriggerDefinition`
   changes nothing a client reads. (This is also the honest reading of B2 — see §Phase 2 scope.)
2. P2P is **hub-and-spoke: the host runs the authoritative engine and guests never construct
   the engine adapter** (`client/src/adapter/p2p-adapter.ts`). There is exactly one engine per
   game, so a Phase-1-build peer and a Phase-2-build peer cannot disagree about whether Hill's
   trigger fires. This is the fact that closes the only skew a no-bump decision would otherwise
   leave open, and it is why the answer differs from Phase 1's.
3. The v70 precedent named in `crates/server-core/src/protocol.rs`
   (`protocol_version_is_70_for_booster_pack_origin`) was a **required-field replacement inside
   a `WaitingFor` variant the client actively decodes**. `tap_cause` is optional, additive, and
   inside an opaque array. Bumping for it would inflate the version on a surface no peer can
   observe, and the convention the script encodes reserves the bump for shapes a peer must decode.

Phase 1 is the opposite case and does bump: `caused_by → cause` is a **rename plus retype** of a
payload field on `GameEvent`, a tagged union the Rust decoder deserializes into typed values on
both the full-game and P2P wires, where `#[serde(default)]` would silently degrade rather than
refuse.

### RA-2. Does Phase 2 need `ai_support/mod.rs`? → **No, and it is not in Phase 2 scope.** Claim **C2.6**.

`crates/engine/src/ai_support/mod.rs` has **no `TriggerMode::Taps` arm** — its beneficial-trigger
predictor enumerates `TapsForMana | ManaAbilityProduced` and falls to `_ => false`.
It cannot over-predict a cost-qualified trigger because it does not predict `Taps` at all.

The production readers of `TriggerMode::Taps` outside `trigger_matchers.rs` are **six** (Correction
2 — the earlier "two sites" undercounted; regenerate with
`rg -n 'TriggerMode::Taps\b' crates/` (do not exclude tests blindly — `ai_support/shortcut_efficacy.rs` names `Taps` in a `#[cfg(test)]` table; the production classifier `trigger_event_unreachable_by_confined_action` dispatches on `mode` alone, so C2.6's verdict is unchanged):

| # | Reader | What it reads |
|---|---|---|
| 1 | `phase-ai/src/policies/self_untap_loop.rs::has_tap_payoff_trigger` | `active.definition.mode` against a `Taps \| TapsForMana \| TapAll \| Untaps \| UntapAll \| AbilityActivated` set |
| 2 | `engine/src/analysis/ability_graph.rs::trigger_axis` | `mode` → `AxisKey::Tap` |
| 3 | `engine/src/game/mana_sources.rs::object_self_tap_harm_amount` | City-of-Brass sibling-trigger penalty: filters `mode == Taps`, then `valid_card`, then folds the `execute` chain |
| 4 | `engine/src/game/trigger_index.rs` | `TriggerMode::Taps \| TapAll => push(TriggerEventKey::Taps)` — the index key |
| 5 | `engine/src/database/forge/trigger.rs` | `"Taps" => Ok(TriggerMode::Taps)` string mapper |
| 6 | `engine/src/types/triggers.rs` | `"Taps" => TriggerMode::Taps` string mapper |

**None reads a tap cause.** Readers 1, 2 and 4 dispatch on `mode` alone; reader 3 additionally
reads `valid_card` and the `execute` chain, neither of which an additive field touches (this is
the one place the correction's "mode/valid_card" shorthand is looser than the code, and it does not
change the verdict — the reader is cause-blind either way); readers 5 and 6 construct a definition
in which `tap_cause` takes its `Default`/`#[serde(default)]` value `None`, which C2.3 measures as
permissive. None is in Phase 2 scope. C2.6 measures this rather than assuming it.

Note that `ai_support/mod.rs` **is** in **Phase 1** scope, for an unrelated and compiler-forced
reason: its mana probe constructs a `PermanentTapped` literal. `trigger_index.rs` appears twice in
this charter for two unrelated reasons — Phase 1 *advisory* (it reads `PermanentTapped` through a
`{ .. }` pattern, so the retype does not force it) and Phase 2 *out of scope* (reader 4 above).

---

# Phase 1 — Stamp the cause on every tap

## Goal

Give every `PermanentTapped` event a typed, producer-stamped `TapCause`, replacing the
`Option<ObjectId> caused_by` field, and translate the existing opponent-tap gate in `match_taps`
onto the new type — **so that no trigger that fires today stops firing; the single intended new
firing is the C1.2 replacement-resume case.** No trigger gains a cost qualifier in this phase; no
card changes behavior except as C1.2 describes.

## Scope rule

Literal paths, no globs. The three standing inclusion classes (`/engine-implementer`:
compiler-forced sites, shared registration files, comment-only edits) are implied and not
enumerated. The orchestrator materializes `SCOPE_PATHS` at scope-freeze.

**Types**
- `crates/engine/src/types/events.rs` — the retype and the `TapCause` / `TapCostKind` definitions
- `crates/engine/src/types/proposed_event.rs` — **conditional, governed by C1.2.** `ProposedEvent::Tap`
  carries `{ object_id, applied }` and no source today; in scope only if C1.2 resolves to stamping
  `Effect { source }` at the replacement-resume site.

`crates/engine/src/types/game_state.rs` was in this group in the prior charter solely to hold
`PendingCast.paying_additional_cost_origin`. That field is withdrawn (see C1.5), and the file has
no other Phase 1 reason — measured: it constructs no `GameEvent::PermanentTapped` and holds no tap
`caused_by` (its `caused_by` occurrences are `RulesExecutionNodeRef` and
`QuantityRef::FilteredTrackedSetSize` doc text, unrelated to the tap event). **Dropped from scope**
and from the T2 count.

**Producers (stamp the cause)**
- `crates/engine/src/game/restrictions.rs` — `tap_permanent_for_cost`, the single cost-tap authority
- `crates/engine/src/game/effects/tap_untap.rs` — `Effect { source }`
- `crates/engine/src/game/combat.rs` — `AttackDeclaration` (CR 508.1f)
- `crates/engine/src/game/engine_replacement.rs` — the C1.2 resume site
- `crates/engine/src/game/casting_costs.rs` — producer; `pay_tap_creatures_selection`'s new
  `origin` parameter; the origin **derivation** at `handle_tap_creatures_for_spell_cost` (C1.5)
- `crates/engine/src/game/mana_sources.rs`
- `crates/engine/src/ai_support/mod.rs` — mana-probe literal

**Cost-payment callers (assign a `TapCostKind` — see C1.6)**
- `crates/engine/src/game/engine_combat.rs` — `apply_attack_enlist` → `Enlist`
- `crates/engine/src/game/engine_casting.rs` — `handle_harmonize_tap_choice` → `Harmonize`
- `crates/engine/src/game/costs.rs` — `pay_ability_cost_inner` → `TapSymbol`
- `crates/engine/src/game/mana_abilities.rs` — `tap_source` → `TapSymbol`; `tap_selected_creature_for_mana_cost` → **C1.6's named site**
- `crates/engine/src/game/engine.rs` — convoke → `ManaShard(ConvokeMode)`; crew / station / saddle →
  `CrewFamily(CrewAction)`; **and** the `CostResume::Resolution` arm's direct
  `pay_tap_creatures_selection` call, which passes `origin: None` (C1.5 leg (c))

**Readers**
- `crates/engine/src/game/public_state.rs` — dirty-marking destructures the field by name
- `crates/engine/src/game/trigger_matchers.rs` — `match_taps`, the opponent-tap gate translation (C1.3)

**Wire** (B1's five paths, plus two authoritative homes B1's list omits — see C1.4)
- `client/src/adapter/ws-adapter.ts`
- `scripts/check-protocol-version.mjs`
- `crates/server-core/src/protocol.rs`
- `client/src/network/__tests__/protocol.test.ts` *(excluded from the T2 count — test)*
- `client/src/adapter/__tests__/p2p-adapter-multiplayer.test.ts` *(excluded from the T2 count — test)*
- `crates/lobby-broker/src/protocol.rs` — **planner addendum.** `pub const PROTOCOL_VERSION: u32`
  is authored here; `server-core` derives it. The bump is not performable without this path.
- `client/src/network/protocol.ts` — **planner addendum.** `export const WIRE_PROTOCOL_VERSION`
  is authored here; `check-protocol-version.mjs` reads it and refuses an unbumped P2P surface.
- `client/src/adapter/types.ts` — the `PermanentTapped` union arm (durable form: the member of the
  `GameEvent` discriminated union whose `type` is `"PermanentTapped"`). This listing is
  **permissive, not required**: the TS arm currently carries `{ object_id }` only (no `caused_by`),
  so the retype forces no edit there. Phase 1's protocol bump rests on the Rust-decoded
  `events: Vec<GameEvent>` wire payload, not on this arm. Dropping the path from a later
  materialization still leaves T2 ≥ 13.

**Advisory — touch only if the compiler or a mirror gate demands (C1.0)**
- Every non-test module that reads `GameEvent::PermanentTapped` through a rest pattern (`{ .. }` or `{ object_id, .. }`). Measured examples include `crates/engine/src/game/log.rs` and `crates/engine/src/game/trigger_index.rs`; regenerate rather than treating that pair as complete.

## Claims Phase 1 must establish

Each is a claim with a measurement, never a statement of fact.

**C1.0 — the advisory list is advisory, and nothing forced hides outside the scope rule.**
Advisory (touch only if the compiler or a mirror gate demands): every non-test module that reads
`GameEvent::PermanentTapped` through a rest pattern (`{ .. }` or `{ object_id, .. }`) — regenerate
rather than treating a hand-list as complete. Claim C1.0 — every remaining site the retype forces
is a `GameEvent::PermanentTapped` struct literal in a `#[cfg(test)]` module, admitted at
materialization as a compiler-forced path. Measurement: enumerate every
`GameEvent::PermanentTapped {` construction under `crates/engine/src` and `crates/engine/tests`
and confirm each forced site outside the scope rule is test-module-only; a forced construction in
a non-test module is a scope-rule finding, not a materialization detail.

**C1.1 — behavioral neutrality.** No trigger that fires at base stops firing after the retype.
Measurement: the existing engine suite is the instrument — `test-engine` green with no test
edited to accommodate a changed firing, and the tap-observer triggers in
`crates/engine/tests/integration/cost_zone_pipeline.rs` (the `TriggerMode::Taps` observers) pass
unmodified. A test that must be *relaxed* to stay green falsifies C1.1.

**C1.2 — the replacement-resume case is the single intended new firing.** The
`ProposedEvent::Tap` resume arm in `game/engine_replacement.rs` stamps `caused_by: None` at base,
which the opponent-tap gate reads as self-initiated and refuses. Measurement: determine from the
resume site whether a causing source is recoverable there; if it is, stamp `Effect { source }`
and show the one gate outcome that flips, with a paired positive reach-guard proving the fixture
reaches the resume arm and not an earlier tap path; if it is not recoverable without widening
`ProposedEvent::Tap`, the phase either widens it (admitting
`crates/engine/src/types/proposed_event.rs` per the conditional scope entry) or stamps the
conservative cause and records that no firing changes. Whichever way it resolves, the phase must
name the outcome and prove it; "the resume site is unreachable" is not admissible without a
reach-guard.

**C1.3 — the opponent-tap gate translates without changing its verdict.** CR 701.26 + CR 508.1f:
`match_taps` today gates a `controller: Some(Opponent)` `valid_card` on
`caused_by`'s controller, refusing `None` outright. Measurement: for each of the three base
verdicts — cause controlled by the trigger's controller (fire), cause controlled by another
(refuse), self-initiated (refuse) — exhibit the `TapCause` value the translated gate receives and
show the verdict is identical, with `AttackDeclaration` and each `CostPayment` variant covered as
the hostile siblings of the `Effect { source }` case.

**C1.4 — the wire bump is complete and every pin moves together.** Measurement:
`node scripts/check-protocol-version.mjs` exits 0 after the bump, exercising specifically —

- **(i) the two `EXPECTED_*` pins this change moves**, both of which live in
  `scripts/check-protocol-version.mjs` itself. `EXPECTED_PROTOCOL_VERSION` must equal the value
  newly authored in `crates/lobby-broker/src/protocol.rs` and mirrored in
  `client/src/adapter/ws-adapter.ts`; `EXPECTED_WIRE_PROTOCOL_VERSION` must equal the value newly
  authored in `client/src/network/protocol.ts`, which has no ws-adapter mirror.
- **(ii) the `protocol_version_is_<n>` require/refuse pair** in
  `crates/server-core/src/protocol.rs` — the script requires a `fn protocol_version_is_<new>` and
  refuses any surviving mention of `protocol_version_is_<new-1>`, so the test function must be
  renamed rather than added alongside. Its body's `assert_eq!(PROTOCOL_VERSION, <new>)` moves with
  the rename.
- **(iii) the P2P name legs in the two test files, plus the two value pins.** Name legs:
  `client/src/network/__tests__/protocol.test.ts` — the script slices its double-quoted
  `describe`/`it` titles, requires `v<new>` and refuses `v<new-1>`;
  `client/src/adapter/__tests__/p2p-adapter-multiplayer.test.ts` — the
  `describe("P2P wire-protocol version gate"` block must exist, must contain an `it(...)` title
  naming refused `v<new-1>` **before** admitted `v<new>`, and must contain
  `setupFrameAt(<new-1>)` before `setupFrameAt(<new>)`. Value pins the script does not itself
  check but the suites do: `assert_eq!(PROTOCOL_VERSION, <new>)` in
  `crates/lobby-broker/src/protocol.rs`, and `expect(WIRE_PROTOCOL_VERSION).toBe(<new>)` in the
  P2P protocol test. Both are vitest/Rust assertions, which is why `test-frontend` is on Phase 1's
  Tilt list and not merely `check-frontend`.

The `AUTHORED_LITERALS` classifier additionally refuses a bumped constant that has been re-derived
into an expression; leave each authored constant a bare integer. Frozen floors
(`MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK`, `MIN_LOBBY_PROTOCOL_FOR_DEFAULT_SCORING`,
`MIN_LOBBY_PROTOCOL_FOR_MATCH_TYPE`) and the lobby and directory versions must **not** move — a
diff that touches them falsifies C1.4.

**C1.5 — the in-flight additional-cost origin is *derived* from the payment already recorded, and
threaded; no new state is added.** This claim replaces the prior charter's
`PendingCast.paying_additional_cost_origin`, which is **withdrawn**: the origin is already recorded
on the ability context before the payment window opens, so a second home for it would be a
duplicate authority.

The shape the phase must implement: at `casting_costs::handle_tap_creatures_for_spell_cost`, derive
`origin: Option<AdditionalCostOrigin>` from `pending.ability.context.additional_cost_payments`, and
pass it as a new parameter to `casting_costs::pay_tap_creatures_selection` (which today receives no
`PendingCast` at all), which stamps `TapCostKind::TapCreatures { origin }` once when it calls
`restrictions::tap_permanent_for_cost`. No caller of `tap_permanent_for_cost` outside that function
may inspect a pending cast to choose its variant.

Measurement, five legs (measured at `BASE_SHA` by the planner; the phase re-measures and each leg
must be exhibited, since all five are what the withdrawn field would otherwise have papered over):

- **(a) The queue head is *not* the authority at the tap site.** Every path that pays a *non-repeatable* queued additional cost pops the instance before paying it: `handle_decide_additional_cost` calls `additional_cost_queue.remove(0)` before `pay_additional_cost_with_source`, and `finish_pending_cost_or_cast`'s `AdditionalCost::Required` arm does the same. `handle_decide_repeatable_additional_cost` is the exception — it pops only on the decline and exhausted paths; on the pay path it records the instance payment and calls `pay_additional_cost` with the instance still at the head, so a repeatable cost can be paid again. Teamwork is queued as `Optional { Once }` and therefore takes the popping path. Because `pay_additional_cost_with_source` is what suspends into `WaitingFor::PayCost { kind: PayCostKind::TapCreatures { .. } }`, the tap site resumes with the in-flight once-instance already gone. Exhibit, for a Teamwork cast, that `pending.additional_cost_queue` is empty at `handle_tap_creatures_for_spell_cost`; and for a spell carrying a second queued instance, that the head is the next, undecided instance rather than the one being paid. A probe that finds a *once*-repeatability instance still at the head falsifies this leg and reopens the reviewer's queue-head alternative; a repeatable instance surviving at the head does not. Regenerate the pop/record site map with `rg -n 'additional_cost_queue\.remove|record_additional_cost_(instance_)?payment|pay_additional_cost' crates/engine/src/game/casting_costs.rs`.
- **(b) The just-recorded payment carries the origin.** Before the pop,
  `handle_decide_additional_cost` calls
  `ability.context.record_additional_cost_instance_payment(instance.origin, instance.origin_ordinal, 1)`
  in both its `Optional { repeatability: Once }` and its `Required` arm, and the resulting
  `updated_pending.ability` is what is boxed into `WaitingFor` and handed back to
  `handle_tap_creatures_for_spell_cost`. Teamwork is queued by
  `effective_teamwork_additional_cost_instances` as exactly that `Optional { Once }` shape stamped
  `AdditionalCostOrigin::Teamwork`, so it takes the recording arm. Exhibit, at the tap site, an
  `AdditionalCostInstancePayment { origin: Teamwork, .. }` — with a **paired positive reach-guard**
  showing the probe reached `handle_tap_creatures_for_spell_cost` (the tap actually occurred / the
  cast completed), not merely that the vector is non-empty.
- **(c) Non-spell callers pass `origin: None`.** Production callers of
  `pay_tap_creatures_selection` are exactly two: `handle_tap_creatures_for_spell_cost`, and the
  `CostResume::Resolution` arm of `game/engine.rs`, which resolves a tap-creatures selection with
  no `PendingCast` in hand and therefore passes `None`. The mana-ability leg does **not** route
  through this function at all — `mana_abilities::handle_tap_creatures_for_mana_ability` is the
  sibling validator, and `mana_abilities::tap_selected_creature_for_mana_cost` is C1.6's named
  site — so there is nothing to thread there and no `origin` parameter is owed. Remaining callers
  live inside `game/costs.rs`'s `#[cfg(test)]` module. Exhibit the enumeration.
- **(d) Multi-payment disambiguation.** `additional_cost_payments` is a push-ordered `Vec`
  (`record_additional_cost_instance_payment` pushes and never merges), so the in-flight payment is
  the **most recently recorded** one — the payment whose decision opened this payment window.
  Exhibit a cast with two additional-cost instances where the tap-creatures cost is decided second:
  the derivation must select the tap cost's origin while the earlier payment remains present and
  unselected.
- **(e) A payment window that recorded no payment of its own must yield `None`, not a stale origin.** The enumeration axis is the **opener**, not the builder: enumerate every production caller of `casting_costs::pay_additional_cost_with_source` (regenerate: `rg -n 'pay_additional_cost_with_source\(|pay_additional_cost\(' crates/engine/src | rg -v tests`), and for each record whether that caller recorded an additional-cost instance payment before suspending. Two openers record before suspending — `handle_decide_additional_cost`'s `Optional { Once }` and `Required` arms, and `handle_decide_repeatable_additional_cost`'s pay path, which records the queued instance and leaves it at the head — and the rest open the window without recording, including the alternative-cost `AdditionalCost::Choice` accept arm of `handle_decide_additional_cost` itself (Ramosian Rally / The Lady of Otaria class), which records only `alternative_mana_cost_paid`. Neither the opener set nor the recording set is a snapshot: the phase must regenerate both with the command above and classify every production opener it returns — the set spans `handle_decide_additional_cost`, `handle_decide_kicker_cost`, `handle_decide_repeatable_additional_cost`, `finish_pending_cost_or_cast`, `check_additional_cost_or_pay_with_distribute`, `continue_after_declared_mana_split`, and `enter_payment_step`, at more than one call site each — and a recording opener is no safer than a non-recording one: the narrowing below is what makes the derivation correct for every window, recording or not. For each opener the phase must either narrow the derivation so that window yields `None`, or prove with a reach-guard that its context carries no earlier payment. The two windows that are not additional costs at all — `surface_next_unpaid_interactive_activation_cost` and the no-target activation detour in `game/casting.rs` — stay in the enumeration under the same obligation. Narrowing must scope the lookup to the origins whose queued cost shape is a tap-creatures cost, so an unrelated earlier payment can never be selected — never by capturing a watermark at window-open time, which would reintroduce the suspended `PendingCast` state this claim withdrew. Recording-site count is durable, not a snapshot: regenerate with `rg -n 'record_additional_cost_(instance_)?payment' crates/engine/src`. Hits in `game/triggers.rs` write onto a freshly built casualty/replicate *copy* ability, never a `PendingCast`'s, so they are builders under this opener-not-builder axis and are not additional PendingCast origin sources.

Two representational notes the phase plan must carry, because `Option<AdditionalCostOrigin>`
already expresses both and the derivation must not collapse them: a cost with no additional-cost
payment in flight derives `None`, while a generic object additional cost derives `Some(Other)`.
`Other` is recorded at more than one site — regenerate with the command already in leg (e) rather
than treating a named fallback as the sole writer. Both refuse Hill under the frozen identity
contract, but they are different facts and must stay distinguishable.

**C1.6 — `TapCostKind` is total over `tap_permanent_for_cost` callers.**
Measurement: enumerate every caller and assign each a variant before the signature change
(regenerate: `rg -n 'tap_permanent_for_cost\(' crates/engine/src`; ten non-test callers at
`BASE_SHA`). `mana_abilities::tap_selected_creature_for_mana_cost` (Springleaf Drum class) is the
one site with no eponymous variant and must be assigned explicitly. A site with no correct variant
falsifies the frozen `TapCostKind` shape and escalates rather than being absorbed into an adjacent
variant.

*Planner note for the phase plan, not a substitute for the measurement:* the candidate assignment
for the named site is `TapCreatures { origin: None }` — it is a tap-creatures cost belonging to an
activated ability rather than a spell's additional cost, which is precisely the case
`Option<AdditionalCostOrigin>` exists to express, and it is consistent with C1.5 leg (c) (the mana
leg has no additional-cost origin to thread). C1.6 is satisfied only if the phase plan confirms
this by enumeration and states it; if `origin: None` turns out to be indistinguishable from a case
that must be distinguishable, that is the escalation C1.6 names. `TapCostKind::ManaShard(ConvokeMode)`
admits `ConvokeMode::Delve`, which exiles a graveyard card rather than tapping a permanent; that
value is unreachable at any tap site and must be stated as such rather than rediscovered in review.

## Verification plan

Phase 1's discriminating test for the *task* is `DEFERRED(phase 2)` — no card observes a tap cause
until the consumer lands, which is the defining property of this seam. Phase 1's own verification
is structural plus unit-level:

- Green tree; existing suites unchanged and unrelaxed (C1.1).
- Tilt resources: `clippy`, `test-engine`, `card-data`, `check-frontend`, and **`test-frontend`**
  (added per B1 — the two protocol test files in the Wire group are vitest suites, so
  `check-frontend` alone cannot observe the bump, and C1.4 leg (iii)'s value pin lives in one of
  them).
- `node scripts/check-protocol-version.mjs` exits 0 (C1.4).
- Unit assertions at the seam: the `match_taps` translation table from C1.3, the caller→variant
  enumeration from C1.6, and the C1.5 derivation legs — at minimum a Teamwork cast asserting the
  origin observed at the tap site and a non-additional-cost tap asserting `None`.
- `cargo fmt --all` run directly (Tilt does not auto-format).

## Deferral list

Everything the full task needs that Phase 1 intentionally omits, attributed to its landing phase:

- `TriggerDefinition.tap_cause` and the equality gate — **phase 2**
- Parsing "to pay a teamwork cost" — **phase 2**
- Parser-side coverage honesty for unrecognized tap cost words — **phase 2**
- `coverage.rs` `trigger_details` rendering of `tap_cause` (C2.4(iii)) — **phase 2**
- The Agent Maria Hill discriminating cast-pipeline test — **phase 2**
- Any second `PROTOCOL_VERSION` bump — **not owed**, decided at RA-1 and claimed at C2.5
- A stored `PendingCast` origin field — **not owed**, withdrawn at C1.5 in favor of deriving from
  `ability.context.additional_cost_payments`

## Recursive T1 ∧ T2

- **T1 (≥2 units): FALSE.** Phase 1 is one unit — a single typed-cause retype implementable by one
  skill-checklist pass. Its breadth is lockstep registration, not independently testable behaviors;
  its only discriminating assertion is C1.2's single flipped gate outcome, a consequence of the
  retype rather than a second behavior.
- **T2 (≥13 scope paths): TRUE.** See the enumeration below (21 non-test, recounted this round
  after `types/game_state.rs` was dropped).
- **Conjunction: FALSE.** Phase 1 does not itself trip the gate; no further split. T2 alone never
  triggers a split.

---

# Phase 2 — Consume the cause on the trigger

## Goal

Let a triggered ability require a specific tap cause, and parse Agent Maria Hill's Teamwork
qualifier into it, so the trigger fires only on a Teamwork additional-cost tap — while any tap
qualifier the parser does not recognize leaves the card honestly unsupported rather than silently
producing an unqualified `Taps` trigger.

## Scope rule

**Engine**
- `crates/engine/src/types/ability.rs` — `TriggerDefinition.tap_cause: Option<TapCause>`, additive,
  `#[serde(default, skip_serializing_if = "Option::is_none")]`
- `crates/engine/src/game/trigger_matchers.rs` — the equality gate in `match_taps`
- `crates/engine/src/parser/oracle_trigger.rs` — the `SimpleEvent::BecomesTapped` arm: both the
  Teamwork binding and the refusal of an unmodelled `to pay …` tail (C2.4)
- `crates/engine/src/game/coverage.rs` — the classifier (B2). **Retained in scope even though C2.4
  now decides the mechanism is parser-side** — the phase owns the decision, and the claim below is
  what records that the classifier needs no edit (or, if the measurement says otherwise, what
  admits the edit).

**Frontend mirror** (B2)
- `client/src/adapter/types.ts` — in scope so the phase owns the decision; whether it is *edited*
  is decided by **C2.5**, whose measurement records that `GameObject.trigger_definitions` is typed
  `unknown[]` and therefore absorbs an additive field without a TS edit. A scope rule admits the
  path; the claim decides the diff.

**Tests** *(excluded from the T2 count)*
- `crates/engine/tests/integration/` — the Hill discriminating test (new file)
- `crates/engine/tests/integration/main.rs` — its `mod` line (shared registration file, standing class)
- `crates/engine/src/parser/oracle_trigger_tests.rs` — parser-level assertions

**Not in scope, by decision:** `crates/engine/src/ai_support/mod.rs` and the six
`TriggerMode::Taps` readers enumerated at RA-2 (C2.6); all wire paths (RA-1 / C2.5).

## Claims Phase 2 must establish

**C2.1 — the defect is live at base and this phase is what fixes it.** Measurement: at
`PHASE_BASE_SHA`, a cast-pipeline test in which Hill becomes tapped for a *non*-Teamwork reason
awards a `+1/+1` counter and a card; after the change it awards neither, while the Teamwork
payment path still awards both. Both legs in one test file, so the negative leg has its paired
positive reach-guard and cannot pass vacuously through an unparsed ability.

**C2.2 — the parser binds the qualifier, and binds it to the frozen identity.** Measurement:
`"becomes tapped to pay a teamwork cost"` yields `TriggerMode::Taps` with
`tap_cause == Some(CostPayment(TapCreatures { origin: Some(Teamwork) }))`, built with nom
combinators (`tag`/`alt`/`value` composed on the existing `SimpleEvent::BecomesTapped` arm — no
`contains`/`find`/`split_once` dispatch), and the sibling `"becomes tapped"` with no qualifier
still yields `tap_cause == None`. The Hill line must round-trip through the same
`parse_leading_turn_constraint` tail handling that arm already performs, so the Captain America
"during your turn" class does not regress.

**C2.3 — the gate is equality, and `None` is permissive.** Measurement: a trigger with
`tap_cause: None` fires on every `TapCause` (the base behavior the other 116 `becomes tapped`
cards depend on — §1.1); a trigger with `tap_cause: Some(x)` fires on `x` and on nothing else.
Hostile siblings: `CostPayment(TapCreatures { origin: Some(Kicker) })`,
`CostPayment(TapCreatures { origin: None })`, `CostPayment(TapCreatures { origin: Some(Other) })`,
`CostPayment(TapSymbol)`, `CrewFamily(_)`, and `AttackDeclaration` must each refuse against a
Teamwork-qualified trigger. `AttackDeclaration` is the load-bearing one: CR 508.1f makes attacker
tapping not a cost, so a card tapped by attacking must not satisfy a cost-qualified trigger. The
permissive-`None` leg must also cover a definition built by the two string mappers (RA-2 readers 5
and 6), which produce `tap_cause` at its `Default` value.

**C2.4 — an unrecognized tap qualifier stays honestly unsupported, and the mechanism is
parser-side.** Measured: `coverage.rs`'s trigger classifier (`build_trigger_item` and the
`missing`-collection path) keys on `TriggerMode::Unknown(_)`, trigger-registry membership, and
`trigger_has_unimplemented_parts` — nothing else. `TriggerMode::Taps` is registered, so **a
well-formed `Taps` trigger cannot be turned red by the classifier**; that is why this issue is a
supported-aspect defect and why no classifier edit can fix it. The honesty mechanism is therefore
the parser: the `SimpleEvent::BecomesTapped` arm must **refuse an unmodelled `to pay …` tail
specifically**, falling through to `TriggerMode::Unknown` (which the classifier does call
unsupported), in the same shape as the `SimpleEvent::DealtDamage` arm's tail guard
(`oracle_trigger.rs`, the arm that *produces* `TriggerMode::DamageReceived`).

It must **not** be a blanket remainder reject: measured at `BASE_SHA`, the `BecomesTapped` arm's
`remaining` legitimately carries the "during your turn" turn phrase and the trailing intervening-if
`", if …"` clause (CR 603.4) — Captain America, Living Legend is the live example the arm's own
comment names — so an all-consuming refusal would regress that class.

Measurement: (i) a `"becomes tapped to pay a <qualifier the parser does not model>"` line reaches
`TriggerMode::Unknown` and a red coverage verdict, exhibiting the classifier path that produces it;
(ii) the Captain America "during your turn" + intervening-if line still parses to a supported
`Taps` trigger with its constraint intact — the paired positive reach-guard that proves the refusal
is tail-specific and not blanket; (iii) the Phase-2 `coverage.rs` diff is decided by two separate
readings, not one: `build_trigger_item`'s red/green verdict is recorded as unchanged, while
`trigger_details` — which already renders `trig.constraint` and `trig.condition`, the two existing
firing qualifiers — either gains a `tap_cause` row or the phase states why a cost-qualified trigger
should read as unqualified in the parse-details surface that triage tooling consumes. If the
measurement contradicts (i), the classifier edit is taken under this claim as well. A fixture that
is red for an unrelated reason does not buy this claim.

**C2.5 — no second wire bump is owed, and the TS mirror needs no edit.** Measurement:
(i) `node scripts/check-protocol-version.mjs` exits 0 against a Phase-2 diff that touches no protocol
constant — its `EXPECTED_*` pins and its `protocol_version_is_<n>` require/refuse pair all read the
Phase-1 values and stay green, which is the script's own statement that this surface did not move.
(ii) Paired with the ws-adapter capability-bump convention: the client decodes
`GameObject.trigger_definitions` as `unknown[]`, so no client capability changes and no bump is
warranted; show that the TS diff for this phase is empty.
(iii) Exhibit that the P2P guest path constructs no engine adapter — the host-side adapter is the
only engine constructor (`client/src/adapter/p2p-adapter.ts`) — so a Phase-1-build peer and a
Phase-2-build peer cannot disagree about whether Hill's trigger fires.
If any leg fails, the wire paths from Phase 1's Wire group are admitted to Phase 2 and the bump is taken.

**C2.6 — the AI and every other `Taps` reader is cause-blind.** Measurement: enumerate every
`TriggerMode::Taps` reader outside `trigger_matchers.rs` — the six at RA-2 — and show each returns
the same verdict for a Hill trigger before and after the additive field: `ai_support/mod.rs` has no
`Taps` arm and falls to `_ => false`; `self_untap_loop.rs::has_tap_payoff_trigger` and
`ability_graph.rs::trigger_axis` match on `mode` alone; `mana_sources.rs::object_self_tap_harm_amount`
reads `mode`, `valid_card` and the `execute` chain, none of which the field touches;
`trigger_index.rs` keys the index on `mode`; and the `forge/trigger.rs` and `types/triggers.rs`
string mappers produce `tap_cause` at its `Default` value. If any reader's verdict changes, that
path is admitted to scope and the claim becomes a behavioral one requiring `cargo ai-gate` with a
paired-seed report.

## Verification plan

- The Phase-1-deferred discriminating test lands here: the C2.1 cast-pipeline test, written to the
  `/card-test` recipe (`GameScenario` + `GameRunner::cast(..).resolve()` + `CastOutcome` deltas,
  verbatim Oracle text, no hand-built `TargetRef` vectors), in
  `crates/engine/tests/integration/` with its `mod` line added to `tests/integration/main.rs` —
  never a new top-level file under `crates/engine/tests/`.
- Parser assertions for C2.2 and C2.4 in `oracle_trigger_tests.rs`, including the unqualified
  sibling, the unmodelled-tail refusal, and the Captain America turn-constraint reach-guard.
- The C2.3 hostile-sibling table as unit assertions at `match_taps`.
- The C2.4 coverage fixture, plus `cargo coverage` (a one-shot binary, not a Tilt resource) to
  confirm Agent Maria Hill's status moves for the right reason.
- Tilt: `clippy`, `test-engine`, `card-data`, `check-frontend`; `test-ai` because C2.6 asserts an
  unchanged AI verdict. `cargo fmt --all` direct.

## Deferral list

Nothing from the full task is deferred past Phase 2. Explicitly **not** owed, each decided rather
than deferred: a second `PROTOCOL_VERSION` bump (C2.5), `ai_support/mod.rs` and the five other
`Taps` readers (C2.6), a `coverage.rs` classifier edit (C2.4, unless measurement (i) contradicts
the red/green path; `trigger_details` rendering of `tap_cause` is decided by C2.4(iii) and is
in-scope either way), and any `TriggerMode` variant for Teamwork (refused in the frozen design).

## Recursive T1 ∧ T2

- **T1 (≥2 units): FALSE.** One unit — the cost-qualified tap trigger, one skill-checklist pass
  (`/add-trigger`) across its lockstep layers, one discriminating test.
- **T2 (≥13 scope paths): FALSE.** Five non-test paths after exclusions (`types/ability.rs`,
  `trigger_matchers.rs`, `oracle_trigger.rs`, `coverage.rs`, `client/src/adapter/types.ts`); test
  files and the integration `mod` line are excluded or grouped.
- **Conjunction: FALSE.** No further split.

---

# Seam notes

- **The seam is the event's cause field.** Phase 1 makes the cause representable and stamps it;
  Phase 2 makes a trigger able to require one. Phase 1 has no discriminating test *for the task*
  because no card observes a cause until Phase 2 lands — that is the infrastructure→consumer edge
  T3 names as the preferred split point, and why Phase 1's verification is structural. Its only
  discriminating assertion is C1.2's flipped gate outcome.
- **The origin derivation is a second, smaller seam inside Phase 1.** The origin is recorded on the
  ability context at decision time (`handle_decide_additional_cost`) and consumed at payment time
  (`handle_tap_creatures_for_spell_cost`), with `WaitingFor::PayCost` in between. Nothing new is
  stored; the phase plan must read the recording sites and the tap site together rather than
  treating either in isolation, because C1.5 legs (a), (d) and (e) all live in the gap between them.
- **Shared files.** `crates/engine/src/game/trigger_matchers.rs` is touched by both phases —
  Phase 1 translates the opponent-tap gate onto `TapCause`, Phase 2 adds the equality gate beside
  it. They are separate edits to `match_taps` and must not be merged; Phase 2 must re-read the file
  rather than working from Phase 1's plan text.
  `client/src/adapter/types.ts` sits in both Phase 1's Wire group (permissive) and Phase 2's
  frontend mirror.
  `crates/engine/tests/integration/main.rs` is the standing shared registration file for Phase 2's
  new test. `crates/engine/src/types/events.rs`, `crates/engine/src/types/ability.rs` and
  `crates/engine/src/game/casting_costs.rs` are frequent multi-agent collision points — edits must
  be surgical.
- **Held green across the seam.** After Phase 1, every trigger's `tap_cause` is absent and
  `match_taps` ignores causes exactly as it does today, so the tree is green and no card's
  behavior has changed (C1.1) except the C1.2 case. Coverage stays honest across the seam because
  Phase 1 changes no card's parse: Agent Maria Hill remains wrongly-supported until Phase 2, which
  is the pre-existing state, not a regression Phase 1 introduces.
- **Advisory files are not a silent third scope.** `log.rs` and `trigger_index.rs` enter
  `SCOPE_PATHS` only through the compiler-forced standing class with the compiler error as
  evidence, never by planner fiat — that is what C1.0 exists to keep honest.

---

# Phase 1 scope-path enumeration (recounted this round)

Counted under the phase-fit rule: test fixtures excluded outright; committed generated artifacts
and translation mirrors grouped with their authored source; directory entries expanded. **The
enumeration is the artifact; the integer is derived from it.** Regenerate with:

```
rg -n 'GameEvent::PermanentTapped\s*\{|tap_permanent_for_cost' crates/ client/ --glob '!target'
```

**Non-test, unconditional — 21:**

| # | Path | Group |
|---|---|---|
| 1 | `crates/engine/src/types/events.rs` | Types |
| 2 | `crates/engine/src/game/restrictions.rs` | Producer |
| 3 | `crates/engine/src/game/effects/tap_untap.rs` | Producer |
| 4 | `crates/engine/src/game/combat.rs` | Producer |
| 5 | `crates/engine/src/game/engine_replacement.rs` | Producer |
| 6 | `crates/engine/src/game/casting_costs.rs` | Producer + caller + C1.5 derivation and threading |
| 7 | `crates/engine/src/game/mana_sources.rs` | Producer |
| 8 | `crates/engine/src/ai_support/mod.rs` | Producer |
| 9 | `crates/engine/src/game/engine_combat.rs` | Caller |
| 10 | `crates/engine/src/game/engine_casting.rs` | Caller |
| 11 | `crates/engine/src/game/costs.rs` | Caller |
| 12 | `crates/engine/src/game/mana_abilities.rs` | Caller (C1.6 site) |
| 13 | `crates/engine/src/game/engine.rs` | Caller ×4 + `CostResume::Resolution` (`origin: None`) |
| 14 | `crates/engine/src/game/public_state.rs` | Reader |
| 15 | `crates/engine/src/game/trigger_matchers.rs` | Reader (C1.3) |
| 16 | `client/src/adapter/types.ts` | Wire |
| 17 | `client/src/adapter/ws-adapter.ts` | Wire |
| 18 | `scripts/check-protocol-version.mjs` | Wire |
| 19 | `crates/server-core/src/protocol.rs` | Wire |
| 20 | `crates/lobby-broker/src/protocol.rs` | Wire (planner addendum) |
| 21 | `client/src/network/protocol.ts` | Wire (planner addendum) |

**Non-test, conditional — 1** (`crates/engine/src/types/proposed_event.rs`, admitted only if C1.2
resolves to threading a source). Upper bound **22**.

**Excluded tests — 2:** `client/src/network/__tests__/protocol.test.ts`,
`client/src/adapter/__tests__/p2p-adapter-multiplayer.test.ts`.

**Reconciliation with the correction's expected 24.** An earlier correction anticipated *24
non-test + 2 excluded tests*; measurement now yields **21 unconditional (22 with the C1.2
conditional) + 2 excluded tests**. The three-path gap is accounted for and is not a missing scope
path:

1. R1 demoted `crates/engine/src/game/log.rs` and `crates/engine/src/game/trigger_index.rs` from
   scope to advisory — exactly two paths — because each reads `PermanentTapped` through `{ .. }`
   or `{ object_id, .. }` and is therefore not compiler-forced. They re-enter only through the
   compiler-forced standing class if the build demands them.
2. This round drops `crates/engine/src/types/game_state.rs` — one path — because its only Phase 1
   reason was the withdrawn `PendingCast.paying_additional_cost_origin` field (C1.5), and the file
   holds no tap-event construction or field of its own.

Two further candidates were measured and rejected rather than silently dropped:
`client/src/animation/types.ts` and `client/src/animation/eventNormalizer.ts` name
`"PermanentTapped"` only as a string key in an animation-duration map and a normalizer allowlist,
never reading the payload, so the retype does not force them.

**T2 verdict for Phase 1: TRUE** (21 ≥ 13). **T1: FALSE.** Conjunction **FALSE** — the recursive
gate result is unchanged by the honest re-count, since 21, 22, 23 and 24 all clear the same
threshold and T1 is what refuses.
