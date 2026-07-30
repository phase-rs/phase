# MSH Coverage Wave 1 — Engine/Parser Implementation Plan

**Scope:** 3 clusters, 6 cards (Leader deferred — see §A.6). All deconflicted from open lgray PRs #3907 (mana) / #3916 (teamwork) and from merged work.

**Hard constraint:** `crates/engine/src/parser/oracle_trigger.rs` is touched by **all three clusters**. Implementation MUST be **sequential on a single checkout** — no parallel executors on this file. `types/triggers.rs`, `game/trigger_matchers.rs`, and `game/trigger_index.rs` are also shared (Cluster A only). Recommended order: **A → D → E** (A touches the most seams; D and E are smaller and disjoint from each other except the shared parser file).

**Inert-until-regen caveat:** This is parser+engine work. Parser-only AST changes are inert until card-data regen+redeploy (memory `parser_fix_inert_until_data_regen`). Every test MUST drive the **real trigger pipeline** through a synthesized card (`add_real_card` + rehydrate / `GameScenario`), never an AST-shape-only `from_oracle_text` assertion (memory `parsed_ast_not_consumed`, `runtime_tests_must_drive_pipeline`).

---

## Traced analogous feature (engine-planner Step 2 — hard gate)

**Cluster A traces `Adapt` / `Explore` end-to-end** (the canonical "dedicated `TriggerMode` keyed off `EffectResolved { kind }`" pattern):

`types/triggers.rs` (`TriggerMode::Adapt`, `TriggerEventKey::AdaptResolved`)
→ `parser/oracle_trigger.rs` (`SimpleEvent::Explores` + `value(SimpleEvent::Explores, tag("explores"))` + mapping arm `def.mode = TriggerMode::Explored; def.valid_card = Some(subject.clone())`)
→ `game/effects/adapt.rs` / `explore.rs` (`events.push(GameEvent::EffectResolved { kind: EffectKind::Adapt, source_id })` — **source_id is the acting creature**)
→ `game/trigger_matchers.rs` (`match_adapt` / `match_explored`: destructure `EffectResolved { kind, source_id: acted_id }`, then `valid_card_matches(trigger, state, *acted_id, source_id)`) — registered in BOTH `trigger_matcher()` exhaustive `match` (line ~45) AND `build_trigger_registry()` `r.insert` (line ~421)
→ `game/trigger_index.rs` (trigger-side `TriggerMode::Adapt => push(TriggerEventKey::AdaptResolved)` at line ~400; event-side `EffectKind::Adapt => push(TriggerEventKey::AdaptResolved)` at line ~682).

**Cluster D traces the existing cast-from-zone shape** (Rocco, Street Chef): `parser/oracle_trigger.rs` lines ~10282–10296 — `split_once_on(payload, " from ")` + `parse_origin_constraint_tail(tail, parse_cast_origin_zone)` → `def.spell_cast_origin = constraint`; honored at runtime by `match_spell_cast` (line ~2449, `trigger.spell_cast_origin` gate).

**Cluster E traces `BecomesTargetSpellOrAbility`** (Valiant cards): `parser/oracle_trigger.rs` line ~7522 — `def.mode = TriggerMode::BecomesTarget; set_trigger_subject(...); def.valid_source = Some(becomes_target_source_filter(controller))`; runtime `match_becomes_target` honors `valid_source` (a `TargetFilter::StackSpell`/`StackAbility`).

---

## Pattern Coverage (engine-planner Step 4)

| Cluster | New capability | Class covered (not just these cards) | Est. cards |
|---|---|---|---|
| A | `TriggerMode::Connives` — "whenever [subject] connives" | **Every** "whenever a creature you control connives / whenever ~ connives" payoff. Connive is an evergreen-ish keyword (SNC, NEO Commander, MKM, OTJ, MSH…). | MSH: 3 in-scope + Leader (deferred). Whole class across sets: ~15–25. |
| D | `PlayCard` trigger accepts optional `from <zone>` | **Every** "whenever you play a card from exile / from your graveyard" trigger (impulse-draw payoffs, foretell/adventure synergy). | MSH: Klaw. Class: ~10+. |
| E | `BecomesTarget` of an **ability** (ability-only source) + optional controller | **Every** "becomes the target of an ability [you control / an opponent controls]" trigger — distinct from the existing spell-or-ability and spell-only arms. | MSH: Loki. Class: ~6+. |

No cluster builds for a single card; each adds a building block keyed on a CR-defined event class.

---

## Cluster A — `TriggerMode::Connives` (Glorious Purpose, Iron Monger Sadistic Tycoon, Ultron Unlimited)

### A.1 — `add-engine-variant` gate (mandatory)

Two new engine-enum variants are proposed: `TriggerMode::Connives` and `TriggerEventKey::ConniveResolved`. Both routed through the three-stage filter.

**Stage 1 — Existence verification:**
- `rg -n "Connive" data/engine-inventory.json` → `EffectKind::Connive` (effect, line 11690) and `Effect::Connive` (line 9608) and `ConniveDiscard` (ChoiceType, 35564) **exist**. The TRIGGER side does **not**: no `TriggerMode::Connive*` and no `TriggerEventKey::Connive*` (`rg "Connive" crates/engine/src/types/triggers.rs` → 0 hits; FromStr table has no "Connive" arm).
- Verdict: **DOES_NOT_EXIST** for the trigger mode / event key. (The effect already exists and emits the event — we are adding the listener half.)

**Stage 2 — Parameterization / sibling-cluster smell:** `TriggerMode` already carries one dedicated variant **per keyword action** that resolves via `EffectResolved { kind }`: `Explored`, `Adapt`, `Discover`, `ManifestDread`, `Surveil`, `Scry`, `Renown`-family, etc. Each maps to a structurally distinct `EffectKind` and a distinct `TriggerEventKey`; the dispatch key **is** the keyword identity. There is no `X / OpponentX / TargetX` naming axis and no comparator/aggregator/scope axis to parameterize — these are siblings-by-design (the same way the enum already holds 140+ event-class variants). No sibling-cluster smell. Verdict: **EXTEND_OK**.

**Stage 3 — Categorical boundary:** Connive is a single keyword-action rule section, **CR 701.50**. The matcher dispatches solely on `EffectKind::Connive`. Single section. Verdict: **WITHIN_SECTION**.

**Result: `APPROVED`** — `TriggerMode::Connives` (unit) + `TriggerEventKey::ConniveResolved` (unit), CR 701.50.

### A.2 — `crates/engine/src/types/triggers.rs`
1. Add `TriggerEventKey::ConniveResolved` (after `ManifestDreadResolved` / near the keyword-action keys), doc: `/// CR 701.50: A permanent connived (the connive process — draw, discard, maybe +1/+1 — completed).`
2. Add `TriggerMode::Connives` near `TriggerMode::Explored` (in the "Adapt / amass / learn" or "Triggered mechanics" cluster), doc: `/// CR 701.50b: Triggers when a permanent connives (after the connive process completes).`
3. Add FromStr arm: `"Connives" => TriggerMode::Connives,`.
4. Update the `trigger_mode_count_at_least_141` test list and the `>= 146` assertion (bump to ≥147) — add `"Connives"`.

### A.3 — `crates/engine/src/game/effects/connive.rs` (event-identity fix — load-bearing)
**Problem:** All three `EffectResolved { kind: Connive }` emissions currently carry `source_id: ability.source_id` (the *causing* ability). The Adapt/Explore convention is that `source_id` is the **acting creature**, and `match_*` checks `valid_card` against it. When connive is caused by a *different* source (e.g. "target creature connives" from a spell), `ability.source_id` ≠ the conniver, so a "whenever a creature you control connives" filter would be evaluated against the wrong object. Nothing consumes this event today (no `TriggerMode::Connives`), so the change is safe and is the correct convention.

**Change — emit the conniver, not the ability source:**
- Prevented-draw path (line ~111–114): `source_id: ability.source_id` → `source_id: conniver_id` (`conniver_id` is already in scope, computed line ~35).
- Auto-discard completion path (line ~156–159): same change.
- `crates/engine/src/game/engine_resolution_choices.rs` discard-choice completion (line ~2289–2291): the `WaitingFor::ConniveDiscard { conniver_id, source_id, .. }` branch emits `EffectResolved { source_id }` — change to `source_id: conniver_id` (`conniver_id` is bound in the same destructure).

CR annotation to add at each site: `// CR 701.50b + CR 701.50c: the EffectResolved carries the CONNIVER's id (LKI if it left the battlefield) so "whenever a creature you control connives" matches the conniving permanent, not the causing source.`

### A.4 — `crates/engine/src/game/trigger_matchers.rs`
1. Add `match_connives` mirroring `match_adapt` exactly:
```rust
/// CR 701.50b: Connives — fires when a permanent connives.
/// `valid_card`/`valid_source` scope the CONNIVER. With no filter, "this
/// creature connives" — match the source by identity (Ultron's self-connive).
pub(super) fn match_connives(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let GameEvent::EffectResolved { kind: EffectKind::Connive, source_id: conniver_id } = event
    else { return false; };
    if trigger.valid_card.is_some() {
        valid_card_matches(trigger, state, *conniver_id, source_id)
    } else {
        *conniver_id == source_id
    }
}
```
2. **Register in BOTH seams** (dual-registration is a common miss):
   - `trigger_matcher()` exhaustive `match` (near line ~45, beside `TriggerMode::Adapt => match_adapt`): `TriggerMode::Connives => match_connives,`. (The exhaustive match will fail to compile until this is added — good.)
   - `build_trigger_registry()` `r.insert` block (near line ~421): `r.insert(TriggerMode::Connives, match_connives);`. (NOT exhaustive-checked — silent no-match if omitted.)

### A.5 — `crates/engine/src/game/trigger_index.rs`
1. Trigger-side derivation (near line ~400, beside `TriggerMode::Adapt => push(TriggerEventKey::AdaptResolved)`): add `TriggerMode::Connives => push(TriggerEventKey::ConniveResolved),`.
2. Event-side `keys_from_effect_kind` (line ~668): **move `EffectKind::Connive` out of the no-op arm into a dispatching arm**: `EffectKind::Connive => push(TriggerEventKey::ConniveResolved),` (beside `EffectKind::Adapt => push(TriggerEventKey::AdaptResolved)`). The exhaustive match forces this edit (Connive currently sits in the explicit no-op list — remove it from there).

### A.6 — `crates/engine/src/parser/oracle_trigger.rs` (subject parser)
1. Add `SimpleEvent::Connives` to the local `SimpleEvent` enum (near `Explores`), doc `/// CR 701.50b: A permanent "connives" after the connive process completes.`.
2. In `parse_simple_event`, add a `value` arm (place beside `Explores` — no prefix collision with any existing arm): `value(SimpleEvent::Connives, tag("connives")),`. Nom-compliant (`value`+`tag`, no string scanning).
3. In the `SimpleEvent → def` mapping (beside the `SimpleEvent::Explores` arm, line ~7609):
```rust
SimpleEvent::Connives => {
    if !remaining.trim().is_empty() { return None; }
    // CR 701.50b: "connives" fires after the connive process completes.
    def.mode = TriggerMode::Connives;
    def.valid_card = Some(subject.clone());
}
```
This handles **Glorious Purpose** / **Iron Monger** ("whenever a creature you control connives" → `subject` = "a creature you control" → `valid_card`) and **Ultron**'s second ability identically. Ultron's own self-connive matches because `valid_card` "a creature you control" includes Ultron.

### A.7 — Ultron's first ability dependency ("Whenever Ultron attacks, he connives")
The **trigger** is `Attacks` (existing). The **effect** "he connives" must parse to `Effect::Connive`. **Pre-check (executor must verify before claiming Ultron works):** grep `parser/oracle_effect*` for a connive-effect arm and confirm `"he connives" / "it connives" / "~ connives"` (self-referential subject) produces `Effect::Connive { target: <self>, count: Fixed 1 }`. If the connive *effect* parser only handles "target creature connives" and not the anaphoric self form, add a self-subject arm (small, in-scope) — otherwise Ultron's chain (attack → connive → second trigger) never starts. Flag in the impl report if this requires more than a one-arm addition.

### A.8 — Leader, Super-Genius — **DEFER (recommendation: do NOT include in Wave 1)**
"If a creature you control would connive, instead you draw a card, then that creature connives" is a **connive-interception replacement effect**, not a trigger. It requires: (1) a new `ReplacementCondition` for "would connive" (or equivalent event-replacement hook on the connive proposed-event path), (2) a substitute effect ("draw a card, then connive") wired through the replacement pipeline, (3) `/add-replacement-effect` lifecycle wiring. That is materially larger than the trigger work and is architecturally independent. **Recommend a dedicated follow-up** (its own plan through `/review-engine-plan`). Leaving it out does not block the 3 trigger cards. Record a strict-fail/deferred marker so coverage stays honest (memory `no_default_deferral` exception applies — this is genuinely a separate replacement-effect subsystem, not a primitive we should bolt onto the trigger fix).

---

## Cluster D — Klaw, Master of Sound ("Whenever you play a card from exile, …")

**No new engine variant.** Reuses `TriggerMode::PlayCard` + existing `spell_cast_origin: OriginConstraint`.

### D.1 — Runtime gap the task framing missed (must fix, NOT parser-only)
`match_play_card = match_spell_cast || match_land_played`. The two halves honor **different** origin fields:
- `match_spell_cast` enforces cast origin via `trigger.spell_cast_origin` (`OriginConstraint`) — line ~2449. **Setting `valid_card` InZone would BREAK the cast path** (the spell is in `Zone::Stack` at fire time, not Exile — confirmed by the comment at oracle_trigger.rs ~10277).
- `match_land_played` enforces origin via `valid_card`'s `FilterProp::InZone` (test `land_played_valid_card_matches_origin_zone`, line ~4954) — checked against the `LandPlayed` event's `from_zone`.

So a single shared `valid_card` can't carry the constraint. The rules-correct fix routes the origin through `spell_cast_origin` for the cast half **and** gates the land half on the same constraint inside `match_play_card`.

### D.2 — `parser/oracle_trigger.rs` — `parse_play_card_trigger_subject`
Change return type `Option<()>` → `Option<OriginConstraint>` (return `OriginConstraint::Any` when no tail). After matching `tag("a card")`, instead of requiring `eof`/`,` immediately, optionally consume a `" from <zone>"` tail using the **existing** building blocks (nom-compliant, mirrors the Rocco cast path):
```rust
// after `let (after_card, _) = tag("a card").parse(after_verb).ok()?;`
let rest = after_card.trim_start();
let (rest, origin) = match nom_primitives::split_once_on(rest, "from ") {
    Ok((_, (_before, after))) => {
        let tail = format!("from {after}");
        match parse_origin_constraint_tail(tail.as_str(), parse_cast_origin_zone) {
            Ok((tail_rest, c)) => (tail_rest, c),       // consumed "from <zone>"
            Err(_) => (rest, OriginConstraint::Any),     // unrecognized → leave intact
        }
    }
    Err(_) => (rest, OriginConstraint::Any),
};
// then require eof / "," on the remaining text exactly as today
alt((value((), eof), value((), tag(","))).parse(rest).ok()?;
Some(origin)
```
(Adjust to thread the `before` slice correctly so the eof/comma gate runs on the residual after the zone tail; keep the existing strict end-anchor so non-zone qualifiers still decline.)

Caller (line ~9995):
```rust
if let Some(origin) = parse_play_card_trigger_subject(lower) {
    let mut def = make_base();
    def.mode = TriggerMode::PlayCard;
    def.valid_target = Some(TargetFilter::Controller);
    def.spell_cast_origin = origin;          // cast half honors this
    return Some((TriggerMode::PlayCard, def));
}
```

### D.3 — `game/trigger_matchers.rs` — `match_play_card` land-half origin gate
```rust
pub(super) fn match_play_card(event, trigger, source_id, state) -> bool {
    if match_spell_cast(event, trigger, source_id, state) { return true; }
    // CR 601.1a + CR 305.1: the land-play half honors the same play-origin
    // constraint the cast half routes through `spell_cast_origin`, since
    // `match_land_played` itself only consults `valid_card` (wrong for the
    // shared PlayCard def, whose spell would be on the stack).
    if let GameEvent::LandPlayed { from_zone, .. } = event {
        if !trigger.spell_cast_origin.matches_from(&Some(*from_zone)) { return false; }
    }
    match_land_played(event, trigger, source_id, state)
}
```
`OriginConstraint::matches_from(&Option<Zone>)` is the single-authority helper (types/ability.rs ~13956). For pure `LandPlayed` triggers `spell_cast_origin` is `Any` → `matches_from` returns true → no regression.

---

## Cluster E — Loki, God of Mischief ("Whenever a player or permanent becomes the target of an ability you control, draw a card. This ability triggers only once each turn.")

**No new engine variant.** Reuses `TriggerMode::BecomesTarget` + `TargetFilter::StackAbility` (existing) + `TriggerConstraint::OncePerTurn` (existing).

### E.1 — Deviation from task spec (recommend, with justification)
The task proposed `SimpleEvent::BecomesTargetAbility { controller: Option<ControllerRef> }`. **Recommend a unit variant `SimpleEvent::BecomesTargetAbility`** (no payload) and parse the "you control" / "an opponent controls" suffix from `remaining` via the existing `parse_target_source_controller`, exactly as `BecomesTargetSpellOrAbility` already does (oracle_trigger.rs ~7522–7531). Rationale: keeps `SimpleEvent` a leaf event-kind enum (no controller state baked into it), reuses the established helper, and mirrors the sibling arm one-for-one. `SimpleEvent` is parser-local (not an engine-inventory enum), so the `add-engine-variant` gate does not formally apply, but the leaf-layer principle still does.

### E.2 — `parser/oracle_trigger.rs`
1. Add `SimpleEvent::BecomesTargetAbility` to the enum (near `BecomesTargetSpellOrAbility`).
2. In `parse_simple_event`, add an arm: `value(SimpleEvent::BecomesTargetAbility, tag("becomes the target of an ability")),` (+ plural `tag("become the target of an ability")`). No prefix collision: "of an ability" ≠ "of a spell or ability" / "of a spell" / "of a backup ability". Place it in the `.or(alt((…)))` block that already holds the backup-ability arm (the first `alt` is at nom's tuple-arity limit — add to a continuation block, mirroring the backup-ability placement comment at ~7425).
3. Mapping arm:
```rust
SimpleEvent::BecomesTargetAbility => {
    def.mode = TriggerMode::BecomesTarget;
    set_trigger_subject(&mut def, subject);
    // CR 115.1: source is an ABILITY on the stack (not a spell), optionally
    // controller-scoped by a trailing "you control" / "an opponent controls".
    let controller = parse_target_source_controller(remaining);
    def.valid_source = Some(TargetFilter::StackAbility { controller, tag: None, kind: None });
}
```

### E.3 — "a player or permanent" subject — **risk to resolve in impl**
`rg "player or permanent"` → 0 hits; the upstream trigger-subject decomposition may not produce a usable filter for "a player or permanent". Loki's subject is the broadest possible (any object or player) → the correct mapping is **no subject restriction** (`valid_target = None`, `valid_card = None`), relying solely on `valid_source`. Executor MUST:
- Trace how the enclosing trigger parser binds `subject` for "a player or permanent …".
- If it yields a clean broad/`Any` filter, `set_trigger_subject` may set a permissive `valid_card`/`valid_target` — verify `match_becomes_target` treats it as "matches any target". If it yields `TargetFilter::Any` or a malformed subject that declines, add a small combinator arm recognizing "a player or permanent" as the **unrestricted** subject (leave both subject filters `None`).
- Discriminating guard: an **opponent's** ability targeting *your* creature must NOT trigger (gated by `valid_source` controller=You), and a **spell** you control targeting must NOT trigger (ability-only `StackAbility`).

### E.4 — Once-each-turn (reuse, no new infra)
"This ability triggers only once each turn." is detected by the existing scanner (oracle_trigger.rs ~1404–1409) → `TriggerConstraint::OncePerTurn`. No change. Verify the constraint survives onto Loki's `def` (the becomes-target line and the once-each-turn sentence are separate clauses; confirm the constraint scanner runs over the full Oracle text and attaches to this def).

---

## Logic Placement (engine-planner Step 4)

| Logic | Layer | Why |
|---|---|---|
| Connive event identity (conniver_id) | engine (`effects/connive.rs`, `engine_resolution_choices.rs`) | Event payload is engine-owned; the conviver's LKI is game state. |
| `match_connives` / `match_play_card` origin gate | engine (`trigger_matchers.rs`) | Trigger matching is pure engine logic. |
| Trigger-index keys | engine (`trigger_index.rs`) | CR 603.2 over-approximation bucket derivation. |
| Oracle phrase → typed `TriggerMode`/`SimpleEvent`/`OriginConstraint`/`StackAbility` | parser (`oracle_trigger.rs`) | Text→AST only; zero game logic. |

Frontend: **none** (no new WaitingFor, no new GameAction — connive's existing `ConniveDiscard` UI is untouched). AI: none beyond existing trigger handling.

## Building Blocks reused (no new helpers)
`EffectResolved`/`EffectKind::Connive` (already emitted), `valid_card_matches`, `match_adapt` (template), `trigger_matcher` + `build_trigger_registry`, `keys_from_effect_kind`, `OriginConstraint::matches_from`, `parse_origin_constraint_tail` + `parse_cast_origin_zone`, `nom_primitives::split_once_on`, `parse_target_source_controller`, `set_trigger_subject`, `TargetFilter::StackAbility`, `TriggerConstraint::OncePerTurn`, `SimpleEvent` + `value`/`tag`/`alt`. Only genuinely new code: `match_connives` (mirrors `match_adapt`), `TriggerMode::Connives`, `TriggerEventKey::ConniveResolved`, three `SimpleEvent` arms.

## Nom Compliance
All parser dispatch uses combinators: `value(.., tag(..))` for the three new `SimpleEvent` arms; `split_once_on` + `parse_origin_constraint_tail` + `parse_cast_origin_zone` + `eof`/`tag` for the play-card tail; `parse_target_source_controller` (`alt`/`value`/`tag`) for the ability controller. No `contains`/`find`/`starts_with` introduced for dispatch. The parser **is** the detector (e.g. `parse_play_card_trigger_subject` returns `Some(origin)` rather than scanning for substrings).

---

## Verification Matrix (engine-planner Step 4 — discriminating, pipeline-driven)

| Card | Changed seam | Production entry | Discriminating runtime test (drive real pipeline) | Revert-failing assertion | Sibling / negative cases |
|---|---|---|---|---|---|
| **Glorious Purpose** | A.2–A.6 | trigger pipeline on connive | Control the card + a creature; cause the creature to connive; assert Glorious Purpose's effect resolves (CastOutcome/state delta). | Without `TriggerMode::Connives` wiring (matcher or index), no effect delta. | Opponent-controlled conniver ⇒ no trigger (`valid_card` = "a creature you control"). |
| **Iron Monger, Sadistic Tycoon** | same | same | Control Iron Monger + a Villain + a conniving creature; connive; assert the Villain gains a +1/+1 counter. | Drop A.5 event-side key ⇒ index never routes ⇒ no counter. | 0 Villains ⇒ no counters; non-controlled conviver ⇒ no trigger. |
| **Ultron, Unlimited** | A.6 (attack→connive) + A.7 effect | Attacks trigger → `Effect::Connive` → Connives trigger | Ultron attacks; assert connive resolves (draw+discard, +1/+1 on nonland) AND the second "whenever a creature you control connives" effect fires. | If A.7 self-connive effect parse missing, chain never starts. | — |
| **source_id fix (A.3)** | `effects/connive.rs` identity | "target creature connives" caused by an **external** source | Make a creature you control connive via a source whose own characteristics do NOT satisfy the watcher's `valid_card`; assert the watcher fires (matches the **conviver**, not the source). | Keep `source_id: ability.source_id` ⇒ filter checks the wrong object ⇒ watcher mis-fires/no-fires. | Conniver leaves battlefield mid-resolution ⇒ LKI still matches (CR 701.50c). |
| **Klaw, Master of Sound** | D.2 + D.3 | `match_play_card` | Cast a card from **exile** ⇒ Klaw fires; cast the same from **hand** ⇒ does NOT fire; play a land from **exile** ⇒ fires; land from **hand** ⇒ does NOT. | Drop D.2 (origin capture) ⇒ fires on hand casts. Drop D.3 ⇒ fires on hand land-plays. | Opponent plays from exile ⇒ no fire (`valid_target = Controller`). |
| **Loki, God of Mischief** | E.2 + E.4 | `match_becomes_target` | Your **ability** targets a permanent ⇒ draw 1, once/turn (2nd target same turn ⇒ no 2nd draw). | Drop E.2 ⇒ line is `unimplemented` ⇒ no trigger. Drop `OncePerTurn` ⇒ draws twice. | Opponent's ability targeting ⇒ no draw; your **spell** targeting ⇒ no draw (ability-only `StackAbility`). |

**Coverage honesty:** No Oracle text is accepted-with-deferred-semantics in this plan — every accepted phrase has a wired runtime matcher. Leader, Super-Genius stays **unsupported** (deferred replacement effect, §A.8) so coverage remains red/honest for it.

---

## File / seam map (sequential, one checkout)

| Order | File | Clusters | Change |
|---|---|---|---|
| 1 | `types/triggers.rs` | A | `TriggerEventKey::ConniveResolved`, `TriggerMode::Connives`, FromStr arm, count test |
| 2 | `game/effects/connive.rs` | A | 2× `source_id` → `conniver_id` |
| 3 | `game/engine_resolution_choices.rs` | A | 1× `source_id` → `conniver_id` |
| 4 | `game/trigger_matchers.rs` | A, D | `match_connives` + 2 registrations (A); `match_play_card` land-origin gate (D) |
| 5 | `game/trigger_index.rs` | A | trigger-side + event-side `ConniveResolved` keys |
| 6 | `parser/oracle_trigger.rs` | A, D, E | `SimpleEvent::Connives` (A); `parse_play_card_trigger_subject` origin + caller (D); `SimpleEvent::BecomesTargetAbility` + subject handling (E) |
| 7 | `parser/oracle_effect*` | A | (conditional) self-connive effect arm if A.7 pre-check fails |

**Risk-scaled verification:** `cargo fmt --all` (always direct). Parser combinator gate (`check-parser-combinators.sh`) + targeted parser tests for the new arms. Tilt `clippy` + `test-engine` for the engine seams (matcher/index/effects) — these are shared trigger-machinery changes, so collect strong Tilt evidence before marking fixed. Do NOT run `cargo build/clippy/test` directly (target-lock contention).

## CR numbers (all grep-verified against `docs/MagicCompRules.txt`)
- **CR 701.50 / 701.50a / 701.50b / 701.50c / 701.50e** — Connive (line 3707–3719). ✓
- **CR 601.1a** — "playing a card" = play as land or cast as spell (line 2451). ✓ (Klaw)
- **CR 601.2i** — abilities trigger when a spell is cast (line 2473). ✓
- **CR 305.1 / 701.18a** — play a land special action (lines 1688 / 3412). ✓ (Klaw land half)
- **CR 115.1** — becomes the target of a spell or ability (verified). ✓ (Loki)
- **CR 603.2** — event→trigger matching / index over-approximation (verified). ✓
- Once-each-turn: handled by existing `TriggerConstraint::OncePerTurn` (no new CR assertion needed).
