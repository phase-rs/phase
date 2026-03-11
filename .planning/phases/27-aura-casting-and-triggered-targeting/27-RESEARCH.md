# Phase 27: Aura Casting, Triggered Ability Targeting, and "Until Leaves" Exile Return - Research

**Researched:** 2026-03-11
**Domain:** Rust game engine -- MTG rules: Aura targeting, triggered ability targets, exile-return tracking
**Confidence:** HIGH

## Summary

Phase 27 implements three engine features needed for cards like Sheltered by Ghosts, Banishing Light, Oblivion Ring, and Fiend Hunter. The codebase has been significantly refactored by Phase 28, which replaced all HashMap-based params with typed structs. The CONTEXT.md was written BEFORE Phase 28 and references obsolete Forge-style patterns (SVars, ValidTgts strings, Duration$ strings, params HashMap). This research documents the CURRENT typed model and identifies exactly what needs to change.

**Critical finding:** 15,930 trigger definitions in the data/abilities/ JSON files have NO `execute` field because the Forge SVar references were unresolvable during Phase 28's migration. Sheltered by Ghosts' trigger has no execute, so the ETB exile effect must be authored manually or the trigger definition must be completed.

**Primary recommendation:** All three features use the typed data model exclusively. No string-based params, SVars, or Forge filter syntax. The targeting system needs a new `find_legal_targets_typed()` function that accepts `TargetFilter` directly, replacing the string-based bridge. The exile-return feature uses `Duration` enum (already has `UntilHostLeavesPlay` variant).

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
1. **Aura Targeting During Casting** -- In `handle_cast_spell`, detect Aura subtype + Enchant keyword, extract TargetFilter, use existing targeting flow (WaitingFor::TargetSelection -> handle_select_targets -> pay_and_push). No new WaitingFor variants for Aura casting.
2. **Aura Attachment on Resolution** -- In `resolve_top`, after moving permanent to Battlefield, check Aura subtype + targets, call `effects::attach::attach_to()`. SBA `check_unattached_auras` provides safety net.
3. **General Filter Fallback in Targeting** -- Add fallback arm in `find_legal_targets` for unrecognized filter strings, delegating to `filter.rs`.
4. **`nonLand` Property in Filter** -- Add generic `non<Type>` handling in `matches_property` (before `_ => true` fallback).
5. **Triggered Ability Target Selection** -- Add `pending_trigger: Option<PendingTrigger>` to GameState, add `WaitingFor::TriggerTargetSelection` variant. In `process_triggers`, when trigger ability has targeting: 0 targets -> skip, 1 target -> auto-target, N targets -> store pending and return TriggerTargetSelection.
6. **"Until Leaves Battlefield" Exile Return** -- Add `exile_links: Vec<ExileLink>` to GameState with `ExileLink { exiled_id, source_id }`. Record links in change_zone when Duration is UntilHostLeavesPlay. Check exile returns after SBAs in apply loop.

### Claude's Discretion
- Exact placement of `check_exile_returns` in the apply loop (before or after SBAs)
- Whether `PendingTrigger` needs additional fields for serialization across WASM boundary
- Whether to add `CancelCast` handling for `TriggerTargetSelection` (triggers are mandatory in MTG, so probably no cancel)
- Test strategy: unit tests per component vs integration test for the full Sheltered by Ghosts flow

### Deferred Ideas (OUT OF SCOPE)
- Multiple triggers needing targeting simultaneously (current design handles one at a time via `pending_trigger`)
- Cancel/decline for triggered ability targeting (MTG rules make triggers mandatory)
- Exile return to a different zone (some cards return to hand instead of battlefield)
- Exile tracking UI (showing which cards are exiled by which source)
- Aura re-targeting (Aura swap effects like Aura Graft)
</user_constraints>

## CONTEXT.md Rewrite Requirements

The CONTEXT.md was written before Phase 28 and uses outdated patterns. Here is a mapping of what changed:

| CONTEXT.md Reference | Current Codebase (Post Phase 28) |
|---|---|
| `Keyword::Enchant(String)` with `"filter:display_text"` format | `Keyword::Enchant(TargetFilter)` -- already typed |
| `compat_params` / `ValidTgts` injection | No compat_params exist; use typed TargetFilter directly |
| `ability.params` containing `Duration$ "UntilHostLeavesPlay"` | `AbilityDefinition.duration: Option<Duration>` with `Duration::UntilHostLeavesPlay` enum variant |
| `PendingTrigger` at `triggers.rs:26` with pub visibility needed | `PendingTrigger` exists at line 27, already pub struct with typed fields |
| `filter::object_matches_filter_controlled` | `filter::matches_target_filter_controlled(state, obj_id, &filter, source_id, controller)` |
| SVars / `TrigExile` SVar reference in card data | Trigger `execute` field is None for 15,930 cards (including Sheltered by Ghosts) |
| `get_valid_tgts_string` bridge function | Still exists in casting.rs and stack.rs; can be replaced with typed targeting |

## Standard Stack

### Core
| Module | Location | Purpose | Why Standard |
|--------|----------|---------|--------------|
| ability.rs types | `crates/engine/src/types/ability.rs` | TargetFilter, Duration, Effect, AbilityDefinition, ResolvedAbility | Phase 28 typed data model -- all new code MUST use these |
| filter.rs | `crates/engine/src/game/filter.rs` | Typed TargetFilter matching against GameObjects | Already handles And/Or/Not/Typed/SelfRef/NonType |
| targeting.rs | `crates/engine/src/game/targeting.rs` | String-based `find_legal_targets` + protection/hexproof/shroud checks | Needs typed extension, `can_target()` checks reused |
| triggers.rs | `crates/engine/src/game/triggers.rs` | Trigger matching, `PendingTrigger`, `build_triggered_ability` | Process_triggers needs target check addition |
| effects/attach.rs | `crates/engine/src/game/effects/attach.rs` | `attach_to(state, attachment_id, target_id)` | Reuse directly for Aura attachment on resolution |
| effects/change_zone.rs | `crates/engine/src/game/effects/change_zone.rs` | `resolve()` with replacement effects for zone changes | Add ExileLink recording here |
| scenario.rs | `crates/engine/src/game/scenario.rs` | GameScenario/CardBuilder/GameRunner test harness | Use for all new tests |

## Architecture Patterns

### Current Data Flow (Post Phase 28)

```
CardDatabase (JSON) -> CardFace
  -> abilities: Vec<AbilityDefinition>     (typed Effect + cost + sub_ability chain)
  -> trigger_definitions: Vec<TriggerDefinition>  (typed mode + execute + filters)
  -> keywords: Vec<Keyword>                (typed -- Enchant(TargetFilter))

GameObject mirrors these fields:
  -> abilities: Vec<AbilityDefinition>
  -> trigger_definitions: Vec<TriggerDefinition>
  -> keywords: Vec<Keyword>

Casting flow:
  handle_cast_spell -> has_targeting_requirement(&ability_def) -> get_valid_tgts_string -> find_legal_targets (string)
  -> WaitingFor::TargetSelection -> handle_select_targets -> pay_and_push -> stack

Resolution flow:
  resolve_top -> execute_effect -> effects::resolve_ability_chain -> individual effect handlers

Trigger flow:
  process_triggers -> build_trigger_registry -> matcher functions -> build_triggered_ability -> push_to_stack
```

### Pattern 1: Aura Casting with Typed TargetFilter

**What:** Extract `TargetFilter` from `Keyword::Enchant(filter)` on the game object and pass it to a typed targeting function.

**When to use:** In `handle_cast_spell` when the spell is an Aura (subtypes contains "Aura").

**Key insight:** `Keyword::Enchant` already stores a `TargetFilter` (not a string). Example from card data after Phase 28 migration:
```json
{
  "keywords": ["Enchant:{ \"type\": \"Typed\", \"card_type\": \"Creature\" }"]
}
```
At runtime after keyword parsing: `Keyword::Enchant(TargetFilter::Typed { card_type: Some(TypeFilter::Creature), ... })`

**Approach:**
```rust
// In handle_cast_spell, after getting ability_def:
let is_aura = obj.card_types.subtypes.iter().any(|s| s == "Aura");
if is_aura {
    // Extract enchant filter from keywords
    let enchant_filter = obj.keywords.iter().find_map(|k| {
        if let Keyword::Enchant(filter) = k { Some(filter.clone()) } else { None }
    });
    if let Some(filter) = enchant_filter {
        let legal = find_legal_targets_typed(state, &filter, player, object_id);
        // ... existing targeting flow (1 target -> auto, N targets -> WaitingFor::TargetSelection)
    }
}
```

### Pattern 2: Typed Target Finding

**What:** New function `find_legal_targets_typed` that accepts `TargetFilter` directly instead of a string.

**Why:** The string-based `find_legal_targets` is a hardcoded match on known filter strings with a dead `_ => {}` fallback. The typed `filter.rs::matches_target_filter_controlled` already handles the full TargetFilter enum. Combining them eliminates the string bridge.

**Approach:**
```rust
pub fn find_legal_targets_typed(
    state: &GameState,
    filter: &TargetFilter,
    source_controller: PlayerId,
    source_id: ObjectId,
) -> Vec<TargetRef> {
    let mut targets = Vec::new();

    // Check if filter could match players
    if matches!(filter, TargetFilter::Any | TargetFilter::Player) {
        add_players(state, &mut targets);
    }

    // Check battlefield objects
    for &obj_id in &state.battlefield {
        if filter::matches_target_filter_controlled(state, obj_id, filter, source_id, source_controller) {
            if can_target(state.objects.get(&obj_id).unwrap(), source_controller, source_id, state) {
                targets.push(TargetRef::Object(obj_id));
            }
        }
    }

    // Check stack for Card/Any filters (counterspells)
    if matches!(filter, TargetFilter::Any | TargetFilter::Typed { card_type: Some(TypeFilter::Card), .. }) {
        add_stack_spells(state, &mut targets);
    }

    targets
}
```

### Pattern 3: Triggered Ability Target Selection

**What:** New `WaitingFor::TriggerTargetSelection` variant + `pending_trigger` field on GameState.

**Why:** Currently `process_triggers` pushes triggered abilities directly to the stack with empty targets. Triggers that need targeting (like Sheltered by Ghosts' exile) must pause for player input.

**Key types involved:**
```rust
// Already exists in triggers.rs (line 27):
pub struct PendingTrigger {
    pub source_id: ObjectId,
    pub controller: PlayerId,
    pub trigger_def: TriggerDefinition,
    pub ability: ResolvedAbility,
    pub timestamp: u32,
}

// New on GameState:
pub pending_trigger: Option<PendingTrigger>,

// New WaitingFor variant:
TriggerTargetSelection {
    player: PlayerId,
    legal_targets: Vec<TargetRef>,
}
```

**WASM boundary impact:** `PendingTrigger` needs `Serialize`/`Deserialize` (already has `Clone`). `TriggerDefinition` already has serde. The WaitingFor variant serializes via `#[serde(tag = "type", content = "data")]`. Frontend needs the new variant added to `WaitingFor` TypeScript type in `adapter/types.ts`.

### Pattern 4: Exile Return Tracking

**What:** `exile_links: Vec<ExileLink>` on GameState, checked after SBAs in the apply loop.

**Key types:**
```rust
pub struct ExileLink {
    pub exiled_id: ObjectId,
    pub source_id: ObjectId,
}
```

**Where to record:** In `effects/change_zone.rs::resolve()`, after the zone change to Exile succeeds, check if the ability's duration is `UntilHostLeavesPlay`. Currently `ResolvedAbility` has no `duration` field -- this needs to be added (or checked from the source trigger's `AbilityDefinition`).

**Important detail:** `ResolvedAbility` currently has: `effect, targets, source_id, controller, sub_ability`. It does NOT have a `duration` field. The `AbilityDefinition` has `duration: Option<Duration>`. When building `ResolvedAbility` from `AbilityDefinition` (in `build_resolved_from_def`), the duration is lost. Options:
1. Add `duration: Option<Duration>` to `ResolvedAbility` (clean, matches AbilityDefinition)
2. Look up the source object's trigger definitions (fragile, object may have left battlefield)

**Recommendation:** Add `duration` field to `ResolvedAbility`. This is a small change and keeps the data self-contained. The `build_resolved_from_def` functions in both casting.rs and triggers.rs would populate it.

**Where to check returns:** After SBAs in the apply loop (engine.rs lines 422-453). Scan events for `GameEvent::ZoneChanged { from: Battlefield, .. }` where `object_id` matches an `ExileLink.source_id`. Move `exiled_id` back to Battlefield.

### Pattern 5: Sheltered by Ghosts Data Gap

**What:** The card's trigger JSON has NO `execute` field:
```json
{
  "triggers": [{
    "mode": "ChangesZone",
    "destination": "Battlefield",
    "origin": "Battlefield",
    "valid_card": { "type": "SelfRef" }
  }]
}
```

This is because the Forge SVar `TrigExile` couldn't be resolved during Phase 28 migration (15,930 triggers have this issue).

**Resolution:** The trigger's `execute` field must be populated. For Sheltered by Ghosts, the correct execute ability is:
```json
{
  "execute": {
    "kind": "Spell",
    "effect": {
      "type": "ChangeZone",
      "origin": "Battlefield",
      "destination": "Exile",
      "target": {
        "type": "Typed",
        "card_type": "Permanent",
        "controller": "Opponent",
        "properties": [{ "type": "NonType", "value": "Land" }]
      }
    },
    "duration": "UntilHostLeavesPlay",
    "target_prompt": "Select target nonland permanent an opponent controls"
  }
}
```

**Scope:** Updating individual card data files for specific test cards (Sheltered by Ghosts, Banishing Light, Oblivion Ring) is in scope. Mass fixing of all 15,930 triggers is NOT in scope.

### Anti-Patterns to Avoid

- **Don't use string-based filter bridges for new code:** The `get_valid_tgts_string` and `extract_target_filter_string` functions in casting.rs and stack.rs are legacy bridges. New targeting code should use `TargetFilter` directly.
- **Don't add HashMap params to any struct:** Phase 28 specifically removed all HashMap-based params. All new fields must be typed.
- **Don't parse SVars:** The SVar system no longer exists at runtime. All ability data is pre-resolved typed structs.
- **Don't modify filter.rs for "nonLand":** Filter.rs already handles `FilterProp::NonType { value: "Land" }` via `matches_filter_prop`. The CONTEXT.md's decision #4 about adding `non<Type>` to filter.rs is already implemented.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| TargetFilter matching | Custom match arms per filter type | `filter::matches_target_filter_controlled()` | Already handles all TargetFilter variants including And/Or/Not/Typed/SelfRef with NonType, EnchantedBy, etc. |
| Aura attachment | Custom attach logic | `effects::attach::attach_to(state, aura_id, target_id)` | Handles detach from old target, sets attached_to, adds to attachments list, marks layers_dirty |
| Protection/Hexproof/Shroud targeting checks | New targeting validation | `can_target()` in targeting.rs | Already handles all protection variants, shroud, hexproof |
| APNAP trigger ordering | Manual sort | Existing sort in `process_triggers` (lines 138-149) | Correct per MTG 603.3b |
| Zone movement with replacement effects | Direct state mutation | `effects/change_zone.rs::resolve()` with `replacement::replace_event()` | Handles indestructible, protection from exile, etc. |

## Common Pitfalls

### Pitfall 1: PendingTrigger Serialization
**What goes wrong:** PendingTrigger has `TriggerDefinition` which contains `Option<Box<AbilityDefinition>>`. If any field lacks Serialize/Deserialize, the GameState won't cross the WASM boundary.
**Why it happens:** PendingTrigger currently derives `Debug, Clone` but NOT Serialize/Deserialize.
**How to avoid:** Add `#[derive(Serialize, Deserialize)]` to PendingTrigger. Also add it to the struct definition. Verify with `cargo test -p engine-wasm`.
**Warning signs:** Compilation errors in engine-wasm crate.

### Pitfall 2: Exile Return Timing
**What goes wrong:** If exile returns happen AFTER triggers process, the returned permanents won't get their own ETB triggers until next action.
**Why it happens:** The apply loop order matters: SBAs -> exile returns -> triggers.
**How to avoid:** Place `check_exile_returns` between SBAs and `process_triggers` in the apply loop (engine.rs line ~434). This way returned permanents' ETB events are included in the trigger scan.
**Warning signs:** Cards returned from exile don't trigger their own ETB abilities.

### Pitfall 3: Aura Self-Targeting
**What goes wrong:** The enchant filter might match the Aura itself (it's on the stack, not battlefield, so usually safe -- but edge cases exist).
**Why it happens:** `find_legal_targets` iterates `state.battlefield`. During casting, the Aura is in the hand/stack, not on the battlefield.
**How to avoid:** The Aura is in the hand zone when targeting happens (before pay_and_push moves it to stack). Battlefield iteration naturally excludes it. No special handling needed.

### Pitfall 4: Missing Execute on Trigger Definitions
**What goes wrong:** `build_triggered_ability` returns `Effect::Unimplemented { name: "TriggerNoExecute" }` for triggers without `execute` field. The ability goes on the stack but does nothing when resolved.
**Why it happens:** 15,930 triggers lost their SVar execute references during Phase 28 migration.
**How to avoid:** For the specific cards being tested (Sheltered by Ghosts, Banishing Light), manually update the JSON ability files with the correct `execute` field. For the triggered target selection feature, the `execute` field's target filter determines what targets are legal.
**Warning signs:** Trigger goes on stack, resolves, but nothing happens.

### Pitfall 5: Forgetting to Update Frontend Types
**What goes wrong:** Adding `WaitingFor::TriggerTargetSelection` on Rust side but not updating `client/src/adapter/types.ts` causes the frontend to not recognize the state.
**Why it happens:** WaitingFor is serialized as JSON discriminated union across WASM boundary.
**How to avoid:** Add the variant to the TypeScript `WaitingFor` type. The targeting UI (TargetingOverlay.tsx) likely needs no changes -- it already handles TargetSelection; TriggerTargetSelection can reuse the same UI.

### Pitfall 6: GameState PartialEq Must Include New Fields
**What goes wrong:** If `pending_trigger` and `exile_links` are added to GameState but not to the manual `PartialEq` impl, tests comparing states will silently ignore these fields.
**Why it happens:** GameState has a manual `PartialEq` impl (game_state.rs lines 236-260) that skips `rng`.
**How to avoid:** Add the new fields to the `PartialEq::eq` method.

## Code Examples

### Detecting Aura in handle_cast_spell
```rust
// Source: crates/engine/src/game/casting.rs -- handle_cast_spell
// After line 66 (ability_def assignment), before timing validation:
let is_aura = obj.card_types.subtypes.iter().any(|s| s == "Aura");
let enchant_filter = if is_aura {
    obj.keywords.iter().find_map(|k| {
        if let Keyword::Enchant(filter) = k {
            Some(filter.clone())
        } else {
            None
        }
    })
} else {
    None
};
```

### ExileLink Structure
```rust
// Source: crates/engine/src/types/game_state.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExileLink {
    pub exiled_id: ObjectId,
    pub source_id: ObjectId,
}
```

### Checking Exile Returns
```rust
// Source: new function in crates/engine/src/game/engine.rs (or separate module)
fn check_exile_returns(state: &mut GameState, events: &[GameEvent], new_events: &mut Vec<GameEvent>) {
    let mut to_return = Vec::new();
    for event in events {
        if let GameEvent::ZoneChanged { object_id, from: Zone::Battlefield, .. } = event {
            // Find any exile links where this object was the source
            let links: Vec<_> = state.exile_links.iter()
                .filter(|link| link.source_id == *object_id)
                .cloned()
                .collect();
            to_return.extend(links);
        }
    }
    for link in &to_return {
        zones::move_to_zone(state, link.exiled_id, Zone::Battlefield, new_events);
    }
    state.exile_links.retain(|link| !to_return.iter().any(|r| r.exiled_id == link.exiled_id));
}
```

### TriggerTargetSelection WaitingFor
```rust
// Source: crates/engine/src/types/game_state.rs -- add to WaitingFor enum
TriggerTargetSelection {
    player: PlayerId,
    legal_targets: Vec<TargetRef>,
},
```

### Frontend Type Addition
```typescript
// Source: client/src/adapter/types.ts -- add to WaitingFor union
| { type: "TriggerTargetSelection"; data: { player: PlayerId; legal_targets: TargetRef[] } }
```

## State of the Art

| Old Approach (Pre-Phase 28) | Current Approach (Post-Phase 28) | When Changed | Impact on Phase 27 |
|---|---|---|---|
| `HashMap<String, String>` params on abilities | Typed `Effect` enum variants with struct fields | Phase 28 (2026-03-11) | All Phase 27 code MUST use typed fields |
| `String` SVars referenced by name | `AbilityDefinition.sub_ability: Option<Box<AbilityDefinition>>` | Phase 28 | No SVar lookup; ability chains are pre-resolved |
| `ValidTgts$ "Creature.YouCtrl"` string | `TargetFilter::Typed { card_type: Some(Creature), controller: Some(You), .. }` | Phase 28 | Use TargetFilter directly |
| `Duration$ "UntilHostLeavesPlay"` string | `Duration::UntilHostLeavesPlay` enum | Phase 28 | Check `ability.duration` typed field |
| `Keyword::Enchant(String)` with `"filter:text"` | `Keyword::Enchant(TargetFilter)` typed | Phase 28 | Extract TargetFilter directly from keyword |
| `filter::object_matches_filter_controlled` string API | `filter::matches_target_filter_controlled` typed API | Phase 28 | Use typed API for all new filter matching |
| `ResolvedAbility { params, svars, .. }` with HashMaps | `ResolvedAbility { effect, targets, source_id, controller, sub_ability }` zero HashMap | Phase 28 | No params/svars fields exist |

## Open Questions

1. **ResolvedAbility.duration field**
   - What we know: AbilityDefinition has `duration: Option<Duration>` but ResolvedAbility does not. The exile-return feature needs to know the duration at resolution time.
   - What's unclear: Whether adding a field to ResolvedAbility has any serialization/deserialization issues across the WASM boundary.
   - Recommendation: Add `#[serde(default)] pub duration: Option<Duration>` to ResolvedAbility. Update `build_resolved_from_def` in triggers.rs and casting.rs to propagate duration. Verify engine-wasm compiles.

2. **Trigger execute field completion for target cards**
   - What we know: Sheltered by Ghosts, Banishing Light, Oblivion Ring, Fiend Hunter all have triggers with NO execute field.
   - What's unclear: Whether updating individual JSON files is sufficient or if a broader fix is needed.
   - Recommendation: Manually author execute fields for 3-4 test cards. This is minimal scope and sufficient for Phase 27.

3. **How targeting interacts with the trigger execute effect**
   - What we know: The trigger's execute `AbilityDefinition` has an `effect` field with a `target` TargetFilter. This filter defines what the triggered ability can target.
   - What's unclear: Whether the targeting should come from the trigger's `valid_target` field or from the execute's effect target field.
   - Recommendation: Use the execute's effect `target` field (e.g., `Effect::ChangeZone { target: Typed { Permanent, OppCtrl, NonType(Land) } }`). The trigger's `valid_target` is for filtering what triggers the ability, not what it targets.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in test + inline #[cfg(test)] modules |
| Config file | N/A (Cargo test) |
| Quick run command | `cargo test -p engine -- --nocapture` |
| Full suite command | `cargo test --all` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| P27-01 | Aura spells prompt for enchant target during casting | integration | `cargo test -p engine aura_cast_targets` | Wave 0 |
| P27-02 | Aura attaches to target on resolution | integration | `cargo test -p engine aura_attach_on_resolve` | Wave 0 |
| P27-03 | Triggered abilities with targets prompt for selection | integration | `cargo test -p engine trigger_target_selection` | Wave 0 |
| P27-04 | Auto-target for single-target triggers | unit | `cargo test -p engine trigger_auto_target` | Wave 0 |
| P27-05 | Cards exiled with UntilHostLeavesPlay return when source leaves | integration | `cargo test -p engine exile_return_on_leave` | Wave 0 |
| P27-06 | find_legal_targets_typed handles Typed filters with properties | unit | `cargo test -p engine typed_targeting` | Wave 0 |
| P27-07 | TriggerTargetSelection in legal_actions | unit | `cargo test -p phase-ai trigger_target_legal` | Wave 0 |
| P27-08 | GameState serialization with new fields | unit | `cargo test -p engine game_state_new_fields` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test -p engine`
- **Per wave merge:** `cargo test --all`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] Tests for Aura casting targeting (P27-01, P27-02) -- new scenario tests needed in casting.rs or rules/
- [ ] Tests for triggered ability targeting (P27-03, P27-04) -- new scenario tests in triggers.rs
- [ ] Tests for exile return (P27-05) -- new scenario test or integration test
- [ ] Tests for typed targeting (P27-06) -- new tests in targeting.rs
- [ ] Tests for legal actions with TriggerTargetSelection (P27-07) -- new test in legal_actions.rs
- [ ] Updated JSON data for Sheltered by Ghosts with execute field -- manual authoring needed
- [ ] Framework install: N/A -- Rust test framework always available

## Sources

### Primary (HIGH confidence)
- `crates/engine/src/types/ability.rs` -- All typed definitions inspected: Effect, TargetFilter, Duration, AbilityDefinition, TriggerDefinition, ResolvedAbility
- `crates/engine/src/types/game_state.rs` -- GameState, WaitingFor enum, PendingCast, StackEntry inspected
- `crates/engine/src/game/casting.rs` -- handle_cast_spell flow, has_targeting_requirement, get_valid_tgts_string inspected
- `crates/engine/src/game/targeting.rs` -- find_legal_targets, can_target, validate_targets inspected
- `crates/engine/src/game/triggers.rs` -- process_triggers, PendingTrigger, build_triggered_ability, target_filter_matches_object inspected
- `crates/engine/src/game/stack.rs` -- resolve_top, extract_target_filter_string inspected
- `crates/engine/src/game/filter.rs` -- matches_target_filter, matches_target_filter_controlled, matches_filter_prop inspected
- `crates/engine/src/game/effects/attach.rs` -- attach_to function inspected
- `crates/engine/src/game/effects/change_zone.rs` -- resolve, resolve_all inspected
- `crates/engine/src/game/engine.rs` -- apply function, SBA/trigger processing loop inspected
- `crates/engine/src/types/keywords.rs` -- Keyword::Enchant(TargetFilter) confirmed
- `crates/phase-ai/src/legal_actions.rs` -- get_legal_actions, target_selection_actions inspected
- `client/src/adapter/types.ts` -- WaitingFor TypeScript type inspected
- `data/abilities/sheltered_by_ghosts.json` -- Current card data (trigger has no execute)
- `data/abilities/banishing_light.json` -- Same pattern confirmed
- `.planning/phases/28-native-ability-data-model/28-04-SUMMARY.md` -- Confirmed 15,930 triggers missing execute

### Secondary (MEDIUM confidence)
- Phase 28 plan summaries (28-01 through 28-06) -- confirmed typed model is fully in place

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- All files inspected directly, types verified
- Architecture: HIGH -- Patterns traced through actual code paths
- Pitfalls: HIGH -- Identified from structural analysis of codebase state
- Data model gap (missing execute): HIGH -- Confirmed by inspecting card JSON files and migration summary

**Research date:** 2026-03-11
**Valid until:** 2026-04-11 (stable -- engine architecture unlikely to change without planned phase)
