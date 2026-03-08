# Phase 3: Game State Engine - Research

**Researched:** 2026-03-07
**Domain:** MTG game engine core — turn structure, priority, stack, zones, mana, state-based actions, mulligan
**Confidence:** HIGH

## Summary

Phase 3 builds the core game loop for a two-player MTG game. The existing codebase has type scaffolding (GameState, Phase, Zone, GameAction, GameEvent, ManaPool, Player, identifiers) but no engine logic. The engine must implement an action-response pattern where each `apply(&mut GameState, action)` call returns events and a `WaitingFor` descriptor, with auto-advancing through phases that need no player input.

Key subsystems: (1) turn phase progression with 12 phases, (2) priority system tracking consecutive passes, (3) stack with LIFO resolution, (4) zone management with a central `HashMap<ObjectId, GameObject>`, (5) mana pool restructured from simple counters to tracked mana units, (6) state-based actions as a fixpoint loop, and (7) London mulligan. The user has explicitly decided on mutate-in-place state management with standard Rust collections (no rpds yet) and Forge parity as the guiding principle.

**Primary recommendation:** Build bottom-up: GameObject model and zone storage first, then ManaPool restructure, then turn/priority engine, then stack, then SBAs, then mulligan. Each layer is testable in isolation.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Standard Rust collections (Vec, HashMap, BTreeMap) — defer rpds/persistent data structures until profiling justifies it (Phase 8 AI earliest)
- Mutate-in-place: `fn apply(&mut GameState, action) -> Vec<GameEvent>` — matches Forge, idiomatic Rust with ownership safety
- Events returned from apply — caller decides what to do with them (trigger system in Phase 5, UI updates in Phase 7)
- Guiding principle: follow Forge's approach unless Rust has a genuinely better alternative
- Action-response pattern: engine processes one action, returns `(state, events, WaitingFor)` — no internal game loop
- Engine auto-advances through phases that need no player input (untap -> upkeep -> draw -> stop at main phase) in a single apply call
- `WaitingFor` enum tells caller what input is needed next — extensible for future phases
- Priority counter: track consecutive passes. Both pass in succession -> empty stack advances phase, non-empty stack resolves top
- State-based actions run as fixpoint loop after every action (match MTG rules 704.3 and Forge's implementation)
- Per-zone collections on Player: library (Vec<ObjectId>), hand, graveyard
- Shared top-level collections: battlefield, stack, exile
- Central `HashMap<ObjectId, GameObject>` for actual object data
- Seeded RNG (rand crate StdRng) for library shuffling — deterministic for testing, replays, and network play
- Port all rules-relevant fields from Forge's Card.java — not display/UI fields (~50 rules fields vs ~200 total in Forge)
- Full model built upfront so the struct stabilizes across phases 4-6
- Full mana payment system in Phase 3 (not deferred to Phase 4)
- ManaPool restructured: track individual mana units with source ObjectId and restrictions
- Add, spend, clear operations — clear pool on phase transitions per MTG rules
- Hybrid/phyrexian cost payment: auto-select best payment, player can override via explicit ManaPayment action
- Supports: 5 colors, colorless, generic, hybrid, phyrexian, X costs, snow

### Claude's Discretion
- Auto-advance behavior: whether to advance through multiple phases in one call or step-by-step
- Internal module organization for the game engine
- Exact WaitingFor enum variants
- SBA implementation order and which SBAs to implement in Phase 3 vs later
- How to represent the stack (simple Vec<StackEntry> vs more complex)
- London mulligan flow details (how bottom-of-library choice is presented)

### Deferred Ideas (OUT OF SCOPE)
None — discussion stayed within phase scope
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| ENG-01 | Full turn structure (untap, upkeep, draw, main1, combat phases, main2, end, cleanup) | Phase enum exists with all 12 phases; engine needs phase progression with auto-advance and turn-based actions per phase |
| ENG-02 | Priority system with LIFO stack resolution | Priority counter pattern, stack as Vec<StackEntry>, consecutive-pass tracking, LIFO pop-and-resolve |
| ENG-03 | State-based actions with fixpoint loop checking | SBA functions for 0-toughness, 0-life, legend rule, unattached auras; loop until no more actions performed |
| ENG-04 | Zone management (library, hand, battlefield, graveyard, stack, exile, command) | Zone enum exists; needs per-player zones + shared zones + central object store + zone-change events |
| ENG-05 | Mana system (5 colors, colorless, generic, hybrid, phyrexian, X costs, snow) | ManaPool restructure from counters to tracked units; ManaCostShard already has all 40+ variants; need spend/clear/payment algorithms |
| ENG-06 | London mulligan | Draw 7, choose keep/mulligan, redraw 7, put N on bottom; action-response pattern for the choice steps |
</phase_requirements>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| rand | 0.9 | Seeded RNG for shuffling | De facto Rust RNG; StdRng provides deterministic seeded generation |
| serde | 1 (workspace) | Serialization for all types | Already in use, all types derive Serialize/Deserialize |
| thiserror | 2 (existing) | Error types for engine errors | Already in Cargo.toml |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| rand_chacha | 0.9 | ChaCha20Rng for cross-platform determinism | StdRng is not guaranteed cross-platform deterministic; ChaCha20Rng is. Use if replays must be cross-platform. |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| rand StdRng | rand ChaCha20Rng | StdRng may differ across platforms; ChaCha20Rng is deterministic everywhere. Recommend ChaCha20Rng for replay/network. |
| Vec for stack | VecDeque | Vec with push/pop is natural LIFO; VecDeque adds unnecessary complexity for a stack |

**Installation:**
```bash
cargo add rand --features std -p engine
```

Note: `rand` 0.9 changed the API significantly from 0.8. Use `rand::rng()` for thread-local RNG and `rand::rngs::StdRng::seed_from_u64(seed)` for deterministic seeding. The `SeedableRng` trait is in `rand::SeedableRng` (re-exported from rand_core).

## Architecture Patterns

### Recommended Module Structure
```
crates/engine/src/
├── types/              # Existing — type definitions
│   ├── game_state.rs   # Expanded GameState with zones, objects, RNG
│   ├── player.rs       # Expanded Player with zone collections
│   ├── mana.rs         # Restructured ManaPool with tracked units
│   └── ...             # Existing types
├── game/               # NEW — engine logic
│   ├── mod.rs
│   ├── engine.rs       # apply() function, action dispatch
│   ├── turns.rs        # Phase progression, auto-advance
│   ├── priority.rs     # Priority system, consecutive-pass tracking
│   ├── stack.rs        # Stack operations, LIFO resolution
│   ├── zones.rs        # Zone-change operations, move_to_zone
│   ├── mana_payment.rs # Mana spending, auto-pay, hybrid resolution
│   ├── sba.rs          # State-based actions fixpoint loop
│   ├── mulligan.rs     # London mulligan flow
│   └── game_object.rs  # GameObject struct and helpers
├── database/           # Existing
├── parser/             # Existing
└── lib.rs
```

### Pattern 1: Action-Response Engine
**What:** Single entry point `apply(&mut GameState, GameAction) -> ActionResult` where `ActionResult` contains events and what input is needed next.
**When to use:** Every external interaction with the engine.
**Example:**
```rust
pub struct ActionResult {
    pub events: Vec<GameEvent>,
    pub waiting_for: WaitingFor,
}

pub enum WaitingFor {
    // Phase 3 variants
    Priority { player: PlayerId },
    MulliganDecision { player: PlayerId, mulligan_count: u8 },
    MulliganBottomCards { player: PlayerId, count: u8 },
    ManaPayment { player: PlayerId, cost: ManaCost, partial: ManaPaymentState },
    GameOver { winner: Option<PlayerId> },
}

pub fn apply(state: &mut GameState, action: GameAction) -> ActionResult {
    // 1. Validate action against current WaitingFor
    // 2. Apply the action
    // 3. Run state-based actions (fixpoint)
    // 4. Auto-advance phases if no player input needed
    // 5. Return events + next WaitingFor
}
```

### Pattern 2: Phase Auto-Advance
**What:** After processing an action, the engine automatically steps through phases that require no player input until reaching one that does.
**When to use:** After every action application. Untap -> Upkeep -> Draw all execute automatically and stop at PreCombatMain.
**Example:**
```rust
fn auto_advance(state: &mut GameState, events: &mut Vec<GameEvent>) -> WaitingFor {
    loop {
        match state.phase {
            Phase::Untap => {
                execute_untap(state, events);
                advance_phase(state, events);
            }
            Phase::Upkeep => {
                // In Phase 3, upkeep has no triggers; just advance
                advance_phase(state, events);
            }
            Phase::Draw => {
                execute_draw(state, events);
                advance_phase(state, events);
            }
            Phase::PreCombatMain | Phase::PostCombatMain => {
                return WaitingFor::Priority { player: state.active_player };
            }
            // Combat phases: skip in Phase 3 (no creatures attacking)
            Phase::BeginCombat | Phase::DeclareAttackers | Phase::DeclareBlockers
            | Phase::CombatDamage | Phase::EndCombat => {
                advance_phase(state, events);
            }
            Phase::End => {
                return WaitingFor::Priority { player: state.active_player };
            }
            Phase::Cleanup => {
                execute_cleanup(state, events);
                // Start next turn
                start_next_turn(state, events);
            }
        }
    }
}
```

### Pattern 3: State-Based Actions Fixpoint Loop
**What:** After every game action, run all SBA checks repeatedly until none fire. Matches MTG Rule 704.3.
**When to use:** Called after every action in `apply()`, before determining next WaitingFor.
**Example:**
```rust
fn check_state_based_actions(state: &mut GameState, events: &mut Vec<GameEvent>) {
    loop {
        let mut any_performed = false;

        // 704.5a - Player at 0 or less life loses
        for player in &state.players {
            if player.life <= 0 {
                // Mark player as lost
                any_performed = true;
            }
        }

        // 704.5f - Creature with 0 or less toughness
        // 704.5g - Creature with lethal damage
        // 704.5h - Creature dealt deathtouch damage
        // 704.5j - Planeswalker with 0 loyalty
        // 704.5k - Legend rule
        // 704.5m - World rule
        // 704.5n - Aura not attached or attached to illegal object

        if !any_performed {
            break;
        }
    }
}
```

### Pattern 4: Central Object Store
**What:** All game objects stored in a single `HashMap<ObjectId, GameObject>` with zones holding only ObjectId references.
**When to use:** All zone lookups and object mutations.
**Rationale:** Avoids borrow-checker issues with objects "belonging" to zones. Object data lives in one place; zones are just lists of IDs.
```rust
pub struct GameState {
    pub turn_number: u32,
    pub active_player: PlayerId,
    pub phase: Phase,
    pub players: Vec<Player>,
    pub priority_player: PlayerId,
    pub priority_pass_count: u8,

    // Central object store
    pub objects: HashMap<ObjectId, GameObject>,
    pub next_object_id: u64,

    // Shared zones
    pub battlefield: Vec<ObjectId>,
    pub stack: Vec<StackEntry>,
    pub exile: Vec<ObjectId>,

    // RNG
    pub rng: StdRng,  // or ChaCha20Rng

    // Game flow
    pub waiting_for: WaitingFor,
    pub lands_played_this_turn: u8,
    pub max_lands_per_turn: u8,
}

pub struct Player {
    pub id: PlayerId,
    pub life: i32,
    pub mana_pool: ManaPool,

    // Per-player zones
    pub library: Vec<ObjectId>,
    pub hand: Vec<ObjectId>,
    pub graveyard: Vec<ObjectId>,

    // Tracking
    pub has_drawn_this_turn: bool,
    pub lands_played_this_turn: u8,
}
```

### Anti-Patterns to Avoid
- **Objects owning zone data:** Don't store the full object inside the zone Vec. Use ObjectId references into a central HashMap. This prevents borrow-checker nightmares when moving objects between zones.
- **Blocking game loop:** Don't create an internal loop that waits for player input. The action-response pattern returns WaitingFor and expects the next action from the caller.
- **Separate mana tracking:** Don't maintain separate color counters alongside mana units. The tracked units ARE the pool; derive counts from them.
- **Phase-specific action enums:** Don't create per-phase action types. Keep a single GameAction enum; validate legality based on current game state and WaitingFor.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| RNG | Custom PRNG | `rand` crate with `StdRng`/`ChaCha20Rng` | Cryptographic quality, seedable, well-tested |
| Mana payment optimization | Brute-force all combinations | Greedy algorithm with preference ordering | Forge uses preference-based approach; brute force is exponential for complex costs |
| Phase ordering | Manual if-else chains for next phase | Array/const of phases with index arithmetic | Less error-prone, matches Forge's PhaseType.getNext() |

**Key insight:** Mana payment for hybrid costs is a constraint satisfaction problem. Forge's approach is a greedy algorithm that prefers the color with the most available mana. This is good enough for Phase 3; optimal payment (minimizing future constraint) can be deferred.

## Common Pitfalls

### Pitfall 1: Borrow Checker with Zone Transfers
**What goes wrong:** Moving an object between zones requires mutating both the source zone, the destination zone, and possibly the object itself — triple mutable borrow.
**Why it happens:** If zones hold objects directly, you can't borrow two zones mutably at the same time.
**How to avoid:** Central `HashMap<ObjectId, GameObject>` for object data. Zone-change is: remove ID from source zone Vec, add to destination zone Vec, update object's `zone` field. All through `&mut GameState`.
**Warning signs:** Functions that take `&mut Zone` as parameter instead of `&mut GameState`.

### Pitfall 2: SBA Infinite Loops
**What goes wrong:** State-based actions can trigger each other infinitely (e.g., a creature dying changes another creature's toughness to 0).
**Why it happens:** The fixpoint loop has no termination guarantee if SBAs create new SBA conditions.
**How to avoid:** Forge caps at 9 iterations (`for (int q = 0; q < 9; q++)`). Use a similar cap. In practice, SBAs should converge quickly because they only remove things from the game.
**Warning signs:** Test hangs during SBA checks.

### Pitfall 3: Priority After Stack Resolution
**What goes wrong:** After a stack item resolves, priority must go back to the active player (not the player who had priority before). Both players must pass priority in succession with an empty stack to advance phases.
**Why it happens:** Misunderstanding MTG priority rules (Rule 117.3b, 117.4).
**How to avoid:** After each stack resolution, reset priority to active player and reset pass counter. Both players must pass consecutively with empty stack to advance.
**Warning signs:** Phases advancing after only one player passes priority.

### Pitfall 4: Mana Pool Clearing Timing
**What goes wrong:** Mana pools clear at wrong time (end of turn instead of each phase transition).
**Why it happens:** Common misconception. Per MTG rules (Rule 106.4), mana empties from pools at the end of each step and phase (not just end of turn).
**How to avoid:** Clear mana pools in `advance_phase()`. In Phase 3, there are no "mana doesn't empty" effects to worry about.
**Warning signs:** Mana persisting from main phase into combat.

### Pitfall 5: First Turn Draw Skip
**What goes wrong:** The first player on the first turn draws a card during their draw step.
**Why it happens:** Forgetting the MTG rule that the first player skips their first draw step in a 2-player game.
**How to avoid:** Check `turn_number == 1 && active_player == starting_player` in draw phase. Forge does this in `isSkippingPhase()` for the DRAW phase.
**Warning signs:** First player starting with 8 cards.

### Pitfall 6: London Mulligan Bottom-of-Library Ordering
**What goes wrong:** Cards put on bottom of library during London mulligan aren't handled correctly — player should choose order.
**Why it happens:** London mulligan draws 7, then puts N cards on bottom (in a player-chosen order), not drawing fewer cards.
**How to avoid:** Model as two separate WaitingFor states: MulliganDecision (keep/mull) and MulliganBottomCards (choose N cards to put on bottom). Forge's LondonMulligan.mulliganDraw() calls `tuckCardsViaMulligan` which asks the player controller to choose.
**Warning signs:** Mulligan always putting cards on bottom in a fixed order.

## Code Examples

### Zone Transfer Operation
```rust
// Source: Derived from Forge's GameAction.moveTo() pattern
fn move_to_zone(
    state: &mut GameState,
    object_id: ObjectId,
    to: Zone,
    events: &mut Vec<GameEvent>,
) {
    let obj = state.objects.get(&object_id).expect("object exists");
    let from = obj.zone;
    let owner = obj.owner;

    // Remove from source zone
    remove_from_zone(state, object_id, from, owner);

    // Add to destination zone
    add_to_zone(state, object_id, to, owner);

    // Update object's zone field
    state.objects.get_mut(&object_id).unwrap().zone = to;

    events.push(GameEvent::ZoneChanged { object_id, from, to });
}
```

### Mana Unit Tracking
```rust
// Source: Modeled after Forge's Mana.java record
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManaUnit {
    pub color: ManaType,
    pub source_id: ObjectId,
    pub snow: bool,
    pub restrictions: Vec<ManaRestriction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ManaType {
    White,
    Blue,
    Black,
    Red,
    Green,
    Colorless,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManaRestriction {
    OnlyForSpellType(String),
    // Future: more restrictions from card abilities
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManaPool {
    pub mana: Vec<ManaUnit>,
}

impl ManaPool {
    pub fn add(&mut self, unit: ManaUnit) {
        self.mana.push(unit);
    }

    pub fn count_color(&self, color: ManaType) -> usize {
        self.mana.iter().filter(|m| m.color == color).count()
    }

    pub fn total(&self) -> usize {
        self.mana.len()
    }

    pub fn clear(&mut self) {
        self.mana.clear();
    }

    pub fn spend(&mut self, color: ManaType) -> Option<ManaUnit> {
        if let Some(pos) = self.mana.iter().position(|m| m.color == color) {
            Some(self.mana.swap_remove(pos))
        } else {
            None
        }
    }
}
```

### GameObject Struct
```rust
// Source: Derived from Forge's Card.java rules-relevant fields
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameObject {
    pub id: ObjectId,
    pub card_id: CardId,           // Reference to CardRules in database
    pub owner: PlayerId,
    pub controller: PlayerId,
    pub zone: Zone,

    // Battlefield state
    pub tapped: bool,
    pub face_down: bool,
    pub flipped: bool,
    pub transformed: bool,

    // Combat
    pub damage_marked: u32,
    pub dealt_deathtouch_damage: bool,

    // Attachments
    pub attached_to: Option<ObjectId>,
    pub attachments: Vec<ObjectId>,

    // Counters
    pub counters: HashMap<CounterType, u32>,

    // Characteristics (may be modified by effects in later phases)
    pub name: String,
    pub power: Option<i32>,
    pub toughness: Option<i32>,
    pub loyalty: Option<u32>,
    pub card_types: CardType,
    pub mana_cost: ManaCost,
    pub keywords: Vec<String>,
    pub abilities: Vec<String>,       // Raw ability strings for Phase 4
    pub color: Vec<ManaColor>,

    // Summoning sickness
    pub entered_battlefield_turn: Option<u32>,
}
```

### Stack Entry
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackEntry {
    pub id: ObjectId,                  // Unique ID on the stack
    pub source_id: ObjectId,           // The card/permanent that created this
    pub controller: PlayerId,
    pub kind: StackEntryKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StackEntryKind {
    Spell { card_id: CardId },
    // Future: ActivatedAbility, TriggeredAbility
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Paris/Vancouver mulligan | London mulligan | 2019 (MTG rules update) | Draw 7 every time, put N on bottom. Simpler to implement than Vancouver (draw 7-N). |
| Mana burn | No mana burn | 2010 (Magic 2010 rules) | Unspent mana just empties, no life loss. Simplifies cleanup. |
| Damage on stack | No damage on stack | 2010 (Magic 2010 rules) | Combat damage doesn't use the stack. Simplifies combat phase. |

**Deprecated/outdated:**
- Mana burn (removed 2010) — do not implement
- Damage on stack (removed 2010) — combat damage is immediate, not a stack object

## Open Questions

1. **StdRng vs ChaCha20Rng for determinism**
   - What we know: `StdRng` is convenient but its algorithm may change between `rand` versions. `ChaCha20Rng` is guaranteed deterministic across platforms and versions.
   - What's unclear: Whether replay/network play is critical enough in Phase 3 to mandate ChaCha20Rng now.
   - Recommendation: Use `ChaCha20Rng` from the start. The cost is trivial (one extra dependency), and switching later means invalidating all saved seeds.

2. **Which SBAs to implement in Phase 3 vs defer**
   - What we know: Phase 3 success criteria mentions "creature with 0 toughness dies, player at 0 life loses, legend rule enforced."
   - What's unclear: Whether to implement aura detachment, planeswalker loyalty, world rule, etc.
   - Recommendation: Implement the three mentioned in success criteria plus aura detachment (704.5n) since auras may be relevant. Defer saga, battle, and exotic SBAs.

3. **Serde on RNG state**
   - What we know: GameState derives Serialize/Deserialize. `StdRng`/`ChaCha20Rng` don't implement Serialize.
   - What's unclear: Best approach for serialization.
   - Recommendation: Store the seed (u64) in GameState alongside the RNG. Serialize the seed; reconstruct RNG from seed on deserialize. Track operation count if mid-game serialization is needed (or use `#[serde(skip)]` and reconstruct).

4. **GameState serialization with HashMap<ObjectId, GameObject>**
   - What we know: HashMap serialization order is non-deterministic in serde_json.
   - What's unclear: Whether deterministic serialization matters for Phase 3.
   - Recommendation: Use BTreeMap<ObjectId, GameObject> instead of HashMap if deterministic serialization is needed (for snapshot diffing, network sync). BTreeMap is ordered. The context doc mentions BTreeMap as an option.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in test framework (cargo test) |
| Config file | None needed — `#[cfg(test)]` modules in each file |
| Quick run command | `cargo test -p engine` |
| Full suite command | `cargo test --workspace` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| ENG-01 | Turn phases progress in correct order | unit | `cargo test -p engine turn_progression -- --exact` | Wave 0 |
| ENG-01 | Auto-advance skips no-input phases | unit | `cargo test -p engine auto_advance -- --exact` | Wave 0 |
| ENG-02 | Priority alternates between players | unit | `cargo test -p engine priority -- --exact` | Wave 0 |
| ENG-02 | Stack resolves LIFO | unit | `cargo test -p engine stack_lifo -- --exact` | Wave 0 |
| ENG-02 | Both players pass with empty stack advances phase | unit | `cargo test -p engine priority_pass_advance -- --exact` | Wave 0 |
| ENG-03 | 0-toughness creature destroyed by SBA | unit | `cargo test -p engine sba_zero_toughness -- --exact` | Wave 0 |
| ENG-03 | 0-life player loses | unit | `cargo test -p engine sba_zero_life -- --exact` | Wave 0 |
| ENG-03 | Legend rule enforced | unit | `cargo test -p engine sba_legend_rule -- --exact` | Wave 0 |
| ENG-03 | SBA fixpoint loop repeats until stable | unit | `cargo test -p engine sba_fixpoint -- --exact` | Wave 0 |
| ENG-04 | Zone transfer generates events | unit | `cargo test -p engine zone_change_events -- --exact` | Wave 0 |
| ENG-04 | Objects track current zone | unit | `cargo test -p engine object_zone_tracking -- --exact` | Wave 0 |
| ENG-05 | Tap land adds correct mana color | unit | `cargo test -p engine land_mana_production -- --exact` | Wave 0 |
| ENG-05 | Mana pool clears on phase change | unit | `cargo test -p engine mana_pool_clear -- --exact` | Wave 0 |
| ENG-05 | Hybrid cost payment selects best option | unit | `cargo test -p engine hybrid_payment -- --exact` | Wave 0 |
| ENG-05 | Generic mana payable by any color | unit | `cargo test -p engine generic_mana -- --exact` | Wave 0 |
| ENG-06 | London mulligan draws 7 each time | unit | `cargo test -p engine london_mulligan_draw -- --exact` | Wave 0 |
| ENG-06 | Cards put on bottom after keeping | unit | `cargo test -p engine mulligan_bottom_cards -- --exact` | Wave 0 |
| ENG-06 | Full game integration: mulligan -> play land -> tap -> pass | integration | `cargo test -p engine full_turn_integration -- --exact` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test -p engine`
- **Per wave merge:** `cargo test --workspace`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `crates/engine/src/game/` — entire module tree (engine.rs, turns.rs, priority.rs, stack.rs, zones.rs, mana_payment.rs, sba.rs, mulligan.rs, game_object.rs)
- [ ] `rand` dependency — not yet in Cargo.toml
- [ ] `rand_chacha` dependency — if using ChaCha20Rng
- [ ] Restructured ManaPool type — current ManaPool is simple counters, needs tracked units
- [ ] Expanded GameState — needs objects HashMap, zone collections, RNG, WaitingFor
- [ ] Expanded Player — needs per-player zone collections
- [ ] GameObject struct — does not exist yet

## Sources

### Primary (HIGH confidence)
- Forge source code at `../forge/forge-game/src/main/java/forge/game/` — PhaseHandler.java, GameAction.java (checkStateEffects), mana/Mana.java, mana/ManaPool.java, mulligan/LondonMulligan.java, zone/MagicStack.java
- Existing forge.rs codebase at `crates/engine/src/types/` — all type files inspected for current state
- MTG Comprehensive Rules (training knowledge) — Rule 117 (priority), 704 (SBAs), 106 (mana), 103 (mulligan), 500-514 (turn structure)

### Secondary (MEDIUM confidence)
- `rand` crate 0.9 API — based on training knowledge of rand 0.8 + known 0.9 breaking changes. Verify exact API on implementation.

### Tertiary (LOW confidence)
- None

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - minimal dependencies (rand only), well-known crate
- Architecture: HIGH - directly informed by Forge source code and user decisions in CONTEXT.md
- Pitfalls: HIGH - derived from Forge source analysis and MTG rules knowledge
- Mana payment: MEDIUM - hybrid/phyrexian payment algorithm details need validation during implementation

**Research date:** 2026-03-07
**Valid until:** 2026-04-07 (stable domain, MTG rules don't change frequently)
