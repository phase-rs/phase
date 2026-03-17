# Phase 31: Kaito, Bane of Nightmares Mechanics - Research

**Researched:** 2026-03-16
**Domain:** MTG engine mechanics — Ninjutsu, Emblems, compound conditions, animation, dynamic quantities
**Confidence:** HIGH

## Summary

Phase 31 delivers five building blocks that together make Kaito, Bane of Nightmares fully playable while unlocking broad card class coverage. The codebase is well-prepared: `Keyword::Ninjutsu(ManaCost)` already parses, `Zone::Command` and `is_commander` patterns exist for the emblem model, `StaticCondition` and `ContinuousModification` infrastructure from Phase 28 handles animation and compound conditions, and `QuantityRef`/`QuantityExpr` exist for dynamic quantities but need new variants.

The primary complexity lies in Ninjutsu runtime (new combat-phase interaction, keyword activation from hand, entering-attacking logic) and Emblem infrastructure (new `is_emblem` flag, `Effect::CreateEmblem`, immunity in zone-change handlers, layer system integration from command zone). The compound conditions, animation, and dynamic quantity system are extensions of existing patterns with moderate effort.

**Primary recommendation:** Sequence work as: (1) type definitions for all five blocks, (2) compound conditions + animation (lowest risk, extends existing statics), (3) emblem infrastructure, (4) dynamic quantity system, (5) Ninjutsu runtime (highest complexity, most new integration points), (6) parser coverage, (7) integration test with Kaito end-to-end.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- **Ninjutsu Runtime:** Activation via WaitingFor during declare blockers. New `AbilityCost::Ninjutsu` variant with compound cost. Handler in `game/keywords.rs`. No attack triggers per CR 702.49c. Basic AI included.
- **Emblem Infrastructure:** Full GameObject model with `is_emblem: bool` flag in Zone::Command. New `Effect::CreateEmblem`. Standard statics through layers (layer 7c for P/T). Engine-level immunity in change_zone/destroy/exile/bounce handlers. Command zone row UI for emblems.
- **Compound Static Conditions:** `StaticCondition::And(Vec<StaticCondition>)` and `StaticCondition::Or(Vec<StaticCondition>)` combinators. New `StaticCondition::HasCounters { counter_type, minimum }` (self-referential).
- **Planeswalker-to-Creature Animation:** `Vec<ContinuousModification>` on StaticDefinition with SetPT, AddType, AddSubtype, AddKeyword. Layer system applies each in correct layer. Full parser coverage for compound condition + modification list.
- **Dynamic Quantity System:** `QuantityExpr` on Draw, DealDamage, GainLife, Mill, LoseLife. Three new `QuantityRef` variants: `ObjectCount { filter }`, `PlayerCount { filter: PlayerFilter }`, `CountersOnSelf { counter_type }`. New `PlayerFilter` enum with `Opponent`, `OpponentLostLife`, `OpponentGainedLife`, `All`. Scalable `parse_for_each()` parser.
- **Testing:** Per-block scenario tests (~3-5 per block), Kaito integration test, parser snapshot tests.

### Claude's Discretion
- Exact function signatures and module placement for new resolvers
- Internal structuring of Ninjutsu combat interaction hooks
- Emblem GameObject field defaults (no P/T, no mana cost, etc.)
- PlayerFilter variant set beyond the initial four
- parse_for_each() routing heuristics for ambiguous noun phrases
- Frontend emblem mini-card visual design

### Deferred Ideas (OUT OF SCOPE)
- Additional PlayerFilter variants beyond Opponent/OpponentLostLife/OpponentGainedLife/All
- "For each" patterns involving zones other than battlefield (graveyard, exile)
- Gideon-style activated ability animation (different trigger than Kaito's static)
- Emblem triggered abilities (some planeswalker emblems have triggers, not just statics)
</user_constraints>

## Standard Stack

This phase is entirely within the existing Rust engine crate. No new dependencies needed.

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| engine crate | workspace | All game logic, types, parser | Project architecture requirement |
| serde + serde_json | workspace | Serialization for new types | All engine types use serde |
| schemars | workspace | JsonSchema derives on new types | Coverage report and editor validation |
| tsify | workspace (engine-wasm) | TypeScript type generation | Frontend needs new type definitions |

### No New Dependencies
All five building blocks extend existing engine infrastructure. No new crates required.

## Architecture Patterns

### Recommended Module Structure

All new code fits into existing modules:

```
crates/engine/src/
├── types/
│   └── ability.rs          # StaticCondition::And/Or/HasCounters, QuantityRef variants,
│                           # PlayerFilter enum, Effect::CreateEmblem, AbilityCost::Ninjutsu
├── game/
│   ├── game_object.rs      # is_emblem: bool field
│   ├── keywords.rs         # activate_ninjutsu() handler
│   ├── combat.rs           # Ninjutsu activation check (unblocked attacker detection)
│   ├── engine.rs           # NinjutsuActivation GameAction dispatch
│   ├── layers.rs           # evaluate_condition() for And/Or/HasCounters
│   ├── static_abilities.rs # Condition evaluation extensions
│   ├── zones.rs            # Command zone collection for emblems
│   └── effects/
│       ├── mod.rs           # CreateEmblem dispatch
│       ├── create_emblem.rs # NEW: emblem creation handler
│       ├── change_zone.rs   # Emblem immunity check
│       ├── destroy.rs       # Emblem immunity check
│       ├── draw.rs          # QuantityExpr resolution
│       ├── life.rs          # QuantityExpr resolution
│       ├── mill.rs          # QuantityExpr resolution
│       └── deal_damage.rs   # QuantityExpr resolution
├── parser/
│   ├── oracle.rs           # is_static_pattern for compound condition
│   ├── oracle_static.rs    # Compound condition + modification list parsing
│   └── oracle_effect.rs    # parse_for_each(), CreateEmblem parsing
└── database/
    └── oracle_loader.rs    # synthesize_ninjutsu() for keyword activation
```

### Pattern 1: StaticCondition Combinator (And/Or)

**What:** Add `And(Vec<StaticCondition>)` and `Or(Vec<StaticCondition>)` to `StaticCondition` enum. The `evaluate_condition()` function in `layers.rs` already switches on condition variants — add two recursive arms.

**When to use:** Any card with "as long as [condition A] and [condition B]" or compound static conditions.

**Key detail:** Kaito's condition becomes `And([DuringYourTurn, HasCounters { counter_type: Loyalty, minimum: 1 }])`. The `HasCounters` variant is self-referential (checks the object bearing the static), so it needs access to the source object's counters in `evaluate_condition()`.

```rust
// In layers.rs evaluate_condition():
StaticCondition::And { conditions } => {
    conditions.iter().all(|c| evaluate_condition(state, c, controller, source_id))
}
StaticCondition::Or { conditions } => {
    conditions.iter().any(|c| evaluate_condition(state, c, controller, source_id))
}
StaticCondition::HasCounters { counter_type, minimum } => {
    state.objects.get(&source_id)
        .map(|obj| obj.counters.get(&counter_type).copied().unwrap_or(0) >= *minimum)
        .unwrap_or(false)
}
```

### Pattern 2: Emblem as GameObject

**What:** Emblems are GameObjects in `Zone::Command` with `is_emblem: true`. They carry `static_definitions` (and potentially trigger_definitions in future) and participate in the layer system naturally.

**Key details:**
- `is_emblem: bool` field on `GameObject` (follows `is_commander` pattern)
- Default to: no power, no toughness, no loyalty, no mana cost, empty keywords, empty card types
- Command zone currently has no collection Vec in GameState (comments say "not tracked as a collection in Phase 3"). Emblems need to be findable — either add a `command_zone: Vec<ObjectId>` to GameState, or iterate all `state.objects` filtering by `zone == Zone::Command`. The collection approach is cleaner.
- CR 114.4 immunity: `change_zone.rs`, `destroy.rs`, `bounce.rs` handlers check `is_emblem` and skip/no-op
- `gather_active_continuous_effects()` in `layers.rs` currently iterates `state.battlefield` only. Must extend to also iterate command zone objects (emblems) whose statics should affect battlefield objects

### Pattern 3: Ninjutsu Keyword Activation

**What:** During declare blockers, after blockers are declared, the engine checks if the active player has Ninjutsu cards in hand and unblocked attackers. If so, priority includes a NinjutsuActivation option.

**Key integration points:**
1. `combat.rs` — detect unblocked attackers (attackers with no entry in `blocker_assignments`)
2. `engine.rs` — after `DeclareBlockers` processing, before granting priority, check for Ninjutsu options
3. New `WaitingFor::NinjutsuActivation` or integrate into `Priority` with additional legal actions
4. `GameAction::ActivateNinjutsu { ninjutsu_card_id, attacker_to_return }` — new action variant
5. Handler in `keywords.rs`: validate mana payment, return attacker to hand, put Ninjutsu creature onto battlefield tapped and attacking
6. CR 702.49c: creature enters attacking (add to `combat.attackers` directly, same defending player as returned creature) but no "whenever ~ attacks" triggers fire

**Synthesis pattern:** Follow Equip synthesis in `oracle_loader.rs` — `synthesize_ninjutsu()` converts `Keyword::Ninjutsu(ManaCost)` into an activation ability the engine can present as a legal action.

### Pattern 4: QuantityExpr on Count-Based Effects

**What:** Replace fixed `count: u32` on Draw, Mill, and `amount: i32` on LoseLife with a `QuantityExpr` that can be either `Fixed` or `Ref(QuantityRef)`.

**Migration concern:** The `count: u32` field on `Effect::Draw` is used extensively. Changing to `QuantityExpr` requires:
- `#[serde(default)]` so existing `card-data.json` with `count: N` still deserializes (or a custom Deserialize that accepts both u32 and QuantityExpr)
- All test code that constructs `Effect::Draw { count: 1 }` needs updating
- Draw resolver must evaluate `QuantityExpr` at resolution time before drawing

**Recommendation:** Use a new field name like `amount: QuantityExpr` alongside the existing `count` field with `#[serde(default)]`, or implement custom deserialization to handle both formats. The cleaner approach is to make `count` accept `QuantityExpr` with a custom deserializer that treats bare integers as `QuantityExpr::Fixed`.

### Anti-Patterns to Avoid
- **Card-specific emblem handling:** Don't create `Effect::CreateKaitoEmblem`. Create generic `Effect::CreateEmblem { abilities, statics, triggers }` that works for all planeswalker emblems.
- **Hardcoded Ninjutsu in combat.rs:** Don't check for Ninjutsu keywords directly in combat flow. Use synthesis to create an activated ability, and integrate via the existing action/WaitingFor pipeline.
- **Special-case animation:** Don't create a `PlaneswalkerAnimation` effect. Use existing `ContinuousModification` list on `StaticDefinition` — the layer system handles type changes (L4), keywords (L6), and P/T (L7b) naturally.
- **Inlining quantity resolution:** Don't resolve `QuantityExpr` in each effect handler separately. Create a shared `resolve_quantity(state, expr, controller) -> i32` helper that all handlers call.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| P/T setting on animated creatures | Custom power/toughness logic in animate handler | `ContinuousModification::SetPower`/`SetToughness` through layers | Layer system handles ordering, timestamps, interactions with other effects |
| Type changes on animated creatures | Direct mutation of card_types | `ContinuousModification::AddType`/`AddSubtype` through layers | Must interact correctly with other type-changing effects |
| Keyword grants on animated creatures | Direct push to keywords vec | `ContinuousModification::AddKeyword` through layers | Keywords must participate in layer 6 evaluation |
| Emblem statics evaluation | Custom emblem-specific static checking | Existing `gather_active_continuous_effects()` extended to command zone | Emblems should use identical path as battlefield statics |
| Counter counting on objects | Custom counter-check logic | `obj.counters.get(&counter_type).copied().unwrap_or(0)` | Standard HashMap access pattern already used throughout |
| Life-lost tracking | Custom per-turn tracking | `player.life_lost_this_turn` (already exists) | Field already tracked and reset each turn in `turns.rs` |

## Common Pitfalls

### Pitfall 1: Layer System Not Iterating Command Zone for Emblems
**What goes wrong:** Emblem statics exist but never affect battlefield objects because `gather_active_continuous_effects()` only iterates `state.battlefield`.
**Why it happens:** The function was written before emblems existed.
**How to avoid:** Extend the iteration in `gather_active_continuous_effects()` to also scan command zone objects. Only objects with `is_emblem: true` (and potentially `is_commander` with static abilities) should be included from the command zone.
**Warning signs:** Emblem created successfully but "+1/+1 to Ninjas" never applies.

### Pitfall 2: Ninjutsu Creature Firing Attack Triggers
**What goes wrong:** A creature put onto the battlefield via Ninjutsu fires "whenever ~ attacks" triggers.
**Why it happens:** The trigger system fires on zone changes or combat events without distinguishing "declared as attacker" from "entered attacking."
**How to avoid:** CR 702.49c is explicit — Ninjutsu creatures are never "declared as attackers." The `process_triggers()` call should not see an `AttackersDeclared` event for the Ninjutsu creature. Ensure the creature is added to `combat.attackers` directly, not through `declare_attackers()`.
**Warning signs:** "Whenever this creature attacks" triggers fire when creature enters via Ninjutsu.

### Pitfall 3: QuantityExpr Breaking Existing Card Data Deserialization
**What goes wrong:** Changing `count: u32` to `count: QuantityExpr` breaks all existing `card-data.json` entries with `"count": 1`.
**Why it happens:** Serde expects the tagged enum format `{"type": "Fixed", "value": 1}` but gets a bare integer `1`.
**How to avoid:** Either keep `count: u32` and add a separate `dynamic_count: Option<QuantityExpr>` with `#[serde(default)]`, or implement a custom deserializer for the field that handles both formats.
**Warning signs:** Panic on game start when loading card data.

### Pitfall 4: Emblem Not Immune to All Removal
**What goes wrong:** An emblem gets destroyed/exiled/bounced because one removal handler was missed.
**Why it happens:** Immunity must be checked in multiple handlers: `change_zone.rs`, `destroy.rs`, `bounce.rs`, and potentially `sacrifice.rs`.
**How to avoid:** Add immunity checks in ALL zone-change and destruction handlers. An emblem should never leave Zone::Command.
**Warning signs:** Emblem disappears after board wipe or exile effect.

### Pitfall 5: Compound Condition Not Re-evaluating on State Change
**What goes wrong:** Kaito stays animated after losing all loyalty counters (or stops being animated when it shouldn't).
**Why it happens:** The layer system evaluates conditions, but `layers_dirty` might not be set when counters change.
**How to avoid:** Counter changes (loyalty, +1/+1, etc.) already set `layers_dirty = true` in the counter handlers. Verify this path works for loyalty counters specifically. Turn changes already trigger re-evaluation since `DuringYourTurn` is a condition.
**Warning signs:** Kaito stays a creature after all loyalty counters are removed.

### Pitfall 6: Summoning Sickness on Animated Planeswalker
**What goes wrong:** Kaito can attack the turn he becomes a creature, even without haste.
**Why it happens:** `entered_battlefield_turn` was set when Kaito entered as a planeswalker. When he becomes a creature via static ability, summoning sickness check might reference the original entry turn.
**How to avoid:** Per CR 302.6, summoning sickness is checked based on when the permanent entered the battlefield, regardless of when it became a creature. The existing `has_summoning_sickness` logic based on `entered_battlefield_turn` should handle this correctly — Kaito entered this turn, he has summoning sickness, even if he wasn't a creature when he entered. Verify this works.
**Warning signs:** Kaito can attack the same turn he enters the battlefield via Ninjutsu.

## Code Examples

### Creating an Emblem
```rust
// In effects/create_emblem.rs
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (statics,) = match &ability.effect {
        Effect::CreateEmblem { statics, .. } => (statics,),
        _ => return Err(EffectError::MissingParam("CreateEmblem".into())),
    };

    let emblem_id = create_object(state, CardId(0), ability.controller,
        "Emblem".to_string(), Zone::Command);
    let obj = state.objects.get_mut(&emblem_id).unwrap();
    obj.is_emblem = true;
    obj.static_definitions = statics.clone();
    obj.base_static_definitions = statics.clone();

    state.layers_dirty = true;
    // ... emit event
    Ok(())
}
```

### Resolving Dynamic Quantity
```rust
// Shared helper, possibly in game/derived.rs or a new game/quantity.rs
pub fn resolve_quantity(state: &GameState, expr: &QuantityExpr, controller: PlayerId) -> i32 {
    match expr {
        QuantityExpr::Fixed { value } => *value,
        QuantityExpr::Ref { qty } => match qty {
            QuantityRef::HandSize => state.hand(controller).len() as i32,
            QuantityRef::LifeTotal => state.players.iter()
                .find(|p| p.id == controller).map(|p| p.life as i32).unwrap_or(0),
            QuantityRef::ObjectCount { filter } => {
                // Count battlefield objects matching filter
                state.battlefield.iter()
                    .filter(|&&id| filter_matches(state, id, filter, controller))
                    .count() as i32
            }
            QuantityRef::PlayerCount { filter } => {
                resolve_player_count(state, filter, controller)
            }
            QuantityRef::CountersOnSelf { counter_type } => {
                // Needs source_id context — pass through from resolver
                0 // placeholder
            }
            // ... existing variants
        },
    }
}
```

### Ninjutsu Activation Flow
```rust
// In keywords.rs, following Equip pattern
pub fn activate_ninjutsu(
    state: &mut GameState,
    player: PlayerId,
    ninjutsu_card_id: ObjectId,
    attacker_to_return: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    // 1. Validate: card in hand, has Ninjutsu keyword, attacker is unblocked
    // 2. Pay mana cost (extracted from Keyword::Ninjutsu(cost))
    // 3. Return attacker to owner's hand (Zone::Hand)
    // 4. Put ninjutsu creature onto battlefield tapped and attacking
    //    - Set zone to Battlefield, tapped = true
    //    - Add to combat.attackers with same defending_player as returned creature
    //    - Set entered_battlefield_turn
    // 5. Do NOT fire attack triggers (CR 702.49c)
    Ok(())
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| `StaticCondition::DuringYourTurn` only | `StaticCondition::And/Or` combinators | Phase 31 | Compound conditions for dozens of cards |
| Fixed `count: u32` on Draw/Mill | `QuantityExpr` with dynamic resolution | Phase 31 | "For each" patterns across many cards |
| No emblem support | Full GameObject emblems in command zone | Phase 31 | Dozens of planeswalker ultimates |
| `Keyword::Ninjutsu` parse-only | Full Ninjutsu runtime activation | Phase 31 | ~30 Ninja cards playable |

## Open Questions

1. **Command zone object tracking**
   - What we know: `Zone::Command` exists, but no `command_zone: Vec<ObjectId>` in GameState. Comments say "Command zone not tracked as a collection in Phase 3."
   - What's unclear: Should we add a `command_zone` Vec to GameState, or filter `state.objects` by zone?
   - Recommendation: Add `command_zone: Vec<ObjectId>` to GameState for consistency with `battlefield`, `graveyard`, `exile` Vecs. Update `add_to_zone()`/`remove_from_zone()` in `zones.rs`.

2. **QuantityExpr backward compatibility on Effect::Draw**
   - What we know: `count: u32` is used in hundreds of places including card-data.json
   - What's unclear: Best migration strategy — custom deserializer vs dual fields
   - Recommendation: Add `#[serde(default, skip_serializing_if = "Option::is_none")] dynamic_count: Option<QuantityExpr>` alongside existing `count`. At resolution time, prefer `dynamic_count` if present, else use `Fixed(count)`. This avoids breaking any existing data.

3. **Ninjutsu timing integration with priority system**
   - What we know: After `DeclareBlockers` is processed, engine grants priority. Ninjutsu should be activatable during this priority window.
   - What's unclear: Whether Ninjutsu activation should be a special action (no stack) or use the existing activated ability framework
   - Recommendation: CR 702.49a says Ninjutsu is an activated ability (uses the stack? No — it's an ability activated from the hand, similar to other alternative-cost abilities). Actually, per CR 702.49a, it IS an activated ability that can only be activated while the ability's controller has priority, so it uses normal priority and the ability uses the stack. The creature entering is part of the resolution. Model as a legal action during Priority after blockers are declared.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in `#[test]` + `insta` for snapshots |
| Config file | `Cargo.toml` workspace |
| Quick run command | `cargo test -p engine -- test_name` |
| Full suite command | `cargo test --all` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| BB1 | Ninjutsu activation returns attacker, puts creature attacking | scenario | `cargo test -p engine -- ninjutsu -x` | Wave 0 |
| BB1 | Ninjutsu creature does not fire attack triggers | scenario | `cargo test -p engine -- ninjutsu_no_attack_trigger -x` | Wave 0 |
| BB2 | CreateEmblem creates GameObject in command zone | unit | `cargo test -p engine -- emblem -x` | Wave 0 |
| BB2 | Emblem statics apply through layer system | scenario | `cargo test -p engine -- emblem_static -x` | Wave 0 |
| BB2 | Emblems immune to destruction/exile/bounce | unit | `cargo test -p engine -- emblem_immune -x` | Wave 0 |
| BB3 | StaticCondition::And evaluates all sub-conditions | unit | `cargo test -p engine -- condition_and -x` | Wave 0 |
| BB3 | StaticCondition::HasCounters checks source counters | unit | `cargo test -p engine -- condition_has_counters -x` | Wave 0 |
| BB4 | Compound static animates planeswalker as creature with correct types/PT | scenario | `cargo test -p engine -- animate_planeswalker -x` | Wave 0 |
| BB5 | QuantityExpr::Ref resolves dynamic count at resolution | unit | `cargo test -p engine -- quantity_expr -x` | Wave 0 |
| BB5 | Draw with PlayerCount draws correct number | scenario | `cargo test -p engine -- draw_for_each -x` | Wave 0 |
| INT | Kaito full card end-to-end | integration | `cargo test -p engine -- kaito_integration -x` | Wave 0 |
| PARSE | Parser handles compound static Oracle text | unit | `cargo test -p engine -- parse_compound_static -x` | Wave 0 |
| PARSE | Parser handles "for each" quantity expressions | unit | `cargo test -p engine -- parse_for_each -x` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test -p engine -x`
- **Per wave merge:** `cargo test --all && cargo clippy --all-targets -- -D warnings`
- **Phase gate:** Full suite green + `cargo coverage` shows Kaito gaps resolved

### Wave 0 Gaps
- [ ] All test files above are new (Wave 0 creates them alongside implementation)
- [ ] No new test infrastructure needed — `GameScenario` harness and `insta` already available
- [ ] Parser snapshot for Kaito Oracle text: `crates/engine/tests/oracle_parser.rs`

## Sources

### Primary (HIGH confidence)
- **Codebase analysis** — Direct inspection of `ability.rs`, `game_object.rs`, `layers.rs`, `combat.rs`, `effects/mod.rs`, `effects/animate.rs`, `keywords.rs`, `zones.rs`, `game_state.rs`
- **CONTEXT.md** — User decisions from Phase 31 context gathering session
- **docs/plan-kaito-bane-of-nightmares.md** — Full gap analysis
- **Skills** — `add-engine-effect`, `add-keyword`, `add-static-ability` skill files

### Secondary (MEDIUM confidence)
- **MTG Comprehensive Rules** — CR 702.49 (Ninjutsu), CR 114 (Emblems), CR 302.6 (Summoning sickness), CR 613 (Layer system)

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH — no new dependencies, all extensions to existing patterns
- Architecture: HIGH — codebase thoroughly inspected, patterns well-established
- Pitfalls: HIGH — based on direct code analysis of integration points
- Ninjutsu timing: MEDIUM — CR 702.49 rules interaction needs validation during implementation

**Research date:** 2026-03-16
**Valid until:** 2026-04-16 (stable engine architecture)
