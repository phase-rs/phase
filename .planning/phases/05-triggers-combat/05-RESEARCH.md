# Phase 5: Triggers & Combat - Research

**Researched:** 2026-03-07
**Domain:** MTG trigger system, combat engine, keyword abilities
**Confidence:** HIGH

## Summary

Phase 5 implements three major subsystems: the trigger/event bus (TRIG-01 through TRIG-04), the combat engine (COMB-01 through COMB-04), and the keyword registry (KWRD-01, KWRD-02). All three subsystems are deeply intertwined -- combat generates events that fire triggers, and keywords modify combat behavior. The existing codebase has strong scaffolding: `GameEvent` enum with 20+ variants, `TriggerDefinition` parsed from card T: lines, `GameAction::DeclareAttackers`/`DeclareBlockers` already defined, combat phases in the `Phase` enum (currently auto-skipped), and `GameObject.keywords: Vec<String>` ready for migration to typed `Vec<Keyword>`.

The primary challenge is getting the trigger pipeline right: events flow out of `apply()` -> trigger matching scans battlefield for registered triggers -> matched triggers are placed on the stack in APNAP order -> they resolve like any other stack entry. Combat is complex due to first strike/double strike requiring two damage sub-steps, trample requiring ordered blocker damage assignment, and deathtouch interacting with both. All 137 Forge trigger modes and 50+ keywords need complete implementations.

**Primary recommendation:** Build bottom-up: Keyword enum first (it has no dependencies), then trigger infrastructure (event bus + matching + stack placement), then combat system (which depends on both keywords and triggers for correctness).

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Trigger matching: hybrid scan-on-event with cache/index invalidated on battlefield changes
- Stack placement: APNAP with auto-ordering by timestamp, player choice deferred to UI phase
- Condition evaluation: check at match time AND recheck intervening-if on resolution (MTG rules 603.4)
- Last-known information: dies/leave-battlefield triggers use pre-move state snapshots (MTG rule 603.10)
- Keyword representation: typed `Keyword` enum with variants, parameterized via associated data
- Keyword dispatch: match-based (combat.rs handles combat keywords, targeting.rs handles hexproof/shroud)
- Keyword parsing: `FromStr` on `Keyword` enum, colon-delimited for params, `Keyword::Unknown(String)` fallback
- Keyword migration: `Vec<String>` -> `Vec<Keyword>` on GameObject
- ALL 137 trigger modes implemented, ALL 50+ keywords implemented -- no stubs
- Typed `TriggerMode` enum with `FromStr` and `TriggerMode::Unknown(String)` fallback
- Multiple blockers: controller orders per MTG 509.2, requires `WaitingFor::OrderBlockers`
- First strike / double strike: two damage sub-steps with SBA check between
- Trample: auto-assign lethal to each blocker in order, excess to defending player
- Trample + deathtouch: 1 damage counts as lethal per blocker
- `CombatState` struct on `GameState`, created at BeginCombat, cleared at EndCombat
- Cross-cutting: NEVER take shortcuts that violate MTG comprehensive rules
- Wire up existing `DeclareAttackers`/`DeclareBlockers` actions, don't create new variants
- Combat phases become interactive when creatures are present (currently auto-skipped)

### Claude's Discretion
- Trigger registry design details (hybrid scan-on-event with cache invalidation strategy)
- APNAP auto-ordering implementation (timestamp-based or other heuristic)
- Token ETB trigger model (zone-change driven vs separate event)
- Internal module organization for triggers, combat, and keywords
- Combat damage assignment data structures
- How `CombatState` integrates with existing `GameState` lifecycle

### Deferred Ideas (OUT OF SCOPE)
None -- discussion stayed within phase scope
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| TRIG-01 | Event bus for game events | Existing `GameEvent` enum has 20+ variants; events already returned from `apply()`. Trigger system consumes these events post-action. |
| TRIG-02 | Trigger matching by mode against registered triggers | Forge's `TriggerHandler.runWaitingTriggers()` pattern; `TriggerDefinition` already parsed with mode + params. Need `TriggerMode` enum + `fn matches(mode, event, params) -> bool`. |
| TRIG-03 | APNAP ordering for simultaneous triggers | Active Player Non-Active Player ordering per MTG 603.3b. Sort by controller (AP first), then timestamp within same controller. |
| TRIG-04 | All 137 trigger mode handlers | Forge has 137 `TriggerType` enum variants mapping to ~80 unique handler classes. Each handler's `performTest()` checks params against event data. |
| COMB-01 | Attack/block declaration with legality validation | Existing `GameAction::DeclareAttackers/DeclareBlockers` actions. Need validation: summoning sickness, tapped, defender keyword, flying/reach for blockers. |
| COMB-02 | Damage assignment (first strike, double strike, trample, deathtouch, lifelink) | Two damage sub-steps when any creature has first/double strike. Trample assigns lethal-then-excess. Deathtouch makes 1 = lethal. Lifelink gains life on damage. |
| COMB-03 | Combat keyword interactions (flying/reach, menace, vigilance, haste, indestructible) | Keywords modify attack/block legality and damage behavior. Match-based dispatch in combat module. |
| COMB-04 | Death triggers and post-combat state-based actions | SBA check after each damage step. Creatures dying trigger `ChangesZone` (Battlefield->Graveyard) events. Dies triggers need LKI snapshots. |
| KWRD-01 | Keyword registry mapping keywords to combat modifiers, triggers, etc. | Typed `Keyword` enum with `FromStr`. Each keyword maps to behavior in specific modules (combat, targeting, casting). |
| KWRD-02 | 50+ keyword ability implementations | Forge's `Keyword` enum has 200+ entries. For v1, all 50+ standard-relevant keywords fully implemented. |
</phase_requirements>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| Rust std | stable | Core language features | HashMap, Vec, enum, pattern matching are the right tools |
| serde | existing | Serialization for CombatState, Keyword, TriggerMode | Already used throughout codebase |

### Supporting
No new external crates needed. This phase is pure game logic built on existing infrastructure.

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| Manual trigger scan | ECS-style event system | Overkill for this project; simple scan-on-event matches Forge's approach and is easier to debug |
| Match-based keyword dispatch | Trait-based dispatch | Match is more explicit, easier to follow, and consistent with existing effect handler pattern |

## Architecture Patterns

### Recommended Project Structure
```
crates/engine/src/
├── game/
│   ├── combat.rs           # CombatState, attack/block validation, damage assignment
│   ├── combat_damage.rs    # Damage resolution (first strike, trample, deathtouch interactions)
│   ├── triggers.rs         # Trigger matching, APNAP ordering, stack placement pipeline
│   ├── keywords.rs         # Keyword enum, FromStr, has_keyword() helpers
│   ├── engine.rs           # Extended with combat action handling + trigger processing
│   ├── turns.rs            # Modified to stop at combat phases when attackers possible
│   ├── sba.rs              # Extended with deathtouch damage check
│   └── ... (existing)
├── types/
│   ├── keywords.rs         # Keyword enum definition (separate from game logic)
│   ├── triggers.rs         # TriggerMode enum definition
│   └── ... (existing)
```

### Pattern 1: Event-to-Trigger Pipeline
**What:** After each action resolves and events are emitted, scan battlefield for triggers matching those events, then place them on the stack in APNAP order.
**When to use:** Every time `apply()` returns events.
**Example:**
```rust
// In engine.rs, after action processing produces events:
fn process_triggers(
    state: &mut GameState,
    events: &[GameEvent],
) -> Vec<StackEntry> {
    let mut triggered = Vec::new();

    for event in events {
        // Scan all permanents with triggers on battlefield
        for &obj_id in &state.battlefield {
            if let Some(obj) = state.objects.get(&obj_id) {
                for trig_def in &obj.trigger_definitions {
                    let mode = TriggerMode::from_str(&trig_def.mode)
                        .unwrap_or(TriggerMode::Unknown(trig_def.mode.clone()));
                    if trigger_matches(&mode, event, &trig_def.params, obj_id, state) {
                        triggered.push(PendingTrigger {
                            source_id: obj_id,
                            controller: obj.controller,
                            trigger_def: trig_def.clone(),
                            timestamp: obj.entered_battlefield_turn.unwrap_or(0),
                        });
                    }
                }
            }
        }
    }

    // APNAP ordering: active player's triggers first, then NAP
    triggered.sort_by_key(|t| {
        let is_nap = if t.controller == state.active_player { 0 } else { 1 };
        (is_nap, t.timestamp)
    });

    // Place on stack (LIFO means last placed resolves first)
    // So place NAP triggers first, AP triggers last
    triggered.reverse();
    triggered
}
```

### Pattern 2: CombatState Lifecycle
**What:** Dedicated struct tracking combat participants, created at BeginCombat, cleared at EndCombat.
**When to use:** During combat phases.
**Example:**
```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CombatState {
    pub attackers: Vec<AttackerInfo>,
    pub blocker_assignments: HashMap<ObjectId, Vec<ObjectId>>, // attacker -> ordered blockers
    pub blocker_to_attacker: HashMap<ObjectId, ObjectId>,      // reverse lookup
    pub damage_assignments: HashMap<ObjectId, Vec<DamageAssignment>>,
    pub first_strike_done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttackerInfo {
    pub object_id: ObjectId,
    pub defending_player: PlayerId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DamageAssignment {
    pub target: DamageTarget,
    pub amount: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DamageTarget {
    Object(ObjectId),
    Player(PlayerId),
}
```

### Pattern 3: Keyword Enum with Parameterized Variants
**What:** Typed enum where simple keywords are unit variants and parameterized keywords carry data.
**When to use:** Everywhere keywords are checked or stored.
**Example:**
```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Keyword {
    // Simple keywords (combat)
    Flying,
    FirstStrike,
    DoubleStrike,
    Trample,
    Deathtouch,
    Lifelink,
    Vigilance,
    Haste,
    Reach,
    Defender,
    Menace,
    Indestructible,
    Hexproof,
    Shroud,
    Flash,
    Fear,
    Intimidate,
    Skulk,
    Shadow,
    Horsemanship,

    // Parameterized keywords
    Protection(ProtectionTarget),
    Kicker(String),         // ManaCost string
    Cycling(String),        // ManaCost string
    Flashback(String),      // ManaCost string
    Ward(String),           // ManaCost string
    Equip(String),          // ManaCost string
    // ... more parameterized

    // Forward compatibility
    Unknown(String),
}

impl std::str::FromStr for Keyword {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Split on colon for parameterized: "Kicker:1G" -> Kicker("1G")
        if let Some((name, param)) = s.split_once(':') {
            return Ok(match name {
                "Kicker" => Keyword::Kicker(param.to_string()),
                "Cycling" => Keyword::Cycling(param.to_string()),
                "Flashback" => Keyword::Flashback(param.to_string()),
                "Ward" => Keyword::Ward(param.to_string()),
                _ => Keyword::Unknown(s.to_string()),
            });
        }
        Ok(match s {
            "Flying" => Keyword::Flying,
            "First Strike" => Keyword::FirstStrike,
            "Double Strike" => Keyword::DoubleStrike,
            "Trample" => Keyword::Trample,
            // ... all simple keywords
            _ => Keyword::Unknown(s.to_string()),
        })
    }
}
```

### Pattern 4: Trigger Mode Handler Registry
**What:** HashMap mapping TriggerMode to a matching function, parallel to effect handler registry.
**When to use:** During trigger matching.
**Example:**
```rust
pub type TriggerMatcher = fn(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool;

pub fn build_trigger_registry() -> HashMap<TriggerMode, TriggerMatcher> {
    let mut registry = HashMap::new();
    registry.insert(TriggerMode::ChangesZone, match_changes_zone as TriggerMatcher);
    registry.insert(TriggerMode::DamageDone, match_damage_done as TriggerMatcher);
    registry.insert(TriggerMode::SpellCast, match_spell_cast as TriggerMatcher);
    // ... all 137 modes
    registry
}

fn match_changes_zone(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::ZoneChanged { object_id, from, to } = event {
        // Check Origin param
        if let Some(origin) = params.get("Origin") {
            if origin != "Any" && !zone_matches(origin, from) {
                return false;
            }
        }
        // Check Destination param
        if let Some(dest) = params.get("Destination") {
            if dest != "Any" && !zone_matches(dest, to) {
                return false;
            }
        }
        // Check ValidCard filter
        if let Some(filter) = params.get("ValidCard") {
            if !card_matches_filter(state, *object_id, filter, source_id) {
                return false;
            }
        }
        true
    } else {
        false
    }
}
```

### Anti-Patterns to Avoid
- **Checking keywords by string comparison**: Use `Keyword::Flying` not `keywords.contains(&"Flying".to_string())`. The whole point of the typed enum is compile-time safety.
- **Processing triggers inside effect handlers**: Triggers must be collected AFTER all effects resolve, not during. Otherwise you get incorrect ordering and intervening-if violations.
- **Skipping SBAs between first strike and regular damage**: MTG rules 510 requires a full SBA check (and trigger processing) between the two damage steps. Creatures can die to first strike before dealing regular damage.
- **Mutating CombatState after damage assignment**: Once damage is assigned, it should be applied atomically. Don't allow modifications between assignment and application.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| APNAP ordering | Custom priority system | Simple sort by (is_nap, timestamp) | MTG 603.3b is straightforward: AP's triggers first, then NAP's, timestamp within |
| Lethal damage calculation | Per-creature ad-hoc checks | Centralized `fn lethal_damage(obj, state) -> u32` | Must account for deathtouch (1 is lethal), toughness modifiers, damage already marked |
| Keyword checking | Repeated Vec iteration | `fn has_keyword(obj, kw: &Keyword) -> bool` helper | Keywords checked extremely frequently; centralized helper avoids bugs |
| Trigger condition evaluation | Inline per-handler checks | Reuse existing `check_conditions()` from effects/mod.rs | Same condition system (ConditionPresent, ConditionCompare) applies to triggers |

**Key insight:** Forge's trigger handlers are almost entirely param-checking boilerplate. Each handler's `performTest()` just checks if the event's data matches the trigger's params. The Rust version can use a single `fn trigger_matches(mode, event, params)` dispatching to mode-specific matchers.

## Common Pitfalls

### Pitfall 1: Intervening-If Clause Timing
**What goes wrong:** Triggers with "if" conditions (e.g., "When ~ enters the battlefield, if you control a creature, draw a card") only check the condition at match time, not at resolution.
**Why it happens:** MTG rule 603.4 says intervening-if conditions are checked BOTH at trigger time AND resolution. If false at either point, the trigger doesn't fire / fizzles.
**How to avoid:** Two-stage checking: `should_trigger()` at event time, `should_resolve()` at resolution time. Only "intervening if" triggers (those with "if" in the trigger condition text, identified by params like `CheckSVar` or similar) get the double-check.
**Warning signs:** Triggers resolving when their conditions are no longer met.

### Pitfall 2: Last-Known Information for Dies Triggers
**What goes wrong:** A creature's "when this dies" trigger can't find the creature's characteristics because it's already in the graveyard.
**Why it happens:** By the time the trigger goes on the stack, the object has moved zones. Its power, toughness, keywords, and controller must be read from its last state on the battlefield.
**How to avoid:** Before moving a creature from battlefield to graveyard, snapshot its relevant characteristics. Store this LKI data alongside the pending trigger or on the GameState (keyed by ObjectId + timestamp).
**Warning signs:** Dies triggers not finding source creature's data, incorrect damage calculations for "deals damage equal to its power" effects.

### Pitfall 3: First Strike/Double Strike Two-Step Damage
**What goes wrong:** All combat damage is dealt in one step, ignoring that first-strike creatures should deal damage before regular creatures.
**Why it happens:** The two-step process is easy to forget: (1) first strike + double strike creatures deal damage, SBAs check, triggers fire, (2) regular + double strike creatures deal damage, SBAs check, triggers fire.
**How to avoid:** `CombatState.first_strike_done` flag. If ANY creature in combat has first strike or double strike, run the first damage sub-step. Between steps, run full SBA + trigger cycle.
**Warning signs:** Creatures with first strike dying simultaneously with their blockers instead of killing them before taking damage.

### Pitfall 4: Trample Damage Assignment Order
**What goes wrong:** Trample damage doesn't account for blocker ordering. Attacker assigns all lethal to first blocker, then remaining to second, then excess to player.
**Why it happens:** MTG 510.1c requires the attacking player to assign at least lethal damage to each blocker in declared order before assigning to the next.
**How to avoid:** Track blocker order (from `OrderBlockers` step). Auto-assign lethal to each in order, remaining to defending player. With deathtouch, 1 damage is lethal per blocker (MTG 702.2e).
**Warning signs:** Trample damage going to player even when blockers aren't assigned lethal damage.

### Pitfall 5: Trigger Stack Ordering (APNAP)
**What goes wrong:** Triggers go on the stack in random order, or all at once without respecting who controls them.
**Why it happens:** MTG 603.3b: if multiple triggers would go on the stack simultaneously, active player puts theirs on first (they resolve last), then non-active player.
**How to avoid:** Collect all triggers from a single event batch, group by controller, sort AP first then NAP. Within each group, sort by timestamp. Place on stack so NAP's triggers are "under" AP's triggers.
**Warning signs:** Non-active player's triggers resolving before active player's.

### Pitfall 6: Summoning Sickness Check
**What goes wrong:** Creatures can attack the turn they enter, or creatures with haste can't attack.
**Why it happens:** Summoning sickness is checked incorrectly. A creature can attack if it has haste OR if `entered_battlefield_turn` is strictly less than the current turn number.
**How to avoid:** `fn can_attack(obj, state) -> bool` checking: is creature? not tapped? not defender? has haste or entered before this turn?
**Warning signs:** Fresh creatures attacking without haste, or hasty creatures being blocked from attacking.

## Code Examples

### Combat Phase Auto-Advance Modification
```rust
// In turns.rs auto_advance(), replace the combat skip block:
Phase::BeginCombat => {
    // Create CombatState
    state.combat = Some(CombatState::default());
    // Check if active player has potential attackers
    if has_potential_attackers(state) {
        advance_phase(state, events);
        // Fall through to DeclareAttackers
    } else {
        // Skip combat entirely
        state.combat = None;
        state.phase = Phase::EndCombat;
        advance_phase(state, events);
    }
}
Phase::DeclareAttackers => {
    return WaitingFor::DeclareAttackers {
        player: state.active_player,
    };
}
```

### New GameEvent Variants Needed
```rust
// Add to GameEvent enum:
AttackersDeclared {
    attacker_ids: Vec<ObjectId>,
    defending_player: PlayerId,
},
BlockersDeclared {
    assignments: Vec<(ObjectId, ObjectId)>,
},
CombatDamageDealt {
    source_id: ObjectId,
    target: TargetRef,
    amount: u32,
    is_combat_damage: bool,
},
CreatureDied {
    object_id: ObjectId,
    controller: PlayerId,
},
```

### Keyword Has-Check Helper
```rust
impl GameObject {
    pub fn has_keyword(&self, keyword: &Keyword) -> bool {
        self.keywords_typed.iter().any(|k| {
            std::mem::discriminant(k) == std::mem::discriminant(keyword)
        })
    }

    pub fn has_flying(&self) -> bool {
        self.keywords_typed.iter().any(|k| matches!(k, Keyword::Flying))
    }

    // Can also use pattern for parameterized:
    pub fn has_protection_from(&self, color: &ManaColor) -> bool {
        self.keywords_typed.iter().any(|k| matches!(k, Keyword::Protection(ProtectionTarget::Color(c)) if c == color))
    }
}
```

### WaitingFor New Variants
```rust
// Add to WaitingFor enum:
DeclareAttackers { player: PlayerId },
DeclareBlockers { player: PlayerId },
OrderBlockers {
    player: PlayerId,
    attacker_id: ObjectId,
    blocker_ids: Vec<ObjectId>,
},
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| `keywords: Vec<String>` | `keywords: Vec<Keyword>` | This phase | Type safety, pattern matching, no string typos |
| Combat phases auto-skipped | Combat phases interactive | This phase | Creatures can actually fight |
| No trigger processing | Event -> match -> stack pipeline | This phase | ETB, dies, attacks triggers all work |
| `TriggerDefinition.mode: String` | `TriggerMode` enum with `FromStr` | This phase | Type-safe trigger dispatch |

## Open Questions

1. **Token ETB trigger model**
   - What we know: Tokens entering the battlefield should trigger ETB effects. Zone changes are tracked via `ZoneChanged` events.
   - What's unclear: Tokens don't come "from" anywhere -- they're created directly on the battlefield. Forge treats token creation as a zone change from null to battlefield.
   - Recommendation: Use `ZoneChanged { from: Zone::None, to: Zone::Battlefield }` or add a separate `TokenEnteredBattlefield` event. Zone-change driven is more consistent with existing patterns. May need to add `Zone::None` or use a sentinel.

2. **Trigger definition storage on GameObject**
   - What we know: `CardFace` has `triggers: Vec<String>` (raw trigger strings). `TriggerDefinition` is parsed from these.
   - What's unclear: Should parsed `TriggerDefinition`s be stored on `GameObject` at creation time, or parsed on-demand during trigger matching?
   - Recommendation: Parse at object creation time and store as `Vec<TriggerDefinition>` on `GameObject`. Avoids repeated parsing during hot path (trigger matching runs after every action).

3. **Cache invalidation strategy for trigger registry**
   - What we know: CONTEXT.md says hybrid scan-on-event with cache invalidated on battlefield changes.
   - What's unclear: What granularity of cache? Per-TriggerMode -> Vec<ObjectId>? Full rebuild on any battlefield change?
   - Recommendation: Start with full scan (no cache) for correctness. Optimize with cache only if performance is an issue. The battlefield is typically <20 permanents, scanning is fast.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in test + cargo test |
| Config file | Cargo.toml (existing) |
| Quick run command | `cargo test --lib -p engine` |
| Full suite command | `cargo test --workspace` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| TRIG-01 | Events flow through trigger pipeline | unit | `cargo test --lib -p engine triggers::tests -x` | Wave 0 |
| TRIG-02 | Trigger matching by mode | unit | `cargo test --lib -p engine triggers::tests::match_ -x` | Wave 0 |
| TRIG-03 | APNAP ordering | unit | `cargo test --lib -p engine triggers::tests::apnap -x` | Wave 0 |
| TRIG-04 | All 137 trigger mode handlers | unit | `cargo test --lib -p engine triggers::tests::mode_ -x` | Wave 0 |
| COMB-01 | Attack/block legality | unit | `cargo test --lib -p engine combat::tests::legality -x` | Wave 0 |
| COMB-02 | Damage assignment | unit | `cargo test --lib -p engine combat::tests::damage -x` | Wave 0 |
| COMB-03 | Combat keyword interactions | unit | `cargo test --lib -p engine combat::tests::keyword -x` | Wave 0 |
| COMB-04 | Death triggers + post-combat SBA | integration | `cargo test --lib -p engine combat::tests::death -x` | Wave 0 |
| KWRD-01 | Keyword registry | unit | `cargo test --lib -p engine keywords::tests -x` | Wave 0 |
| KWRD-02 | 50+ keyword implementations | unit | `cargo test --lib -p engine keywords::tests::parse -x` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test --lib -p engine`
- **Per wave merge:** `cargo test --workspace`
- **Phase gate:** Full suite green before verify

### Wave 0 Gaps
- [ ] `crates/engine/src/game/keywords.rs` -- Keyword enum + FromStr + tests
- [ ] `crates/engine/src/game/triggers.rs` -- trigger pipeline + matching + tests
- [ ] `crates/engine/src/game/combat.rs` -- CombatState + attack/block validation + tests
- [ ] `crates/engine/src/game/combat_damage.rs` -- damage resolution + tests
- [ ] `crates/engine/src/types/keywords.rs` -- Keyword/TriggerMode type definitions
- [ ] `GameObject.trigger_definitions: Vec<TriggerDefinition>` field -- parsed at creation time
- [ ] `GameState.combat: Option<CombatState>` field

## Sources

### Primary (HIGH confidence)
- Forge source code at `../forge` -- TriggerType.java (137 variants), Keyword.java (200+ variants), Combat.java, TriggerHandler.java, TriggerChangesZone.java
- Existing codebase -- types/events.rs, types/ability.rs, types/actions.rs, types/phase.rs, game/engine.rs, game/turns.rs, game/sba.rs, game/effects/mod.rs, parser/ability.rs, game/game_object.rs, types/game_state.rs

### Secondary (MEDIUM confidence)
- MTG Comprehensive Rules 603 (triggers), 509 (declare blockers), 510 (combat damage) -- referenced in CONTEXT.md decisions

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- pure Rust, no new dependencies needed
- Architecture: HIGH -- patterns established in Phase 4 (fn pointer registry, effect handlers), extended naturally
- Keyword enum: HIGH -- Forge's Keyword.java provides complete reference, FromStr pattern already used in codebase
- Trigger system: HIGH -- Forge's TriggerHandler + TriggerType provide complete reference, event system already exists
- Combat system: HIGH -- Forge's Combat.java provides reference, existing Phase enum and actions defined
- Pitfalls: HIGH -- well-documented MTG rules edge cases, Forge source code confirms

**Research date:** 2026-03-07
**Valid until:** 2026-04-07 (stable domain, no external dependency changes)
