# Phase 29: Support N Players in Engine and on Board in React for Various Formats - Research

**Researched:** 2026-03-11
**Domain:** N-player MTG engine architecture, multiplayer board UI, format-aware game rules
**Confidence:** HIGH

## Summary

This phase extends a hardcoded 2-player MTG engine to support N players (2-6) with format awareness (Standard, Commander, FFA, Two-Headed Giant). The codebase has 17+ locations using `PlayerId(1 - player.0)` for opponent lookup, a `new_two_player()` constructor, and priority logic hardcoded to `priority_pass_count >= 2`. The frontend `GameBoard.tsx` renders a fixed top/bottom opponent/player split. All of these need systematic replacement with N-player equivalents.

The engine architecture (pure reducer, discriminated unions, immutable state) is fundamentally sound for N-player extension. `PlayerId(pub u8)` and `players: Vec<Player>` already support N players at the type level. The changes are primarily: (1) replacing hardcoded 2-player assumptions with seat-order-based logic, (2) adding format configuration types, (3) extending combat for per-creature attack targets, (4) updating the board UI for multiple opponent areas, and (5) extending the AI for threat-aware N-player evaluation.

**Primary recommendation:** Execute in waves -- engine core types and format config first, then game logic N-player conversion, then combat targeting, then frontend board layout, then AI and networking, then deck builder and game setup flow.

<user_constraints>

## User Constraints (from CONTEXT.md)

### Locked Decisions
- Support four formats: Standard (1v1, 20 life), Commander/EDH (2-6 players, 40 life), Free-for-All (2-6 players, 20 life), Two-Headed Giant (4 players, 2v2, shared 30 life per team)
- Full Commander rules: command zone, commander tax (+2 per cast), commander damage tracking (21 from one commander = loss), color identity enforcement, 100-card singleton, partner commander support
- Proper Two-Headed Giant simultaneous turns (team members share a turn and combat phase, team-based priority passing)
- Full MTG elimination rules (CR 800.4): eliminated player's permanents exiled, spells removed from stack, effects end
- Full deck validation per format at game start
- `FormatConfig` enum + struct approach
- Optional range of influence field
- Format rule overrides (house rules)
- Unified `PlayerArea` component with `mode` prop: `full`, `focused`, `compact`
- 1v1 visual parity is a hard requirement
- Strict MTG multiplayer rules: clockwise turn rotation, APNAP priority order
- Explicit `seat_order: Vec<PlayerId>` on GameState
- Priority passes clockwise; all living players must pass consecutively
- Per-creature attack target selection
- "Attack all" convenience button plus per-creature override
- WebSocket server only for 3+ player games (P2P stays 2-player only)
- Lobby with ready-up system, format-aware
- Pause game on disconnect for multiplayer
- Spectator support with hidden-info view
- AI fills any empty seat with per-seat difficulty
- Threat-aware AI
- Full Commander deck builder support
- Format legality badges from MTGJSON data
- Format-first game setup flow
- Pre-built Commander decks (4-5 precons)
- Game preset saving

### Claude's Discretion
- Exact `opponent()` / `opponents()` / `next_player()` API design based on usage analysis
- AI search algorithm choice (alpha-beta adaptation vs MCTS vs hybrid)
- Deck builder format access UX
- Compact strip exact layout and card sizing
- Commander visual highlight style
- Pre-game lobby room chat implementation details

### Deferred Ideas (OUT OF SCOPE)
- Draft/Sealed game modes
- Brawl format
- Archenemy format
- Planechase
- P2P for 3+ players
- Mobile-optimized multiplayer layout
- Replay/spectator replay system

</user_constraints>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| engine crate | existing | Core game logic extension | Foundation -- all N-player logic lives here |
| serde + serde_json | existing | Format config serialization | Already used for all types |
| rpds | existing | Persistent data structures | Structural sharing for state cloning during AI search |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| MTGJSON AtomicCards.json | existing | Card legalities data | Format legality badges, deck validation |
| zustand | existing | Frontend state management | multiplayerStore format extensions |
| tailwindcss v4 | existing | Board layout styling | PlayerArea compact/focused/full modes |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| Alpha-beta for N-player | MCTS (Monte Carlo Tree Search) | MCTS handles N-player better theoretically but alpha-beta is already implemented and working; recommend adapting alpha-beta with paranoid search (treat all opponents as minimizers) for 2-4 players, switching to max-N with shallow depth for 5-6 players |
| CSS Grid for N-player layout | Flexbox | CSS Grid better for N-player board since player areas need 2D positioning |

**Installation:**
No new dependencies needed. All changes are to existing crates and packages.

## Architecture Patterns

### Engine Type Additions
```
crates/engine/src/types/
├── format.rs            # NEW: GameFormat enum, FormatConfig struct, FormatRules
├── game_state.rs        # MODIFIED: add seat_order, format_config, eliminated_players, commander_damage
├── player.rs            # MODIFIED: add is_eliminated, commander tracking
├── actions.rs           # MODIFIED: DeclareAttackers with per-creature targets
└── zones.rs             # UNCHANGED: Command zone already exists
```

### Game Logic Modifications
```
crates/engine/src/game/
├── engine.rs            # MODIFIED: elimination handling, format-aware win conditions
├── priority.rs          # MODIFIED: clockwise N-player priority, living player pass count
├── turns.rs             # MODIFIED: clockwise turn rotation via seat_order
├── combat.rs            # MODIFIED: per-creature attack targets, multi-defender
├── combat_damage.rs     # MODIFIED: commander damage tracking
├── sba.rs               # MODIFIED: N-player elimination (not instant game over)
├── mulligan.rs          # MODIFIED: N-player mulligan sequence
├── commander.rs         # NEW: command zone, commander tax, commander damage SBA
├── elimination.rs       # NEW: CR 800.4 elimination cleanup
└── format_rules.rs      # NEW: format-specific rule enforcement
```

### Frontend Structure
```
client/src/
├── components/board/
│   ├── PlayerArea.tsx       # NEW: unified component with full/focused/compact modes
│   ├── CompactStrip.tsx     # NEW: condensed opponent view
│   ├── GameBoard.tsx        # MODIFIED: renders PlayerArea array instead of hardcoded 2
│   └── CommanderDisplay.tsx # NEW: commander card with highlight
├── components/controls/
│   ├── AttackTargetPicker.tsx  # NEW: per-creature attack target selection
│   └── CommanderDamage.tsx     # NEW: commander damage tracker display
├── pages/
│   └── GameSetupPage.tsx    # NEW or MODIFIED: format-first flow
└── stores/
    ├── gameStore.ts         # MODIFIED: N-player state handling
    └── multiplayerStore.ts  # MODIFIED: format, player list, ready-up
```

### Pattern 1: Seat Order and Player Iteration
**What:** Replace all `PlayerId(1 - player.0)` with seat-order-based traversal
**When to use:** Every place that needs "next player", "opponents", or "all other players"

The 17 locations using `PlayerId(1 - player.0)` break down into these semantic categories (based on code analysis):

1. **Next in turn order** (turns.rs:62, priority.rs:43): `next_player_in_seat_order(state, current)`
2. **Defending player in combat** (combat.rs:113, 279, 471, turns.rs:258, engine.rs:224): replaced by per-creature `AttackTarget`
3. **All opponents for evaluation** (eval.rs:46, combat_ai.rs:14, card_hints.rs:64): `opponents(state, player)` returning `Vec<PlayerId>`
4. **Winner determination** (sba.rs:67, 89): replaced by elimination logic (no immediate winner with N players)
5. **State filtering** (filter.rs:8, server main): `filter_state_for_player` must hide ALL opponents' hands
6. **Server opponent lookup** (main.rs:259, 507, 805, 837): replaced by broadcast to all other players in session

**Recommended API:**
```rust
// In a new module: crates/engine/src/game/players.rs
/// Next living player in seat order (clockwise)
pub fn next_player(state: &GameState, current: PlayerId) -> PlayerId;

/// All opponents of a player (all living players except them)
pub fn opponents(state: &GameState, player: PlayerId) -> Vec<PlayerId>;

/// All living players in APNAP order starting from active player
pub fn apnap_order(state: &GameState) -> Vec<PlayerId>;

/// Teammate(s) for team-based formats
pub fn teammates(state: &GameState, player: PlayerId) -> Vec<PlayerId>;
```

### Pattern 2: FormatConfig as Game State
**What:** Format configuration stored on GameState, consulted by game logic
**When to use:** Starting life, deck validation, commander rules, win conditions

```rust
// Source: new types/format.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameFormat {
    Standard,
    Commander,
    FreeForAll,
    TwoHeadedGiant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatConfig {
    pub format: GameFormat,
    pub starting_life: i32,
    pub min_players: u8,
    pub max_players: u8,
    pub deck_size: u16,           // 60 or 100
    pub singleton: bool,
    pub command_zone: bool,
    pub commander_damage_threshold: Option<u8>,  // 21 for Commander
    pub range_of_influence: Option<u8>,
    pub team_based: bool,
}

impl FormatConfig {
    pub fn standard() -> Self { /* 20 life, 2 players, 60 cards */ }
    pub fn commander() -> Self { /* 40 life, 2-6 players, 100 cards, singleton */ }
    pub fn free_for_all() -> Self { /* 20 life, 2-6 players, 60 cards */ }
    pub fn two_headed_giant() -> Self { /* 30 life per team, 4 players */ }
}
```

### Pattern 3: Per-Creature Attack Targets
**What:** Each attacking creature independently selects its target (player or planeswalker)
**When to use:** DeclareAttackers action in multiplayer

```rust
// Modified GameAction
GameAction::DeclareAttackers {
    // Vec of (attacker_id, attack_target)
    attacks: Vec<(ObjectId, AttackTarget)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum AttackTarget {
    Player(PlayerId),
    Planeswalker(ObjectId),
}

// Modified CombatState -- AttackerInfo already has defending_player field
// Just need to populate it per-creature instead of globally
```

### Pattern 4: Elimination vs Game Over
**What:** In N-player games, a player losing doesn't end the game -- they're eliminated
**When to use:** SBA checks for life <= 0, poison >= 10, commander damage >= 21

```rust
// Current: immediately sets WaitingFor::GameOver with winner
// New: mark player eliminated, clean up their permanents/spells, continue game
// Game ends when only one player/team remains

// New fields on GameState:
pub eliminated_players: Vec<PlayerId>,
// Player struct addition:
pub is_eliminated: bool,

// New field for commander damage tracking:
pub commander_damage: HashMap<(PlayerId, ObjectId), u32>,  // (victim, commander_source) -> damage
```

### Anti-Patterns to Avoid
- **Hardcoded player indices:** Never use `PlayerId(0)` or `PlayerId(1)` for semantic meaning (active/opponent). Always derive from seat_order.
- **Direct Vec indexing with PlayerId:** Use `state.players.iter().find(|p| p.id == id)` not `state.players[id.0 as usize]` -- seats may not match indices after elimination.
- **Binary opponent assumption:** Never assume exactly one opponent. Every "opponent" reference must handle N opponents.
- **Immediate game over on player loss:** In N-player, player loss = elimination, not game end (unless last player remaining).

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| MTGJSON legality data | Custom legality scraper | MTGJSON `legalities` field | Already in AtomicCards.json with format-keyed legal/banned/restricted status |
| Commander color identity | Manual color parsing | MTGJSON `colorIdentity` field | Already available per card, handles hybrid mana and color indicators |
| Pre-built Commander decks | Manual JSON deck files | Standard .dck format with import | Deck parser already exists, just need commander auto-detection from sideboard |
| Clockwise turn rotation | Custom modular arithmetic | `seat_order.iter().cycle()` pattern | Rust iterators handle wraparound cleanly |

**Key insight:** MTGJSON already provides `legalities` and `colorIdentity` -- the engine just needs to deserialize these fields from AtomicCards.json (they exist in the full file but are not currently parsed by `AtomicCard` struct).

## Common Pitfalls

### Pitfall 1: Priority Pass Count Race Condition
**What goes wrong:** With N players, tracking `priority_pass_count` as a simple integer and comparing to player count is fragile -- if a player casts a spell mid-round, the count must reset to 0 and restart from active player.
**Why it happens:** Current code uses `priority_pass_count >= 2` which implicitly handles the 2-player case but doesn't track which players passed.
**How to avoid:** Track priority passes as a set of player IDs who have passed consecutively. Any action other than passing clears the set. Stack resolves when set contains all living players.
**Warning signs:** Stack resolving with only some players having passed.

### Pitfall 2: Elimination Cleanup Ordering
**What goes wrong:** When a player is eliminated (CR 800.4), their permanents are exiled, spells removed from stack, effects end. If cleanup is done in wrong order, triggers from exile may fire, or stack entries may reference deleted objects.
**Why it happens:** MTG comprehensive rules specify a specific cleanup order that's easy to get wrong.
**How to avoid:** Follow CR 800.4 exactly: (1) remove player's spells from stack, (2) exile all permanents they own, (3) end all effects from their sources, (4) remove any of their triggered abilities from stack. Do NOT fire triggers during this process.
**Warning signs:** Crash or panic when accessing eliminated player's objects.

### Pitfall 3: Two-Headed Giant Shared Turns
**What goes wrong:** 2HG teams share a turn, meaning both team members have a main phase simultaneously and share a combat phase. This is fundamentally different from sequential turns.
**Why it happens:** It's tempting to treat 2HG as "just another format" but the turn structure is unique.
**How to avoid:** Model 2HG turns as a special case where `active_team` replaces `active_player` for turn purposes, but individual players still take priority actions within APNAP order.
**Warning signs:** Only one team member getting priority, or combat phases happening separately.

### Pitfall 4: `opponent()` Migration Breaking Tests
**What goes wrong:** The 764+ usages of `opponent()` across 57 files mean a big-bang replacement will cause massive compile errors.
**Why it happens:** The function is used everywhere with slightly different semantics.
**How to avoid:** Phase the migration: (1) Add new `next_player()`, `opponents()`, `apnap_order()` functions alongside `opponent()`. (2) Update callers file-by-file with the correct semantic replacement. (3) Eventually deprecate `opponent()`. (4) Keep `opponent()` working for 2-player games as a compatibility wrapper during migration.
**Warning signs:** Compile error avalanche when removing `opponent()` function.

### Pitfall 5: AI Search Explosion with N Players
**What goes wrong:** Alpha-beta search complexity grows exponentially with player count. A 2-player game at depth 3 is manageable; a 6-player game at depth 3 is intractable.
**Why it happens:** Each additional player adds another level of branching per turn cycle.
**How to avoid:** Scale search budget inversely with player count: depth 2 max for 3+ players, depth 1 for 5+ players. Compensate with better heuristic evaluation (threat assessment).
**Warning signs:** AI taking seconds per decision in multiplayer, or WASM timing out.

### Pitfall 6: Frontend Performance with 6 Player Boards
**What goes wrong:** Rendering 5 opponent boards with card images causes performance issues and layout problems.
**Why it happens:** Each player has lands, creatures, and other permanents, plus hand cards to track.
**How to avoid:** Compact mode uses art-crop thumbnails (no full card rendering). Only render full card details for the focused opponent. Use virtualization for large boards. Lazy-load opponent card images.
**Warning signs:** FPS drops when 6 players all have boards.

## Code Examples

### GameState Factory with Format
```rust
// Replace GameState::new_two_player(seed)
impl GameState {
    pub fn new(config: FormatConfig, player_count: u8, seed: u64) -> Self {
        let players: Vec<Player> = (0..player_count)
            .map(|i| Player {
                id: PlayerId(i),
                life: config.starting_life,
                ..Player::default()
            })
            .collect();

        let seat_order: Vec<PlayerId> = (0..player_count).map(PlayerId).collect();

        GameState {
            players,
            seat_order,
            format_config: config,
            eliminated_players: Vec::new(),
            commander_damage: HashMap::new(),
            // ... rest of fields same as current
        }
    }

    // Keep for backward compat during migration
    pub fn new_two_player(seed: u64) -> Self {
        Self::new(FormatConfig::standard(), 2, seed)
    }
}
```

### N-Player Priority Pass
```rust
pub fn handle_priority_pass(state: &mut GameState, events: &mut Vec<GameEvent>) -> WaitingFor {
    state.priority_passes.insert(state.priority_player);

    let living_count = state.players.iter()
        .filter(|p| !p.is_eliminated)
        .count();

    if state.priority_passes.len() >= living_count {
        // All living players passed consecutively
        state.priority_passes.clear();
        if state.stack.is_empty() {
            turns::advance_phase(state, events);
            turns::auto_advance(state, events)
        } else {
            super::stack::resolve_top(state, events);
            reset_priority(state);
            WaitingFor::Priority { player: state.active_player }
        }
    } else {
        let next = next_player(state, state.priority_player);
        state.priority_player = next;
        events.push(GameEvent::PriorityPassed { player_id: next });
        WaitingFor::Priority { player: next }
    }
}
```

### Clockwise Turn Rotation
```rust
pub fn start_next_turn(state: &mut GameState, events: &mut Vec<GameEvent>) {
    state.turn_number += 1;
    state.active_player = next_player(state, state.active_player);
    // ... reset per-turn counters
}

/// Find next living player in seat order after `current`
pub fn next_player(state: &GameState, current: PlayerId) -> PlayerId {
    let pos = state.seat_order.iter()
        .position(|&id| id == current)
        .expect("player in seat order");

    for offset in 1..=state.seat_order.len() {
        let next = state.seat_order[(pos + offset) % state.seat_order.len()];
        if !state.eliminated_players.contains(&next) {
            return next;
        }
    }
    current // only player remaining
}
```

### Elimination Handling
```rust
pub fn eliminate_player(state: &mut GameState, player: PlayerId, events: &mut Vec<GameEvent>) {
    state.eliminated_players.push(player);
    if let Some(p) = state.players.iter_mut().find(|p| p.id == player) {
        p.is_eliminated = true;
    }

    // CR 800.4a: Remove spells they control from stack
    state.stack.retain(|entry| entry.controller != player);

    // CR 800.4b: Exile permanents they own
    let owned: Vec<ObjectId> = state.battlefield.iter()
        .filter(|&&id| state.objects.get(&id).map(|o| o.owner == player).unwrap_or(false))
        .copied()
        .collect();
    for id in owned {
        zones::move_to_zone(state, id, Zone::Exile, events);
    }

    events.push(GameEvent::PlayerEliminated { player_id: player });

    // Check if game is over (only one player/team remaining)
    let living: Vec<PlayerId> = state.players.iter()
        .filter(|p| !p.is_eliminated)
        .map(|p| p.id)
        .collect();

    if living.len() <= 1 {
        let winner = living.first().copied();
        state.waiting_for = WaitingFor::GameOver { winner };
        events.push(GameEvent::GameOver { winner });
    }
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| `PlayerId(1 - player.0)` | `next_player(state, player)` | This phase | All 17 locations must change |
| `new_two_player(seed)` | `new(format_config, player_count, seed)` | This phase | Constructor updated, old one kept as compat wrapper |
| `priority_pass_count >= 2` | `priority_passes.len() >= living_count` | This phase | Priority system becomes set-based |
| `CombatState` with single defending_player | Per-creature `AttackTarget` on each `AttackerInfo` | This phase | Combat system extended |
| GameBoard with hardcoded opponent/player | `PlayerArea` with mode prop | This phase | Board layout fully restructured |

**Deprecated/outdated:**
- `opponent()` function: Will be replaced by `next_player()`, `opponents()`, `apnap_order()` depending on call-site semantics
- `GameState::new_two_player()`: Kept as compatibility wrapper calling `new(FormatConfig::standard(), 2, seed)`
- `priority_pass_count: u8`: Replaced by `priority_passes: HashSet<PlayerId>` (or `BTreeSet` for determinism)

## Open Questions

1. **2HG Turn Model Complexity**
   - What we know: 2HG teams share turns and combat phases. Both team members can act during main phases.
   - What's unclear: Whether to model 2HG as `active_team: TeamId` with sub-player priority, or as `active_players: Vec<PlayerId>` for simultaneous turns.
   - Recommendation: Start with `active_team` approach since it maps cleanly to MTG rules. Team members take priority in APNAP order within the team.

2. **AI Algorithm for N-Player**
   - What we know: Current alpha-beta works well for 2-player. Paranoid search (all opponents minimize) is the standard adaptation for N-player.
   - What's unclear: Whether paranoid search or max-N gives better play in MTG specifically.
   - Recommendation: Use paranoid search for 3-4 players (proven, simpler). Add threat-weighting to the evaluation function (weight opponents by threat level, don't treat them equally). For 5-6 players, fall back to heuristic-only (no search) due to branching explosion.

3. **State Size Growth**
   - What we know: GameState cloning is used extensively in AI search. With 6 players, state is ~3x larger.
   - What's unclear: Whether rpds structural sharing mitigates the cloning cost sufficiently for WASM.
   - Recommendation: Profile after implementation. If too slow, reduce AI search depth on WASM for 4+ player games.

4. **MTGJSON Legalities Coverage**
   - What we know: MTGJSON `legalities` field exists with format-keyed status. The `AtomicCard` struct does not currently deserialize this field.
   - What's unclear: Whether all supported cards have legality data populated.
   - Recommendation: Add `legalities: HashMap<String, String>` to `AtomicCard` struct. Fall back to "unknown" for missing data.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | cargo test (Rust) + vitest (TypeScript) |
| Config file | Cargo.toml / client/vitest.config.ts |
| Quick run command | `cargo test -p engine -- test_name` |
| Full suite command | `cargo test --all && cd client && pnpm test -- --run` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| N-PLAYER-01 | GameState::new creates N players with format-specific life | unit | `cargo test -p engine -- new_creates_n_players` | Wave 0 |
| N-PLAYER-02 | Priority passes clockwise through all living players | unit | `cargo test -p engine -- priority_clockwise_n_player` | Wave 0 |
| N-PLAYER-03 | Turn rotation follows seat_order | unit | `cargo test -p engine -- turn_rotation_seat_order` | Wave 0 |
| N-PLAYER-04 | Elimination removes permanents and spells (CR 800.4) | unit | `cargo test -p engine -- elimination_cleanup` | Wave 0 |
| N-PLAYER-05 | Per-creature attack target selection | unit | `cargo test -p engine -- per_creature_attack_targets` | Wave 0 |
| N-PLAYER-06 | Commander damage tracking and 21-damage loss | unit | `cargo test -p engine -- commander_damage_threshold` | Wave 0 |
| N-PLAYER-07 | Commander tax (+2 per cast from command zone) | unit | `cargo test -p engine -- commander_tax` | Wave 0 |
| N-PLAYER-08 | Deck validation per format | unit | `cargo test -p engine -- deck_validation_format` | Wave 0 |
| N-PLAYER-09 | State filtering hides all opponents' hands | unit | `cargo test -p server_core -- filter_n_player_state` | Wave 0 |
| N-PLAYER-10 | AI threat evaluation across N opponents | unit | `cargo test -p phase_ai -- threat_eval_n_player` | Wave 0 |
| N-PLAYER-11 | 2-player games produce identical behavior to current | integration | `cargo test -p engine -- two_player_compat` | Wave 0 |
| N-PLAYER-12 | PlayerArea renders full/focused/compact modes | unit | `cd client && pnpm test -- --run PlayerArea` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test -p engine && cargo test -p phase_ai`
- **Per wave merge:** `cargo test --all && cd client && pnpm test -- --run`
- **Phase gate:** Full suite green before verification

### Wave 0 Gaps
- [ ] `crates/engine/src/types/format.rs` -- FormatConfig type and tests
- [ ] `crates/engine/src/game/players.rs` -- next_player, opponents, apnap_order functions with tests
- [ ] `crates/engine/src/game/elimination.rs` -- elimination cleanup with tests
- [ ] `crates/engine/src/game/commander.rs` -- commander zone, tax, damage tracking
- [ ] Tests verifying 2-player backward compatibility after refactor

## Sources

### Primary (HIGH confidence)
- Codebase analysis: 17 locations using `PlayerId(1 - player.0)` across 10 files
- Codebase analysis: `opponent()` function in priority.rs used in 6 occurrences across 2 files
- Codebase analysis: `GameState::new_two_player()` constructor, `priority_pass_count >= 2` check
- Codebase analysis: `CombatState` with `AttackerInfo.defending_player` already per-creature
- Codebase analysis: MTGJSON AtomicCards.json contains `legalities` and `colorIdentity` fields
- Codebase analysis: `Zone::Command` already exists in zones.rs
- MTG Comprehensive Rules 800.4 (elimination), 801 (range of influence), 810 (Two-Headed Giant)

### Secondary (MEDIUM confidence)
- N-player game search: Paranoid search and max-N are the two standard approaches for extending minimax to N players. Paranoid search is simpler and works well when alliances are unstable (like MTG FFA).

### Tertiary (LOW confidence)
- MCTS performance claims for N-player games -- theoretical advantage but implementation complexity is high and the existing alpha-beta infrastructure would need full replacement. Recommend against.

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - no new dependencies needed, all extends existing codebase
- Architecture: HIGH - thoroughly analyzed all 17 hardcoded 2-player locations and their semantic requirements
- Pitfalls: HIGH - based on direct code analysis of current patterns that will break
- AI search strategy: MEDIUM - paranoid search is well-documented but untested in this specific codebase
- 2HG turn model: MEDIUM - MTG rules are clear but implementation approach has options

**Research date:** 2026-03-11
**Valid until:** 2026-04-11 (stable domain, rules don't change)
