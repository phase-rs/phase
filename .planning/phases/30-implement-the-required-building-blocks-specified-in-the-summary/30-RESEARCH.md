# Phase 30: Implement the Required Building Blocks - Research

**Researched:** 2026-03-16
**Domain:** Rust MTG engine — trigger context resolution, Oracle parser, Adventure casting, game restrictions
**Confidence:** HIGH

## Summary

This phase delivers four composable building blocks identified from the Bonecrusher Giant gap analysis. Each block extends existing engine infrastructure with well-defined integration points. The codebase has strong patterns for all four areas: `TargetFilter` flat variants (like `AttachedTo`, `LastCreated`) for event-context targets, `oracle_target.rs` possessive parsing for new phrase patterns, the `SpellCastingOption`/`SpellCastingOptionKind` system for Adventure casting, and the replacement pipeline for damage prevention gating.

The highest-risk item is the Adventure casting subsystem (Building Block #3) because it spans engine, frontend, and AI with a new `CastingPermission` concept and zone-aware casting. The other three blocks are relatively contained engine-only changes. The wave ordering in CONTEXT.md (types first, then pipeline wiring, then Adventure) correctly sequences dependencies.

**Primary recommendation:** Implement in wave order. Blocks #1, #2, and #4's type definitions are independent and parallelizable. Adventure is the largest block and should be tackled last with full-stack attention.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- **Event-Context Target Resolution:** Specific flat `TargetFilter` variants (`TriggeringSpellController`, `TriggeringSource`, `TriggeringPlayer`) — not nested `EventContext(EventRef)`. `trigger_event: Option<GameEvent>` field on `StackEntryKind::TriggeredAbility`.
- **Parser Possessive References:** New patterns in `oracle_target.rs` for "that spell's controller" -> `TriggeringSpellController`, "that spell's owner" -> `TriggeringSpellOwner`, "that player" -> `TriggeringPlayer`.
- **Adventure Casting:** `CastAdventure` variant in `SpellCastingOptionKind`. Exile-on-resolve modeled as replacement effect. `CastingPermission` enum as `Vec<CastingPermission>` on `GameObject` with `AdventureCreature` variant. Frontend + AI included.
- **Damage Prevention Disabling:** `GameRestriction` enum on `GameState` as `Vec<GameRestriction>` with `DamagePreventionDisabled` variant. `RestrictionScope` + `RestrictionExpiry` enums. Cleanup at CR 514.2.
- **Wave Ordering:** Wave 1 (parallel types), Wave 2 (pipeline wiring), Wave 3 (Adventure full-stack).
- **Testing:** ~3-5 GameScenario tests per block + one Bonecrusher Giant integration test.

### Claude's Discretion
- Exact function signatures and module placement for new resolvers
- Internal structuring of Adventure zone-change hooks
- Effect handler registration details
- Frontend component design and layout for Adventure cards
- AI evaluation heuristics for face selection

### Deferred Ideas (OUT OF SCOPE)
- Bonecrusher Giant card integration (subsequent phase)
- Full Adventure card suite testing (Brazen Borrower, Murderous Rider)
- Additional GameRestriction variants (LifeGainDisabled, CantBeCountered)
- Additional CastingPermission variants (Foretell, ImpulseDraw)
- MDFC face-casting
</user_constraints>

## Architecture Patterns

### Building Block #1: Event-Context Target Resolution

**What:** Thread trigger event data through the stack so effect resolvers can extract context-derived targets (e.g., "that spell's controller").

**Current state (verified):**
- `StackEntryKind::TriggeredAbility` has `source_id` and `ability` but no event context field
- `GameEvent::BecomesTarget { object_id, source_id }` exists and is emitted during casting
- `TargetFilter` has flat variants like `AttachedTo`, `LastCreated`, `TrackedSet` as precedent
- `match_becomes_target()` in `triggers.rs:1485` fires correctly but has no way to pass event data through

**Integration points:**

1. **`types/ability.rs` (TargetFilter enum, ~line 625):** Add three flat variants:
   ```rust
   /// Resolves to the controller of the spell/ability that triggered this.
   /// CR 603.7c: event-context derived target.
   TriggeringSpellController,
   /// Resolves to the owner of the spell/ability that triggered this.
   TriggeringSpellOwner,
   /// Resolves to the player involved in the triggering event.
   TriggeringPlayer,
   /// Resolves to the source object of the triggering event.
   TriggeringSource,
   ```

2. **`types/game_state.rs` (StackEntryKind::TriggeredAbility, ~line 300):** Add field:
   ```rust
   TriggeredAbility {
       source_id: ObjectId,
       ability: ResolvedAbility,
       condition: Option<TriggerCondition>,
       #[serde(default, skip_serializing_if = "Option::is_none")]
       trigger_event: Option<GameEvent>,  // NEW
   },
   ```

3. **`game/triggers.rs` (process_triggers / collect_matching_triggers):** When building the `StackEntry` for a matched trigger, capture the matched `GameEvent` and store it in `trigger_event`.

4. **`game/effects/` (target resolution path):** When resolving an effect with `TriggeringSpellController` target, read `trigger_event` from the stack context. The resolver needs access to the triggering event — this means either:
   - Store `trigger_event` on `GameState` transiently (like `pending_continuation`), set before resolution, cleared after
   - Or pass it through `resolve_ability_chain` — but this changes the signature

   **Recommendation:** Store as `state.current_trigger_event: Option<GameEvent>` set in `stack.rs::resolve_top()` before calling `execute_effect()`, cleared after. This is the minimal-change approach consistent with how `pending_continuation` works.

5. **`game/targeting.rs` (find_legal_targets / resolve target refs):** Add resolution for the new `TargetFilter` variants. For `TriggeringSpellController`: look up `trigger_event`, extract `source_id`, find its controller in `state.objects`. These are non-targeting (no player selection needed) — they auto-resolve like `SelfRef` and `Controller`.

**CR Reference:** CR 603.7c — A triggered ability can use information about the event that triggered it.

### Building Block #2: Parser Event-Context Possessive References

**What:** Parse "that spell's controller", "that spell's owner", "that player" into the new `TargetFilter` variants.

**Current state (verified):**
- `oracle_target.rs` handles `parse_target()` for "target X" phrases
- `contains_possessive()` in `oracle_util.rs` handles "your"/"their"/"its owner's"
- No pattern exists for "that spell's" or "that player" event-context references

**Integration points:**

1. **`parser/oracle_target.rs`:** Add detection for event-context possessives. These appear in trigger effect text (after the comma in "Whenever ~ becomes the target of a spell, ~ deals 2 damage to **that spell's controller**"):
   - `"that spell's controller"` -> `TargetFilter::TriggeringSpellController`
   - `"that spell's owner"` -> `TargetFilter::TriggeringSpellOwner`
   - `"that player"` -> `TargetFilter::TriggeringPlayer`

2. **`parser/oracle_effect.rs`:** The effect text "~ deals 2 damage to that spell's controller" needs to produce `Effect::DealDamage { target: TriggeringSpellController, ... }`. This likely requires a new helper or extension to the subject-stripping intercept pattern in `lower_imperative_clause()`.

**Pattern:** These are NOT targeting phrases (no "target" keyword) — they are context references that auto-resolve. They should NOT go through `parse_target()`. Instead, add a `parse_event_context_ref()` helper that runs before standard possessive/targeting parsing.

### Building Block #3: Adventure Casting Subsystem

**What:** Full CR 715 Adventure mechanic — cast Adventure half, exile on resolution, cast creature from exile.

**Current state (verified):**
- `CardLayout::Adventure(CardFace, CardFace)` exists in `types/card.rs:63`
- `SpellCastingOption` + `SpellCastingOptionKind` enum exists with `AlternativeCost`, `CastWithoutManaCost`, `AsThoughHadFlash`
- `casting_options: Vec<SpellCastingOption>` exists on `GameObject`
- `BackFaceData` exists for DFCs with all ability fields
- `stack.rs::resolve_top()` moves non-permanent spells to graveyard (line 82) — Adventure hook goes here
- No `CastingPermission` concept exists yet

**Integration points — Engine:**

1. **`types/ability.rs` (SpellCastingOptionKind):** Add `CastAdventure` variant.

2. **`game/game_object.rs` (GameObject):** Add:
   ```rust
   #[serde(default, skip_serializing_if = "Vec::is_empty")]
   pub casting_permissions: Vec<CastingPermission>,
   ```

3. **New type `CastingPermission`** (in `types/ability.rs` or own file):
   ```rust
   #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
   #[serde(tag = "type")]
   pub enum CastingPermission {
       /// CR 715.5: After Adventure resolves to exile, creature face castable from exile.
       AdventureCreature,
       // Future: Foretell, ImpulseDraw, etc.
   }
   ```

4. **`game/casting.rs`:** Major changes:
   - When player casts Adventure card from hand, present choice: creature face vs Adventure half
   - New `WaitingFor::AdventureCastChoice` or integrate into existing modal system
   - When `CastAdventure` selected: use Adventure face's characteristics (name, mana cost, abilities, card types) but keep the object's identity
   - Check `casting_permissions` to allow casting from exile (for the creature face after Adventure resolves)

5. **`game/stack.rs::resolve_top()`:** After Adventure spell resolves (line 76-84), instead of moving to graveyard:
   - Check if the card is an Adventure spell (track this on the stack entry or check card layout)
   - If yes: move to exile + add `CastingPermission::AdventureCreature` to the object
   - **Alternative (from CONTEXT.md decision):** Model as replacement effect — "If an Adventure spell would go to graveyard on resolution, exile it instead." This fits the replacement pipeline naturally.

6. **`game/restrictions.rs`:** Add casting permission checks — when a player tries to cast from exile, verify `CastingPermission::AdventureCreature` exists on the object.

**Integration points — Frontend:**

7. **New `WaitingFor` variant** (or extend existing cast flow) for Adventure face choice
8. **Card rendering:** Adventure cards show both faces. The Adventure half name/text appears on the card.
9. **Exile zone UI:** Show "Cast creature" button on Adventure cards in exile with `AdventureCreature` permission

**Integration points — AI:**

10. **`ai_support/candidates.rs`:** Generate `CastAdventure` as a legal action when Adventure cards in hand
11. **`phase-ai/src/eval.rs`:** Basic evaluation — compare creature face vs Adventure spell value based on board state

**CR References:**
- CR 715.3a: A player casting an Adventure card chooses whether to cast the creature or the Adventure
- CR 715.4: Adventure spell resolves -> exile (not graveyard); countered/leaves stack otherwise -> graveyard
- CR 715.5: While in exile with Adventure permission, may cast as creature

### Building Block #4: Damage Prevention Disabling (GameRestriction)

**What:** General `GameRestriction` system that gates the replacement pipeline, starting with `DamagePreventionDisabled`.

**Current state (verified):**
- `oracle_replacement.rs:67-77` detects "damage can't be prevented" but produces a no-op `ReplacementDefinition`
- `replacement.rs` has `ReplacementResult::Prevented` but no mechanism to suppress prevention
- `GameState` has no restrictions field
- Cleanup step in `turns.rs::execute_cleanup()` prunes end-of-turn effects at `prune_end_of_turn_effects()`

**Integration points:**

1. **New types** (in `types/ability.rs` or `types/game_state.rs`):
   ```rust
   #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
   #[serde(tag = "type")]
   pub enum GameRestriction {
       DamagePreventionDisabled {
           source: ObjectId,
           expiry: RestrictionExpiry,
           #[serde(default, skip_serializing_if = "Option::is_none")]
           scope: Option<RestrictionScope>,
       },
   }

   #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
   #[serde(tag = "type")]
   pub enum RestrictionExpiry {
       EndOfTurn,
       EndOfCombat,
   }

   #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
   #[serde(tag = "type")]
   pub enum RestrictionScope {
       SourcesControlledBy(PlayerId),
       SpecificSource(ObjectId),
       DamageToTarget(ObjectId),
   }
   ```

2. **`types/game_state.rs` (GameState):** Add field:
   ```rust
   #[serde(default, skip_serializing_if = "Vec::is_empty")]
   pub restrictions: Vec<GameRestriction>,
   ```

3. **`game/replacement.rs` (pipeline_loop or replace_event):** Before applying prevention-type replacements, check `state.restrictions` for `DamagePreventionDisabled`. If matched (scope allows), skip the prevention replacement.

   **Specific integration:** In `find_applicable_replacements()` or in `pipeline_loop()`, when a candidate is a damage prevention replacement, check if any `GameRestriction::DamagePreventionDisabled` applies. If yes, exclude that candidate from the list.

4. **`game/turns.rs` (execute_cleanup):** Add cleanup for expired restrictions:
   ```rust
   // CR 514.2: Remove end-of-turn restrictions
   state.restrictions.retain(|r| match r {
       GameRestriction::DamagePreventionDisabled { expiry, .. } => {
           !matches!(expiry, RestrictionExpiry::EndOfTurn)
       }
   });
   ```

5. **New effect type or use existing:** The effect "Damage can't be prevented this turn" needs to ADD a `GameRestriction` to `GameState`. Options:
   - New `Effect::AddRestriction { restriction: GameRestriction }` variant
   - Or handle inline as part of the replacement parser detection

   **Recommendation:** New `Effect::AddRestriction` is more composable — it can be used from effects, triggers, and replacement definitions.

6. **`parser/oracle_replacement.rs` (lines 73-77):** Change from producing a hollow `ReplacementDefinition` to producing an `Effect::AddRestriction` (or equivalent). Note: "damage can't be prevented this turn" is NOT itself a replacement effect — it's a restriction on the game. The parser should route it as an effect, not a replacement. This means updating `is_replacement_pattern()` in `oracle.rs` to NOT match this text, and instead letting it fall through to effect parsing.

   **Alternative:** Keep it as a "replacement definition" but with an `execute` that adds the restriction. This keeps the parser routing unchanged.

### Recommended Project Structure (changes)

```
crates/engine/src/
├── types/
│   ├── ability.rs          # +TargetFilter variants, CastingPermission, GameRestriction types
│   └── game_state.rs       # +restrictions field, +current_trigger_event
├── game/
│   ├── triggers.rs         # +trigger_event threading in process_triggers
│   ├── stack.rs            # +trigger_event setting, +Adventure exile-on-resolve
│   ├── casting.rs          # +Adventure cast choice, +exile casting permission check
│   ├── replacement.rs      # +restriction gating in pipeline
│   ├── turns.rs            # +restriction cleanup at CR 514.2
│   └── effects/
│       ├── mod.rs           # +AddRestriction dispatch
│       └── add_restriction.rs  # NEW: restriction effect handler
├── parser/
│   ├── oracle_target.rs    # +event-context possessive patterns
│   ├── oracle_effect.rs    # +event-context target routing
│   └── oracle_replacement.rs  # Wire detection to AddRestriction effect
client/src/
├── components/
│   ├── modal/
│   │   └── AdventureCastModal.tsx  # NEW: creature vs adventure choice
│   └── zone/
│       └── ExileZone.tsx           # +cast-from-exile button
```

### Anti-Patterns to Avoid

- **Nested `EventContext(EventRef)` wrapper on TargetFilter:** CONTEXT.md explicitly locks flat variants. Don't nest.
- **Hardcoding Bonecrusher Giant behavior:** Every block must be general-purpose. The integration test uses Bonecrusher, but the blocks work for any card in the class.
- **Making Adventure exile-on-resolve a special case in stack.rs:** Model as replacement effect per CONTEXT.md decision, so it interacts correctly with Rest in Peace and similar.
- **Boolean `damage_prevention_disabled` on GameState:** Use the `Vec<GameRestriction>` system — it's composable for scope, expiry, and future restriction types.
- **Routing "damage can't be prevented" through the targeting/effect system as a target filter:** It's a restriction on the game, not a target filter.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Trigger event threading | Custom event passing mechanism | `trigger_event` field on StackEntryKind + transient `current_trigger_event` on GameState | Follows existing patterns (pending_continuation, post_replacement_effect) |
| Adventure face choice UI | Custom from-scratch modal | Extend existing ModeChoiceModal or TargetSelection pattern | Consistent UX, less code |
| Restriction expiry cleanup | Manual cleanup scattered across modules | Single `prune_expired_restrictions()` call in `execute_cleanup()` alongside `prune_end_of_turn_effects()` | Centralized, no missed cleanup paths |
| CastingPermission zone checks | Inline checks in casting.rs | Dedicated `check_casting_permissions()` function | Reusable for Foretell, Impulse draw later |
| Event-context target resolution | Pass event through entire call chain | Transient state field pattern (set before resolve, clear after) | Matches existing patterns, minimal signature changes |

## Common Pitfalls

### Pitfall 1: trigger_event Not Threaded to Stack Entry
**What goes wrong:** Trigger fires correctly, but by the time the ability resolves, the triggering event is gone — `TriggeringSpellController` resolves to nothing.
**Why it happens:** `process_triggers` creates `StackEntry` without capturing the matched event.
**How to avoid:** Ensure `collect_matching_triggers` returns the matched `GameEvent` alongside the trigger data, and `StackEntryKind::TriggeredAbility` stores it.
**Warning signs:** Tests pass for trigger firing but fail for event-context target resolution.

### Pitfall 2: Adventure Spell Goes to Graveyard on Countered
**What goes wrong:** Countered Adventure spell goes to exile instead of graveyard.
**Why it happens:** The replacement effect applies to ALL zone changes from stack, not just resolution.
**How to avoid:** The replacement effect condition must check that the spell is RESOLVING (not being countered). CR 715.4 is explicit: only on resolution. Model the check as: "if this Adventure spell would be put into graveyard from the stack as it resolves" — not just any stack-to-graveyard transition.
**Warning signs:** Test where Adventure spell is countered shows it in exile instead of graveyard.

### Pitfall 3: Missing `#[serde(default)]` on New Fields
**What goes wrong:** Existing serialized `GameState` or `StackEntry` data fails to deserialize.
**Why it happens:** New fields without defaults break backward compatibility.
**How to avoid:** Every new field on `GameState`, `StackEntryKind`, `GameObject` gets `#[serde(default)]` and `skip_serializing_if`.
**Warning signs:** Deserialization errors in WASM or multiplayer.

### Pitfall 4: Prevention Gating Applied Too Broadly
**What goes wrong:** `DamagePreventionDisabled` suppresses ALL replacements, not just prevention replacements.
**Why it happens:** Checking restrictions at the wrong level in the replacement pipeline.
**How to avoid:** Only gate replacements that are specifically damage prevention (look for `ReplacementEvent::DamageDone` + handler that returns `Prevented`). The `ReplacementHandlerEntry` needs to be identifiable as a prevention effect.
**Warning signs:** Non-prevention replacement effects (like "enters tapped") stop working when damage prevention is disabled.

### Pitfall 5: Adventure Cast From Exile Doesn't Remove Permission
**What goes wrong:** Player can cast the creature from exile multiple times.
**Why it happens:** `CastingPermission::AdventureCreature` not removed after casting from exile.
**How to avoid:** When the creature is cast from exile, remove the permission (or the object leaves exile entirely via normal zone change).
**Warning signs:** Infinite Adventure creature casting loop in testing.

### Pitfall 6: Event-Context Targets Treated as Targeting
**What goes wrong:** `TriggeringSpellController` goes through the targeting phase, presenting player choice.
**Why it happens:** `extract_target_filter_from_effect()` returns these variants as needing targets.
**How to avoid:** Exclude `TriggeringSpellController`, `TriggeringSpellOwner`, `TriggeringPlayer`, `TriggeringSource` from `extract_target_filter_from_effect()` — they are auto-resolved like `SelfRef` and `Controller`.
**Warning signs:** Game pauses for target selection when a BecomesTarget trigger fires.

## Code Examples

### Event-Context Target Resolution (in effect resolver)
```rust
// In the target resolution path (targeting.rs or effects/deal_damage.rs)
// Source: Architectural pattern from CONTEXT.md + codebase analysis
fn resolve_target_ref(
    state: &GameState,
    target: &TargetFilter,
) -> Option<TargetRef> {
    match target {
        TargetFilter::TriggeringSpellController => {
            // Read from transient state field set by stack.rs before resolution
            if let Some(GameEvent::BecomesTarget { source_id, .. }) = &state.current_trigger_event {
                state.objects.get(source_id).map(|obj| TargetRef::Player(obj.controller))
            } else {
                None
            }
        }
        // ... other variants
    }
}
```

### GameRestriction Check in Replacement Pipeline
```rust
// In replacement.rs, within find_applicable_replacements or pipeline_loop
// Source: Architectural pattern from CONTEXT.md
fn is_prevention_disabled(state: &GameState, event: &ProposedEvent) -> bool {
    state.restrictions.iter().any(|r| match r {
        GameRestriction::DamagePreventionDisabled { scope, .. } => {
            match scope {
                None => true, // Global — all prevention disabled
                Some(RestrictionScope::SpecificSource(id)) => {
                    matches!(event, ProposedEvent::Damage { source, .. } if source == id)
                }
                // ... other scopes
            }
        }
    })
}
```

### Adventure Replacement Effect Condition
```rust
// In stack.rs or replacement handler for Adventure exile-on-resolve
// Source: CR 715.4 + CONTEXT.md decision
// The replacement applies ONLY when an Adventure spell resolves — not when countered
fn is_adventure_resolving(state: &GameState, entry: &StackEntry) -> bool {
    if let StackEntryKind::Spell { card_id, .. } = &entry.kind {
        state.objects.get(&entry.id)
            .and_then(|obj| obj.printed_ref.as_ref())
            .map(|pr| matches!(pr.layout, Some(CardLayout::Adventure(_, _))))
            .unwrap_or(false)
        // Additional check: was the spell cast as the Adventure half?
        // Need a flag on the stack entry or object to distinguish
    } else {
        false
    }
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| No event context on triggers | Flat TargetFilter variants + trigger_event field | This phase | Enables all event-context-derived targeting |
| No casting permissions | `Vec<CastingPermission>` on GameObject | This phase | Enables Adventure, Foretell, Impulse draw |
| No game restrictions system | `Vec<GameRestriction>` on GameState | This phase | Enables prevention disabling, "can't gain life", "can't be countered" |
| Parser no-ops "can't be prevented" | Effect-based restriction addition | This phase | Parser coverage improvement for prevention cards |

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in `#[test]` + GameScenario harness |
| Config file | `Cargo.toml` (workspace test configuration) |
| Quick run command | `cargo test -p engine -- <test_name>` |
| Full suite command | `cargo test --all` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| BB-01 | TriggeringSpellController resolves to spell caster | unit | `cargo test -p engine -- trigger_event_context -x` | Wave 0 |
| BB-01 | BecomesTarget trigger + event-context damage | integration | `cargo test -p engine -- becomes_target_context -x` | Wave 0 |
| BB-02 | Parser: "that spell's controller" -> TriggeringSpellController | unit | `cargo test -p engine -- parse_that_spells_controller -x` | Wave 0 |
| BB-02 | Parser: "that player" -> TriggeringPlayer | unit | `cargo test -p engine -- parse_that_player -x` | Wave 0 |
| BB-03 | Adventure cast choice (creature vs Adventure) | integration | `cargo test -p engine -- adventure_cast_choice -x` | Wave 0 |
| BB-03 | Adventure spell resolves -> exile with permission | integration | `cargo test -p engine -- adventure_exile_on_resolve -x` | Wave 0 |
| BB-03 | Adventure spell countered -> graveyard | integration | `cargo test -p engine -- adventure_countered_to_graveyard -x` | Wave 0 |
| BB-03 | Cast creature from exile after Adventure | integration | `cargo test -p engine -- adventure_cast_from_exile -x` | Wave 0 |
| BB-04 | DamagePreventionDisabled blocks prevention | unit | `cargo test -p engine -- prevention_disabled -x` | Wave 0 |
| BB-04 | GameRestriction cleanup at end of turn | unit | `cargo test -p engine -- restriction_cleanup -x` | Wave 0 |
| BB-04 | Parser: "damage can't be prevented" -> AddRestriction | unit | `cargo test -p engine -- parse_damage_cant_be_prevented -x` | Wave 0 |
| BB-ALL | Bonecrusher Giant end-to-end | integration | `cargo test -p engine -- bonecrusher_giant -x` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test -p engine`
- **Per wave merge:** `cargo test --all && cargo clippy --all-targets -- -D warnings`
- **Phase gate:** Full suite green + clippy clean before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] All test files listed above — new tests needed for all four building blocks
- [ ] No framework install needed — Rust test infrastructure is in place
- [ ] GameScenario harness already exists for integration tests

## Open Questions

1. **Adventure face tracking on stack**
   - What we know: When an Adventure card is cast as the Adventure half, the stack entry uses the Adventure face's characteristics. When it resolves, we need to know it was cast as Adventure (not creature).
   - What's unclear: Where to store this flag — on the `StackEntry` as a new field, or on the `GameObject` as transient state?
   - Recommendation: Add `cast_as_adventure: bool` to `StackEntryKind::Spell` (or a more general `CastFace` enum for future MDFC support). This is cleaner than mutating the object.

2. **Prevention replacement identification**
   - What we know: The restriction gating needs to identify which replacements are "prevention" effects to skip them.
   - What's unclear: How to distinguish prevention replacements from other damage replacements (e.g., "if ~ would deal damage, it deals double instead" is NOT prevention).
   - Recommendation: Add a `is_prevention: bool` field to `ReplacementHandlerEntry` or use the `ReplacementEvent` + `ApplyResult::Prevented` pattern to identify them.

3. **Adventure card loading from MTGJSON**
   - What we know: `CardLayout::Adventure(CardFace, CardFace)` exists and MTGJSON data loads both faces.
   - What's unclear: How Adventure face abilities are currently loaded into `GameObject` — is only the creature face loaded, or are both faces accessible?
   - Recommendation: Investigate `database/oracle_loader.rs` or `json_loader.rs` during implementation.

## Sources

### Primary (HIGH confidence)
- Codebase inspection of all referenced files in CONTEXT.md canonical_refs section
- `types/ability.rs` — TargetFilter enum (line 625), SpellCastingOptionKind (line 898)
- `types/game_state.rs` — GameState struct (line 309), StackEntryKind (line 291)
- `game/replacement.rs` — ReplacementResult pipeline, find_applicable_replacements
- `game/stack.rs` — resolve_top() resolution flow
- `game/triggers.rs` — match_becomes_target() at line 1485
- `game/game_object.rs` — GameObject struct (line 72)
- `parser/oracle_replacement.rs` — "can't be prevented" detection (lines 73-77)
- `types/card.rs` — CardLayout::Adventure (line 63)
- Project skills: add-engine-effect, add-replacement-effect, casting-stack-conditions, extend-oracle-parser, add-trigger

### Secondary (MEDIUM confidence)
- MTG Comprehensive Rules CR 603.7c, CR 614.16, CR 715 (Adventure), CR 514.2 (from CONTEXT.md citations — verified against pattern in codebase)

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH — all integration points verified in source code
- Architecture: HIGH — patterns established by existing codebase (AttachedTo, LastCreated, replacement pipeline, casting options)
- Pitfalls: HIGH — derived from actual code flow analysis and CR rules
- Adventure subsystem: MEDIUM — largest scope, frontend/AI aspects less explored
- Parser changes: HIGH — oracle_target.rs and oracle_effect.rs patterns well-understood from skills

**Research date:** 2026-03-16
**Valid until:** 2026-04-16 (stable engine internals, no external dependencies)
