# MSH Coverage Wave 2 — Engine/Parser Implementation Plan

**Scope:** 5 confirmed cards + 1 stretch. Parser-fix-heavy cluster. Re-derived from
`client/public/coverage-data.json` (898 Marvel MSH/MSC cards; 89 unsupported at plan time).

**Deconfliction:** Wave 1 (Connive + Klaw) shipped (#3947); Loki deferred. Wave 2 touches a
*different* primary file set than Wave 1's `oracle_trigger.rs` connive work, but it DOES touch
`oracle_trigger.rs` (Molten Lavamancer, Captain America) — so it must run **sequentially on a
single checkout**, after Wave 1 has landed, and serialized against any other open `oracle_trigger.rs`
work. Per-card sub-changes within Wave 2 are mostly disjoint and ordered below.

**Inert-until-regen caveat (load-bearing):** M1 (Storm) is a **card-data synthesis** change
(`database/synthesis.rs`), and the parser fixes (Fixer, Molten Lavamancer) are inert until
card-data regen + redeploy (memory `parser_fix_inert_until_data_regen`). Every runtime test MUST
drive the real pipeline via `add_real_card` + rehydrate / `GameScenario` (memories
`parsed_ast_not_consumed`, `runtime_tests_must_drive_pipeline`), except the Storm guard, whose
discriminating test drives the **synthesis face-builder directly** (the authoritative data path).

---

## Confirmed Wave 2 Card Set (re-derived + live-parser-verified)

| # | Card | Cluster | Verdict | New engine variant? | Effort |
|---|------|---------|---------|---------------------|--------|
| 1 | **Storm, Queen of Wakanda** | M1 | Synthesis keyword name-guard (corroboration-based) | No | LOW–MED |
| 2 | **Fixer, Techno Terror** | M2a | Parser-only: optional " the battlefield" in ETB-this-turn condition | No | LOW |
| 3 | **Molten Lavamancer** | G | Parser-only: add "one or more of your opponents" recipient arm | No | LOW |
| 4 | **Flying Drone** | M2b | Runtime filter-eval: `WithKeyword`+`Another` on entry snapshots | No (struct field) | MED |
| 5 | **Captain America, Living Legend** | M2c | New `TriggerCondition` + per-object tap ledger + parser | **YES** (1 TriggerCondition) | MED |
| (6) | **Beast, Erudite Aerialist** *(stretch)* | M0 | Parser self-ref `~` arm + verify runtime source-filter | No | LOW–MED, verify-first |

**Important coverage note on Storm:** Storm is currently `supported: true` in `coverage-data.json`
— it will NOT appear in an `supported==false` derivation. But its `parse_details` carries a phantom
`label: "Storm"` (the MTG **Storm** keyword, CR 702.40) it should not have. This is a misparse that
"parses successfully" and so reads as supported (memory `backlog_is_parser_misparses`). It is a
genuine correctness defect and the highest-ROI item in the wave (it also hardens every future
keyword-named creature), so it is included despite not surfacing in the unsupported list.

**Excluded from Wave 2 (deferred / out of scope):** Loki (deferred, Wave 1 §E), Leader/Quantum
Reduction/Galactus/M.O.D.O.K. ("replacement"/"unknown"/conditional-attack infra), Teamwork cluster
(×17, separate wave), Harness (The Mind Stone, novel mechanic), all `replacement_structure` /
`static_structure` cards (Waves 3–4), and every gap=0 M0 runtime-diagnosis card except Beast.

---

## Applicable Skills (engine-planner Step 1)

| Skill | Cards | Why |
|-------|-------|-----|
| `/add-card-data-pipeline` | Storm (1) | Keyword synthesis output shape / `merge_extracted_keywords` reconciliation. |
| `/oracle-parser` | Fixer (2), Molten Lavamancer (3), Beast (6) | Parser-only nom-combinator arms. |
| `/add-static-ability` (filter-eval) | Flying Drone (4) | Conditional cost-reduction gate evaluation (`battlefield_entry_matches_filter`). |
| `/add-trigger` | Captain America (5) | Intervening-if `TriggerCondition` on a Taps trigger. |
| `/add-engine-variant` | Captain America (5) | Mandatory gate — one new `TriggerCondition` variant. |

---

## Analogous Traces (engine-planner Step 2 — hard gate)

- **Storm (M1)** traces the existing **craft retain guard** in the same synthesis face-builder:
  `database/synthesis.rs` `build_card_face` → MTGJSON-keyword vec build (~9670–9683) →
  `extracted_keywords` from `parse_oracle_with_cleave_brackets` (~9703) →
  `keywords.retain(|k| !craft …)` corroboration retain (~9712–9714) →
  `merge_extracted_keywords` (~9721). The new name-guard retain slots **immediately beside the craft
  retain**, reusing the exact "MTGJSON-said-X but Oracle didn't corroborate → drop" pattern.

- **Fixer (M2a)** traces the existing **ETB-this-turn condition** path:
  `parser/oracle.rs` `strip_activated_constraints` ("activate only if ") → `parse_restriction_condition`
  (`oracle_condition.rs:51`) → `parse_event_condition` (125) → `parse_etb_this_turn_condition` (939–963)
  → `ParsedCondition::YouHadArtifactEnterThisTurn` → runtime `game/restrictions.rs:1196` (reads
  `battlefield_entries_this_turn`). All layers exist and work for "...entered **the battlefield**
  under your control this turn"; the only gap is the elided-"the battlefield" templating.

- **Molten Lavamancer (G)** traces the existing **source-deals-damage trigger**:
  `parser/oracle_trigger.rs` `try_parse_source_deals_damage_trigger` (~8100) →
  `parse_damage_to_qualifier` (~6199) → `parse_opponent_player_recipient` (~6234) → `DamageDone` +
  `DamageKindFilter::NoncombatOnly` + `valid_target=Opponent`. Verified working for the singular
  recipient ("an opponent" / "one of your opponents", test at ~23587). Only the plural-batched
  recipient phrase is unwired.

- **Captain America (M2c)** traces **`TriggerCondition::DealtDamageBySourceThisTurn`** end-to-end as
  the per-object look-back-ledger template:
  `types/ability.rs` `TriggerCondition` enum → ledger field on `GameState`
  (`damage_dealt_this_turn`, ~6058) populated at damage time → `game/triggers.rs check_trigger_condition`
  (~5091) evaluates the record. The new tap-count ledger + condition mirror this exactly. The base
  Taps trigger (`TriggerMode::Taps`, triggers.rs:76; parser `SimpleEvent::BecomesTapped`,
  oracle_trigger.rs:7582) and the `OnlyDuringYourTurn` constraint (ability.rs:13877; parser 1414)
  already exist.

- **Flying Drone (M2b)** traces **`CostReduction { condition }`** end-to-end (already supported for
  conditional activation-cost reductions — Esquire of the King, Razorlash Transmogrant; tests in
  `tests/conditional_cost_reduction_3223.rs`): parser `oracle_cost.rs:987` ("less to activate if ") →
  `CostReduction { condition: Some(ParsedCondition::BattlefieldEntriesThisTurn{..}) }` on
  `AbilityDefinition.cost_reduction` → runtime `casting.rs apply_cost_reduction` (~12575) →
  `restrictions::evaluate_condition` → `battlefield_entry_matches_filter` (~393). The ONLY missing
  link is filter-prop coverage in that last function.

---

## Pattern Coverage (engine-planner Step 4 — build for the class)

| Cluster | New capability | Class covered (not just this card) | Est. cards in class |
|---|---|---|---|
| M1 Storm | Drop an MTGJSON-asserted keyword whose token coincides with a word in the card's own name **and** is not corroborated by Oracle-text keyword extraction | **Every** creature whose name embeds a keyword word and gets a phantom keyword from MTGJSON mistagging (Storm-, Flying-, Trample-, Haste-named creatures, future Marvel/UB names). Prevents a whole defect class. | MSH: ≥1; cross-set: open-ended safety net |
| M2a Fixer | Make " the battlefield" optional in ETB-this-turn restriction conditions | **Every** "[type] entered under your control this turn" cast/activation restriction using the modern elided templating (creature/artifact/angel-or-berserker arms all upgraded). | ~10+ across sets |
| G Molten Lavamancer | "one or more of your opponents" as a damage-trigger recipient | **Every** "deals damage to one or more of your opponents" batched-recipient damage trigger. | ~5+ |
| M2b Flying Drone | `FilterProp::WithKeyword` + `FilterProp::Another` evaluation against `BattlefieldEntryRecord` snapshots | **Every** "[type] with [keyword] entered this turn" / "another [type] entered this turn" cost-reduction or restriction condition. Closes a structural hole in the entry-snapshot filter evaluator. | ~8+ |
| M2c Captain America | `TriggerCondition` "first time the triggering object became tapped this turn" + per-object tap ledger | **Every** "if it's the first time that [creature] has become tapped this turn" intervening-if (CR 701.26 class). | ~3–5 |

No item builds for a single card; each adds a building block keyed on a CR-defined event/templating class.

---

## `/add-engine-variant` Gate — Captain America's `TriggerCondition` (mandatory)

Only ONE new engine-enum variant is proposed in the whole wave. Routed through the three-stage filter
(`data/engine-inventory.json` consulted; `cargo engine-inventory` is the canonical surface).

**Proposed:** `TriggerCondition::FirstTimeObjectTappedThisTurn` (unit; subject = the triggering object).

- **Stage 1 — Existence:** `rg "FirstTime|TappedThisTurn|object_tapped" data/engine-inventory.json` →
  the per-*ability* limiters exist (`TriggerConstraint::OncePerTurn`, `MaxTimesPerTurn`,
  `OncePerOpponentPerTurn`; `GameState.triggers_fired_this_turn` / `trigger_fire_counts_this_turn`),
  but these count **ability firings**, not **per-object tap events**. No per-object "this turn"
  tap-count ledger and no "first time this object did X" `TriggerCondition` exist. Verdict:
  **DOES_NOT_EXIST**.
- **Stage 2 — Parameterization / sibling-cluster smell:** `TriggerCondition` carries many leaf
  predicates; there is no existing "first time / Nth time this turn" family to parameterize, so this
  is not a sibling-cluster lift. Resist over-generalizing to `FirstTimeEventThisTurn { event_type }`
  — see Stage 3. Verdict: **EXTEND_OK** (single leaf, no cluster to refactor).
- **Stage 3 — Categorical boundary:** "became tapped" is CR 701.26 (Tap/Untap). "first time
  attacked" (CR 508), "first time dealt damage" (CR 120) are *separate rule sections*. A unified
  `FirstTimeEventThisTurn { event_type }` would conflate rule sections at the leaf-reference layer —
  prohibited by the categorical-boundary rule. The parameterization axis must lie within one CR
  section, so the variant stays tap-specific. Verdict: **WITHIN_SECTION**.

**Result: APPROVED** — `TriggerCondition::FirstTimeObjectTappedThisTurn` (unit), CR 701.26 + CR 603.4.
The companion ledger `GameState.object_tap_count_this_turn: HashMap<ObjectId, u32>` is a state field,
not an enum variant (no gate); it mirrors `damage_dealt_this_turn` / `exerted_this_turn` precedent.
Flying Drone's `BattlefieldEntryRecord` keyword snapshot is likewise an additive struct field (no
new enum variant). No `bool` flags anywhere (memory `no_bool_flags`).

---

## Per-Cluster Implementation Detail

### Card 1 — Storm, Queen of Wakanda (M1, synthesis name-guard)

**Root cause (verified):** MTGJSON's own `keywords` array lists "Storm" for this card; it is ingested
verbatim at `database/synthesis.rs` ~9673–9683 with no name/corroboration guard. Her Oracle text
never grants Storm as a standalone keyword line ("Flying" is a real standalone line; "Storm" appears
only inside ability text "Whenever Storm attacks", which self-ref-normalizes to "~"). So the parser's
own `extracted_keywords` contains `Flying` but NOT `Storm`.

**Rules-correct design (NOT the naive name-match the recon suggested):** A naive "drop any keyword
whose word is in the card name" would strip the **legitimate** Flying keyword from a card literally
named "Flying Men". The correct rule (CR 702.x keywords are abilities that must be printed in the
card's rules text; CR 201.5 a name reference means just that object): **drop an MTGJSON keyword only
when its token coincides with a whole word of the card's own name AND the Oracle-text keyword
extractor did not independently produce it.**

- Storm: token `storm` ∈ name-words {storm, queen, of, wakanda} AND `Storm` ∉ `extracted_keywords` → **drop**.
- Storm's `Flying`: token `flying` ∉ name-words → **keep** (untouched).
- "Flying Men" (negative): token `flying` ∈ name-words BUT `Flying` ∈ `extracted_keywords` (standalone line) → **keep**.

**Change — `database/synthesis.rs`, immediately after the craft retain (~line 9714), before
`merge_extracted_keywords` (~9721)** so `face_name` (9686) and `extracted_keywords` (9703) are both
in scope:

```rust
// CR 702.40 + CR 201.5: MTGJSON sometimes asserts a keyword whose token coincides with a
// word in the card's own name (e.g. it tags "Storm" for "Storm, Queen of Wakanda"). A
// keyword ability is only real if the card's rules text grants it. Drop a name-colliding
// MTGJSON keyword that the Oracle-text keyword extractor did NOT independently produce;
// corroborated keywords (e.g. Flying on "Flying Men") and non-colliding keywords are kept.
let name_words: std::collections::HashSet<String> = face_name
    .split(|c: char| !c.is_alphanumeric())
    .filter(|w| !w.is_empty())
    .map(str::to_lowercase)
    .collect();
keywords.retain(|kw| {
    let token = kw.to_string().to_lowercase(); // Display impl, keywords.rs:1753
    !name_words.contains(&token) || extracted_keywords.iter().any(|e| e == kw)
});
```

Executor must verify: `Keyword::Storm.to_string()` yields `"Storm"` (Display, keywords.rs:1753 — it
is the coverage label "Storm"), and that `extracted_keywords` is still in scope at the insertion
point (it is `let`-bound at 9703 and only read afterward). Use value-equality `e == kw` for
corroboration (matches the craft-retain and `merge_extracted_keywords` equality convention,
synthesis.rs:9589 "Equality, not discriminant"), so parameterized keywords compare by full value.

**Scenario-harness note (secondary, optional):** `game/scenario.rs` (~73–88) has a *separate*
test-only keyword inference path that also lacks the guard. It only runs when no keyword hint is
passed in tests. The shipped data path (and therefore coverage + real gameplay) is `synthesis.rs`;
the single synthesis fix is sufficient to make Storm correct. Applying the same guard to the
scenario inference is a consistency follow-up, not required — flag in the impl report, do not expand
scope unless the discriminating test forces it.

### Card 2 — Fixer, Techno Terror (M2a, optional " the battlefield")

**Root cause (verified — recon was wrong):** Fixer's text is "Activate only if an artifact entered
**under your control** this turn" — no "the battlefield". Every arm in
`parse_etb_this_turn_condition` (`oracle_condition.rs:939–963`) hardcodes "entered the battlefield
under your control this turn", so Fixer's elided form fails to parse and the restriction is dropped
(card unsupported, gap=1).

**Change — `parser/oracle_condition.rs`, `parse_etb_this_turn_condition` (942–962).** Make
" the battlefield" optional via `opt`, applied to all three arms for class coverage (nom-idiomatic;
compose, do not enumerate permutations). Restructure each `value(.., tag("full string"))` into a
sequence `(type_prefix, opt(tag(" the battlefield")), tag(" under your control this turn"))`:

```rust
fn parse_etb_this_turn_condition(text: &str) -> nom::IResult<&str, ParsedCondition, OracleError<'_>> {
    use nom::combinator::opt;
    let battlefield_suffix = |i| {
        // CR 400.7: modern templating elides "the battlefield" after "enter(ed)".
        (opt(tag(" the battlefield")), tag(" under your control this turn")).parse(i)
    };
    alt((
        value(
            ParsedCondition::YouHadCreatureEnterThisTurn,
            (alt((tag("a creature entered"), tag("creature enter"))), battlefield_suffix),
        ),
        value(
            ParsedCondition::YouHadAngelOrBerserkerEnterThisTurn,
            (tag("angel or berserker enter"), battlefield_suffix),
        ),
        value(
            ParsedCondition::YouHadArtifactEnterThisTurn,
            (alt((tag("an artifact entered"), tag("artifact entered"))), battlefield_suffix),
        ),
    ))
    .parse(text)
}
```

Executor verifies the `value(_, tuple)` form compiles (nom `value` accepts any parser, incl. a
tuple-sequence), and that the closure's lifetime threads cleanly (factor to a named
`fn entered_under_your_control_suffix` if the closure fights the borrow checker). Runtime + the
`YouHadArtifactEnterThisTurn` arm (`restrictions.rs:1196`) are already correct — no engine change.

### Card 3 — Molten Lavamancer (G, plural opponent recipient)

**Root cause (verified):** `parse_opponent_player_recipient` (`oracle_trigger.rs:6234–6244`) accepts
"an opponent" / "one of your opponents" / "another player" but NOT "one or more of your opponents",
so the recipient is unmatched and `try_parse_source_deals_damage_trigger` bails
(`valid_target.is_none()` guard ~4169). All other primitives exist (`DamageDone`,
`DamageKindFilter::NoncombatOnly`, `OncePerTurn`, `OnlyDuringYourTurn`).

**Change — `parser/oracle_trigger.rs`, `parse_opponent_player_recipient` (~6234).** Add one `alt`
arm (the phrase is already recognized elsewhere in the file at ~627, confirming the typed filter is
the same `opponent_player_filter()`):

```rust
preceded(tag("one or more of your "), tag("opponents")),
```

Place it adjacent to the existing `preceded(tag("one of your "), tag("opponents"))` arm. No new type.
The "during your turn" + "only once each turn" tails are already consumed by the existing constraint
scanners — executor confirms both `OnlyDuringYourTurn` and `OncePerTurn` attach to Molten Lavamancer's
`def` (they are separate clauses; verify the constraint scanner runs over the full trigger text).

### Card 4 — Flying Drone (M2b, entry-snapshot filter-eval)

**Root cause (verified):** Parser, `CostReduction { condition }`, `ParsedCondition::
BattlefieldEntriesThisTurn`, and the `battlefield_entries_this_turn` ledger all exist and the
"another creature with flying" subject parses to `FilterProp::WithKeyword(Flying)` + `FilterProp::
Another`. The blocker is `game/restrictions.rs battlefield_entry_matches_filter` (~393–421), whose
property match arm only handles `HasColor` and `InZone` and returns `false` for every other prop —
so `WithKeyword` and `Another` silently fail the condition. Also `BattlefieldEntryRecord` does not
snapshot keyword state at ETB.

**Changes (no new enum variant):**

1. **`types/game_state.rs` — `BattlefieldEntryRecord`:** add an additive keyword snapshot field:
   ```rust
   /// CR 400.7 + CR 603.10a: keyword abilities the object had at the moment it entered, so
   /// look-back conditions ("a creature with flying entered this turn") evaluate against the
   /// entry-time characteristics (consistent with the existing core_types/color snapshots).
   #[serde(default, skip_serializing_if = "Vec::is_empty")]
   pub keywords: Vec<Keyword>,
   ```
   Populate it where the record is built (`restrictions.rs` ~342, beside `core_types`/`color`) from
   the entering object's resolved keywords (use the existing keyword-query building block in
   `game/keywords.rs`, not a hand-rolled scan).

2. **`game/restrictions.rs` — `battlefield_entry_matches_filter` (~393):** thread `source_id`
   (already available at the `evaluate_condition` call site `casting.rs apply_cost_reduction`) into
   this function, and extend the property match arm:
   ```rust
   // CR 702.9b: keyword presence is snapshot at entry time (see record.keywords).
   FilterProp::WithKeyword { keyword } => record.keywords.iter().any(|k| k == keyword),
   // CR 113.x "another": exclude the ability's own source object.
   FilterProp::Another => record.object_id != source_id,
   ```
   Executor verifies the exact `FilterProp::WithKeyword` shape (field name `keyword` vs tuple) and
   that `BattlefieldEntryRecord` carries an `object_id` for the `Another` exclusion (add it as an
   additive field if absent — it is required for the "another" semantics and is generally useful).
   Keep the existing `_ => false` for genuinely-unsupported props so coverage stays honest.

**Rules note (CR look-back):** snapshotting flying at ETB matches the existing entry-record design
(core_types/color are already entry-time snapshots) and is the defensible reading of "a creature
with flying entered this turn". Annotate accordingly.

### Card 5 — Captain America, Living Legend (M2c, first-time-tapped intervening-if)

**Root cause (verified):** Taps trigger + `OnlyDuringYourTurn` constraint exist and parse. The
intervening-if "if it's the first time that creature has become tapped this turn" has no
representation: the per-ability `OncePerTurn` family counts ability firings, not per-object tap
events, and the ability here fires for *many* different creatures.

**Changes:**

1. **`types/ability.rs` — `TriggerCondition`** (beside the other look-back conditions): add
   ```rust
   /// CR 701.26 + CR 603.4: True iff the triggering object (the permanent that became tapped)
   /// has become tapped exactly once so far this turn — i.e. this is the first time. Read from
   /// GameState.object_tap_count_this_turn against the Taps event's object id.
   FirstTimeObjectTappedThisTurn,
   ```
   (APPROVED by the gate above.)

2. **`types/game_state.rs` — `GameState`:** add the per-object ledger (mirrors `exerted_this_turn` /
   `damage_dealt_this_turn`):
   ```rust
   /// CR 701.26: count of times each object became tapped this turn; cleared at turn start.
   #[serde(default, skip_serializing_if = "HashMap::is_empty")]
   pub object_tap_count_this_turn: HashMap<ObjectId, u32>,
   ```
   Increment it at the single authority that emits the Taps event / sets `tapped` (find the tap
   keyword-action site in `game/` — the same place the `Taps` `GameEvent` is produced — do NOT
   scatter increments). Clear it at turn start beside `battlefield_entries_this_turn.clear()`
   (`game/turns.rs` ~604).

3. **`game/triggers.rs` — `check_trigger_condition`** (~5091, beside `DealtDamageBySourceThisTurn`):
   ```rust
   TriggerCondition::FirstTimeObjectTappedThisTurn => trigger_event
       .and_then(|e| match e { GameEvent::Taps { object_id, .. } => Some(*object_id), _ => None })
       .is_some_and(|id| state.object_tap_count_this_turn.get(&id).copied() == Some(1)),
   ```
   Per CR 603.4 the intervening-if is checked at BOTH trigger time and resolution. Confirm this
   condition is consulted on both paths (the existing `check_trigger_condition` is the shared
   authority — verify it runs at resolution as well, otherwise the count may have advanced).
   **Increment-vs-check ordering is load-bearing:** the ledger must be incremented to 1 *before* the
   condition is evaluated for this event, so "first time" reads `== Some(1)`. Executor confirms the
   Taps event is recorded before triggers are matched (standard event-then-trigger order); add a
   discriminating test for the second tap in the same turn (count==2 ⇒ no untap).

4. **`parser/oracle_trigger.rs`:** parse the intervening-if "if it's the first time that creature has
   become tapped this turn" (and the generic "that permanent") into
   `condition: Some(TriggerCondition::FirstTimeObjectTappedThisTurn)`. Use nom combinators
   (`tag`/`alt`), placed in the intervening-if condition parser that already feeds `parse_inner_condition`
   — delegate to the shared condition combinator path, do not hand-roll string matching. The subject
   "that creature" binds to the triggering (tapped) object, not the source.

### Card 6 — Beast, Erudite Aerialist (stretch / verify-first)

**Status (verified):** Infra exists — Beast's "as long as you've put one or more +1/+1 counters on
Beast this turn, he has flying" maps to `StaticCondition::QuantityComparison { lhs:
QuantityRef::CounterAddedThisTurn { actor, counters, target }, GE, 1 }`. BUT
`parse_counter_added_target` (`oracle_nom/quantity.rs` ~3007) only recognizes
"creature(s)"/"permanent(s)" targets — it has **no self-reference `~` arm**. Beast's "on Beast" →
self-ref-normalized "on ~" fails to parse the target; the condition then falls through (likely to a
permissive/unrecognized static — consistent with gap=0 + `supported:true` yet broken at runtime: Beast
would have flying unconditionally or never).

**Verify-then-fix:** First write a discriminating runtime test (counters on Beast → flying; counters
on *another* creature only → no flying) to pin whether the failure is the parser target arm or a
runtime source-filter gap in `QuantityRef::CounterAddedThisTurn`. If parser-only, add a self-ref arm:
```rust
value(TargetFilter::SelfRef, tag("~")),  // CR 201.5 self-reference → the source object
```
to `parse_counter_added_target`, and confirm the runtime quantity resolver filters counter events to
the source object when `target == SelfRef`. **Include in Wave 2 only if the diagnosis shows it is
parser-only; otherwise reclassify as an M0 runtime-investigation card and defer.** Do not let Beast
expand the wave's scope.

---

## Logic Placement (engine-planner Step 4)

| Logic | Layer | Why |
|---|---|---|
| Keyword name-guard / corroboration | card-data (`database/synthesis.rs`) | Keyword synthesis is a build-time data-shaping step; sits beside the craft retain. |
| ETB-this-turn templating; plural opponent recipient; first-time-tapped phrase; Beast `~` arm | parser | Text → typed AST only; zero game logic. |
| Tap ledger increment/clear; first-time condition eval; entry-snapshot keyword/Another match | engine (`game/…`) | Pure game-state tracking + condition evaluation. |
| Keyword snapshot on `BattlefieldEntryRecord` | engine state | Entry-time characteristics are engine-owned look-back data. |

Frontend: **none** (no new WaitingFor / GameAction). AI: none beyond existing trigger/cost handling.

## Building Blocks reused (engine-planner Step 4)

`merge_extracted_keywords` + craft-retain pattern + `Keyword: Display` (synthesis); `parse_type_phrase`,
`ParsedCondition::{YouHadArtifactEnterThisTurn, BattlefieldEntriesThisTurn}`, `restrictions::
evaluate_condition`, `battlefield_entry_matches_filter` (Fixer/Flying Drone); `try_parse_source_deals_
damage_trigger`, `opponent_player_filter`, `DamageKindFilter::NoncombatOnly` (Molten Lavamancer);
`TriggerMode::Taps`, `TriggerConstraint::OnlyDuringYourTurn`, `check_trigger_condition`, the
`damage_dealt_this_turn` ledger pattern, `game/keywords.rs` keyword queries (Captain America);
`parse_counter_added_target`, `QuantityRef::CounterAddedThisTurn`, `TargetFilter::SelfRef` (Beast).
Only genuinely new code: one `TriggerCondition` variant + one `HashMap` ledger + one
`BattlefieldEntryRecord` field + 5 nom arms + one synthesis retain.

## Nom Compliance (mandatory — parser files change)

All dispatch uses combinators: `opt(tag(" the battlefield"))` + tuple-sequence (Fixer);
`preceded(tag(..), tag(..))` added to an existing `alt` (Molten Lavamancer); `tag`/`alt` for the
first-time-tapped intervening-if delegated to the shared condition combinator (Captain America);
`value(TargetFilter::SelfRef, tag("~"))` (Beast). No `contains`/`find`/`starts_with`/`split` for
parsing dispatch is introduced. The Storm guard is data-layer (not parser dispatch) and uses set
membership + value-equality, which is correct there.

---

## Verification Matrix (engine-planner Step 4 — discriminating, pipeline-driven)

| Card | Changed seam | Production entry | Discriminating runtime test | Revert-failing assertion | Sibling / negative cases |
|---|---|---|---|---|---|
| **Storm** | synthesis name-guard | `build_card_face` synthesis | Build Storm's face from real MTGJSON (keywords incl. "Storm", name "Storm, Queen of Wakanda", oracle with standalone "Flying") via the synthesis face-builder; assert result keywords contain `Flying` and **not** `Storm`. | Without the retain, `Storm` survives. | **Flying Men negative**: name "Flying Men", MTGJSON `[Flying]`, oracle "Flying" ⇒ `Flying` retained (corroborated). Non-colliding keyword (`Flying` on Storm) always retained. |
| **Fixer** | `parse_etb_this_turn_condition` | activation-restriction → `restrictions::evaluate_condition` | `GameScenario`: control Fixer + cause an artifact to enter this turn ⇒ `{T},Pay 2: Draw` is a legal activation; with **no** artifact entering ⇒ illegal. | Revert the `opt` ⇒ restriction unparsed ⇒ activation legal even with no artifact entry (wrong). | "the battlefield" full form still parses (creature/artifact arms); creature-entered variant still works. |
| **Molten Lavamancer** | `parse_opponent_player_recipient` | `match` on `DamageDone` trigger | `GameScenario`: a source you control deals **noncombat** damage to an opponent on **your** turn ⇒ create 1/1 Elemental, once; second noncombat damage same turn ⇒ no 2nd token. | Drop the arm ⇒ trigger never parses ⇒ no token. | Combat damage ⇒ no token (`NoncombatOnly`); damage on opponent's turn ⇒ no token (`OnlyDuringYourTurn`); damage to self ⇒ no token. |
| **Flying Drone** | `battlefield_entry_matches_filter` + record snapshot | `apply_cost_reduction` | `GameScenario`: have **another** creature with flying enter this turn, then activate ⇒ cost is {U}; with no other flyer entering ⇒ cost is {1}{U}. | Revert the prop arm ⇒ filter returns false ⇒ reduction never applies. | Source itself entering (no other flyer) ⇒ no reduction (`Another`); a non-flyer entering ⇒ no reduction (`WithKeyword`); flyer entering under opponent's control ⇒ no reduction (controller). |
| **Captain America** | `TriggerCondition::FirstTimeObjectTappedThisTurn` + ledger | Taps trigger → `check_trigger_condition` | `GameScenario` on your turn: tap a creature you control the **first** time ⇒ it untaps; tap a (different/second) time on the same creature ⇒ **no** untap (count==2). | Drop the condition/ledger ⇒ untaps on every tap. | Tap during **opponent's** turn ⇒ no trigger (`OnlyDuringYourTurn`); tapping a **different** creature its first time ⇒ untaps (per-object, not per-ability). |
| **Beast** *(stretch)* | `parse_counter_added_target` `~` arm | static layer eval | `GameScenario`: put a +1/+1 counter on Beast ⇒ Beast has flying; put counters only on another creature ⇒ Beast lacks flying. | Without `~` arm, condition is permissive/unparsed ⇒ flying state independent of counters-on-Beast. | Counter on another creature only ⇒ no flying (source-filtered). |

**Coverage honesty:** No Oracle text is accepted with deferred semantics — every accepted phrase has a
wired runtime path. Beast stays unsupported until its discriminating test confirms a parser-only fix;
if the test reveals a runtime gap it is reclassified/deferred so coverage stays red/honest. The
`battlefield_entry_matches_filter` `_ => false` fallthrough is preserved so genuinely-unsupported
props remain unsupported (no silent over-acceptance).

---

## File / Seam Map (sequential, one checkout; ordered)

| Order | File | Cards | Change |
|---|---|---|---|
| 1 | `database/synthesis.rs` | Storm | name-guard retain before `merge_extracted_keywords` |
| 2 | `parser/oracle_condition.rs` | Fixer | optional " the battlefield" in `parse_etb_this_turn_condition` |
| 3 | `parser/oracle_trigger.rs` | Molten Lavamancer, **Captain America** | plural-opponent recipient arm; first-time-tapped intervening-if parse |
| 4 | `types/ability.rs` | Captain America | `TriggerCondition::FirstTimeObjectTappedThisTurn` |
| 5 | `types/game_state.rs` | Captain America, Flying Drone | `object_tap_count_this_turn`; `BattlefieldEntryRecord.keywords` (+`object_id` if absent) |
| 6 | `game/turns.rs` | Captain America | clear tap ledger at turn start |
| 7 | `game/triggers.rs` | Captain America | evaluate new condition |
| 8 | `game/restrictions.rs` | Flying Drone, Captain America | `WithKeyword`/`Another` match + thread `source_id`; populate entry keyword snapshot |
| 9 | `game/casting.rs` | Flying Drone | pass `source_id` to `battlefield_entry_matches_filter` (if not already) |
| 10 | `parser/oracle_nom/quantity.rs` | Beast *(stretch)* | `~` self-ref arm in `parse_counter_added_target` |

`oracle_trigger.rs` (order 3) is the only Wave-1-shared file — serialize against any open
`oracle_trigger.rs` work. Cards are otherwise independent; recommended sequencing groups by file to
minimize re-reads.

## Recommended Implementation Order (by ROI + risk)

1. **Storm** (highest ROI: fixes a shipped misparse + hardens a whole defect class; isolated file).
2. **Fixer** (lowest effort; parser-only; isolated file).
3. **Molten Lavamancer** (one-line `alt` arm; isolated within oracle_trigger.rs).
4. **Flying Drone** (medium; self-contained restriction-eval seam; no new variant).
5. **Captain America** (heaviest; new variant + ledger + parser; run the gate; sequence last).
6. **Beast** (only if the verify test shows parser-only; else defer to M0 wave).

## CR Numbers (all grep-verified against `docs/MagicCompRules.txt`)

- **CR 702.40** — Storm keyword ability (line 4298). ✓ (phantom keyword to drop)
- **CR 201.5 / 201.5b** — a name reference on an object means just that object (lines 1320/1325). ✓
  (memory note "CR 201.4" is imprecise — 201.4 is "choose a card name"; the correct rule is 201.5.)
- **CR 400.7** — object becomes a new object on zone change; entered-battlefield look-back basis (line 1948). ✓
- **CR 302.6** — summoning sickness / "since their most recent turn began" (line 1628). ✓ (existing `ParsedCondition::SourceEnteredThisTurn` cites this)
- **CR 602.5b** — activation restriction persists on the object (line 2541). ✓ (Fixer)
- **CR 120.3** — damage results (line 1095); noncombat damage is damage not dealt as combat damage (CR 510.2). ✓ (Molten Lavamancer)
- **CR 111.1** — token creation (line 645). ✓ (Molten Lavamancer 1/1 Elemental)
- **CR 701.26** — Tap and Untap keyword action (line 3512). ✓ (Captain America tap ledger + untap)
- **CR 603.4** — intervening-"if" clause, checked at trigger time AND resolution (line 2588). ✓ (Captain America)

---

## Confirmed Wave 2 Card Count

**5 confirmed cards** — Storm, Queen of Wakanda; Fixer, Techno Terror; Molten Lavamancer; Flying
Drone; Captain America, Living Legend — **plus 1 stretch** (Beast, Erudite Aerialist) gated on a
verify-first discriminating test. **One** new engine-enum variant in the entire wave
(`TriggerCondition::FirstTimeObjectTappedThisTurn`, gate-approved); everything else reuses existing
primitives or adds additive struct fields. Three of the five are parser/data-only; two
(Flying Drone, Captain America) touch the engine but require no new effect or replacement subsystem.
