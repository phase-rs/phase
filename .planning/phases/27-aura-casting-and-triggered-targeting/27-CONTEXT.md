# Phase 27: Aura Casting, Triggered Ability Targeting, and "Until Leaves" Exile Return -- Context

**Gathered:** 2026-03-10
**Updated:** 2026-03-11 (rewritten to reflect typed data model implementation)
**Status:** Implementation complete

<domain>
## Phase Boundary

Implement full Aura spell support (targeting + attachment), triggered ability target selection, and "until source leaves the battlefield" exile return tracking. The motivating card is "Sheltered by Ghosts" but all changes are general engine features that enable many cards.

Three engine gaps are addressed:
1. **Aura casting** -- Auras currently enter the battlefield unattached (no enchant target selection during casting, no attachment on resolution)
2. **Triggered ability targeting** -- Triggered abilities with targeting requirements go on the stack with empty targets; players cannot select targets
3. **"Until leaves" exile return** -- No tracking for `Duration::UntilHostLeavesPlay`; exiled cards never return when the source leaves

</domain>

<decisions>
## Implementation Decisions

### 1. Aura Targeting During Casting (`crates/engine/src/game/casting.rs`)

- In `handle_cast_spell`, when `obj.card_types.subtypes` contains `"Aura"`:
  - Extract the typed target filter from `Keyword::Enchant(TargetFilter)` directly
  - Call `find_legal_targets_typed(state, &filter, player, object_id)` to get legal targets
- The Aura targeting logic is placed before the existing `has_targeting_requirement` check because Auras target via the Enchant keyword, not via the Effect target field
- Single legal target auto-targets, multiple targets prompt `WaitingFor::TargetSelection`, no legal targets prevents casting
- `find_legal_targets_typed` delegates to `filter::matches_target_filter_controlled` for battlefield object matching

### 2. Aura Attachment on Resolution (`crates/engine/src/game/stack.rs`)

- In `resolve_top`, after moving a permanent to the Battlefield:
  - Check if the object has `"Aura"` subtype AND the resolved ability has targets
  - Extract the first `TargetRef::Object(target_id)` from the cloned `spell_targets`
  - Call `effects::attach::attach_to(state, object_id, target_id)`
- This is the correct MTG timing -- Aura enters battlefield already attached to its target
- Existing SBA (`check_unattached_auras`) provides the safety net: if target is gone, Aura dies
- `spell_targets` are cloned before the fizzle check path (which moves ability) to preserve targets for attachment

### 3. Typed Target Filter in Targeting (`crates/engine/src/game/targeting.rs`)

- `find_legal_targets_typed(state, filter, player, source_id)` handles all `TargetFilter` variants:
  - `TargetFilter::Typed { card_type, controller, properties }` -- delegates to `filter::matches_target_filter_controlled` for battlefield objects
  - `TargetFilter::Any` -- all battlefield permanents are legal targets
  - `TargetFilter::Player` -- returns player refs
  - `TargetFilter::Card { zone }` -- targets cards in specific zones
- No string parsing or filter syntax involved -- works entirely with typed enum variants
- This replaces any need for a general string-based filter fallback

### 4. NonType Property in Filter (`crates/engine/src/game/filter.rs`)

- `TargetFilter::Typed { conditions }` uses `TargetCondition::NonType(CoreType::Land)` enum variant for nonLand filtering
- Handled by `matches_target_filter_controlled` in filter.rs which checks each condition against the object
- Also supports `NonType(CoreType::Creature)`, `NonType(CoreType::Artifact)`, etc. via the same enum variant
- No string parsing or `starts_with("non")` hack -- fully typed

### 5. Triggered Ability Target Selection (`crates/engine/src/game/triggers.rs` + `engine.rs`)

- `extract_target_filter_from_effect(effect: &Effect)` inspects the trigger's typed `Effect` for targeting requirements
  - Returns `Option<TargetFilter>` -- None means no targeting needed
  - Excludes `SelfRef`, `Controller`, `None` as non-targeting targets (no player choice required)
- In `process_triggers`, when a trigger's effect has a targeting requirement:
  - Call `find_legal_targets_typed` to get legal targets
  - 0 targets: skip trigger (can't go on stack without legal targets)
  - 1 target: auto-target, push to stack as normal (no player prompt)
  - N targets: store trigger in `state.pending_trigger`, return early from `process_triggers` (remaining triggers deferred until after target selection)
- In `engine.rs::apply`, after `process_triggers`, check `state.pending_trigger.is_some()`:
  - If so, compute legal targets and return `WaitingFor::TriggerTargetSelection { player, legal_targets }`
- Handler in `apply` match: `(WaitingFor::TriggerTargetSelection { .. }, GameAction::SelectTargets { targets })`:
  - Take `state.pending_trigger`, set targets on its ability, push to stack, return `WaitingFor::Priority`
- `PendingTrigger` carries full `TriggerDefinition` with `Serialize`/`Deserialize` for WASM boundary crossing

### 6. "Until Leaves Battlefield" Exile Return (`engine.rs` + `effects/change_zone.rs`)

- `Duration::UntilHostLeavesPlay` enum variant on `ResolvedAbility.duration` field
- `ExileLink { exiled_id: ObjectId, source_id: ObjectId }` tracked on `GameState.exile_links`
- In `effects/change_zone.rs::resolve`, when `ability.duration == Some(Duration::UntilHostLeavesPlay)`:
  - After moving the target to exile, record `ExileLink { exiled_id: target, source_id: ability.source_id }` on `state.exile_links`
- `check_exile_returns(state, events, new_events)` in `engine.rs`:
  - Scans events for `ZoneChanged { from: Battlefield, .. }` where the `object_id` matches a `source_id` in `exile_links`
  - For each match, moves the `exiled_id` back to Battlefield via `zones::move_to_zone`
  - Removes the processed links from `exile_links`
  - Gracefully handles cards already moved from exile (no panic, no-op)
- Called in `engine.rs::apply` after SBA checks and before `process_triggers`:
  - This ensures returned permanents generate their own ETB triggers

### Claude's Discretion

- Test strategy: unit tests per component vs integration test for the full Sheltered by Ghosts flow
- Whether `PendingTrigger` needs additional fields for serialization across WASM boundary

</decisions>

<specifics>
## Specific Ideas

- `Keyword::Enchant(TargetFilter)` stores the typed filter directly on the enum variant -- no string splitting or parsing needed
- `TargetCondition::NonType(CoreType)` handles nonLand, nonCreature, nonArtifact at the type level (very common filter patterns across many cards)
- `find_legal_targets_typed` eliminates the need for hardcoded match arms for every new filter pattern -- all TargetFilter variants dispatched through a single function
- "Until leaves" exile is used by approximately 50+ cards (Banishing Light, Fiend Hunter, Skyclave Apparition, etc.) -- this is a high-value engine feature
- `extract_target_filter_from_effect` is reusable for any effect targeting analysis beyond triggered abilities

</specifics>

<code_context>
## Existing Code Insights

### Files Modified

1. **`crates/engine/src/game/casting.rs`** -- `handle_cast_spell`: detect Aura via subtype, extract `Keyword::Enchant(TargetFilter)`, call `find_legal_targets_typed`
2. **`crates/engine/src/game/stack.rs`** -- `resolve_top`: attach Aura to target after entering battlefield via `effects::attach::attach_to`
3. **`crates/engine/src/game/targeting.rs`** -- `find_legal_targets_typed()`: typed TargetFilter matching using `filter::matches_target_filter_controlled`
4. **`crates/engine/src/game/filter.rs`** -- `matches_target_filter_controlled`: handles `TargetFilter::Typed` with `TargetCondition::NonType(CoreType)` conditions
5. **`crates/engine/src/game/triggers.rs`** -- `process_triggers`: `extract_target_filter_from_effect` detects targeting, auto-target/multi-target/skip logic
6. **`crates/engine/src/types/game_state.rs`** -- `ExileLink` struct, `pending_trigger`, `exile_links` fields, `WaitingFor::TriggerTargetSelection` variant
7. **`crates/engine/src/game/engine.rs`** -- `TriggerTargetSelection` handler, `check_exile_returns` in apply loop
8. **`crates/engine/src/game/effects/change_zone.rs`** -- Record `ExileLink` when `Duration::UntilHostLeavesPlay`
9. **`crates/phase-ai/src/legal_actions.rs`** -- Handle `TriggerTargetSelection` in `get_legal_actions`
10. **`client/src/adapter/types.ts`** -- `TriggerTargetSelection` in `WaitingFor` union
11. **`client/src/components/targeting/TargetingOverlay.tsx`** -- Handles both `TargetSelection` and `TriggerTargetSelection` states
12. **`client/src/pages/GamePage.tsx`** -- Renders `TargetingOverlay` for `TriggerTargetSelection`

### Established Patterns

- `WaitingFor::EquipTarget` -- closest pattern for target selection outside of casting
- `WaitingFor::TargetSelection` with `PendingCast` -- the casting target selection flow reused for Auras
- `PendingReplacement` on `GameState` -- pattern for storing pending state that needs player input
- `check_unattached_auras` in `sba.rs` -- safety net for Auras that lose their target
- `effects::attach::attach_to` -- the attachment function called from stack.rs
- `filter::matches_target_filter_controlled` -- typed filter matching for battlefield objects
- Prowess synthetic trigger in `triggers.rs` -- pattern for engine-generated triggers

### Key Type References

- `Keyword::Enchant(TargetFilter)` -- stores typed target filter directly
- `TargetRef::Object(ObjectId)` / `TargetRef::Player(PlayerId)` -- target union
- `PendingTrigger` -- public with Serialize/Deserialize for WASM boundary
- `ResolvedAbility.targets: Vec<TargetRef>` -- where selected targets are stored
- `ResolvedAbility.duration: Option<Duration>` -- carries duration from AbilityDefinition
- `StackEntryKind::TriggeredAbility { source_id, ability }` -- triggered ability on stack
- `ExileLink { exiled_id, source_id }` -- exile-return tracking
- `Duration::UntilHostLeavesPlay` -- enum variant for exile-until-leaves effects
- `TargetFilter::Typed { card_type, controller, properties }` -- typed filter with conditions
- `TargetCondition::NonType(CoreType)` -- negated type condition (nonLand, nonCreature, etc.)

### Card Data (Sheltered by Ghosts -- typed format)

```json
{
  "triggers": [{
    "mode": "ChangesZone",
    "destination": "Battlefield",
    "origin": "Battlefield",
    "valid_card": { "type": "SelfRef" },
    "description": "When CARDNAME enters, exile target nonland permanent...",
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
  }]
}
```

</code_context>

<deferred>
## Deferred Ideas

- Multiple triggers needing targeting simultaneously (current design handles one at a time via `pending_trigger`)
- Cancel/decline for triggered ability targeting (MTG rules make triggers mandatory -- defer)
- Exile return to a different zone (some cards return to hand instead of battlefield)
- Exile tracking UI (showing which cards are exiled by which source)
- Aura re-targeting (Aura swap effects like Aura Graft)

</deferred>

---

*Phase: 27-aura-casting-and-triggered-targeting*
*Context gathered: 2026-03-10*
*Updated: 2026-03-11 (typed data model)*
