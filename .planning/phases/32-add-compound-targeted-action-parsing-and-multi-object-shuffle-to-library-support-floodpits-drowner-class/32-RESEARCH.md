# Phase 32: Compound Targeted-Action Parsing & Multi-Object Shuffle-to-Library - Research

**Researched:** 2026-03-17
**Domain:** Oracle text parser extension, effect composition, target filter system
**Confidence:** HIGH

## Summary

This phase adds four composable building blocks to the engine: (1) a generic compound effect splitter for "tap X and put a counter on it" patterns, (2) a compound subject splitter for "shuffle this and target X into libraries" patterns, (3) counter-based target filtering ("creature with a stun counter on it"), and (4) multi-object shuffle-to-library via chained ChangeZone effects with auto-shuffle per CR 401.3.

All four building blocks have well-defined integration points in the existing codebase. The parser already has a compound splitting precedent (`try_split_pump_compound` in oracle_effect.rs), the filter system has a `FilterProp` enum with established suffix parsing in `parse_type_phrase`, and the ChangeZone handler already routes through the replacement pipeline. The `CounterType` enum and `counters` HashMap on `GameObject` are already in place.

**Primary recommendation:** Build the four building blocks in dependency order: counter filter first (no deps), then ParentTarget (no deps), then compound effect splitter (uses ParentTarget), then compound subject splitter + ChangeZone auto-shuffle (uses counter filter). Validate each with parser unit tests, then run full pipeline regen + coverage audit.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- **Generic Compound Effect Splitter:** `try_split_compound(primary: ParsedEffectClause, remainder: &str) -> ParsedEffectClause` at ParsedEffectClause level. Handles "and"/"then" connectors. Single split, no recursion. Call-site gating: only runs when caller knows it has a single-target effect with leftover text. Unparseable remainder emits Unimplemented sub_ability.
- **TargetFilter::ParentTarget:** New variant for anaphoric "it"/"that creature"/"that player" references. Resolution copies resolved ObjectId/PlayerId from parent ability's target list. Handles both object and player targets.
- **Generic Compound Subject Splitter:** `try_split_compound_subject(text: &str) -> Option<(Subject, Subject, &str)>` verb-generic helper. Returns two subjects + remainder. Callers chain effects of the same type for each subject.
- **FilterProp::HasCounter { counter_type: CounterType, minimum: u32 }:** Uses typed CounterType enum (not String). General `parse_counter_filter()` parses "with a/an [type] counter(s) on it". Composed into parse_target() pipeline as property suffix. Runtime matching in filter_matches_object reads obj.counters HashMap.
- **ChangeZone per object decomposition:** Self via SelfRef, target via normal resolution. Parser uses try_split_compound_subject.
- **owner_library: true on ChangeZone:** Each object goes to owner's library per CR 400.7.
- **Auto-shuffle per CR 401.3:** ChangeZone-to-Library auto-shuffles owner's library (unless specific_position set). No explicit Shuffle sub_ability needed.
- **No shuffle dedup:** Per CR 401.3, shuffling happens per object moved. Same-owner double shuffle is CR-correct.
- **No shuffle if replacement redirects:** Per CR 614.6.
- **Pre-loop SelfRef guard:** ChangeZone handler checks for SelfRef before normal target loop.
- **Full coverage audit:** cargo coverage before/after.
- **Full Floodpits Drowner integration test:** GameScenario with Flash, ETB compound trigger, shuffle ability.
- **Pipeline regeneration:** gen-card-data.sh + cargo coverage as final step.

### Claude's Discretion
- Exact function signatures and module placement for try_split_compound and try_split_compound_subject
- Internal connector detection heuristics (distinguishing "and" between effects vs "and" in keyword lists)
- ChangeZone specific_position flag design for "put on top/bottom" opt-out
- Parser integration point for counter filter within parse_target() flow
- Test fixture design for compound-action sample cards

### Deferred Ideas (OUT OF SCOPE)
- HasCountersMin patterns ("with three or more +1/+1 counters") -- HasCounter has the min field, parser support deferred
- Compound subjects with 3+ objects ("exile X, Y, and Z") -- binary split only
- "then" connector semantics distinction from "and" -- both produce sub_ability chains
- Compound subject for non-shuffle verbs (exile, return, bounce) -- helper ready, callers added when needed
- Counter filter with "no counters"/"without" negation
</user_constraints>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| engine crate | workspace | Parser + effect system + filter system | All changes are internal to the engine |
| serde/serde_json | workspace | Serialization for new FilterProp/TargetFilter variants | Existing derive pattern |
| tsify | workspace | TypeScript type generation for new variants | Existing WASM bridge pattern |

No new external dependencies required. All work extends existing engine internals.

## Architecture Patterns

### Recommended Change Structure
```
crates/engine/src/
├── types/ability.rs           # FilterProp::HasCounter, TargetFilter::ParentTarget, ChangeZone owner_library field
├── parser/oracle_target.rs    # parse_counter_filter() suffix in parse_type_phrase pipeline
├── parser/oracle_effect.rs    # try_split_compound(), try_split_compound_subject(), parse_shuffle_ast extension
├── game/filter.rs             # HasCounter runtime matching in filter_matches_object()
├── game/effects/change_zone.rs # SelfRef pre-loop guard, owner_library routing, auto-shuffle per CR 401.3
├── game/effects/shuffle.rs    # (may be simplified if auto-shuffle covers common cases)
└── game/targeting.rs          # ParentTarget resolution (copies parent's resolved targets)
```

### Pattern 1: Compound Effect Splitting (follows try_split_pump_compound)

**What:** After `parse_targeted_action_ast` parses a primary effect and `parse_target` returns a remainder, `try_split_compound` takes the remainder and produces a sub_ability chain.

**When to use:** When `parse_targeted_action_ast` consumes "tap target creature an opponent controls" but the remainder " and put a stun counter on it" is discarded (currently the `_` in `let (target, _) = parse_target(...)`).

**Integration point:** The `_remainder` from `parse_target()` calls inside `parse_targeted_action_ast()` is where compound text lives. Currently discarded. The compound splitter needs the remainder to be captured and processed.

**Existing precedent:**
```rust
// oracle_effect.rs:3071 - existing compound splitter for pump effects
fn try_split_pump_compound(
    normalized: &str,
    application: &SubjectApplication,
) -> Option<ParsedEffectClause> {
    let and_pos = lower.find(" and ")?;
    let pump_part = &normalized[..and_pos];
    let remainder = normalized[and_pos + " and ".len()..].trim();
    // Parse remainder as independent effect chain (sub_ability)
    let sub_ability = Some(Box::new(parse_effect_chain(remainder, AbilityKind::Spell)));
    // ...
}
```

**New pattern follows same shape:** split at "and"/"then", parse remainder as effect chain, attach as sub_ability.

### Pattern 2: Counter Filter Suffix (follows parse_power_suffix / parse_mana_value_suffix)

**What:** A new `parse_counter_suffix()` function in `oracle_target.rs` that matches "with a/an [type] counter(s) on it" and returns `(FilterProp::HasCounter { counter_type, minimum: 1 }, bytes_consumed)`.

**Integration point:** `parse_type_phrase()` already has a series of suffix parsers at lines ~260-270:
```rust
// Check "with mana value N or less/greater" suffix
if let Some((prop, consumed)) = parse_mana_value_suffix(&lower[pos..]) { ... }
// Check "with power N or less/greater" suffix
if let Some((prop, consumed)) = parse_power_suffix(&lower[pos..]) { ... }
// NEW: Check "with a/an [type] counter(s) on it" suffix
if let Some((prop, consumed)) = parse_counter_suffix(&lower[pos..]) { ... }
```

**Counter type parsing reuses existing `parse_counter_type()` from game_object.rs:**
```rust
pub fn parse_counter_type(text: &str) -> CounterType {
    match text.trim().trim_end_matches(" counter").trim() {
        "P1P1" | "+1/+1" | "plus1plus1" => CounterType::Plus1Plus1,
        "M1M1" | "-1/-1" | "minus1minus1" => CounterType::Minus1Minus1,
        "LOYALTY" | "loyalty" => CounterType::Loyalty,
        "stun" => CounterType::Stun,
        other => CounterType::Generic(other.to_string()),
    }
}
```

### Pattern 3: ParentTarget Resolution

**What:** `TargetFilter::ParentTarget` is a new variant that resolves to the same target(s) as the parent ability. At resolution time, the engine copies the parent's resolved TargetRef values into the child ability's targets.

**Integration point:** Target resolution in `targeting.rs` / `stack.rs`. When a sub_ability has `target: ParentTarget`, the resolver skips the targeting phase and inherits the parent's targets.

**Existing parallel:** `TargetFilter::LastCreated` already resolves to specific objects from context rather than player selection. ParentTarget follows the same pattern.

### Pattern 4: ChangeZone Auto-Shuffle for Library Destination

**What:** When `ChangeZone` moves an object to Library and `owner_library` is true (or whenever destination is Library without specific_position), the handler auto-shuffles the owner's library after the move.

**CR 401.3 reference:** "If an object is put into a library at a specific position or put on top or bottom of a library, that library doesn't get shuffled. Otherwise, if an object is put into a library 'from anywhere', that library is shuffled."

**Integration:** Extend `change_zone.rs::resolve()` to call shuffle after `zones::move_to_zone()` when destination is Library and no specific_position flag is set.

### Anti-Patterns to Avoid
- **Don't create a CompoundTap effect** -- compose Tap + PutCounter via sub_ability chain
- **Don't create a ShuffleMultiple effect** -- compose individual ChangeZone effects per object
- **Don't match verbatim Oracle text** -- use grammatical decomposition (strip_prefix, parse_target, parse_counter_filter)
- **Don't duplicate counter matching logic** -- reuse CounterType enum and parse_counter_type()

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Counter type detection | Custom string matching | `parse_counter_type()` in game_object.rs | Already handles +1/+1, stun, loyalty, etc. |
| Target remainder parsing | Manual string slicing | `parse_target()` return value | Already returns (filter, remainder) tuple |
| Effect chaining | Custom composition | `sub_ability` field + `parse_effect_chain()` | Standard engine composition pattern |
| Library shuffle after zone change | Explicit Shuffle sub_ability | Auto-shuffle in ChangeZone handler | CR 401.3 is universal, not card-specific |
| Self-reference detection | Manual "~" matching | `TargetFilter::SelfRef` from `parse_target()` | Already handles normalized self-references |

## Common Pitfalls

### Pitfall 1: "and" Ambiguity in Compound Splitting
**What goes wrong:** "and" appears in multiple Oracle text contexts: compound effects ("tap X and put a counter"), compound subjects ("this creature and target creature"), keyword lists ("flying and vigilance"), and type phrases ("creature or artifact").
**Why it happens:** Naive splitting on "and" catches all of these.
**How to avoid:** Call-site gating -- `try_split_compound` only runs when the caller KNOWS it has a single-target effect with leftover text (i.e., after parse_targeted_action_ast consumed the primary verb+target). The compound subject splitter runs specifically in the shuffle parser where it detects "X and Y" in subject position before the verb.
**Warning signs:** Keywords being split into separate effects; type filters being broken.

### Pitfall 2: ParentTarget vs SelfRef Confusion
**What goes wrong:** Using `SelfRef` for "put a stun counter on it" would put the counter on the source creature (Floodpits Drowner) instead of the targeted creature.
**Why it happens:** "it" in "tap target creature and put a counter on it" refers to the TARGET, not SELF.
**How to avoid:** ParentTarget specifically exists for this anaphoric reference. Parser must detect "it"/"that creature" after compound split and emit ParentTarget, not SelfRef.
**Warning signs:** Counter landing on the wrong creature in integration tests.

### Pitfall 3: Owner vs Controller for Library Destination
**What goes wrong:** Shuffling into controller's library instead of owner's library when they differ (e.g., stolen creature).
**Why it happens:** The default zone placement uses controller, but "owner's library" is explicit in Oracle text.
**How to avoid:** The `owner_library` flag on ChangeZone explicitly routes to `obj.owner`'s library zone. The zones system already tracks `obj.owner` on every GameObject.
**Warning signs:** Stolen creatures going to the wrong library.

### Pitfall 4: Auto-Shuffle Suppression for Specific Position
**What goes wrong:** "Put on top of library" triggers an unwanted shuffle.
**Why it happens:** Auto-shuffle fires for all Library destinations.
**How to avoid:** Add `specific_position: Option<LibraryPosition>` field (or check existing patterns) on ChangeZone. When set, suppress auto-shuffle.
**Warning signs:** Scry/put-on-top effects randomizing the positioned card.

### Pitfall 5: Replacement Redirect Suppressing Shuffle
**What goes wrong:** Object gets redirected to exile (e.g., Rest in Peace) but the library still gets shuffled.
**Why it happens:** Auto-shuffle fires before checking replacement result.
**How to avoid:** Only shuffle when `ReplacementResult::Execute` with destination still Library. The existing ChangeZone handler already branches on `ReplacementResult::Execute(event)` and checks `to == Zone::*` -- the shuffle must be inside that branch, after confirming destination is still Library.

### Pitfall 6: ChangeZone SelfRef Before Target Loop
**What goes wrong:** The source creature (Floodpits Drowner) can't be found by normal target resolution because it uses SelfRef instead of an explicit target.
**Why it happens:** SelfRef is resolved differently from targeted objects -- it's the ability's source, not a selected target.
**How to avoid:** Add SelfRef pre-loop handling in change_zone.rs: before iterating `ability.targets`, check if the effect has `self_included: true` or similar, and process `ability.source_id` through the same zone-change + replacement pipeline.

## Code Examples

### Counter Filter Suffix Parser
```rust
// oracle_target.rs - new suffix parser
fn parse_counter_suffix(text: &str) -> Option<(FilterProp, usize)> {
    let trimmed = text.trim_start();
    // "with a stun counter on it" / "with an oil counter on it"
    let rest = trimmed.strip_prefix("with a ")?
        .or_else(|| trimmed.strip_prefix("with an "))?;
    // Extract counter type before " counter"
    let counter_end = rest.find(" counter")?;
    let counter_name = &rest[..counter_end];
    let counter_type = parse_counter_type(counter_name);
    // Consume "counter on it" / "counter on them" / "counters on it"
    let after_counter = &rest[counter_end..];
    let consumed = /* calculate bytes consumed */;
    Some((FilterProp::HasCounter { counter_type, minimum: 1 }, consumed))
}
```

### FilterProp::HasCounter Runtime Matching
```rust
// filter.rs - new match arm in filter_matches_object()
FilterProp::HasCounter { counter_type, minimum } => {
    obj.counters.get(counter_type).copied().unwrap_or(0) >= *minimum
}
```

### ChangeZone Auto-Shuffle After Library Move
```rust
// change_zone.rs - inside the Execute branch
if let ProposedEvent::ZoneChange { object_id, to, .. } = event {
    zones::move_to_zone(state, object_id, to, events);
    // CR 401.3: Auto-shuffle library when object put into library (not at specific position)
    if to == Zone::Library && !has_specific_position {
        let owner = state.objects.get(&object_id).map(|o| o.owner);
        if let Some(owner) = owner {
            shuffle_player_library(state, owner);
        }
    }
    // ...existing battlefield/layers logic...
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|---|---|---|---|
| Discarded remainder in parse_targeted_action_ast | Capture remainder for compound splitting | Phase 32 | Unlocks ~20+ compound targeted action cards |
| CountersGE with String counter_type | HasCounter with typed CounterType enum | Phase 32 | Type-safe counter filtering across filter system |
| Explicit Shuffle sub_ability for "shuffle into library" | Auto-shuffle in ChangeZone handler per CR 401.3 | Phase 32 | Universal building block for all shuffle-to-library cards |
| No anaphoric target reference | ParentTarget for "it"/"that creature" in sub_abilities | Phase 32 | Enables compound effects that reference parent's target |

**Existing infrastructure leveraged:**
- `CounterType` enum with Stun, Plus1Plus1, Loyalty, Generic variants
- `obj.counters: HashMap<CounterType, u32>` on every GameObject
- `parse_counter_type()` helper for string-to-CounterType conversion
- `try_split_pump_compound()` as architectural precedent for compound splitting
- `parse_type_phrase()` suffix pipeline for filter properties
- `ReplacementResult` branching in ChangeZone handler

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in #[cfg(test)] + cargo test |
| Config file | Cargo.toml workspace |
| Quick run command | `cargo test -p engine` |
| Full suite command | `cargo test --all` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| BB-01 | parse_counter_filter parses "with a stun counter on it" | unit | `cargo test -p engine -- parse_counter` | Wave 0 |
| BB-02 | FilterProp::HasCounter matches objects with counters | unit | `cargo test -p engine -- filter_has_counter` | Wave 0 |
| BB-03 | try_split_compound splits "tap X and put counter on it" | unit | `cargo test -p engine -- split_compound` | Wave 0 |
| BB-04 | ParentTarget resolves to parent's target | unit | `cargo test -p engine -- parent_target` | Wave 0 |
| BB-05 | try_split_compound_subject splits "this and target X" | unit | `cargo test -p engine -- compound_subject` | Wave 0 |
| BB-06 | ChangeZone to Library auto-shuffles | unit | `cargo test -p engine -- auto_shuffle` | Wave 0 |
| BB-07 | owner_library routes to owner's library | unit | `cargo test -p engine -- owner_library` | Wave 0 |
| BB-08 | Floodpits Drowner full integration | integration | `cargo test -p engine -- floodpits_drowner` | Wave 0 |
| BB-09 | Coverage delta positive | smoke | `cargo coverage` | Existing |

### Sampling Rate
- **Per task commit:** `cargo test -p engine`
- **Per wave merge:** `cargo test --all && cargo clippy --all-targets -- -D warnings`
- **Phase gate:** Full suite green + `cargo coverage` delta positive

### Wave 0 Gaps
- [ ] Parser unit tests for counter filter suffix in oracle_target.rs
- [ ] Parser unit tests for compound effect splitting in oracle_effect.rs
- [ ] Parser unit tests for compound subject splitting in oracle_effect.rs
- [ ] Filter unit test for HasCounter matching in filter.rs
- [ ] ChangeZone unit test for auto-shuffle in change_zone.rs
- [ ] GameScenario integration test for Floodpits Drowner

## Open Questions

1. **ChangeZone `specific_position` field**
   - What we know: The context says "unless specific_position is set" but `specific_position` doesn't exist on ChangeZone yet (only on `zones::move_object_to_position()`).
   - What's unclear: Whether to add `specific_position: Option<LibraryPosition>` to the ChangeZone Effect variant or use a separate mechanism.
   - Recommendation: Add `#[serde(default)] specific_position: Option<LibraryPosition>` to Effect::ChangeZone. The "put on bottom" parser (`parse_put_ast`) already produces ChangeZone -- it needs this field. This is Claude's discretion per CONTEXT.md.

2. **Compound splitter connector heuristics**
   - What we know: "and" vs "then" both produce sub_ability chains.
   - What's unclear: How to reliably distinguish "and" between effects ("tap and put counter") from "and" in type phrases ("creature and planeswalker").
   - Recommendation: Call-site gating resolves this. The compound splitter only fires AFTER parse_target consumed the target phrase (including any "and" in type combinations). The remainder starts with the connector.

3. **ParentTarget resolution mechanism**
   - What we know: Sub_abilities already inherit parent targets via target propagation in resolve_ability_chain.
   - What's unclear: Whether ParentTarget needs explicit resolution logic or if existing target propagation already handles it.
   - Recommendation: Investigate existing target propagation first. If sub_abilities with no explicit targets already receive parent targets, ParentTarget may just be a parser-side marker that tells the engine "don't add new targets, use inherited ones." If not, add resolution logic in resolve_ability_chain.

## Sources

### Primary (HIGH confidence)
- `crates/engine/src/parser/oracle_effect.rs` - Existing compound splitting pattern (try_split_pump_compound), targeted action AST, shuffle AST
- `crates/engine/src/parser/oracle_target.rs` - Target parsing pipeline, parse_type_phrase suffix system
- `crates/engine/src/types/ability.rs` - FilterProp enum, TargetFilter enum, Effect::ChangeZone
- `crates/engine/src/game/filter.rs` - filter_matches_object() runtime dispatch
- `crates/engine/src/game/effects/change_zone.rs` - ChangeZone handler with replacement pipeline
- `crates/engine/src/game/game_object.rs` - CounterType enum, counters HashMap, parse_counter_type()
- `crates/engine/src/game/zones.rs` - move_to_zone() with commander redirect
- `docs/plan-floodpits-drowner.md` - Gap analysis with 3 identified gaps
- `docs/parser-instructions.md` - Parser architecture guide

### Secondary (MEDIUM confidence)
- `.claude/skills/extend-oracle-parser/SKILL.md` - Parser extension checklist
- `.claude/skills/add-engine-effect/SKILL.md` - Effect addition lifecycle

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - all changes are internal to existing crates, no new dependencies
- Architecture: HIGH - all four patterns follow established precedents in the codebase (try_split_pump_compound, parse_power_suffix, TargetFilter::LastCreated, ChangeZone replacement routing)
- Pitfalls: HIGH - identified from direct code inspection and CR rule analysis

**Research date:** 2026-03-17
**Valid until:** 2026-04-17 (stable engine internals, no external dependency churn)
