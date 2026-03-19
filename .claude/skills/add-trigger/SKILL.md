---
name: add-trigger
description: Use when adding or modifying triggered abilities — ETB, dies, attacks, damage dealt, spell cast, phase-based, or any "When/Whenever/At the beginning of" ability. Covers TriggerDefinition, TriggerMode, the matcher registry, APNAP ordering, targeting, intervening-if, constraints, and parser wiring.
---

# Adding a Triggered Ability

Triggered abilities fire in response to game events and go on the stack (MTG Rule 603). They differ from replacement effects (which modify events) and static abilities (which exist continuously). This skill covers the full trigger pipeline: event fired → matcher detects → APNAP ordering → targeting → stack → resolution.

**Before you start:** Trace how `ChangesZone` (ETB/dies) triggers work end-to-end. They're the most complete reference: `parse_trigger_line()` in `oracle_trigger.rs` → `TriggerDefinition` → `process_triggers()` → `match_changes_zone()` → stack placement → `resolve_top()`.

---

## MTG Rules Reference

| Rule | What it governs | Engine implication |
|------|----------------|-------------------|
| **603.2** | A triggered ability triggers when its event occurs | `process_triggers()` scans all permanents against the event |
| **603.3** | Triggered abilities are placed on the stack (APNAP order) | Sort by `(is_nap, timestamp)` before stack placement |
| **603.3b** | Active player's triggers go on stack first (resolve last) | NAP triggers pushed after AP triggers |
| **603.4** | Intervening-if checked at fire-time AND resolution-time | `TriggerCondition` checked in both `process_triggers()` and `resolve_top()` |
| **603.5** | Triggered abilities target when placed on stack | `extract_target_filter_from_effect()` → targeting phase before stack |
| **603.6c** | "Once each turn" constraint | `TriggerConstraint::OncePerTurn` tracked in `triggers_fired_this_turn` |

---

## Key Types

### `TriggerDefinition` — `crates/engine/src/types/ability.rs`

```rust
pub struct TriggerDefinition {
    pub mode: TriggerMode,                    // Event type: ChangesZone, DamageDone, SpellCast, etc.
    pub execute: Option<Box<AbilityDefinition>>, // Effect to run when triggered
    pub valid_card: Option<TargetFilter>,      // Filter on the event subject ("another creature")
    pub origin: Option<Zone>,                  // For zone changes: from where
    pub destination: Option<Zone>,             // For zone changes: to where
    pub trigger_zones: Vec<Zone>,             // Active zones (default: Battlefield)
    pub phase: Option<Phase>,                 // For phase triggers (Upkeep, End, etc.)
    pub optional: bool,                       // "You may" triggers
    pub combat_damage: bool,                  // Combat damage only filter
    pub secondary: bool,                      // Multi-target secondary indicator
    pub valid_target: Option<TargetFilter>,   // Controller filter for events (You, Opponent)
    pub valid_source: Option<TargetFilter>,   // Source filter for damage/spell triggers
    pub description: Option<String>,          // Human-readable text
    pub constraint: Option<TriggerConstraint>, // OncePerTurn, OncePerGame, OnlyDuringYourTurn
    pub condition: Option<TriggerCondition>,   // Intervening-if condition
}
```

### `TriggerMode` — `crates/engine/src/types/triggers.rs`

The event discriminant. ~220 variants organized by category:

| Category | Example Modes | Typical Event |
|----------|--------------|---------------|
| Zone changes | `ChangesZone`, `ChangesZoneAll` | `GameEvent::ZoneChanged` |
| Damage | `DamageDone`, `DamageDoneOnce`, `ExcessDamage` | `GameEvent::DamageDealt` |
| Spells | `SpellCast`, `SpellCopy`, `Countered` | `GameEvent::SpellCast` |
| Combat | `Attacks`, `Blocks`, `AttackerBlocked`, `AttackerUnblocked` | `GameEvent::AttackersDeclared` |
| Permanents | `Taps`, `Untaps`, `Sacrificed`, `Destroyed` | `GameEvent::Tapped` |
| Cards | `Drawn`, `Discarded`, `Milled`, `Exiled`, `Revealed` | `GameEvent::CardDrawn` |
| Life | `LifeGained`, `LifeLost` | `GameEvent::LifeChanged` |
| Phases | `Phase` (with `phase: Some(Phase::Upkeep)`) | `GameEvent::PhaseStarted` |
| Tokens | `TokenCreated`, `TokenCreatedOnce` | `GameEvent::TokenCreated` |
| Other | `Explored`, `Fight`, `Transformed`, `DayTimeChanges` | Various |
| Fallback | `Unknown(String)` | Never matches (parsed but unimplemented) |

### `TriggerConstraint` — `crates/engine/src/types/ability.rs`

Rate-limiting:
- `OncePerTurn` — tracked in `state.triggers_fired_this_turn`
- `OncePerGame` — tracked in `state.triggers_fired_this_game`
- `OnlyDuringYourTurn` — checked at fire-time via `state.active_player`

### `TriggerCondition` — `crates/engine/src/types/ability.rs`

Intervening-if (checked at fire AND resolution):
- `LifeGainedThisTurn { minimum: u32 }` — "if you've gained N or more life this turn"

---

## The Trigger Pipeline

```
GameEvent fired (zone change, damage, spell cast, etc.)
    ↓
process_triggers(state, events) — crates/engine/src/game/triggers.rs
    ↓
For each battlefield permanent (+ graveyard if trigger_zones includes Graveyard):
    For each TriggerDefinition on the object:
        ↓
        1. Get matcher from registry by TriggerMode
        2. Call matcher(event, trigger_def, source_id, state)
        3. If matched:
           a. Check constraint (OncePerTurn, etc.)
           b. Check condition (intervening-if at fire-time)
           c. Build ResolvedAbility from execute field
           d. Add to pending list
    ↓
Also check keyword-based triggers:
    Prowess → synthetic trigger for noncreature spells
    ↓
Sort pending by APNAP order: (is_nap, timestamp)
    Reverse so NAP triggers resolve first on stack
    ↓
For each pending trigger:
    extract_target_filter_from_effect()
    ├─ No targets needed → push to stack directly
    ├─ Exactly 1 legal target → auto-target, push to stack
    └─ Multiple legal targets → store in state.pending_trigger
         → engine returns WaitingFor::TriggerTargetSelection
         → player selects via GameAction::SelectTargets
         → engine pushes to stack with selected targets
    ↓
Stack resolves later:
    TriggeredAbility with condition → re-check intervening-if (Rule 603.4)
    If condition false → ability is countered (not executed)
```

---

## Checklist — Adding a New Trigger

### Phase 1 — Type Definition

- [ ] **`crates/engine/src/types/triggers.rs` — `TriggerMode` enum**
  Add a variant for your event type. Skip if an existing mode fits. Convention: use PascalCase verb form matching the event (`SpellCast`, `DamageDone`, `Attacks`).

- [ ] **`crates/engine/src/types/ability.rs` — `TriggerCondition` enum** (if new intervening-if)
  Add a variant if the trigger has a condition not covered by existing conditions.

- [ ] **`crates/engine/src/types/ability.rs` — `TriggerConstraint` enum** (if new constraint)
  Add a variant if the trigger has a rate-limit not covered by existing constraints.

### Phase 2 — Event Emission

The trigger pipeline responds to `GameEvent` variants. If no existing event covers your trigger:

- [ ] **`crates/engine/src/types/events.rs` — `GameEvent` enum**
  Add a variant for the event your trigger responds to. Include enough data for the matcher to validate (who, what, where).

- [ ] **Emit the event** at the appropriate point in the game logic module (e.g., `combat.rs`, `zones.rs`, `effects/`). Events must be emitted BEFORE `process_triggers()` is called for them to be detected.

### Phase 3 — Matcher

- [ ] **`crates/engine/src/game/triggers.rs` — matcher function**
  Write a matcher with signature:
  ```rust
  fn match_your_event(
      event: &GameEvent, trigger: &TriggerDefinition,
      source_id: ObjectId, state: &GameState,
  ) -> bool
  ```
  Pattern:
  1. Match on the `GameEvent` variant
  2. Check `valid_card` filter via `valid_card_matches()`
  3. Check trigger-specific fields (`origin`, `destination`, `combat_damage`, `valid_target`, etc.)
  4. Return true if all checks pass

- [ ] **`crates/engine/src/game/trigger_matchers.rs` — `build_trigger_registry()`**
  Add an entry: `registry.insert(TriggerMode::YourEvent, match_your_event);`

  **If you skip this, the trigger will parse correctly but never fire at runtime — a silent failure.**

### Phase 4 — Target Extraction (if trigger effect targets)

- [ ] **`crates/engine/src/game/triggers.rs` — `extract_target_filter_from_effect()`**
  If the trigger's `execute` effect has a target that requires player selection (not `SelfRef`/`Controller`/`None`), add a match arm returning `Some(&target_filter)`.

  **If you skip this, the trigger will go on the stack without requesting targets. The effect will fail at resolution with no valid targets.**

### Phase 5 — Parser

- [ ] **`crates/engine/src/parser/oracle_trigger.rs` — `parse_trigger_line()`**
  The trigger parser handles three main patterns:

  - **"When/Whenever X, do Y"** — One-shot or recurring event trigger
  - **"At the beginning of X, do Y"** — Phase-based trigger
  - **"Whenever X deals combat damage to a player, do Y"** — Damage trigger

  Add detection for your trigger's Oracle text pattern. Key sub-parsers:
  - Subject parsing: "~", "another creature", "a creature you control"
  - Event verb parsing: "enters", "dies", "attacks", "deals damage"
  - Intervening-if extraction: "if you've gained life this turn"
  - Constraint parsing: "triggers only once each turn"

- [ ] **`crates/engine/src/parser/oracle_trigger.rs` — parser tests**
  Every new pattern needs a test:
  ```rust
  #[test]
  fn trigger_your_new_pattern() {
      let t = parse_trigger_line("When ~ enters, draw a card.");
      assert_eq!(t.mode, TriggerMode::ChangesZone);
      assert_eq!(t.destination, Some(Zone::Battlefield));
      assert!(matches!(t.execute.unwrap().effect, Effect::Draw { count: 1 }));
  }
  ```

### Phase 6 — Condition/Constraint Tracking (if new)

- [ ] **`crates/engine/src/game/triggers.rs` — `check_trigger_condition()`**
  If you added a new `TriggerCondition`, add evaluation logic here. This is called both at fire-time and resolution-time.

- [ ] **`crates/engine/src/types/game_state.rs` — tracking state** (if new constraint)
  `OncePerTurn` uses `triggers_fired_this_turn: HashSet`. `OncePerGame` uses `triggers_fired_this_game: HashSet`. If you need new tracking, add it to `GameState`.

### Phase 7 — Stack Resolution

- [ ] **`crates/engine/src/game/stack.rs` — `resolve_top()`**
  Usually no changes needed — triggered abilities resolve through the standard `resolve_ability_chain()` path. Only modify if:
  - Your trigger has a new `TriggerCondition` that needs re-checking
  - Your trigger has special resolution behavior

### Phase 8 — Tests

- [ ] Parser test: Oracle text → correct `TriggerDefinition`
- [ ] Matcher test: event + trigger def → matches/doesn't match
- [ ] Integration test: full game flow → event fires → trigger on stack → resolves
- [ ] APNAP test: multiple player triggers → correct stack order
- [ ] Constraint test: "once per turn" fires once, not twice
- [ ] `cargo test -p engine && cargo clippy --all-targets -- -D warnings`

---

## Reference: Existing Matchers Worth Studying

| Matcher | TriggerMode | What it checks | Complexity |
|---------|-------------|---------------|------------|
| `match_changes_zone` | `ChangesZone` | origin, destination, valid_card | Simple — canonical reference |
| `match_spell_cast` | `SpellCast` | caster (valid_target), spell type (valid_card) | Medium — two filters |
| `match_damage_done` | `DamageDone` | source, target, combat_damage flag | Medium — three filters |
| `match_attacks` | `Attacks` | attacker (valid_card or SelfRef) | Simple |
| `match_phase` | `Phase` | phase match + controller (valid_target) | Simple |
| `match_life_gained` | `LifeGained` | who gained (valid_target) | Simple |

### The `valid_card_matches()` Helper

Shared function that validates the `valid_card` filter against the event's subject:

```
SelfRef → subject must be the trigger source
Another → subject must NOT be the trigger source
Typed { card_type, controller, props } → full filter evaluation against the subject
```

Used by all matchers — always call this instead of reimplementing filter logic.

---

## Common Mistakes

| Mistake | Consequence | Fix |
|---------|-------------|-----|
| **Missing registry entry in `build_trigger_registry()`** | Trigger parses but never fires — completely silent failure | Always add the registry entry |
| Missing `extract_target_filter_from_effect()` arm | Triggered ability goes on stack without targets, fails at resolution | Add the match arm for targeting effects |
| Emitting event AFTER `process_triggers()` call | Trigger misses the event entirely | Emit events before the trigger scan |
| Not checking intervening-if at both fire and resolve time | Trigger fires when it shouldn't, or resolves when condition is false | Use `TriggerCondition` — checked automatically at both points |
| APNAP ordering wrong | Triggers resolve in wrong player order | `process_triggers()` handles this — don't manually reorder |
| `trigger_zones` empty (default) | Trigger only fires from battlefield, not graveyard | Set `trigger_zones: vec![Zone::Graveyard]` for dies triggers that work from graveyard |
| Forgetting `valid_card: Some(SelfRef)` on "When ~" triggers | Trigger fires for any permanent, not just the source | Set valid_card for self-triggers |

---

## Self-Maintenance

After completing work using this skill:

1. **Verify references** with the check below
2. **Update the reference table** if you added a new matcher
3. **Update TriggerCondition/Constraint sections** if you added new variants

### Verification

```bash
rg -q "fn process_triggers" crates/engine/src/game/triggers.rs && \
rg -q "fn collect_matching_triggers" crates/engine/src/game/triggers.rs && \
rg -q "fn extract_target_filter_from_effect" crates/engine/src/game/triggers.rs && \
rg -q "fn build_trigger_registry" crates/engine/src/game/trigger_matchers.rs && \
rg -q "struct TriggerDefinition" crates/engine/src/types/ability.rs && \
rg -q "enum TriggerMode" crates/engine/src/types/triggers.rs && \
rg -q "enum TriggerConstraint" crates/engine/src/types/ability.rs && \
rg -q "enum TriggerCondition" crates/engine/src/types/ability.rs && \
rg -q "fn parse_trigger_line" crates/engine/src/parser/oracle_trigger.rs && \
rg -q "pending_trigger" crates/engine/src/types/game_state.rs && \
rg -q "TriggerTargetSelection" crates/engine/src/types/game_state.rs && \
rg -q "fn resolve_top" crates/engine/src/game/stack.rs && \
echo "✓ add-trigger skill references valid" || \
echo "✗ STALE — update skill references"
```
