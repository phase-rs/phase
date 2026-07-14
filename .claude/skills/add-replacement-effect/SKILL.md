---
name: add-replacement-effect
description: Use when adding or modifying replacement effects — ETB-tapped, shock lands, damage prevention, "as enters" choices, or any event-modifying ability. Covers ReplacementDefinition wiring, the pipeline flow, post-replacement effects, and interactive pre-zone-change choices.
---

# Adding a Replacement Effect

Replacement effects modify or prevent game events before they happen (MTG Rule 614.1). They are **not** triggered abilities — they don't use the stack. This skill covers wiring a new replacement through the pipeline: definition → parser → registry → handler → engine.

**Before you start:** Trace how shock lands work end-to-end. They're the most complete example: `parse_shock_land()` in `oracle_replacement.rs` → `ReplacementDefinition` with `Optional` mode → replacement pipeline → `handle_replacement_choice()` in `engine_replacement.rs` delivers the accept/decline effect inline → `apply_post_replacement_effect()` handles any remaining copy-target follow-up.

> **CR Verification Rule:** Every CR number in annotations MUST be verified by grepping `docs/MagicCompRules.txt` before writing. Do NOT rely on memory — 701.x and 702.x numbers are arbitrary sequential assignments that LLMs consistently hallucinate. Run `grep -n "^614.1" docs/MagicCompRules.txt` (etc.) for every number. If you cannot find it, do not write the annotation.

---

## MTG Rules Reference

| Rule | What it governs | Engine implication |
|------|----------------|-------------------|
| **614.1** | Replacement effects modify events, don't use the stack | Handled in `replacement.rs` pipeline, not `effects/` |
| **614.12** | Self-replacement effects apply even when the card isn't on the battlefield yet | `find_applicable_replacements()` scans the entering object in addition to battlefield |
| **614.16** | "As [permanent] enters" choices are replacement effects | Must resolve *before* zone change completes — see Interactive Replacements below |
| **616.1** | Multiple replacements on same event: affected player/controller chooses order | `pipeline_loop()` returns `NeedsChoice` when multiple candidates exist |
| **614.6** | A replacement can only apply once to a given event | `applied: HashSet<ReplacementId>` on `ProposedEvent` tracks this |

---

## Key Types

### `ReplacementDefinition` — `crates/engine/src/types/ability.rs`

```rust
pub struct ReplacementDefinition {
    pub event: ReplacementEvent,              // Which event type to intercept
    pub execute: Option<Box<AbilityDefinition>>, // Side effect on accept (Optional) or main action (Mandatory)
    pub mode: ReplacementMode,                // Mandatory | Optional { decline }
    pub valid_card: Option<TargetFilter>,      // Which card this applies to (usually SelfRef)
    pub description: Option<String>,          // UI text for player choice
    pub condition: Option<ReplacementCondition>, // Additional applicability check
}
```

### `ReplacementMode` — same file

- **`Mandatory`** — Always applies. Player doesn't choose whether it happens, only which order when multiple exist. Example: "enters tapped" on taplands.
- **`Optional { decline }`** — Player chooses accept or decline. `execute` runs on accept, `decline` runs on decline. Example: shock lands ("you may pay 2 life").

### `ProposedEvent` — `crates/engine/src/types/proposed_event.rs`

The event being evaluated. Key variant for ETB replacements:

```rust
ProposedEvent::ZoneChange {
    object_id, from, to, cause,
    enter_tapped: bool,           // Set by replacement handlers
    applied: HashSet<ReplacementId>, // Prevents re-application (Rule 614.6)
}
```

Other variants: `Damage`, `Draw`, `LifeGain`, `LifeLoss`, `Discard`, `Destroy`, `Sacrifice`, `Tap`, `Untap`, `AddCounter`, `RemoveCounter`, `CreateToken`.

### `ReplacementEvent` — `crates/engine/src/types/replacements.rs`

Enum of interceptable event types. Add a new variant here when the event being replaced doesn't match any existing type.

---

## The Pipeline — How Replacements Execute

```
Event proposed (zone change, damage, draw, etc.)
    ↓
replace_event() → find_applicable_replacements()
    Scans: battlefield + command zone + entering object (Rule 614.12)
    ↓
pipeline_loop():
    ├─ 0 candidates → Execute(proposed)  [no replacement]
    ├─ 1 Mandatory → auto-apply → re-enter pipeline at depth+1
    ├─ 1 Optional → NeedsChoice(player)  [save PendingReplacement]
    └─ 2+ candidates → NeedsChoice(player) [player orders per Rule 616.1]

Player responds with GameAction::ChooseReplacement { index }
    ↓
continue_replacement():
    ├─ Optional accept (index 0) → apply replacement, store execute as post_replacement_effect
    ├─ Optional decline (index 1) → skip replacement, store decline as post_replacement_effect
    └─ Mandatory → apply chosen candidate
    ↓
Re-enter pipeline_loop() → check for cascading replacements
    ↓
ReplacementResult::Execute(modified_event) → caller processes the event
```

### Delivery Lifecycle — Where Effects Actually Run

**Non-ZoneChange events deliver inline.** When `handle_replacement_choice()` in `engine_replacement.rs` receives the accepted `ProposedEvent`, its exhaustive match delivers the event directly: `Damage`, `Draw`, `LifeGain`/`LifeLoss`, `AddCounter`/`RemoveCounter`, `Tap`/`Untap`, `Discard`, `Destroy`, `Sacrifice`, `CreateToken`, `ProduceMana` — all execute in their match arm before the function returns. The replacement pipeline does not defer these through a separate "post-replacement" phase.

**ZoneChange events** route through `move_to_zone()` and then apply `enter_tapped`, `enter_with_counters`, `controller_override`, `enter_transformed` flags set by the replacement pipeline (see `handle_replacement_choice()` match arm for `ProposedEvent::ZoneChange`).

**`apply_post_replacement_effect()`** (in `engine_replacement.rs`) is the general-purpose side-effect resolver used *after* the event is delivered. It:
- Handles `Effect::BecomeCopy` specially by returning `WaitingFor::CopyTargetChoice` (CR 707.9 — "enter as a copy").
- Delegates everything else to `effects::resolve_ability_chain()`, so any `AbilityDefinition` variant is supported as a follow-up — not just a hand-picked pair.

**For effects that must happen *before* the zone change** (like "choose a basic land type" — CR 614.16), see Interactive Replacements below. Those pause the pipeline mid-flight rather than using the post-delivery lifecycle.

---

## Checklist — Adding a New Replacement

### Phase 1 — Type Definition

- [ ] **`crates/engine/src/types/replacements.rs` — `ReplacementEvent` enum** (if new event type)
  Add a variant for the event being intercepted. Skip if an existing variant fits.

- [ ] **`crates/engine/src/types/ability.rs` — `ReplacementCondition` enum** (if new condition)
  Add a variant if the replacement needs a condition beyond `valid_card` filtering.

### Phase 2 — Registry & Handler

- [ ] **`crates/engine/src/game/replacement.rs` — `build_replacement_registry()`**
  Add an entry mapping your `ReplacementEvent` → `ReplacementHandlerEntry { matcher, applier }`.

  - **`matcher`**: `fn(&ProposedEvent, ObjectId, &GameState) -> bool` — Returns true if this replacement applies to this event. Check event type, source object, and conditions.
  - **`applier`**: `fn(ProposedEvent, ObjectId, &mut GameState, &mut Vec<GameEvent>) -> ApplyResult` — Returns `Modified(new_event)` or `Prevented`. Modify the proposed event (e.g., set `enter_tapped = true`) and/or mutate state.

### Phase 3 — Parser

- [ ] **`crates/engine/src/parser/oracle_replacement.rs` — parsing function**
  Write a parser that recognizes the Oracle text pattern and returns `Option<ReplacementDefinition>`.

  Entry point: `parse_replacement(text: &str) -> Option<ReplacementDefinition>` — called from the main Oracle parser.

  Follow existing patterns:
  - `parse_shock_land()` — Optional mode with accept/decline AbilityDefinitions
  - `parse_enters_tapped()` — Mandatory mode with `enter_tapped` flag
  - `parse_etb_counter()` — Mandatory mode that modifies entering state

- [ ] **`crates/engine/src/parser/oracle.rs` — routing**
  Ensure the Oracle parser calls your new parser at the right priority. Replacement text is detected and routed before effect parsing.

### Phase 4 — Engine Integration (if post-replacement effect)

- [ ] **`crates/engine/src/game/engine_replacement.rs` — `apply_post_replacement_effect()`**
  If your replacement produces a post-zone-change side effect using a new `Effect` variant, extend this helper. It owns post-replacement side effects, copy-target follow-up, and replacement-choice execution after the zone change is committed.

- [ ] **`crates/engine/src/game/engine.rs` — routing only**
  Ensure the relevant `(WaitingFor::ReplacementChoice { .. }, GameAction::ChooseReplacement { .. })` or `CopyTargetChoice` route still delegates into `engine_replacement.rs`. Do not reintroduce replacement execution logic into `engine.rs`.

### Phase 5 — Tests

- [ ] Parser test: Oracle text → correct `ReplacementDefinition` (verbatim Oracle text, never a paraphrase)
- [ ] Pipeline test: proposed event → replacement applies → modified event — with an assertion that FAILS if the change is reverted; any "replacement does NOT apply" assertion needs a positive reach-guard proving the card parsed (see `/card-test` foot-gun 6)
- [ ] Engine flow test: full game action → replacement → zone change → post-effect
- [ ] Provenance test: identify the selected replacement identity carried through `ReplacementId`, `PendingReplacement`, `ProposedEvent::applied`, or the relevant pending/pre-zone-change state. Assert the selected replacement's execute/decline/rider/effect is the one consumed.
- [ ] Candidate-count tests when applicable: 0 candidates, exactly 1 candidate, and 2+ candidates where CR 616.1 order choice matters.
- [ ] Multi-replacement test: with two matching replacements, selecting one must not apply the other's execute/decline/rider/effect, and the applied set must prevent reapplication of exactly the selected replacement.
- [ ] Optional replacement test: accept and decline through the actual `GameAction::ChooseReplacement` / `engine_replacement.rs` path, not only direct `replacement.rs` helper calls.
- [ ] Serialized-state test: if `ProposedEvent`, pending replacement state, `WaitingFor`, `GameAction`, or serialized replacement fields change, add `#[serde(default)]` or an explicit migration plus a fixture/load test for existing repo-owned serialized data.
- [ ] Verify per CLAUDE.md § "Canonical verification pattern" — `cargo fmt --all`, then if `tilt get uiresource clippy >/dev/null 2>&1`: `./scripts/tilt-wait.sh --timeout 240 clippy test-engine card-data`; else: `cargo clippy --all-targets -- -D warnings` + `cargo test -p engine` + `./scripts/gen-card-data.sh`.

---

## "Instead" Lowers to a BRANCH, Never Two Effects

**CR 614.1a: effects that use the word "instead" are replacement effects. CR 614.6: if an
event is replaced, it never happens.** So an override clause is a *branch*, not a sibling —
emitting it as a second, independent effect makes the engine execute BOTH the original and
the replacement. That is the #44 / #79 defect class, and it was ~40 faces.

This bites hardest **across lines**, where the "instead" clause is a separate printed line
from the effect it overrides:

```
Destroy target creature.
If that creature would die this turn, exile it instead.   ← NOT a second ability
```

The correct lowering is an `AbilityDefinition` whose `else_ability` carries the *displaced*
branch — one def, two mutually exclusive outcomes. In the IR this is a **clause
disposition**, not an emitted sibling:

| IR carrier (`parser/oracle_ir/effect_chain.rs`) | Shape |
|---|---|
| `ClauseDisposition::ReplaceMeaning { kind }` | The clause replaces/overrides the meaning of the **prior emitted def(s)** rather than emitting an independent sibling (CR 608.2c: "later text on the card may modify the meaning of earlier text") |
| `ReplaceMeaningKind::Instead(..)` | Multi-clause base + "instead" override; the tail clauses are stashed in the override's `else_ability` |
| `ReplaceMeaningKind::DigAlt(..)` / `KeywordOverride` | The other two override shapes — the prior def is popped/wrapped, or attached as `sub_ability` |

**The rule for contributors:** if your new pattern's Oracle text contains an override
("instead", "rather than"), you are extending a `ReplaceMeaning` disposition — you are not
adding a second `AbilityDefinition` to `result.abilities`. If you find yourself emitting a
conditional ability whose condition duplicates the override's antecedent, stop: the engine
will run both branches.

**Test it at runtime, not at parse shape.** A parse test asserting two defs exist proves
nothing about which ones *execute*. Cast the card and assert the overridden effect did
**not** happen (see `/card-test`).

---

## Interactive Replacements (Pre-Zone-Change Choices)

**CR 614.1c** ("As [this permanent] enters . . ." is a replacement effect) **+ CR 614.12a**
(a replacement that modifies how a permanent enters and requires a choice makes that choice
as part of the replacement): the choice modifies the entering event itself — the permanent enters with the choice already made.

This is architecturally harder than standard replacements because it requires player input *during* the replacement pipeline, *before* the zone change completes.

### The Timing Invariant

**The permanent must never exist on the battlefield without its chosen characteristic set.** If the choice happens post-zone-change, there's a window where layers evaluate the permanent without the choice, which can cause incorrect ETB trigger behavior.

### Implementation Pattern

For replacements that need interactive choice before zone completion:

1. **Add state to `GameObject`** for the choice result (e.g., `chosen_basic_land_type: Option<BasicLandType>`)
2. **Add `WaitingFor` + `GameAction` variants** for the interactive round-trip (see `add-interactive-effect` skill)
3. **In the replacement pipeline**: when the interactive replacement is detected, store the pending `ProposedEvent` and return a waiting state *before* executing the zone change
4. **In `engine_replacement.rs`**: when the player responds, set the choice on the object, *then* execute the stored zone change, *then* process any additional post-replacement effects

This ensures layers never evaluate the permanent in an undefined state.

Interactive replacement tests must enter the waiting state from the real zone-change / cast / play path, then resume through the real `GameAction`. Assert the permanent never exists on the battlefield without the chosen characteristic or effect state; a manually constructed pending replacement is not enough for that invariant.

### Example: "As ~ enters, choose a basic land type"

Cards: Multiversal Passage, Convincing Mirage

The replacement pipeline detects the "choose" requirement → pauses for player input → player selects a land type → engine sets `chosen_basic_land_type` on the object → zone change executes → layers apply the continuous effect that sets the subtype.

The `ProposedEvent::ZoneChange` can carry additional data (or the choice can be stored on `GameState` transiently, like `post_replacement_effect`) to bridge the pause.

---

## Common Mistakes

| Mistake | Consequence | Fix |
|---------|-------------|-----|
| Missing `valid_card: Some(SelfRef)` | Replacement applies to ALL zone changes, not just self | Always set `valid_card` for self-replacements |
| Forgetting `applied` set check in matcher | Same replacement fires twice on cascading events | `proposed.applied` tracking prevents this automatically |
| Running interactive choice post-zone-change | Permanent on battlefield without chosen characteristic | Use pre-zone-change pattern (see above) |
| Not handling both accept and decline paths | Optional replacement silently no-ops on one path | Test both branches |
| Missing `#[serde(default)]` on new ProposedEvent fields | Deserialization breaks for existing card data | Always default new optional fields |
| Rescanning replacement candidates after player choice | Multiple matching replacements can apply the wrong execute/decline/rider/effect | Consume the stored `PendingReplacement` / `ReplacementId` identity selected by the player |
| Handler returns `Modified` but doesn't modify anything | Event processed as-is but marked as "replaced" | Either modify the event or return the original unchanged |
| Lowering an "instead" override as a second, independent ability | The engine executes **both** the original and the replacement — CR 614.6 says the replaced event never happens (~40 faces, #44/#79) | Lower to a BRANCH: `ClauseDisposition::ReplaceMeaning` → `else_ability`. Prove it with a runtime test asserting the overridden effect did NOT happen |

---

## Self-Maintenance

This skill stays current through use. After completing work using this skill:

1. **Verify references still exist** by running the check below
2. **Update if stale**: If a referenced function has moved or been renamed, update this skill
3. **Add new patterns**: If you discovered a new registration point or gotcha, add it

### Verification

```bash
# All referenced anchors should exist — if any grep fails, update the skill
rg -q "fn replace_event" crates/engine/src/game/replacement.rs && \
rg -q "fn continue_replacement" crates/engine/src/game/replacement.rs && \
rg -q "fn find_applicable_replacements" crates/engine/src/game/replacement.rs && \
rg -q "fn pipeline_loop" crates/engine/src/game/replacement.rs && \
rg -q "fn apply_post_replacement_effect" crates/engine/src/game/engine_replacement.rs && \
rg -q "fn handle_replacement_choice" crates/engine/src/game/engine_replacement.rs && \
rg -q "fn build_replacement_registry" crates/engine/src/game/replacement.rs && \
rg -q "struct ReplacementDefinition" crates/engine/src/types/ability.rs && \
rg -q "enum ReplacementMode" crates/engine/src/types/ability.rs && \
rg -q "post_replacement_effect" crates/engine/src/types/game_state.rs && \
rg -q "enum ProposedEvent" crates/engine/src/types/proposed_event.rs && \
rg -q "fn parse_shock_land" crates/engine/src/parser/oracle_replacement.rs && \
rg -q "enum ClauseDisposition" crates/engine/src/parser/oracle_ir/effect_chain.rs && \
rg -q "enum ReplaceMeaningKind" crates/engine/src/parser/oracle_ir/effect_chain.rs && \
rg -q "else_ability" crates/engine/src/types/ability.rs && \
echo "✓ add-replacement-effect skill references valid" || \
echo "✗ STALE — update skill references"
```
