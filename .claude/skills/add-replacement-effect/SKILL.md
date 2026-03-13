---
name: add-replacement-effect
description: Use when adding or modifying replacement effects — ETB-tapped, shock lands, damage prevention, "as enters" choices, or any event-modifying ability. Covers ReplacementDefinition wiring, the pipeline flow, post-replacement effects, and interactive pre-zone-change choices.
---

# Adding a Replacement Effect

Replacement effects modify or prevent game events before they happen (MTG Rule 614.1). They are **not** triggered abilities — they don't use the stack. This skill covers wiring a new replacement through the pipeline: definition → parser → registry → handler → engine.

**Before you start:** Trace how shock lands work end-to-end. They're the most complete example: `parse_shock_land()` in `oracle_replacement.rs` → `ReplacementDefinition` with `Optional` mode → replacement pipeline → `post_replacement_effect` → `apply_post_replacement_effect` in `engine.rs`.

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

### `post_replacement_effect` Lifecycle

Stored on `GameState` as `Option<Box<AbilityDefinition>>`. Set by `continue_replacement()`, consumed in `engine.rs` after the zone change completes. Currently handles `LoseLife` and `Tap` in `apply_post_replacement_effect()`.

**Important**: This runs *after* the zone change. For effects that must happen *before* the zone change (like "choose a basic land type"), see Interactive Replacements below.

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

- [ ] **`crates/engine/src/game/engine.rs` — `apply_post_replacement_effect()`**
  If your replacement produces a post-zone-change side effect using a new `Effect` variant, add a match arm in this function. Currently handles `LoseLife` and `Tap`.

### Phase 5 — Tests

- [ ] Parser test: Oracle text → correct `ReplacementDefinition`
- [ ] Pipeline test: proposed event → replacement applies → modified event
- [ ] Engine flow test: full game action → replacement → zone change → post-effect
- [ ] `cargo test -p engine && cargo clippy --all-targets -- -D warnings`

---

## Interactive Replacements (Pre-Zone-Change Choices)

**MTG Rule 614.16**: "As [permanent] enters the battlefield, choose..." is a replacement effect. The choice modifies the entering event itself — the permanent enters with the choice already made.

This is architecturally harder than standard replacements because it requires player input *during* the replacement pipeline, *before* the zone change completes.

### The Timing Invariant

**The permanent must never exist on the battlefield without its chosen characteristic set.** If the choice happens post-zone-change, there's a window where layers evaluate the permanent without the choice, which can cause incorrect ETB trigger behavior.

### Implementation Pattern

For replacements that need interactive choice before zone completion:

1. **Add state to `GameObject`** for the choice result (e.g., `chosen_basic_land_type: Option<BasicLandType>`)
2. **Add `WaitingFor` + `GameAction` variants** for the interactive round-trip (see `add-interactive-effect` skill)
3. **In the replacement pipeline**: when the interactive replacement is detected, store the pending `ProposedEvent` and return a waiting state *before* executing the zone change
4. **In `engine.rs`**: when the player responds, set the choice on the object, *then* execute the stored zone change, *then* process any additional post-replacement effects

This ensures layers never evaluate the permanent in an undefined state.

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
| Handler returns `Modified` but doesn't modify anything | Event processed as-is but marked as "replaced" | Either modify the event or return the original unchanged |

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
rg -q "fn apply_post_replacement_effect" crates/engine/src/game/engine.rs && \
rg -q "fn build_replacement_registry" crates/engine/src/game/replacement.rs && \
rg -q "struct ReplacementDefinition" crates/engine/src/types/ability.rs && \
rg -q "enum ReplacementMode" crates/engine/src/types/ability.rs && \
rg -q "post_replacement_effect" crates/engine/src/types/game_state.rs && \
rg -q "enum ProposedEvent" crates/engine/src/types/proposed_event.rs && \
rg -q "fn parse_shock_land" crates/engine/src/parser/oracle_replacement.rs && \
echo "✓ add-replacement-effect skill references valid" || \
echo "✗ STALE — update skill references"
```
